//! `schema.ts`：`canonicalJson`、`hash64`、`deepEqual`、TypeBox 兼容的
//! `Value.Check/Errors/Clean/Default/Clone`、`assertValid`、`normalize`、`canonicalIdentity`、`fits`。
//!
//! TS 的 schema 是 TypeBox 对象（`Type.Object(...)` 等），运行时 `Value.Default`/`Value.Clean`
//! 按 **`~kind` 标记**分派。`~kind` 是**不可枚举**的自有属性，因此经 JSON 传输后消失：普通
//! JSON schema 上 `Default`/`Clean` 是近空操作（只有顶层 `default` 仍生效）。Rust 侧 schema 用
//! [`serde_json::Value`] 承载（与 TS 的 `JSON.stringify(schema)` 同形），没有属性可枚举性，因此
//! 用普通键 [`KIND_KEY`] 表示标记：`remote()`/`local()` 收到的声明 schema 经
//! [`mark_live_schema`] 标记（等价于 `Type.Object(...)` 构造出的 TypeBox 对象），所有序列化路径
//! 经 [`strip_live_markers`] 剥除；直接来自 JSON 的 schema 没有标记，`Default`/`Clean` 因而
//! 是近空操作。`check` 不依赖标记（TypeBox 的 `Check` 按关键字分派）。
//!
//! 逐字复刻的行为（用户可见）：
//! - `assertValid` 的消息：`${what}: ` + 前 3 个错误，每条 `${instancePath || "/"} ${message}`，
//!   用 `"; "` 连接；`details` 是这些错误对象（`keyword`/`schemaPath`/`instancePath`/`params`/`message`）。
//! - `normalize` = `Clean(Default(Clone(value ?? {})))`，然后 `assertValid("input", …)`。
//! - `hash64`：两路 FNV-1a，按 **UTF-16 code unit** 迭代，`Math.imul` = `u32::wrapping_mul`。
//! - `canonicalJson`：键排序（UTF-16 码元序）、丢弃值为 `undefined` 的键（JSON 里不存在）、
//!   数字走 `kiana_json::format_js_number`。
//!
//! 已覆盖：`$ref`/`$recursiveRef`/`$dynamicRef`（`$id`/`$anchor`/`$dynamicAnchor`/JSON Pointer，
//! 解析不到即 `false` schema）、`unevaluatedItems`/`unevaluatedProperties`（含
//! `allOf`/`anyOf`/`oneOf`/`if`/`not` 分支的求值上下文合并）、`allOf` 的 Default/Clean
//! （`Instantiate` + `Evaluate`：Distribute/Narrow/Composite/Broaden/EvaluateUnion）、
//! `Cyclic`/`Ref` 的 Default/Clean（上下文 = `$defs`）。
//!
//! 仍不可还原（纯 JSON 里没有 `~kind`，见 [`mark_live_schema`]）：`~refine`、`Type.Any`/
//! `Type.Unknown`/`Type.Never`/`Type.Enum`/`Type.Dependent`/`Type.TemplateLiteral` 的
//! 构造器语义，以及 `readonly`/`optional` 修饰符（符号属性）。
//!
//! 另一条不可消除的近似：JSON 序列化后，`Type.Object(...)` 构造的活节点与 `Type.Unsafe`/
//! 手写字面量（含 `JSON.parse` 得到的）子 schema 字节相同，[`mark_live_schema`] 无法区分，
//! 因此活 schema 中嵌入的字面量子 schema 也被当作活节点参与 Default/Clean；TS 的
//! `Value.Default`/`Value.Clean` 只按真实 `~kind` 分派，对这类子 schema 保持原值。这是 JSON
//! 丢失不可枚举 `~kind` 的必然结果（直接来自 JSON、未经 `mark_live_schema` 的 schema 用
//! `live=false`，不参与活节点处理）。
//!
//! # 设计与执行流程
//!
//! 本文件是纯函数库：调用方传入 `serde_json::Value` 形式的 schema 与值，函数返回校验
//! 结果、规范化值或 `ChordError`。唯一的共享状态是 `OnceLock` 缓存的内置正则
//! （首次使用时编译，之后复用）。crate 内由 `wire.rs`（帧校验）与 `contract/`（契约声明
//! 准备、fingerprint、coverage）使用；crate 内只依赖 `crate::errors`，此外只用
//! `serde_json`/`kiana_json`/`regress`/`url`/`unicode-normalization` 等基础库，不反向
//! 依赖 host/连接层。
//!
//! 一次校验/规范化的主路径：
//!
//! 1. [`check`] / [`errors`] 建立内部校验上下文 `Walk`（根 schema 供 `$ref` 解析），调用
//!    `validate_schema`。Check 模式在首个失败处短路；Error 模式继续评估其余关键字，最多
//!    收集 8 条错误。
//! 2. 关键字按固定顺序评估：`type` → 对象关键字 → 数组关键字 → 字符串关键字 → 数字关键字
//!    → `$ref`/`$recursiveRef`/`$dynamicRef` → `const`/`enum` → `if`/`then`/`else` →
//!    `not` → `allOf`/`anyOf`/`oneOf` → `unevaluatedItems`/`unevaluatedProperties`。
//!    顺序有意义：`unevaluated*` 只看到前面分支写入的"已求值"键/下标集合。
//! 3. [`normalize`] 依次执行 [`clone_value`] → `default_value` → `clean_value` →
//!    [`assert_valid`]；Default/Clean 只在 schema 带 [`KIND_KEY`] 标记时递归。
//!
//! # 边界与失败模式
//!
//! - `schema` 为 `false` 时任何值都不通过（错误关键字 `boolean`）；为 `true` 或 `{}` 时通过。
//! - `$ref` 解析不到按 `false` schema 处理，不会单独报"引用错误"。
//! - 非法 `pattern`/`patternProperties`：TS 会抛未捕获的 `SyntaxError`；本文件不 panic——
//!   [`check`] 视为不匹配；[`normalize`]/[`assert_valid`] 只在 [`first_invalid_regex`]
//!   扫描到时才转成错误码 `unavailable`。注意 [`first_invalid_regex`] 只递归**对象**位置，
//!   不进入数组：位于 `anyOf`/`allOf`/`oneOf`/`prefixItems`/tuple `items` 等数组里的非法
//!   pattern 会被漏检，表现为普通不匹配——可能因其他 `anyOf` 分支通过而整体成功，否则报
//!   `invalid`。
//! - 校验失败经 [`assert_valid`] 变成错误码 `invalid`，`details` 是前 3 条错误对象。
//! - 没有 `~kind` 标记的 schema 上 `default_value`/`clean_value` 几乎是空操作（只保留顶层
//!   `default`）；这是"schema 经 JSON 传输"的常态，不是失败。
//!
//! # 阅读顺序
//!
//! 1. `canonical_json`/`hash64`/`deep_equal`（值与散列）；
//! 2. `TbError` 与本地化文案；
//! 3. `validate_schema`（校验引擎，最长的一段）；
//! 4. `$ref` 解析与 `format`/IDNA；
//! 5. `default_from_ctx`/`clean_from_ctx` 与 `Evaluate` 系列；
//! 6. `fits` 与 `object_fits`/`array_fits`/`function_fits`；
//! 7. 末尾 `mod tests` 的关键回归（锁定 `~kind` 分派、非法正则、`additionalItems` 路径等）。

use std::sync::OnceLock;

use serde_json::{Map, Value};
use unicode_normalization::UnicodeNormalization;

use crate::errors::{ChordError, ChordErrorCode, invalid};

// ---------------------------------------------------------------------------
// JS 值助手
// ---------------------------------------------------------------------------

/// TS 的 `asSchema`：对象原样；`false` → `{ not: {} }`；其余 → `{}`。
fn as_schema(value: &Value) -> Value {
    match value {
        Value::Object(_) => value.clone(),
        Value::Bool(false) => {
            let mut map = Map::new();
            map.insert("not".to_string(), Value::Object(Map::new()));
            Value::Object(map)
        }
        _ => Value::Object(Map::new()),
    }
}

/// `Object.keys` 的 JS 顺序：数组索引形式的键按数值升序在前，其余按插入序。
fn js_keys(map: &Map<String, Value>) -> Vec<&String> {
    let mut indices: Vec<(u64, &String)> = Vec::new();
    let mut others: Vec<&String> = Vec::new();
    for key in map.keys() {
        match array_index(key) {
            Some(index) => indices.push((index, key)),
            None => others.push(key),
        }
    }
    indices.sort_by_key(|(index, _)| *index);
    let mut out: Vec<&String> = Vec::with_capacity(map.len());
    out.extend(indices.into_iter().map(|(_, key)| key));
    out.extend(others);
    out
}

/// 规范数组索引（`"0"`、`"1"`、…，不含 `"-0"`、`"01"`、`"4294967295"`）。
fn array_index(key: &str) -> Option<u64> {
    if key.is_empty() || key.len() > 10 {
        return None;
    }
    if key == "0" {
        return Some(0);
    }
    if key.starts_with('0') || !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value: u64 = key.parse().ok()?;
    if value < (u32::MAX as u64) {
        Some(value)
    } else {
        None
    }
}

/// `Object.prototype.hasOwnProperty` 语义（`__proto__` 等也按普通键）。
fn has(map: &Map<String, Value>, key: &str) -> bool {
    map.contains_key(key)
}

// ---------------------------------------------------------------------------
// 数字
// ---------------------------------------------------------------------------

/// JS `Number.isInteger`。
fn is_js_integer(value: &Value) -> bool {
    match value {
        Value::Number(n) => n.as_f64().is_some_and(kiana_json::is_js_integer),
        _ => false,
    }
}

/// JS `typeof value === "number" && Number.isFinite(value)`。
fn is_js_number(value: &Value) -> bool {
    matches!(value, Value::Number(n) if n.as_f64().is_some_and(f64::is_finite))
}
// ---------------------------------------------------------------------------
// canonicalJson / hash64 / deepEqual
// ---------------------------------------------------------------------------

/// 规范 JSON：对象键排序（UTF-16 码元序），值为 `undefined` 的键丢弃（JSON 值里不存在）。
///
/// 输出是确定性的：结构相同（对象键序无关）的两个 JSON 值得到同一字符串，因此
/// [`deep_equal`] 直接比较它的输出。对象键先按 JS 自有键序分出数组索引键（升序在前），
/// 再按 UTF-16 码元排序；数组保持原顺序；数字用 `kiana_json::format_js_number`、字符串用
/// `kiana_json::js_quote`（与 `JSON.stringify` 的转义一致）。
///
/// 注意：`serde_json::Value` 没有 `undefined`，代码里并没有实际的键过滤；"丢弃
/// undefined 键"描述的是 TS 行为（JSON 值中不可能出现 undefined）。
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

/// [`canonical_json`] 的递归写出：对象先取 JS 键序，再按 UTF-16 码元排序；数组保持顺序。
fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => out.push_str(&kiana_json::format_js_number(
            number.as_f64().unwrap_or(f64::NAN),
        )),
        Value::String(text) => out.push_str(&kiana_json::js_quote(text)),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                // 数组里的 undefined 变成 null；serde_json 没有 undefined。
                write_canonical(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys = js_keys(map);
            // `Array.prototype.sort()` 默认按 UTF-16 码元比较。
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&kiana_json::js_quote(key));
                out.push(':');
                // 值为 undefined 的键被过滤；JSON 值里没有 undefined。
                write_canonical(&map[key.as_str()], out);
            }
            out.push('}');
        }
    }
}

/// 非加密 64 位散列（两路 FNV-1a），16 个小写十六进制字符。
///
/// 按 **UTF-16 码元**迭代（不是 UTF-8 字节），两路分别用 `0x01000193` 与 `0x5bd1e995`
/// 作乘子，第二路每步再混入一次 `b >> 13`，最后拼成 `{a:08x}{b:08x}`。它只用于标识/
/// 指纹（调用方 `contract/describe.rs` 用它给契约 description 算指纹），不是安全散列，
/// 不能用于安全用途。
pub fn hash64(text: &str) -> String {
    let mut a: u32 = 0x811c_9dc5;
    let mut b: u32 = 0x0100_0193 ^ 0x5bd1_e995;
    for unit in text.encode_utf16() {
        let code = u32::from(unit);
        a = (a ^ code).wrapping_mul(0x0100_0193);
        b = (b ^ code).wrapping_mul(0x5bd1_e995) ^ (b >> 13);
    }
    format!("{a:08x}{b:08x}")
}

/// `deepEqual(a, b) = a === b || canonicalJson(a) === canonicalJson(b)`。
///
/// 与 `is_deep_equal`（TypeBox `Guard.IsDeepEqual`，用于 `const`/`enum` 校验）的区别：
/// 本函数比较规范化后的文本，`is_deep_equal` 逐结构递归；两者都不受对象键序影响。
pub fn deep_equal(a: &Value, b: &Value) -> bool {
    canonical_json(a) == canonical_json(b)
}

/// TypeBox `Guard.IsDeepEqual`：结构相等、键序无关、数组按长度与逐项。
///
/// 数值统一转成 `f64` 比较（与 JS 的 `===` 一致，`1` 与 `1.0` 相等）；对象要求键数相同
/// 且每个键在另一侧存在并递归相等。
pub(crate) fn is_deep_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Object(l), Value::Object(r)) => {
            l.len() == r.len()
                && l.iter()
                    .all(|(key, value)| r.get(key).is_some_and(|other| is_deep_equal(value, other)))
        }
        (Value::Array(l), Value::Array(r)) => {
            l.len() == r.len() && l.iter().zip(r.iter()).all(|(a, b)| is_deep_equal(a, b))
        }
        (Value::Number(a), Value::Number(b)) => match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        },
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// 关键字守卫（`schema/types/*` 的 JSON 等价物）
// ---------------------------------------------------------------------------

/// `IsSchema`：对象或 boolean。
fn is_schema(value: &Value) -> bool {
    matches!(value, Value::Object(_) | Value::Bool(_))
}

fn is_schema_array(value: &Value) -> bool {
    matches!(value, Value::Array(items) if items.iter().all(is_schema))
}

/// `IsType`：`type` 是字符串或字符串数组。
fn is_type(schema: &Map<String, Value>) -> bool {
    match schema.get("type") {
        Some(Value::String(_)) => true,
        Some(Value::Array(items)) => items.iter().all(|item| item.is_string()),
        _ => false,
    }
}

/// `type` 的字符串视图（`IsType` 已验证）。
fn type_names(schema: &Map<String, Value>) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(text)) => vec![text.as_str()],
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn is_properties(schema: &Map<String, Value>) -> bool {
    match schema.get("properties") {
        Some(Value::Object(map)) => map.values().all(is_schema),
        _ => false,
    }
}

fn is_required(schema: &Map<String, Value>) -> bool {
    match schema.get("required") {
        Some(Value::Array(items)) => items.iter().all(Value::is_string),
        _ => false,
    }
}

fn is_additional_properties(schema: &Map<String, Value>) -> bool {
    schema.get("additionalProperties").is_some_and(is_schema)
}

fn is_items(schema: &Map<String, Value>) -> bool {
    match schema.get("items") {
        Some(value) if is_schema(value) => true,
        Some(Value::Array(items)) => items.iter().all(is_schema),
        _ => false,
    }
}

fn is_pattern_properties(schema: &Map<String, Value>) -> bool {
    match schema.get("patternProperties") {
        Some(Value::Object(map)) => map.values().all(is_schema),
        _ => false,
    }
}

/// `IsAdditionalItems`。
fn is_additional_items(schema: &Map<String, Value>) -> bool {
    schema.get("additionalItems").is_some_and(is_schema)
}

fn is_prefix_items(schema: &Map<String, Value>) -> bool {
    schema.get("prefixItems").is_some_and(is_schema_array)
}

fn is_number_keyword(schema: &Map<String, Value>, key: &str) -> bool {
    schema.get(key).is_some_and(is_js_number)
}

fn is_boolean_keyword(schema: &Map<String, Value>, key: &str) -> bool {
    matches!(schema.get(key), Some(Value::Bool(_)))
}

fn is_schema_keyword(schema: &Map<String, Value>, key: &str) -> bool {
    schema.get(key).is_some_and(is_schema)
}

fn is_schema_array_keyword(schema: &Map<String, Value>, key: &str) -> bool {
    schema.get(key).is_some_and(is_schema_array)
}

fn is_string_keyword(schema: &Map<String, Value>, key: &str) -> bool {
    matches!(schema.get(key), Some(Value::String(_)))
}

/// `HasPropertyKey`（`const` 等只需存在性）。
fn has_property_key(schema: &Map<String, Value>, key: &str) -> bool {
    has(schema, key)
}

// ---------------------------------------------------------------------------
// 值测试（TypeBox `Guard`）
// ---------------------------------------------------------------------------

fn is_js_string(value: &Value) -> bool {
    value.is_string()
}

fn is_js_boolean(value: &Value) -> bool {
    value.is_boolean()
}
fn is_js_object_not_array(value: &Value) -> bool {
    matches!(value, Value::Object(_))
}

fn is_js_array(value: &Value) -> bool {
    value.is_array()
}

// ---------------------------------------------------------------------------
// 错误对象与本地化（`system/locale/en_US.mjs`，逐字）
// ---------------------------------------------------------------------------

/// TypeBox 错误对象（`{ keyword, schemaPath, instancePath, params }`）。
///
/// 这是未本地化的内部形态：[`TbError::to_json`] 会补上 `message` 并转成公开错误数组的
/// 元素，[`TbError::message`] 单独给出 en_US 文案。`schema_path`/`instance_path` 都是
/// JSON Pointer 形式的字符串（schema 路径以 `#` 开头）；顶层 schema 路径由
/// [`check`]/[`errors`] 初始化为 `"#"`，顶层实例路径为空串。
#[derive(Debug, Clone, PartialEq)]
pub struct TbError {
    /// 失败的关键字（`type`/`required`/`pattern`/`anyOf` …），决定 [`TbError::message`]
    /// 走哪条文案分支。
    pub keyword: String,
    /// 出错 schema 的位置（如 `"#/properties/a"`、`"#/items/0"`）。
    pub schema_path: String,
    /// 出错实例值的位置（如 `"/a"`、`"/1"`）；顶层为空串。
    pub instance_path: String,
    /// 关键字的参数，本地化文案从这里取数。来源分两类：
    /// - 多数直接取自 schema 关键字：`limit`（`min/maxItems` 等的值）、`type`、`pattern`、
    ///   `format`、`multipleOf`、`minContains`（schema 里是数字时）等；
    /// - 其余为校验过程中派生的值或字面量：`requiredProperties` = 实例中**缺失**的 required
    ///   键列表、`additionalProperties` = 多余键列表、`propertyNames` = 非法键列表、
    ///   `comparison` = 比较运算符字面量、`failingKeyword` = `"then"`/`"else"`、
    ///   `passingSchemas` = oneOf 命中分支的下标、`duplicateItems` = uniqueItems 违规的
    ///   重复项**下标**、`unevaluatedItems`/`unevaluatedProperties` = 未被评估的项下标/
    ///   属性名；`minContains` 在 schema 未给数字时缺省为 1。
    pub params: Value,
}

impl TbError {
    /// 本地化后的错误对象：`{ keyword, schemaPath, instancePath, params, message }`。
    ///
    /// 按 `keyword`→`schemaPath`→`instancePath`→`params`→`message` 的顺序写入对象；
    /// `message` 来自 [`TbError::message`]。
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("keyword".to_string(), Value::String(self.keyword.clone()));
        map.insert(
            "schemaPath".to_string(),
            Value::String(self.schema_path.clone()),
        );
        map.insert(
            "instancePath".to_string(),
            Value::String(self.instance_path.clone()),
        );
        map.insert("params".to_string(), self.params.clone());
        map.insert("message".to_string(), Value::String(self.message()));
        Value::Object(map)
    }

    /// en_US 本地化消息（`system/locale/en_US.mjs` 逐字）。
    ///
    /// 未知 `keyword` 回退为 `an unknown validation error occurred`；`params` 里缺少某个
    /// 键时按 `null` 参与格式化。
    pub fn message(&self) -> String {
        let param = |key: &str| self.params.get(key).cloned().unwrap_or(Value::Null);
        let join_strings = |value: &Value| -> String {
            match value {
                Value::Array(items) => items
                    .iter()
                    .map(|item| match item {
                        Value::String(text) => text.clone(),
                        other => canonical_json(other),
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
                other => canonical_json(other),
            }
        };
        let number_text = |value: &Value| -> String {
            match value {
                Value::Number(n) => kiana_json::format_js_number(n.as_f64().unwrap_or(f64::NAN)),
                other => canonical_json(other),
            }
        };
        match self.keyword.as_str() {
            "additionalProperties" => "must not have additional properties".to_string(),
            "anyOf" => "must match a schema in anyOf".to_string(),
            "boolean" => "schema is false".to_string(),
            "const" => "must be equal to constant".to_string(),
            "contains" => "must contain at least 1 valid item".to_string(),
            "dependencies" | "dependentRequired" => format!(
                "must have properties {} when property {} is present",
                join_strings(&param("dependencies")),
                match param("property") {
                    Value::String(text) => text,
                    other => canonical_json(&other),
                }
            ),
            "enum" => "must be equal to one of the allowed values".to_string(),
            "exclusiveMaximum" | "exclusiveMinimum" | "maximum" | "minimum" => format!(
                "must be {} {}",
                match param("comparison") {
                    Value::String(text) => text,
                    other => canonical_json(&other),
                },
                number_text(&param("limit"))
            ),
            "format" => format!(
                "must match format \"{}\"",
                match param("format") {
                    Value::String(text) => text,
                    other => canonical_json(&other),
                }
            ),
            "if" => format!(
                "must match \"{}\" schema",
                match param("failingKeyword") {
                    Value::String(text) => text,
                    other => canonical_json(&other),
                }
            ),
            "maxItems" | "minItems" => format!(
                "must not have {} than {} items",
                if self.keyword == "maxItems" {
                    "more"
                } else {
                    "fewer"
                },
                number_text(&param("limit"))
            ),
            "maxLength" | "minLength" => format!(
                "must not have {} than {} characters",
                if self.keyword == "maxLength" {
                    "more"
                } else {
                    "fewer"
                },
                number_text(&param("limit"))
            ),
            "maxProperties" | "minProperties" => format!(
                "must not have {} than {} properties",
                if self.keyword == "maxProperties" {
                    "more"
                } else {
                    "fewer"
                },
                number_text(&param("limit"))
            ),
            "multipleOf" => format!("must be multiple of {}", number_text(&param("multipleOf"))),
            "not" => "must not be valid".to_string(),
            "oneOf" => "must match exactly one schema in oneOf".to_string(),
            "pattern" => format!(
                "must match pattern \"{}\"",
                match param("pattern") {
                    Value::String(text) => text,
                    other => canonical_json(&other),
                }
            ),
            "propertyNames" => format!(
                "property names {} are invalid",
                join_strings(&param("propertyNames"))
            ),
            "required" => format!(
                "must have required properties {}",
                join_strings(&param("requiredProperties"))
            ),
            "type" => match param("type") {
                Value::String(text) => format!("must be {text}"),
                Value::Array(items) => format!(
                    "must be either {}",
                    items
                        .iter()
                        .map(|item| match item {
                            Value::String(text) => text.clone(),
                            other => canonical_json(other),
                        })
                        .collect::<Vec<_>>()
                        .join(" or ")
                ),
                other => format!("must be {}", canonical_json(&other)),
            },
            "unevaluatedItems" => "must not have unevaluated items".to_string(),
            "unevaluatedProperties" => "must not have unevaluated properties".to_string(),
            "uniqueItems" => "must not have duplicate items".to_string(),
            _ => "an unknown validation error occurred".to_string(),
        }
    }
}

fn params(entries: Vec<(&str, Value)>) -> Value {
    let mut map = Map::new();
    for (key, value) in entries {
        map.insert(key.to_string(), value);
    }
    Value::Object(map)
}

/// TypeBox 的 `params` 直接回填 **schema 字段本身**（`params: { limit: schema.minLength }`），
/// 不经过数值转换：`minLength: 2` 报出来的就是整数 `2`，不是 `2.0`。比较仍用 `f64`，
/// 但写进 `params` 的必须是原始 JSON 值。
fn keyword_value(schema: &Map<String, Value>, key: &str) -> Value {
    schema.get(key).cloned().unwrap_or(Value::Null)
}

/// `Settings.maxErrors`（默认 8）。
const MAX_ERRORS: usize = 8;
// ---------------------------------------------------------------------------
// 字符串长度：TypeBox 按**字素簇**计数（`guard/string.mjs`）
// ---------------------------------------------------------------------------

/// `IsCombiningMark`。
fn is_combining_mark(cp: u32) -> bool {
    (0x0300..=0x036F).contains(&cp)
        || (0x1AB0..=0x1AFF).contains(&cp)
        || (0x1DC0..=0x1DFF).contains(&cp)
        || (0xFE20..=0xFE2F).contains(&cp)
}

fn is_variation_selector(cp: u32) -> bool {
    (0xFE00..=0xFE0F).contains(&cp)
}

fn is_zero_width_joiner(cp: u32) -> bool {
    cp == 0x200D
}

fn is_regional_indicator(cp: u32) -> bool {
    (0x1F1E6..=0x1F1FF).contains(&cp)
}

fn code_point_length(cp: u32) -> usize {
    if cp > 0xFFFF { 2 } else { 1 }
}

/// 以 UTF-16 码元视图表示字符串（TypeBox 的索引都是码元索引）。
fn utf16_units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn code_point_at(units: &[u16], index: usize) -> u32 {
    let unit = units[index] as u32;
    if (0xD800..=0xDBFF).contains(&unit) && index + 1 < units.len() {
        let low = units[index + 1] as u32;
        if (0xDC00..=0xDFFF).contains(&low) {
            return 0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00);
        }
    }
    unit
}

fn consume_modifiers(units: &[u16], mut index: usize) -> usize {
    while index < units.len() {
        let point = code_point_at(units, index);
        if is_combining_mark(point) || is_variation_selector(point) {
            index += code_point_length(point);
        } else {
            break;
        }
    }
    index
}

fn next_grapheme_cluster_index(units: &[u16], cluster_start: usize) -> usize {
    let start = code_point_at(units, cluster_start);
    let mut cluster_end = cluster_start + code_point_length(start);
    cluster_end = consume_modifiers(units, cluster_end);
    while cluster_end + 1 < units.len() && is_zero_width_joiner(code_point_at(units, cluster_end)) {
        let next = code_point_at(units, cluster_end + 1);
        cluster_end += 1 + code_point_length(next);
        cluster_end = consume_modifiers(units, cluster_end);
    }
    if is_regional_indicator(start)
        && cluster_end < units.len()
        && is_regional_indicator(code_point_at(units, cluster_end))
    {
        cluster_end += code_point_length(code_point_at(units, cluster_end));
    }
    cluster_end
}

/// `GraphemeCount`。
fn grapheme_count(text: &str) -> usize {
    let units = utf16_units(text);
    let mut count = 0;
    let mut index = 0;
    while index < units.len() {
        index = next_grapheme_cluster_index(&units, index);
        count += 1;
    }
    count
}

fn is_grapheme_code_point(unit: u16) -> bool {
    let value = unit as u32;
    value >= 0x0300
        && ((0xD800..=0xDBFF).contains(&value)
            || is_combining_mark(value)
            || is_variation_selector(value)
            || is_zero_width_joiner(value))
}

/// `IsMinLength`：至少 `min` 个字素簇。
pub(crate) fn is_min_length(text: &str, min: f64) -> bool {
    if min == 0.0 {
        return true;
    }
    let units = utf16_units(text);
    if (units.len() as f64) < min {
        return false;
    }
    let mut index: usize = 0;
    loop {
        let unit = units.get(index).copied();
        if unit.is_some_and(is_grapheme_code_point) {
            // `IsMinLengthSegmented`：完整计数。
            let mut count: f64 = 0.0;
            let mut cursor = 0usize;
            while cursor < units.len() {
                cursor = next_grapheme_cluster_index(&units, cursor);
                count += 1.0;
                if count >= min {
                    return true;
                }
            }
            return false;
        }
        index += 1;
        if (index as f64) >= min {
            return true;
        }
    }
}

/// `IsMaxLength`：至多 `max` 个字素簇。
pub(crate) fn is_max_length(text: &str, max: f64) -> bool {
    let units = utf16_units(text);
    if (units.len() as f64) <= max {
        return true;
    }
    let mut index: usize = 0;
    loop {
        let unit = units.get(index).copied();
        if unit.is_some_and(is_grapheme_code_point) {
            let mut count: f64 = 0.0;
            let mut cursor = 0usize;
            while cursor < units.len() {
                cursor = next_grapheme_cluster_index(&units, cursor);
                count += 1.0;
                if count > max {
                    return false;
                }
            }
            return true;
        }
        index += 1;
        if (index as f64) > max {
            return false;
        }
    }
}

/// 公开的 `grapheme_count`（测试与其他单元校验 `minLength`/`maxLength` 文案时用）。
pub fn graphemes(text: &str) -> usize {
    grapheme_count(text)
}

// ---------------------------------------------------------------------------
// 校验引擎（`schema/engine/*` 的 JSON 等价物）
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Check,
    Error,
}

/// 校验上下文：`Check` 模式不需要错误，`Error` 模式累积（容量 8）。
struct Walk {
    mode: Mode,
    errors: Vec<TbError>,
    /// 顶层 schema：`$ref` 解析的根（TypeBox 的 `Stack.schema`）。
    root: std::sync::Arc<Value>,
    /// `CheckContext` 的已求值键栈（`unevaluatedProperties` 用）。
    eval_keys: Vec<std::collections::HashSet<String>>,
    /// `CheckContext` 的已求值数组下标栈（`unevaluatedItems` 用）。
    eval_indices: Vec<std::collections::HashSet<usize>>,
}

impl Walk {
    fn with_root(mode: Mode, root: std::sync::Arc<Value>) -> Self {
        Walk {
            mode,
            errors: Vec::new(),
            root,
            eval_keys: vec![std::collections::HashSet::new()],
            eval_indices: vec![std::collections::HashSet::new()],
        }
    }

    /// 同模式的分支上下文（TypeBox 的 `new CheckContext()` / `new ErrorContext()`）。
    fn branch(&self) -> Walk {
        Walk::with_root(self.mode, self.root.clone())
    }

    /// Check 模式的分支（`not`/`if` 条件/`contains` 等内部探针用）。
    fn check_branch(&self) -> Walk {
        Walk::with_root(Mode::Check, self.root.clone())
    }

    /// `CheckSchemaPushStack`/`ErrorSchemaPushStack` 的 `context.Push()`。
    fn push_eval(&mut self) {
        self.eval_keys.push(std::collections::HashSet::new());
        self.eval_indices.push(std::collections::HashSet::new());
    }

    /// `context.Pop()`。
    fn pop_eval(&mut self) {
        if self.eval_keys.len() > 1 {
            self.eval_keys.pop();
            self.eval_indices.pop();
        }
    }

    fn add_eval_key(&mut self, key: &str) {
        if let Some(frame) = self.eval_keys.last_mut() {
            frame.insert(key.to_string());
        }
    }

    fn add_eval_index(&mut self, index: usize) {
        if let Some(frame) = self.eval_indices.last_mut() {
            frame.insert(index);
        }
    }

    fn eval_key_seen(&self, key: &str) -> bool {
        self.eval_keys
            .last()
            .is_some_and(|frame| frame.contains(key))
    }

    fn eval_index_seen(&self, index: usize) -> bool {
        self.eval_indices
            .last()
            .is_some_and(|frame| frame.contains(&index))
    }

    fn at_capacity(&self) -> bool {
        self.mode == Mode::Error && self.errors.len() >= MAX_ERRORS
    }

    /// `context.AddError(...)`：记录并返回 false。
    fn add(&mut self, error: TbError) -> bool {
        if self.mode == Mode::Error && self.errors.len() < MAX_ERRORS {
            self.errors.push(error);
        }
        false
    }

    /// 把子上下文的错误与已求值集合并进来（`context.Merge`）。
    fn merge(&mut self, other: Walk) {
        if self.mode == Mode::Error {
            for error in other.errors {
                if self.errors.len() < MAX_ERRORS {
                    self.errors.push(error);
                }
            }
        }
        if let (Some(mine), Some(theirs)) = (self.eval_keys.last_mut(), other.eval_keys.last()) {
            mine.extend(theirs.iter().cloned());
        }
        if let (Some(mine), Some(theirs)) =
            (self.eval_indices.last_mut(), other.eval_indices.last())
        {
            mine.extend(theirs.iter().cloned());
        }
    }
}

/// `CheckSchemaPushStack`/`ErrorSchemaPushStack`：成功才 `Pop`（TS 里 `Pop` 在 `&&` 右侧，
/// 子检查失败时短路、不弹出——错误模式下后续关键字的 `AddKey` 会落在这个帧里）。
fn validate_schema_push(
    walk: &mut Walk,
    schema: &Value,
    value: &Value,
    schema_path: &str,
    instance_path: &str,
) -> bool {
    walk.push_eval();
    if validate_schema(walk, schema, value, schema_path, instance_path) {
        walk.pop_eval();
        true
    } else {
        false
    }
}

fn path_key(base: &str, key: &str) -> String {
    format!("{base}/{key}")
}

fn path_index(base: &str, index: usize) -> String {
    format!("{base}/{index}")
}

/// `UnicodeRegExp(pattern)`（TypeBox `schema/engine/_regexp.mjs`）：JS 的 `new RegExp(pattern, "u")`。
///
/// `format: "regex"` 用它做"编译成功即合法"的判定（失败返回 `None`）；`pattern`/
/// `patternProperties` 关键字用它匹配（失败时抛 `SyntaxError`，见 [`pattern_matches`]）。
fn compile_pattern(pattern: &str) -> Option<regress::Regex> {
    let flags = regress::Flags {
        unicode: true,
        ..Default::default()
    };
    regress::Regex::with_flags(pattern, flags).ok()
}

/// JS `regexp.test(text)`：任意位置匹配。
///
/// pattern 非法时 TypeBox 会抛未捕获的 `SyntaxError`；Rust 不 panic，视为不匹配
/// （[`normalize`]/[`assert_valid`] 会先经 [`first_invalid_regex`] 转成 `unavailable` 错误）。
fn pattern_matches(pattern: &str, text: &str) -> bool {
    compile_pattern(pattern).is_some_and(|regex| regex.find(text).is_some())
}

/// `GetPropertiesPattern` 的语义：键是已声明属性名，或匹配任一 `patternProperties`。
fn is_known_property_key(schema: &Map<String, Value>, key: &str) -> bool {
    if let Some(Value::Object(properties)) = schema.get("properties")
        && properties.contains_key(key)
    {
        return true;
    }
    if let Some(Value::Object(patterns)) = schema.get("patternProperties") {
        for pattern in patterns.keys() {
            if pattern_matches(pattern, key) {
                return true;
            }
        }
    }
    false
}

/// 递归校验（`CheckSchema` / `ErrorSchema` 合一）。
///
/// Check 模式短路（TypeBox 的 `&&` 链）；Error 模式评估所有关键字（TypeBox 的 `&` 链），
/// 受 `Settings.maxErrors = 8` 截断。
///
/// 关键字按固定顺序评估（`type` → 对象 → 数组 → 字符串 → 数字 → `$ref` 家族 →
/// `const`/`enum` → `if` → `not` → `allOf`/`anyOf`/`oneOf` → `unevaluated*`）。两个宏决定
/// 控制流：`gate!` 在失败时让 Check 模式立刻 `return false`、Error 模式只把 `ok` 置
/// `false` 继续；`child!()` 为组合关键字的分支建立独立上下文，分支成功后 `merge` 把错误
/// 与已求值键/下标集合合回当前上下文。
fn validate_schema(
    walk: &mut Walk,
    schema: &Value,
    value: &Value,
    schema_path: &str,
    instance_path: &str,
) -> bool {
    if walk.at_capacity() {
        return false;
    }
    // `Schema.IsSchemaBoolean`。
    if let Value::Bool(allowed) = schema {
        if *allowed {
            return true;
        }
        return walk.add(TbError {
            keyword: "boolean".to_string(),
            schema_path: schema_path.to_string(),
            instance_path: instance_path.to_string(),
            params: params(vec![]),
        });
    }
    let Value::Object(schema) = schema else {
        return true;
    };

    let mut ok = true;
    macro_rules! gate {
        ($this_ok:expr) => {{
            if !$this_ok {
                ok = false;
                if walk.mode == Mode::Check {
                    return false;
                }
            }
        }};
    }
    macro_rules! child {
        () => {
            walk.branch()
        };
    }

    // type
    if is_type(schema) {
        let names = type_names(schema);
        let matched = names.iter().any(|name| type_name_matches(name, value));
        if !matched {
            let type_param = schema.get("type").cloned().unwrap_or(Value::Null);
            walk.add(TbError {
                keyword: "type".to_string(),
                schema_path: schema_path.to_string(),
                instance_path: instance_path.to_string(),
                params: params(vec![("type", type_param)]),
            });
            gate!(false);
        }
    }

    // 对象关键字（value 是对象且不是数组）。
    if let Value::Object(object) = value {
        if is_required(schema) {
            let required: Vec<String> = schema
                .get("required")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let missing: Vec<Value> = required
                .iter()
                .filter(|key| !has_js_property_key(object, key))
                .map(|key| Value::String(key.clone()))
                .collect();
            if !missing.is_empty() {
                walk.add(TbError {
                    keyword: "required".to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    params: params(vec![("requiredProperties", Value::Array(missing))]),
                });
                gate!(false);
            }
        }
        if is_additional_properties(schema) {
            let additional = schema
                .get("additionalProperties")
                .cloned()
                .unwrap_or(Value::Null);
            let mut extra: Vec<Value> = Vec::new();
            let mut all_ok = true;
            for key in js_keys(object) {
                if is_known_property_key(schema, key) {
                    continue;
                }
                let next_schema_path = path_key(schema_path, "additionalProperties");
                let next_instance_path = path_key(instance_path, key);
                if validate_schema_push(
                    walk,
                    &additional,
                    &object[key.as_str()],
                    &next_schema_path,
                    &next_instance_path,
                ) {
                    walk.add_eval_key(key);
                } else {
                    all_ok = false;
                    extra.push(Value::String(key.clone()));
                }
            }
            if !all_ok {
                walk.add(TbError {
                    keyword: "additionalProperties".to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    params: params(vec![("additionalProperties", Value::Array(extra))]),
                });
                gate!(false);
            }
        }
        if is_dependencies(schema) {
            let entries: Vec<(String, Value)> = schema
                .get("dependencies")
                .and_then(Value::as_object)
                .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();
            let mut all_ok = true;
            for (key, dependency) in entries {
                if !has_js_property_key(object, &key) {
                    continue;
                }
                match &dependency {
                    Value::Array(keys) => {
                        for dependent in keys.iter().filter_map(Value::as_str) {
                            if !has_js_property_key(object, dependent) {
                                all_ok = false;
                                walk.add(TbError {
                                    keyword: "dependencies".to_string(),
                                    schema_path: schema_path.to_string(),
                                    instance_path: instance_path.to_string(),
                                    params: params(vec![
                                        ("property", Value::String(key.clone())),
                                        ("dependencies", dependency.clone()),
                                    ]),
                                });
                            }
                        }
                    }
                    other => {
                        let next_schema_path =
                            path_key(&path_key(schema_path, "dependencies"), &key);
                        if !validate_schema(walk, other, value, &next_schema_path, instance_path) {
                            all_ok = false;
                        }
                    }
                }
            }
            if !object.is_empty() && !all_ok {
                gate!(false);
            }
        }
        if is_string_array_map_keyword(schema, "dependentRequired") {
            let entries: Vec<(String, Value)> = schema
                .get("dependentRequired")
                .and_then(Value::as_object)
                .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();
            let mut all_ok = true;
            for (key, keys) in entries {
                if !has_js_property_key(object, &key) {
                    continue;
                }
                for dependent in keys.as_array().cloned().unwrap_or_default() {
                    let Some(dependent) = dependent.as_str() else {
                        continue;
                    };
                    if !has_js_property_key(object, dependent) {
                        all_ok = false;
                        walk.add(TbError {
                            keyword: "dependentRequired".to_string(),
                            schema_path: schema_path.to_string(),
                            instance_path: instance_path.to_string(),
                            params: params(vec![
                                ("property", Value::String(key.clone())),
                                ("dependencies", keys.clone()),
                            ]),
                        });
                    }
                }
            }
            if !object.is_empty() && !all_ok {
                gate!(false);
            }
        }
        if is_schema_object_keyword(schema, "dependentSchemas") {
            let entries: Vec<(String, Value)> = schema
                .get("dependentSchemas")
                .and_then(Value::as_object)
                .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();
            let mut all_ok = true;
            for (key, dependency) in entries {
                if !has_js_property_key(object, &key) {
                    continue;
                }
                let next_schema_path = path_key(&path_key(schema_path, "dependentSchemas"), &key);
                if !validate_schema(walk, &dependency, value, &next_schema_path, instance_path) {
                    all_ok = false;
                }
            }
            if !object.is_empty() && !all_ok {
                gate!(false);
            }
        }
        if is_pattern_properties(schema) {
            let patterns: Vec<(String, Value)> = schema
                .get("patternProperties")
                .and_then(Value::as_object)
                .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();
            let mut all_ok = true;
            for (pattern, property) in patterns {
                let next_schema_path =
                    path_key(&path_key(schema_path, "patternProperties"), &pattern);
                for key in js_keys(object) {
                    if !pattern_matches(&pattern, key) {
                        continue;
                    }
                    let next_instance_path = path_key(instance_path, key);
                    if validate_schema_push(
                        walk,
                        &property,
                        &object[key.as_str()],
                        &next_schema_path,
                        &next_instance_path,
                    ) {
                        walk.add_eval_key(key);
                    } else {
                        all_ok = false;
                    }
                }
            }
            if !all_ok {
                gate!(false);
            }
        }
        if is_properties(schema) {
            let properties: Vec<(String, Value)> = schema
                .get("properties")
                .and_then(Value::as_object)
                .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();
            let mut all_ok = true;
            for (key, property) in properties {
                // `Guard.HasPropertyKey(value, key)`：原型链上的名字（`toString` 等）也算"存在"，
                // 其值是 `Object.prototype` 上的函数（见 [`validate_inherited`]）。
                if !has_js_property_key(object, &key) {
                    continue;
                }
                let next_schema_path = path_key(&path_key(schema_path, "properties"), &key);
                let next_instance_path = path_key(instance_path, &key);
                let ok = match object.get(&key) {
                    Some(property_value) => validate_schema_push(
                        walk,
                        &property,
                        property_value,
                        &next_schema_path,
                        &next_instance_path,
                    ),
                    None => validate_inherited_push(
                        walk,
                        &property,
                        &next_schema_path,
                        &next_instance_path,
                    ),
                };
                if ok {
                    walk.add_eval_key(&key);
                } else {
                    all_ok = false;
                }
            }
            if !all_ok {
                gate!(false);
            }
        }
        if is_schema_keyword(schema, "propertyNames") {
            let property_names = schema.get("propertyNames").cloned().unwrap_or(Value::Null);
            let mut invalid: Vec<Value> = Vec::new();
            let mut all_ok = true;
            let next_schema_path = path_key(schema_path, "propertyNames");
            for key in js_keys(object) {
                let next_instance_path = path_key(instance_path, key);
                if !validate_schema(
                    walk,
                    &property_names,
                    &Value::String(key.clone()),
                    &next_schema_path,
                    &next_instance_path,
                ) {
                    all_ok = false;
                    invalid.push(Value::String(key.clone()));
                }
            }
            if !all_ok {
                walk.add(TbError {
                    keyword: "propertyNames".to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    params: params(vec![("propertyNames", Value::Array(invalid))]),
                });
                gate!(false);
            }
        }
        if is_number_keyword(schema, "minProperties") {
            let limit = schema
                .get("minProperties")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            if (object.len() as f64) < limit {
                walk.add(TbError {
                    keyword: "minProperties".to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    params: params(vec![("limit", keyword_value(schema, "minProperties"))]),
                });
                gate!(false);
            }
        }
        if is_number_keyword(schema, "maxProperties") {
            let limit = schema
                .get("maxProperties")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            if (object.len() as f64) > limit {
                walk.add(TbError {
                    keyword: "maxProperties".to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    params: params(vec![("limit", keyword_value(schema, "maxProperties"))]),
                });
                gate!(false);
            }
        }
    }

    // 数组关键字。
    if let Value::Array(items) = value {
        if is_additional_items(schema) && matches!(schema.get("items"), Some(Value::Array(_))) {
            let tuple_items = schema
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let additional = schema
                .get("additionalItems")
                .cloned()
                .unwrap_or(Value::Null);
            let mut all_ok = true;
            for (index, item) in items.iter().enumerate() {
                if index < tuple_items.len() {
                    continue;
                }
                if validate_schema_push(
                    walk,
                    &additional,
                    item,
                    &path_key(schema_path, "additionalItems"),
                    &path_index(instance_path, index),
                ) {
                    walk.add_eval_index(index);
                } else {
                    // TypeBox `ErrorAdditionalItems` 用 `Guard.Every`：首个失败的额外元素后
                    // 短路，后续额外元素不再报告错误。
                    all_ok = false;
                    break;
                }
            }
            if !all_ok {
                gate!(false);
            }
        }
        if is_schema_keyword(schema, "contains") {
            let contains = schema.get("contains").cloned().unwrap_or(Value::Null);
            let skip = matches!(schema.get("minContains"), Some(Value::Number(n)) if n.as_f64() == Some(0.0));
            if !skip {
                let mut matched = false;
                if !items.is_empty() {
                    for (index, item) in items.iter().enumerate() {
                        let mut probe = walk.check_branch();
                        if validate_schema(
                            &mut probe,
                            &contains,
                            item,
                            &path_key(schema_path, "contains"),
                            &path_index(instance_path, index),
                        ) {
                            matched = true;
                            walk.add_eval_index(index);
                            break;
                        }
                    }
                }
                if !matched {
                    walk.add(TbError {
                        keyword: "contains".to_string(),
                        schema_path: schema_path.to_string(),
                        instance_path: instance_path.to_string(),
                        params: params(vec![("minContains", Value::from(1))]),
                    });
                    gate!(false);
                }
            }
        }
        if is_items(schema) {
            match schema.get("items") {
                Some(Value::Array(tuple_items)) => {
                    let tuple_items = tuple_items.clone();
                    let mut all_ok = true;
                    for (index, item_schema) in tuple_items.iter().enumerate() {
                        let Some(item) = items.get(index) else {
                            continue;
                        };
                        let next_schema_path = path_index(&path_key(schema_path, "items"), index);
                        let next_instance_path = path_index(instance_path, index);
                        if validate_schema_push(
                            walk,
                            item_schema,
                            item,
                            &next_schema_path,
                            &next_instance_path,
                        ) {
                            walk.add_eval_index(index);
                        } else {
                            all_ok = false;
                        }
                    }
                    if !all_ok {
                        gate!(false);
                    }
                }
                Some(item_schema) => {
                    let item_schema = item_schema.clone();
                    let offset = schema
                        .get("prefixItems")
                        .and_then(Value::as_array)
                        .map(|items| items.len())
                        .unwrap_or(0);
                    let mut all_ok = true;
                    for (index, item) in items.iter().enumerate().skip(offset) {
                        if validate_schema_push(
                            walk,
                            &item_schema,
                            item,
                            &path_key(schema_path, "items"),
                            &path_index(instance_path, index),
                        ) {
                            walk.add_eval_index(index);
                        } else {
                            all_ok = false;
                        }
                    }
                    if !all_ok {
                        gate!(false);
                    }
                }
                None => {}
            }
        }
        if is_number_keyword(schema, "maxContains") && is_schema_keyword(schema, "contains") {
            let contains = schema.get("contains").cloned().unwrap_or(Value::Null);
            let limit = schema
                .get("maxContains")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let mut count = 0f64;
            for item in items {
                let mut probe = walk.check_branch();
                if validate_schema(&mut probe, &contains, item, schema_path, instance_path) {
                    count += 1.0;
                }
            }
            if count > limit {
                walk.add(TbError {
                    keyword: "contains".to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    // TS: `Schema.IsMinContains(schema) ? schema.minContains : 1`。
                    params: params(vec![
                        (
                            "minContains",
                            if is_number_keyword(schema, "minContains") {
                                keyword_value(schema, "minContains")
                            } else {
                                Value::from(1)
                            },
                        ),
                        ("maxContains", keyword_value(schema, "maxContains")),
                    ]),
                });
                gate!(false);
            }
        }
        if is_number_keyword(schema, "maxItems") {
            let limit = schema
                .get("maxItems")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            if (items.len() as f64) > limit {
                walk.add(TbError {
                    keyword: "maxItems".to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    params: params(vec![("limit", keyword_value(schema, "maxItems"))]),
                });
                gate!(false);
            }
        }
        if is_number_keyword(schema, "minContains") && is_schema_keyword(schema, "contains") {
            let contains = schema.get("contains").cloned().unwrap_or(Value::Null);
            let limit = schema
                .get("minContains")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let mut count = 0f64;
            for (index, item) in items.iter().enumerate() {
                let mut probe = walk.check_branch();
                if validate_schema(&mut probe, &contains, item, schema_path, instance_path) {
                    count += 1.0;
                    walk.add_eval_index(index);
                }
            }
            if count < limit {
                walk.add(TbError {
                    keyword: "contains".to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    params: params(vec![("minContains", keyword_value(schema, "minContains"))]),
                });
                gate!(false);
            }
        }
        if is_number_keyword(schema, "minItems") {
            let limit = schema
                .get("minItems")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            if (items.len() as f64) < limit {
                walk.add(TbError {
                    keyword: "minItems".to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    params: params(vec![("limit", keyword_value(schema, "minItems"))]),
                });
                gate!(false);
            }
        }
        if is_prefix_items(schema) && !items.is_empty() {
            let prefix: Vec<Value> = schema
                .get("prefixItems")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut all_ok = true;
            for (index, item_schema) in prefix.iter().enumerate() {
                let Some(item) = items.get(index) else {
                    break;
                };
                let next_schema_path = path_index(&path_key(schema_path, "prefixItems"), index);
                let next_instance_path = path_index(instance_path, index);
                if validate_schema_push(
                    walk,
                    item_schema,
                    item,
                    &next_schema_path,
                    &next_instance_path,
                ) {
                    walk.add_eval_index(index);
                } else {
                    all_ok = false;
                }
            }
            if !all_ok {
                gate!(false);
            }
        }
        if is_boolean_keyword(schema, "uniqueItems")
            && schema.get("uniqueItems") == Some(&Value::Bool(true))
        {
            let mut seen: Vec<String> = Vec::new();
            let mut duplicates: Vec<Value> = Vec::new();
            for (index, item) in items.iter().enumerate() {
                let hash = value_hash(item);
                if seen.contains(&hash) {
                    duplicates.push(Value::from(index as u64));
                } else {
                    seen.push(hash);
                }
            }
            if !duplicates.is_empty() {
                walk.add(TbError {
                    keyword: "uniqueItems".to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    params: params(vec![("duplicateItems", Value::Array(duplicates))]),
                });
                gate!(false);
            }
        }
    }

    // 字符串关键字。
    if let Value::String(text) = value {
        if is_number_keyword(schema, "maxLength") {
            let limit = schema
                .get("maxLength")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            if !is_max_length(text, limit) {
                walk.add(TbError {
                    keyword: "maxLength".to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    params: params(vec![("limit", keyword_value(schema, "maxLength"))]),
                });
                gate!(false);
            }
        }
        if is_number_keyword(schema, "minLength") {
            let limit = schema
                .get("minLength")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            if !is_min_length(text, limit) {
                walk.add(TbError {
                    keyword: "minLength".to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    params: params(vec![("limit", keyword_value(schema, "minLength"))]),
                });
                gate!(false);
            }
        }
        if is_string_keyword(schema, "format") {
            let format = schema.get("format").and_then(Value::as_str).unwrap_or("");
            if !format_test(format, text) {
                walk.add(TbError {
                    keyword: "format".to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    params: params(vec![("format", Value::String(format.to_string()))]),
                });
                gate!(false);
            }
        }
        if is_string_keyword(schema, "pattern") {
            let pattern = schema.get("pattern").and_then(Value::as_str).unwrap_or("");
            if !pattern_matches(pattern, text) {
                walk.add(TbError {
                    keyword: "pattern".to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    params: params(vec![("pattern", Value::String(pattern.to_string()))]),
                });
                gate!(false);
            }
        }
    }

    // 数字关键字。
    if let Value::Number(number) = value
        && let Some(actual) = number.as_f64()
    {
        for (keyword, comparison) in [
            ("exclusiveMaximum", "<"),
            ("exclusiveMinimum", ">"),
            ("maximum", "<="),
            ("minimum", ">="),
        ] {
            if !is_number_keyword(schema, keyword) {
                continue;
            }
            let limit = schema.get(keyword).and_then(Value::as_f64).unwrap_or(0.0);
            let satisfied = match comparison {
                "<" => actual < limit,
                ">" => actual > limit,
                "<=" => actual <= limit,
                _ => actual >= limit,
            };
            if !satisfied {
                walk.add(TbError {
                    keyword: keyword.to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    params: params(vec![
                        ("comparison", Value::String(comparison.to_string())),
                        ("limit", keyword_value(schema, keyword)),
                    ]),
                });
                gate!(false);
            }
        }
        if is_number_keyword(schema, "multipleOf") {
            let divisor = schema
                .get("multipleOf")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            if !is_multiple_of(actual, divisor) {
                walk.add(TbError {
                    keyword: "multipleOf".to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    params: params(vec![("multipleOf", keyword_value(schema, "multipleOf"))]),
                });
                gate!(false);
            }
        }
    }

    // $ref / $recursiveRef / $dynamicRef（`CheckSchema` 在 const 之前评估）。
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let target = resolve_schema_ref(&walk.root, reference);
        let mut next = walk.branch();
        let ok_ref = match &target {
            Some(target) => validate_schema(&mut next, target, value, "#", instance_path),
            None => validate_schema(&mut next, &Value::Bool(false), value, "#", instance_path),
        };
        if ok_ref {
            walk.merge(next);
        } else {
            // `ErrorRef`：失败时把目标上下文的错误并入当前上下文。
            walk.merge(next);
            gate!(false);
        }
    }
    if let Some(reference) = schema.get("$recursiveRef").and_then(Value::as_str) {
        let target = if reference == "#" {
            Some((*walk.root).clone())
        } else {
            resolve_schema_ref(&walk.root, reference)
        };
        let mut next = walk.branch();
        let ok_ref = match &target {
            Some(target) => validate_schema(&mut next, target, value, "#", instance_path),
            None => validate_schema(&mut next, &Value::Bool(false), value, "#", instance_path),
        };
        if ok_ref {
            walk.merge(next);
        } else {
            walk.merge(next);
            gate!(false);
        }
    }
    if let Some(reference) = schema.get("$dynamicRef").and_then(Value::as_str) {
        let target = resolve_schema_ref(&walk.root, reference).or_else(|| {
            reference
                .strip_prefix('#')
                .and_then(|name| find_dynamic_anchor(&walk.root, name))
        });
        let mut next = walk.branch();
        let ok_ref = match &target {
            Some(target) => validate_schema(&mut next, target, value, "#", instance_path),
            None => validate_schema(&mut next, &Value::Bool(false), value, "#", instance_path),
        };
        if ok_ref {
            walk.merge(next);
        } else {
            walk.merge(next);
            gate!(false);
        }
    }

    // const / enum
    if has_property_key(schema, "const") {
        let allowed = schema.get("const").cloned().unwrap_or(Value::Null);
        if !is_deep_equal(value, &allowed) {
            walk.add(TbError {
                keyword: "const".to_string(),
                schema_path: schema_path.to_string(),
                instance_path: instance_path.to_string(),
                params: params(vec![("allowedValue", allowed)]),
            });
            gate!(false);
        }
    }
    if let Some(Value::Array(options)) = schema.get("enum") {
        let matched = options.iter().any(|option| is_deep_equal(value, option));
        if !matched {
            walk.add(TbError {
                keyword: "enum".to_string(),
                schema_path: schema_path.to_string(),
                instance_path: instance_path.to_string(),
                params: params(vec![("allowedValues", Value::Array(options.clone()))]),
            });
            gate!(false);
        }
    }

    // if / then / else
    if is_schema_keyword(schema, "if") {
        let condition = schema.get("if").cloned().unwrap_or(Value::Null);
        let then_schema = schema.get("then").cloned().unwrap_or(Value::Bool(true));
        let else_schema = schema.get("else").cloned().unwrap_or(Value::Bool(true));
        // `ErrorIf`：`if` 与 `then` 共用同一个**私有** `ErrorContext`（`trueContext`）。整体成功时
        // `context.Merge(trueContext)` 把已求值集合合回主流（`Merge` 不复制错误）；失败时丢弃。
        let mut true_walk = walk.branch();
        let condition_ok = validate_schema(
            &mut true_walk,
            &condition,
            value,
            &path_key(schema_path, "if"),
            instance_path,
        );
        if condition_ok {
            // `then` 失败时丢弃它的错误，只报一条 `if`（`params.failingKeyword = "then"`）。
            let next_path = path_key(schema_path, "then");
            if validate_schema(
                &mut true_walk,
                &then_schema,
                value,
                &next_path,
                instance_path,
            ) {
                walk.merge(true_walk);
            } else {
                walk.add(TbError {
                    keyword: "if".to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    params: params(vec![("failingKeyword", Value::String("then".to_string()))]),
                });
                gate!(false);
            }
        } else {
            // 条件失败：`else` 在**主流**里评估；成功时 `isIf` 为真，仍 Merge 条件上下文的已求值
            // 集合（错误从不 Merge），失败时报 else 的错误 + 一条 `if`。
            true_walk.errors.clear();
            let next_path = path_key(schema_path, "else");
            if validate_schema(walk, &else_schema, value, &next_path, instance_path) {
                walk.merge(true_walk);
            } else {
                walk.add(TbError {
                    keyword: "if".to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    params: params(vec![("failingKeyword", Value::String("else".to_string()))]),
                });
                gate!(false);
            }
        }
    }

    // not
    if is_schema_keyword(schema, "not") {
        let negated = schema.get("not").cloned().unwrap_or(Value::Null);
        let mut probe = walk.check_branch();
        let matched = validate_schema(&mut probe, &negated, value, schema_path, instance_path);
        if matched {
            walk.add(TbError {
                keyword: "not".to_string(),
                schema_path: schema_path.to_string(),
                instance_path: instance_path.to_string(),
                params: params(vec![]),
            });
            gate!(false);
        }
    }

    // allOf
    if is_schema_array_keyword(schema, "allOf") {
        let branches: Vec<Value> = schema
            .get("allOf")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut all_ok = true;
        let mut failed: Vec<Walk> = Vec::new();
        for (index, branch) in branches.iter().enumerate() {
            let next_path = path_index(&path_key(schema_path, "allOf"), index);
            let mut branch_walk = child!();
            if validate_schema(&mut branch_walk, branch, value, &next_path, instance_path) {
                walk.merge(branch_walk);
            } else {
                all_ok = false;
                failed.push(branch_walk);
            }
        }
        if !all_ok {
            for context in failed {
                walk.merge(context);
            }
            gate!(false);
        }
    }
    // anyOf
    if is_schema_array_keyword(schema, "anyOf") {
        let branches: Vec<Value> = schema
            .get("anyOf")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut passed = 0usize;
        let mut failed: Vec<Walk> = Vec::new();
        for (index, branch) in branches.iter().enumerate() {
            let next_path = path_index(&path_key(schema_path, "anyOf"), index);
            let mut branch_walk = child!();
            if validate_schema(&mut branch_walk, branch, value, &next_path, instance_path) {
                passed += 1;
                walk.merge(branch_walk);
            } else {
                failed.push(branch_walk);
            }
        }
        if passed == 0 {
            for context in failed {
                walk.merge(context);
            }
            walk.add(TbError {
                keyword: "anyOf".to_string(),
                schema_path: schema_path.to_string(),
                instance_path: instance_path.to_string(),
                params: params(vec![]),
            });
            gate!(false);
        }
    }
    // oneOf
    if is_schema_array_keyword(schema, "oneOf") {
        let branches: Vec<Value> = schema
            .get("oneOf")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut passed = 0usize;
        let mut passing_schemas: Vec<Value> = Vec::new();
        let mut failed: Vec<Walk> = Vec::new();
        for (index, branch) in branches.iter().enumerate() {
            let next_path = path_index(&path_key(schema_path, "oneOf"), index);
            let mut branch_walk = child!();
            if validate_schema(&mut branch_walk, branch, value, &next_path, instance_path) {
                passed += 1;
                passing_schemas.push(Value::from(index as u64));
                walk.merge(branch_walk);
            } else {
                failed.push(branch_walk);
            }
        }
        if passed != 1 {
            if passing_schemas.is_empty() {
                for context in failed {
                    walk.merge(context);
                }
            }
            walk.add(TbError {
                keyword: "oneOf".to_string(),
                schema_path: schema_path.to_string(),
                instance_path: instance_path.to_string(),
                params: params(vec![("passingSchemas", Value::Array(passing_schemas))]),
            });
            gate!(false);
        }
    }

    // `unevaluatedItems` / `unevaluatedProperties`：在 if/not/allOf/anyOf/oneOf 之后评估，
    // 只检查"尚未被求值"的数组下标/对象键（TypeBox `CheckContext` 的键集）。
    if is_schema_keyword(schema, "unevaluatedItems")
        && let Value::Array(items) = value
    {
        let unevaluated = schema
            .get("unevaluatedItems")
            .cloned()
            .unwrap_or(Value::Null);
        let mut offending: Vec<Value> = Vec::new();
        let mut all_ok = true;
        for (index, item) in items.iter().enumerate() {
            if walk.eval_index_seen(index) {
                continue;
            }
            let mut probe = walk.check_branch();
            if validate_schema(&mut probe, &unevaluated, item, schema_path, instance_path) {
                walk.add_eval_index(index);
            } else {
                all_ok = false;
                offending.push(Value::from(index as u64));
            }
        }
        if !all_ok {
            walk.add(TbError {
                keyword: "unevaluatedItems".to_string(),
                schema_path: schema_path.to_string(),
                instance_path: instance_path.to_string(),
                params: params(vec![("unevaluatedItems", Value::Array(offending))]),
            });
            gate!(false);
        }
    }
    if is_schema_keyword(schema, "unevaluatedProperties")
        && let Value::Object(object) = value
    {
        let unevaluated = schema
            .get("unevaluatedProperties")
            .cloned()
            .unwrap_or(Value::Null);
        let mut offending: Vec<Value> = Vec::new();
        let mut all_ok = true;
        for key in js_keys(object) {
            if walk.eval_key_seen(key) {
                continue;
            }
            let mut probe = walk.check_branch();
            if validate_schema(
                &mut probe,
                &unevaluated,
                &object[key.as_str()],
                schema_path,
                instance_path,
            ) {
                walk.add_eval_key(key);
            } else {
                all_ok = false;
                offending.push(Value::String(key.clone()));
            }
        }
        if !all_ok {
            walk.add(TbError {
                keyword: "unevaluatedProperties".to_string(),
                schema_path: schema_path.to_string(),
                instance_path: instance_path.to_string(),
                params: params(vec![("unevaluatedProperties", Value::Array(offending))]),
            });
            gate!(false);
        }
    }

    ok
}

/// `CheckSchemaPushStack` 的"继承函数值"版本。
fn validate_inherited_push(
    walk: &mut Walk,
    schema: &Value,
    schema_path: &str,
    instance_path: &str,
) -> bool {
    walk.push_eval();
    if validate_inherited(walk, schema, schema_path, instance_path) {
        walk.pop_eval();
        true
    } else {
        false
    }
}

/// 检查 JS 原型链上的继承值（`Object.prototype` 的成员函数）。
///
/// JSON 没有函数值，但 `Guard.HasPropertyKey(value, "toString")` 在 `{}` 上为真，TypeBox 会用
/// `value["toString"]`（一个 native 函数）去跑属性 schema。值的 JS 类型是 `function`：只有
/// `type`（`function`/`constructor`/未知名字）、`$ref` 与逻辑关键字（`if`/`not`/`allOf`/
/// `anyOf`/`oneOf`）可能命中；`const`/`enum` 永远不相等；对象/数组/字符串/数字关键字被
/// `G.IsObject/IsArray/...` 守卫掉。
fn validate_inherited(
    walk: &mut Walk,
    schema: &Value,
    schema_path: &str,
    instance_path: &str,
) -> bool {
    if walk.at_capacity() {
        return false;
    }
    if let Value::Bool(allowed) = schema {
        if *allowed {
            return true;
        }
        return walk.add(TbError {
            keyword: "boolean".to_string(),
            schema_path: schema_path.to_string(),
            instance_path: instance_path.to_string(),
            params: params(vec![]),
        });
    }
    let Value::Object(map) = schema else {
        return true;
    };
    let mut ok = true;
    macro_rules! gate {
        ($this_ok:expr) => {{
            if !$this_ok {
                ok = false;
                if walk.mode == Mode::Check {
                    return false;
                }
            }
        }};
    }

    if is_type(map) {
        let names = type_names(map);
        // 继承值都是 `Object.prototype` 上的 native 函数：`typeof === "function"`，且
        // `Function.prototype.toString` 含 `[native code]`，因此 `Guard.IsConstructor` 也为真。
        let matched = names.iter().any(|name| match *name {
            "function" | "constructor" => true,
            "object" | "array" | "boolean" | "integer" | "number" | "null" | "string"
            | "bigint" | "symbol" | "undefined" | "void" => false,
            _ => true,
        });
        if !matched {
            let type_param = map.get("type").cloned().unwrap_or(Value::Null);
            walk.add(TbError {
                keyword: "type".to_string(),
                schema_path: schema_path.to_string(),
                instance_path: instance_path.to_string(),
                params: params(vec![("type", type_param)]),
            });
            gate!(false);
        }
    }

    // `const`/`enum`：`IsDeepEqual(function, <JSON value>)` 恒为 false。
    if has_property_key(map, "const") {
        let allowed = map.get("const").cloned().unwrap_or(Value::Null);
        walk.add(TbError {
            keyword: "const".to_string(),
            schema_path: schema_path.to_string(),
            instance_path: instance_path.to_string(),
            params: params(vec![("allowedValue", allowed)]),
        });
        gate!(false);
    }
    if let Some(Value::Array(options)) = map.get("enum") {
        walk.add(TbError {
            keyword: "enum".to_string(),
            schema_path: schema_path.to_string(),
            instance_path: instance_path.to_string(),
            params: params(vec![("allowedValues", Value::Array(options.clone()))]),
        });
        gate!(false);
    }

    if is_schema_keyword(map, "if") {
        let condition = map.get("if").cloned().unwrap_or(Value::Null);
        let then_schema = map.get("then").cloned().unwrap_or(Value::Bool(true));
        let else_schema = map.get("else").cloned().unwrap_or(Value::Bool(true));
        // 与 [`validate_schema`] 的 `if` 相同：`if`+`then` 共用一个私有上下文，成功才 Merge。
        let mut true_walk = walk.branch();
        let condition_ok = validate_inherited(
            &mut true_walk,
            &condition,
            &path_key(schema_path, "if"),
            instance_path,
        );
        if condition_ok {
            if validate_inherited(
                &mut true_walk,
                &then_schema,
                &path_key(schema_path, "then"),
                instance_path,
            ) {
                walk.merge(true_walk);
            } else {
                walk.add(TbError {
                    keyword: "if".to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    params: params(vec![("failingKeyword", Value::String("then".to_string()))]),
                });
                gate!(false);
            }
        } else {
            true_walk.errors.clear();
            if validate_inherited(
                walk,
                &else_schema,
                &path_key(schema_path, "else"),
                instance_path,
            ) {
                walk.merge(true_walk);
            } else {
                walk.add(TbError {
                    keyword: "if".to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    params: params(vec![("failingKeyword", Value::String("else".to_string()))]),
                });
                gate!(false);
            }
        }
    }

    if is_schema_keyword(map, "not") {
        let negated = map.get("not").cloned().unwrap_or(Value::Null);
        let mut probe = walk.check_branch();
        if validate_inherited(&mut probe, &negated, schema_path, instance_path) {
            walk.add(TbError {
                keyword: "not".to_string(),
                schema_path: schema_path.to_string(),
                instance_path: instance_path.to_string(),
                params: params(vec![]),
            });
            gate!(false);
        }
    }

    for keyword in ["allOf", "anyOf", "oneOf"] {
        if !is_schema_array_keyword(map, keyword) {
            continue;
        }
        let branches: Vec<Value> = map
            .get(keyword)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut passed = 0usize;
        let mut passing: Vec<Value> = Vec::new();
        let mut failed: Vec<Walk> = Vec::new();
        for (index, branch) in branches.iter().enumerate() {
            let next_path = path_index(&path_key(schema_path, keyword), index);
            let mut branch_walk = walk.branch();
            if validate_inherited(&mut branch_walk, branch, &next_path, instance_path) {
                passed += 1;
                if keyword == "oneOf" {
                    passing.push(Value::from(index as u64));
                }
                walk.merge(branch_walk);
            } else {
                failed.push(branch_walk);
            }
        }
        match keyword {
            "allOf" if passed != branches.len() => {
                for context in failed {
                    walk.merge(context);
                }
                gate!(false);
            }
            "anyOf" if passed == 0 => {
                for context in failed {
                    walk.merge(context);
                }
                walk.add(TbError {
                    keyword: "anyOf".to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    params: params(vec![]),
                });
                gate!(false);
            }
            "oneOf" if passed != 1 => {
                if passing.is_empty() {
                    for context in failed {
                        walk.merge(context);
                    }
                }
                walk.add(TbError {
                    keyword: "oneOf".to_string(),
                    schema_path: schema_path.to_string(),
                    instance_path: instance_path.to_string(),
                    params: params(vec![("passingSchemas", Value::Array(passing))]),
                });
                gate!(false);
            }
            _ => {}
        }
    }

    // `$ref`：按根 schema 解析（解析不到的 `$ref` 在 TypeBox 里是 `false` schema）。
    if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
        let target = resolve_schema_ref(&walk.root, reference);
        let allowed = match target {
            Some(target) => validate_inherited(walk, &target, "#", instance_path),
            None => walk.add(TbError {
                keyword: "boolean".to_string(),
                schema_path: "#".to_string(),
                instance_path: instance_path.to_string(),
                params: params(vec![]),
            }),
        };
        if !allowed {
            gate!(false);
        }
    }

    ok
}

// ---------------------------------------------------------------------------
// $ref 解析（`schema/resolve/resolve.mjs` 的子集）
//
// TypeBox 的 `Stack.Ref` 用 URL 基址 + `$id`/`$anchor`/`$dynamicAnchor` + JSON Pointer 解析。
// chord 的 schema 来自本进程内构造的 TypeBox 对象或同一条 JSON 文档，不加载远程文档，
// 因此这里实现同文档解析：
// - `#` / `#/pointer`：根（或当前词法资源）上的 JSON Pointer；
// - 绝对/相对 URL 形式的名字（`node`、`https://…/node`）：在整棵 schema 树里按 `$id` 计算
//   出的 base 文档路径匹配；
// - `$anchor` / `$dynamicAnchor` 片段。
// 解析不到时返回 `None`（TypeBox 视作 `false` schema）。
// ---------------------------------------------------------------------------

/// `Resolve.DefaultBase`。
const JSON_SCHEMA_DEFAULT_BASE: &str = "https://json-schema.org/";

/// JSON Pointer 的 `~1`/`~0` 解码。
fn pointer_unescape(token: &str) -> String {
    let mut out = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(ch) = chars.next() {
        if ch == '~' {
            match chars.next() {
                Some('0') => out.push('~'),
                Some('1') => out.push('/'),
                Some(other) => {
                    out.push('~');
                    out.push(other);
                }
                None => out.push('~'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// `%XX` 解码（`decodeURIComponent` 的近似：不处理非法序列时原样返回）。
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && bytes[index + 1].is_ascii_hexdigit()
            && bytes[index + 2].is_ascii_hexdigit()
        {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("00");
            out.push(u8::from_str_radix(hex, 16).unwrap_or(b'?'));
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `Pointer.Get(schema, pointer)`：RFC 6901。
fn pointer_get(schema: &Value, pointer: &str) -> Option<Value> {
    if pointer.is_empty() {
        return Some(schema.clone());
    }
    let mut current = schema.clone();
    for raw in pointer.split('/').skip(1) {
        let token = pointer_unescape(raw);
        current = match current {
            Value::Object(map) => map.get(&token)?.clone(),
            Value::Array(items) => {
                if token == "-" {
                    return None;
                }
                let index: usize = token.parse().ok()?;
                items.get(index)?.clone()
            }
            _ => return None,
        };
    }
    Some(current)
}

/// `MatchHash(schema, ref)`。
fn match_hash(schema: &Value, absolute: &url::Url) -> Option<Value> {
    if absolute.as_str().ends_with('#') {
        return Some(schema.clone());
    }
    let fragment = absolute.fragment()?;
    if fragment.is_empty() {
        return None;
    }
    let decoded = percent_decode(fragment);
    if !decoded.starts_with('/') {
        return None;
    }
    pointer_get(schema, &decoded)
}

/// `FromValue`：深度优先查找 `$id`/`$anchor`/`$dynamicAnchor`/pointer 匹配的 schema。
fn resolve_ref_value(schema: &Value, base: &url::Url, absolute: &url::Url) -> Option<Value> {
    let next_base = match schema {
        Value::Object(map) => match map.get("$id").and_then(Value::as_str) {
            Some(id) => base.join(id).unwrap_or_else(|_| base.clone()),
            None => base.clone(),
        },
        _ => base.clone(),
    };
    if let Value::Object(map) = schema {
        if let Some(id) = map.get("$id").and_then(Value::as_str) {
            // `MatchId`：`$id` 是裸片段时直接匹配；否则比较文档路径。
            let ref_hash = match absolute.fragment() {
                Some(fragment) => format!("#{fragment}"),
                None => String::new(),
            };
            if id == ref_hash {
                return Some(schema.clone());
            }
            if next_base.path() == absolute.path() {
                if absolute.fragment().is_some() {
                    if let Some(found) = match_hash(schema, absolute) {
                        return Some(found);
                    }
                } else {
                    return Some(schema.clone());
                }
            }
        }
        for key in ["$anchor", "$dynamicAnchor"] {
            if let Some(anchor) = map.get(key).and_then(Value::as_str)
                && let Ok(anchor_url) = next_base.join(&format!("#{anchor}"))
                && anchor_url == *absolute
            {
                return Some(schema.clone());
            }
        }
        if let Some(found) = match_hash(schema, absolute) {
            return Some(found);
        }
        for (key, child) in map {
            if key == "const" || key == "enum" {
                continue;
            }
            if let Some(found) = resolve_ref_value(child, &next_base, absolute) {
                return Some(found);
            }
        }
        None
    } else if let Value::Array(items) = schema {
        for item in items {
            if let Some(found) = resolve_ref_value(item, &next_base, absolute) {
                return Some(found);
            }
        }
        None
    } else {
        None
    }
}

/// `Stack.Ref`：把 `$ref` 解析成目标 schema（`None` = 解析不到 → `false`）。
fn resolve_schema_ref(root: &Value, reference: &str) -> Option<Value> {
    let base = url::Url::parse(JSON_SCHEMA_DEFAULT_BASE).ok()?;
    let absolute = base.join(reference).ok()?;
    resolve_ref_value(root, &base, &absolute)
}

/// `$dynamicRef` 的兜底：按 `$dynamicAnchor` 名字在树里查找。
fn find_dynamic_anchor(schema: &Value, name: &str) -> Option<Value> {
    match schema {
        Value::Object(map) => {
            if map.get("$dynamicAnchor").and_then(Value::as_str) == Some(name) {
                return Some(schema.clone());
            }
            for child in map.values() {
                if let Some(found) = find_dynamic_anchor(child, name) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items
            .iter()
            .find_map(|item| find_dynamic_anchor(item, name)),
        _ => None,
    }
}

/// `CheckTypeName`（JS 值语义）。
fn type_name_matches(name: &str, value: &Value) -> bool {
    match name {
        "object" => is_js_object_not_array(value),
        "array" => is_js_array(value),
        "boolean" => is_js_boolean(value),
        "integer" => is_js_integer(value),
        "number" => is_js_number(value),
        "null" => value.is_null(),
        "string" => is_js_string(value),
        // xschema 类型在 JSON 里没有对应值。
        "bigint" | "constructor" | "function" | "symbol" | "undefined" | "void" => false,
        _ => true,
    }
}

/// `Guard.IsMultipleOf`（含 1e-10 容差与整数快捷路径）。
fn is_multiple_of(dividend: f64, divisor: f64) -> bool {
    if !dividend.is_finite() {
        return true;
    }
    if kiana_json::is_js_integer(dividend) && kiana_json::is_js_integer(1.0 / divisor) {
        return true;
    }
    let modulus = dividend % divisor;
    modulus.abs() < 1e-10 || (modulus - divisor).abs() < 1e-10 || (modulus + divisor).abs() < 1e-10
}

fn is_schema_object_keyword(schema: &Map<String, Value>, key: &str) -> bool {
    match schema.get(key) {
        Some(Value::Object(map)) => map.values().all(is_schema),
        _ => false,
    }
}

/// `IsDependencies`（`schema/types/dependencies.mjs`）：每个值可以是 schema，也可以是
/// **字符串数组**（draft-07 的 `{"a": ["b"]}` 形式）；两者混合也合法。
fn is_dependencies(schema: &Map<String, Value>) -> bool {
    match schema.get("dependencies") {
        Some(Value::Object(map)) => map.values().all(|value| {
            is_schema(value)
                || matches!(value, Value::Array(items) if items.iter().all(Value::is_string))
        }),
        _ => false,
    }
}

fn is_string_array_map_keyword(schema: &Map<String, Value>, key: &str) -> bool {
    match schema.get(key) {
        Some(Value::Object(map)) => map.values().all(|value| match value {
            Value::Array(items) => items.iter().all(Value::is_string),
            _ => false,
        }),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// uniqueItems 的值散列（`system/hashing/hash.mjs`，FNV1A-64）
// ---------------------------------------------------------------------------

const FNV_OFFSET: u64 = 14_695_981_039_346_656_037;
const FNV_PRIME: u64 = 1_099_511_628_211;

fn value_hash(value: &Value) -> String {
    let mut hash = FNV_OFFSET;
    hash_value(value, &mut hash);
    format!("{hash:016x}")
}

fn hash_bytes(bytes: &[u8], hash: &mut u64) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

/// 首字节是类型标签（`null`=6、`bool`=2、`number`=7、`string`=10、`array`=0、`object`=8），
/// 数字按 `f64` 的 little-endian 字节混入；对象键先按 UTF-16 码元排序再逐对混入，因此
/// 键序不影响结果。标签值是移植自 `system/hashing/hash.mjs` 的固定常量（见上方小节），
/// 改动会让 `uniqueItems` 的散列结果与参考实现不同。
fn hash_value(value: &Value, hash: &mut u64) {
    match value {
        Value::Null => hash_bytes(&[6], hash),
        Value::Bool(flag) => {
            hash_bytes(&[2], hash);
            hash_bytes(&[u8::from(*flag)], hash);
        }
        Value::Number(number) => {
            hash_bytes(&[7], hash);
            hash_bytes(&number.as_f64().unwrap_or(f64::NAN).to_le_bytes(), hash);
        }
        Value::String(text) => {
            hash_bytes(&[10], hash);
            hash_bytes(text.as_bytes(), hash);
        }
        Value::Array(items) => {
            hash_bytes(&[0], hash);
            for item in items {
                hash_value(item, hash);
            }
        }
        Value::Object(map) => {
            hash_bytes(&[8], hash);
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            for key in keys {
                hash_value(&Value::String(key.clone()), hash);
                hash_value(&map[key.as_str()], hash);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// format（`format/_registry.mjs` + 各 validator）
//
// 全部按 TypeBox 1.3.27 的源逐字移植：正则源码与 flags 原样保留（`new RegExp(source, flags)`，
// 由 `regress` 提供 ECMAScript 语义），非正则的校验逻辑逐行对照 `date.mjs`/`time.mjs`/
// `idna/*`/`iri.mjs`/`iri_reference.mjs`/`url.mjs`/`regex.mjs`。JS 的 `String.length` 是
// UTF-16 码元数，凡长度限制一律用 [`utf16_len`]。
// ---------------------------------------------------------------------------

/// `new RegExp(source, flags)`（JS 语义；非法时抛异常——对应 TypeBox 里未被捕获的 `SyntaxError`）。
fn js_regex(source: &str, unicode: bool, icase: bool) -> regress::Regex {
    let flags = regress::Flags {
        unicode,
        icase,
        ..Default::default()
    };
    match regress::Regex::with_flags(source, flags) {
        Ok(regex) => regex,
        Err(error) => panic!(
            "SyntaxError: Invalid regular expression: /{source}/{}: {}",
            flag_letters(unicode, icase),
            error.text
        ),
    }
}

fn flag_letters(unicode: bool, icase: bool) -> &'static str {
    match (unicode, icase) {
        (true, true) => "iu",
        (true, false) => "u",
        (false, true) => "i",
        (false, false) => "",
    }
}

/// 缓存一个静态正则（`OnceLock`；`regress::Regex` 是 `Sync`）。
fn cached_regex(
    cell: &'static OnceLock<regress::Regex>,
    source: &'static str,
    unicode: bool,
    icase: bool,
) -> &'static regress::Regex {
    cell.get_or_init(|| js_regex(source, unicode, icase))
}

/// `regexp.test(text)`：任意位置匹配。
fn regex_test(
    cell: &'static OnceLock<regress::Regex>,
    source: &'static str,
    unicode: bool,
    icase: bool,
    text: &str,
) -> bool {
    cached_regex(cell, source, unicode, icase)
        .find(text)
        .is_some()
}

/// JS `String.length`：UTF-16 码元数。
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `Format.Test(format, value)`：未知 format 一律通过（TypeBox 的 `?? true`）。
///
/// format 名区分大小写；内置集合就是下面 `match` 列出的那些（date/date-time/duration/
/// email/hostname/idn-email/idn-hostname/ipv4/ipv6/iri/iri-reference/json-pointer/
/// json-pointer-uri-fragment/regex/relative-json-pointer/time/uri/uri-reference/
/// uri-template/url/uuid）。`regex` 以"JS `u` 模式能编译"为合法，`url` 用
/// `url::Url::parse`，`idn-email` 先做 NFC 规范化，其余是逐字移植的正则或逐行移植的
/// 解析逻辑。
pub fn format_test(format: &str, value: &str) -> bool {
    match format {
        "date" => is_format_date(value),
        "date-time" => is_format_date_time(value),
        "duration" => regex_test(&DURATION, DURATION_SOURCE, false, false, value),
        "email" => is_format_email(value),
        "hostname" => is_format_hostname(value),
        "idn-email" => is_format_idn_email(value),
        "idn-hostname" => is_format_idn_hostname(value),
        "ipv4" => regex_test(&IPV4, IPV4_SOURCE, false, false, value),
        "ipv6" => regex_test(&IPV6, IPV6_SOURCE, false, true, value),
        "iri" => is_format_iri(value),
        "iri-reference" => is_format_iri_reference(value),
        "json-pointer" => regex_test(&JSON_POINTER, JSON_POINTER_SOURCE, false, false, value),
        "json-pointer-uri-fragment" => regex_test(
            &JSON_POINTER_URI_FRAGMENT,
            JSON_POINTER_URI_FRAGMENT_SOURCE,
            false,
            true,
            value,
        ),
        "regex" => compile_pattern(value).is_some(),
        "relative-json-pointer" => regex_test(
            &RELATIVE_JSON_POINTER,
            RELATIVE_JSON_POINTER_SOURCE,
            false,
            false,
            value,
        ),
        "time" => is_format_time(value),
        "uri" => regex_test(&URI, URI_SOURCE, false, true, value),
        "uri-reference" => regex_test(&URI_REFERENCE, URI_REFERENCE_SOURCE, false, true, value),
        "uri-template" => regex_test(&URI_TEMPLATE, URI_TEMPLATE_SOURCE, false, true, value),
        "url" => is_format_url(value),
        "uuid" => regex_test(&UUID, UUID_SOURCE, false, true, value),
        _ => true,
    }
}

// 逐字照抄 TypeBox 的 `format/*.mjs`（源码与 flags）。
/// `duration.Duration`（`no flags`）。
const DURATION_SOURCE: &str = r#"^P((\d+Y(\d+M(\d+D)?)?|\d+M(\d+D)?|\d+D)(T(\d+H(\d+M(\d+S)?)?|\d+M(\d+S)?|\d+S))?|T(\d+H(\d+M(\d+S)?)?|\d+M(\d+S)?|\d+S)|\d+W)$"#;
/// `email.Email`（`i`）。
const EMAIL_SOURCE: &str = r#"^(?:[a-z0-9!#$%&'*+/=?^_`{|}~-]+(?:\.[a-z0-9!#$%&'*+/=?^_`{|}~-]+)*|"(?:[^"\\]|\\[\x20-\x7e])*")@(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)*|\[(?:IPv6:[a-f0-9:]+|(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])(?:\.(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])){3})\])$"#;
/// `idn_email.IdnEmail`（`iu`）。
const IDN_EMAIL_SOURCE: &str = r#"^(?:[A-Za-z0-9!#$%&'*+\/=?^_`{|}~\u{0080}-\u{10FFFF}-]+(?:\.[A-Za-z0-9!#$%&'*+\/=?^_`{|}~\u{0080}-\u{10FFFF}-]+)*|"(?:[^"\\]|\\.)*")@[\p{L}\p{N}](?:[\p{L}\p{N}-]{0,62})(?<!-)(?:\.[\p{L}\p{N}](?:[\p{L}\p{N}-]{0,62})(?<!-))*$"#;
/// `ipv4.IPv4`（`no flags`）。
const IPV4_SOURCE: &str =
    r#"^(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)$"#;
/// `ipv6.IPv6`（`i`）。
const IPV6_SOURCE: &str = r#"^(?:(?:(?:[0-9a-f]{1,4}:){6}|::(?:[0-9a-f]{1,4}:){5}|(?:[0-9a-f]{1,4})?::(?:[0-9a-f]{1,4}:){4}|(?:(?:[0-9a-f]{1,4}:)?[0-9a-f]{1,4})?::(?:[0-9a-f]{1,4}:){3}|(?:(?:[0-9a-f]{1,4}:){0,2}[0-9a-f]{1,4})?::(?:[0-9a-f]{1,4}:){2}|(?:(?:[0-9a-f]{1,4}:){0,3}[0-9a-f]{1,4})?::[0-9a-f]{1,4}:|(?:(?:[0-9a-f]{1,4}:){0,4}[0-9a-f]{1,4})?::)(?:[0-9a-f]{1,4}:[0-9a-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[0-9a-f]{1,4}:){0,5}[0-9a-f]{1,4})?::[0-9a-f]{1,4}|(?:(?:[0-9a-f]{1,4}:){0,6}[0-9a-f]{1,4})?::)$"#;
/// `json_pointer.JsonPointer`（`no flags`）。
const JSON_POINTER_SOURCE: &str = r#"^(?:\/(?:[^~/]|~0|~1)*)*$"#;
/// `json_pointer_uri_fragment.JsonPointerUriFragment`（`i`）。
const JSON_POINTER_URI_FRAGMENT_SOURCE: &str =
    r#"^#(?:\/(?:[a-z0-9_\-.!$&'()*+,;:=@]|%[0-9a-f]{2}|~0|~1)*)*$"#;
/// `relative_json_pointer.RelativeJsonPointer`（`no flags`）。
const RELATIVE_JSON_POINTER_SOURCE: &str = r#"^(?:0|[1-9][0-9]*)(?:#|(?:\/(?:[^~/]|~0|~1)*)*)$"#;
/// `uri.Uri`（`i`）。
const URI_SOURCE: &str = r#"^[a-z][a-z0-9+\-.]*:(?:\/\/(?:(?:[-a-z0-9._~!$&'()*+,;=:]|%[0-9a-f]{2})*@)?(?:\[(?:(?:(?:[\da-f]{1,4}:){6}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|::(?:[\da-f]{1,4}:){5}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:[\da-f]{1,4})?::(?:[\da-f]{1,4}:){4}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,1}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){3}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,2}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){2}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,3}[\da-f]{1,4})?::[\da-f]{1,4}:(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,4}[\da-f]{1,4})?::(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,5}[\da-f]{1,4})?::[\da-f]{1,4}|(?:(?:[\da-f]{1,4}:){0,6}[\da-f]{1,4})?::)|v[0-9a-f]+\.[-a-z0-9._~!$&'()*+,;=:]+)\]|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)|(?:[-a-z0-9._~!$&'()*+,;=]|%[0-9a-f]{2})*)(?::\d*)?(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*|\/(?:(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?|(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?(?:\?(?:[-a-z0-9._~!$&'()*+,;=:@/?]|%[0-9a-f]{2})*)?(?:#(?:[-a-z0-9._~!$&'()*+,;=:@/?]|%[0-9a-f]{2})*)?$"#;
/// `uri_reference.UriReference`（`i`）。
const URI_REFERENCE_SOURCE: &str = r#"^(?:[a-z][a-z0-9+\-.]*:(?:\/\/(?:(?:[-a-z0-9._~!$&'()*+,;=:]|%[0-9a-f]{2})*@)?(?:\[(?:(?:(?:[\da-f]{1,4}:){6}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|::(?:[\da-f]{1,4}:){5}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:[\da-f]{1,4})?::(?:[\da-f]{1,4}:){4}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,1}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){3}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,2}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){2}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,3}[\da-f]{1,4})?::[\da-f]{1,4}:(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,4}[\da-f]{1,4})?::(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,5}[\da-f]{1,4})?::[\da-f]{1,4}|(?:(?:[\da-f]{1,4}:){0,6}[\da-f]{1,4})?::)|v[0-9a-f]+\.[-a-z0-9._~!$&'()*+,;=:]+)\]|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)|(?:[-a-z0-9._~!$&'()*+,;=]|%[0-9a-f]{2})*)(?::\d*)?(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*|\/(?:(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?|(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?|(?:\/\/(?:(?:[-a-z0-9._~!$&'()*+,;=:]|%[0-9a-f]{2})*@)?(?:\[(?:(?:(?:[\da-f]{1,4}:){6}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|::(?:[\da-f]{1,4}:){5}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:[\da-f]{1,4})?::(?:[\da-f]{1,4}:){4}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,1}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){3}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,2}[\da-f]{1,4})?::(?:[\da-f]{1,4}:){2}(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,3}[\da-f]{1,4})?::[\da-f]{1,4}:(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,4}[\da-f]{1,4})?::(?:[\da-f]{1,4}:[\da-f]{1,4}|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d))|(?:(?:[\da-f]{1,4}:){0,5}[\da-f]{1,4})?::[\da-f]{1,4}|(?:(?:[\da-f]{1,4}:){0,6}[\da-f]{1,4})?::)|v[0-9a-f]+\.[-a-z0-9._~!$&'()*+,;=:]+)\]|(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)|(?:[-a-z0-9._~!$&'()*+,;=]|%[0-9a-f]{2})*)(?::\d*)?(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*|\/(?:(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})+(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?|(?:[-a-z0-9._~!$&'()*+,;=@]|%[0-9a-f]{2})+(?:\/(?:[-a-z0-9._~!$&'()*+,;=:@]|%[0-9a-f]{2})*)*)?)(?:\?(?:[-a-z0-9._~!$&'()*+,;=:@/?]|%[0-9a-f]{2})*)?(?:#(?:[-a-z0-9._~!$&'()*+,;=:@/?]|%[0-9a-f]{2})*)?$"#;
/// `uri_template.UriTemplate`（`i`）。
const URI_TEMPLATE_SOURCE: &str = r#"^(?:(?:[^\x00-\x20"<>%\\^`{|}\x7f]|%[0-9a-f]{2})|\{[+#./;?&=,!@|]?(?:[a-z0-9_]|%[0-9a-f]{2})+(?:\.(?:[a-z0-9_]|%[0-9a-f]{2})+)*(?::[1-9]\d{0,3}|\*)?(?:,(?:[a-z0-9_]|%[0-9a-f]{2})+(?:\.(?:[a-z0-9_]|%[0-9a-f]{2})+)*(?::[1-9]\d{0,3}|\*)?)*\})*$"#;
/// `uuid.Uuid`（`i`）。
const UUID_SOURCE: &str = r#"^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$"#;
/// `iri.InvalidIriChars`（`no flags`）。
const IRI_INVALID_CHARS_SOURCE: &str = r#"[\x00-\x20<>\^`{|}\\]"#;
/// `iri.InvalidPercentEncoding`（`no flags`）。
const IRI_INVALID_PERCENT_SOURCE: &str = r#"%(?![0-9a-fA-F]{2})"#;
/// `iri.IpvFutureMatch`（`no flags`）。
const IPV_FUTURE_MATCH_SOURCE: &str = r#"\[[vV][0-9a-fA-F]+\.[^\]]+\]"#;
/// `iri_reference.InvalidIriChars`（`no flags`）。
const IRI_REFERENCE_INVALID_CHARS_SOURCE: &str = r#"[\x00-\x20\x7F\\]|%(?![0-9a-fA-F]{2})"#;
/// `iri_reference.MalformedScheme`（`no flags`）。
const MALFORMED_SCHEME_SOURCE: &str = r#"^[a-zA-Z][a-zA-Z0-9+\-.]*\/\/"#;

// ── 缓存槽 ──────────────────────────────────────────────────────────────────

static DURATION: OnceLock<regress::Regex> = OnceLock::new();
static EMAIL: OnceLock<regress::Regex> = OnceLock::new();
static IDN_EMAIL: OnceLock<regress::Regex> = OnceLock::new();
static IPV4: OnceLock<regress::Regex> = OnceLock::new();
static IPV6: OnceLock<regress::Regex> = OnceLock::new();
static JSON_POINTER: OnceLock<regress::Regex> = OnceLock::new();
static JSON_POINTER_URI_FRAGMENT: OnceLock<regress::Regex> = OnceLock::new();
static RELATIVE_JSON_POINTER: OnceLock<regress::Regex> = OnceLock::new();
static URI: OnceLock<regress::Regex> = OnceLock::new();
static URI_REFERENCE: OnceLock<regress::Regex> = OnceLock::new();
static URI_TEMPLATE: OnceLock<regress::Regex> = OnceLock::new();
static UUID: OnceLock<regress::Regex> = OnceLock::new();
static IRI_INVALID_CHARS: OnceLock<regress::Regex> = OnceLock::new();
static IRI_INVALID_PERCENT: OnceLock<regress::Regex> = OnceLock::new();
static IPV_FUTURE_MATCH: OnceLock<regress::Regex> = OnceLock::new();
static IRI_REFERENCE_INVALID_CHARS: OnceLock<regress::Regex> = OnceLock::new();
static MALFORMED_SCHEME: OnceLock<regress::Regex> = OnceLock::new();

// ── date / time / date-time ────────────────────────────────────────────────

fn is_ascii_digit(byte: u8) -> bool {
    byte.is_ascii_digit()
}

fn digits(bytes: &[u8], start: usize, count: usize) -> Option<u32> {
    let slice = bytes.get(start..start + count)?;
    if !slice.iter().all(|b| is_ascii_digit(*b)) {
        return None;
    }
    std::str::from_utf8(slice).ok()?.parse().ok()
}

/// `IsDate`（`format/date.mjs`）：`DATE` 正则 + 闰年天数表。
fn is_format_date(value: &str) -> bool {
    // `^(\d\d\d\d)-(\d\d)-(\d\d)$`：非 u 模式下 `\d` 是 ASCII 数字，长度与分隔符固定，
    // 因此逐字节判定与 `DATE.exec` 等价。
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let (Some(year), Some(month), Some(day)) = (
        digits(bytes, 0, 4),
        digits(bytes, 5, 2),
        digits(bytes, 8, 2),
    ) else {
        return false;
    };
    if !(1..=12).contains(&month) || day < 1 {
        return false;
    }
    let days = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max = if month == 2 && leap {
        29
    } else {
        days[month as usize]
    };
    day <= max
}

/// `IsTime`（`format/time.mjs`，`strictTimeZone = true`）。
fn is_format_time(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 8 || bytes[2] != b':' || bytes[5] != b':' {
        return false;
    }
    let (Some(hr), Some(min), Some(sec)) = (
        digits(bytes, 0, 2),
        digits(bytes, 3, 2),
        digits(bytes, 6, 2),
    ) else {
        return false;
    };
    let mut rest = &bytes[8..];
    // 可选的小数秒：`.` 后必须至少一位 ASCII 数字，多余的位数不限制。
    if let Some(stripped) = rest.strip_prefix(b".") {
        let end = stripped
            .iter()
            .position(|b| !is_ascii_digit(*b))
            .unwrap_or(stripped.len());
        if end == 0 {
            return false;
        }
        rest = &stripped[end..];
    }
    let mut tz_sign: i32 = 1;
    let mut tz_present = false;
    let mut tz_h = 0u32;
    let mut tz_m = 0u32;
    // 时区：`Z`/`z` 或 `±HH:MM`；strictTimeZone 要求必须出现。
    if !rest.is_empty() {
        if rest == b"Z" || rest == b"z" {
            tz_present = true;
        } else if rest.len() == 6 && (rest[0] == b'+' || rest[0] == b'-') && rest[3] == b':' {
            let (Some(h), Some(m)) = (digits(rest, 1, 2), digits(rest, 4, 2)) else {
                return false;
            };
            tz_sign = if rest[0] == b'-' { -1 } else { 1 };
            tz_h = h;
            tz_m = m;
            tz_present = true;
        } else {
            return false;
        }
    }
    if !tz_present {
        return false; // strictTimeZone
    }
    if hr > 23 || min > 59 || sec > 60 {
        return false;
    }
    if tz_h > 23 || tz_m > 59 {
        return false;
    }
    if sec < 60 {
        return true;
    }
    // 闰秒：必须折算到 23:59:60 UTC（1439 UTC 分钟）。
    let total = (hr as i32 * 60 + min as i32) - tz_sign * (tz_h as i32 * 60 + tz_m as i32);
    (total % 1440 + 1440) % 1440 == 1439
}

/// `IsDateTime`：`value.split(/T/i)` 长度 2 且两半分别是 date 与 time。
fn is_format_date_time(value: &str) -> bool {
    let parts: Vec<&str> = value.split(['T', 't']).collect();
    parts.len() == 2 && is_format_date(parts[0]) && is_format_time(parts[1])
}

/// `IsEmail`。
fn is_format_email(value: &str) -> bool {
    regex_test(&EMAIL, EMAIL_SOURCE, false, true, value)
}

/// `IsIdnEmail`：`value.normalize('NFC')` 后跑 `/iu` 正则。
fn is_format_idn_email(value: &str) -> bool {
    let normalized: String = value.nfc().collect();
    regex_test(&IDN_EMAIL, IDN_EMAIL_SOURCE, true, true, &normalized)
}

/// `IsUrl`（`URL.canParse`）：`url` crate 的解析即 WHATWG URL。
fn is_format_url(value: &str) -> bool {
    url::Url::parse(value).is_ok()
}

// ── iri / iri-reference（`format/iri.mjs` / `iri_reference.mjs`） ──────────────

/// `IpvFutureMatchMaxLength`。
const IPV_FUTURE_MATCH_MAX_LENGTH: usize = 2048;

/// `NarrowIpvFuture`：把首个 IPvFuture 字面量换成 `[::1]`（`String.replace` 无 `g` 只换一处）。
fn narrow_ipv_future(value: &str) -> String {
    if utf16_len(value) >= IPV_FUTURE_MATCH_MAX_LENGTH {
        return value.to_owned();
    }
    let regex = cached_regex(&IPV_FUTURE_MATCH, IPV_FUTURE_MATCH_SOURCE, false, false);
    match regex.find(value) {
        Some(found) => {
            let mut out = String::with_capacity(value.len());
            out.push_str(&value[..found.start()]);
            out.push_str("[::1]");
            out.push_str(&value[found.end()..]);
            out
        }
        None => value.to_owned(),
    }
}

/// `IsIri`。
fn is_format_iri(value: &str) -> bool {
    if regex_test(
        &IRI_INVALID_CHARS,
        IRI_INVALID_CHARS_SOURCE,
        false,
        false,
        value,
    ) {
        return false;
    }
    if regex_test(
        &IRI_INVALID_PERCENT,
        IRI_INVALID_PERCENT_SOURCE,
        false,
        false,
        value,
    ) {
        return false;
    }
    url::Url::parse(&narrow_ipv_future(value)).is_ok()
}

/// `IsIriReference`：`URL.canParse(value, 'http://example.com')`。
fn is_format_iri_reference(value: &str) -> bool {
    if regex_test(
        &IRI_REFERENCE_INVALID_CHARS,
        IRI_REFERENCE_INVALID_CHARS_SOURCE,
        false,
        false,
        value,
    ) {
        return false;
    }
    if regex_test(
        &MALFORMED_SCHEME,
        MALFORMED_SCHEME_SOURCE,
        false,
        false,
        value,
    ) {
        return false;
    }
    let Ok(base) = url::Url::parse("http://example.com") else {
        return false;
    };
    url::Url::options()
        .base_url(Some(&base))
        .parse(value)
        .is_ok()
}

// ── idna（`format/idna/*`，逐行移植） ────────────────────────────────────────

const RE_NON_ASCII_SOURCE: &str = r#"[^\p{ASCII}]"#;
const RE_COMBINING_MARK_SOURCE: &str = r#"[\p{Mn}\p{Mc}\p{Me}]"#;
const RE_RFC5892_DISALLOWED_SOURCE: &str =
    r#"[\u{0640}\u{07fa}\u{302e}\u{302f}\u{3031}\u{3032}\u{3033}\u{3034}\u{3035}\u{303b}]"#;
const RE_VIRAMA_SOURCE: &str = r#"[\u{094d}\u{09cd}\u{0a4d}\u{0acd}\u{0b4d}\u{0bcd}\u{0c4d}\u{0ccd}\u{0d3b}\u{0d3c}\u{0d4d}\u{0dca}\u{1b44}\u{1baa}\u{1bab}\u{a9c0}\u{11046}\u{1107f}\u{110b9}\u{11133}\u{11134}\u{111c0}\u{11235}\u{1134d}\u{11442}\u{114c2}\u{115bf}\u{1163f}\u{116b6}\u{11c3f}\u{11d44}\u{11d45}]"#;
const RE_SCRIPT_GREEK_SOURCE: &str = r#"\p{Script=Greek}"#;
const RE_SCRIPT_HEBREW_SOURCE: &str = r#"\p{Script=Hebrew}"#;
const RE_SCRIPT_JAPANESE_SOURCE: &str = r#"[\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Han}]"#;
const RE_SCRIPT_ARABIC_LETTER_SOURCE: &str =
    r#"[\p{Script=Arabic}\p{Script=Syriac}\p{Script=Thaana}\p{Script=Mandaic}]"#;
const RE_LETTER_SOURCE: &str = r#"\p{L}"#;
const RE_MARK_NONSPACING_SOURCE: &str = r#"\p{Mn}"#;
const RE_ARABIC_INDIC_DIGIT_SOURCE: &str = r#"[\u{0660}-\u{0669}]"#;

/// `RE_EUROPEAN_NUMBER`：`[0-9]|[\u{06f0}-\u{06f9}]`（`u`）。
const RE_EUROPEAN_NUMBER_SOURCE: &str = r#"[0-9]|[\u{06f0}-\u{06f9}]"#;
/// `RE_PERMITTED_CATEGORY`：八个源码用 `|` 连接（`u`）。
const RE_PERMITTED_CATEGORY_SOURCE: &str = r#"\p{L}|[\u{002d}\u{002b}]|[\u{002e}\u{002c}\u{003a}\u{002f}]|\p{Nd}|\p{Mn}|\p{Mc}|[\u{00b7}\u{0375}\u{05f3}\u{05f4}\u{200c}\u{200d}\u{30fb}]|[\u{00df}\u{03c2}\u{06fd}\u{06fe}\u{0f0b}\u{3007}]"#;

static RE_NON_ASCII: OnceLock<regress::Regex> = OnceLock::new();
static RE_COMBINING_MARK: OnceLock<regress::Regex> = OnceLock::new();
static RE_RFC5892_DISALLOWED: OnceLock<regress::Regex> = OnceLock::new();
static RE_VIRAMA: OnceLock<regress::Regex> = OnceLock::new();
static RE_SCRIPT_GREEK: OnceLock<regress::Regex> = OnceLock::new();
static RE_SCRIPT_HEBREW: OnceLock<regress::Regex> = OnceLock::new();
static RE_SCRIPT_JAPANESE: OnceLock<regress::Regex> = OnceLock::new();
static RE_SCRIPT_ARABIC_LETTER: OnceLock<regress::Regex> = OnceLock::new();
static RE_LETTER: OnceLock<regress::Regex> = OnceLock::new();
static RE_MARK_NONSPACING: OnceLock<regress::Regex> = OnceLock::new();
static RE_ARABIC_INDIC_DIGIT: OnceLock<regress::Regex> = OnceLock::new();
static RE_EUROPEAN_NUMBER: OnceLock<regress::Regex> = OnceLock::new();
static RE_PERMITTED_CATEGORY: OnceLock<regress::Regex> = OnceLock::new();

/// 单字符（码点）的正则测试：`regexp.test(String.fromCodePoint(cp))`。
fn char_test(cell: &'static OnceLock<regress::Regex>, source: &'static str, ch: char) -> bool {
    let mut buffer = [0u8; 4];
    regex_test(cell, source, true, false, ch.encode_utf8(&mut buffer))
}

/// `RE_NON_ASCII.test(value)`。
fn has_non_ascii(value: &str) -> bool {
    regex_test(&RE_NON_ASCII, RE_NON_ASCII_SOURCE, true, false, value)
}

/// `RE_COMBINING_MARK.test(value)`（与 `IsCombiningMark` 的码点范围判定无关）。
fn is_combining_mark_str(value: &str) -> bool {
    regex_test(
        &RE_COMBINING_MARK,
        RE_COMBINING_MARK_SOURCE,
        true,
        false,
        value,
    )
}

fn is_virama(value: &str) -> bool {
    regex_test(&RE_VIRAMA, RE_VIRAMA_SOURCE, true, false, value)
}

fn is_script_greek(value: &str) -> bool {
    regex_test(&RE_SCRIPT_GREEK, RE_SCRIPT_GREEK_SOURCE, true, false, value)
}

fn is_script_hebrew(value: &str) -> bool {
    regex_test(
        &RE_SCRIPT_HEBREW,
        RE_SCRIPT_HEBREW_SOURCE,
        true,
        false,
        value,
    )
}

fn is_permitted_category(ch: char) -> bool {
    char_test(&RE_PERMITTED_CATEGORY, RE_PERMITTED_CATEGORY_SOURCE, ch)
}

fn is_rfc5892_disallowed(ch: char) -> bool {
    char_test(&RE_RFC5892_DISALLOWED, RE_RFC5892_DISALLOWED_SOURCE, ch)
}

fn is_script_japanese(ch: char) -> bool {
    char_test(&RE_SCRIPT_JAPANESE, RE_SCRIPT_JAPANESE_SOURCE, ch)
}

fn is_letter(ch: char) -> bool {
    char_test(&RE_LETTER, RE_LETTER_SOURCE, ch)
}

fn is_mark_nonspacing(ch: char) -> bool {
    char_test(&RE_MARK_NONSPACING, RE_MARK_NONSPACING_SOURCE, ch)
}

fn is_script_hebrew_char(ch: char) -> bool {
    char_test(&RE_SCRIPT_HEBREW, RE_SCRIPT_HEBREW_SOURCE, ch)
}

fn is_script_arabic_letter_char(ch: char) -> bool {
    char_test(&RE_SCRIPT_ARABIC_LETTER, RE_SCRIPT_ARABIC_LETTER_SOURCE, ch)
}

fn is_european_number(ch: char) -> bool {
    char_test(&RE_EUROPEAN_NUMBER, RE_EUROPEAN_NUMBER_SOURCE, ch)
}

fn is_arabic_indic_digit(ch: char) -> bool {
    char_test(&RE_ARABIC_INDIC_DIGIT, RE_ARABIC_INDIC_DIGIT_SOURCE, ch)
}

/// `IsAcePrefixed(value)`：`value.toLowerCase().startsWith("xn--")`。
fn is_ace_prefixed(value: &str) -> bool {
    value
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("xn--"))
}

/// `NormalizeHostname`（`idna/idn-hostname.mjs`）：全角映射 → NFC → 删除被忽略码点 → 全角句点。
fn normalize_hostname(value: &str) -> String {
    // RE_FULLWIDTH_MAPPING：`[！-～]` → charCode - 0xfee0。
    let mapped: String = value
        .chars()
        .map(|ch| {
            let code = ch as u32;
            if (0xFF01..=0xFF5E).contains(&code) {
                char::from_u32(code - 0xFEE0).unwrap_or(ch)
            } else {
                ch
            }
        })
        .collect();
    let normalized: String = mapped.nfc().collect();
    // RE_IGNORED_MAPPING：`[­͏᠋-᠍​︀-️\u{e0100}-\u{e01ef}]`。
    let filtered: String = normalized
        .chars()
        .filter(|ch| !is_ignored_mapping(*ch))
        .collect();
    // RE_FULL_STOP_MAPPING：`[.。．｡]` → '.'。
    filtered
        .chars()
        .map(|ch| match ch {
            '\u{002E}' | '\u{3002}' | '\u{FF0E}' | '\u{FF61}' => '.',
            other => other,
        })
        .collect()
}

fn is_ignored_mapping(ch: char) -> bool {
    matches!(
        ch,
        '\u{00AD}' | '\u{034F}' | '\u{180B}'..='\u{180D}' | '\u{200B}' | '\u{FE00}'..='\u{FE0F}'
            | '\u{E0100}'..='\u{E01EF}'
    )
}

/// `IsHostname`（`idna/hostname.mjs`）。
fn is_format_hostname(value: &str) -> bool {
    let length = utf16_len(value);
    if length == 0 || length > 253 {
        return false;
    }
    if value.ends_with('.') {
        return false;
    }
    value.split('.').all(is_hostname_label)
}

fn is_hostname_label(value: &str) -> bool {
    let length = utf16_len(value);
    length > 0 && length <= 63 && (is_puny_label(value) || is_ascii_label(value))
}

/// `IsAsciiLabel`（`label/ascii.mjs`）：三条规则的 `&&`。
fn is_ascii_label(value: &str) -> bool {
    // RE_RULE_HYPHEN_PLACEMENT `^(?!-).*(?<!-)$`：不以 '-' 开头或结尾。`.` 不匹配行终止符，
    // 但 RE_ASCII_LDH 已排除它们，最终结论相同。
    if value.starts_with('-') || value.ends_with('-') {
        return false;
    }
    // RE_RULE_NOT_RESERVED_ACE `^(?!..--)`：第 3、4 个字符不能是 `--`。
    let chars: Vec<char> = value.chars().collect();
    if chars.len() >= 4 && chars[2] == '-' && chars[3] == '-' {
        return false;
    }
    // RE_ASCII_LDH `^[a-zA-Z0-9-]*$`。
    chars
        .iter()
        .all(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
}

/// `IsPunyLabel`（`label/puny.mjs`）。
fn is_puny_label(value: &str) -> bool {
    if !is_ace_prefixed(value) {
        return false;
    }
    let Some(rest) = value.get(4..) else {
        return false;
    };
    let body = rest.to_lowercase();
    // 载荷只有分隔符、或以分隔符开头（如 `-9uc`）：前面 0 个基本 ASCII 字符，非法。
    if body.rfind('-') == Some(0) {
        return false;
    }
    let Some(decoded) = puny_decode(&body) else {
        return false;
    };
    if !has_non_ascii(&decoded) {
        return false;
    }
    is_unicode_label(&decoded)
}

/// `ExceedsMaxALabelLength`：`RE_NON_ASCII.test(value) && Encode(value).length + 4 > 63`。
fn exceeds_max_a_label_length(value: &str) -> bool {
    has_non_ascii(value) && puny_encode(value).len() + 4 > 63
}

/// `HasInvalidHyphens(chars)`。
fn has_invalid_hyphens(chars: &[char]) -> bool {
    if chars.first() == Some(&'-') || chars.last() == Some(&'-') {
        return true;
    }
    chars.len() >= 4 && chars[2] == '-' && chars[3] == '-'
}

/// `IsUnicodeLabel`（`label/unicode.mjs`）。
fn is_unicode_label(value: &str) -> bool {
    if exceeds_max_a_label_length(value) {
        return false;
    }
    if has_right_to_left_characters(value) && !satisfies_bidi_rule(value) {
        return false;
    }
    let chars: Vec<char> = value.chars().collect();
    let code_points: Vec<u32> = chars.iter().map(|ch| *ch as u32).collect();
    if has_invalid_hyphens(&chars) {
        return false;
    }
    if let Some(first) = chars.first()
        && is_combining_mark_str(&first.to_string())
    {
        return false;
    }
    let mut has_japanese = false;
    for (index, ch) in chars.iter().enumerate() {
        let code_point = code_points[index];
        // 1. DISALLOWED 例外。
        if is_rfc5892_disallowed(*ch) {
            return false;
        }
        // 1b. 通用类别（PVALID）约束。
        if !is_permitted_category(*ch) {
            return false;
        }
        // 2. 收集标记。
        if is_script_japanese(*ch) {
            has_japanese = true;
        }
        // 3. CONTEXTO / CONTEXTJ 邻接规则。
        let prev = index.checked_sub(1).map(|previous| code_points[previous]);
        let next = code_points.get(index + 1).copied();
        match code_point {
            0x00B7 => {
                // MIDDLE DOT（加泰罗尼亚语）：前后必须是 'l'。
                if prev != Some(0x006C) || next != Some(0x006C) {
                    return false;
                }
            }
            0x0375 => {
                // Greek KERAIA：后面必须是希腊文字母。
                let Some(next_index) = next.map(|_| index + 1) else {
                    return false;
                };
                if !is_script_greek(&chars[next_index].to_string()) {
                    return false;
                }
            }
            0x05F3 | 0x05F4 => {
                // Hebrew GERESH：前面必须是希伯来文字母。
                let Some(previous) = prev else {
                    return false;
                };
                let _ = previous;
                if !is_script_hebrew(&chars[index - 1].to_string()) {
                    return false;
                }
            }
            0x200C => {
                // ZWNJ：仅拒绝前面是 ASCII 且不是 virama 的情形（其余需要 joining type 表）。
                match prev {
                    None => return false,
                    Some(previous) if previous < 0x0080 => {
                        if !is_virama(&chars[index - 1].to_string()) {
                            return false;
                        }
                    }
                    Some(_) => {}
                }
            }
            0x200D => {
                // ZWJ：前面必须是 virama。
                if prev.is_none() || !is_virama(&chars[index - 1].to_string()) {
                    return false;
                }
            }
            0x30FB => {} // KATAKANA MIDDLE DOT：循环后统一检查。
            _ => {}
        }
    }
    // 4. 全局上下文校验。
    if value.contains('\u{30FB}') && !has_japanese {
        return false;
    }
    true
}

/// `IsIdnHostname`（`idna/idn-hostname.mjs`）。
fn is_format_idn_hostname(value: &str) -> bool {
    if value.is_empty() || value.contains(' ') {
        return false;
    }
    let normalized = normalize_hostname(value);
    if utf16_len(&normalized) > 253 {
        return false;
    }
    let labels: Vec<&str> = normalized.split('.').collect();
    let has_bidi_chars = labels.iter().any(|label| has_bidi_chars(label));
    labels
        .iter()
        .all(|label| is_idn_label(label) && (!has_bidi_chars || satisfies_bidi_rule(label)))
}

fn is_idn_label(value: &str) -> bool {
    let length = utf16_len(value);
    length > 0 && length <= 63 && (is_puny_label(value) || is_unicode_label(value))
}

// ── Punycode（`idna/format/puny.mjs`，RFC 3492） ─────────────────────────────

const PUNYCODE_BASE: f64 = 36.0;
const PUNYCODE_TMIN: f64 = 1.0;
const PUNYCODE_TMAX: f64 = 26.0;
const PUNYCODE_SKEW: f64 = 38.0;
const PUNYCODE_DAMP: f64 = 700.0;
const PUNYCODE_INITIAL_BIAS: f64 = 72.0;
const PUNYCODE_INITIAL_N: f64 = 128.0;

/// JS 的 `ToInt32`（`>>` 运算符的转换）。
fn js_to_int32(value: f64) -> i64 {
    if !value.is_finite() {
        return 0;
    }
    let truncated = value.trunc();
    let modulo = truncated % 4_294_967_296.0;
    let modulo = if modulo < 0.0 {
        modulo + 4_294_967_296.0
    } else {
        modulo
    };
    if modulo >= 2_147_483_648.0 {
        modulo as i64 - 4_294_967_296
    } else {
        modulo as i64
    }
}

/// `delta >> 1`（JS 的 32 位按位语义）。
fn js_shift_right_one(value: f64) -> f64 {
    (js_to_int32(value) >> 1) as f64
}

/// `Adapt`。
fn puny_adapt(delta: f64, num_points: f64, first_time: bool) -> f64 {
    let mut delta = if first_time {
        (delta / PUNYCODE_DAMP).floor()
    } else {
        js_shift_right_one(delta)
    };
    delta += (delta / num_points).floor();
    let mut k = 0.0;
    while delta > (((PUNYCODE_BASE - PUNYCODE_TMIN) * PUNYCODE_TMAX) / 2.0).floor() {
        delta = (delta / (PUNYCODE_BASE - PUNYCODE_TMIN)).floor();
        k += PUNYCODE_BASE;
    }
    k + (((PUNYCODE_BASE - PUNYCODE_TMIN + 1.0) * delta) / (delta + PUNYCODE_SKEW)).floor()
}

/// `Decode`：`Err(())` 对应 `ThrowDontCare`（TypeBox 的 `catch { return false }`）。
fn puny_decode(value: &str) -> Option<String> {
    let units: Vec<u32> = value.encode_utf16().map(u32::from).collect();
    let mut output: Vec<u32> = Vec::new();
    let mut n = PUNYCODE_INITIAL_N;
    let mut i = 0.0f64;
    let mut bias = PUNYCODE_INITIAL_BIAS;
    let delim_index: Option<usize> = units.iter().rposition(|unit| *unit == 0x2D);
    if let Some(delim) = delim_index
        && delim > 0
    {
        for unit in &units[..delim] {
            let cp = *unit;
            if cp >= 128 {
                return None;
            }
            output.push(cp);
        }
    }
    let mut in_index = match delim_index {
        Some(delim) => delim + 1,
        None => 0,
    };
    while in_index < units.len() {
        let old_i = i;
        let mut w = 1.0f64;
        let mut k = PUNYCODE_BASE;
        loop {
            if in_index >= units.len() {
                return None;
            }
            let ch = units[in_index];
            in_index += 1;
            let digit = if (0x61..=0x7A).contains(&ch) {
                f64::from(ch - 0x61)
            } else if (0x30..=0x39).contains(&ch) {
                f64::from(ch - 0x30) + 26.0
            } else {
                return None;
            };
            i += digit * w;
            let t = if k <= bias {
                PUNYCODE_TMIN
            } else if k >= bias + PUNYCODE_TMAX {
                PUNYCODE_TMAX
            } else {
                k - bias
            };
            if digit < t {
                break;
            }
            w *= PUNYCODE_BASE - t;
            k += PUNYCODE_BASE;
        }
        let out_length = (output.len() + 1) as f64;
        bias = puny_adapt(i - old_i, out_length, old_i == 0.0);
        n += (i / out_length).floor();
        i %= out_length;
        if !(0.0..=0x10FFFF as f64).contains(&n) || n.fract() != 0.0 {
            return None;
        }
        output.insert(i as usize, n as u32);
        i += 1.0;
    }
    let mut decoded = String::new();
    for cp in output {
        // JS 的 `String.fromCodePoint` 接受孤立代理项；Rust 的 `String` 不能，
        // 用 U+FFFD 占位（后面的 PVALID 检查同样拒绝）。
        decoded.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
    }
    Some(decoded)
}

/// `DigitToChar`。
fn puny_digit_to_char(digit: u32) -> char {
    if digit < 26 {
        char::from_u32(digit + 0x61).expect("ascii")
    } else {
        char::from_u32(digit - 26 + 0x30).expect("ascii")
    }
}

/// `Encode`：把 Unicode 字符串编码为 punycode 载荷（`xn--` 之后的部分）。
fn puny_encode(input: &str) -> String {
    let basic: String = input.chars().filter(|ch| ch.is_ascii()).collect();
    let basic_length = basic.chars().count() as i64;
    let mut result = String::new();
    if basic_length > 0 {
        result.push_str(&basic);
        result.push('-');
    }
    let code_points: Vec<u32> = input.chars().map(|ch| ch as u32).collect();
    let mut n: i64 = 128;
    let mut delta: i64 = 0;
    let mut bias: i64 = 72;
    let mut handled = basic_length;
    while handled < code_points.len() as i64 {
        let mut m = i64::MAX;
        for cp in &code_points {
            let cp = i64::from(*cp);
            if cp >= n && cp < m {
                m = cp;
            }
        }
        delta += (m - n) * (handled + 1);
        n = m;
        for cp in &code_points {
            let cp = i64::from(*cp);
            if cp < n {
                delta += 1;
            }
            if cp == n {
                let mut q = delta;
                let mut k: i64 = 36;
                loop {
                    let t = if k <= bias {
                        1
                    } else if k >= bias + 26 {
                        26
                    } else {
                        k - bias
                    };
                    if q < t {
                        break;
                    }
                    let digit = t + ((q - t) % (36 - t));
                    result.push(puny_digit_to_char(digit as u32));
                    q = (q - t) / (36 - t);
                    k += 36;
                }
                result.push(puny_digit_to_char(q as u32));
                bias =
                    puny_adapt(delta as f64, (handled + 1) as f64, handled == basic_length) as i64;
                delta = 0;
                handled += 1;
            }
        }
        delta += 1;
        n += 1;
    }
    result
}

// ── Bidi（`idna/format/bidi.mjs`） ──────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum BidiClass {
    En,
    An,
    On,
    Nsm,
    R,
    Al,
    L,
}

/// `GetBidiClass(codePoint)`：先 EN，再 AN、NSM、R、AL、L，其余 ON。
fn get_bidi_class(ch: char) -> BidiClass {
    if is_european_number(ch) {
        BidiClass::En
    } else if is_arabic_indic_digit(ch) {
        BidiClass::An
    } else if is_mark_nonspacing(ch) {
        BidiClass::Nsm
    } else if is_script_hebrew_char(ch) {
        BidiClass::R
    } else if is_script_arabic_letter_char(ch) {
        BidiClass::Al
    } else if is_letter(ch) {
        BidiClass::L
    } else {
        BidiClass::On
    }
}

/// `HasRightToLeftCharacters`：含有 R、AL 或 AN。
fn has_right_to_left_characters(value: &str) -> bool {
    value.chars().any(|ch| {
        matches!(
            get_bidi_class(ch),
            BidiClass::R | BidiClass::Al | BidiClass::An
        )
    })
}

/// `HasBidiChars`：ACE 标签先解码，解码失败视为否。
fn has_bidi_chars(value: &str) -> bool {
    if is_ace_prefixed(value) {
        let Some(rest) = value.get(4..) else {
            return false;
        };
        let body = rest.to_lowercase();
        return match puny_decode(&body) {
            Some(decoded) => has_right_to_left_characters(&decoded),
            None => false,
        };
    }
    has_right_to_left_characters(value)
}

/// `RE_RTL_ALLOWED`：`^(?:R|AL|AN|EN|ES|CS|ET|ON|BN|NSM)$`。
///
/// `GetBidiClass` 只产出 EN/AN/ON/NSM/R/AL/L（ES/CS/ET/BN 的分支在 TypeBox 里被注释掉，
/// 归入 ON），因此判定等价于下面的集合。
fn rtl_allowed(class: BidiClass) -> bool {
    matches!(
        class,
        BidiClass::R
            | BidiClass::Al
            | BidiClass::An
            | BidiClass::En
            | BidiClass::On
            | BidiClass::Nsm
    )
}

/// `RE_LTR_ALLOWED`：`^(?:L|EN|ES|CS|ET|ON|BN|NSM)$`。
fn ltr_allowed(class: BidiClass) -> bool {
    matches!(
        class,
        BidiClass::L | BidiClass::En | BidiClass::On | BidiClass::Nsm
    )
}

/// `SatisfiesBidiRule`：首字符必须属于 L/R/AL（决定方向），后续每个字符必须在该方向的
/// 允许集合内；RTL 标签里 EN 与 AN 不能同时出现。
fn satisfies_bidi_rule(value: &str) -> bool {
    let mut is_rtl = false;
    let mut allowed: fn(BidiClass) -> bool = ltr_allowed;
    let mut saw_en = false;
    let mut saw_an = false;
    let mut is_first = true;
    for ch in value.chars() {
        let bidi_class = get_bidi_class(ch);
        if is_first {
            if !matches!(bidi_class, BidiClass::L | BidiClass::R | BidiClass::Al) {
                return false;
            }
            is_rtl = matches!(bidi_class, BidiClass::R | BidiClass::Al);
            allowed = if is_rtl { rtl_allowed } else { ltr_allowed };
            is_first = false;
        }
        if !allowed(bidi_class) {
            return false;
        }
        if bidi_class == BidiClass::En {
            saw_en = true;
        } else if bidi_class == BidiClass::An {
            saw_an = true;
        }
    }
    if is_rtl && saw_en && saw_an {
        return false;
    }
    true
}

// ---------------------------------------------------------------------------
// 公开 API：check / errors / assert_valid / normalize / canonical_identity
// ---------------------------------------------------------------------------

/// `Value.Check(schema, value)`。
///
/// 注意：TS 在遇到非法正则（`pattern`/`patternProperties`）时会抛 `SyntaxError`；Rust 侧
/// **不 panic**，非法 pattern 视为不匹配（`false`）。需要"可捕获的 SyntaxError"语义的调用方
/// （[`normalize`]/[`assert_valid`]）会先用 [`first_invalid_regex`] 返回 `unavailable` 错误。
///
/// 返回 `true` 表示值满足 schema。`schema` 为 `false`（或 `$ref` 解析不到）时任何值都不通过；
/// `true`/`{}` 接受任何值。本函数只给布尔结果，失败原因用 [`errors`] 取。
pub fn check(schema: &Value, value: &Value) -> bool {
    let mut walk = Walk::with_root(Mode::Check, std::sync::Arc::new(schema.clone()));
    validate_schema(&mut walk, schema, value, "#", "")
}

/// `[...Value.Errors(schema, value)]`（已本地化：`{keyword, schemaPath, instancePath, params, message}`）。
///
/// 空数组表示通过。Error 模式不会因单个关键字失败就返回（继续评估后续关键字），但最多
/// 返回 8 条（TypeBox `Settings.maxErrors`），所以一个值可能同时带多条不同关键字的错误。
/// `errors` 与 [`check`] 是两次独立校验，不共享中间结果。
pub fn errors(schema: &Value, value: &Value) -> Vec<Value> {
    let mut walk = Walk::with_root(Mode::Error, std::sync::Arc::new(schema.clone()));
    validate_schema(&mut walk, schema, value, "#", "");
    walk.errors.iter().map(TbError::to_json).collect()
}

/// 未本地化的错误对象（内部核对用）。
///
/// 保留 `params` 原值与路径字段，供测试做未本地化的结构核对；对外展示用 [`errors`]。
pub fn raw_errors(schema: &Value, value: &Value) -> Vec<TbError> {
    let mut walk = Walk::with_root(Mode::Error, std::sync::Arc::new(schema.clone()));
    validate_schema(&mut walk, schema, value, "#", "");
    walk.errors
}

/// schema 的对象位置里第一个无法编译的 `pattern`/`patternProperties` 正则。
///
/// TS 的 `BuildPattern`/`BuildPatternProperties` 在构造校验函数时执行
/// `new RegExp(pattern, "u")`，非法 pattern 会抛 `SyntaxError`。Rust 用 `regress` 编译失败
/// 表示同一件事；调用方把它转成 `unavailable` 错误（TS 的 `SyntaxError` 不是 `ChordError`，
/// 但 Rust 不能抛任意异常）。
///
/// 注意：递归只沿**对象**形式的节点进行。`items: [...]`、`anyOf`/`allOf`/`oneOf`/
/// `prefixItems` 这类**数组**里的子 schema 在入口处因 `let Value::Object` 不匹配而直接返回
/// `None`，其中的非法 pattern 不会被本函数发现。另一方面，除 `const`/`enum` 外的对象值都会
/// 被递归，因此 `default`/`examples` 等数据位置里的对象也可能被误当作 schema 扫描（见函数
/// 末尾循环处的说明）。
pub fn first_invalid_regex(schema: &Value) -> Option<String> {
    let Value::Object(map) = schema else {
        return None;
    };
    if let Some(pattern) = map.get("pattern").and_then(Value::as_str)
        && compile_pattern(pattern).is_none()
    {
        return Some(pattern.to_string());
    }
    if let Some(Value::Object(patterns)) = map.get("patternProperties") {
        for pattern in patterns.keys() {
            if compile_pattern(pattern).is_none() {
                return Some(pattern.clone());
            }
        }
    }
    // `child` 若不是对象（典型的数组：`allOf`/`anyOf`/`oneOf`/`prefixItems`/tuple `items`），
    // 递归调用会在入口的 `let Value::Object` 处返回 `None`，因此数组里的子 schema 不会被扫描。
    // 另外这里只跳过 `const`/`enum`：`default`/`examples` 等数据位置里的对象也会被当成 schema
    // 递归，其中若恰有字符串字段 `pattern`，会被当作 pattern 关键字参与编译。
    for (key, child) in map {
        if key == "const" || key == "enum" {
            continue;
        }
        if let Some(pattern) = first_invalid_regex(child) {
            return Some(pattern);
        }
    }
    None
}

/// TS `SyntaxError` 的 `unavailable` 等价物。
fn regex_syntax_error(pattern: &str) -> ChordError {
    ChordError::new(
        ChordErrorCode::Unavailable,
        format!(
            "SyntaxError: Invalid regular expression: /{pattern}/u: invalid regular expression"
        ),
    )
}

/// `assertValid(reason, what, schema, value)`：不匹配时抛 `invalid(reason, …)`，`details` 是前 3 个错误。
///
/// 成功返回 `Ok(())`。错误语义：
/// - schema 内有非法正则时先返回错误码 `unavailable`（消息以 `SyntaxError: ...` 开头），
///   不会进入校验；但 [`first_invalid_regex`] 只递归对象位置、不进入数组，位于
///   `anyOf`/`allOf` 等数组分支里的非法 pattern 不会被检测到，只会表现为普通不匹配；
/// - 校验失败返回错误码 `invalid`：`ChordError.reason` 是传入的 `reason`，`message` 是
///   `"{what}: "` 加前 3 条错误（不足 3 条则全部）的 `"{instancePath 或 /} {message}"`
///   （`"; "` 连接），`details` 是这些错误的 JSON 对象数组。
pub fn assert_valid(
    reason: &str,
    what: &str,
    schema: &Value,
    value: &Value,
) -> Result<(), ChordError> {
    if let Some(pattern) = first_invalid_regex(schema) {
        return Err(regex_syntax_error(&pattern));
    }
    if check(schema, value) {
        return Ok(());
    }
    let all = errors(schema, value);
    let head: Vec<Value> = all.into_iter().take(3).collect();
    let summary = head
        .iter()
        .map(|error| {
            let instance_path = error
                .get("instancePath")
                .and_then(Value::as_str)
                .unwrap_or("");
            let message = error.get("message").and_then(Value::as_str).unwrap_or("");
            let path = if instance_path.is_empty() {
                "/"
            } else {
                instance_path
            };
            format!("{path} {message}")
        })
        .collect::<Vec<_>>()
        .join("; ");
    Err(invalid(reason, format!("{what}: {summary}")).with_details(Value::Array(head)))
}

/// `Value.Clone` 的 JSON 等价物。
///
/// TS（`system/memory/clone.mjs`）对**普通对象**逐键重建：跳过
/// `is_unsafe_property_key`（`__proto__`/`constructor`/`prototype`，防原型污染），
/// 键序是 `Guard.Keys`（`Object.getOwnPropertyNames`：数组索引键升序在前，其余保持插入序）。
/// 数组逐元素克隆。因此 `normalize` 的 Clone 步骤会丢掉这些键并重排整数样式键。
pub fn clone_value(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(clone_value).collect()),
        Value::Object(map) => {
            let mut out = Map::new();
            for key in js_keys(map) {
                if is_unsafe_property_key(key) {
                    continue;
                }
                out.insert(key.clone(), clone_value(&map[key.as_str()]));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// TypeBox 的 `~kind` 标记键。
///
/// TS 里它是 `Memory.Create` 写的**不可枚举**自有属性：`Object.keys`/`JSON.stringify`/
/// `canonicalJson` 都看不见它，但 `IsKind(value, kind)`（`Guard.HasPropertyKey(value,'~kind')`）
/// 看得见，`Value.Default`/`Value.Clean` 正是按它分派。Rust 的 [`Value`] 只有字符串键、没有
/// 属性可枚举性，因此用普通键表示标记，并在所有 schema 序列化路径
/// （[`strip_live_markers`]）上剥除。
pub const KIND_KEY: &str = "~kind";

/// 按 JSON 关键字推断 TypeBox 构造器写下的 `~kind`。
///
/// 只推断 `Default`/`Clean` 分派用到的种类。判定顺序对应 TypeBox 构造器的产物形状：
/// `Type.Array`/`Type.Tuple` 有 `items`；`Type.Object` **总是**有 `properties`（即使 `{}`），
/// 且可以同时带 `patternProperties`——所以 `properties`/`required` 必须在 `patternProperties`
/// 之前判定（`Type.Record` 的产物只有 `type` + `patternProperties`）。`Type.Intersect` 有
/// `allOf`，`Type.Cyclic` 是 `$defs` + `$ref`，`Type.Ref` 只有 `$ref`，`Type.Union` 有 `anyOf`。
fn infer_schema_kind(schema: &Value) -> Option<&'static str> {
    let Value::Object(map) = schema else {
        return None;
    };
    if matches!(map.get("items"), Some(Value::Array(_))) {
        return Some("Tuple");
    }
    if map.contains_key("items") {
        return Some("Array");
    }
    if map.get("$ref").is_some_and(Value::is_string)
        && matches!(map.get("$defs"), Some(Value::Object(_)))
    {
        return Some("Cyclic");
    }
    if map.contains_key("allOf") {
        return Some("Intersect");
    }
    if map.contains_key("properties") || map.contains_key("required") {
        return Some("Object");
    }
    if map.contains_key("patternProperties") {
        return Some("Record");
    }
    if map.get("$ref").is_some_and(Value::is_string) {
        return Some("Ref");
    }
    if map.contains_key("anyOf") {
        return Some("Union");
    }
    if is_type_named(map, "object") {
        return Some("Object");
    }
    None
}

/// 读取 `~kind`（`IsKind` 的公共部分）。
fn schema_kind(schema: &Value) -> Option<&str> {
    match schema {
        Value::Object(map) => map.get(KIND_KEY).and_then(Value::as_str),
        _ => None,
    }
}

/// `Guard.IsObject`：`typeof value === "object"` 且非 `null`（**数组也算**）。
fn is_guard_object(value: &Value) -> bool {
    !matches!(
        value,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
    )
}

/// JS 的 `typeof value === "object"`：`null` 与数组都算（`objectFits` 的 additionalProperties
/// 守卫用）。
fn is_typeof_object(value: &Value) -> bool {
    matches!(value, Value::Object(_) | Value::Array(_) | Value::Null)
}

/// TypeBox `schema/types/schema.mjs` 的 `IsSchema`：非数组对象或 boolean。
fn is_schema_value(value: &Value) -> bool {
    matches!(value, Value::Object(_) | Value::Bool(_))
}

/// 给 schema 树打上 `~kind`：`remote()`/`local()` 收到的声明 schema，等价于用户代码里
/// `Type.Object(...)`/`Type.Array(...)` 等构造出的 TypeBox 对象（本进程内"活"的 schema）。
///
/// 只标记 schema 位置（`properties`/`patternProperties`/`items`/`additionalProperties`/
/// `anyOf` …），绝不进入 `default`/`const`/`enum`/`examples` 里的**数据**。
pub fn mark_live_schema(schema: &Value) -> Value {
    let Value::Object(map) = schema else {
        return schema.clone();
    };
    let mut out = Map::new();
    if let Some(kind) = infer_schema_kind(schema) {
        out.insert(KIND_KEY.to_owned(), Value::String(kind.to_owned()));
    }
    for (key, value) in map {
        if key == KIND_KEY {
            continue;
        }
        let marked = match key.as_str() {
            "properties" | "patternProperties" | "definitions" | "$defs" | "dependentSchemas" => {
                match value {
                    Value::Object(children) => {
                        let mut next = Map::new();
                        for (name, child) in children {
                            next.insert(
                                name.clone(),
                                if is_guard_object(child) {
                                    mark_live_schema(child)
                                } else {
                                    child.clone()
                                },
                            );
                        }
                        Value::Object(next)
                    }
                    other => other.clone(),
                }
            }
            "items" => match value {
                Value::Array(entries) => {
                    Value::Array(entries.iter().map(mark_live_schema).collect())
                }
                other => mark_live_schema(other),
            },
            "additionalProperties"
            | "additionalItems"
            | "not"
            | "contains"
            | "propertyNames"
            | "if"
            | "then"
            | "else" => {
                if is_guard_object(value) {
                    mark_live_schema(value)
                } else {
                    value.clone()
                }
            }
            "anyOf" | "allOf" | "oneOf" | "prefixItems" => match value {
                Value::Array(entries) => {
                    Value::Array(entries.iter().map(mark_live_schema).collect())
                }
                other => other.clone(),
            },
            _ => value.clone(),
        };
        out.insert(key.clone(), marked);
    }
    Value::Object(out)
}

/// 剥掉 schema 树里的 `~kind`：`JSON.stringify`/`Object.keys` 看不见标记的等价物。
///
/// 只进入 schema 位置（与 [`mark_live_schema`] 对称），不会误删 `default`/`const`/`enum`
/// 里同名键的用户数据。
pub fn strip_live_markers(schema: &Value) -> Value {
    let Value::Object(map) = schema else {
        return schema.clone();
    };
    let mut out = Map::new();
    for (key, value) in map {
        if key == KIND_KEY {
            continue;
        }
        let next = match key.as_str() {
            "properties" | "patternProperties" | "definitions" | "$defs" | "dependentSchemas" => {
                match value {
                    Value::Object(children) => {
                        let mut next = Map::new();
                        for (name, child) in children {
                            next.insert(
                                name.clone(),
                                if is_guard_object(child) {
                                    strip_live_markers(child)
                                } else {
                                    child.clone()
                                },
                            );
                        }
                        Value::Object(next)
                    }
                    other => other.clone(),
                }
            }
            "items" => match value {
                Value::Array(entries) => {
                    Value::Array(entries.iter().map(strip_live_markers).collect())
                }
                other => strip_live_markers(other),
            },
            "additionalProperties"
            | "additionalItems"
            | "not"
            | "contains"
            | "propertyNames"
            | "if"
            | "then"
            | "else" => {
                if is_guard_object(value) {
                    strip_live_markers(value)
                } else {
                    value.clone()
                }
            }
            "anyOf" | "allOf" | "oneOf" | "prefixItems" => match value {
                Value::Array(entries) => {
                    Value::Array(entries.iter().map(strip_live_markers).collect())
                }
                other => other.clone(),
            },
            _ => value.clone(),
        };
        out.insert(key.clone(), next);
    }
    Value::Object(out)
}

/// JS 的数组下标键（`ToString(ToUint32(k)) === k && ToUint32(k) !== 2^32-1`）。
fn js_array_index(key: &str) -> Option<u32> {
    if key.is_empty() || key.len() > 10 {
        return None;
    }
    if key.len() > 1 && key.starts_with('0') {
        return None;
    }
    if !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let index: u32 = key.parse().ok()?;
    if index == u32::MAX {
        return None;
    }
    Some(index)
}

/// 按 JS 自有键序递归重排一个 JSON 值（数组下标键升序在前，其余保持插入序）。
///
/// JS 引擎对**任何**对象的自有键都按这个顺序枚举（`Object.keys`/`JSON.stringify`），
/// 而 `serde_json` 的 `preserve_order` 只保插入序。schema 在 `remote()`/`local()` 里先过这里，
/// 使 `Object.keys(schema)` 顺序符合 JS 引擎的自有键序（也决定 `Default` 遍历 `properties` 的顺序）。
pub fn reorder_js_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut indices: Vec<(u32, String, Value)> = Vec::new();
            let mut others: Vec<(String, Value)> = Vec::new();
            for (key, item) in map {
                let reordered = reorder_js_keys(item);
                match js_array_index(key) {
                    Some(index) => indices.push((index, key.clone(), reordered)),
                    None => others.push((key.clone(), reordered)),
                }
            }
            indices.sort_by_key(|(index, _, _)| *index);
            let mut out = Map::new();
            for (_, key, item) in indices {
                out.insert(key, item);
            }
            for (key, item) in others {
                out.insert(key, item);
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(reorder_js_keys).collect()),
        other => other.clone(),
    }
}

/// `Object.getOwnPropertyNames(Object.prototype)` 里**非** `IsUnsafePropertyKey` 的那些：
/// `Guard.HasPropertyKey(obj, key)` 用 `key in obj`，这些名字通过原型链恒为真。
const OBJECT_PROTOTYPE_NAMES: [&str; 10] = [
    "__defineGetter__",
    "__defineSetter__",
    "hasOwnProperty",
    "__lookupGetter__",
    "__lookupSetter__",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toString",
    "valueOf",
    "toLocaleString",
];

/// `Guard.IsUnsafePropertyKey`：原型上还要特殊处理的名字。
fn is_unsafe_property_key(key: &str) -> bool {
    matches!(key, "__proto__" | "constructor" | "prototype")
}

/// `Guard.HasPropertyKey`：`Object.prototype.hasOwnProperty.call` 或 `key in value`。
pub(crate) fn has_js_property_key(map: &Map<String, Value>, key: &str) -> bool {
    if is_unsafe_property_key(key) {
        map.contains_key(key)
    } else {
        map.contains_key(key) || OBJECT_PROTOTYPE_NAMES.contains(&key)
    }
}

/// [`has_js_property_key`] 的"仅原型链"分支：键不是自有属性、但 `in` 为真。
fn inherits_js_property_key(map: &Map<String, Value>, key: &str) -> bool {
    !map.contains_key(key) && !is_unsafe_property_key(key) && OBJECT_PROTOTYPE_NAMES.contains(&key)
}

fn is_type_named(schema: &Map<String, Value>, name: &str) -> bool {
    matches!(schema.get("type"), Some(Value::String(text)) if text == name)
}

/// `new RegExp(pattern)`（**无** `u` 标志）：`FromRecord` 的 `RecordPattern` 用它。
/// 非法 pattern 在 TS 里抛 `SyntaxError`；Rust 不 panic，视为不匹配。
fn record_pattern_matches(pattern: &str, text: &str) -> bool {
    let flags = regress::Flags::default();
    regress::Regex::with_flags(pattern, flags).is_ok_and(|regex| regex.find(text).is_some())
}

/// `Value.Default(schema, value)`：TypeBox `default/from_type.mjs` 的逐字移植。
///
/// `FromType` 按 `~kind` 分派：**没有标记的 schema**（经 JSON 传输后的形态，TS 里就是普通对象）
/// 只保留顶层的 `default`（`IsDefault` 不依赖 `~kind`），其余原样返回；带标记的 schema
/// （[`mark_live_schema`] 的产物，即本进程内构造的 TypeBox schema）才递归注入默认值。
///
/// 顶层不接受 `undefined`（参数是 `Value` 而非 `Option`）；"值缺失"只在内部递归里用
/// `None` 表示（对象缺失属性、元组缺失槽位）。返回新值，不修改入参。
pub fn default_value(schema: &Value, value: Value) -> Value {
    default_from(schema, Some(value)).unwrap_or(Value::Null)
}

/// `undefined` 在 Rust 侧用 `None` 表示（键缺失）。
fn default_from(schema: &Value, value: Option<Value>) -> Option<Value> {
    default_from_ctx(&DefaultContext::new(), schema, value)
}

/// 带 `$ref` 上下文（TS `Default(context, type, value)` 的 `context`）的 `FromType`。
///
/// 分两步：先对**当前节点**应用 `default`（仅当输入为 `None`/undefined 且 schema 是带
/// `default` 的对象），再按 [`KIND_KEY`] 分派到各构造器的 Default 逻辑；没有标记的节点
/// 原样返回。`None` 代表 TS 的 `undefined`：对象缺失属性、元组缺失槽位都以它传入，
/// 返回值 `None` 表示该位置仍为空（调用方决定跳过还是写 `null`）。
fn default_from_ctx(
    context: &DefaultContext,
    schema: &Value,
    value: Option<Value>,
) -> Option<Value> {
    // `IsDefault(type) ? FromDefault(type, value) : value`
    let value = match (schema, &value) {
        (Value::Object(map), None) if map.contains_key("default") => map.get("default").cloned(),
        _ => value,
    };
    match schema_kind(schema) {
        // `FromCyclic`：`FromType({...context, ...type.$defs}, Ref(type.$ref), value)`。
        Some("Cyclic") => match (
            schema.get("$ref").and_then(Value::as_str),
            schema.get("$defs"),
        ) {
            (Some(reference), Some(Value::Object(definitions))) => {
                let mut next = context.clone();
                for (key, definition) in definitions {
                    next.insert(key.clone(), definition.clone());
                }
                default_from_ctx(&next, &ref_node(reference), value)
            }
            _ => value,
        },
        // `FromIntersect`：`FromType(context, Evaluate(Instantiate(context, type)), value)`。
        Some("Intersect") => {
            let evaluated = evaluate_intersect(context, schema, false);
            default_from_ctx(context, &evaluated, value)
        }
        // `FromRef`：`context[$ref]` 存在才继续，否则原样返回。
        Some("Ref") => match schema.get("$ref").and_then(Value::as_str) {
            Some(reference) => match context.get(reference) {
                Some(target) => {
                    let target = target.clone();
                    default_from_ctx(context, &target, value)
                }
                None => value,
            },
            None => value,
        },
        Some("Array") => match value {
            Some(Value::Array(items)) => {
                let item_schema = schema.get("items").cloned().unwrap_or(Value::Null);
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(
                        default_from_ctx(context, &item_schema, Some(item)).unwrap_or(Value::Null),
                    );
                }
                Some(Value::Array(out))
            }
            other => other,
        },
        Some("Tuple") => match value {
            Some(Value::Array(items)) => {
                let tuple: Vec<Value> = schema
                    .get("items")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let max = tuple.len().max(items.len());
                let mut out = items;
                for index in 0..max {
                    if index >= tuple.len() {
                        continue;
                    }
                    let current = out.get(index).cloned();
                    // `value[i] = FromType(items[i], value[i])`：缺失的槽位写 undefined（JSON 里是 null）。
                    let next =
                        default_from_ctx(context, &tuple[index], current).unwrap_or(Value::Null);
                    if index < out.len() {
                        out[index] = next;
                    } else {
                        out.push(next);
                    }
                }
                Some(Value::Array(out))
            }
            other => other,
        },
        Some("Object") => match value {
            Some(Value::Object(mut map)) => {
                let properties = schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                for (key, property_schema) in &properties {
                    let current = map.get(key).cloned();
                    // `isUnassignableUndefined`：仍为 undefined 且（可选或无默认值）→ 跳过。
                    let Some(next) = default_from_ctx(context, property_schema, current) else {
                        continue;
                    };
                    // `value['__proto__'] = x` 在 JS 里设置原型（无自有键）；只有本来就是自有
                    // 数据属性（如 JSON.parse 的产物）时才更新键。
                    if key == "__proto__" && !map.contains_key(key) {
                        continue;
                    }
                    map.insert(key.clone(), next);
                }
                if let Some(additional) = schema.get("additionalProperties")
                    && is_schema_value(additional)
                    && !additional.is_boolean()
                {
                    let keys: Vec<String> = js_keys(&map).into_iter().cloned().collect();
                    for key in keys {
                        if properties.contains_key(&key) {
                            continue;
                        }
                        let current = map.get(&key).cloned();
                        if let Some(next) = default_from_ctx(context, additional, current) {
                            map.insert(key, next);
                        }
                    }
                }
                Some(Value::Object(map))
            }
            Some(Value::Array(mut items)) => {
                // `Guard.IsObject(array)` 在 TS 里为真：`FromObject` 把数组当对象处理
                // （`Guard.Keys` 是索引键加 `length`）。数组上的具名属性与 `length` 赋值在
                // JSON 里不可观察，这里只复刻索引键上的默认值。
                let properties = schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                for (key, property_schema) in &properties {
                    let Some(index) = js_array_index(key) else {
                        continue;
                    };
                    let index = index as usize;
                    let current = items.get(index).cloned();
                    let Some(next) = default_from_ctx(context, property_schema, current) else {
                        continue;
                    };
                    while items.len() < index {
                        items.push(Value::Null);
                    }
                    if index < items.len() {
                        items[index] = next;
                    } else {
                        items.push(next);
                    }
                }
                if let Some(additional) = schema.get("additionalProperties")
                    && is_schema_value(additional)
                    && !additional.is_boolean()
                {
                    for (index, item) in items.iter_mut().enumerate() {
                        if properties.contains_key(&index.to_string()) {
                            continue;
                        }
                        *item = default_from_ctx(context, additional, Some(item.clone()))
                            .unwrap_or(Value::Null);
                    }
                }
                Some(Value::Array(items))
            }
            other => other,
        },
        Some("Record") => match value {
            Some(Value::Object(mut map)) => {
                // `RecordPattern(type)` 只取 `patternProperties` 的第一个自有键。
                let patterns: Vec<(String, Value)> = schema
                    .get("patternProperties")
                    .and_then(Value::as_object)
                    .map(|patterns| {
                        patterns
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                let record = patterns.first().cloned();
                if let Some((pattern, record_value)) = &record {
                    let has_default =
                        matches!(record_value, Value::Object(map) if map.contains_key("default"));
                    if has_default {
                        let keys: Vec<String> = map.keys().cloned().collect();
                        for key in keys {
                            if !record_pattern_matches(pattern, &key) {
                                continue;
                            }
                            let current = map.get(&key).cloned();
                            if let Some(next) = default_from_ctx(context, record_value, current) {
                                map.insert(key, next);
                            }
                        }
                    }
                }
                if let Some(additional) = schema.get("additionalProperties")
                    && is_schema_value(additional)
                {
                    let keys: Vec<String> = map.keys().cloned().collect();
                    for key in keys {
                        if record
                            .as_ref()
                            .is_some_and(|(pattern, _)| record_pattern_matches(pattern, &key))
                        {
                            continue;
                        }
                        let current = map.get(&key).cloned();
                        if let Some(next) = default_from_ctx(context, additional, current) {
                            map.insert(key, next);
                        }
                    }
                }
                Some(Value::Object(map))
            }
            other => other,
        },
        Some("Union") => match value {
            Some(current) => {
                let branches: Vec<Value> = schema
                    .get("anyOf")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for branch in branches {
                    if let Some(result) =
                        default_from_ctx(context, &branch, Some(clone_value(&current)))
                        && check(
                            &instantiate_schema(context, &branch, &mut Vec::new()),
                            &result,
                        )
                    {
                        return Some(result);
                    }
                }
                Some(current)
            }
            None => None,
        },
        _ => value,
    }
}

/// `Value.Clean(schema, value)`：移除 schema 未声明/未允许的字段。
///
/// TS：`Clean` 先做 `UnionPrioritySort`（`Settings.unionPrioritySort = true`），再按 `~kind`
/// 分派（`clean/from_type.mjs`）。没有 `~kind` 标记的 schema（经 JSON 传输后的形态）原样返回；
/// 带标记的 schema（[`mark_live_schema`]）才真正清洗。
///
/// Object 分支的保留规则：`properties` 里声明的键按子 schema 递归清洗；未声明的键只有
/// `additionalProperties` 为 `true` 或为匹配的 schema 时才保留（为 schema 时还会继续清洗），
/// `false` 或缺失时删除。`toString` 这类经原型链命中的名字按"已声明"原样保留（见
/// `inherits_js_property_key`）。Record 分支只用 `patternProperties` 的第一个 pattern。
pub fn clean_value(schema: &Value, value: Value) -> Value {
    let sorted = union_priority_sort(schema);
    clean_from(&sorted, value)
}

fn clean_from(schema: &Value, value: Value) -> Value {
    clean_from_ctx(&DefaultContext::new(), schema, value)
}

/// 带 `$ref` 上下文的 `FromType`（Clean 版本）。
///
/// 按 [`KIND_KEY`] 递归重建值；没有标记的节点原样返回（TS `FromType` 的分派条件）。
/// Object/Record 分支决定删除哪些键：已声明/命中 pattern 的键按子 schema 继续清洗，其余
/// 键只在 `additionalProperties` 允许时保留。`Cyclic`/`Ref` 从 `context`（`$defs` 或
/// 上层注入的定义）解析引用。
fn clean_from_ctx(context: &DefaultContext, schema: &Value, value: Value) -> Value {
    match schema_kind(schema) {
        // `FromCyclic` / `FromRef` / `FromIntersect`（Clean 版本）。
        Some("Cyclic") => match (
            schema.get("$ref").and_then(Value::as_str),
            schema.get("$defs"),
        ) {
            (Some(reference), Some(Value::Object(definitions))) => {
                let mut next = context.clone();
                for (key, definition) in definitions {
                    next.insert(key.clone(), definition.clone());
                }
                clean_from_ctx(&next, &ref_node(reference), value)
            }
            _ => value,
        },
        // Clean 把 `unevaluatedProperties` 重新解释成 `additionalProperties`（`EvaluateIntersection`）。
        Some("Intersect") => {
            let evaluated = evaluate_intersect(context, schema, true);
            clean_from_ctx(context, &evaluated, value)
        }
        Some("Ref") => match schema.get("$ref").and_then(Value::as_str) {
            Some(reference) => match context.get(reference) {
                Some(target) => {
                    let target = target.clone();
                    clean_from_ctx(context, &target, value)
                }
                None => value,
            },
            None => value,
        },
        Some("Array") => match value {
            Value::Array(items) => {
                let item_schema = schema.get("items").cloned().unwrap_or(Value::Null);
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(clean_from_ctx(context, &item_schema, item));
                }
                Value::Array(out)
            }
            other => other,
        },
        Some("Tuple") => match value {
            Value::Array(items) => {
                let tuple: Vec<Value> = schema
                    .get("items")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let length = items.len().min(tuple.len());
                let mut out: Vec<Value> = Vec::with_capacity(length);
                for (index, item) in items.into_iter().enumerate().take(length) {
                    out.push(clean_from_ctx(context, &tuple[index], item));
                }
                Value::Array(out)
            }
            other => other,
        },
        Some("Object") => match value {
            Value::Object(map) => {
                let properties = schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let additional = schema.get("additionalProperties").cloned();
                let mut out = Map::new();
                // `FromObject` 按 `Guard.Keys(value)` 迭代（JS 枚举序：数组索引键升序在前）。
                for key in js_keys(&map) {
                    let item = map[key.as_str()].clone();
                    if let Some(property_schema) = properties.get(key) {
                        out.insert(key.clone(), clean_from_ctx(context, property_schema, item));
                        continue;
                    }
                    // `HasPropertyKey(type.properties, key)` 走原型链：`toString` 之类的名字算已声明，
                    // `FromType(函数, value)` 原样返回。
                    if inherits_js_property_key(&properties, key.as_str()) {
                        out.insert(key.clone(), item);
                        continue;
                    }
                    let keep = match &additional {
                        Some(Value::Bool(true)) => true,
                        Some(schema_value) if is_schema_value(schema_value) => {
                            check(schema_value, &item)
                        }
                        _ => false,
                    };
                    if keep {
                        let next = match &additional {
                            Some(schema_value) if is_schema_value(schema_value) => {
                                clean_from_ctx(context, schema_value, item)
                            }
                            _ => item,
                        };
                        out.insert(key.clone(), next);
                    }
                }
                Value::Object(out)
            }
            other => other,
        },
        Some("Record") => match value {
            Value::Object(map) => {
                let patterns: Vec<(String, Value)> = schema
                    .get("patternProperties")
                    .and_then(Value::as_object)
                    .map(|patterns| {
                        patterns
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                let record = patterns.first().cloned();
                let additional = schema.get("additionalProperties").cloned();
                let mut out = Map::new();
                for key in js_keys(&map) {
                    let item = map[key.as_str()].clone();
                    if let Some((pattern, record_value)) = &record
                        && record_pattern_matches(pattern, key)
                    {
                        out.insert(key.clone(), clean_from_ctx(context, record_value, item));
                        continue;
                    }
                    let keep = match &additional {
                        Some(Value::Bool(true)) => true,
                        Some(schema_value) if is_schema_value(schema_value) => {
                            check(schema_value, &item)
                        }
                        _ => false,
                    };
                    if keep {
                        let next = match &additional {
                            Some(schema_value) if is_schema_value(schema_value) => {
                                clean_from_ctx(context, schema_value, item)
                            }
                            _ => item,
                        };
                        out.insert(key.clone(), next);
                    }
                }
                Value::Object(out)
            }
            other => other,
        },
        Some("Union") => match schema.get("anyOf").and_then(Value::as_array) {
            Some(branches) => {
                for branch in branches {
                    let clean = clean_from_ctx(context, branch, clone_value(&value));
                    if check(
                        &instantiate_schema(context, branch, &mut Vec::new()),
                        &clean,
                    ) {
                        return clean;
                    }
                }
                value
            }
            None => value,
        },
        _ => value,
    }
}

/// Default/Clean 的 `$ref` 上下文（TS `Default(context, type, value)` 的 `context`：`$ref` 名 → schema）。
type DefaultContext = Map<String, Value>;

/// `Ref(ref)` 的 JSON 形态（带 `~kind`；`FromCyclic` 用 `Ref(type.$ref)` 转交 `FromRef`）。
fn ref_node(reference: &str) -> Value {
    let mut map = Map::new();
    map.insert(KIND_KEY.to_owned(), Value::String("Ref".to_owned()));
    map.insert("$ref".to_owned(), Value::String(reference.to_owned()));
    Value::Object(map)
}

/// `Never()`。
fn never_node() -> Value {
    let mut map = Map::new();
    map.insert(KIND_KEY.to_owned(), Value::String("Never".to_owned()));
    map.insert("not".to_owned(), Value::Object(Map::new()));
    Value::Object(map)
}

fn is_kind(schema: &Value, kind: &str) -> bool {
    schema_kind(schema) == Some(kind)
}

/// `CanComposite`：`IsObject(type) || IsTuple(type)`。
fn is_composite_operand(schema: &Value) -> bool {
    matches!(schema_kind(schema), Some("Object") | Some("Tuple"))
}

/// `Compare` 的四档结果（`type/engine/evaluate/compare.mjs`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompareResult {
    Equal,
    Disjoint,
    LeftInside,
    RightInside,
}

/// `Compare(left, right)`：两次 `Extends`（Rust 侧用 [`fits`]）。
fn compare_types(left: &Value, right: &Value) -> CompareResult {
    let left_in_right = fits(left, right);
    let right_in_left = fits(right, left);
    match (left_in_right, right_in_left) {
        (true, true) => CompareResult::Equal,
        (true, false) => CompareResult::LeftInside,
        (false, true) => CompareResult::RightInside,
        (false, false) => CompareResult::Disjoint,
    }
}

/// `Instantiate(context, type)`：只复刻影响 `$ref` 解析的部分。
///
/// TS 的 `InstantiateImmediate` 重建 Array/Tuple/Object/Record/Union/Intersect 的**所有**
/// schema 位置（`properties`/`items`/`patternProperties`/`anyOf`/`allOf`），解析 `Ref`；
/// 其它选项（如 `additionalProperties`）原样拷贝，不递归。`Cyclic` 不在重建列表里，原样返回。
/// `visited` 是 TS 的 `state.visited`：重复出现的 `$ref` 保持为 Ref，避免无限递归。
fn instantiate_schema(
    context: &DefaultContext,
    schema: &Value,
    visited: &mut Vec<String>,
) -> Value {
    let Value::Object(map) = schema else {
        return schema.clone();
    };
    if is_kind(schema, "Ref") {
        let Some(reference) = map.get("$ref").and_then(Value::as_str) else {
            return schema.clone();
        };
        if visited.iter().any(|item| item == reference) {
            return schema.clone();
        }
        let Some(target) = context.get(reference).cloned() else {
            return schema.clone();
        };
        let mut next_visited = visited.clone();
        next_visited.push(reference.to_owned());
        return instantiate_schema(context, &target, &mut next_visited);
    }
    let mut out = map.clone();
    match schema_kind(schema) {
        Some("Array") => {
            if let Some(items) = map.get("items") {
                out.insert(
                    "items".to_owned(),
                    instantiate_schema(context, items, &mut *visited),
                );
            }
        }
        Some("Tuple") => {
            if let Some(Value::Array(items)) = map.get("items") {
                let mut next = Vec::with_capacity(items.len());
                for item in items {
                    next.push(instantiate_schema(context, item, &mut *visited));
                }
                out.insert("items".to_owned(), Value::Array(next));
            }
        }
        Some("Object") => {
            if let Some(Value::Object(properties)) = map.get("properties") {
                let mut next = Map::new();
                for (key, property) in properties {
                    next.insert(
                        key.clone(),
                        instantiate_schema(context, property, &mut *visited),
                    );
                }
                out.insert("properties".to_owned(), Value::Object(next));
            }
        }
        Some("Record") => {
            if let Some(Value::Object(patterns)) = map.get("patternProperties") {
                let mut next = Map::new();
                for (key, pattern) in patterns {
                    next.insert(
                        key.clone(),
                        instantiate_schema(context, pattern, &mut *visited),
                    );
                }
                out.insert("patternProperties".to_owned(), Value::Object(next));
            }
        }
        Some("Union") => {
            if let Some(Value::Array(branches)) = map.get("anyOf") {
                let mut next = Vec::with_capacity(branches.len());
                for branch in branches {
                    next.push(instantiate_schema(context, branch, &mut *visited));
                }
                out.insert("anyOf".to_owned(), Value::Array(next));
            }
        }
        Some("Intersect") => {
            if let Some(Value::Array(branches)) = map.get("allOf") {
                let mut next = Vec::with_capacity(branches.len());
                for branch in branches {
                    next.push(instantiate_schema(context, branch, &mut *visited));
                }
                out.insert("allOf".to_owned(), Value::Array(next));
            }
        }
        _ => {}
    }
    Value::Object(out)
}

/// `EvaluateType`：本引擎只能还原 `Intersect`/`Union` 的 `~kind`
/// （`Dependent`/`Enum`/`TemplateLiteral` 在纯 JSON 里无法与普通 schema 区分）。
fn evaluate_type(schema: &Value) -> Value {
    match schema_kind(schema) {
        Some("Intersect") => {
            let types = schema
                .get("allOf")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            evaluate_intersect_types(types)
        }
        Some("Union") => {
            let types = schema
                .get("anyOf")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            evaluate_union_types(&types)
        }
        _ => schema.clone(),
    }
}

/// `EvaluateIntersect(types)` = `EvaluateUnion(Broaden(Distribute(types)))`。
fn evaluate_intersect_types(types: Vec<Value>) -> Value {
    let distribution = distribute_types(&types);
    let broadened = broaden(distribution);
    evaluate_union_types(&broadened)
}

/// `EvaluateUnion(types)` = `EvaluateUnionFast(Broaden(types))`。
fn evaluate_union_types(types: &[Value]) -> Value {
    let broadened = broaden(types.to_vec());
    let flattened = flatten_types(&broadened);
    if flattened.is_empty() {
        return never_node();
    }
    if flattened.len() == 1 {
        return flattened[0].clone();
    }
    let mut map = Map::new();
    map.insert(KIND_KEY.to_owned(), Value::String("Union".to_owned()));
    map.insert("anyOf".to_owned(), Value::Array(flattened));
    Value::Object(map)
}

/// `Flatten(types)`：展开顶层 Union。
fn flatten_types(types: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for item in types {
        if is_kind(item, "Union")
            && let Some(branches) = item.get("anyOf").and_then(Value::as_array)
        {
            out.extend(branches.iter().cloned());
            continue;
        }
        out.push(item.clone());
    }
    out
}

/// `Distribute(types)`。
fn distribute_types(types: &[Value]) -> Vec<Value> {
    distribute_with(types, Vec::new())
}

fn distribute_with(types: &[Value], result: Vec<Value>) -> Vec<Value> {
    let mut result = result;
    for left in types {
        result = if is_kind(left, "Union") {
            // `DistributeUnion(left.anyOf, result)`。
            let branches: Vec<Value> = left
                .get("anyOf")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut next = Vec::new();
            for branch in &branches {
                next.extend(distribute_with(
                    std::slice::from_ref(branch),
                    result.clone(),
                ));
            }
            next
        } else {
            distribute_type(left, &result)
        };
    }
    result
}

/// `DistributeType(type, types)`。
fn distribute_type(branch: &Value, types: &[Value]) -> Vec<Value> {
    if types.is_empty() {
        return vec![branch.clone()];
    }
    types
        .iter()
        .map(|left| distribute_operation(left, branch))
        .collect()
}

/// `DistributeOperation(left, right)`：任一侧是 Union 时递归分配，否则 `Narrow`。
fn distribute_operation(left: &Value, right: &Value) -> Value {
    let evaluated_left = evaluate_type(left);
    let evaluated_right = evaluate_type(right);
    if is_kind(&evaluated_left, "Union") || is_kind(&evaluated_right, "Union") {
        evaluate_intersect_types(vec![evaluated_left, evaluated_right])
    } else {
        narrow(&evaluated_left, &evaluated_right)
    }
}

/// `Narrow(left, right)`（`type/engine/evaluate/narrow.mjs`）。
fn narrow(left: &Value, right: &Value) -> Value {
    if is_kind(left, "Never") {
        return left.clone();
    }
    if is_kind(left, "Any") {
        return left.clone();
    }
    if is_kind(left, "Unknown") {
        return right.clone();
    }
    if is_kind(right, "Never") {
        return right.clone();
    }
    if is_kind(right, "Any") {
        return right.clone();
    }
    if is_kind(right, "Unknown") {
        return left.clone();
    }
    let can_composite_left = is_composite_operand(left);
    let can_composite_right = is_composite_operand(right);
    match (can_composite_left, can_composite_right) {
        (true, true) => composite(left, right),
        (true, false) => left.clone(),
        (false, true) => right.clone(),
        (false, false) => match compare_types(left, right) {
            CompareResult::LeftInside => left.clone(),
            CompareResult::RightInside | CompareResult::Equal => right.clone(),
            CompareResult::Disjoint => never_node(),
        },
    }
}

/// `Composite(left, right)`：合并两个 Object/Tuple 的属性集。
///
/// 每个属性的 `CompositeProperty` 是 `EvaluateIntersect([left, right])`；`readonly`/`optional`
/// 修饰符在 JSON 里不可见（`~modifier` 是符号属性），因此只保留两侧 `required` 的并集。
fn composite(left: &Value, right: &Value) -> Value {
    let left_properties = composite_properties(left);
    let right_properties = composite_properties(right);
    let mut properties = Map::new();
    for (key, value) in &left_properties {
        let merged = match right_properties.get(key) {
            Some(other) => composite_property(value, other),
            None => value.clone(),
        };
        properties.insert(key.clone(), merged);
    }
    for (key, value) in &right_properties {
        if !left_properties.contains_key(key) {
            properties.insert(key.clone(), value.clone());
        }
    }
    let mut required: Vec<Value> = Vec::new();
    for source in [left, right] {
        if let Some(Value::Array(names)) = source.get("required") {
            for name in names {
                let Some(name) = name.as_str() else {
                    continue;
                };
                if properties.contains_key(name)
                    && !required.iter().any(|item| item.as_str() == Some(name))
                {
                    required.push(Value::String(name.to_owned()));
                }
            }
        }
    }
    let mut map = Map::new();
    map.insert(KIND_KEY.to_owned(), Value::String("Object".to_owned()));
    map.insert("type".to_owned(), Value::String("object".to_owned()));
    if !required.is_empty() {
        map.insert("required".to_owned(), Value::Array(required));
    }
    map.insert("properties".to_owned(), Value::Object(properties));
    Value::Object(map)
}

/// `CompositeProperty(left, right)`。
fn composite_property(left: &Value, right: &Value) -> Value {
    // `EvaluateIntersect([left, right])`：属性级交集。
    evaluate_intersect_types(vec![left.clone(), right.clone()])
}

/// `GetProperties(type)`：Object 取 `properties`，Tuple 把元素转成索引属性。
fn composite_properties(schema: &Value) -> Map<String, Value> {
    match schema_kind(schema) {
        Some("Object") => schema
            .get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
        Some("Tuple") => {
            let items: Vec<Value> = schema
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut out = Map::new();
            for (index, item) in items.into_iter().enumerate() {
                out.insert(index.to_string(), item);
            }
            out
        }
        _ => Map::new(),
    }
}

/// `Broaden(types)`（`type/engine/evaluate/broaden.mjs`）。
fn broaden(types: Vec<Value>) -> Vec<Value> {
    let mut result: Vec<Value> = Vec::new();
    for item in types {
        let evaluated = evaluate_type(&item);
        if is_kind(&evaluated, "Any") || is_kind(&evaluated, "Unknown") {
            return vec![evaluated];
        }
        if is_kind(&evaluated, "Never") {
            continue;
        }
        if is_kind(&evaluated, "Object") {
            // 对象比较太贵，TS 总是保留。
            result.push(evaluated);
            continue;
        }
        // `BroadenFilter(evaluated, result)`：遇到 left-inside / equal 时结果回到原始集合。
        let mut kept: Vec<Value> = Vec::new();
        let mut absorbed = false;
        for existing in &result {
            match compare_types(&evaluated, existing) {
                CompareResult::LeftInside | CompareResult::Equal => {
                    absorbed = true;
                    break;
                }
                CompareResult::Disjoint => kept.push(existing.clone()),
                CompareResult::RightInside => {}
            }
        }
        if absorbed {
            continue;
        }
        kept.push(evaluated);
        result = kept;
    }
    result
}

/// `FromIntersect` 的 `EvaluateIntersection`：`Evaluate(Instantiate(context, type))`；
/// Clean 时把 `unevaluatedProperties` 重新解释成 `additionalProperties`（仅当结果是 Object）。
fn evaluate_intersect(context: &DefaultContext, schema: &Value, clean: bool) -> Value {
    let additional = if clean {
        schema.get("unevaluatedProperties").cloned()
    } else {
        None
    };
    let instantiated = instantiate_schema(context, schema, &mut Vec::new());
    let evaluated = evaluate_type(&instantiated);
    match (additional, evaluated) {
        (Some(additional), Value::Object(mut map)) => {
            map.insert("additionalProperties".to_owned(), additional);
            Value::Object(map)
        }
        (_, evaluated) => evaluated,
    }
}

/// `UnionPrioritySort`（`value/shared/union_priority_sort.mjs`）：按 `~kind` 递归重建类型，
/// 只对**带标记**的节点生效（TS 里 `FromType` 只按 `~kind` 分派；无标记的普通对象原样返回）。
///
/// 重建规则与 TS 的 `FromType` 一一对应：
/// `Array` → `items` 递归；`Tuple` → `items` 逐项；`Intersect` → `allOf` 逐项；
/// `Object` → `properties` 逐项；`Record` → `Record(RecordKey(type), FromType(RecordValue))`
/// （自定义 pattern 经 `TemplateLiteralDecodeUnsafe` 变成普通 `String`，新 pattern 是 `^.*$`）；
/// `Union` → 对 `anyOf` 做 [`priority_sort`]。其余种类原样返回。
fn union_priority_sort(schema: &Value) -> Value {
    let Value::Object(map) = schema else {
        return schema.clone();
    };
    let mut out = map.clone();
    match schema_kind(schema) {
        Some("Array") => {
            if let Some(items) = map.get("items") {
                out.insert("items".to_string(), union_priority_sort(items));
            }
        }
        Some("Tuple") => {
            if let Some(Value::Array(items)) = map.get("items") {
                out.insert(
                    "items".to_string(),
                    Value::Array(items.iter().map(union_priority_sort).collect()),
                );
            }
        }
        Some("Intersect") => {
            if let Some(Value::Array(entries)) = map.get("allOf") {
                out.insert(
                    "allOf".to_string(),
                    Value::Array(entries.iter().map(union_priority_sort).collect()),
                );
            }
        }
        Some("Object") => {
            if let Some(Value::Object(children)) = map.get("properties") {
                let mut sorted_children = Map::new();
                for (name, child) in children {
                    sorted_children.insert(name.clone(), union_priority_sort(child));
                }
                out.insert("properties".to_string(), Value::Object(sorted_children));
            }
        }
        Some("Record") => {
            // `Record(RecordKey(type), FromType(RecordValue(type)))`：重建时 pattern 由
            // `RecordKey` 决定（标准 `^.*$`/整数/数字 pattern 保持不变；自定义 pattern 经
            // `TemplateLiteralDecodeUnsafe` 退化为 `String()`，即 `^.*$`）。
            if let Some(Value::Object(patterns)) = map.get("patternProperties")
                && let Some((pattern, record_value)) = patterns.iter().next()
            {
                let next_pattern = record_key_pattern(pattern);
                let next_value = union_priority_sort(record_value);
                let mut next_patterns = Map::new();
                next_patterns.insert(next_pattern, next_value);
                out.insert(
                    "patternProperties".to_string(),
                    Value::Object(next_patterns),
                );
            }
        }
        Some("Union") => {
            if let Some(Value::Array(branches)) = map.get("anyOf") {
                out.insert(
                    "anyOf".to_string(),
                    Value::Array(priority_sort(branches.clone())),
                );
            }
        }
        _ => return schema.clone(),
    }
    Value::Object(out)
}

/// `RecordKey(Record)`：把 `patternProperties` 的键过一遍 `RecordPatternToType` 再取 pattern。
fn record_key_pattern(pattern: &str) -> String {
    // `StringKey` / `IntegerKey` / `NumberKey` 是构造器的标准 pattern，重建后不变。
    if pattern == "^.*$"
        || pattern == "^-?(?:0|[1-9][0-9]*)$"
        || pattern == "^-?(?:0|[1-9][0-9]*)(?:\\.[0-9]+)?$"
    {
        return pattern.to_string();
    }
    // 其余 pattern：`TemplateLiteralDecodeUnsafe` 对无法解析成模板字面量的 pattern 返回
    // `String()`（非有限模板字面量也退化为 `String()`，见 `TemplateLiteralDecode`），
    // `Record(String(), value)` 的 pattern 是 `^.*$`。
    "^.*$".to_string()
}

fn priority_sort(branches: Vec<Value>) -> Vec<Value> {
    // TS `Sort(types, result) = ShiftLeft(types, (left, right) => Sort(right, Insert(left, result)), () => result)`。
    let mut result: Vec<Value> = Vec::new();
    for branch in branches {
        result = insert_branch(branch, result);
    }
    result
}

/// TS `Insert(type, types, result)`：
/// `ShiftLeft(types, (left, right) => Comparer(type,left)==1 ? Insert(type, right, [...result, left])
/// : [...result, type, ...types], () => [...result, type])`。
///
/// 关键：`Comparer == 1`（right-inside / disjoint）时**已有分支在前**，新类型继续向后找位置；
/// 只有 `Comparer == 0`（equal / left-inside）时新类型才插到该分支之前。
fn insert_branch(branch: Value, types: Vec<Value>) -> Vec<Value> {
    let mut result: Vec<Value> = Vec::new();
    let mut rest = types;
    while let Some((left, right)) = rest.split_first() {
        if comparator(&branch, left) == 1 {
            result.push(left.clone());
            rest = right.to_vec();
        } else {
            result.push(branch);
            result.extend(rest);
            return result;
        }
    }
    result.push(branch);
    result
}

/// `Comparer(left, right)`：`Compare` 结果为 right-inside/disjoint → 1，否则 0。
fn comparator(left: &Value, right: &Value) -> u8 {
    let left_in_right = fits(left, right);
    let right_in_left = fits(right, left);
    // TS：`equal` 与 `left-inside` 都是 0；`right-inside` 与 `disjoint` 都是 1。写回四档是为了
    // 与 TS 的 `Comparer` 一一对照（两两组合都覆盖到），结果只看 `left_in_right`。
    match (left_in_right, right_in_left) {
        (true, true) => 0,   // equal
        (true, false) => 0,  // left-inside
        (false, true) => 1,  // right-inside
        (false, false) => 1, // disjoint
    }
}

/// `normalize(schema, value, what)`：`Clean(Default(Clone(value ?? {})))` 然后 `assertValid("input", …)`。
///
/// `value` 为 `None` 表示 TS 的 `undefined`，`Some(Value::Null)` 表示 `null`：两者都走
/// `value ?? {}` → `{}`。
///
/// 返回清洗后的新 JSON 值。两个失败出口：非法正则 → `unavailable`；清洗后仍不满足 schema →
/// `invalid`（`reason` 固定为 `"input"`，`what` 只出现在消息前缀，例如
/// `"input: / must have required properties session"`）。因为校验发生在 Default/Clean 之后，
/// 默认值补出的字段也会被校验。
pub fn normalize(schema: &Value, value: Option<&Value>, what: &str) -> Result<Value, ChordError> {
    if let Some(pattern) = first_invalid_regex(schema) {
        return Err(regex_syntax_error(&pattern));
    }
    let seed = match value {
        // `value ?? {}`：`undefined` 与 `null` 都换成 `{}`。
        Some(Value::Null) | None => Value::Object(Map::new()),
        Some(value) => value.clone(),
    };
    let cloned = clone_value(&seed);
    let defaulted = default_from(schema, Some(cloned));
    let cleaned = clean_value(schema, defaulted.unwrap_or(Value::Null));
    assert_valid("input", what, schema, &cleaned)?;
    Ok(cleaned)
}

/// `canonicalIdentity(schema, value)`：与 schema 默认值相等的字段被省略后的规范 JSON。
///
/// 步骤：先用 `as_schema` 归一化 schema（`false` → `{not:{}}`，非对象 → `{}`），再用
/// `strip_defaults` 递归删除"值与该层子 schema 的 `default` 深相等"的对象键（数组按
/// `items` 递归，tuple 按下标），最后 [`canonical_json`]。默认值比较用的是
/// [`deep_equal`]。
pub fn canonical_identity(schema: &Value, value: &Value) -> String {
    let schema = as_schema(schema);
    canonical_json(&strip_defaults(&schema, value))
}

/// `stripDefaults(schema, value)`：递归删除"值等于该层子 schema `default`"的对象键。
///
/// 数组按 `items` 递归：tuple schema 按下标取子 schema，单个 item schema 则对每个元素复用；
/// 非数组/对象值原样返回。对象键缺失对应 `properties` 时按空 schema 处理（不会删除）。
/// 只用于 [`canonical_identity`]。
fn strip_defaults(schema: &Value, value: &Value) -> Value {
    if let Value::Array(items) = value {
        let items_schema = schema.get("items").cloned().unwrap_or(Value::Null);
        if let Value::Array(tuple_schemas) = &items_schema {
            // tuple：第 i 项用 `items[i]`，越界按空 schema。
            return Value::Array(
                items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        let child = tuple_schemas.get(index).cloned().unwrap_or(Value::Null);
                        strip_defaults(&as_schema(&child), item)
                    })
                    .collect(),
            );
        }
        return Value::Array(
            items
                .iter()
                .map(|item| strip_defaults(&as_schema(&items_schema), item))
                .collect(),
        );
    }
    let Value::Object(map) = value else {
        return value.clone();
    };
    let properties = schema.get("properties").cloned().unwrap_or(Value::Null);
    let mut result = Map::new();
    for (key, item) in map {
        // 键的 schema 取 `properties[key]`；缺失按空 schema（`{}`）处理。
        let property = as_schema(properties.get(key).unwrap_or(&Value::Null));
        if let Some(default) = property.get("default")
            && deep_equal(default, item)
        {
            // 与子 schema 的默认值深相等：省略此键。
            continue;
        }
        result.insert(key.clone(), strip_defaults(&property, item));
    }
    Value::Object(result)
}

// ---------------------------------------------------------------------------
// fits（`schema.ts` 的 `fits`/`objectFits`/`arrayFits`/`functionFits`）
// ---------------------------------------------------------------------------

const CONSTRAINTS: [&str; 14] = [
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minProperties",
    "maxProperties",
];

const KNOWN: [&str; 26] = [
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minProperties",
    "maxProperties",
    "type",
    "properties",
    "required",
    "additionalProperties",
    "patternProperties",
    "items",
    "additionalItems",
    "const",
    "enum",
    "anyOf",
    "parameters",
    "returnType",
];

const ANNOTATIONS: [&str; 10] = [
    "title",
    "description",
    "default",
    "examples",
    "$id",
    "$schema",
    "$comment",
    "readOnly",
    "writeOnly",
    "deprecated",
];

fn without_annotations(schema: &Value) -> Value {
    match schema {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, value) in map {
                // `~kind` 在 TS 里不可枚举，`Object.entries` 看不见它；Rust 侧必须显式丢弃，
                // 否则每个活 schema 都会因"未建模关键字"而不 fits。
                if key != KIND_KEY && !ANNOTATIONS.contains(&key.as_str()) {
                    out.insert(key.clone(), value.clone());
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

fn schema_types(schema: &Value) -> Option<Vec<String>> {
    let map = schema.as_object()?;
    match map.get("type") {
        Some(Value::String(text)) => Some(vec![text.clone()]),
        Some(Value::Array(items)) => Some(
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect(),
        ),
        _ => None,
    }
}

/// 保守结构子类型：`sub` 的每个合法值都是 `sup` 的合法值（算法顺序照抄 TS）。
///
/// 方向是 `sub ⊆ sup`，非对称（不判断等价）。近似规则：两侧先做 `as_schema` 归一化、
/// 去掉注解关键字并剥除 `~kind`；归一化结果完全相同的 schema 由紧随其后的 `deep_equal`
/// 短路直接返回 `true`（即使带 `not`/`if` 等未建模关键字或 schema 形式的
/// `additionalProperties`）。在两侧结果**不同**的前提下，sup 含本文件未建模的关键字、sup 的
/// `additionalProperties` 不是 boolean、或 sup 的某个约束关键字取值与 sub 不同（sub
/// 缺省也算不同）时，返回 `false`。crate 内调用方：`contract/coverage.rs`（provider 与
/// consumer 的 input/参数/结果/state 兼容性）与 `contract/mod.rs` 的 [`crate::contract::input_fits`]
/// 便利封装；本文件末尾的测试也直接调用。
pub fn fits(sub_schema: &Value, sup_schema: &Value) -> bool {
    // `withoutAnnotations` 在 TS 里是浅层过滤；`~kind` 的"不可见"是整棵树的性质（嵌套 schema
    // 也带标记），因此这里递归剥除标记后再比较。只进入 schema 位置，不碰 `const`/`enum` 数据。
    let sub = strip_live_markers(&without_annotations(&as_schema(sub_schema)));
    let sup = strip_live_markers(&without_annotations(&as_schema(sup_schema)));
    let sup_map = match &sup {
        Value::Object(map) => map,
        _ => return true,
    };
    if sup_map.is_empty() {
        // 空 schema（`{}` 或 `true`）接受一切。
        return true;
    }
    if deep_equal(&sub, &sup) {
        // 归一化后完全相同，直接通过。
        return true;
    }
    if sup_map.keys().any(|key| !KNOWN.contains(&key.as_str())) {
        // sup 用了本文件未建模的关键字：无法证明子类型关系，保守判否。
        return false;
    }
    if let Some(additional) = sup_map.get("additionalProperties")
        && !additional.is_boolean()
    {
        // sup 对额外属性逐键约束时无法枚举证明，同样判否。
        return false;
    }
    let sub_map = match &sub {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    if let Some(Value::Array(branches)) = sub_map.get("anyOf") {
        // 子侧是联合：每个分支都必须 fits sup。
        return branches.iter().all(|branch| fits(branch, &sup));
    }
    if let Some(Value::Array(branches)) = sup_map.get("anyOf") {
        // 父侧是联合：命中的任一分支即可。
        return branches.iter().any(|branch| fits(&sub, branch));
    }
    if let Some(const_value) = sup_map.get("const") {
        // sup 限定 const：sub 必须也是这个常量。
        return sub_map
            .get("const")
            .is_some_and(|value| deep_equal(value, const_value));
    }
    if let Some(Value::Array(allowed)) = sup_map.get("enum") {
        let values: Option<Vec<Value>> = if let Some(const_value) = sub_map.get("const") {
            Some(vec![const_value.clone()])
        } else if let Some(Value::Array(values)) = sub_map.get("enum") {
            Some(values.clone())
        } else {
            None
        };
        return values.is_some_and(|values| {
            values
                .iter()
                .all(|item| allowed.iter().any(|allowed| deep_equal(allowed, item)))
        });
    }
    // 约束关键字必须逐字相等：sup 有而 sub 没有，视为 sub 更宽，判否。
    for key in CONSTRAINTS {
        if sup_map.contains_key(key) {
            let equal = match (sub_map.get(key), sup_map.get(key)) {
                (Some(a), Some(b)) => deep_equal(a, b),
                (None, Some(_)) => false,
                _ => true,
            };
            if !equal {
                return false;
            }
        }
    }
    let (Some(sup_types), Some(sub_types)) = (schema_types(&sup), schema_types(&sub)) else {
        // 任一侧没有 `type`：无法比较值域。
        return false;
    };
    for sub_type in &sub_types {
        // 类型逐项包含；`integer` 是 `number` 的子集。
        if !sup_types.contains(sub_type)
            && !(sub_type == "integer" && sup_types.iter().any(|t| t == "number"))
        {
            return false;
        }
    }
    if sub_map.contains_key("const") || sub_map.contains_key("enum") {
        // 子侧带 const/enum：不再做对象/数组/函数的结构检查。
        return true;
    }
    // 结构检查按子类型分别进行。
    if sub_types.iter().any(|t| t == "object") && !object_fits(&sub, &sup) {
        return false;
    }
    if sub_types.iter().any(|t| t == "array") && !array_fits(&sub, &sup) {
        return false;
    }
    if sub_types.iter().any(|t| t == "function") {
        return function_fits(&sub, &sup);
    }
    true
}

/// `subProperties[key]` / `subPatterns[pattern]`：自有键 → 值；原型链上的名字 →
/// `Object.prototype` 的函数，而 `asSchema(function)` 是 `{}`，fits 时等价于空 schema。
fn inherited_property_value(map: &Map<String, Value>, key: &str) -> Option<Value> {
    match map.get(key) {
        Some(value) => Some(value.clone()),
        None if inherits_js_property_key(map, key) => Some(Value::Object(Map::new())),
        None => None,
    }
}

/// `objectFits(sub, sup)`：对象 schema 的结构比较。
///
/// 1. sup 的每个 `required` 也必须是 sub 的 required（sub 可以额外要求更多必填字段，
///    这只会让 sub 更窄，不影响通过）；
/// 2. sup 声明的每个属性：sub 有对应 schema 时必须更窄；sub 没有对应属性时，若 sub 的
///    `additionalProperties` 是对象形式（`typeof` 语义，`null`/数组也算），还要求它能覆盖
///    该属性，否则跳过这一项（不会仅因缺属性就判否）；
/// 3. sup 的 `additionalProperties: false` 要求 sub 也禁止额外属性，且 sub 不声明 sup
///    未知的属性；
/// 4. sup 的 `patternProperties` 要求 sub 的额外属性/同名 pattern 覆盖且更窄，sub 自己的
///    pattern 名也必须出现在 sup 中。
fn object_fits(sub: &Value, sup: &Value) -> bool {
    let sub_properties = sub.get("properties").cloned().unwrap_or(Value::Null);
    let sup_properties = sup.get("properties").cloned().unwrap_or(Value::Null);
    let sub_required: Vec<String> = sub
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let sup_required: Vec<String> = sup
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    // sub 必须覆盖 sup 的全部必填字段。
    if sup_required.iter().any(|key| !sub_required.contains(key)) {
        return false;
    }
    let sub_properties_map = sub_properties.as_object().cloned().unwrap_or_default();
    let sup_properties_map = sup_properties.as_object().cloned().unwrap_or_default();
    // sup 的每个属性都要在 sub 中有更窄的定义，或由 sub 的 additionalProperties 覆盖。
    for (key, property) in &sup_properties_map {
        // `key in subProperties`：原型链上的名字也算"有"，其值是 `Object.prototype` 的函数
        // （`asSchema(function)` = `{}`）。
        if let Some(sub_property) = inherited_property_value(&sub_properties_map, key) {
            if !fits(&sub_property, property) {
                return false;
            }
        } else if sub
            .get("additionalProperties")
            // `typeof sub.additionalProperties === "object"`：null 与数组也算。
            .is_some_and(is_typeof_object)
            && !fits(
                sub.get("additionalProperties").unwrap_or(&Value::Null),
                property,
            )
        {
            return false;
        }
    }
    // sup 禁止额外属性：sub 也必须禁止，且不能声明 sup 没声明的属性。
    if sup.get("additionalProperties") == Some(&Value::Bool(false)) {
        if sub.get("additionalProperties") != Some(&Value::Bool(false)) {
            return false;
        }
        if sub_properties_map
            .keys()
            .any(|key| !has_js_property_key(&sup_properties_map, key))
        {
            return false;
        }
    }
    let sup_patterns = sup.get("patternProperties").cloned().unwrap_or(Value::Null);
    let sup_patterns_map = sup_patterns.as_object().cloned().unwrap_or_default();
    // patternProperties：sup 的每个 pattern 都要被 sub 的属性/pattern 覆盖。
    if !sup_patterns_map.is_empty() {
        let sub_patterns = sub.get("patternProperties").cloned().unwrap_or(Value::Null);
        let sub_patterns_map = sub_patterns.as_object().cloned().unwrap_or_default();
        if sub.get("additionalProperties") != Some(&Value::Bool(false))
            && sub_patterns_map.is_empty()
        {
            return false;
        }
        for (pattern, property) in &sup_patterns_map {
            for (key, sub_property) in &sub_properties_map {
                if !has_js_property_key(&sup_properties_map, key)
                    && pattern_matches(pattern, key)
                    && !fits(sub_property, property)
                {
                    return false;
                }
            }
            if !sub_patterns_map.is_empty() {
                match inherited_property_value(&sub_patterns_map, pattern) {
                    Some(sub_pattern) => {
                        if !fits(&sub_pattern, property) {
                            return false;
                        }
                    }
                    None => return false,
                }
            }
        }
        for pattern in sub_patterns_map.keys() {
            if !has_js_property_key(&sup_patterns_map, pattern) {
                return false;
            }
        }
    }
    true
}

/// `arrayFits(sub, sup)`：数组 schema 的结构比较。
///
/// sup 是 tuple（`items` 为数组）时，sub 也必须是等长 tuple 且逐项 fits；sub 是 tuple 而
/// sup 是单个 schema 时，sub 的每一项都要 fits sup 的 item schema；其余情况直接比较两侧的
/// `items`（缺失的 `items` 按 `null` → 空 schema 处理）。
fn array_fits(sub: &Value, sup: &Value) -> bool {
    let sup_items = sup.get("items").cloned().unwrap_or(Value::Null);
    let sub_items = sub.get("items").cloned().unwrap_or(Value::Null);
    if let Value::Array(sup_tuple) = &sup_items {
        let Value::Array(sub_tuple) = &sub_items else {
            return false;
        };
        if sub_tuple.len() != sup_tuple.len() {
            return false;
        }
        return sub_tuple
            .iter()
            .enumerate()
            .all(|(index, item)| fits(item, &sup_tuple[index]));
    }
    if let Value::Array(sub_tuple) = &sub_items {
        return sub_tuple.iter().all(|item| fits(item, &sup_items));
    }
    fits(&sub_items, &sup_items)
}

/// `functionFits(sub, sup)`：函数 schema 的比较。
///
/// 参数**逆变**：sub 的参数个数不能多于 sup，且 sup 的第 i 个参数类型必须 fits sub 的第
/// i 个（sub 能接受更宽，调用方才安全）；返回**协变**：sub 的 `returnType` 必须 fits sup 的。
fn function_fits(sub: &Value, sup: &Value) -> bool {
    let sub_parameters: Vec<Value> = sub
        .get("parameters")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let sup_parameters: Vec<Value> = sup
        .get("parameters")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if sub_parameters.len() > sup_parameters.len() {
        return false;
    }
    for (index, parameter) in sub_parameters.iter().enumerate() {
        // 逆变：`fits(supParameters[index], parameter)`。
        if !fits(&sup_parameters[index], parameter) {
            return false;
        }
    }
    fits(
        sub.get("returnType").unwrap_or(&Value::Null),
        sup.get("returnType").unwrap_or(&Value::Null),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chat_strict() -> Value {
        json!({
            "type": "object",
            "required": ["session", "conversation"],
            "properties": {
                "session": {"type": "string"},
                "conversation": {"type": "number", "default": 0}
            },
            "additionalProperties": false
        })
    }

    /// 关键回归：`Default`/`Clean` 按 `~kind` 分派。经 JSON 传输后的 schema（没有标记）
    /// 是近空操作；只有本进程内构造的活 schema（`mark_live_schema`）才注入默认值/清洗字段。
    #[test]
    fn json_schemas_do_not_default_or_clean() {
        let schema = chat_strict();
        assert_eq!(
            default_value(&schema, json!({"session": "s"})),
            json!({"session": "s"})
        );
        assert_eq!(
            clean_value(&schema, json!({"session": "s", "extra": 1})),
            json!({"session": "s", "extra": 1})
        );

        let live = mark_live_schema(&schema);
        assert_eq!(
            default_value(&live, json!({"session": "s"})),
            json!({"session": "s", "conversation": 0})
        );
        assert_eq!(
            clean_value(&live, json!({"session": "s", "extra": 1})),
            json!({"session": "s"})
        );
    }

    /// 关键回归：JSON schema 上唯一的存活行为是**顶层** `default`（`IsDefault` 不看 `~kind`）。
    #[test]
    fn only_the_top_level_default_survives_json() {
        assert_eq!(
            default_from(&json!({"type": "number", "default": 5}), None),
            Some(json!(5))
        );
        // 没有 `~kind`：`FromObject` 不递归，属性默认值不生效。
        let schema = json!({
            "type": "object",
            "properties": {"conversation": {"type": "number", "default": 0}}
        });
        assert_eq!(default_from(&schema, Some(json!({}))), Some(json!({})));
        // 活 schema 才递归。
        let live = mark_live_schema(&schema);
        assert_eq!(
            default_from(&live, Some(json!({}))),
            Some(json!({"conversation": 0}))
        );
    }

    /// 关键回归：Tuple 的 Default 会补齐到 `items.length`（缺省槽位写 undefined → JSON null），
    /// Clean 会裁掉多余元素；JSON schema 两者都不做。
    #[test]
    fn tuple_default_pads_and_clean_slices_only_when_live() {
        let schema = json!({"type": "array", "items": [{"type": "number", "default": 1}]});
        assert_eq!(default_value(&schema, json!([])), json!([]));
        assert_eq!(clean_value(&schema, json!([2, 3])), json!([2, 3]));

        let live = mark_live_schema(&schema);
        assert_eq!(default_value(&live, json!([])), json!([1]));
        assert_eq!(
            default_value(
                &mark_live_schema(&json!({"type": "array", "items": [{"type": "number"}]})),
                json!([])
            ),
            json!([null]),
            "missing slots become undefined → null"
        );
        assert_eq!(clean_value(&live, json!([2, 3])), json!([2]));
    }

    /// 关键回归：`Clean` 的 `HasPropertyKey(type.properties, key)` 走原型链：
    /// `toString` 等名字算"已声明"而原样保留；`constructor`/`__proto__` 用
    /// `hasOwnProperty`（不在自有属性里 → 当未知键删除）。
    #[test]
    fn clean_object_keeps_prototype_names_but_not_unsafe_ones() {
        let live = mark_live_schema(&json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "additionalProperties": false
        }));
        let value = json!({"a": "x", "toString": 1, "normal": 2, "constructor": 3, "__proto__": 4});
        assert_eq!(clean_value(&live, value), json!({"a": "x", "toString": 1}));
        // 没有 properties 也一样：`toString` 通过原型链命中。
        let empty = mark_live_schema(
            &json!({"type": "object", "properties": {}, "additionalProperties": false}),
        );
        assert_eq!(
            clean_value(&empty, json!({"toString": 1, "normal": 2})),
            json!({"toString": 1})
        );
        // `additionalProperties` 是 schema 时，原型名与普通未知键都按它检查/清洗。
        let loose = mark_live_schema(&json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "additionalProperties": {"type": "number", "default": 7}
        }));
        assert_eq!(
            clean_value(
                &loose,
                json!({"a": "x", "toString": 1, "normal": 2, "bad": "z"})
            ),
            json!({"a": "x", "toString": 1, "normal": 2})
        );
    }

    /// 关键回归：`normalize` 的 `value ?? {}`：`null` 与缺失一样换成 `{}`。
    #[test]
    fn normalize_coalesces_null_to_empty_object() {
        let live = mark_live_schema(&chat_strict());
        let error = normalize(&live, Some(&Value::Null), "input").expect_err("missing session");
        assert_eq!(
            error.message,
            "input: / must have required properties session"
        );
        assert_eq!(
            normalize(
                &mark_live_schema(&json!({"type": "object", "properties": {}})),
                Some(&Value::Null),
                "input"
            )
            .expect("empty object"),
            json!({})
        );
    }

    /// 关键回归：`pattern` 走 JS 的 `new RegExp(pattern, "u")`：`\w`/`\d`/`\b` 是 ASCII，
    /// `.` 不匹配行终止符，lookahead/反向引用可用；非法 pattern 在 TS 里抛 `SyntaxError`，
    /// Rust 侧不 panic：`check` 视为不匹配，`normalize`/`assert_valid` 返回 `unavailable` 错误
    /// （reviewer 发现 #14 的回归测试）。
    #[test]
    fn pattern_uses_javascript_unicode_regexp() {
        let check_pattern = |pattern: &str, value: &str| {
            check(
                &json!({"type": "string", "pattern": pattern}),
                &Value::String(value.to_owned()),
            )
        };
        assert!(!check_pattern("^\\w+$", "é"));
        assert!(check_pattern("^\\w+$", "abc"));
        assert!(!check_pattern("^\\d+$", "١٢"));
        assert!(!check_pattern("a.b", "a\nb"));
        assert!(check_pattern("a.b", "axb"));
        assert!(check_pattern("(?=a)a", "abc"));
        assert!(check_pattern("(a)\\1", "aa"));
        assert!(!check_pattern("^.$", "\n"));

        // 非法 pattern：不 panic，`check` 返回 false（TS 里 `Value.Check` 会抛 `SyntaxError`，
        // 由 `normalize`/`assert_valid` 转成可捕获的 `unavailable` 错误）。
        assert!(!check_pattern("[", "a"));
        let schema = json!({"type": "string", "pattern": "["});
        let error = normalize(&schema, Some(&Value::String("a".to_owned())), "input")
            .expect_err("invalid pattern must be reported");
        assert_eq!(error.code, ChordErrorCode::Unavailable);
        assert!(
            error
                .message
                .starts_with("SyntaxError: Invalid regular expression: /[/u:"),
            "unexpected message: {error}"
        );
        let error = assert_valid("input", "input", &schema, &json!("a"))
            .expect_err("invalid pattern must be reported");
        assert_eq!(error.code, ChordErrorCode::Unavailable);
    }

    /// 关键回归：`additionalItems` 的错误 `schemaPath` 是 `#/additionalItems`，且首个失败后短路。
    #[test]
    fn additional_items_errors_use_the_additional_items_path() {
        let schema = json!({
            "type": "array",
            "items": [{"type": "string"}],
            "additionalItems": {"type": "number"}
        });
        let errors = errors(&schema, &json!([5, "b", "c"]));
        assert_eq!(
            errors.len(),
            2,
            "the first failing extra item short-circuits; `/2` is not reported: {errors:?}"
        );
        assert_eq!(errors[0]["keyword"], "type");
        assert_eq!(errors[0]["schemaPath"], "#/additionalItems");
        assert_eq!(errors[0]["instancePath"], "/1");
        assert_eq!(errors[1]["schemaPath"], "#/items/0");
    }

    /// 关键回归：两个 `anyOf` 分支都必须**直接返回**（不是"只在失败时返回 false"）。
    #[test]
    fn fits_anyof_branches_return_immediately() {
        assert!(fits(
            &json!({"anyOf": [{"type": "string"}]}),
            &json!({"type": "string"})
        ));
        assert!(fits(
            &json!({"type": "string"}),
            &json!({"anyOf": [{"type": "string"}, {"type": "number"}]})
        ));
        assert!(!fits(
            &json!({"type": "boolean"}),
            &json!({"anyOf": [{"type": "string"}, {"type": "number"}]})
        ));
        // 活 schema 的 `~kind` 对 fits 不可见。
        let live_object =
            mark_live_schema(&json!({"type": "object", "properties": {"a": {"type": "string"}}}));
        assert!(fits(
            &live_object,
            &json!({"type": "object", "properties": {"a": {"type": "string"}}})
        ));
    }

    /// 关键回归：`typeof sub.additionalProperties === "object"` 把 `null` 与数组也算作对象。
    #[test]
    fn object_fits_uses_typeof_object_for_additional_properties() {
        assert!(!fits(
            &json!({"type": "object", "additionalProperties": null}),
            &json!({"type": "object", "properties": {"a": {"type": "string"}}})
        ));
        assert!(!fits(
            &json!({"type": "object", "additionalProperties": [{"type": "string"}]}),
            &json!({"type": "object", "properties": {"a": {"type": "string"}}})
        ));
    }

    /// 关键回归：`Default` 把数组当 `Guard.IsObject` 处理（`FromObject` 的 `additionalProperties`
    /// 会作用到每个索引上）。
    #[test]
    fn default_object_treats_arrays_as_objects() {
        let live = mark_live_schema(&json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "additionalProperties": {"type": "object", "required": ["n"], "properties": {"n": {"type": "number", "default": 9}}}
        }));
        assert_eq!(default_value(&live, json!([{}])), json!([{"n": 9}]));
    }

    /// 与 `mark_live_schema` 对称：序列化路径剥掉标记，且不碰 `default` 里的同名数据。
    #[test]
    fn strip_live_markers_removes_only_schema_markers() {
        let schema = json!({
            "type": "object",
            "properties": {"a": {"type": "object", "properties": {"b": {"type": "number", "default": 7}}}},
            "default": {"~kind": "data"}
        });
        let marked = mark_live_schema(&schema);
        assert_eq!(strip_live_markers(&marked), schema);
    }

    /// 关键回归（major）：format 语义按 TypeBox 实现：email 大小写不敏感、uri-reference
    /// 拒绝 `12:30`、idn-hostname 接受 Unicode 标签。
    #[test]
    fn formats_match_typebox() {
        assert!(format_test("email", "A@B.CO"));
        assert!(format_test("email", "A.B@C.D"));
        assert!(!format_test("uri-reference", "12:30"));
        assert!(format_test("uri-reference", "//h/p"));
        assert!(format_test("idn-hostname", "例え.テスト"));
        assert!(format_test("idn-email", "ü@b.co"));
        assert!(!format_test("ipv4", "256.1.1.1"));
    }
}
