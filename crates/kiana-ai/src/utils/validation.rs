//! `utils/validation.ts`——工具参数校验与强制转换。
//!
//! TS: `packages/ai/src/utils/validation.ts`。
//!
//! TS 侧依赖 TypeBox：`Compile(schema)` 编译校验器、`Value.Convert` 做类型强制转换、
//! `Object.getOwnPropertySymbols(...)` 判断 schema 是否带 `Symbol.for("TypeBox.Kind")`。
//! Rust 侧**不引入 typebox**（它是 JS 包），因此：
//!
//! - [`check_json_schema`] / [`collect_json_schema_errors`] 逐字复刻 TypeBox 1.3.27 的
//!   `CheckSchema` / `ErrorSchema`（关键字顺序、`keyword`、`instancePath`、`params` 与
//!   `en_US` 本地化消息全部对齐），包括 `maxErrors = 8` 的容量上限与 `anyOf`/`oneOf`/`allOf`
//!   各自独立的错误上下文；
//! - [`value_convert`] 复刻 `Value.Convert` 的 `FromType`（Number/Integer/Boolean/String/
//!   Null/Array/Tuple/Object/Record/Union/Literal/Enum/Intersect 及其 `Try*` 规则）。TypeBox
//!   的 `~kind` 用 `Object.defineProperty`（`enumerable: false`）挂载，`JSON.stringify` 会丢弃；
//!   Rust 侧用**显式 `"~kind"` 键**表示“活 TypeBox 节点”（与 `kiana-chord::schema` 的
//!   `mark_live_schema` 约定一致），**没有 `~kind` 的节点是普通 JSON schema，`Value.Convert`
//!   对它是空操作**（与 TS 里 `JSON.parse` 得到的 schema 行为相同，如 MCP 的 `inputSchema`）；
//! - TS 的 `Object.getOwnPropertySymbols(...)` 永远看不到字符串 `~kind`，所以 `Value.Convert`
//!   之后**总是**再走 [`coerce_with_json_schema`]；Rust 侧两步都做，顺序与 TS 相同；
//! - `Value.Convert` 的**返回值被 TS 丢弃**（`Value.Convert(tool.parameters, args)` 不接收
//!   结果），只有原地修改会保留：[`value_convert`] 只应用原地写入（Object/Record 的属性赋值、
//!   Tuple 的元素赋值、Intersect 各分支的原地效果、Array 元素内部的原地效果），根节点/数组
//!   `map`/联合候选产生的新值一律丢弃，与 TS 相同。
//!
//! 与 TS 的已知差异（均写在各函数文档里）：
//! - 活 schema 的 `~kind` 需要调用方用显式键标记（JSON 没有不可枚举属性）。未标记的 TypeBox
//!   构造物在 Rust 侧按普通 schema 处理，`Value.Convert` 为空操作（TS 里它们是活的）；
//! - 手写的 `~kind` 子 schema 与 TypeBox 构造的活节点字节相同，无法区分（同 kiana-chord）；
//! - `key in value` 的**原型链**语义：JS 里 `"toString" in {}` 为真，Rust 只认自有键；
//! - 正则（`pattern`/`patternProperties`/`format` 之外的内置正则）用 `regress`（ECMAScript
//!   引擎）复刻 JS `RegExp`；非法 `pattern` 在 TS 里 `Compile` 时抛 `SyntaxError`，这里在
//!   `Value.Convert` 之后、`Check` 之前返回同文本的 `Request` 错误；
//! - `idn-hostname` / `iri` / `iri-reference` 的 Bidi 规则与 WHATWG URL 解析无法逐字复刻，
//!   见 `format_test`。
//!
//! 错误文本（模型可见，必须逐字）：
//!
//! ```text
//! Validation failed for tool "<name>":
//!   - <path>: <message>
//!   - ...
//!
//! Received arguments:
//! <JSON.stringify(arguments, null, 2)>
//! ```
//!
//! # 在系统中的位置
//!
//! 本模块由 `utils/mod.rs` 声明，并经 `index.rs` 的 `pub use crate::utils::validation::*`
//! 再导出。仓库内的直接调用方：
//!
//! - `kiana-agent-core` 的 agent loop：对象参数走 [`validate_tool_arguments`]；非对象参数
//!   在本地按 `normalize_optional_nulls` → [`value_convert`] → [`coerce_with_json_schema`]
//!   → [`check_json_schema`] 的顺序组合，先后顺序与本模块相同；
//! - `kiana-durable` 的工具 harness：参数走 [`validate_tool_arguments`]，工具返回的
//!   `structuredOutput` 用 [`check_json_schema`] / [`collect_json_schema_errors`] 校验；
//! - `kiana-extensions` 的虚拟模块：直接调用 [`validate_tool_arguments`]，把错误文本抛给
//!   JS 调用方；
//! - 其他只需要“判定”或“错误目录”的调用方直接用 [`check_json_schema`] /
//!   [`collect_json_schema_errors`]。
//!
//! # 一次典型调用
//!
//! 1. [`validate_tool_call`] 按 `tool_call.name` 线性查找 [`Tool`]，找不到直接返回
//!    `Request` 错误；
//! 2. [`validate_tool_arguments`] 把参数复制成独立的 [`JsonValue`]（对应 TS 的
//!    `structuredClone`），后续原地修改不会写回 `tool_call`；
//! 3. `normalize_optional_nulls` 删除“可选且显式 null”的字段；
//! 4. `value_convert` 应用 TypeBox `Value.Convert` 的原地效果；
//! 5. `validate_schema_patterns` 预检非法正则（对应 TS `Compile` 的 `SyntaxError`）；
//! 6. `coerce_with_json_schema` 做普通 JSON Schema 的强制转换；
//! 7. `check_json_schema` 判定；失败时用 `collect_json_schema_errors` 生成有序错误列表；
//! 8. 成功返回强制转换后的参数，失败返回文件开头模板的错误文本。
//!
//! # 阅读顺序
//!
//! 关心“判定”的读者：`check_schema`（关键字顺序与对象/数组分组）→ 各 `check_*` 辅助函数
//! → `resolve_ref`。关心“错误文本”的读者：`error_schema` → `localized_message` →
//! `format_validation_path`。关心“转换”的读者：`coerce_with_json_schema`（普通 schema）与
//! `value_convert`（活 TypeBox schema）；两者互不依赖，可以分开读。
//!
//! # 注意事项
//!
//! - 判定引擎与错误引擎是两套独立实现（对应 TS 的 `CheckSchema` 与 `ErrorSchema`）。修改
//!   某个关键字的顺序或边界时必须同步另一套，否则 [`check_json_schema`] 的结论与
//!   `Validation failed ...` 列表会互相矛盾。`tests/validation.rs` 的黄金 fixture
//!   （`tests/fixtures/utils/validation.json`）锁定了 95 个 schema 用例和 117 条错误。
//! - 错误列表最多 [`MAX_ERRORS`] 条；到达上限后更深层的子 schema 不再展开，因此模型看到
//!   的列表可能不完整（这是 TypeBox 的既有行为）。
//! - `pattern` / `patternProperties` 的非法正则在 TS 里是 `Compile` 期异常；本模块把它
//!   推迟到 [`validate_tool_arguments`] 的第 5 步，直接调用 [`check_json_schema`] 的入口
//!   则静默判为不匹配。

use std::collections::HashSet;

use serde_json::{Map, Value};
use unicode_normalization::UnicodeNormalization;

use crate::error::{AiError, AiErrorKind};
use crate::types::{JsonValue, Tool, ToolCall};

// ---------------------------------------------------------------------------
// JSON Schema 视图的小工具（TS: `JsonSchemaObject` 与 `getSchemaTypes`/`matchesJsonType`）
// ---------------------------------------------------------------------------

/// `typeof value === "object" && value !== null && !Array.isArray(value)`。
pub(crate) fn is_object_not_array(value: &Value) -> bool {
    value.is_object()
}

/// `schema[key]`；非对象时 `undefined`。
fn get<'a>(schema: &'a Value, key: &str) -> Option<&'a Value> {
    schema.as_object().and_then(|map| map.get(key))
}

/// `getSchemaTypes(schema)`：`type` 是 string -> `[type]`；数组 -> 过滤字符串；否则 `[]`。
pub(crate) fn get_schema_types(schema: &Value) -> Vec<String> {
    match get(schema, "type") {
        Some(Value::String(text)) => vec![text.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// `matchesJsonType(value, type)`。
pub(crate) fn matches_json_type(value: &Value, schema_type: &str) -> bool {
    match schema_type {
        "number" => value.is_number(),
        "integer" => value
            .as_f64()
            .is_some_and(kiana_json::js_num::is_js_integer),
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "null" => value.is_null(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// `coercePrimitiveByType`
// ---------------------------------------------------------------------------

/// `String(value)`（`coercePrimitiveByType` 的 `string` 分支）。
fn js_to_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => {
            kiana_json::js_num::format_js_number(number.as_f64().unwrap_or(f64::NAN))
        }
        Value::String(text) => text.clone(),
        Value::Array(_) | Value::Object(_) => kiana_json::js_string::js_string(Some(value)),
    }
}

/// `coercePrimitiveByType(value, type)`：按 `schema_type` 逐分支强制转换；各分支的转换语义是当前稳定契约（见 spec §2.2.28），改动需同步测试。
pub(crate) fn coerce_primitive_by_type(value: Value, schema_type: &str) -> Value {
    match schema_type {
        "number" => {
            if value.is_null() {
                return Value::from(0);
            }
            if let Value::String(text) = &value
                && !kiana_json::js_string::js_trim(text).is_empty()
            {
                let parsed = kiana_json::js_num::parse_js_number(text);
                if parsed.is_finite() {
                    return number_value(parsed);
                }
            }
            match value {
                Value::Bool(flag) => Value::from(if flag { 1 } else { 0 }),
                other => other,
            }
        }
        "integer" => {
            if value.is_null() {
                return Value::from(0);
            }
            if let Value::String(text) = &value
                && !kiana_json::js_string::js_trim(text).is_empty()
            {
                let parsed = kiana_json::js_num::parse_js_number(text);
                if kiana_json::js_num::is_js_integer(parsed) {
                    return number_value(parsed);
                }
            }
            match value {
                Value::Bool(flag) => Value::from(if flag { 1 } else { 0 }),
                other => other,
            }
        }
        "boolean" => {
            if value.is_null() {
                return Value::Bool(false);
            }
            match &value {
                Value::String(text) if text == "true" => Value::Bool(true),
                Value::String(text) if text == "false" => Value::Bool(false),
                Value::Number(number) => {
                    let number = number.as_f64().unwrap_or(f64::NAN);
                    if number == 1.0 {
                        Value::Bool(true)
                    } else if number == 0.0 {
                        Value::Bool(false)
                    } else {
                        value
                    }
                }
                _ => value,
            }
        }
        "string" => {
            if value.is_null() {
                return Value::String(String::new());
            }
            match &value {
                Value::Number(_) | Value::Bool(_) => Value::String(js_to_string(&value)),
                _ => value,
            }
        }
        "null" => match &value {
            // TS 用 `===`：空串、`0`、`false` 都命中（`0`/`false` 与 `""` 一样被折叠）。
            Value::String(text) if text.is_empty() => Value::Null,
            Value::Number(number) if number.as_f64() == Some(0.0) => Value::Null,
            Value::Bool(false) => Value::Null,
            _ => value,
        },
        _ => value,
    }
}

/// 把 `f64` 写成 JSON 数字（整数保持整数形态，`-0` 保持 `-0.0`）。
pub(crate) fn number_value(number: f64) -> Value {
    if number.is_finite()
        && kiana_json::js_num::is_js_integer(number)
        && number.abs() <= 9_007_199_254_740_992.0
    {
        if number == 0.0 && number.is_sign_negative() {
            return Value::Number(serde_json::Number::from_f64(-0.0).expect("finite"));
        }
        return Value::from(number as i64);
    }
    Value::Number(
        serde_json::Number::from_f64(number).unwrap_or_else(|| serde_json::Number::from(0)),
    )
}

// ---------------------------------------------------------------------------
// `coerceWithJsonSchema` 家族
// ---------------------------------------------------------------------------

/// `coerceWithJsonSchema(value, schema)`（顺序固定，见 spec §2.2.28）。
///
/// 处理顺序是契约：先 `allOf`，再 `anyOf`/`oneOf`，再按 `type` 做原始类型转换，最后才是
/// 对象/数组的成员递归，所以同时带多个关键字时，后面的步骤看到的是前面转换后的值。
/// `type` 是数组（联合）且值已匹配其中任一类型时跳过原始类型转换，避免把已经合法的值
/// （例如 `["string", "number"]` 下的 `"1"`）强行改成另一种类型。函数只读 `schema`，
/// 入参 `value` 按值传入：对象/数组分支原地改写它并递归，联合分支则为每个候选克隆一份。
pub fn coerce_with_json_schema(value: Value, schema: &Value) -> Value {
    let mut next = value;

    if let Some(Value::Array(all_of)) = get(schema, "allOf") {
        for nested in all_of {
            next = coerce_with_json_schema(next, nested);
        }
    }
    if let Some(Value::Array(any_of)) = get(schema, "anyOf") {
        next = coerce_with_union_schema(next, any_of);
    }
    if let Some(Value::Array(one_of)) = get(schema, "oneOf") {
        next = coerce_with_union_schema(next, one_of);
    }

    let schema_types = get_schema_types(schema);
    let matches_union_member = schema_types.len() > 1
        && schema_types
            .iter()
            .any(|schema_type| matches_json_type(&next, schema_type));
    if !schema_types.is_empty() && !matches_union_member {
        for schema_type in &schema_types {
            let candidate = coerce_primitive_by_type(next.clone(), schema_type);
            if !kiana_json::js_string::js_strict_equal(&candidate, &next) {
                next = candidate;
                break;
            }
        }
    }

    let includes_object = schema_types
        .iter()
        .any(|schema_type| schema_type == "object");
    if includes_object && is_object_not_array(&next) {
        apply_schema_object_coercion(&mut next, schema);
    }
    let includes_array = schema_types
        .iter()
        .any(|schema_type| schema_type == "array");
    if includes_array && next.is_array() {
        apply_schema_array_coercion(&mut next, schema);
    }
    next
}

/// `coerceWithUnionSchema(value, schemas)`：先原值、再逐个 `structuredClone` + 强转。
fn coerce_with_union_schema(value: Value, schemas: &[Value]) -> Value {
    for schema in schemas {
        if check_json_schema(schema, &value) {
            return value;
        }
    }
    for schema in schemas {
        let candidate = value.clone();
        let coerced = coerce_with_json_schema(candidate, schema);
        if check_json_schema(schema, &coerced) {
            return coerced;
        }
    }
    value
}

/// `applySchemaObjectCoercion(value, schema)`：`properties` 已有的键，再 `additionalProperties`
/// 里不在 `properties` 键集合中的键。
fn apply_schema_object_coercion(value: &mut Value, schema: &Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let properties = get(schema, "properties").and_then(Value::as_object);
    if let Some(properties) = properties {
        let defined_keys: HashSet<&String> = properties.keys().collect();
        for (key, property_schema) in properties {
            if let Some(current) = object.get_mut(key) {
                *current = coerce_with_json_schema(std::mem::take(current), property_schema);
            }
        }
        let _ = defined_keys;
    }
    if let Some(additional) = get(schema, "additionalProperties")
        && additional.is_object()
    {
        let defined: HashSet<String> = properties
            .map(|properties| properties.keys().cloned().collect())
            .unwrap_or_default();
        for (key, property_value) in object.iter_mut() {
            if defined.contains(key) {
                continue;
            }
            *property_value = coerce_with_json_schema(std::mem::take(property_value), additional);
        }
    }
}

/// `applySchemaArrayCoercion(value, schema)`：元组按索引（缺位跳过），数组对全部元素。
fn apply_schema_array_coercion(value: &mut Value, schema: &Value) {
    let Some(items) = value.as_array_mut() else {
        return;
    };
    if let Some(Value::Array(tuple)) = get(schema, "items") {
        for (index, item) in items.iter_mut().enumerate() {
            if let Some(item_schema) = tuple.get(index) {
                *item = coerce_with_json_schema(std::mem::take(item), item_schema);
            }
        }
        return;
    }
    if let Some(item_schema) = get(schema, "items")
        && item_schema.is_object()
    {
        for item in items.iter_mut() {
            *item = coerce_with_json_schema(std::mem::take(item), item_schema);
        }
    }
}

// ---------------------------------------------------------------------------
// `normalizeOptionalNulls`
// ---------------------------------------------------------------------------

/// `normalizeOptionalNulls(value, schema)`：把“可选字段显式 null”变成“缺省”。
///
/// 删除条件（全部满足）：键存在、值为 `null`、键不在当前对象的 `required` 里、
/// `propertySchema.$ref` 不是字符串、子 schema 校验 `null` **失败**。
///
/// 只遍历 schema 的 `properties` 直接声明的键；数组按 `items`（元组按位、其余逐元素）递归，
/// 标量原样返回。函数原地修改 `value`，没有返回值，也不做 schema 合法性检查。
///
/// 已知差异：属性子 schema 含非法 `pattern` 时，TS 的 `getSubSchemaValidator` 在 `Compile`
/// 抛错时返回 `undefined`，`undefined === false` 为 false，null 会保留；本实现没有
/// `Compile` 步骤，`check_json_schema(property_schema, Null)` 按常规规则判定（例如
/// `type: "string"` 会拒绝 null），于是 null 被删除。该差异只在 schema 自身非法时出现，
/// [`validate_tool_arguments`] 随后会因非法 pattern 报错（见 `validate_schema_patterns`），
/// 不会静默接受参数。
pub fn normalize_optional_nulls(value: &mut Value, schema: &Value) {
    if value.is_array() {
        let items_len = value.as_array().map(Vec::len).unwrap_or(0);
        if let Some(Value::Array(tuple)) = get(schema, "items") {
            let Some(array) = value.as_array_mut() else {
                return;
            };
            for index in 0..items_len {
                if let Some(item_schema) = tuple.get(index) {
                    let Some(item) = array.get_mut(index) else {
                        continue;
                    };
                    normalize_optional_nulls(item, item_schema);
                }
            }
        } else if let Some(item_schema) = get(schema, "items") {
            let Some(array) = value.as_array_mut() else {
                return;
            };
            for item in array.iter_mut() {
                normalize_optional_nulls(item, item_schema);
            }
        }
        return;
    }
    if !value.is_object() {
        return;
    }
    let Some(properties) = get(schema, "properties").and_then(Value::as_object) else {
        return;
    };
    let required: HashSet<String> = match get(schema, "required") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        _ => HashSet::new(),
    };
    let Some(object) = value.as_object_mut() else {
        return;
    };
    for (key, property_schema) in properties {
        if !object.contains_key(key) {
            continue;
        }
        let is_null = object.get(key).is_some_and(Value::is_null);
        let has_ref = matches!(get(property_schema, "$ref"), Some(Value::String(_)));
        if is_null
            && !required.contains(key)
            && !has_ref
            && !check_json_schema(property_schema, &Value::Null)
        {
            object.shift_remove(key);
        } else if let Some(current) = object.get_mut(key) {
            normalize_optional_nulls(current, property_schema);
        }
    }
}

// ---------------------------------------------------------------------------
// 校验错误与消息（TypeBox 1.3.27 `en_US` 本地化）
// ---------------------------------------------------------------------------

/// TypeBox `Settings.maxErrors` 的默认值。
///
/// 只限制 [`collect_json_schema_errors`] 返回的条数：`Context` 达到上限后丢弃后续错误，
/// [`check_json_schema`] 的判定不受影响（判定引擎不记录错误）。
pub const MAX_ERRORS: usize = 8;

/// 一条校验错误（`TLocalizedValidationError`）。
///
/// `keyword` 与 `params` 决定 `message`；`instance_path` 仅参与错误路径的格式化
/// （`format_validation_path`），不影响 `message`。`params` 的形状随关键字变化
/// （例如 `type` 是 `{"type": ...}`、`required` 是 `{"requiredProperties": [...]}`）。
/// 这三个字段保持 TypeBox 的原始形状，`tests/validation.rs` 会与 TS 参照实现逐字段比对，
/// 因此不要为了“更好读”改写 `params` 的键名。
#[derive(Debug, Clone, PartialEq)]
pub struct ValidationError {
    /// 关键字名（`type`、`required`、`anyOf`…）。
    pub keyword: String,
    /// 原始 JSON Pointer（**不**做 `~`/`/` 转义，与 typebox 一致）。
    pub instance_path: String,
    /// `en_US` 本地化后的消息。
    pub message: String,
    /// 关键字参数。
    pub params: JsonValue,
}

impl ValidationError {
    fn new(keyword: &str, instance_path: String, params: JsonValue) -> Self {
        let message = localized_message(keyword, &params);
        ValidationError {
            keyword: keyword.to_string(),
            instance_path,
            message,
            params,
        }
    }
}

/// `en_US` 本地化（`typebox/build/system/locale/en_US.mjs`）。
///
/// 文案是模型可见契约：`tests/validation.rs` 的黄金 fixture 逐字比对。未列出的关键字退回
/// `an unknown validation error occurred`；`type` 的 `params` 形状不对时也退回同一句。
/// 不要为了“更友好”改写这些字符串。
fn localized_message(keyword: &str, params: &Value) -> String {
    let text = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let number = |key: &str| -> String {
        match params.get(key) {
            Some(Value::Number(value)) => {
                kiana_json::js_num::format_js_number(value.as_f64().unwrap_or(f64::NAN))
            }
            Some(Value::String(text)) => text.clone(),
            _ => String::new(),
        }
    };
    let list = |key: &str| -> String {
        match params.get(key) {
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| match item {
                    Value::String(text) => text.clone(),
                    other => kiana_json::js_string::js_string(Some(other)),
                })
                .collect::<Vec<_>>()
                .join(", "),
            _ => String::new(),
        }
    };
    match keyword {
        "additionalProperties" => "must not have additional properties".to_string(),
        "anyOf" => "must match a schema in anyOf".to_string(),
        "boolean" => "schema is false".to_string(),
        "const" => "must be equal to constant".to_string(),
        "contains" => "must contain at least 1 valid item".to_string(),
        "dependencies" | "dependentRequired" => format!(
            "must have properties {} when property {} is present",
            list("dependencies"),
            text("property")
        ),
        "enum" => "must be equal to one of the allowed values".to_string(),
        "exclusiveMaximum" | "exclusiveMinimum" | "maximum" | "minimum" => {
            format!("must be {} {}", text("comparison"), number("limit"))
        }
        "format" => format!("must match format \"{}\"", text("format")),
        "if" => format!("must match \"{}\" schema", text("failingKeyword")),
        "maxItems" => format!("must not have more than {} items", number("limit")),
        "maxLength" => format!("must not have more than {} characters", number("limit")),
        "maxProperties" => format!("must not have more than {} properties", number("limit")),
        "minItems" => format!("must not have fewer than {} items", number("limit")),
        "minLength" => format!("must not have fewer than {} characters", number("limit")),
        "minProperties" => format!("must not have fewer than {} properties", number("limit")),
        "multipleOf" => format!("must be multiple of {}", number("multipleOf")),
        "not" => "must not be valid".to_string(),
        "oneOf" => "must match exactly one schema in oneOf".to_string(),
        "pattern" => format!("must match pattern \"{}\"", text("pattern")),
        "propertyNames" => format!("property names {} are invalid", list("propertyNames")),
        "required" => format!(
            "must have required properties {}",
            list("requiredProperties")
        ),
        "type" => match params.get("type") {
            Some(Value::String(name)) => format!("must be {name}"),
            Some(Value::Array(names)) => format!(
                "must be either {}",
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
            _ => "an unknown validation error occurred".to_string(),
        },
        "unevaluatedItems" => "must not have unevaluated items".to_string(),
        "unevaluatedProperties" => "must not have unevaluated properties".to_string(),
        "uniqueItems" => "must not have duplicate items".to_string(),
        _ => "an unknown validation error occurred".to_string(),
    }
}

/// `formatValidationPath(error)`：`required` 带上首个缺失属性名，其余按 `a.b.0` 形式。
///
/// 返回的是给模型看的路径：去掉开头的 `/`，其余 `/` 换成 `.`；因此键名里的 `.` 或 `/`
/// 会与层级分隔符混淆（与 TS 的 `replace` 行为相同，不做转义）。空路径时：非 `required`
/// 错误返回 `root`；`required` 错误返回首个缺失属性名，而不是 `root`。
pub fn format_validation_path(error: &ValidationError) -> String {
    let base = |path: &str| -> String { path.strip_prefix('/').unwrap_or(path).replace('/', ".") };
    if error.keyword == "required"
        && let Some(required) = error
            .params
            .get("requiredProperties")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(Value::as_str)
    {
        let base_path = base(&error.instance_path);
        return if base_path.is_empty() {
            required.to_string()
        } else {
            format!("{base_path}.{required}")
        };
    }
    let path = base(&error.instance_path);
    if path.is_empty() {
        "root".to_string()
    } else {
        path
    }
}

// ---------------------------------------------------------------------------
// 引擎上下文
// ---------------------------------------------------------------------------

/// `CheckContext` 的一个栈帧：已求值的键/索引（`unevaluated*` 用）。
#[derive(Debug, Default, Clone)]
struct ContextFrame {
    keys: HashSet<String>,
    indices: HashSet<usize>,
}

/// `ErrorContext`/`CheckContext` 的合并体：错误列表 + 已求值的键/索引栈。
///
/// TypeBox 的 `Push`/`Pop` 会压入/弹出新的键集合（嵌套 schema 校验的属性键不能泄漏到外层
/// 对象的 `unevaluated*` 集合）；`Merge` 只合并其他上下文**栈顶**的集合。`errors` 跨帧共享。
#[derive(Debug, Clone)]
struct Context {
    errors: Vec<ValidationError>,
    frames: Vec<ContextFrame>,
}

impl Context {
    fn new() -> Self {
        Context {
            errors: Vec::new(),
            frames: vec![ContextFrame::default()],
        }
    }

    fn at_capacity(&self) -> bool {
        self.errors.len() >= MAX_ERRORS
    }

    fn add_error(&mut self, error: ValidationError) -> bool {
        // 返回 `false` 是为了对齐 TypeBox `context.AddError(...)` 的调用写法，所有调用点都只
        // 依赖副作用。容量满后静默丢弃，保证列表与 TS 的前 `MAX_ERRORS` 条一致。
        if !self.at_capacity() {
            self.errors.push(error);
        }
        false
    }

    /// `CheckContext.Push`。
    fn push(&mut self) -> bool {
        self.frames.push(ContextFrame::default());
        true
    }

    /// `CheckContext.Pop`。
    fn pop(&mut self) -> bool {
        self.frames.pop();
        true
    }

    fn top(&self) -> &ContextFrame {
        self.frames.last().expect("context always has a frame")
    }

    fn top_mut(&mut self) -> &mut ContextFrame {
        self.frames.last_mut().expect("context always has a frame")
    }

    fn keys(&self) -> &HashSet<String> {
        &self.top().keys
    }

    fn indices(&self) -> &HashSet<usize> {
        &self.top().indices
    }

    fn add_key(&mut self, key: &str) -> bool {
        self.top_mut().keys.insert(key.to_string());
        true
    }

    fn add_index(&mut self, index: usize) -> bool {
        self.top_mut().indices.insert(index);
        true
    }

    /// `CheckContext.Merge(results)`：只合并每个上下文栈顶的键/索引。
    fn merge(&mut self, other: &Context) -> bool {
        let keys = other.keys().clone();
        let indices = other.indices().clone();
        let top = self.top_mut();
        top.keys.extend(keys);
        top.indices.extend(indices);
        true
    }
}

/// `CheckSchemaPushStack` / `ErrorSchemaPushStack`：`(context.Push() && check(...)) && context.Pop()`。
///
/// 注意与 TypeBox 一致的短路语义：子 schema 校验失败时 **不会** `Pop`，失败分支的帧会留在栈上，
/// 后续 `add_key`/`add_index` 写入该帧，`unevaluated*` 读到的是这个帧（TypeBox 的实测行为，
/// 例如先通过后失败的 `properties` 列表会把先前求值的键一并列为未求值）。
fn push_checked(context: &mut Context, check: impl FnOnce(&mut Context) -> bool) -> bool {
    context.push();
    let result = check(context);
    if result {
        context.pop();
    }
    result
}

// ---------------------------------------------------------------------------
// 字符串长度（TypeBox 计的是**字素簇**，不是 UTF-16 码元）
// ---------------------------------------------------------------------------

fn is_high_surrogate(value: u32) -> bool {
    (0xD800..=0xDBFF).contains(&value)
}

fn is_combining_mark(value: u32) -> bool {
    (0x0300..=0x036F).contains(&value)
        || (0x1AB0..=0x1AFF).contains(&value)
        || (0x1DC0..=0x1DFF).contains(&value)
        || (0xFE20..=0xFE2F).contains(&value)
}

fn is_variation_selector(value: u32) -> bool {
    (0xFE00..=0xFE0F).contains(&value)
}

fn is_regional_indicator(value: u32) -> bool {
    (0x1F1E6..=0x1F1FF).contains(&value)
}

/// `IsGraphemeCodePoint`：只有 `>= 0x0300` 的码点才可能落在字素簇内部。
fn is_grapheme_code_point(value: u32) -> bool {
    value >= 0x0300
        && (is_high_surrogate(value)
            || is_combining_mark(value)
            || is_variation_selector(value)
            || value == 0x200D)
}

/// `String.prototype.length` 的 UTF-16 视图 + 逐码点的字素簇切分。
struct Graphemes {
    /// UTF-16 码元（`charCodeAt` 的取值域）。
    units: Vec<u16>,
    /// 每个码点：`(码点, UTF-16 起始位置)`。
    points: Vec<(u32, usize)>,
}

impl Graphemes {
    fn new(text: &str) -> Self {
        let units: Vec<u16> = text.encode_utf16().collect();
        let mut points = Vec::new();
        let mut offset = 0usize;
        for ch in text.chars() {
            points.push((ch as u32, offset));
            offset += ch.len_utf16();
        }
        Graphemes { units, points }
    }

    fn point(&self, index: usize) -> Option<u32> {
        self.points.get(index).map(|(value, _)| *value)
    }

    /// `ConsumeModifiers`：吞掉组合记号与变体选择符。
    fn consume_modifiers(&self, mut index: usize) -> usize {
        while let Some(point) = self.point(index) {
            if is_combining_mark(point) || is_variation_selector(point) {
                index += 1;
            } else {
                break;
            }
        }
        index
    }

    /// `NextGraphemeClusterIndex`。
    fn next_cluster(&self, start: usize) -> usize {
        let start_point = self.point(start).unwrap_or(0);
        let mut end = self.consume_modifiers(start + 1);
        // 多 ZWJ 序列（emoji 家庭等）。
        while end < self.points.len().saturating_sub(1) && self.point(end) == Some(0x200D) {
            end += 1 + usize::from(self.point(end + 1).is_some_and(|point| point > 0xFFFF));
            end = self.consume_modifiers(end);
        }
        // 区域指示符对（国旗）。
        if is_regional_indicator(start_point) && self.point(end).is_some_and(is_regional_indicator)
        {
            end += 1;
        }
        end
    }

    fn grapheme_count(&self) -> usize {
        let mut count = 0;
        let mut index = 0;
        while index < self.points.len() {
            index = self.next_cluster(index);
            count += 1;
        }
        count
    }

    /// `IsMinLengthFast`。
    fn is_min_length(&self, min_length: usize) -> bool {
        if min_length == 0 {
            return true;
        }
        // 码元数是字素数的上界。
        if self.units.len() < min_length {
            return false;
        }
        let mut index = 0;
        loop {
            // `charCodeAt` 越界返回 NaN，`IsGraphemeCodePoint(NaN)` 为 false。
            if self
                .units
                .get(index)
                .is_some_and(|unit| is_grapheme_code_point(*unit as u32))
            {
                return self.grapheme_count() >= min_length;
            }
            index += 1;
            if index >= min_length {
                return true;
            }
        }
    }

    /// `IsMaxLengthFast`。
    fn is_max_length(&self, max_length: usize) -> bool {
        if self.units.len() <= max_length {
            return true;
        }
        let mut index = 0;
        loop {
            if self
                .units
                .get(index)
                .is_some_and(|unit| is_grapheme_code_point(*unit as u32))
            {
                return self.grapheme_count() <= max_length;
            }
            index += 1;
            if index > max_length {
                return false;
            }
        }
    }
}

/// `Guard.IsMinLength`。
fn is_min_length(text: &str, min_length: usize) -> bool {
    Graphemes::new(text).is_min_length(min_length)
}

/// `Guard.IsMaxLength`。
fn is_max_length(text: &str, max_length: usize) -> bool {
    Graphemes::new(text).is_max_length(max_length)
}

// ---------------------------------------------------------------------------
// 正则（TypeBox 用 JS `RegExp(pattern, 'u')`）
// ---------------------------------------------------------------------------

/// JS 正则（`new RegExp(pattern, 'u').test(text)`）。用 `regress` 精确复刻 ECMAScript
/// 语义：`\d`/`\w` 只认 ASCII、`\s` 含 U+FEFF、`.` 不匹配四个行终止符、`$` 只匹配输入末尾、
/// 支持 lookaround/反向引用。`fancy_regex` 的 Unicode 字符类与 JS 不同，不能用在这里。
type JsRegex = regress::Regex;

/// 编译 JS 风格正则（`u` 标志）。非法模式的抛错行为见
/// [`validate_schema_patterns`]；直接校验路径上退化为「永不匹配」。
fn compile_pattern(pattern: &str) -> Option<JsRegex> {
    JsRegex::with_flags(pattern, "u").ok()
}

fn regex_test(pattern: Option<&JsRegex>, text: &str) -> bool {
    pattern.is_some_and(|regex| regex.find(text).is_some())
}

/// 对必存正则做 `is_match`（`fancy_regex` 会返回 `Result`；不可达的错误按不匹配处理）。
fn regex_is_match(pattern: &fancy_regex::Regex, text: &str) -> bool {
    pattern.is_match(text).unwrap_or(false)
}

/// `GetPropertyKeyAsPattern`：`key.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')`。
fn escape_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for ch in key.chars() {
        if matches!(
            ch,
            '.' | '*' | '+' | '?' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// `GetPropertiesPattern`：`(^p1$|^p2$|...)`，没有任何键时是 `(?!)`。
fn properties_pattern(schema: &Value) -> Option<JsRegex> {
    let mut patterns: Vec<String> = Vec::new();
    if let Some(Value::Object(map)) = get(schema, "patternProperties") {
        patterns.extend(map.keys().cloned());
    }
    if let Some(Value::Object(map)) = get(schema, "properties") {
        patterns.extend(map.keys().map(|key| format!("^{}$", escape_key(key))));
    }
    if patterns.is_empty() {
        patterns.push("(?!)".to_string());
    }
    compile_pattern(&format!("({})", patterns.join("|")))
}

// ---------------------------------------------------------------------------
// TypeBox `Compile(schema)` 的模式语法检查
// ---------------------------------------------------------------------------

/// TypeBox 的 `Compile` 在构建校验器时用 `new RegExp(pattern, 'u')` 编译每个 `pattern` 与
/// `patternProperties` 键；非法模式让 `validateToolArguments` 直接抛 `SyntaxError`
/// （`Invalid regular expression: /<pattern>/u: <reason>`）。Rust 侧在 `Value.Convert` 之后、
/// 强制转换之前做同样的可达性遍历（不遍历未被 `$ref` 引用的 `$defs`，与 `Compile` 一致）。
fn validate_schema_patterns(schema: &Value) -> Result<(), String> {
    let mut visited = HashSet::new();
    walk_schema_patterns(schema, schema, &mut visited)
}

fn walk_schema_patterns(
    node: &Value,
    root: &Value,
    visited: &mut HashSet<usize>,
) -> Result<(), String> {
    if !node.is_object() {
        return Ok(());
    }
    if !visited.insert(node as *const Value as usize) {
        // 按节点地址去重：`$ref` 可以在 schema 图里形成环，每个节点最多预检一次。
        return Ok(());
    }
    if let Some(Value::String(pattern)) = get(node, "pattern") {
        check_js_pattern(pattern)?;
    }
    if let Some(Value::Object(patterns)) = get(node, "patternProperties") {
        for pattern in patterns.keys() {
            check_js_pattern(pattern)?;
        }
    }
    if let Some(Value::String(reference)) = get(node, "$ref")
        && let Some(pointer) = reference.strip_prefix('#')
        && let Some(target) = root.pointer(pointer)
    {
        // 只跟随 `#/...` 形式的 JSON Pointer；`#anchor` / `#dynamicAnchor` 不会在这里被检查。
        walk_schema_patterns(target, root, visited)?;
    }
    for keyword in [
        "additionalProperties",
        "additionalItems",
        "unevaluatedProperties",
        "unevaluatedItems",
        "propertyNames",
        "contains",
        "not",
        "if",
        "then",
        "else",
        "contentSchema",
    ] {
        if let Some(child) = get(node, keyword) {
            walk_schema_patterns(child, root, visited)?;
        }
    }
    if let Some(child) = get(node, "items") {
        match child {
            Value::Array(items) => {
                for item in items {
                    walk_schema_patterns(item, root, visited)?;
                }
            }
            _ => walk_schema_patterns(child, root, visited)?,
        }
    }
    if let Some(Value::Array(items)) = get(node, "prefixItems") {
        for item in items {
            walk_schema_patterns(item, root, visited)?;
        }
    }
    // `$defs`/`definitions` 只在被 `$ref` 引用时才会被 `Compile` 展开，因此这里故意
    // **不**遍历它们的内容（只看 `$ref` 解析到的目标）。
    for keyword in ["properties", "patternProperties", "dependentSchemas"] {
        if let Some(Value::Object(map)) = get(node, keyword) {
            for child in map.values() {
                walk_schema_patterns(child, root, visited)?;
            }
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(Value::Array(items)) = get(node, keyword) {
            for item in items {
                walk_schema_patterns(item, root, visited)?;
            }
        }
    }
    Ok(())
}

fn check_js_pattern(pattern: &str) -> Result<(), String> {
    match JsRegex::with_flags(pattern, "u") {
        Ok(_) => Ok(()),
        Err(error) => Err(format!(
            "Invalid regular expression: /{pattern}/u: {}",
            js_regex_error_detail(pattern, &error.text)
        )),
    }
}

/// 把 `regress` 的解析错误文本映射为 V8（Node）的 `SyntaxError` 理由，消息逐字对齐：
/// `new RegExp(pattern, 'u')` 抛 `Invalid regular expression: /<pattern>/u: <reason>`。
///
/// 覆盖常见错误（未闭合字符类/分组、量词、转义、属性名、命名组、反向引用）；极少数
/// 语法错误在两个引擎里的分类不同（例如 `\c` 的残缺控制转义），此时保留 V8 的通用文案。
fn js_regex_error_detail(pattern: &str, detail: &str) -> String {
    match detail {
        "Unbalanced bracket" => "Unterminated character class".to_string(),
        "Unbalanced parenthesis" => {
            if has_unmatched_closing_paren(pattern) {
                "Unmatched ')'".to_string()
            } else {
                "Unterminated group".to_string()
            }
        }
        "Invalid atom character" => {
            if pattern.contains('*') || pattern.contains('+') || pattern.contains('?') {
                "Nothing to repeat".to_string()
            } else {
                "Lone quantifier brackets".to_string()
            }
        }
        "Invalid quantifier" | "Quantifier not allowed here" => {
            if let Some((low, high)) = reversed_quantifier(pattern) {
                let _ = (low, high);
                "numbers out of order in {} quantifier".to_string()
            } else if pattern.contains('{') && !pattern.contains('}') {
                "Incomplete quantifier".to_string()
            } else {
                "Invalid quantifier".to_string()
            }
        }
        "Range values reversed, start char code is greater than end char code." => {
            "Range out of order in character class".to_string()
        }
        "Invalid character range" => "Invalid character class".to_string(),
        "Incomplete escape" | "Unterminated escape" => "\\ at end of pattern".to_string(),
        "Invalid token at named capture group identifier" => {
            if pattern.contains("(?P") {
                "Invalid group".to_string()
            } else {
                "Invalid capture group name".to_string()
            }
        }
        "Invalid character escape" => "Invalid escape".to_string(),
        "Invalid unicode escape" => "Invalid Unicode escape".to_string(),
        "Invalid property name" => "Invalid property name".to_string(),
        "Duplicate capture group name" => "Duplicate capture group name".to_string(),
        other if other.starts_with("Backreference to invalid named capture group") => {
            "Invalid named capture referenced".to_string()
        }
        other => other.to_string(),
    }
}

/// 粗略判断 `)` 是否多于 `(`（V8 对多余的 `)` 报 `Unmatched ')'`，对未闭合的 `(` 报
/// `Unterminated group`）。跳过字符类与转义。
fn has_unmatched_closing_paren(pattern: &str) -> bool {
    let mut depth = 0i32;
    let mut in_class = false;
    let mut escaped = false;
    for ch in pattern.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            '(' if !in_class => depth += 1,
            ')' if !in_class => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// 找到第一个 `{m,n}` 且 `m > n` 的量词。
fn reversed_quantifier(pattern: &str) -> Option<(u64, u64)> {
    let bytes = pattern.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'{' {
            index += 1;
            continue;
        }
        let end = pattern[index..].find('}').map(|offset| index + offset)?;
        let body = &pattern[index + 1..end];
        if let Some((low, high)) = body.split_once(',')
            && let (Ok(low), Ok(high)) = (low.trim().parse::<u64>(), high.trim().parse::<u64>())
            && low > high
        {
            return Some((low, high));
        }
        index = end + 1;
    }
    None
}

// ---------------------------------------------------------------------------
// `format` 关键字（TypeBox 内置格式表）
// ---------------------------------------------------------------------------

fn regex_from(source: &str) -> fancy_regex::Regex {
    fancy_regex::Regex::new(source).expect("内置正则必须可编译")
}

/// `IsDate`（RFC 3339 的 `YYYY-MM-DD`，含闰年与每月天数）。
fn is_date_format(text: &str) -> bool {
    const DAYS: [u32; 13] = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    static DATE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let date = DATE.get_or_init(|| regex::Regex::new(r"^(\d\d\d\d)-(\d\d)-(\d\d)$").expect("date"));
    let Some(captures) = date.captures(text) else {
        return false;
    };
    let parse = |index: usize| -> u32 { captures[index].parse().unwrap_or(0) };
    let year = parse(1);
    let month = parse(2);
    let day = parse(3);
    let is_leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max_day = if month == 2 && is_leap {
        29
    } else {
        DAYS.get(month as usize).copied().unwrap_or(0)
    };
    (1..=12).contains(&month) && day >= 1 && day <= max_day
}

/// `IsTime`（`strictTimeZone = true`）。
fn is_time_format(text: &str) -> bool {
    static TIME: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let time = TIME.get_or_init(|| {
        regex::Regex::new(r"^(\d\d):(\d\d):(\d\d)(?:\.\d+)?(?:([Zz])|([+-])(\d\d):(\d\d))?$")
            .expect("time")
    });
    let Some(captures) = time.captures(text) else {
        return false;
    };
    // 严格时区：必须带 `Z`/`z` 或偏移。
    if captures.get(4).is_none() && captures.get(5).is_none() {
        return false;
    }
    let parse = |index: usize| -> i64 {
        captures
            .get(index)
            .map_or(0, |m| m.as_str().parse().unwrap_or(0))
    };
    let (hour, minute, second) = (parse(1), parse(2), parse(3));
    if hour > 23 || minute > 59 || second > 60 {
        return false;
    }
    let tz_hour = parse(6);
    let tz_minute = parse(7);
    if tz_hour > 23 || tz_minute > 59 {
        return false;
    }
    if second < 60 {
        return true;
    }
    // 闰秒：必须落在 23:59:60 UTC。
    let sign = if captures.get(5).is_some_and(|m| m.as_str() == "-") {
        -1
    } else {
        1
    };
    let total = hour * 60 + minute - sign * (tz_hour * 60 + tz_minute);
    ((total % 1440) + 1440) % 1440 == 1439
}

/// `IsDateTime`：按 `T`/`t` 切成两段，分别过 `IsDate` 与 `IsTime`。
fn is_date_time_format(text: &str) -> bool {
    let mut parts = text.split(['T', 't']);
    let (Some(date), Some(time)) = (parts.next(), parts.next()) else {
        return false;
    };
    // `split` 后的第三段（多出来的 `T`）让 `dateTime.length === 2` 失败。
    if parts.next().is_some() {
        return false;
    }
    is_date_format(date) && is_time_format(time)
}

/// `IsHostname`（IDNA 的 LABEL 校验，不含 Bidi）。
fn is_hostname_format(text: &str) -> bool {
    let length = text.encode_utf16().count();
    if length == 0 || length > 253 {
        return false;
    }
    if text.ends_with('.') {
        return false;
    }
    text.split('.').all(is_hostname_label)
}

/// `IsHostname` 的单个标签：长度 1..=63，且是 punycode 标签或 ASCII LDH 标签。
fn is_hostname_label(label: &str) -> bool {
    let length = label.encode_utf16().count();
    if length == 0 || length > 63 {
        return false;
    }
    is_puny_label(label) || is_ascii_label(label)
}

/// `IsAsciiLabel`：不以 `-` 起止、不以 ACE 前缀开头、内容为 `[a-zA-Z0-9-]`。
fn is_ascii_label(label: &str) -> bool {
    let bytes = label.as_bytes();
    if bytes.is_empty() || bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
        return false;
    }
    // TS: `RE_RULE_NOT_RESERVED_ACE = /^(?!..--)/`。按**UTF-16 码元**比较第 3、4 个字符；
    // 这里用字节比较即可：多字节字符的第 2 个字节不可能是 `-`，且随后的 ASCII LDH 检查
    // 一定会拒绝非 ASCII 标签。绝不按字节切 `&str`（会在字符边界上 panic）。
    if bytes.len() >= 4 && bytes[2] == b'-' && bytes[3] == b'-' {
        return false;
    }
    bytes
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
}

/// `IsPunyLabel`：`xn--` 前缀 + 可解码 + 解码后含非 ASCII + Unicode 标签规则。
fn is_puny_label(label: &str) -> bool {
    let lowercase = label.to_lowercase();
    if !lowercase.starts_with("xn--") {
        return false;
    }
    let Some(body) = lowercase.get(4..) else {
        return false;
    };
    // TS: `if (body.lastIndexOf("-") === 0) return false`——只有 `-` 本身或
    // `-` 是**最后一个**连字符（即 0 个基础字符）时才畸形。
    if body.rfind('-') == Some(0) {
        return false;
    }
    let Some(decoded) = punycode_decode(body) else {
        return false;
    };
    if decoded.is_ascii() {
        return false;
    }
    is_unicode_label(&decoded)
}

/// RFC 3492 的 punycode 解码。
fn punycode_decode(value: &str) -> Option<String> {
    const BASE: u32 = 36;
    const TMIN: u32 = 1;
    const TMAX: u32 = 26;
    const SKEW: u32 = 38;
    const DAMP: u32 = 700;
    const INITIAL_BIAS: u32 = 72;
    const INITIAL_N: u32 = 128;

    fn adapt(mut delta: u32, num_points: u32, first_time: bool) -> u32 {
        delta = if first_time { delta / DAMP } else { delta >> 1 };
        delta += delta / num_points;
        let mut k = 0;
        while delta > ((BASE - TMIN) * TMAX) >> 1 {
            delta /= BASE - TMIN;
            k += BASE;
        }
        k + ((BASE - TMIN + 1) * delta) / (delta + SKEW)
    }

    // TS 的字符串按 UTF-16 码元索引：`lastIndexOf('-')`、`charCodeAt(j)`、`slice` 全部是
    // 码元下标。这里统一在 `units`（UTF-16 码元）上操作，绝不混用字节下标。
    let units: Vec<u16> = value.encode_utf16().collect();
    let delim = units.iter().rposition(|unit| *unit == u16::from(b'-'));
    let mut output: Vec<u32> = Vec::new();
    let mut in_index;
    if let Some(delim) = delim {
        if delim > 0 {
            for unit in &units[..delim] {
                if *unit >= 128 {
                    return None;
                }
                output.push(*unit as u32);
            }
        }
        in_index = delim + 1;
    } else {
        in_index = 0;
    }
    let mut n = INITIAL_N;
    let mut i: u32 = 0;
    let mut bias = INITIAL_BIAS;
    while in_index < units.len() {
        let old_i = i;
        let mut w: u32 = 1;
        let mut k = BASE;
        loop {
            let unit = *units.get(in_index)? as u32;
            in_index += 1;
            // TS 的 A-Z 分支被注释掉了（`(no-spec-coverage)`），大写字母因此是非法 digit。
            let digit = match unit {
                0x30..=0x39 => unit - 0x30 + 26,
                0x61..=0x7A => unit - 0x61,
                _ => return None,
            };
            i = i.checked_add(digit.checked_mul(w)?)?;
            let t = if k <= bias {
                TMIN
            } else if k >= bias + TMAX {
                TMAX
            } else {
                k - bias
            };
            if digit < t {
                break;
            }
            w = w.checked_mul(BASE - t)?;
            k += BASE;
        }
        let length = output.len() as u32 + 1;
        bias = adapt(i - old_i, length, old_i == 0);
        n = n.checked_add(i / length)?;
        i %= length;
        output.insert(i as usize, n);
        i += 1;
    }
    let mut text = String::new();
    for point in output {
        text.push(char::from_u32(point)?);
    }
    Some(text)
}

/// `IsUnicodeLabel`（Bidi 规则见 [`has_right_to_left`]）。
fn is_unicode_label(value: &str) -> bool {
    static NON_ASCII: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let non_ascii =
        NON_ASCII.get_or_init(|| regex::Regex::new(r"[^\x00-\x7F]").expect("non-ascii"));
    // RFC 5890 §2.3.2.1：A-label 不超过 63 个八位组。
    if non_ascii.is_match(value) {
        match punycode_encode(value) {
            Some(encoded) => {
                if encoded.len() + 4 > 63 {
                    return false;
                }
            }
            None => return false,
        }
    }
    if has_right_to_left(value) && !satisfies_bidi_rule(value) {
        return false;
    }
    let chars: Vec<char> = value.chars().collect();
    if chars.is_empty() {
        return false;
    }
    // RFC 5891 §4.2.3.2：不以 `-` 起止，第 3、4 个码点不同时为 `-`。
    if chars[0] == '-' || chars[chars.len() - 1] == '-' {
        return false;
    }
    if chars.len() >= 4 && chars[2] == '-' && chars[3] == '-' {
        return false;
    }
    if is_combining_mark_char(chars[0]) {
        return false;
    }
    let mut has_japanese = false;
    for (index, ch) in chars.iter().enumerate() {
        if is_rfc5892_disallowed(*ch) {
            return false;
        }
        if !is_permitted_category(*ch) {
            return false;
        }
        if is_japanese_script(*ch) {
            has_japanese = true;
        }
        let prev = index
            .checked_sub(1)
            .and_then(|prev| chars.get(prev))
            .copied();
        let next = chars.get(index + 1).copied();
        match *ch {
            // MIDDLE DOT（加泰罗尼亚语）：两侧必须是 `l`。
            '\u{00b7}' => {
                if prev != Some('l') || next != Some('l') {
                    return false;
                }
            }
            // 希腊语 KERAIA：后面必须是希腊字母。
            '\u{0375}' => {
                if next.is_none_or(|next| !is_greek_script(next)) {
                    return false;
                }
            }
            // 希伯来语 GERESH/GERSHAYIM：前面必须是希伯来字母。
            '\u{05f3}' | '\u{05f4}' => {
                if prev.is_none_or(|prev| !is_hebrew_script(prev)) {
                    return false;
                }
            }
            // ZWNJ：前面必须是非 ASCII 的 Virama。
            '\u{200c}' => {
                if prev.is_none_or(|prev| (prev as u32) < 0x0080 && !is_virama(prev)) {
                    return false;
                }
            }
            // ZWJ：前面必须是 Virama。
            '\u{200d}' if prev.is_none_or(|prev| !is_virama(prev)) => {
                return false;
            }
            _ => {}
        }
    }
    // RFC 5892 A.7：出现片假名中点时必须同时出现日文假名/汉字。
    if value.contains('\u{30fb}') && !has_japanese {
        return false;
    }
    true
}

/// RFC 3492 的 punycode 编码（只用于长度检查）。
fn punycode_encode(value: &str) -> Option<String> {
    const BASE: u32 = 36;
    const TMIN: u32 = 1;
    const TMAX: u32 = 26;
    const SKEW: u32 = 38;
    const DAMP: u32 = 700;
    const INITIAL_BIAS: u32 = 72;
    const INITIAL_N: u32 = 128;

    fn adapt(mut delta: u32, num_points: u32, first_time: bool) -> u32 {
        delta = if first_time { delta / DAMP } else { delta >> 1 };
        delta += delta / num_points;
        let mut k = 0;
        while delta > ((BASE - TMIN) * TMAX) >> 1 {
            delta /= BASE - TMIN;
            k += BASE;
        }
        k + ((BASE - TMIN + 1) * delta) / (delta + SKEW)
    }

    fn encode_digit(digit: u32) -> char {
        // 0..=25 -> a..=z，26..=35 -> 0..=9。
        if digit < 26 {
            char::from(b'a' + digit as u8)
        } else {
            char::from(b'0' + (digit - 26) as u8)
        }
    }

    let input: Vec<u32> = value.chars().map(|ch| ch as u32).collect();
    let mut output = String::new();
    for point in &input {
        if *point < 0x80 {
            output.push(char::from_u32(*point)?);
        }
    }
    let basic = output.len();
    let mut handled = basic;
    if basic > 0 {
        output.push('-');
    }
    let mut n = INITIAL_N;
    let mut delta: u32 = 0;
    let mut bias = INITIAL_BIAS;
    while handled < input.len() {
        let m = input.iter().copied().filter(|point| *point >= n).min()?;
        delta = delta.checked_add((m - n).checked_mul(handled as u32 + 1)?)?;
        n = m;
        for point in &input {
            if *point < n {
                delta = delta.checked_add(1)?;
            }
            if *point == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = if k <= bias {
                        TMIN
                    } else if k >= bias + TMAX {
                        TMAX
                    } else {
                        k - bias
                    };
                    if q < t {
                        break;
                    }
                    output.push(encode_digit(t + (q - t) % (BASE - t)));
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                output.push(encode_digit(q));
                bias = adapt(delta, handled as u32 + 1, handled == basic);
                delta = 0;
                handled += 1;
            }
        }
        delta = delta.checked_add(1)?;
        n += 1;
    }
    Some(output)
}

fn is_combining_mark_char(ch: char) -> bool {
    let point = ch as u32;
    (0x0300..=0x036F).contains(&point)
        || (0x1AB0..=0x1AFF).contains(&point)
        || (0x1DC0..=0x1DFF).contains(&point)
        || (0xFE20..=0xFE2F).contains(&point)
        || (0x0900..=0x0903).contains(&point)
        || (0x093A..=0x094F).contains(&point)
        || (0x20D0..=0x20F0).contains(&point)
}

fn is_letter(ch: char) -> bool {
    ch.is_alphabetic()
}

fn is_decimal_number(ch: char) -> bool {
    ch.is_numeric() && ch.is_ascii_digit()
}

fn is_greek_script(ch: char) -> bool {
    (0x0370..=0x03FF).contains(&(ch as u32)) || (0x1F00..=0x1FFF).contains(&(ch as u32))
}

fn is_hebrew_script(ch: char) -> bool {
    (0x0590..=0x05FF).contains(&(ch as u32)) || (0xFB1D..=0xFB4F).contains(&(ch as u32))
}

fn is_japanese_script(ch: char) -> bool {
    let point = ch as u32;
    (0x3040..=0x309F).contains(&point)   // Hiragana
        || (0x30A0..=0x30FF).contains(&point) // Katakana
        || (0x4E00..=0x9FFF).contains(&point) // CJK Unified Ideographs
        || (0x3400..=0x4DBF).contains(&point)
        || (0xF900..=0xFAFF).contains(&point)
}

fn is_arabic_letter(ch: char) -> bool {
    let point = ch as u32;
    (0x0600..=0x06FF).contains(&point) || (0x0700..=0x074F).contains(&point)
}

fn is_rfc5892_disallowed(ch: char) -> bool {
    matches!(
        ch,
        '\u{0640}'
            | '\u{07fa}'
            | '\u{302e}'
            | '\u{302f}'
            | '\u{3031}'
            | '\u{3032}'
            | '\u{3033}'
            | '\u{3034}'
            | '\u{3035}'
            | '\u{303b}'
    )
}

fn is_virama(ch: char) -> bool {
    matches!(
        ch,
        '\u{094d}'
            | '\u{09cd}'
            | '\u{0a4d}'
            | '\u{0acd}'
            | '\u{0b4d}'
            | '\u{0bcd}'
            | '\u{0c4d}'
            | '\u{0ccd}'
            | '\u{0d3b}'
            | '\u{0d3c}'
            | '\u{0d4d}'
            | '\u{0dca}'
            | '\u{1b44}'
            | '\u{1baa}'
            | '\u{1bab}'
            | '\u{a9c0}'
            | '\u{11046}'
            | '\u{1107f}'
            | '\u{110b9}'
            | '\u{11133}'
            | '\u{11134}'
            | '\u{111c0}'
            | '\u{11235}'
            | '\u{1134d}'
            | '\u{11442}'
            | '\u{114c2}'
            | '\u{115bf}'
            | '\u{1163f}'
            | '\u{116b6}'
            | '\u{11c3f}'
            | '\u{11d44}'
            | '\u{11d45}'
    )
}

/// `RE_PERMITTED_CATEGORY`：字母 / 欧洲分隔符 / 通用分隔符 / 十进制数字 / 组合记号 /
/// CONTEXTO 例外 / PVALID 例外。
fn is_permitted_category(ch: char) -> bool {
    is_letter(ch)
        || matches!(ch, '-' | '+')
        || matches!(ch, '.' | ',' | ':' | '/')
        || is_decimal_number(ch)
        || is_combining_mark_char(ch)
        || matches!(
            ch,
            '\u{00b7}'
                | '\u{0375}'
                | '\u{05f3}'
                | '\u{05f4}'
                | '\u{200c}'
                | '\u{200d}'
                | '\u{30fb}'
        )
        || matches!(
            ch,
            '\u{00df}' | '\u{03c2}' | '\u{06fd}' | '\u{06fe}' | '\u{0f0b}' | '\u{3007}'
        )
}

/// RFC 5893 的 Bidi 类（只实现 typebox 用到的子集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BidiClass {
    L,
    R,
    Al,
    An,
    En,
    Nsm,
    On,
}

fn bidi_class(ch: char) -> BidiClass {
    if ch.is_ascii_digit() || (0x06F0..=0x06F9).contains(&(ch as u32)) {
        BidiClass::En
    } else if (0x0660..=0x0669).contains(&(ch as u32)) {
        BidiClass::An
    } else if is_combining_mark_char(ch) {
        BidiClass::Nsm
    } else if is_hebrew_script(ch) {
        BidiClass::R
    } else if is_arabic_letter(ch) {
        BidiClass::Al
    } else if is_letter(ch) {
        BidiClass::L
    } else {
        BidiClass::On
    }
}

/// `HasRightToLeftCharacters`。
fn has_right_to_left(value: &str) -> bool {
    value
        .chars()
        .any(|ch| matches!(bidi_class(ch), BidiClass::R | BidiClass::Al | BidiClass::An))
}

/// `SatisfiesBidiRule`。
fn satisfies_bidi_rule(value: &str) -> bool {
    let mut is_rtl = false;
    let mut is_first = true;
    let mut saw_en = false;
    let mut saw_an = false;
    for ch in value.chars() {
        let class = bidi_class(ch);
        if is_first {
            // 首字符必须是 L、R 或 AL。
            if !matches!(class, BidiClass::L | BidiClass::R | BidiClass::Al) {
                return false;
            }
            is_rtl = matches!(class, BidiClass::R | BidiClass::Al);
            is_first = false;
        }
        // 只允许该方向允许的类。
        let allowed = if is_rtl {
            matches!(
                class,
                BidiClass::R
                    | BidiClass::Al
                    | BidiClass::An
                    | BidiClass::En
                    | BidiClass::Nsm
                    | BidiClass::On
            )
        } else {
            matches!(
                class,
                BidiClass::L | BidiClass::En | BidiClass::Nsm | BidiClass::On
            )
        };
        if !allowed {
            return false;
        }
        match class {
            BidiClass::En => saw_en = true,
            BidiClass::An => saw_an = true,
            _ => {}
        }
    }
    !(is_rtl && saw_en && saw_an)
}

/// `IsIPv4`。
fn is_ipv4_format(text: &str) -> bool {
    static IPV4: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    IPV4.get_or_init(|| {
        regex::Regex::new(
            r"^(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)$",
        )
        .expect("ipv4")
    })
    .is_match(text)
}

/// `IsIPv6`。
fn is_ipv6_format(text: &str) -> bool {
    static IPV6: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    IPV6.get_or_init(|| {
        regex::Regex::new(
            r"(?i)^(?:(?:(?:[0-9a-f]{1,4}:){6}|::(?:[0-9a-f]{1,4}:){5}|(?:[0-9a-f]{1,4})?::(?:[0-9a-f]{1,4}:){4}|(?:(?:[0-9a-f]{1,4}:)?[0-9a-f]{1,4})?::(?:[0-9a-f]{1,4}:){3}|(?:(?:[0-9a-f]{1,4}:){0,2}[0-9a-f]{1,4})?::(?:[0-9a-f]{1,4}:){2}|(?:(?:[0-9a-f]{1,4}:){0,3}[0-9a-f]{1,4})?::[0-9a-f]{1,4}:|(?:(?:[0-9a-f]{1,4}:){0,4}[0-9a-f]{1,4})?::)(?:[0-9a-f]{1,4}:[0-9a-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[0-9a-f]{1,4}:){0,5}[0-9a-f]{1,4})?::[0-9a-f]{1,4}|(?:(?:[0-9a-f]{1,4}:){0,6}[0-9a-f]{1,4})?::)$",
        )
        .expect("ipv6")
    })
    .is_match(text)
}

/// `IsEmail`。
fn is_email_format(text: &str) -> bool {
    static EMAIL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    EMAIL
        .get_or_init(|| {
            regex::Regex::new(
                r#"(?i)^(?:[a-z0-9!#$%&'*+/=?^_`{|}~-]+(?:\.[a-z0-9!#$%&'*+/=?^_`{|}~-]+)*|"(?:[^"\\]|\\[\x20-\x7e])*")@(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)*|\[(?:IPv6:[a-f0-9:]+|(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])(?:\.(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])){3})\])$"#,
            )
            .expect("email")
        })
        .is_match(text)
}

/// `IsUuid`。
fn is_uuid_format(text: &str) -> bool {
    static UUID: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    UUID.get_or_init(|| {
        regex::Regex::new(r"(?i)^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$").expect("uuid")
    })
    .is_match(text)
}

/// `IsDuration`。
fn is_duration_format(text: &str) -> bool {
    static DURATION: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    DURATION
        .get_or_init(|| {
            regex::Regex::new(
                r"^P((\d+Y(\d+M(\d+D)?)?|\d+M(\d+D)?|\d+D)(T(\d+H(\d+M(\d+S)?)?|\d+M(\d+S)?|\d+S))?|T(\d+H(\d+M(\d+S)?)?|\d+M(\d+S)?|\d+S)|\d+W)$",
            )
            .expect("duration")
        })
        .is_match(text)
}

/// `IsJsonPointer`。
fn is_json_pointer_format(text: &str) -> bool {
    static POINTER: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    POINTER
        .get_or_init(|| regex::Regex::new(r"^(?:/(?:[^~/]|~0|~1)*)*$").expect("pointer"))
        .is_match(text)
}

/// `IsRelativeJsonPointer`。
fn is_relative_json_pointer_format(text: &str) -> bool {
    static POINTER: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    POINTER
        .get_or_init(|| {
            regex::Regex::new(r"^(?:0|[1-9][0-9]*)(?:#|(?:/(?:[^~/]|~0|~1)*)*)$").expect("rel")
        })
        .is_match(text)
}

/// `IsJsonPointerUriFragment`。
fn is_json_pointer_uri_fragment_format(text: &str) -> bool {
    static POINTER: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    POINTER
        .get_or_init(|| {
            regex::Regex::new(r"(?i)^#(?:/(?:[a-z0-9_\-.!$&'()*+,;:=@]|%[0-9a-f]{2}|~0|~1)*)*$")
                .expect("frag")
        })
        .is_match(text)
}

/// `IsUri`（TypeBox `format/uri.mjs` 的 RFC 3986 正则）。
fn is_uri_format(text: &str) -> bool {
    static URI: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    URI.get_or_init(|| compile_js_ascii_regex(URI_PATTERN))
        .is_match(text)
}

/// 编译 TypeBox 的正则字面量（JS 无 `u` 标志：大小写折叠与 `\d` 都只认 ASCII，
/// Rust 侧用 `(?i-u)` 复刻）。
fn compile_js_ascii_regex(source: &str) -> regex::Regex {
    regex::Regex::new(&format!("(?i-u){source}")).expect("内置正则必须可编译")
}

/// `uri` 的正文（TypeBox 1.3.27 `format/uri.mjs` 的 RFC 3986 正则，逐字）。
const URI_PATTERN: &str = r"^[a-z][a-z0-9+\-.]*:(?:\/\/(?:(?:[-a-z0-9._~!$&'()*+,;=:]|%[0-9a-f]{2})*@)?(?:\[(?:(?:(?:[\da-f]{1,4}:){6}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|::(?:[\da-f]{1,4}:){5}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:[\da-f]{1,4})?::(?:[\da-f]{1,4}:){4}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,1}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){3}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,2}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){2}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,3}[\da-f]{1,4})?::[\da-f]{1,4}:(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,4}[\da-f]{1,4})?::(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,5}[\da-f]{1,4})?::[\da-f]{1,4}|(?:(?:[\da-f]{1,4}:){0,6}[\da-f]{1,4})?::)|v[0-9a-f]+\.[-a-z0-9._~!$&'()*+,;=:]+)\]|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)|(?:[-a-z0-9._~!$&'()*+,;=]|%[0-9a-f]{2})*)(?::\d*)?(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*|\/(?:(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?|(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?(?:\?(?:[-a-z0-9._~!$&'()*+,;=:@/?]|%[0-9a-f]{2})*)?(?:#(?:[-a-z0-9._~!$&'()*+,;=:@/?]|%[0-9a-f]{2})*)?$";

/// `IsUriReference`：与 `uri` 的区别是 scheme 段整体可选。
fn is_uri_reference_format(text: &str) -> bool {
    static URI_REFERENCE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    URI_REFERENCE
        .get_or_init(|| compile_js_ascii_regex(URI_REFERENCE_PATTERN))
        .is_match(text)
}

/// `uri-reference` 的正文（scheme 可选）。
const URI_REFERENCE_PATTERN: &str = r"^(?:[a-z][a-z0-9+\-.]*:(?:\/\/(?:(?:[-a-z0-9._~!$&'()*+,;=:]|%[0-9a-f]{2})*@)?(?:\[(?:(?:(?:[\da-f]{1,4}:){6}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|::(?:[\da-f]{1,4}:){5}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:[\da-f]{1,4})?::(?:[\da-f]{1,4}:){4}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,1}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){3}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,2}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){2}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,3}[\da-f]{1,4})?::[\da-f]{1,4}:(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,4}[\da-f]{1,4})?::(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,5}[\da-f]{1,4})?::[\da-f]{1,4}|(?:(?:[\da-f]{1,4}:){0,6}[\da-f]{1,4})?::)|v[0-9a-f]+\.[-a-z0-9._~!$&'()*+,;=:]+)\]|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)|(?:[-a-z0-9._~!$&'()*+,;=]|%[0-9a-f]{2})*)(?::\d*)?(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*|\/(?:(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?|(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?|(?:\/\/(?:(?:[-a-z0-9._~!$&'()*+,;=:]|%[0-9a-f]{2})*@)?(?:\[(?:(?:(?:[\da-f]{1,4}:){6}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|::(?:[\da-f]{1,4}:){5}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:[\da-f]{1,4})?::(?:[\da-f]{1,4}:){4}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,1}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){3}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,2}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){2}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,3}[\da-f]{1,4})?::[\da-f]{1,4}:(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,4}[\da-f]{1,4})?::(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,5}[\da-f]{1,4})?::[\da-f]{1,4}|(?:(?:[\da-f]{1,4}:){0,6}[\da-f]{1,4})?::)|v[0-9a-f]+\.[-a-z0-9._~!$&'()*+,;=:]+)\]|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)|(?:[-a-z0-9._~!$&'()*+,;=]|%[0-9a-f]{2})*)(?::\d*)?(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*|\/(?:(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?|(?:[-a-z0-9._~!$&'()*+,;=@]|%[0-9a-f]{2})+(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?)(?:\?(?:[-a-z0-9._~!$&'()*+,;=:@/?]|%[0-9a-f]{2})*)?(?:#(?:[-a-z0-9._~!$&'()*+,;=:@/?]|%[0-9a-f]{2})*)?$";

type OnceLockRegex = std::sync::OnceLock<fancy_regex::Regex>;

/// `IsUriTemplate`。
fn is_uri_template_format(text: &str) -> bool {
    static TEMPLATE: OnceLockRegex = std::sync::OnceLock::new();
    TEMPLATE
        .get_or_init(|| regex_from(r#"(?i)^(?:(?:[^\x00-\x20"<>%\\^`{|}\x7f]|%[0-9a-f]{2})|\{[+#./;?&=,!@|]?(?:[a-z0-9_]|%[0-9a-f]{2})+(?:\.(?:[a-z0-9_]|%[0-9a-f]{2})+)*(?::[1-9]\d{0,3}|\*)?(?:,(?:[a-z0-9_]|%[0-9a-f]{2})+(?:\.(?:[a-z0-9_]|%[0-9a-f]{2})+)*(?::[1-9]\d{0,3}|\*)?)*\})*$"#))
        .is_match(text)
        .unwrap_or(false)
}

/// `IsRegex`：`new RegExp(value, 'u')` 不抛错（TypeBox `format/regex.mjs`）。
///
/// V8 的 `u` 模式语法与 Rust `regex` 语法不同（`{`/`}`/`]` 的孤立括号、受限的 identity
/// escape、字符类范围端点、`\k<name>`/数字反向引用等），因此这里用一个显式的
/// ECMAScript `Pattern[u]` 语法校验器，而不是借用 Rust 正则编译器。
fn is_regex_format(text: &str) -> bool {
    js_regex::is_valid_u_pattern(text)
}

/// ECMAScript `Pattern[u]` 语法校验（只判语法，不执行）。
///
/// 规则来源：ECMA-262 §22.2.1（`Pattern`、`ClassRanges`、`Quantifier`、
/// `RegExpIdentifierName`）与 V8 的 `u` 模式错误分类；对每个测试向量用 Node 的
/// `new RegExp(value, "u")` 生成黄金结果（`tests/fixtures/utils/js_regex.json`）。
mod js_regex {
    use std::collections::HashSet;

    pub(super) fn is_valid_u_pattern(pattern: &str) -> bool {
        let mut validator = Validator {
            chars: pattern.chars().collect(),
            pos: 0,
            group_count: 0,
            max_backref: 0,
            names: HashSet::new(),
            named_refs: Vec::new(),
        };
        validator.parse_disjunction(false)
            && validator.pos == validator.chars.len()
            && validator.max_backref <= validator.group_count
            && validator
                .named_refs
                .iter()
                .all(|name| validator.names.contains(name))
    }

    /// 一个字符类原子：普通码点，或 `\d`/`\p{..}` 这类集合（不能作范围端点）。
    ///
    /// 用码点（而非 [`char`]）表示，因为 u 模式允许 `\uD800` 这类代理项码点作类原子。
    enum ClassAtom {
        Char(u32),
        Set,
    }

    struct Validator {
        chars: Vec<char>,
        pos: usize,
        group_count: u32,
        max_backref: u32,
        names: HashSet<String>,
        named_refs: Vec<String>,
    }

    impl Validator {
        fn peek(&self) -> Option<char> {
            self.chars.get(self.pos).copied()
        }

        fn peek_at(&self, offset: usize) -> Option<char> {
            self.chars.get(self.pos + offset).copied()
        }

        fn eat(&mut self, expected: char) -> bool {
            if self.peek() == Some(expected) {
                self.pos += 1;
                true
            } else {
                false
            }
        }

        fn parse_disjunction(&mut self, in_group: bool) -> bool {
            loop {
                if !self.parse_alternative(in_group) {
                    return false;
                }
                if self.peek() == Some('|') {
                    self.pos += 1;
                    continue;
                }
                return true;
            }
        }

        fn parse_alternative(&mut self, in_group: bool) -> bool {
            loop {
                match self.peek() {
                    None => return true,
                    Some('|') => return true,
                    Some(')') => return in_group,
                    Some(_) => {
                        if !self.parse_term() {
                            return false;
                        }
                    }
                }
            }
        }

        fn parse_term(&mut self) -> bool {
            if self.parse_assertion() {
                return true;
            }
            if !self.parse_atom() {
                return false;
            }
            let Some(ch) = self.peek() else {
                return true;
            };
            if matches!(ch, '*' | '+' | '?') {
                self.pos += 1;
            } else if ch == '{' {
                if !self.parse_brace_quantifier() {
                    return false;
                }
            } else {
                return true;
            }
            // 非贪婪后缀。
            let _ = self.eat('?');
            // 量词之后不能再接量词（`a**`、`a{1}{2}`、`a??*`…）。
            !matches!(self.peek(), Some('*' | '+' | '?' | '{'))
        }

        fn parse_assertion(&mut self) -> bool {
            match self.peek() {
                Some('^' | '$') => {
                    self.pos += 1;
                    true
                }
                Some('\\') if matches!(self.peek_at(1), Some('b' | 'B')) => {
                    self.pos += 2;
                    true
                }
                Some('(') if self.peek_at(1) == Some('?') => match self.peek_at(2) {
                    Some('=' | '!') => {
                        self.pos += 3;
                        self.parse_disjunction(true) && self.eat(')')
                    }
                    Some('<') if matches!(self.peek_at(3), Some('=' | '!')) => {
                        self.pos += 4;
                        self.parse_disjunction(true) && self.eat(')')
                    }
                    _ => false,
                },
                _ => false,
            }
        }

        fn parse_atom(&mut self) -> bool {
            let Some(ch) = self.peek() else {
                return false;
            };
            match ch {
                '.' => {
                    self.pos += 1;
                    true
                }
                '(' => self.parse_group(),
                '[' => self.parse_class(),
                '\\' => self.parse_atom_escape(),
                // `* + ? { } ] )` 在原子位置都是语法错误（u 模式）。
                '*' | '+' | '?' | '{' | '}' | ']' | ')' => false,
                _ => {
                    self.pos += 1;
                    true
                }
            }
        }

        fn parse_group(&mut self) -> bool {
            self.pos += 1; // '('
            if self.peek() == Some('?') {
                self.pos += 1;
                match self.peek() {
                    Some(':') => self.pos += 1,
                    Some('<') => {
                        // 断言（`(?<=` / `(?<!`）已在 parse_assertion 处理。
                        if matches!(self.peek_at(1), Some('=' | '!')) {
                            return false;
                        }
                        self.pos += 1;
                        let Some(name) = self.parse_group_name() else {
                            return false;
                        };
                        if !self.names.insert(name) {
                            return false;
                        }
                        self.group_count += 1;
                        if !self.eat('>') {
                            return false;
                        }
                    }
                    _ => return false,
                }
            } else {
                self.group_count += 1;
            }
            if !self.parse_disjunction(true) || !self.eat(')') {
                return false;
            }
            true
        }

        /// `GroupName`：`(?<name>...)`，允许 `\uXXXX` / `\u{...}` 转义。
        fn parse_group_name(&mut self) -> Option<String> {
            let mut name = String::new();
            let mut first = true;
            loop {
                let ch = self.peek()?;
                if ch == '>' {
                    break;
                }
                let value = if ch == '\\' {
                    self.pos += 1;
                    if !self.eat('u') {
                        return None;
                    }
                    let codepoint = self.read_unicode_escape()?;
                    char::from_u32(codepoint)?
                } else {
                    self.pos += 1;
                    ch
                };
                let ok = if first {
                    value == '$' || value == '_' || value.is_alphabetic()
                } else {
                    value == '$'
                        || value == '_'
                        || value == '\u{200c}'
                        || value == '\u{200d}'
                        || value.is_alphanumeric()
                        || matches!(value, '\u{0300}'..='\u{036f}')
                };
                if !ok {
                    return None;
                }
                name.push(value);
                first = false;
            }
            if name.is_empty() { None } else { Some(name) }
        }

        fn parse_atom_escape(&mut self) -> bool {
            self.pos += 1; // '\\'
            let Some(ch) = self.peek() else {
                return false;
            };
            match ch {
                'd' | 'D' | 's' | 'S' | 'w' | 'W' => {
                    self.pos += 1;
                    true
                }
                'p' | 'P' => self.parse_property_escape(),
                '0' => {
                    self.pos += 1;
                    !self.peek().is_some_and(|next| next.is_ascii_digit())
                }
                '1'..='9' => {
                    let value = self.read_decimal();
                    if value == 0 {
                        return false;
                    }
                    self.max_backref = self.max_backref.max(value);
                    true
                }
                'k' => {
                    self.pos += 1;
                    if !self.eat('<') {
                        return false;
                    }
                    let Some(name) = self.parse_group_name() else {
                        return false;
                    };
                    if !self.eat('>') {
                        return false;
                    }
                    self.named_refs.push(name);
                    true
                }
                'b' | 'B' => false,
                'c' => {
                    self.pos += 1;
                    if self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                        self.pos += 1;
                        true
                    } else {
                        false
                    }
                }
                'x' => {
                    self.pos += 1;
                    self.read_hex(2)
                }
                'u' => {
                    self.pos += 1;
                    self.read_unicode_escape().is_some()
                }
                'f' | 'n' | 'r' | 't' | 'v' => {
                    self.pos += 1;
                    true
                }
                // `SyntaxCharacter` 与 `/`（u 模式允许的 identity escape）。
                '^' | '$' | '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}'
                | '|' | '/' => {
                    self.pos += 1;
                    true
                }
                _ => false,
            }
        }

        fn parse_class(&mut self) -> bool {
            self.pos += 1; // '['
            let _ = self.eat('^');
            loop {
                match self.peek() {
                    None => return false,
                    Some(']') => {
                        self.pos += 1;
                        return true;
                    }
                    Some(_) => {}
                }
                let Some(first) = self.parse_class_atom() else {
                    return false;
                };
                // 范围：`-` 后还有字符且不是结尾的 `]`。
                if self.peek() == Some('-')
                    && self.peek_at(1) != Some(']')
                    && self.peek_at(1).is_some()
                {
                    self.pos += 1;
                    let Some(second) = self.parse_class_atom() else {
                        return false;
                    };
                    match (first, second) {
                        (ClassAtom::Char(start), ClassAtom::Char(end)) => {
                            if start > end {
                                return false;
                            }
                        }
                        // `[\d-a]` / `[a-\d]`：类转义不能作范围端点。
                        _ => return false,
                    }
                }
            }
        }

        fn parse_class_atom(&mut self) -> Option<ClassAtom> {
            let ch = self.peek()?;
            if ch != '\\' {
                self.pos += 1;
                return Some(ClassAtom::Char(ch as u32));
            }
            self.pos += 1;
            let escape = self.peek()?;
            match escape {
                'b' => {
                    self.pos += 1;
                    Some(ClassAtom::Char(0x08))
                }
                'd' | 'D' | 's' | 'S' | 'w' | 'W' => {
                    self.pos += 1;
                    Some(ClassAtom::Set)
                }
                'p' | 'P' => self.parse_property_escape().then_some(ClassAtom::Set),
                '0' => {
                    self.pos += 1;
                    if self.peek().is_some_and(|next| next.is_ascii_digit()) {
                        None
                    } else {
                        Some(ClassAtom::Char(0))
                    }
                }
                '1'..='9' => None,
                'c' => {
                    self.pos += 1;
                    let letter = self.peek()?;
                    if !letter.is_ascii_alphabetic() {
                        return None;
                    }
                    self.pos += 1;
                    Some(ClassAtom::Char((letter as u32) % 32))
                }
                'x' => {
                    self.pos += 1;
                    Some(ClassAtom::Char(self.read_hex_value(2)?))
                }
                'u' => {
                    self.pos += 1;
                    Some(ClassAtom::Char(self.read_unicode_escape()?))
                }
                'f' => {
                    self.pos += 1;
                    Some(ClassAtom::Char(0x0c))
                }
                'n' => {
                    self.pos += 1;
                    Some(ClassAtom::Char(0x0a))
                }
                'r' => {
                    self.pos += 1;
                    Some(ClassAtom::Char(0x0d))
                }
                't' => {
                    self.pos += 1;
                    Some(ClassAtom::Char(0x09))
                }
                'v' => {
                    self.pos += 1;
                    Some(ClassAtom::Char(0x0b))
                }
                // 类内的 identity escape：SyntaxCharacter、`/`、`-`。
                '^' | '$' | '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}'
                | '|' | '/' | '-' => {
                    self.pos += 1;
                    Some(ClassAtom::Char(escape as u32))
                }
                _ => None,
            }
        }

        /// `\p{...}` / `\P{...}`；属性的真实性交给 `regex` crate 的 Unicode 表
        /// （另有 JS 接受但 Rust 表未收录的少数属性名单）。
        fn parse_property_escape(&mut self) -> bool {
            self.pos += 1; // 'p' | 'P'
            if !self.eat('{') {
                return false;
            }
            let start = self.pos;
            while let Some(ch) = self.peek() {
                if ch == '}' {
                    break;
                }
                if !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '=') {
                    return false;
                }
                self.pos += 1;
            }
            if self.peek() != Some('}') {
                return false;
            }
            let content: String = self.chars[start..self.pos].iter().collect();
            self.pos += 1;
            is_valid_unicode_property(&content)
        }

        /// `{n}` / `{n,}` / `{n,m}`（只允许出现在量词位置，否则是语法错误）。
        fn parse_brace_quantifier(&mut self) -> bool {
            self.pos += 1; // '{'
            let Some(min) = self.read_decimal_opt() else {
                return false;
            };
            let mut max = Some(min);
            if self.eat(',') {
                max = self.read_decimal_opt();
            }
            if !self.eat('}') {
                return false;
            }
            if let Some(max) = max
                && min > max
            {
                return false;
            }
            true
        }

        fn read_decimal_opt(&mut self) -> Option<u128> {
            let start = self.pos;
            while self.peek().is_some_and(|ch| ch.is_ascii_digit()) {
                self.pos += 1;
            }
            if self.pos == start {
                return None;
            }
            let text: String = self.chars[start..self.pos].iter().collect();
            // V8 不像 `u32` 那样限制量词数字（`a{999999999999999999999}` 合法）；
            // 比较时饱和即可。
            Some(text.parse::<u128>().unwrap_or(u128::MAX))
        }

        /// 十进制数字串（反向引用）；溢出 `u32` 时饱和，之后必然大于组数而失败。
        fn read_decimal(&mut self) -> u32 {
            let start = self.pos;
            while self.peek().is_some_and(|ch| ch.is_ascii_digit()) {
                self.pos += 1;
            }
            let text: String = self.chars[start..self.pos].iter().collect();
            text.parse::<u32>().unwrap_or(u32::MAX)
        }

        fn read_hex(&mut self, digits: usize) -> bool {
            self.read_hex_value(digits).is_some()
        }

        fn read_hex_value(&mut self, digits: usize) -> Option<u32> {
            let start = self.pos;
            for _ in 0..digits {
                if !self.peek().is_some_and(|ch| ch.is_ascii_hexdigit()) {
                    return None;
                }
                self.pos += 1;
            }
            let text: String = self.chars[start..self.pos].iter().collect();
            u32::from_str_radix(&text, 16).ok()
        }

        /// `\uHHHH` 或 `\u{H...}`（调用时已吃掉 `u`）。
        fn read_unicode_escape(&mut self) -> Option<u32> {
            if self.eat('{') {
                let start = self.pos;
                while self.peek().is_some_and(|ch| ch.is_ascii_hexdigit()) {
                    self.pos += 1;
                }
                let digits = self.pos - start;
                if digits == 0 || digits > 6 || !self.eat('}') {
                    return None;
                }
                let text: String = self.chars[start..start + digits].iter().collect();
                let value = u32::from_str_radix(&text, 16).ok()?;
                (value <= 0x10FFFF).then_some(value)
            } else {
                self.read_hex_value(4)
            }
        }
    }

    /// Unicode Script 属性值别名（long 名与 ISO 15924 短码，**大小写敏感**）。
    ///
    /// 由 `regex-syntax 0.8.11` 的 `unicode_tables/property_values.rs`（Unicode 16.0.0）
    /// 的 Script 段生成；JS 的 `\p{Script=latin}` 是 SyntaxError，而 Rust `regex` 接受
    /// 小写，所以必须显式比对大小写。
    const SCRIPT_ALIASES: &[&str] = &[
        "Adlam",
        "Adlm",
        "Aghb",
        "Ahom",
        "Anatolian_Hieroglyphs",
        "Arab",
        "Arabic",
        "Armenian",
        "Armi",
        "Armn",
        "Avestan",
        "Avst",
        "Bali",
        "Balinese",
        "Bamu",
        "Bamum",
        "Bass",
        "Bassa_Vah",
        "Batak",
        "Batk",
        "Beng",
        "Bengali",
        "Bhaiksuki",
        "Bhks",
        "Bopo",
        "Bopomofo",
        "Brah",
        "Brahmi",
        "Brai",
        "Braille",
        "Bugi",
        "Buginese",
        "Buhd",
        "Buhid",
        "Cakm",
        "Canadian_Aboriginal",
        "Cans",
        "Cari",
        "Carian",
        "Caucasian_Albanian",
        "Chakma",
        "Cham",
        "Cher",
        "Cherokee",
        "Chorasmian",
        "Chrs",
        "Common",
        "Copt",
        "Coptic",
        "Cpmn",
        "Cprt",
        "Cuneiform",
        "Cypriot",
        "Cypro_Minoan",
        "Cyrillic",
        "Cyrl",
        "Deseret",
        "Deva",
        "Devanagari",
        "Diak",
        "Dives_Akuru",
        "Dogr",
        "Dogra",
        "Dsrt",
        "Dupl",
        "Duployan",
        "Egyp",
        "Egyptian_Hieroglyphs",
        "Elba",
        "Elbasan",
        "Elym",
        "Elymaic",
        "Ethi",
        "Ethiopic",
        "Gara",
        "Garay",
        "Geor",
        "Georgian",
        "Glag",
        "Glagolitic",
        "Gong",
        "Gonm",
        "Goth",
        "Gothic",
        "Gran",
        "Grantha",
        "Greek",
        "Grek",
        "Gujarati",
        "Gujr",
        "Gukh",
        "Gunjala_Gondi",
        "Gurmukhi",
        "Guru",
        "Gurung_Khema",
        "Han",
        "Hang",
        "Hangul",
        "Hani",
        "Hanifi_Rohingya",
        "Hano",
        "Hanunoo",
        "Hatr",
        "Hatran",
        "Hebr",
        "Hebrew",
        "Hira",
        "Hiragana",
        "Hluw",
        "Hmng",
        "Hmnp",
        "Hrkt",
        "Hung",
        "Imperial_Aramaic",
        "Inherited",
        "Inscriptional_Pahlavi",
        "Inscriptional_Parthian",
        "Ital",
        "Java",
        "Javanese",
        "Kaithi",
        "Kali",
        "Kana",
        "Kannada",
        "Katakana",
        "Katakana_Or_Hiragana",
        "Kawi",
        "Kayah_Li",
        "Khar",
        "Kharoshthi",
        "Khitan_Small_Script",
        "Khmer",
        "Khmr",
        "Khoj",
        "Khojki",
        "Khudawadi",
        "Kirat_Rai",
        "Kits",
        "Knda",
        "Krai",
        "Kthi",
        "Lana",
        "Lao",
        "Laoo",
        "Latin",
        "Latn",
        "Lepc",
        "Lepcha",
        "Limb",
        "Limbu",
        "Lina",
        "Linb",
        "Linear_A",
        "Linear_B",
        "Lisu",
        "Lyci",
        "Lycian",
        "Lydi",
        "Lydian",
        "Mahajani",
        "Mahj",
        "Maka",
        "Makasar",
        "Malayalam",
        "Mand",
        "Mandaic",
        "Mani",
        "Manichaean",
        "Marc",
        "Marchen",
        "Masaram_Gondi",
        "Medefaidrin",
        "Medf",
        "Meetei_Mayek",
        "Mend",
        "Mende_Kikakui",
        "Merc",
        "Mero",
        "Meroitic_Cursive",
        "Meroitic_Hieroglyphs",
        "Miao",
        "Mlym",
        "Modi",
        "Mong",
        "Mongolian",
        "Mro",
        "Mroo",
        "Mtei",
        "Mult",
        "Multani",
        "Myanmar",
        "Mymr",
        "Nabataean",
        "Nag_Mundari",
        "Nagm",
        "Nand",
        "Nandinagari",
        "Narb",
        "Nbat",
        "New_Tai_Lue",
        "Newa",
        "Nko",
        "Nkoo",
        "Nshu",
        "Nushu",
        "Nyiakeng_Puachue_Hmong",
        "Ogam",
        "Ogham",
        "Ol_Chiki",
        "Ol_Onal",
        "Olck",
        "Old_Hungarian",
        "Old_Italic",
        "Old_North_Arabian",
        "Old_Permic",
        "Old_Persian",
        "Old_Sogdian",
        "Old_South_Arabian",
        "Old_Turkic",
        "Old_Uyghur",
        "Onao",
        "Oriya",
        "Orkh",
        "Orya",
        "Osage",
        "Osge",
        "Osma",
        "Osmanya",
        "Ougr",
        "Pahawh_Hmong",
        "Palm",
        "Palmyrene",
        "Pau_Cin_Hau",
        "Pauc",
        "Perm",
        "Phag",
        "Phags_Pa",
        "Phli",
        "Phlp",
        "Phnx",
        "Phoenician",
        "Plrd",
        "Prti",
        "Psalter_Pahlavi",
        "Qaac",
        "Qaai",
        "Rejang",
        "Rjng",
        "Rohg",
        "Runic",
        "Runr",
        "Samaritan",
        "Samr",
        "Sarb",
        "Saur",
        "Saurashtra",
        "Sgnw",
        "Sharada",
        "Shavian",
        "Shaw",
        "Shrd",
        "Sidd",
        "Siddham",
        "SignWriting",
        "Sind",
        "Sinh",
        "Sinhala",
        "Sogd",
        "Sogdian",
        "Sogo",
        "Sora",
        "Sora_Sompeng",
        "Soyo",
        "Soyombo",
        "Sund",
        "Sundanese",
        "Sunu",
        "Sunuwar",
        "Sylo",
        "Syloti_Nagri",
        "Syrc",
        "Syriac",
        "Tagalog",
        "Tagb",
        "Tagbanwa",
        "Tai_Le",
        "Tai_Tham",
        "Tai_Viet",
        "Takr",
        "Takri",
        "Tale",
        "Talu",
        "Tamil",
        "Taml",
        "Tang",
        "Tangsa",
        "Tangut",
        "Tavt",
        "Telu",
        "Telugu",
        "Tfng",
        "Tglg",
        "Thaa",
        "Thaana",
        "Thai",
        "Tibetan",
        "Tibt",
        "Tifinagh",
        "Tirh",
        "Tirhuta",
        "Tnsa",
        "Todhri",
        "Todr",
        "Toto",
        "Tulu_Tigalari",
        "Tutg",
        "Ugar",
        "Ugaritic",
        "Unknown",
        "Vai",
        "Vaii",
        "Vith",
        "Vithkuqi",
        "Wancho",
        "Wara",
        "Warang_Citi",
        "Wcho",
        "Xpeo",
        "Xsux",
        "Yezi",
        "Yezidi",
        "Yi",
        "Yiii",
        "Zanabazar_Square",
        "Zanb",
        "Zinh",
        "Zyyy",
        "Zzzz",
    ];

    /// ECMAScript 二值属性（Unicode `PropertyAliases.txt` 的 long 与 short 别名）。
    ///
    /// 裸属性名只能是二值属性或 General_Category；脚本名必须写成 `Script=...`（JS 不接受
    /// `\p{Latin}`），所以不能直接用 Rust `regex` 的 `\p{...}` 真值表。
    const BINARY_PROPERTIES: &[&str] = &[
        "ASCII",
        "Any",
        "Assigned",
        "Alphabetic",
        "Alpha",
        "ASCII_Hex_Digit",
        "AHex",
        "Bidi_Control",
        "Bidi_C",
        "Bidi_Mirrored",
        "Bidi_M",
        "Case_Ignorable",
        "CI",
        "Cased",
        "Changes_When_Casefolded",
        "CWCF",
        "Changes_When_Casemapped",
        "CWCM",
        "Changes_When_Lowercased",
        "CWL",
        "Changes_When_NFKC_Casefolded",
        "CWKCF",
        "Changes_When_Titlecased",
        "CWT",
        "Changes_When_Uppercased",
        "CWU",
        "Dash",
        "Default_Ignorable_Code_Point",
        "DI",
        "Deprecated",
        "Dep",
        "Diacritic",
        "Dia",
        "Emoji",
        "Emoji_Component",
        "EComp",
        "Emoji_Modifier",
        "EMod",
        "Emoji_Modifier_Base",
        "EBase",
        "Emoji_Presentation",
        "EPres",
        "Extended_Pictographic",
        "ExtPict",
        "Extender",
        "Ext",
        "Full_Composition_Exclusion",
        "Comp_Ex",
        "Grapheme_Base",
        "Gr_Base",
        "Grapheme_Extend",
        "Gr_Ext",
        "Hex_Digit",
        "Hex",
        "ID_Continue",
        "IDC",
        "ID_Start",
        "IDS",
        "Ideographic",
        "Ideo",
        "IDS_Binary_Operator",
        "IDSB",
        "IDS_Trinary_Operator",
        "IDST",
        "Join_Control",
        "Join_C",
        "Logical_Order_Exception",
        "LOE",
        "Lowercase",
        "Lower",
        "Math",
        "Noncharacter_Code_Point",
        "NChar",
        "Pattern_Syntax",
        "Pat_Syn",
        "Pattern_White_Space",
        "Pat_WS",
        "Prepended_Concatenation_Mark",
        "PCM",
        "Quotation_Mark",
        "QMark",
        "Radical",
        "Regional_Indicator",
        "RI",
        "Sentence_Terminal",
        "STerm",
        "Soft_Dotted",
        "SD",
        "Terminal_Punctuation",
        "Term",
        "Unified_Ideograph",
        "UIdeo",
        "Uppercase",
        "Upper",
        "Variation_Selector",
        "VS",
        "White_Space",
        "WSpace",
        "space",
        "XID_Continue",
        "XIDC",
        "XID_Start",
        "XIDS",
    ];

    /// General_Category 的 long/short 别名（`\p{L}`、`\p{Letter}`、`\p{gc=Lu}`…）。
    const GENERAL_CATEGORIES: &[&str] = &[
        "C",
        "Other",
        "Cc",
        "Control",
        "Cf",
        "Format",
        "Cn",
        "Unassigned",
        "Co",
        "Private_Use",
        "Cs",
        "Surrogate",
        "L",
        "Letter",
        "LC",
        "Cased_Letter",
        "Ll",
        "Lowercase_Letter",
        "Lm",
        "Modifier_Letter",
        "Lo",
        "Other_Letter",
        "Lt",
        "Titlecase_Letter",
        "Lu",
        "Uppercase_Letter",
        "M",
        "Mark",
        "Mc",
        "Spacing_Mark",
        "Me",
        "Enclosing_Mark",
        "Mn",
        "Nonspacing_Mark",
        "N",
        "Number",
        "Nd",
        "Decimal_Number",
        "digit",
        "Nl",
        "Letter_Number",
        "No",
        "Other_Number",
        "P",
        "Punctuation",
        "punct",
        "Pc",
        "Connector_Punctuation",
        "Pd",
        "Dash_Punctuation",
        "Pe",
        "Close_Punctuation",
        "Pf",
        "Final_Punctuation",
        "Kiana",
        "Initial_Punctuation",
        "Po",
        "Other_Punctuation",
        "Ps",
        "Open_Punctuation",
        "S",
        "Symbol",
        "Sc",
        "Currency_Symbol",
        "Sk",
        "Modifier_Symbol",
        "Sm",
        "Math_Symbol",
        "So",
        "Other_Symbol",
        "Z",
        "Separator",
        "Zl",
        "Line_Separator",
        "Zp",
        "Paragraph_Separator",
        "Zs",
        "Space_Separator",
    ];

    /// `UnicodePropertyValueExpression`：`Name` 或 `Name=Value`。
    fn is_valid_unicode_property(content: &str) -> bool {
        if let Some((name, value)) = content.split_once('=') {
            return match name {
                "General_Category" | "gc" => GENERAL_CATEGORIES.contains(&value),
                "Script" | "sc" | "Script_Extensions" | "scx" => SCRIPT_ALIASES.contains(&value),
                _ => false,
            };
        }
        BINARY_PROPERTIES.contains(&content) || GENERAL_CATEGORIES.contains(&content)
    }
}

/// `IsIri`：拒绝非法字符与畸形百分号编码，其余交给 URL 解析器（Rust 侧用 `url` 近似）。
fn is_iri_format(text: &str) -> bool {
    // 前瞻 `(?![0-9a-fA-F]{2})` 是 TS 原型的写法；`regex` crate 不支持前瞻，因此用
    // `fancy_regex`（本 crate 已依赖，见 `utils/overflow.rs`）。
    static INVALID: std::sync::OnceLock<fancy_regex::Regex> = std::sync::OnceLock::new();
    static IPV_FUTURE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let invalid = INVALID.get_or_init(|| {
        fancy_regex::Regex::new(r"[\x00-\x20<>\^`{|}\\]|%(?![0-9a-fA-F]{2})").expect("iri")
    });
    !regex_is_match(invalid, text) && url_can_parse(&narrow_ipv_future(text, &IPV_FUTURE))
}

/// TypeBox `format/iri.mjs` 的 `NarrowIpvFuture`：WHATWG URL 解析器拒绝 RFC 3987 允许的
/// IPvFuture 字面量（如 `http://[v1.fe]/`），TypeBox 在解析前把它替换成 `[::1]`（仅当输入
/// 短于 2048 个 UTF-16 码元，避免在大字符串上做替换）。
fn narrow_ipv_future(text: &str, pattern: &std::sync::OnceLock<regex::Regex>) -> String {
    if text.encode_utf16().count() >= 2048 {
        return text.to_string();
    }
    let pattern = pattern
        .get_or_init(|| regex::Regex::new(r"\[[vV][0-9a-fA-F]+\.[^\]]+\]").expect("ipvfuture"));
    match pattern.find(text) {
        Some(found) => {
            let mut narrowed = String::with_capacity(text.len() - found.as_str().len() + 5);
            narrowed.push_str(&text[..found.start()]);
            narrowed.push_str("[::1]");
            narrowed.push_str(&text[found.end()..]);
            narrowed
        }
        None => text.to_string(),
    }
}

/// `IsUrl`（`URL.canParse(value)`）。
fn is_url_format(text: &str) -> bool {
    url_can_parse(text)
}

/// `IsIriReference`：非法字符 + 畸形 scheme + 相对解析。
fn is_iri_reference_format(text: &str) -> bool {
    static INVALID: std::sync::OnceLock<fancy_regex::Regex> = std::sync::OnceLock::new();
    static MALFORMED: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let invalid = INVALID.get_or_init(|| {
        fancy_regex::Regex::new(r"[\x00-\x20\x7F\\]|%(?![0-9a-fA-F]{2})").expect("iri-ref")
    });
    let malformed = MALFORMED
        .get_or_init(|| regex::Regex::new(r"^[a-zA-Z][a-zA-Z0-9+\-.]*//").expect("scheme"));
    !regex_is_match(invalid, text) && !malformed.is_match(text) && url_can_parse_relative(text)
}

/// `URL.canParse(value)`。
fn url_can_parse(text: &str) -> bool {
    url::Url::parse(text).is_ok()
}

/// `URL.canParse(value, 'http://example.com')`：相对解析。
fn url_can_parse_relative(text: &str) -> bool {
    url::Url::options()
        .base_url(Some(&url::Url::parse("http://example.com").expect("base")))
        .parse(text)
        .is_ok()
}

/// `IsIdnEmail`（先 `normalize("NFC")`，再用 TypeBox 的 `u` 模式正则）。
fn is_idn_email_format(text: &str) -> bool {
    static IDN_EMAIL: std::sync::OnceLock<fancy_regex::Regex> = std::sync::OnceLock::new();
    let pattern = IDN_EMAIL.get_or_init(|| {
        fancy_regex::Regex::new(
            r#"(?iu)^(?:[A-Za-z0-9!#$%&'*+/=?^_`{|}~\u{0080}-\u{10FFFF}-]+(?:\.[A-Za-z0-9!#$%&'*+/=?^_`{|}~\u{0080}-\u{10FFFF}-]+)*|"(?:[^"\\]|\\.)*")@[\p{L}\p{N}](?:[\p{L}\p{N}-]{0,62})(?<!-)(?:\.[\p{L}\p{N}](?:[\p{L}\p{N}-]{0,62})(?<!-))*$"#,
        )
        .expect("idn-email")
    });
    // TS: `value.normalize("NFC")`。JS 的 `u` 正则量词按码点计数，Rust 同样按字符。
    let normalized: String = text.nfc().collect();
    regex_is_match(pattern, &normalized)
}

/// `IsIdnHostname`（`NormalizeHostname` + 标签校验 + Bidi 规则）。
fn is_idn_hostname_format(text: &str) -> bool {
    if text.is_empty() || text.contains(' ') {
        return false;
    }
    let normalized = normalize_idn_hostname(text);
    // TS 的 `.length` 是 UTF-16 码元数。
    if normalized.encode_utf16().count() > 253 {
        return false;
    }
    let labels: Vec<&str> = normalized.split('.').collect();
    let has_bidi = labels.iter().any(|label| has_right_to_left(label));
    labels
        .iter()
        .all(|label| is_idn_label(label) && (!has_bidi || satisfies_bidi_rule(label)))
}

/// TS `NormalizeHostname`：全角映射 → NFC → 去掉 ignored 码点 → 全角句点映射为 `.`。
fn normalize_idn_hostname(text: &str) -> String {
    let fullwidth_mapped: String = text
        .chars()
        .map(|ch| {
            let point = ch as u32;
            // RE_FULLWIDTH_MAPPING：U+FF01..U+FF5E 映射回半角。
            if (0xFF01..=0xFF5E).contains(&point) {
                char::from_u32(point - 0xFEE0).unwrap_or(ch)
            } else {
                ch
            }
        })
        .collect();
    let normalized: String = fullwidth_mapped.nfc().collect();
    normalized
        .chars()
        .filter(|ch| {
            // RE_IGNORED_MAPPING
            !matches!(
                ch,
                '\u{00ad}'
                    | '\u{034f}'
                    | '\u{180b}'..='\u{180d}'
                    | '\u{200b}'
                    | '\u{fe00}'..='\u{fe0f}'
                    | '\u{e0100}'..='\u{e01ef}'
            )
        })
        .map(|ch| {
            // RE_FULL_STOP_MAPPING
            if matches!(ch, '\u{3002}' | '\u{ff0e}' | '\u{ff61}') {
                '.'
            } else {
                ch
            }
        })
        .collect()
}

/// `IsIdnHostname` 的单个标签：punycode 标签或 Unicode 标签。
fn is_idn_label(label: &str) -> bool {
    let length = label.encode_utf16().count();
    if length == 0 || length > 63 {
        return false;
    }
    is_puny_label(label) || is_unicode_label(label)
}

/// `Format.Test(format, value)`：未注册的格式一律通过。
fn format_test(name: &str, text: &str) -> bool {
    match name {
        "date-time" => is_date_time_format(text),
        "date" => is_date_format(text),
        "duration" => is_duration_format(text),
        "email" => is_email_format(text),
        "hostname" => is_hostname_format(text),
        "idn-email" => is_idn_email_format(text),
        "idn-hostname" => is_idn_hostname_format(text),
        "ipv4" => is_ipv4_format(text),
        "ipv6" => is_ipv6_format(text),
        "iri-reference" => is_iri_reference_format(text),
        "iri" => is_iri_format(text),
        "json-pointer-uri-fragment" => is_json_pointer_uri_fragment_format(text),
        "json-pointer" => is_json_pointer_format(text),
        "regex" => is_regex_format(text),
        "relative-json-pointer" => is_relative_json_pointer_format(text),
        "time" => is_time_format(text),
        "uri-reference" => is_uri_reference_format(text),
        "uri-template" => is_uri_template_format(text),
        "uri" => is_uri_format(text),
        "url" => is_url_format(text),
        "uuid" => is_uuid_format(text),
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// 共享的校验骨架（`Check` 与 `Error` 走同一棵树，只是收集方式不同）
// ---------------------------------------------------------------------------

/// `$ref` 解析用的栈帧：当前文档根 + 已展开的引用。
///
/// `refs` 记录 `(引用, 被校验值的地址)`：同一引用作用在**同一个值**上才是真环（typebox 会
/// `RangeError: Maximum call stack size exceeded`）；作用在更深的子值上则是正常的递归 schema。
#[derive(Debug, Clone, Copy)]
struct RefFrame<'a> {
    root: &'a Value,
    refs: &'a [(String, usize)],
}

fn value_address(value: &Value) -> usize {
    std::ptr::from_ref(value).addr()
}

/// `Check(schema, value)`：逐关键字与 TypeBox 的 `CheckSchema` 同序。
///
/// 只回答“是否合法”、不收集错误。`schema` 既不是对象也不是布尔（例如 `null`）时返回
/// `false`（`check_schema` 的入口分支决定）。对象/数组/字符串/数字各自的关键字分组按固定
/// 顺序短路，所以前面的组失败后，后面的关键字不会执行。需要错误列表时用
/// [`collect_json_schema_errors`]（独立遍历，有 [`MAX_ERRORS`] 上限，不可假设两者逐分支一致）。
pub fn check_json_schema(schema: &Value, value: &Value) -> bool {
    let frame = RefFrame {
        root: schema,
        refs: &[],
    };
    let mut context = Context::new();
    check_schema(schema, value, &frame, &mut context)
}

/// `Errors(schema, value)`：与 `Compile(schema).Errors(value)` 同序、同文本、同 `params`。
///
/// 返回顺序是关键字遍历顺序（不是按 `instance_path` 排序），最多 [`MAX_ERRORS`] 条；达到
/// 上限后更深层的子 schema 不再展开。列表为空**不保证**校验通过：`$ref` 指向非 schema 的
/// 分支会返回失败却不产生错误，[`validate_tool_arguments`] 因此准备了
/// `Unknown validation error` 兜底。
pub fn collect_json_schema_errors(schema: &Value, value: &Value) -> Vec<ValidationError> {
    collect_json_schema_errors_with_root(schema, schema, value)
}

/// 校验 `value` 是否符合 `schema`，但用 `root` 解析相对 `$ref`。
///
/// 子 schema 可能引用只定义在输出契约根上的 `$defs`（例如产物的属性 schema 是
/// `{"$ref":"#/$defs/Finding"}`）；直接把它当作校验根会让合法提交被拒绝。
pub fn collect_json_schema_errors_with_root(
    root: &Value,
    schema: &Value,
    value: &Value,
) -> Vec<ValidationError> {
    let frame = RefFrame { root, refs: &[] };
    let mut context = Context::new();
    error_schema(schema, "", value, &mut context, &frame);
    context.errors
}

/// `CheckTypeName`。
fn check_type_name(name: &str, value: &Value) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "boolean" => value.is_boolean(),
        "integer" => value
            .as_f64()
            .is_some_and(kiana_json::js_num::is_js_integer),
        "number" => value.is_number(),
        "null" => value.is_null(),
        "string" => value.is_string(),
        "bigint" | "constructor" | "function" | "symbol" | "undefined" | "void" => {
            // JSON 值里不存在这些类型。
            false
        }
        _ => true,
    }
}

/// `CheckType`。
fn check_type(schema: &Value, value: &Value) -> bool {
    match get(schema, "type") {
        None => true,
        Some(Value::String(name)) => check_type_name(name, value),
        Some(Value::Array(names)) => {
            names.iter().all(Value::is_string)
                && names.iter().any(|name| {
                    name.as_str()
                        .is_some_and(|name| check_type_name(name, value))
                })
        }
        Some(_) => true,
    }
}

/// `HasPropertyKey`：JSON 只有自有键（没有原型链）。
fn has_property_key(object: &Value, key: &str) -> bool {
    object.as_object().is_some_and(|map| map.contains_key(key))
}

/// `IsRequired`：`required` 是字符串数组。
///
/// 缺失或形状不对（含非字符串元素）时返回 `None`，调用方据此整体跳过 `required` 检查，
/// 而不是把它当作“所有键都缺失”。
fn required_keys(schema: &Value) -> Option<Vec<String>> {
    match get(schema, "required") {
        Some(Value::Array(items)) if items.iter().all(Value::is_string) => Some(
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect(),
        ),
        _ => None,
    }
}

/// `CheckRequired`。
fn check_required(schema: &Value, value: &Value) -> bool {
    required_keys(schema).is_none_or(|keys| keys.iter().all(|key| has_property_key(value, key)))
}

/// `CheckProperties`。
fn check_properties(
    schema: &Value,
    value: &Value,
    frame: &RefFrame<'_>,
    context: &mut Context,
) -> bool {
    let Some(Value::Object(properties)) = get(schema, "properties") else {
        return true;
    };
    if !properties
        .values()
        .all(|schema| schema.is_object() || schema.is_boolean())
    {
        return true;
    }
    properties.iter().all(|(key, property_schema)| {
        // `InexactOptionalCheck`（`value[key] === undefined`）在 JSON 里恒为 false，
        // 因此 `IsExactOptional` 的两个分支都等价于 `isProperty`。
        if !has_property_key(value, key) {
            return true;
        }
        let ok = value.get(key).is_some_and(|property| {
            push_checked(context, |context| {
                check_schema(property_schema, property, frame, context)
            })
        });
        if ok {
            context.add_key(key);
        }
        ok
    })
}

/// `CheckAdditionalProperties`：`properties`/`patternProperties` 之外的键要过
/// `additionalProperties`。
fn check_additional_properties(
    schema: &Value,
    value: &Value,
    frame: &RefFrame<'_>,
    context: &mut Context,
) -> bool {
    let Some(additional) = get(schema, "additionalProperties") else {
        return true;
    };
    if !(additional.is_object() || additional.is_boolean()) {
        return true;
    }
    let Some(object) = value.as_object() else {
        return true;
    };
    let regexp = properties_pattern(schema);
    object.iter().all(|(key, property)| {
        if regex_test(regexp.as_ref(), key) {
            return true;
        }
        let ok = push_checked(context, |context| {
            check_schema(additional, property, frame, context)
        });
        if ok {
            context.add_key(key);
        }
        ok
    })
}

/// `CheckPatternProperties`。
fn check_pattern_properties(
    schema: &Value,
    value: &Value,
    frame: &RefFrame<'_>,
    context: &mut Context,
) -> bool {
    let Some(Value::Object(patterns)) = get(schema, "patternProperties") else {
        return true;
    };
    if !patterns
        .values()
        .all(|schema| schema.is_object() || schema.is_boolean())
    {
        return true;
    }
    let Some(object) = value.as_object() else {
        return true;
    };
    for (pattern, property_schema) in patterns {
        let regexp = compile_pattern(pattern);
        for (key, property) in object {
            if !regex_test(regexp.as_ref(), key) {
                continue;
            }
            let ok = push_checked(context, |context| {
                check_schema(property_schema, property, frame, context)
            });
            if !ok {
                return false;
            }
            context.add_key(key);
        }
    }
    true
}

/// `CheckPropertyNames`。
fn check_property_names(
    schema: &Value,
    value: &Value,
    frame: &RefFrame<'_>,
    context: &mut Context,
) -> bool {
    let Some(property_names) = get(schema, "propertyNames") else {
        return true;
    };
    if !(property_names.is_object() || property_names.is_boolean()) {
        return true;
    }
    let Some(object) = value.as_object() else {
        return true;
    };
    object
        .keys()
        .all(|key| check_schema(property_names, &Value::String(key.clone()), frame, context))
}

/// `CheckMinProperties` / `CheckMaxProperties`。
fn check_property_count(schema: &Value, value: &Value, keyword: &str, is_min: bool) -> bool {
    let Some(Value::Number(limit)) = get(schema, keyword) else {
        return true;
    };
    let Some(limit) = limit.as_f64() else {
        return true;
    };
    let count = value.as_object().map_or(0, Map::len) as f64;
    if is_min {
        count >= limit
    } else {
        count <= limit
    }
}

/// `CheckDependencies` / `CheckDependentRequired` / `CheckDependentSchemas`。
fn check_dependencies(
    schema: &Value,
    value: &Value,
    frame: &RefFrame<'_>,
    context: &mut Context,
) -> bool {
    let Some(object) = value.as_object() else {
        return true;
    };
    if object.is_empty() {
        return true;
    }
    if let Some(Value::Object(dependencies)) = get(schema, "dependencies") {
        for (key, dependency) in dependencies {
            if !has_property_key(value, key) {
                continue;
            }
            let ok = match dependency {
                Value::Array(keys) => keys
                    .iter()
                    .all(|key| key.as_str().is_some_and(|key| has_property_key(value, key))),
                other => check_schema(other, value, frame, context),
            };
            if !ok {
                return false;
            }
        }
    }
    if let Some(Value::Object(dependent)) = get(schema, "dependentRequired") {
        for (key, keys) in dependent {
            if !has_property_key(value, key) {
                continue;
            }
            let ok = keys.as_array().is_none_or(|keys| {
                keys.iter()
                    .all(|key| key.as_str().is_some_and(|key| has_property_key(value, key)))
            });
            if !ok {
                return false;
            }
        }
    }
    if let Some(Value::Object(dependent)) = get(schema, "dependentSchemas") {
        for (key, nested) in dependent {
            if !has_property_key(value, key) {
                continue;
            }
            if !check_schema(nested, value, frame, context) {
                return false;
            }
        }
    }
    true
}

/// `CheckAdditionalItems`（仅对元组 `items` 生效）。
fn check_additional_items(
    schema: &Value,
    value: &Value,
    frame: &RefFrame<'_>,
    context: &mut Context,
) -> bool {
    let Some(Value::Array(tuple)) = get(schema, "items") else {
        return true;
    };
    if !tuple
        .iter()
        .all(|item| item.is_object() || item.is_boolean())
    {
        return true;
    }
    let Some(additional) = get(schema, "additionalItems") else {
        return true;
    };
    if !(additional.is_object() || additional.is_boolean()) {
        return true;
    }
    let Some(items) = value.as_array() else {
        return true;
    };
    items.iter().enumerate().all(|(index, item)| {
        if index < tuple.len() {
            return true;
        }
        let ok = push_checked(context, |context| {
            check_schema(additional, item, frame, context)
        });
        if ok {
            context.add_index(index);
        }
        ok
    })
}

/// `CheckItems`（元组逐位、数组逐元素）。
fn check_items(schema: &Value, value: &Value, frame: &RefFrame<'_>, context: &mut Context) -> bool {
    let Some(items_schema) = get(schema, "items") else {
        return true;
    };
    let Some(items) = value.as_array() else {
        return true;
    };
    match items_schema {
        Value::Array(tuple)
            if tuple
                .iter()
                .all(|item| item.is_object() || item.is_boolean()) =>
        {
            tuple.iter().enumerate().all(|(index, item_schema)| {
                let Some(item) = items.get(index) else {
                    return true;
                };
                let ok = push_checked(context, |context| {
                    check_schema(item_schema, item, frame, context)
                });
                if ok {
                    context.add_index(index);
                }
                ok
            })
        }
        Value::Object(_) | Value::Bool(_) => {
            let offset = get(schema, "prefixItems")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            items.iter().enumerate().skip(offset).all(|(index, item)| {
                let ok = push_checked(context, |context| {
                    check_schema(items_schema, item, frame, context)
                });
                if ok {
                    context.add_index(index);
                }
                ok
            })
        }
        _ => true,
    }
}

/// `CheckPrefixItems`。
fn check_prefix_items(
    schema: &Value,
    value: &Value,
    frame: &RefFrame<'_>,
    context: &mut Context,
) -> bool {
    let Some(Value::Array(prefix_items)) = get(schema, "prefixItems") else {
        return true;
    };
    if !prefix_items
        .iter()
        .all(|item| item.is_object() || item.is_boolean())
    {
        return true;
    }
    let Some(items) = value.as_array() else {
        return true;
    };
    if items.is_empty() {
        return true;
    }
    prefix_items.iter().enumerate().all(|(index, item_schema)| {
        let Some(item) = items.get(index) else {
            return true;
        };
        let ok = push_checked(context, |context| {
            check_schema(item_schema, item, frame, context)
        });
        if ok {
            context.add_index(index);
        }
        ok
    })
}

/// `CheckMinItems` / `CheckMaxItems`。
fn check_items_count(schema: &Value, value: &Value) -> bool {
    let Some(items) = value.as_array() else {
        return true;
    };
    for (keyword, is_min) in [("maxItems", false), ("minItems", true)] {
        let Some(Value::Number(limit)) = get(schema, keyword) else {
            continue;
        };
        let Some(limit) = limit.as_f64() else {
            continue;
        };
        let count = items.len() as f64;
        let ok = if is_min {
            count >= limit
        } else {
            count <= limit
        };
        if !ok {
            return false;
        }
    }
    true
}

/// `CheckUniqueItems`（`Hashing.Hash` 的等价物：结构等价）。
///
/// 实现是逐对深比较：对每个元素检查它是否等于任一前驱，最坏 O(n²) 次深比较。TS 侧用哈希
/// 去重；这里没有哈希表，大数组上的代价不同，不要把它当作查重哈希。
fn check_unique_items(schema: &Value, value: &Value) -> bool {
    let Some(Value::Bool(enabled)) = get(schema, "uniqueItems") else {
        return true;
    };
    if !*enabled {
        return true;
    }
    let Some(items) = value.as_array() else {
        return true;
    };
    for (index, item) in items.iter().enumerate() {
        if items[..index]
            .iter()
            .any(|previous| kiana_json::js_object::deep_equal(previous, item))
        {
            return false;
        }
    }
    true
}

/// `IsContains`：`contains` 关键字存在且是 schema。
fn is_contains(schema: &Value) -> bool {
    get(schema, "contains").is_some_and(|contains| contains.is_object() || contains.is_boolean())
}

/// `CheckContains`：`minContains: 0` 只让“至少一个”检查失效，`maxContains` 仍单独生效。
fn check_contains(
    schema: &Value,
    value: &Value,
    frame: &RefFrame<'_>,
    context: &mut Context,
) -> bool {
    if !is_contains(schema) {
        return true;
    }
    if get(schema, "minContains").and_then(Value::as_f64) == Some(0.0) {
        return true;
    }
    let Some(items) = value.as_array() else {
        return true;
    };
    if items.is_empty() {
        return false;
    }
    let Some(contains) = get(schema, "contains") else {
        return true;
    };
    let mut matched = false;
    for (index, item) in items.iter().enumerate() {
        if check_schema(contains, item, frame, context) {
            context.add_index(index);
            matched = true;
        }
    }
    matched
}

/// `CheckMaxContains`（不记录已求值下标）。
fn check_max_contains(
    schema: &Value,
    value: &Value,
    frame: &RefFrame<'_>,
    context: &mut Context,
) -> bool {
    if !is_contains(schema) {
        return true;
    }
    let Some(max) = get(schema, "maxContains").and_then(Value::as_f64) else {
        return true;
    };
    let Some(items) = value.as_array() else {
        return true;
    };
    let Some(contains) = get(schema, "contains") else {
        return true;
    };
    let mut count = 0usize;
    for item in items {
        if check_schema(contains, item, frame, context) {
            count += 1;
        }
    }
    count as f64 <= max
}

/// `CheckMinContains`（匹配的下标全部记录为已求值）。
fn check_min_contains(
    schema: &Value,
    value: &Value,
    frame: &RefFrame<'_>,
    context: &mut Context,
) -> bool {
    if !is_contains(schema) {
        return true;
    }
    let Some(min) = get(schema, "minContains").and_then(Value::as_f64) else {
        return true;
    };
    let Some(items) = value.as_array() else {
        return true;
    };
    let Some(contains) = get(schema, "contains") else {
        return true;
    };
    let mut count = 0usize;
    for (index, item) in items.iter().enumerate() {
        if check_schema(contains, item, frame, context) {
            context.add_index(index);
            count += 1;
        }
    }
    count as f64 >= min
}

/// `CheckMaxLength` / `CheckMinLength`。
///
/// `limit` 由 `f64` 强转成 `usize`：负数与 `NaN` 饱和为 0，非整数被截断。正常 schema 的
/// 长度限制是非负整数，不受影响；非法 schema（负数/小数限制）会与“直接按数值比较”的语义
/// 产生偏差，例如 `minLength: 2.5` 对 2 个字素会被这里判为通过。
fn check_string_length(schema: &Value, text: &str) -> bool {
    if let Some(Value::Number(limit)) = get(schema, "maxLength")
        && let Some(limit) = limit.as_f64()
        && !is_max_length(text, limit as usize)
    {
        return false;
    }
    if let Some(Value::Number(limit)) = get(schema, "minLength")
        && let Some(limit) = limit.as_f64()
        && !is_min_length(text, limit as usize)
    {
        return false;
    }
    true
}

/// `CheckFormat`。
fn check_format(schema: &Value, text: &str) -> bool {
    match get(schema, "format") {
        Some(Value::String(name)) => format_test(name, text),
        _ => true,
    }
}

/// `CheckPattern`。
fn check_pattern(schema: &Value, text: &str) -> bool {
    match get(schema, "pattern") {
        Some(Value::String(pattern)) => regex_test(compile_pattern(pattern).as_ref(), text),
        Some(_) => true,
        None => true,
    }
}

/// 数值比较关键字的通用实现。
fn compare_number(
    schema: &Value,
    value: &Value,
    keyword: &str,
    compare: fn(f64, f64) -> bool,
) -> bool {
    let Some(Value::Number(limit)) = get(schema, keyword) else {
        return true;
    };
    let (Some(limit), Some(number)) = (limit.as_f64(), value.as_f64()) else {
        return true;
    };
    compare(number, limit)
}

/// `IsMultipleOf`（typebox 的容差实现）。
///
/// 非有限被除数直接放行（JSON 数字不会出现 NaN/∞，该分支在正常调用中不可达）；除数为 0
/// 时模运算得到 `NaN`，最终判为不通过。整数被除数且 `1 / divisor` 为整数时走捷径，避免
/// `0.1` 这类无法精确表示的除数带来伪失败；其余情况用 `1e-10` 容差比较余数。
fn is_multiple_of(dividend: f64, divisor: f64) -> bool {
    if !dividend.is_finite() {
        return true;
    }
    if kiana_json::js_num::is_js_integer(dividend) && (1.0 / divisor) % 1.0 == 0.0 {
        return true;
    }
    let modulus = dividend % divisor;
    let tolerance = 1e-10;
    modulus
        .abs()
        .min((modulus - divisor).abs())
        .min((modulus + divisor).abs())
        < tolerance
}

/// `CheckRef`：`$ref` 解析（`#`、`#/a/b`、`#anchor`）。
fn check_ref(schema: &Value, value: &Value, frame: &RefFrame<'_>, context: &mut Context) -> bool {
    let Some(Value::String(reference)) = get(schema, "$ref") else {
        return true;
    };
    let Some(target) = resolve_ref(frame.root, reference) else {
        // 解析不到目标时 typebox 用 `false` 校验，即永远失败。
        return false;
    };
    if !(target.is_object() || target.is_boolean()) {
        // `Schema.IsSchema(target)` 为假：typebox 视为校验失败且不产生错误。
        return false;
    }
    if frame
        .refs
        .iter()
        .any(|(seen, address)| seen == reference && *address == value_address(value))
    {
        // 真环：typebox 会栈溢出（`RangeError`）。这里按“通过”处理，避免 Rust 侧 abort。
        return true;
    }
    let mut refs = frame.refs.to_vec();
    refs.push((reference.clone(), value_address(value)));
    let nested = RefFrame {
        root: frame.root,
        refs: &refs,
    };
    // `CheckRef` 在**新**的 `CheckContext` 里校验目标，成功后再合并键/索引。
    let mut next = Context::new();
    let result = check_schema(&target, value, &nested, &mut next);
    if result {
        context.merge(&next);
    }
    result
}

/// 解析 `$ref`：与 TypeBox 的 `Resolve.Ref`/`FromValue` 一致。
///
/// 对 `#...` 引用，typebox 以**词法根**（没有 `$id` 时就是根 schema）为起点做深度优先遍历：
/// 每访问一个 schema 对象，先按 `$anchor`/`$dynamicAnchor`/JSON Pointer 尝试匹配，再按
/// `Object.getOwnPropertyNames` 的顺序递归其属性值（跳过 `const`/`enum` 的值），数组逐项递归。
/// 因此引用可以命中**任意嵌套子 schema** 里的 `$defs`（例如属性 schema 自己声明的 `$defs`），
/// 而不只是根文档。
fn resolve_ref(root: &Value, reference: &str) -> Option<Value> {
    let rest = reference.strip_prefix('#')?;
    if rest.is_empty() {
        return Some(root.clone());
    }
    resolve_fragment(root, rest)
}

/// `FromValue`/`Match` 的等价实现（见 [`resolve_ref`]）。
fn resolve_fragment(node: &Value, fragment: &str) -> Option<Value> {
    match node {
        Value::Object(map) => {
            if map.get("$anchor").and_then(Value::as_str) == Some(fragment) {
                return Some(node.clone());
            }
            if map.get("$dynamicAnchor").and_then(Value::as_str) == Some(fragment) {
                return Some(node.clone());
            }
            if let Some(pointer) = fragment.strip_prefix('/')
                && let Some(found) = pointer_get(node, pointer)
            {
                return Some(found);
            }
            for (key, child) in map {
                // `SkipProperty`：`const`/`enum` 里的对象不是 schema。
                if key == "const" || key == "enum" {
                    continue;
                }
                if let Some(found) = resolve_fragment(child, fragment) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => {
            for item in items {
                if let Some(found) = resolve_fragment(item, fragment) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

/// `Pointer.Get`：`~1`/`~0` 解码，数组只接受规范数字下标，`__proto__`/`constructor`/
/// `prototype` 视为不可访问（typebox 的 `IsUnsafePropertyKey`）。
fn pointer_get(node: &Value, pointer: &str) -> Option<Value> {
    if pointer.is_empty() {
        return Some(node.clone());
    }
    let mut current = node.clone();
    for token in pointer.split('/') {
        let token = token.replace("~1", "/").replace("~0", "~");
        if matches!(token.as_str(), "__proto__" | "constructor" | "prototype") {
            return None;
        }
        current = match current {
            Value::Object(map) => map.get(&token)?.clone(),
            Value::Array(items) => {
                if !is_canonical_index(&token) {
                    return None;
                }
                items.get(token.parse::<usize>().ok()?)?.clone()
            }
            _ => return None,
        };
    }
    Some(current)
}

/// `Pointer.IsNumericIndex`：`/^(0|[1-9]\d*)$/`。
fn is_canonical_index(token: &str) -> bool {
    if token == "0" {
        return true;
    }
    let mut bytes = token.bytes();
    matches!(bytes.next(), Some(b'1'..=b'9')) && bytes.all(|byte| byte.is_ascii_digit())
}

/// `CheckConst`：JSON 值之间用深比较（`===` 对对象是按引用）。
fn check_const(schema: &Value, value: &Value) -> bool {
    match schema.as_object().and_then(|map| map.get("const")) {
        Some(constant) => kiana_json::js_object::deep_equal(value, constant),
        None => true,
    }
}

/// `CheckEnum`。
fn check_enum(schema: &Value, value: &Value) -> bool {
    match get(schema, "enum") {
        Some(Value::Array(options)) => options
            .iter()
            .any(|option| kiana_json::js_object::deep_equal(value, option)),
        _ => true,
    }
}

/// `CheckIf`：`if`/`then`/`else` 三个子 schema 共享当前上下文（不做 Push/Pop）。
fn check_if(schema: &Value, value: &Value, frame: &RefFrame<'_>, context: &mut Context) -> bool {
    let Some(if_schema) = get(schema, "if") else {
        return true;
    };
    if !(if_schema.is_object() || if_schema.is_boolean()) {
        return true;
    }
    let branch = if check_schema(if_schema, value, frame, context) {
        get(schema, "then")
    } else {
        get(schema, "else")
    };
    match branch {
        Some(branch) if branch.is_object() || branch.is_boolean() => {
            check_schema(branch, value, frame, context)
        }
        _ => true,
    }
}

/// `CheckNot`：子 schema 在**新**上下文里校验；`not` 通过（即子 schema 失败）时把子上下文
/// 栈顶的键/索引合并进来。
fn check_not(schema: &Value, value: &Value, frame: &RefFrame<'_>, context: &mut Context) -> bool {
    match get(schema, "not") {
        Some(not_schema) if not_schema.is_object() || not_schema.is_boolean() => {
            let mut next = Context::new();
            let failed = !check_schema(not_schema, value, frame, &mut next);
            if failed {
                context.merge(&next);
            }
            failed
        }
        _ => true,
    }
}

/// `CheckAllOf`：每个分支独立上下文，全部通过后合并所有分支的键/索引。
fn check_all_of(
    schema: &Value,
    value: &Value,
    frame: &RefFrame<'_>,
    context: &mut Context,
) -> bool {
    match get(schema, "allOf") {
        Some(Value::Array(schemas)) if schemas.iter().all(|s| s.is_object() || s.is_boolean()) => {
            let mut results: Vec<Context> = Vec::new();
            for nested in schemas {
                let mut next = Context::new();
                if check_schema(nested, value, frame, &mut next) {
                    results.push(next);
                }
            }
            if results.len() == schemas.len() {
                for result in &results {
                    context.merge(result);
                }
                true
            } else {
                false
            }
        }
        _ => true,
    }
}

/// `CheckAnyOf`：每个分支独立上下文，任一通过则合并所有通过分支的键/索引。
fn check_any_of(
    schema: &Value,
    value: &Value,
    frame: &RefFrame<'_>,
    context: &mut Context,
) -> bool {
    match get(schema, "anyOf") {
        Some(Value::Array(schemas)) if schemas.iter().all(|s| s.is_object() || s.is_boolean()) => {
            let mut results: Vec<Context> = Vec::new();
            for nested in schemas {
                let mut next = Context::new();
                if check_schema(nested, value, frame, &mut next) {
                    results.push(next);
                }
            }
            if results.is_empty() {
                return false;
            }
            for result in &results {
                context.merge(result);
            }
            true
        }
        _ => true,
    }
}

/// `CheckOneOf`：恰好一个分支通过时合并该分支的键/索引。
fn check_one_of(
    schema: &Value,
    value: &Value,
    frame: &RefFrame<'_>,
    context: &mut Context,
) -> bool {
    match get(schema, "oneOf") {
        Some(Value::Array(schemas)) if schemas.iter().all(|s| s.is_object() || s.is_boolean()) => {
            let mut results: Vec<Context> = Vec::new();
            for nested in schemas {
                let mut next = Context::new();
                if check_schema(nested, value, frame, &mut next) {
                    results.push(next);
                }
            }
            if results.len() != 1 {
                return false;
            }
            for result in &results {
                context.merge(result);
            }
            true
        }
        _ => true,
    }
}

/// `CheckUnevaluatedProperties`：对**对象和数组**（数组用下标字符串作为键）检查未求值的成员。
fn check_unevaluated_properties(
    schema: &Value,
    value: &Value,
    frame: &RefFrame<'_>,
    context: &mut Context,
) -> bool {
    let Some(unevaluated) = get(schema, "unevaluatedProperties") else {
        return true;
    };
    if !(unevaluated.is_object() || unevaluated.is_boolean()) {
        return true;
    }
    let entries = object_entries(value);
    for (key, property) in entries {
        if context.keys().contains(&key) {
            continue;
        }
        if !check_schema(unevaluated, property, frame, context) {
            return false;
        }
        context.add_key(&key);
    }
    true
}

/// `CheckUnevaluatedItems`：`prefixItems`/`items`/`contains` 等关键字把下标记为已求值。
fn check_unevaluated_items(
    schema: &Value,
    value: &Value,
    frame: &RefFrame<'_>,
    context: &mut Context,
) -> bool {
    let Some(unevaluated) = get(schema, "unevaluatedItems") else {
        return true;
    };
    if !(unevaluated.is_object() || unevaluated.is_boolean()) {
        return true;
    }
    let Some(items) = value.as_array() else {
        return true;
    };
    for (index, item) in items.iter().enumerate() {
        if context.indices().contains(&index) {
            continue;
        }
        if !check_schema(unevaluated, item, frame, context) {
            return false;
        }
        context.add_index(index);
    }
    true
}

/// `G.Entries(value)` 的等价物：对象按下标插入顺序，数组用 `"0".."n"` 作为键。
fn object_entries(value: &Value) -> Vec<(String, &Value)> {
    match value {
        Value::Object(map) => map.iter().map(|(key, item)| (key.clone(), item)).collect(),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| (index.to_string(), item))
            .collect(),
        _ => Vec::new(),
    }
}

/// `CheckSchema`：按固定的关键字顺序校验（顺序即契约，见下）。
///
/// 顺序本身就是不可见的契约：先 `type`，再按值的种类分组（对象 → 数组 → 字符串 → 数字），
/// 最后是 `$ref`/`const`/`enum`/`if`/`not`/`allOf`/`anyOf`/`oneOf`/`unevaluatedItems`，
/// `unevaluatedProperties` 放在最后（它读取前面关键字记下的“已求值键”）。每组内用 `||`
/// 短路，前面失败就不再求值后面的关键字——这也是它只返回布尔、不产生诊断的原因。
fn check_schema(
    schema: &Value,
    value: &Value,
    frame: &RefFrame<'_>,
    context: &mut Context,
) -> bool {
    match schema {
        Value::Bool(flag) => return *flag,
        Value::Object(_) => {}
        _ => return false,
    }
    if !check_type(schema, value) {
        return false;
    }
    if value.is_object()
        && (!check_required(schema, value)
            || !check_additional_properties(schema, value, frame, context)
            || !check_dependencies(schema, value, frame, context)
            || !check_pattern_properties(schema, value, frame, context)
            || !check_properties(schema, value, frame, context)
            || !check_property_names(schema, value, frame, context)
            || !check_property_count(schema, value, "minProperties", true)
            || !check_property_count(schema, value, "maxProperties", false))
    {
        return false;
    }
    if value.is_array()
        && (!check_additional_items(schema, value, frame, context)
            || !check_contains(schema, value, frame, context)
            || !check_items(schema, value, frame, context)
            || !check_max_contains(schema, value, frame, context)
            || !check_items_count(schema, value)
            || !check_min_contains(schema, value, frame, context)
            || !check_prefix_items(schema, value, frame, context)
            || !check_unique_items(schema, value))
    {
        return false;
    }
    if let Value::String(text) = value
        && (!check_string_length(schema, text)
            || !check_format(schema, text)
            || !check_pattern(schema, text))
    {
        return false;
    }
    if value.is_number() {
        if !compare_number(schema, value, "exclusiveMaximum", |a, b| a < b)
            || !compare_number(schema, value, "exclusiveMinimum", |a, b| a > b)
            || !compare_number(schema, value, "maximum", |a, b| a <= b)
            || !compare_number(schema, value, "minimum", |a, b| a >= b)
        {
            return false;
        }
        if let Some(Value::Number(divisor)) = get(schema, "multipleOf")
            && let (Some(divisor), Some(number)) = (divisor.as_f64(), value.as_f64())
            && !is_multiple_of(number, divisor)
        {
            return false;
        }
    }
    if !check_ref(schema, value, frame, context)
        || !check_const(schema, value)
        || !check_enum(schema, value)
        || !check_if(schema, value, frame, context)
        || !check_not(schema, value, frame, context)
        || !check_all_of(schema, value, frame, context)
        || !check_any_of(schema, value, frame, context)
        || !check_one_of(schema, value, frame, context)
        || !check_unevaluated_items(schema, value, frame, context)
    {
        return false;
    }
    // `unevaluatedProperties` 的门控是 `G.IsObject(value)`（数组也算 object）。
    if (value.is_object() || value.is_array())
        && !check_unevaluated_properties(schema, value, frame, context)
    {
        return false;
    }
    true
}

// ---------------------------------------------------------------------------
// `Errors`（`typebox/build/schema/engine/ErrorSchema`）
// ---------------------------------------------------------------------------

/// 把 `context` 里已有的错误并入 `into`（`failedContext.GetErrors().forEach(...)`）。
fn merge_errors(into: &mut Context, from: &Context) {
    for error in &from.errors {
        into.add_error(error.clone());
    }
}

/// `ErrorType`。
fn error_type(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    frame: &RefFrame<'_>,
) -> bool {
    if check_type(schema, value) {
        return true;
    }
    let schema_type = get(schema, "type").cloned().unwrap_or(Value::Null);
    context.add_error(ValidationError::new(
        "type",
        instance_path.to_string(),
        serde_json::json!({ "type": schema_type }),
    ));
    let _ = frame;
    false
}

/// `ErrorRequired`。
fn error_required(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
) -> bool {
    let Some(keys) = required_keys(schema) else {
        return true;
    };
    let missing: Vec<Value> = keys
        .iter()
        .filter(|key| !has_property_key(value, key))
        .map(|key| Value::String(key.clone()))
        .collect();
    if missing.is_empty() {
        return true;
    }
    context.add_error(ValidationError::new(
        "required",
        instance_path.to_string(),
        serde_json::json!({ "requiredProperties": missing }),
    ));
    false
}

/// `ErrorProperties`。
fn error_properties(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    frame: &RefFrame<'_>,
) -> bool {
    let Some(Value::Object(properties)) = get(schema, "properties") else {
        return true;
    };
    if !properties
        .values()
        .all(|schema| schema.is_object() || schema.is_boolean())
    {
        return true;
    }
    let mut result = true;
    for (key, property_schema) in properties {
        if !has_property_key(value, key) {
            continue;
        }
        let Some(property) = value.get(key) else {
            continue;
        };
        let next_instance_path = format!("{instance_path}/{key}");
        let ok = push_checked(context, |context| {
            error_schema(
                property_schema,
                &next_instance_path,
                property,
                context,
                frame,
            )
        });
        if ok {
            context.add_key(key);
        } else {
            result = false;
        }
    }
    result
}

/// `ErrorAdditionalProperties`。
fn error_additional_properties(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    frame: &RefFrame<'_>,
) -> bool {
    let Some(additional) = get(schema, "additionalProperties") else {
        return true;
    };
    if !(additional.is_object() || additional.is_boolean()) {
        return true;
    }
    let Some(object) = value.as_object() else {
        return true;
    };
    let regexp = properties_pattern(schema);
    let mut additional_properties: Vec<Value> = Vec::new();
    let mut result = true;
    for (key, property) in object {
        let next_instance_path = format!("{instance_path}/{key}");
        if regex_test(regexp.as_ref(), key) {
            continue;
        }
        let ok = push_checked(context, |context| {
            error_schema(additional, &next_instance_path, property, context, frame)
        });
        if ok {
            context.add_key(key);
        } else {
            additional_properties.push(Value::String(key.clone()));
            result = false;
        }
    }
    if result {
        return true;
    }
    context.add_error(ValidationError::new(
        "additionalProperties",
        instance_path.to_string(),
        serde_json::json!({ "additionalProperties": additional_properties }),
    ));
    false
}

/// `ErrorPatternProperties`。
fn error_pattern_properties(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    frame: &RefFrame<'_>,
) -> bool {
    let Some(Value::Object(patterns)) = get(schema, "patternProperties") else {
        return true;
    };
    if !patterns
        .values()
        .all(|schema| schema.is_object() || schema.is_boolean())
    {
        return true;
    }
    let Some(object) = value.as_object() else {
        return true;
    };
    let mut result = true;
    for (pattern, property_schema) in patterns {
        let regexp = compile_pattern(pattern);
        for (key, property) in object {
            if !regex_test(regexp.as_ref(), key) {
                continue;
            }
            let next_instance_path = format!("{instance_path}/{key}");
            let ok = push_checked(context, |context| {
                error_schema(
                    property_schema,
                    &next_instance_path,
                    property,
                    context,
                    frame,
                )
            });
            if ok {
                context.add_key(key);
            } else {
                result = false;
            }
        }
    }
    result
}

/// `ErrorPropertyNames`。
fn error_property_names(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    frame: &RefFrame<'_>,
) -> bool {
    let Some(property_names) = get(schema, "propertyNames") else {
        return true;
    };
    if !(property_names.is_object() || property_names.is_boolean()) {
        return true;
    }
    let Some(object) = value.as_object() else {
        return true;
    };
    let mut invalid: Vec<Value> = Vec::new();
    for key in object.keys() {
        let next_instance_path = format!("{instance_path}/{key}");
        if !error_schema(
            property_names,
            &next_instance_path,
            &Value::String(key.clone()),
            context,
            frame,
        ) {
            invalid.push(Value::String(key.clone()));
        }
    }
    if invalid.is_empty() {
        return true;
    }
    context.add_error(ValidationError::new(
        "propertyNames",
        instance_path.to_string(),
        serde_json::json!({ "propertyNames": invalid }),
    ));
    false
}

/// `ErrorMinProperties` / `ErrorMaxProperties`。
fn error_property_count(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    keyword: &str,
    is_min: bool,
) -> bool {
    let Some(Value::Number(limit)) = get(schema, keyword) else {
        return true;
    };
    if !check_property_count(schema, value, keyword, is_min) {
        context.add_error(ValidationError::new(
            keyword,
            instance_path.to_string(),
            serde_json::json!({ "limit": limit.clone() }),
        ));
        return false;
    }
    true
}

/// `ErrorDependencies` / `ErrorDependentRequired` / `ErrorDependentSchemas`。
fn error_dependencies(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    frame: &RefFrame<'_>,
) -> bool {
    let Some(object) = value.as_object() else {
        return true;
    };
    if object.is_empty() {
        return true;
    }
    let mut result = true;
    if let Some(Value::Object(dependencies)) = get(schema, "dependencies") {
        for (key, dependency) in dependencies {
            if !has_property_key(value, key) {
                continue;
            }
            match dependency {
                Value::Array(keys) => {
                    // typebox 用 `schema.every(...)`：首个缺失的依赖就短路，因此每个条目
                    // **只**产生一条 `dependencies` 错误（`dependentRequired` 用 `EveryAll`，
                    // 每个缺失键各产生一条）。
                    let missing = keys.iter().any(|dependency_key| {
                        !dependency_key
                            .as_str()
                            .is_some_and(|key| has_property_key(value, key))
                    });
                    if missing {
                        context.add_error(ValidationError::new(
                            "dependencies",
                            instance_path.to_string(),
                            serde_json::json!({ "property": key, "dependencies": keys }),
                        ));
                        result = false;
                    }
                }
                other => {
                    let _next_schema_path = format!("{instance_path}/dependencies/{key}");
                    if !error_schema(other, instance_path, value, context, frame) {
                        result = false;
                    }
                }
            }
        }
    }
    if let Some(Value::Object(dependent)) = get(schema, "dependentRequired") {
        for (key, keys) in dependent {
            if !has_property_key(value, key) {
                continue;
            }
            let Some(keys) = keys.as_array() else {
                continue;
            };
            for dependency in keys {
                let present = dependency
                    .as_str()
                    .is_some_and(|dependency| has_property_key(value, dependency));
                if !present {
                    context.add_error(ValidationError::new(
                        "dependentRequired",
                        instance_path.to_string(),
                        serde_json::json!({ "property": key, "dependencies": keys }),
                    ));
                    result = false;
                }
            }
        }
    }
    if let Some(Value::Object(dependent)) = get(schema, "dependentSchemas") {
        for (key, nested) in dependent {
            if !has_property_key(value, key) {
                continue;
            }
            let _next_schema_path = format!("{instance_path}/dependentSchemas/{key}");
            if !error_schema(nested, instance_path, value, context, frame) {
                result = false;
            }
        }
    }
    result
}

/// `ErrorItems`。
fn error_items(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    frame: &RefFrame<'_>,
) -> bool {
    let Some(items_schema) = get(schema, "items") else {
        return true;
    };
    let Some(items) = value.as_array() else {
        return true;
    };
    let mut result = true;
    match items_schema {
        Value::Array(tuple)
            if tuple
                .iter()
                .all(|item| item.is_object() || item.is_boolean()) =>
        {
            for (index, item_schema) in tuple.iter().enumerate() {
                let Some(item) = items.get(index) else {
                    continue;
                };
                let next_instance_path = format!("{instance_path}/{index}");
                let ok = push_checked(context, |context| {
                    error_schema(item_schema, &next_instance_path, item, context, frame)
                });
                if ok {
                    context.add_index(index);
                } else {
                    result = false;
                }
            }
        }
        Value::Object(_) | Value::Bool(_) => {
            let offset = get(schema, "prefixItems")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            for (index, item) in items.iter().enumerate().skip(offset) {
                let next_instance_path = format!("{instance_path}/{index}");
                let ok = push_checked(context, |context| {
                    error_schema(items_schema, &next_instance_path, item, context, frame)
                });
                if ok {
                    context.add_index(index);
                } else {
                    result = false;
                }
            }
        }
        _ => {}
    }
    result
}

/// `ErrorPrefixItems`。
fn error_prefix_items(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    frame: &RefFrame<'_>,
) -> bool {
    let Some(Value::Array(prefix_items)) = get(schema, "prefixItems") else {
        return true;
    };
    if !prefix_items
        .iter()
        .all(|item| item.is_object() || item.is_boolean())
    {
        return true;
    }
    let Some(items) = value.as_array() else {
        return true;
    };
    if items.is_empty() {
        return true;
    }
    let mut result = true;
    for (index, item_schema) in prefix_items.iter().enumerate() {
        let Some(item) = items.get(index) else {
            continue;
        };
        let next_instance_path = format!("{instance_path}/{index}");
        let ok = push_checked(context, |context| {
            error_schema(item_schema, &next_instance_path, item, context, frame)
        });
        if ok {
            context.add_index(index);
        } else {
            result = false;
        }
    }
    result
}

/// `ErrorAdditionalItems`。
fn error_additional_items(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    frame: &RefFrame<'_>,
) -> bool {
    let Some(Value::Array(tuple)) = get(schema, "items") else {
        return true;
    };
    if !tuple
        .iter()
        .all(|item| item.is_object() || item.is_boolean())
    {
        return true;
    }
    let Some(additional) = get(schema, "additionalItems") else {
        return true;
    };
    if !(additional.is_object() || additional.is_boolean()) {
        return true;
    }
    let Some(items) = value.as_array() else {
        return true;
    };
    let mut result = true;
    for (index, item) in items.iter().enumerate() {
        if index < tuple.len() {
            continue;
        }
        let next_instance_path = format!("{instance_path}/{index}");
        let ok = push_checked(context, |context| {
            error_schema(additional, &next_instance_path, item, context, frame)
        });
        if ok {
            context.add_index(index);
        } else {
            result = false;
        }
        // typebox 的 `ErrorAdditionalItems` 用 `G.Every`（短路），首个失败后不再检查后续元素。
        if !ok {
            break;
        }
    }
    result
}

/// `ErrorMinItems` / `ErrorMaxItems`。
fn error_item_limit(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    keyword: &str,
    is_min: bool,
) -> bool {
    let Some(Value::Number(limit)) = get(schema, keyword) else {
        return true;
    };
    let Some(limit_value) = limit.as_f64() else {
        return true;
    };
    let count = value.as_array().map_or(0, Vec::len) as f64;
    let ok = if is_min {
        count >= limit_value
    } else {
        count <= limit_value
    };
    if ok {
        return true;
    }
    context.add_error(ValidationError::new(
        keyword,
        instance_path.to_string(),
        serde_json::json!({ "limit": limit.clone() }),
    ));
    false
}

/// `ErrorUniqueItems`：`params.duplicateItems` 是**重复项的下标**。
fn error_unique_items(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
) -> bool {
    let Some(Value::Bool(enabled)) = get(schema, "uniqueItems") else {
        return true;
    };
    if !*enabled {
        return true;
    }
    let Some(items) = value.as_array() else {
        return true;
    };
    let mut duplicates: Vec<Value> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if items[..index]
            .iter()
            .any(|previous| kiana_json::js_object::deep_equal(previous, item))
        {
            duplicates.push(Value::from(index));
        }
    }
    if duplicates.is_empty() {
        return true;
    }
    context.add_error(ValidationError::new(
        "uniqueItems",
        instance_path.to_string(),
        serde_json::json!({ "duplicateItems": duplicates }),
    ));
    false
}

/// `ErrorContains`：`params = {minContains: 1}`；`minContains: 0` 时本关键字被跳过。
fn error_contains(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    frame: &RefFrame<'_>,
) -> bool {
    if !is_contains(schema) {
        return true;
    }
    if get(schema, "minContains").and_then(Value::as_f64) == Some(0.0) {
        return true;
    }
    let Some(contains) = get(schema, "contains") else {
        return true;
    };
    let Some(items) = value.as_array() else {
        return true;
    };
    // `CheckContains`：空数组直接失败（`SomeAll` 不执行，也就不记录下标）。
    if !items.is_empty() {
        let mut matched = false;
        for (index, item) in items.iter().enumerate() {
            if check_schema(contains, item, frame, context) {
                context.add_index(index);
                matched = true;
            }
        }
        if matched {
            return true;
        }
    }
    context.add_error(ValidationError::new(
        "contains",
        instance_path.to_string(),
        serde_json::json!({ "minContains": 1 }),
    ));
    false
}

/// `ErrorMaxContains`：`params = {minContains: minContains ?? 1, maxContains}`。
///
/// 与 `contains` 不同，`minContains: 0` **不会**禁用本关键字。
fn error_max_contains(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    frame: &RefFrame<'_>,
) -> bool {
    let Some(max) = get(schema, "maxContains") else {
        return true;
    };
    if !is_contains(schema) {
        return true;
    }
    let Some(max) = max.as_f64() else {
        return true;
    };
    let Some(contains) = get(schema, "contains") else {
        return true;
    };
    let Some(items) = value.as_array() else {
        return true;
    };
    let mut count = 0usize;
    for item in items {
        if check_schema(contains, item, frame, context) {
            count += 1;
        }
    }
    if count as f64 <= max {
        return true;
    }
    let min_contains = get(schema, "minContains")
        .and_then(Value::as_f64)
        .map_or(Value::from(1), number_value);
    context.add_error(ValidationError::new(
        "contains",
        instance_path.to_string(),
        serde_json::json!({ "minContains": min_contains, "maxContains": number_value(max) }),
    ));
    false
}

/// `ErrorMinContains`：`params = {minContains}`（匹配项的下标全部记录为已求值）。
fn error_min_contains(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    frame: &RefFrame<'_>,
) -> bool {
    let Some(min) = get(schema, "minContains") else {
        return true;
    };
    if !is_contains(schema) {
        return true;
    }
    let Some(min) = min.as_f64() else {
        return true;
    };
    let Some(contains) = get(schema, "contains") else {
        return true;
    };
    let Some(items) = value.as_array() else {
        return true;
    };
    let mut count = 0usize;
    for (index, item) in items.iter().enumerate() {
        if check_schema(contains, item, frame, context) {
            context.add_index(index);
            count += 1;
        }
    }
    if count as f64 >= min {
        return true;
    }
    context.add_error(ValidationError::new(
        "contains",
        instance_path.to_string(),
        serde_json::json!({ "minContains": number_value(min) }),
    ));
    false
}

/// `ErrorMaxLength` / `ErrorMinLength` / `ErrorFormat` / `ErrorPattern`（字符串关键字，
/// 顺序与 typebox 一致：maxLength → minLength → format → pattern）。
fn error_string_keywords(
    schema: &Value,
    instance_path: &str,
    text: &str,
    context: &mut Context,
) -> bool {
    let mut result = true;
    for (keyword, is_min) in [("maxLength", false), ("minLength", true)] {
        let Some(Value::Number(limit)) = get(schema, keyword) else {
            continue;
        };
        let Some(limit_value) = limit.as_f64() else {
            continue;
        };
        // 与 `check_string_length` 相同：`f64 as usize` 会截断/饱和，正常 schema 的非负
        // 整数 limit 不受影响。
        let ok = if is_min {
            is_min_length(text, limit_value as usize)
        } else {
            is_max_length(text, limit_value as usize)
        };
        if !ok {
            context.add_error(ValidationError::new(
                keyword,
                instance_path.to_string(),
                serde_json::json!({ "limit": limit.clone() }),
            ));
            result = false;
        }
    }
    if let Some(Value::String(name)) = get(schema, "format")
        && !format_test(name, text)
    {
        context.add_error(ValidationError::new(
            "format",
            instance_path.to_string(),
            serde_json::json!({ "format": name }),
        ));
        result = false;
    }
    if let Some(Value::String(pattern)) = get(schema, "pattern")
        && !regex_test(compile_pattern(pattern).as_ref(), text)
    {
        context.add_error(ValidationError::new(
            "pattern",
            instance_path.to_string(),
            serde_json::json!({ "pattern": pattern }),
        ));
        result = false;
    }
    result
}

/// 数值比较函数（`<`/`>`/`<=`/`>=`）。
type ComparisonFn = fn(f64, f64) -> bool;

/// 数值关键字的错误收集（`exclusiveMaximum` → `exclusiveMinimum` → `maximum` → `minimum`
/// → `multipleOf`，与 typebox 的顺序一致）。
fn error_number_keywords(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
) -> bool {
    let mut result = true;
    let comparisons: [(&str, &str, ComparisonFn); 4] = [
        ("exclusiveMaximum", "<", |a, b| a < b),
        ("exclusiveMinimum", ">", |a, b| a > b),
        ("maximum", "<=", |a, b| a <= b),
        ("minimum", ">=", |a, b| a >= b),
    ];
    for (keyword, comparison, compare) in comparisons {
        let Some(Value::Number(limit)) = get(schema, keyword) else {
            continue;
        };
        let (Some(limit_value), Some(number)) = (limit.as_f64(), value.as_f64()) else {
            continue;
        };
        if !compare(number, limit_value) {
            context.add_error(ValidationError::new(
                keyword,
                instance_path.to_string(),
                serde_json::json!({ "comparison": comparison, "limit": limit.clone() }),
            ));
            result = false;
        }
    }
    if let Some(Value::Number(divisor)) = get(schema, "multipleOf")
        && let (Some(divisor_value), Some(number)) = (divisor.as_f64(), value.as_f64())
        && !is_multiple_of(number, divisor_value)
    {
        context.add_error(ValidationError::new(
            "multipleOf",
            instance_path.to_string(),
            serde_json::json!({ "multipleOf": divisor }),
        ));
        result = false;
    }
    result
}

/// `ErrorConst`。
fn error_const(schema: &Value, instance_path: &str, value: &Value, context: &mut Context) -> bool {
    let Some(constant) = schema.as_object().and_then(|map| map.get("const")) else {
        return true;
    };
    if kiana_json::js_object::deep_equal(value, constant) {
        return true;
    }
    context.add_error(ValidationError::new(
        "const",
        instance_path.to_string(),
        serde_json::json!({ "allowedValue": constant }),
    ));
    false
}

/// `ErrorEnum`。
fn error_enum(schema: &Value, instance_path: &str, value: &Value, context: &mut Context) -> bool {
    let Some(Value::Array(options)) = get(schema, "enum") else {
        return true;
    };
    if options
        .iter()
        .any(|option| kiana_json::js_object::deep_equal(value, option))
    {
        return true;
    }
    context.add_error(ValidationError::new(
        "enum",
        instance_path.to_string(),
        serde_json::json!({ "allowedValues": options }),
    ));
    false
}

/// `ErrorIf`：`then`/`else` 的失败消息落在**各自的**上下文中，`if` 关键字只报
/// `{failingKeyword}`。
fn error_if(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    frame: &RefFrame<'_>,
) -> bool {
    let Some(if_schema) = get(schema, "if") else {
        return true;
    };
    if !(if_schema.is_object() || if_schema.is_boolean()) {
        return true;
    }
    let mut local = Context::new();
    let result = if error_schema(if_schema, instance_path, value, &mut local, frame) {
        let then_schema = get(schema, "then").cloned().unwrap_or(Value::Bool(true));
        let ok = error_schema(&then_schema, instance_path, value, &mut local, frame);
        if !ok {
            context.add_error(ValidationError::new(
                "if",
                instance_path.to_string(),
                serde_json::json!({ "failingKeyword": "then" }),
            ));
        }
        ok
    } else {
        // `else` 分支的错误直接进外层上下文（与 typebox 一致）。
        let else_schema = get(schema, "else").cloned().unwrap_or(Value::Bool(true));
        let ok = error_schema(&else_schema, instance_path, value, context, frame);
        if !ok {
            context.add_error(ValidationError::new(
                "if",
                instance_path.to_string(),
                serde_json::json!({ "failingKeyword": "else" }),
            ));
        }
        ok
    };
    if result {
        context.merge(&local);
    }
    result
}

/// `ErrorNot`：与 `CheckNot` 一致 —— 子 schema 在**新**上下文里校验，子 schema 失败时合并
/// 其栈顶的键/索引；子 schema 通过则报 `not` 错误。
fn error_not(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    frame: &RefFrame<'_>,
) -> bool {
    let Some(not_schema) = get(schema, "not") else {
        return true;
    };
    if !(not_schema.is_object() || not_schema.is_boolean()) {
        return true;
    }
    let mut next = Context::new();
    let failed = !check_schema(not_schema, value, frame, &mut next);
    if failed {
        context.merge(&next);
        return true;
    }
    context.add_error(ValidationError::new(
        "not",
        instance_path.to_string(),
        serde_json::json!({}),
    ));
    false
}

/// `ErrorAllOf`：失败时不产生 `allOf` 自身的错误，只把失败分支的错误并进外层。
fn error_all_of(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    frame: &RefFrame<'_>,
) -> bool {
    let Some(Value::Array(schemas)) = get(schema, "allOf") else {
        return true;
    };
    if !schemas.iter().all(|s| s.is_object() || s.is_boolean()) {
        return true;
    }
    let mut failed: Vec<Context> = Vec::new();
    let mut results: Vec<Context> = Vec::new();
    for (index, nested) in schemas.iter().enumerate() {
        let mut local = Context::new();
        // typebox 只把下标拼进 **schemaPath**；`instancePath` 保持调用方传入的值。
        let _schema_path = format!("{instance_path}/allOf/{index}");
        if error_schema(nested, instance_path, value, &mut local, frame) {
            results.push(local);
        } else {
            failed.push(local);
        }
    }
    if results.len() == schemas.len() {
        for result in &results {
            context.merge(result);
        }
        return true;
    }
    for failure in failed {
        merge_errors(context, &failure);
    }
    false
}

/// `ErrorAnyOf`。
fn error_any_of(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    frame: &RefFrame<'_>,
) -> bool {
    let Some(Value::Array(schemas)) = get(schema, "anyOf") else {
        return true;
    };
    if !schemas.iter().all(|s| s.is_object() || s.is_boolean()) {
        return true;
    }
    let mut failed: Vec<Context> = Vec::new();
    let mut results: Vec<Context> = Vec::new();
    for (index, nested) in schemas.iter().enumerate() {
        let mut local = Context::new();
        let _schema_path = format!("{instance_path}/anyOf/{index}");
        if error_schema(nested, instance_path, value, &mut local, frame) {
            results.push(local);
        } else {
            failed.push(local);
        }
    }
    if !results.is_empty() {
        for result in &results {
            context.merge(result);
        }
        return true;
    }
    for failure in failed {
        merge_errors(context, &failure);
    }
    context.add_error(ValidationError::new(
        "anyOf",
        instance_path.to_string(),
        serde_json::json!({}),
    ));
    false
}

/// `ErrorOneOf`。
fn error_one_of(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    frame: &RefFrame<'_>,
) -> bool {
    let Some(Value::Array(schemas)) = get(schema, "oneOf") else {
        return true;
    };
    if !schemas.iter().all(|s| s.is_object() || s.is_boolean()) {
        return true;
    }
    let mut failed: Vec<Context> = Vec::new();
    let mut results: Vec<Context> = Vec::new();
    let mut passing_schemas: Vec<Value> = Vec::new();
    for (index, nested) in schemas.iter().enumerate() {
        let mut local = Context::new();
        let _schema_path = format!("{instance_path}/oneOf/{index}");
        if error_schema(nested, instance_path, value, &mut local, frame) {
            passing_schemas.push(Value::from(index));
            results.push(local);
        } else {
            failed.push(local);
        }
    }
    if results.len() == 1 {
        for result in &results {
            context.merge(result);
        }
        return true;
    }
    if passing_schemas.is_empty() {
        for failure in failed {
            merge_errors(context, &failure);
        }
    }
    context.add_error(ValidationError::new(
        "oneOf",
        instance_path.to_string(),
        serde_json::json!({ "passingSchemas": passing_schemas }),
    ));
    false
}

/// `ErrorRef`。
fn error_ref(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    frame: &RefFrame<'_>,
) -> bool {
    let Some(Value::String(reference)) = get(schema, "$ref") else {
        return true;
    };
    let Some(target) = resolve_ref(frame.root, reference) else {
        // 解析失败时 typebox 用 `false`，即一条 `boolean`/`schema is false` 错误。
        context.add_error(ValidationError::new(
            "boolean",
            instance_path.to_string(),
            serde_json::json!({}),
        ));
        return false;
    };
    if !(target.is_object() || target.is_boolean()) {
        // `Schema.IsSchema(target)` 为假：视为校验失败，但不产生错误。
        return false;
    }
    if frame
        .refs
        .iter()
        .any(|(seen, address)| seen == reference && *address == value_address(value))
    {
        return true;
    }
    let mut refs = frame.refs.to_vec();
    refs.push((reference.clone(), value_address(value)));
    let nested = RefFrame {
        root: frame.root,
        refs: &refs,
    };
    let mut local = Context::new();
    // `ErrorRef` 用 `schemaPath = '#'`，但错误本身按 `instancePath` 记录。
    let result = error_schema(&target, instance_path, value, &mut local, &nested);
    if result {
        context.merge(&local);
    } else {
        merge_errors(context, &local);
    }
    result
}

/// `ErrorUnevaluatedItems`：已求值的下标（`items`/`prefixItems`/`contains` 等）不再校验。
fn error_unevaluated_items(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    frame: &RefFrame<'_>,
) -> bool {
    let Some(unevaluated) = get(schema, "unevaluatedItems") else {
        return true;
    };
    if !(unevaluated.is_object() || unevaluated.is_boolean()) {
        return true;
    }
    let Some(items) = value.as_array() else {
        return true;
    };
    let mut unevaluated_items: Vec<Value> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if context.indices().contains(&index) {
            continue;
        }
        // 子 schema 的错误落在一个丢弃的上下文里，只把“已求值”记到外层。
        let mut local = Context::new();
        if error_schema(unevaluated, instance_path, item, &mut local, frame) {
            context.add_index(index);
        } else {
            unevaluated_items.push(Value::from(index));
        }
    }
    if unevaluated_items.is_empty() {
        return true;
    }
    context.add_error(ValidationError::new(
        "unevaluatedItems",
        instance_path.to_string(),
        serde_json::json!({ "unevaluatedItems": unevaluated_items }),
    ));
    false
}

/// `ErrorUnevaluatedProperties`：对**对象和数组**（数组用下标字符串作为键）检查未求值的成员。
fn error_unevaluated_properties(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    frame: &RefFrame<'_>,
) -> bool {
    let Some(unevaluated) = get(schema, "unevaluatedProperties") else {
        return true;
    };
    if !(unevaluated.is_object() || unevaluated.is_boolean()) {
        return true;
    }
    let mut unevaluated_properties: Vec<Value> = Vec::new();
    for (key, property) in object_entries(value) {
        if context.keys().contains(&key) {
            continue;
        }
        // 子 schema 的错误落在一个丢弃的上下文里，只把“已求值”记到外层。
        let mut local = Context::new();
        if error_schema(unevaluated, instance_path, property, &mut local, frame) {
            context.add_key(&key);
        } else {
            unevaluated_properties.push(Value::String(key));
        }
    }
    if unevaluated_properties.is_empty() {
        return true;
    }
    context.add_error(ValidationError::new(
        "unevaluatedProperties",
        instance_path.to_string(),
        serde_json::json!({ "unevaluatedProperties": unevaluated_properties }),
    ));
    false
}

/// `ErrorSchemaBoolean` / 非 schema：`schema is false`。
///
/// 与 [`check_schema`] 不同，这里**不短路**：每个关键字组都会执行。`result = error_xxx(...)
/// && result` 把调用放在 `&&` 左边，正是为了让副作用（追加错误）一定发生，`result` 只记录
/// 整体结论。因此错误列表按固定的关键字顺序排列，而不是“第一个失败点”。递归入口先检查
/// 容量（[`MAX_ERRORS`]），满了立即返回 false，不再为无法展示的错误继续遍历。
fn error_schema(
    schema: &Value,
    instance_path: &str,
    value: &Value,
    context: &mut Context,
    frame: &RefFrame<'_>,
) -> bool {
    if context.at_capacity() {
        return false;
    }
    match schema {
        Value::Bool(true) => return true,
        Value::Bool(false) => {
            context.add_error(ValidationError::new(
                "boolean",
                instance_path.to_string(),
                serde_json::json!({}),
            ));
            return false;
        }
        Value::Object(_) => {}
        _ => {
            context.add_error(ValidationError::new(
                "boolean",
                instance_path.to_string(),
                serde_json::json!({}),
            ));
            return false;
        }
    }
    let mut result = error_type(schema, instance_path, value, context, frame);
    if value.is_object() {
        result = error_required(schema, instance_path, value, context) && result;
        result =
            error_additional_properties(schema, instance_path, value, context, frame) && result;
        result = error_dependencies(schema, instance_path, value, context, frame) && result;
        result = error_pattern_properties(schema, instance_path, value, context, frame) && result;
        result = error_properties(schema, instance_path, value, context, frame) && result;
        result = error_property_names(schema, instance_path, value, context, frame) && result;
        result = error_property_count(schema, instance_path, value, context, "minProperties", true)
            && result;
        result = error_property_count(
            schema,
            instance_path,
            value,
            context,
            "maxProperties",
            false,
        ) && result;
    }
    if value.is_array() {
        // 与 typebox 的 `ErrorSchema` 同序：additionalItems、contains、items、maxContains、
        // maxItems、minContains、minItems、prefixItems、uniqueItems。
        result = error_additional_items(schema, instance_path, value, context, frame) && result;
        result = error_contains(schema, instance_path, value, context, frame) && result;
        result = error_items(schema, instance_path, value, context, frame) && result;
        result = error_max_contains(schema, instance_path, value, context, frame) && result;
        result =
            error_item_limit(schema, instance_path, value, context, "maxItems", false) && result;
        result = error_min_contains(schema, instance_path, value, context, frame) && result;
        result =
            error_item_limit(schema, instance_path, value, context, "minItems", true) && result;
        result = error_prefix_items(schema, instance_path, value, context, frame) && result;
        result = error_unique_items(schema, instance_path, value, context) && result;
    }
    if let Value::String(text) = value {
        result = error_string_keywords(schema, instance_path, text, context) && result;
    }
    if value.is_number() {
        result = error_number_keywords(schema, instance_path, value, context) && result;
    }
    result = error_ref(schema, instance_path, value, context, frame) && result;
    result = error_const(schema, instance_path, value, context) && result;
    result = error_enum(schema, instance_path, value, context) && result;
    result = error_if(schema, instance_path, value, context, frame) && result;
    result = error_not(schema, instance_path, value, context, frame) && result;
    result = error_all_of(schema, instance_path, value, context, frame) && result;
    result = error_any_of(schema, instance_path, value, context, frame) && result;
    result = error_one_of(schema, instance_path, value, context, frame) && result;
    result = error_unevaluated_items(schema, instance_path, value, context, frame) && result;
    // `unevaluatedProperties` 的门控是 `G.IsObject(value)`（数组也算 object，用下标作键）。
    if value.is_object() || value.is_array() {
        result =
            error_unevaluated_properties(schema, instance_path, value, context, frame) && result;
    }
    result
}

// ---------------------------------------------------------------------------
// `Value.Convert`
// ---------------------------------------------------------------------------

/// TypeBox 的 kind（隐藏的 `~kind` 字符串）。Rust 侧 schema 是 JSON，调用方用显式 `"~kind"`
/// 键标记“活 TypeBox 节点”（JSON 没有不可枚举属性）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConvertKind {
    Array,
    BigInt,
    Boolean,
    Cyclic,
    Enum,
    Integer,
    Intersect,
    Literal,
    Null,
    Number,
    Object,
    Record,
    Ref,
    String,
    TemplateLiteral,
    Tuple,
    Undefined,
    Union,
    Void,
    None,
}

fn kind_from_name(name: &str) -> ConvertKind {
    match name {
        "Array" => ConvertKind::Array,
        "BigInt" => ConvertKind::BigInt,
        "Boolean" => ConvertKind::Boolean,
        "Cyclic" => ConvertKind::Cyclic,
        "Enum" => ConvertKind::Enum,
        "Integer" => ConvertKind::Integer,
        "Intersect" => ConvertKind::Intersect,
        "Literal" => ConvertKind::Literal,
        "Null" => ConvertKind::Null,
        "Number" => ConvertKind::Number,
        "Object" => ConvertKind::Object,
        "Record" => ConvertKind::Record,
        "Ref" => ConvertKind::Ref,
        "String" => ConvertKind::String,
        "TemplateLiteral" => ConvertKind::TemplateLiteral,
        "Tuple" => ConvertKind::Tuple,
        "Undefined" => ConvertKind::Undefined,
        "Union" => ConvertKind::Union,
        "Void" => ConvertKind::Void,
        _ => ConvertKind::None,
    }
}

/// `schema` 的 TypeBox kind：`FromType` 按 `Type.Is*`（即自有 `~kind` 字符串）分派，普通 JSON
/// schema 没有任何 kind，`Value.Convert` 对它**整体空操作**（TS 里 `JSON.parse` 出来的 schema
/// 就是这样）。Rust 侧用显式 `"~kind"` 键表示活节点。
fn schema_convert_kind(schema: &Value) -> ConvertKind {
    match get(schema, "~kind") {
        Some(Value::String(name)) => kind_from_name(name),
        _ => ConvertKind::None,
    }
}

/// TypeBox 的 `FromType(context, schema, value)`（`context` 在 `validateToolArguments` 里是
/// 空对象，因此 `$ref` 不展开）。
fn convert_from_type(schema: &Value, value: Value) -> Value {
    match schema_convert_kind(schema) {
        ConvertKind::Literal => convert_literal(schema, value),
        ConvertKind::Enum => convert_enum(schema, value),
        ConvertKind::Union => convert_union(schema, value),
        ConvertKind::Intersect => convert_intersect(schema, value),
        ConvertKind::Object => convert_object_schema(schema, value),
        ConvertKind::Record => convert_record_schema(schema, value),
        ConvertKind::Array => convert_array(schema, value),
        ConvertKind::Tuple => convert_tuple(schema, value),
        ConvertKind::Number => try_number(value),
        ConvertKind::Integer => try_integer(value),
        ConvertKind::Boolean => try_boolean(value),
        ConvertKind::String => try_string(value),
        ConvertKind::Null => try_null(value),
        // `BigInt` 在 JSON 里没有表示；`Ref`/`Cyclic`/`Undefined`/`Void` 在
        // `validateToolArguments` 的 `Value.Convert(parameters, args)` 调用里都是空操作。
        _ => value,
    }
}

/// `FromLiteral`：`const` 等值时原样保留，否则按字面量类型尝试 `TryNumber/TryString/TryBoolean`。
fn convert_literal(schema: &Value, value: Value) -> Value {
    let Some(const_value) = get(schema, "const") else {
        return value;
    };
    if kiana_json::js_string::js_strict_equal(const_value, &value) {
        return value;
    }
    let converted = match const_value {
        Value::Number(_) => try_number(value.clone()),
        Value::String(_) => try_string(value.clone()),
        Value::Bool(_) => try_boolean(value.clone()),
        _ => return value,
    };
    if kiana_json::js_string::js_strict_equal(const_value, &converted) {
        converted
    } else {
        value
    }
}

/// `FromEnum`：`Evaluate(Enum)` 展开成字面量联合，再走 `FromUnion`。
fn convert_enum(schema: &Value, value: Value) -> Value {
    let Some(members) = get(schema, "enum").and_then(Value::as_array) else {
        return value;
    };
    let arms: Vec<Value> = members.iter().map(literal_arm).collect();
    let mut union_schema = Map::new();
    union_schema.insert("anyOf".to_string(), Value::Array(arms.clone()));
    convert_union_arms(&Value::Object(union_schema), &arms, value)
}

fn literal_arm(member: &Value) -> Value {
    let mut schema = Map::new();
    // `Evaluate(Enum)` 产出的是 `Type.Literal(...)` 活节点（自带 `~kind`）。
    schema.insert("~kind".to_string(), Value::String("Literal".to_string()));
    schema.insert(
        "type".to_string(),
        Value::String(js_typeof_name(member).to_string()),
    );
    schema.insert("const".to_string(), member.clone());
    Value::Object(schema)
}

fn js_typeof_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) | Value::Object(_) => "object",
    }
}

/// `FromUnion`：先看原值是否已匹配任一分支；否则逐个克隆并转换，取第一个通过**联合**
/// 校验的候选（与 TypeBox 1.3.27 的实现一致），都不行则原样返回。
fn convert_union(schema: &Value, value: Value) -> Value {
    let Some(arms) = get(schema, "anyOf").and_then(Value::as_array) else {
        return value;
    };
    convert_union_arms(schema, arms, value)
}

/// `convert_union` 的共用实现；`check_schema` 是用于 `Check` 的联合 schema（`Enum` 用
/// 合成的 `anyOf` 字面量联合）。
fn convert_union_arms(check_schema: &Value, arms: &[Value], value: Value) -> Value {
    if arms.iter().any(|arm| check_json_schema(arm, &value)) {
        return value;
    }
    let candidates: Vec<Value> = arms
        .iter()
        .map(|arm| convert_from_type(arm, value.clone()))
        .collect();
    for candidate in candidates {
        if check_json_schema(check_schema, &candidate) {
            return candidate;
        }
    }
    value
}

/// `FromIntersect`：TypeBox 先 `Instantiate`+`Evaluate` 再转换；Rust 侧按 `allOf` 顺序
/// 逐个子 schema 转换（对常见的不相交属性并集与原实现等价）。
fn convert_intersect(schema: &Value, mut value: Value) -> Value {
    if let Some(arms) = get(schema, "allOf").and_then(Value::as_array) {
        for arm in arms {
            value = convert_from_type(arm, value);
        }
    }
    value
}

/// `FromArray`：非数组先包成单元素数组（`TryArray`），再对每个元素递归转换。
fn convert_array(schema: &Value, value: Value) -> Value {
    let items_schema = get(schema, "items");
    let items = match value {
        Value::Array(items) => items,
        other => vec![other],
    };
    Value::Array(
        items
            .into_iter()
            .map(|item| match items_schema {
                Some(items_schema) => convert_from_type(items_schema, item),
                None => item,
            })
            .collect(),
    )
}

/// `FromTuple`：只转换下标小于 `items.len()` 的元素。
fn convert_tuple(schema: &Value, value: Value) -> Value {
    let Value::Array(mut items) = value else {
        return value;
    };
    if let Some(schemas) = get(schema, "items").and_then(Value::as_array) {
        for (item, item_schema) in items.iter_mut().zip(schemas) {
            *item = convert_from_type(item_schema, std::mem::take(item));
        }
    }
    Value::Array(items)
}

/// `properties` / `patternProperties` 的 `Guard.EntriesRegExp`：按插入顺序生成
/// `[new RegExp("^" + key + "$"), value]`（`properties` 的键不转义正则元字符；TypeBox 行为）。
fn property_entries<'a>(schema: &'a Value, keyword: &str) -> Vec<(Option<JsRegex>, &'a Value)> {
    get(schema, keyword)
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(key, value)| (compile_key_matcher(key), value))
                .collect()
        })
        .unwrap_or_default()
}

fn matcher_matches(matcher: &Option<JsRegex>, key: &str) -> bool {
    matcher
        .as_ref()
        .is_some_and(|regex| regex.find(key).is_some())
}

/// `FromProperties`/`FromPatternProperties` 的赋值循环：`value[key] = FromType(schema, value[key])`
/// （对原对象的写入，因此根级调用也会保留）。
fn apply_entries(
    map: &mut Map<String, Value>,
    keys: &[String],
    entries: &[(Option<JsRegex>, &Value)],
) {
    for (matcher, property_schema) in entries {
        for key in keys {
            if !matcher_matches(matcher, key) {
                continue;
            }
            if let Some(current) = map.get_mut(key) {
                let taken = std::mem::take(current);
                *current = convert_from_type(property_schema, taken);
            }
        }
    }
}

/// `FromAdditionalProperties`：TypeBox 1.3.27 对**每个** entry 各跑一遍，把**不匹配该 entry**
/// 的键都按 `additionalProperties` 转换。多 entry 时，某个已声明的键可能在其他 entry 的循环里
/// 被再次转换（甚至被转成 additionalProperties 的类型）；逐字复刻，包括这个反直觉行为。
fn apply_additional_properties(
    map: &mut Map<String, Value>,
    keys: &[String],
    entries: &[(Option<JsRegex>, &Value)],
    schema: &Value,
) {
    let Some(additional) = get(schema, "additionalProperties") else {
        return;
    };
    if !additional.is_object() {
        return;
    }
    for (matcher, _) in entries {
        for key in keys {
            if matcher_matches(matcher, key) {
                continue;
            }
            if let Some(current) = map.get_mut(key) {
                let taken = std::mem::take(current);
                *current = convert_from_type(additional, taken);
            }
        }
    }
}

/// `FromObject`：`properties` 按 `^key$` 正则匹配（`EntriesRegExp`），随后无条件执行
/// `FromAdditionalProperties`。
fn convert_object_schema(schema: &Value, value: Value) -> Value {
    let Value::Object(mut map) = value else {
        return value;
    };
    let entries = property_entries(schema, "properties");
    let keys: Vec<String> = map.keys().cloned().collect();
    apply_entries(&mut map, &keys, &entries);
    apply_additional_properties(&mut map, &keys, &entries, schema);
    Value::Object(map)
}

/// `FromRecord`：`patternProperties` 的键本身就是正则；`additionalProperties` 的循环与
/// `FromObject` 相同（entry 来自 `patternProperties`）。
fn convert_record_schema(schema: &Value, value: Value) -> Value {
    let Value::Object(mut map) = value else {
        return value;
    };
    let entries = property_entries(schema, "patternProperties");
    let keys: Vec<String> = map.keys().cloned().collect();
    apply_entries(&mut map, &keys, &entries);
    apply_additional_properties(&mut map, &keys, &entries, schema);
    Value::Object(map)
}

/// TypeBox `EntriesRegExp`：`new RegExp("^" + key + "$")`
/// （**没有** `u` 标志；属性名里的正则元字符不转义）。
fn compile_key_matcher(source: &str) -> Option<JsRegex> {
    JsRegex::new(&format!("^{source}$")).ok()
}

/// `TryNumber`：`Number.isFinite(+value)` 优先，其次大小写不敏感的 `"true"/"false"`，
/// 最后是 `TryBigInt` 的 `123n`/`1.5`/`123` 三种字符串形态（仅在 `+value` 非有限时可达）。
fn try_number(value: Value) -> Value {
    match value {
        Value::Null => Value::from(0),
        Value::Bool(flag) => Value::from(if flag { 1 } else { 0 }),
        Value::Number(_) => value,
        Value::String(text) => {
            let parsed = kiana_json::js_num::parse_js_number(&text);
            if parsed.is_finite() {
                return number_value(parsed);
            }
            let lowercase = text.to_lowercase();
            if lowercase == "false" {
                return Value::from(0);
            }
            if lowercase == "true" {
                return Value::from(1);
            }
            if let Some(integer) = string_to_safe_integer(&text) {
                return number_value(integer);
            }
            Value::String(text)
        }
        other => other,
    }
}

/// `TryBigInt` 的字符串分支：`/^-?(0|[1-9]\d*)n$/`、`/^-?(0|[1-9]\d*)\.\d+$/`、
/// `/^-?(0|[1-9]\d*)$/`，超出 `Number.MAX_SAFE_INTEGER` 时失败。
fn string_to_safe_integer(text: &str) -> Option<f64> {
    if let Some(body) = text.strip_suffix('n')
        && is_integer_literal(body)
    {
        return safe_integer(body);
    }
    if let Some((integer, fraction)) = text.split_once('.')
        && !fraction.is_empty()
        && fraction.bytes().all(|byte| byte.is_ascii_digit())
        && is_integer_literal(integer)
    {
        return safe_integer(integer);
    }
    if is_integer_literal(text) {
        return safe_integer(text);
    }
    None
}

fn is_integer_literal(text: &str) -> bool {
    let body = text.strip_prefix('-').unwrap_or(text);
    if body == "0" {
        return true;
    }
    let mut bytes = body.bytes();
    matches!(bytes.next(), Some(b'1'..=b'9')) && bytes.all(|byte| byte.is_ascii_digit())
}

fn safe_integer(text: &str) -> Option<f64> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(digits) => (true, digits),
        None => (false, text),
    };
    if digits.len() > 16 {
        return None;
    }
    let magnitude: i64 = digits.parse().ok()?;
    const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
    if magnitude > MAX_SAFE_INTEGER {
        return None;
    }
    Some(if negative {
        -(magnitude as f64)
    } else {
        magnitude as f64
    })
}

/// `TryNumber` + `Math.trunc`（`FromInteger`）；不可转换时原样返回。
fn try_integer(value: Value) -> Value {
    let number = match &value {
        Value::Number(_) => value.clone(),
        _ => try_number(value.clone()),
    };
    match number {
        Value::Number(numeric) => match numeric.as_f64() {
            Some(number) => number_value(number.trunc()),
            None => value,
        },
        _ => value,
    }
}

/// `TryBoolean`：`0/1` 数字、`null -> false`、大小写不敏感的 `"true"/"false"`，以及精确的
/// `"0"/"1"` 字符串。
fn try_boolean(value: Value) -> Value {
    match value {
        Value::Bool(_) => value,
        Value::Number(numeric) => {
            let number = numeric.as_f64();
            if number == Some(0.0) {
                Value::Bool(false)
            } else if number == Some(1.0) {
                Value::Bool(true)
            } else {
                Value::Number(numeric)
            }
        }
        Value::Null => Value::Bool(false),
        Value::String(text) => {
            let lowercase = text.to_lowercase();
            if lowercase == "false" {
                Value::Bool(false)
            } else if lowercase == "true" {
                Value::Bool(true)
            } else if text == "0" {
                Value::Bool(false)
            } else if text == "1" {
                Value::Bool(true)
            } else {
                Value::String(text)
            }
        }
        other => other,
    }
}

/// `TryString`：布尔/数字/null 转成 JS 的 `String(value)`。
fn try_string(value: Value) -> Value {
    match value {
        Value::Bool(flag) => Value::String(if flag {
            "true".to_string()
        } else {
            "false".to_string()
        }),
        Value::Number(numeric) => Value::String(kiana_json::js_num::format_js_number(
            numeric.as_f64().unwrap_or(f64::NAN),
        )),
        Value::Null => Value::String("null".to_string()),
        Value::String(_) => value,
        other => other,
    }
}

/// `TryNull`：`0`、`false`、`""`、`"0"`、`"null"`、`"undefined"` -> `null`。
fn try_null(value: Value) -> Value {
    match value {
        Value::Null => value,
        Value::Number(numeric) if numeric.as_f64() == Some(0.0) => Value::Null,
        Value::Bool(false) => Value::Null,
        Value::String(text) => {
            let lowercase = text.to_lowercase();
            if lowercase == "undefined" || lowercase == "null" || text.is_empty() || text == "0" {
                Value::Null
            } else {
                Value::String(text)
            }
        }
        other => other,
    }
}

/// `Value.Convert(schema, value)`（原地）。
///
/// TypeBox 的 `FromType` 按 `~kind` 分派；`~kind` 是**不可枚举**属性，`JSON.stringify` 会丢弃，
/// 所以 Rust 侧用显式 `"~kind"` 键表示活 TypeBox 节点。没有 `~kind` 的普通 JSON schema
/// （例如 MCP 服务器给的 `inputSchema`）与 TS 里 `JSON.parse` 得到的一样：整个转换是空操作。
///
/// TS 的调用点 `Value.Convert(tool.parameters, args)` **丢弃返回值**，只有原地写入会保留，
/// 因此这里只应用原地效果（见 `apply_root_convert`）：根级 Object/Record 的属性赋值与
/// Tuple 的元素赋值保留；根级 Array 的 `map` 结果被丢弃（只有元素内部的原地修改保留）；
/// 联合/枚举/字面量/原始类型的新值被丢弃。
pub fn value_convert(schema: &Value, value: &mut Value) {
    apply_root_convert(schema, value);
}

/// `Value.Convert` 根级调用的可观察效果（返回值被丢弃，只保留原地修改）。
fn apply_root_convert(schema: &Value, value: &mut Value) {
    match schema_convert_kind(schema) {
        // `FromObject`/`FromRecord`/`FromTuple` 直接写入传入的对象/数组，返回值就是同一个引用。
        ConvertKind::Object | ConvertKind::Record | ConvertKind::Tuple => {
            let current = std::mem::take(value);
            *value = convert_from_type(schema, current);
        }
        // `FromArray`：`TryArray(value).map(...)` 生成**新数组**（返回值被丢弃），元素赋值不保留；
        // 只有元素内部的原地修改保留。非数组被包成 `[value]` 后同理。
        ConvertKind::Array => {
            let Some(items_schema) = get(schema, "items") else {
                return;
            };
            match value {
                Value::Array(items) => {
                    for item in items {
                        apply_root_convert(items_schema, item);
                    }
                }
                other => apply_root_convert(items_schema, other),
            }
        }
        // `FromIntersect`：`FromType(Evaluate(...), value)`；按 `allOf` 顺序应用各分支的原地效果。
        ConvertKind::Intersect => {
            if let Some(arms) = get(schema, "allOf").and_then(Value::as_array) {
                for arm in arms {
                    apply_root_convert(arm, value);
                }
            }
        }
        // `Union` 只在克隆上转换候选；`Enum`/`Literal`/原始类型返回新值；`Ref` 在空 context 下
        // 不展开；`Cyclic`/`TemplateLiteral` 等同上。全部丢弃后无效果。
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// `validateToolCall` / `validateToolArguments`
// ---------------------------------------------------------------------------

/// `validateToolCall(tools, toolCall)`：按名字线性查找工具。
///
/// 返回强制转换后的参数值（工具参数通常是对象）；工具不存在时返回
/// `AiErrorKind::Request` 的 `Tool "<name>" not found`，参数不合法时错误由
/// [`validate_tool_arguments`] 生成。
// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定，签名保持不变。
#[allow(clippy::result_large_err)]
pub fn validate_tool_call(tools: &[Tool], tool_call: &ToolCall) -> Result<JsonValue, AiError> {
    let Some(tool) = tools.iter().find(|tool| tool.name == tool_call.name) else {
        return Err(AiError::new(
            AiErrorKind::Request,
            format!("Tool \"{}\" not found", tool_call.name),
        ));
    };
    validate_tool_arguments(tool, tool_call)
}

/// `validateToolArguments(tool, toolCall)`：返回强制转换后的参数。
///
/// 处理步骤的顺序有依赖：`argumentsError` 短路 → `structuredClone` → `normalizeOptionalNulls` →
/// `Value.Convert` → 普通 JSON Schema 的 `coerceWithJsonSchema` → `Check` → 失败时按
/// `  - <path>: <message>` 列表抛出。
///
/// 成功时返回**强制转换后**的独立副本，不修改 `tool_call`。错误一律是
/// `AiErrorKind::Request`：`arguments_error` 原样拼接；非法正则是
/// `Invalid regular expression: ...`；校验失败是文件开头的多行模板（错误列表为空时用
/// `Unknown validation error` 兜底）。
// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定，签名保持不变。
#[allow(clippy::result_large_err)]
pub fn validate_tool_arguments(tool: &Tool, tool_call: &ToolCall) -> Result<JsonValue, AiError> {
    if let Some(arguments_error) = &tool_call.arguments_error {
        return Err(AiError::new(
            AiErrorKind::Request,
            format!(
                "Invalid arguments for tool \"{}\": {arguments_error}",
                tool_call.name
            ),
        ));
    }
    let parameters = &tool.parameters;
    // 对应 TS 的 `structuredClone(toolCall.arguments)`：复制一份，后续的原地转换不会影响
    // 调用方持有的 `tool_call`。
    let mut args = Value::Object(
        tool_call
            .arguments
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );
    normalize_optional_nulls(&mut args, parameters);
    value_convert(parameters, &mut args);
    // TS 的 `getValidator(parameters)` 在这里 `Compile`（非法 `pattern` 抛 `SyntaxError`）。
    if let Err(message) = validate_schema_patterns(parameters) {
        return Err(AiError::new(AiErrorKind::Request, message));
    }

    // 非 TypeBox schema（JSON 里永远没有 `Symbol.for("TypeBox.Kind")`）：再做一次
    // `coerceWithJsonSchema`。返回的对象与 `args` 身份不同时，TS 会清空 `args` 再 `assign`，
    // 效果等同于直接用转换结果。
    // TS 用 `coerced !== args` 判身份；JSON 值没有可用的对象身份，这里用深比较近似：深相等
    // 时后续的“清空再 assign”不会改变内容，跳过该分支不影响结果。
    let coerced = coerce_with_json_schema(args.clone(), parameters);
    if !kiana_json::js_object::deep_equal(&coerced, &args) {
        if args.is_object() && coerced.is_object() {
            args = coerced;
        } else if check_json_schema(parameters, &coerced) {
            return Ok(coerced);
        }
    }

    if check_json_schema(parameters, &args) {
        return Ok(args);
    }

    let errors = collect_json_schema_errors(parameters, &args)
        .iter()
        .map(|error| format!("  - {}: {}", format_validation_path(error), error.message))
        .collect::<Vec<_>>()
        .join("\n");
    let errors = if errors.is_empty() {
        "Unknown validation error".to_string()
    } else {
        errors
    };
    let received = Value::Object(
        tool_call
            .arguments
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );
    Err(AiError::new(
        AiErrorKind::Request,
        format!(
            "Validation failed for tool \"{}\":\n{}\n\nReceived arguments:\n{}",
            tool_call.name,
            errors,
            kiana_json::js_object::stringify_pretty(&received, 2)
        ),
    ))
}
