//! api/constrained-sampling.ts。
//!
//! 受约束采样：把工具 schema 收紧为 provider 的 strict 子集（JSON schema），以及 OpenAI
//! grammar 工具（lark / regex）的流式输入拼装。
//!
//! TS: `packages/ai/src/api/constrained-sampling.ts`。
//!
//! ## 对象键序
//!
//! TS 在同一份对象上原地修改，`Object.keys` 的顺序是「整数索引键（升序）→ 插入序」。
//! Rust 侧在入口用 [`kiana_json::js_object::sort_js_keys`] 把整棵 schema 归一成 JS 键序，
//! 之后 `serde_json::Map`（`preserve_order`）的迭代序就与 JS 一致；由于所有输出最终都经
//! `js_object::stringify` 序列化，字节结果与 TS 相同。
//!
//! # 为什么需要这个模块
//!
//! 工具定义由上层用普通 JSON Schema 给出，但 provider 对工具参数有两套互不相同的协议，
//! 不能直接下发：
//!
//! 1. **strict JSON schema**（OpenAI、Anthropic、Bedrock、Google 等适配器都会用到）：
//!    strict 模式只接受 JSON Schema 的一个很窄子集——根必须是 `type: "object"`，对象必须
//!    `additionalProperties: false`，`required` 必须列全所有属性，可选属性只能用
//!    `anyOf: [<schema>, { "type": "null" }]` 表达「可空」。本模块把工具 schema 收紧成该
//!    子集，并在做不到时按 `strict: "prefer" | "require"` 回退或报错。
//! 2. **grammar 工具**（OpenAI 的 custom tool + `lark`/`regex` 语法）：provider 在流式响应里
//!    逐块下发的是**纯文本**，不是 JSON。本模块把它实时包装成普通 function 工具那样的参数
//!    文本 `{"<属性名>":"<转义后的输入>"}`，让上层用同一条路径累积工具参数。
//!
//! 两套逻辑都从 `Tool` 的 `constrained_sampling` 字段分派，错误文本共享同一套契约。
//!
//! # 上下游
//!
//! 上游依赖：`crate::types`（`Tool`、`ConstrainedSampling`、`StrictMode`）、`serde_json`
//! （`Value`/`Map`）、`indexmap`（有序映射）以及 `kiana_json` 的 JS 语义助手。下游全部在
//! crate 内的 provider 适配器里，分两个阶段调用：
//!
//! - 请求构建：`api::openai_completions`（`convert_tools`）、`api::openai_responses_shared`
//!   （`convert_responses_tools`）、`api::openai_responses`、`api::azure_openai_responses`、
//!   `api::openai_codex_responses`、`api::anthropic_messages`、`api::bedrock_converse_stream`、
//!   `api::google_shared`、`api::mistral_conversations`（`to_function_tools`，只使用
//!   JSON-schema strict 路径，不调用 grammar 相关函数）。
//! - 流式事件处理：`openai_completions` 与 `openai_responses_shared` 用 grammar buffer
//!   （[`GrammarToolInputJsonBuffer`]）把 `custom_tool_call_input.delta` 之类的文本块拼成
//!   JSON 参数；[`get_grammar_tool_input`] 则在消息转换时取回最终文本。
//!
//! # 一次典型调用
//!
//! 1. 适配器读 `Tool.constrained_sampling`，先用 [`resolve_grammar_constrained_sampling`]
//!    判断是否走 grammar 工具；再对剩余工具用 [`resolve_json_schema_strict_sampling`] 判断
//!    能否 strict。
//! 2. 真正下发的参数由 [`get_json_schema_tool_parameters`] 生成：strict 时它内部再次调用
//!    [`make_strict_json_schema`]（不做缓存）。
//! 3. 流式阶段：grammar 工具的每个文本块经 [`append_grammar_tool_input_json_delta`] 变成
//!    JSON 片段；结束时 [`close_grammar_tool_input_json`] 补齐收尾引号与 `}`。
//!
//! # 约束与失败模式
//!
//! - **错误文本是外部契约**：所有 `AiError.message` 都是稳定契约，
//!   改动任何字符串都会破坏 fixture 的逐字节对照。主要错误分支由
//!   `tests/fixtures/api-shared/constrained_sampling.json` 的 `cases` 锁定，但覆盖不完整：
//!   黑名单里只覆盖 `$ref`/`$defs`/`allOf` 三个键，`boolean schemas are unsupported` 以及
//!   `definitions`/`oneOf`/`patternProperties` 等其余 13 个黑名单键的报错文本没有任何
//!   fixture 或测试断言（布尔根用例走的是 `root schema must have type object`）；
//!   `Cannot read properties of null (reading 'type')` 只在 `tests/constrained_sampling.rs`
//!   中断言，不在 fixture 里。完整逐字文本以 `spec/ai-api-anthropic-openai.md`
//!   §2.7.4/§2.7.10 为准。
//! - **错误 kind 统一为 [`crate::error::AiErrorKind::Request`]**：这些错误表示「请求参数
//!   不可用」，不是传输/解析失败；适配器用 `?` 传播，最终变成同步 `Err` 或流里的错误事件。
//! - **无 IO、无锁、无全局状态**。唯一的跨调用状态是调用方持有的
//!   [`GrammarToolInputJsonBuffer`]：append/close 会修改它的 `input`/`started`/`closed`，
//!   因此这两个函数不是纯函数。`&mut` 参数只保证同一时刻不存在对同一 buffer 的别名可变
//!   借用；若调用方把 buffer 放进 `Mutex`，多个线程仍可串行调用它，模块本身不提供额外的
//!   并发保证。
//! - **不修改入参**：`make_strict_json_schema` 先深拷贝（`sort_js_keys` 返回新树），
//!   `get_json_schema_tool_parameters` 在非 strict 分支返回克隆。
//! - **JS 语义必须显式复刻**：键序、`String.prototype.trim`、`JSON.stringify` 走
//!   `kiana_json`（`sort_js_keys`、`js_trim`、`js_object::stringify`）；真值由本文件的
//!   `js_truthy` 复刻（规则与 `kiana_json::js_string::is_truthy` 相同，但没有调用它）。
//!   不能用 `HashMap`、`str::trim`、`serde_json::to_string` 之类的近似物。
//! - 逐条协议细节（每条错误消息的触发条件与顺序）见 `spec/ai-api-anthropic-openai.md` §2.7。

use std::collections::HashSet;

use serde_json::{Map, Value, json};

use crate::types::{
    ConstrainedSampling, ConstrainedSamplingConfig, GrammarVariants, StrictMode, Tool,
};

/// `UNSUPPORTED_STRICT_SCHEMA_KEYS`（顺序固定：先命中先报错）。
///
/// 这些关键字不在 provider strict 子集允许的范围内（引用、组合、模式属性、条件等），
/// 命中即拒绝，而不是尝试内联或降级。顺序是错误文本契约的一部分：多个关键字同时出现时，
/// 报的是本数组中**最先**出现的那个，因此不能改成字典序或其它顺序。
pub const UNSUPPORTED_STRICT_SCHEMA_KEYS: [&str; 16] = [
    "$ref",
    "$defs",
    "definitions",
    "allOf",
    "oneOf",
    "patternProperties",
    "dependentSchemas",
    "dependencies",
    "unevaluatedProperties",
    "propertyNames",
    "contains",
    "prefixItems",
    "not",
    "if",
    "then",
    "else",
];

/// Returns true when a provider's strict mode rejects this schema keyword with this value.
///
/// TS: `src/api/constrained-sampling.ts#UnsupportedStrictSchemaKeywordCheck`
///
/// 固定黑名单之外的 provider 扩展检查：适配器可以借此拒绝自家 strict 模式不接受的额外
/// 关键字（例如 `api::anthropic_messages` 的 `is_anthropic_strict_unsupported_keyword`）。
///
/// 调用约定：对每个被访问节点的每一对键值按该对象的 JS 键序调用一次；`key` 是键名，
/// `value` 是该键的 JSON 值。返回 `true` 表示不支持，错误文本为
/// `` `<key>: <JSON.stringify(value)> is unsupported` ``。
///
/// 闭包只在本模块的调用期间被借用，不会被保存；类型要求 `Send + Sync`，所以实现不能捕获
/// 非线程安全的可变状态。
pub type UnsupportedStrictSchemaKeywordCheck = dyn Fn(&str, &Value) -> bool + Send + Sync;

/// `UnsupportedStrictJsonSchemaError`：内部用于区分「schema 不受支持」与其它错误。
///
/// 只在 `make_json_schema_node_strict` 的 `Result` 里出现（TS 用 `instanceof` 做同样的
/// 区分）；`make_strict_json_schema` 立刻把它转成 `AiError`，因此它不会出现在公开 API 的
/// 错误类型里。
#[derive(Debug, Clone, PartialEq, Eq)]
struct UnsupportedStrictJsonSchemaError {
    message: String,
}

impl UnsupportedStrictJsonSchemaError {
    fn new(message: impl Into<String>) -> Self {
        UnsupportedStrictJsonSchemaError {
            message: message.into(),
        }
    }
}

/// 普通 `Error`（TS 里的非 `UnsupportedStrictJsonSchemaError`）。
///
/// 统一使用 [`crate::error::AiErrorKind::Request`]：这些都是「请求参数不可用」。上层主要
/// 消费 `message`——它会被写进会话文件、UI 错误框与诊断。
fn ai_error(message: impl Into<String>) -> crate::error::AiError {
    crate::error::AiError::new(crate::error::AiErrorKind::Request, message)
}

// TS 的 `UnsupportedStrictJsonSchemaError` 只是用 `instanceof` 区分分支；Rust 侧由
// `make_json_schema_node_strict` 的返回类型表达该区分，跨出本模块后两类错误都归为
// `AiErrorKind::Request`，因此这里只是语义化的别名。
fn unsupported_error(message: impl Into<String>) -> crate::error::AiError {
    ai_error(message)
}

/// `isJsonSchemaObject(value)`。
///
/// TS 的条件是「object 且非 `null` 且非数组」；`serde_json` 用不同的 `Value` 变体区分
/// 对象/数组/`null`，所以 `is_object()` 一个判断就够。
fn is_json_schema_object(value: &Value) -> bool {
    value.is_object()
}

/// `isStructuredSchema(schema)`。
///
/// 只用于拒绝 `anyOf` 里的**对象/数组联合**。判定故意宽松：`type` 是字符串时看它本身，
/// 是数组时看是否包含 `"object"`/`"array"`；即使没有 `type`，只要出现 `properties` 或
/// `items` 键也算结构化。非对象返回 false。
fn is_structured_schema(schema: &Value) -> bool {
    let Value::Object(map) = schema else {
        return false;
    };
    let includes_type = |name: &str| match map.get("type") {
        Some(Value::String(value)) => value == name,
        Some(Value::Array(values)) => values
            .iter()
            .any(|value| matches!(value, Value::String(value) if value == name)),
        _ => false,
    };
    includes_type("object")
        || includes_type("array")
        || map.get("properties").is_some()
        || map.get("items").is_some()
}

/// `schemaAllowsNull(schema)`。
///
/// 判断属性 schema 是否**已经**显式允许 `null`；`make_json_schema_node_strict` 用它避免
/// 给可空属性重复套一层 `anyOf`。识别四种写法：`type: "null"`、`type` 数组含 `"null"`、
/// `const: null`、`enum` 含 `null`，外加递归检查 `anyOf` 的每个分支。非对象一律 false。
fn schema_allows_null(schema: &Value) -> bool {
    let Value::Object(map) = schema else {
        return false;
    };
    match map.get("type") {
        Some(Value::String(value)) if value == "null" => return true,
        Some(Value::Array(values))
            if values
                .iter()
                .any(|value| matches!(value, Value::String(value) if value == "null")) =>
        {
            return true;
        }
        _ => {}
    }
    if matches!(map.get("const"), Some(Value::Null)) {
        return true;
    }
    if let Some(Value::Array(values)) = map.get("enum")
        && values.iter().any(Value::is_null)
    {
        return true;
    }
    if let Some(Value::Array(variants)) = map.get("anyOf")
        && variants.iter().any(schema_allows_null)
    {
        return true;
    }
    false
}

/// JS 真值语义（`Object.entries` 里的值、`schema.properties?.[key]` 判定用）。
///
/// 复刻 JS 的 falsy 集合：`null`/`undefined`、`false`、`0`（含 `-0`）、`NaN`、空串；
/// **空数组与空对象也为真**——这与很多语言不同，是 `infer_grammar_input_property` 判断
/// `properties[key]` 是否存在的依据。
fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value
            .as_f64()
            .is_some_and(|number| number != 0.0 && !number.is_nan()),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `JSON.stringify(value)`（与 JS 键序/数字格式一致）。
///
/// 用在两处外部可见输出：自定义关键字错误文本里的值，以及 grammar 参数文本的键名与增量。
/// 两者都要与 TS 逐字节相同，所以不能用 `serde_json::to_string`（键序、数字格式、转义
/// 细节都可能不同）。
fn js_stringify(value: &Value) -> String {
    kiana_json::js_object::stringify(value)
}

/// JSON 字符串字面量的内部片段（`JSON.stringify(s).slice(1, -1)`）。
///
/// 先整体序列化再切掉首尾引号，而不是自己写转义：转义表（引号、反斜杠、控制字符）由
/// `kiana_json` 统一保证与 V8 一致。返回的片段要拼进一个已经打开的双引号字符串内部。
fn json_string_inner(value: &str) -> String {
    let quoted = js_stringify(&Value::String(value.to_string()));
    // 首尾必是 ASCII 引号，按字节切安全。
    quoted[1..quoted.len() - 1].to_string()
}

/// `makeJsonSchemaNodeStrict(schema, isUnsupportedKeyword?)`（原地修改）。
///
/// 对单个 schema 节点做 strict 化，并递归处理子节点。**执行顺序是错误契约的一部分**：
/// TS 用「先抛哪个」决定 `error.message`，所以下面各步的先后不能调整。
///
/// 1. 非对象节点（布尔 schema、`null` 等）→ `boolean schemas are unsupported`。
/// 2. 按 [`UNSUPPORTED_STRICT_SCHEMA_KEYS`] 的固定顺序扫描，命中即报
///    `` `<key> schemas are unsupported` ``。
/// 3. provider 自定义检查：遍历**本节点**的键（JS 的 `Object.entries` 顺序），命中即报
///    `` `<key>: <JSON.stringify(value)> is unsupported` ``。这里的顺序依赖入口的
///    `sort_js_keys`：整数样式的键在 JS 里会排到前面。
/// 4. `anyOf`：必须是**非空数组**；任一分支是结构化 schema（见 `is_structured_schema`）
///    即拒绝；其余分支逐个递归。
/// 5. `items`：数组形式是元组，拒绝；其余形式递归（布尔/`null` 会在第 1 步报错）。
/// 6. `properties` 存在但 `type != "object"` → `properties require type object`。
/// 7. 非对象 schema 到此**提前返回**（下面只对对象 schema 有意义）。
/// 8. 对象专属校验：`additionalProperties` 只允许缺省或 `false`；`properties` 必须是对象；
///    `required` 必须是字符串数组；`required` 里的名字必须都在 `properties` 中。
/// 9. 递归每个属性；**不在 `required` 中且不允许 `null`** 的属性原地替换为
///    `{ "anyOf": [<原属性>, { "type": "null" }] }`。
/// 10. 把 `required` 改写为**全部**属性名（顺序 = `properties` 的键序），并写入
///     `additionalProperties: false`。
///
/// # 不变量
/// - 入参 `schema` 必须来自 [`make_strict_json_schema`] 深拷贝并归一键序后的树；本函数直接
///   原地修改它，调用方的原树不会被碰到。
/// - 返回 `Ok` 时该节点及其所有子节点都满足 strict 子集；但第 9 步插入的 `null` 包装分支
///   是可信的，**不会**再走一遍校验。
/// - 键在序列化输出里的**位置**有意义：`serde_json::Map::insert` 对已存在的键只改值、保持
///   位置（与 JS 赋值一致），新键追加到末尾。
/// - 递归深度 = schema 嵌套深度，函数不设上限。
fn make_json_schema_node_strict(
    schema: &mut Value,
    is_unsupported_keyword: Option<&UnsupportedStrictSchemaKeywordCheck>,
) -> Result<(), UnsupportedStrictJsonSchemaError> {
    let Value::Object(map) = schema else {
        return Err(UnsupportedStrictJsonSchemaError::new(
            "boolean schemas are unsupported",
        ));
    };

    // 1) 固定黑名单：按数组顺序扫描，先命中的决定错误文本。
    for key in UNSUPPORTED_STRICT_SCHEMA_KEYS {
        if map.contains_key(key) {
            return Err(UnsupportedStrictJsonSchemaError::new(format!(
                "{key} schemas are unsupported"
            )));
        }
    }

    // 2) provider 自定义检查：按本对象自身的键序（入口已归一成 JS 键序）遍历。
    if let Some(check) = is_unsupported_keyword {
        for (key, value) in map.iter() {
            if check(key, value) {
                return Err(UnsupportedStrictJsonSchemaError::new(format!(
                    "{key}: {} is unsupported",
                    js_stringify(value)
                )));
            }
        }
    }

    // 3) anyOf：strict 不支持对象/数组联合；先要求非空数组，再逐分支检查与递归。
    if map.contains_key("anyOf") {
        match map.get_mut("anyOf") {
            Some(Value::Array(variants)) if !variants.is_empty() => {
                for variant in variants.iter_mut() {
                    if is_structured_schema(variant) {
                        return Err(UnsupportedStrictJsonSchemaError::new(
                            "object and array unions are unsupported",
                        ));
                    }
                    make_json_schema_node_strict(variant, is_unsupported_keyword)?;
                }
            }
            _ => {
                return Err(UnsupportedStrictJsonSchemaError::new(
                    "anyOf must contain at least one schema",
                ));
            }
        }
    }

    // 4) items：数组形式是元组（不支持）；其余递归，布尔/null 会在第 1 步报错。
    if map.contains_key("items") {
        match map.get_mut("items") {
            Some(Value::Array(_)) => {
                return Err(UnsupportedStrictJsonSchemaError::new(
                    "tuple schemas are unsupported",
                ));
            }
            Some(items) => make_json_schema_node_strict(items, is_unsupported_keyword)?,
            None => unreachable!("contains_key(\"items\") guarantees a value"),
        }
    }

    // 5) properties 只能出现在对象 schema 上；只对对象 schema 继续做下面的校验，
    //    非对象在这里返回（additionalProperties/required 对它们没有意义）。
    let is_object_schema =
        matches!(map.get("type"), Some(Value::String(value)) if value == "object");
    if map.get("properties").is_some() && !is_object_schema {
        return Err(UnsupportedStrictJsonSchemaError::new(
            "properties require type object",
        ));
    }
    if !is_object_schema {
        return Ok(());
    }

    // 6) 对象专属校验：additionalProperties 只允许缺省或 false。
    if let Some(additional_properties) = map.get("additionalProperties")
        && !matches!(additional_properties, Value::Bool(false))
    {
        return Err(UnsupportedStrictJsonSchemaError::new(
            "schema-valued or true additionalProperties is unsupported",
        ));
    }
    // 7) properties 必须是 schema 的映射，required 必须是字符串数组。
    if let Some(properties) = map.get("properties")
        && !is_json_schema_object(properties)
    {
        return Err(UnsupportedStrictJsonSchemaError::new(
            "object properties must be a schema map",
        ));
    }
    if let Some(required) = map.get("required")
        && !matches!(required, Value::Array(values) if values.iter().all(Value::is_string))
    {
        return Err(UnsupportedStrictJsonSchemaError::new(
            "object required must be a string array",
        ));
    }

    // 8) required 不能引用 properties 里不存在的名字；下一步又会在校验通过后把它改写成
    //    全量属性名，所以这里必须先按「原 required」检查。
    let property_names: Vec<String> = match map.get("properties") {
        Some(Value::Object(properties)) => properties.keys().cloned().collect(),
        _ => Vec::new(),
    };
    let required_set: HashSet<String> = match map.get("required") {
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect(),
        _ => HashSet::new(),
    };
    if required_set
        .iter()
        .any(|key| !property_names.iter().any(|name| name == key))
    {
        return Err(UnsupportedStrictJsonSchemaError::new(
            "required contains an unknown property",
        ));
    }

    // 9) 递归收紧每个属性；不在 required 中且不允许 null 的属性包一层 anyOf + null。
    //    新插入的 null 分支是可信的，不再递归校验。
    if let Some(Value::Object(properties)) = map.get_mut("properties") {
        let keys: Vec<String> = properties.keys().cloned().collect();
        for key in keys {
            let needs_null_wrapper = {
                let property = properties
                    .get_mut(&key)
                    .expect("key came from this map's own key list");
                make_json_schema_node_strict(property, is_unsupported_keyword)?;
                !required_set.contains(&key) && !schema_allows_null(property)
            };
            if needs_null_wrapper {
                let original = properties
                    .get(&key)
                    .cloned()
                    .expect("key came from this map's own key list");
                // 原地替换：`Map::insert` 对已存在的键保持位置（与 JS 赋值一致）。
                properties.insert(key, json!({ "anyOf": [original, { "type": "null" }] }));
            }
        }
    }

    // 10) strict 契约：required 列全所有属性（顺序 = properties 的键序），
    //     additionalProperties 必须为 false。
    map.insert(
        "required".to_string(),
        Value::Array(property_names.into_iter().map(Value::String).collect()),
    );
    map.insert("additionalProperties".to_string(), Value::Bool(false));
    Ok(())
}

/// `makeStrictJsonSchema(schema, isUnsupportedKeyword?)`。
///
/// 深拷贝后收紧；失败时的 `AiError::message` 逐字等于 TS 的错误文本。
///
/// TS: `src/api/constrained-sampling.ts#makeStrictJsonSchema`
///
/// 返回**新树**：`sort_js_keys` 负责深拷贝并把整棵树的键序归一成 JS 顺序（见模块文档），
/// 所以调用方持有的 `schema` 不会被修改。
///
/// `is_unsupported_keyword` 是可选的 provider 扩展检查（见
/// [`UnsupportedStrictSchemaKeywordCheck`]），会对每个被访问的节点生效，而不只是根。
///
/// 两次根检查不能合并：转换前要求根是对象，转换后还要求 `type == "object"`。像只有一个
/// `anyOf`（且分支都不是对象/数组 schema）的根能通过节点转换——它不是对象 schema，会提前
/// 返回——但 provider 的 strict 模式要求工具参数根必须是对象。
///
/// # 错误
/// 均为 [`crate::error::AiErrorKind::Request`]，`message` 逐字等于 TS：例如
/// `root schema must have type object`、`boolean schemas are unsupported`、
/// `$ref schemas are unsupported`。触发顺序见 `make_json_schema_node_strict` 的文档。
// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定，签名保持不变。
#[allow(clippy::result_large_err)]
pub fn make_strict_json_schema(
    schema: &Value,
    is_unsupported_keyword: Option<&UnsupportedStrictSchemaKeywordCheck>,
) -> Result<Value, crate::error::AiError> {
    // `structuredClone` + JS 键序归一（见模块文档）。
    let mut cloned = kiana_json::js_object::sort_js_keys(schema);
    if !is_json_schema_object(&cloned) {
        return Err(unsupported_error("root schema must have type object"));
    }
    make_json_schema_node_strict(&mut cloned, is_unsupported_keyword)
        .map_err(|error| unsupported_error(error.message))?;
    // 节点转换对「没有 type 的根」（例如只有 anyOf 的根）会提前返回 Ok，所以必须再查一次。
    if !matches!(cloned.get("type"), Some(Value::String(value)) if value == "object") {
        return Err(unsupported_error("root schema must have type object"));
    }
    Ok(cloned)
}

/// `getJsonSchemaToolParameters(tool, strict)`。
///
/// `strict === true` 时返回收紧后的 schema，否则原样返回（不缓存：每次调用都重新转换）。
///
/// TS: `src/api/constrained-sampling.ts#getJsonSchemaToolParameters`
///
/// `strict` 保留三态：`None` 对应 TS 的 `undefined`，`Some(false)` 对应显式 `false`，
/// 只有 `Some(true)` 才会转换。非 strict 分支返回 `tool.parameters` 的克隆，调用方可以自由
/// 修改而不影响工具定义。
///
/// # 错误
/// 只有 strict 分支会失败，转换错误原样向上传播。本函数**不**处理
/// `strict: "prefer" | "require"` 的回退——那由 [`resolve_json_schema_strict_sampling`]
/// 决定；走到这里时 `Some(true)` 是调用方明确要求 strict，schema 仍可能不可转换。
// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定，签名保持不变。
#[allow(clippy::result_large_err)]
pub fn get_json_schema_tool_parameters(
    tool: &Tool,
    strict: Option<bool>,
) -> Result<Value, crate::error::AiError> {
    if strict == Some(true) {
        make_strict_json_schema(&tool.parameters, None)
    } else {
        Ok(tool.parameters.clone())
    }
}

/// `GrammarFormat`（TS 的 `"lark" | "regex"`）。
///
/// 取值经 [`GrammarFormat::as_str`] 写进请求体里的 grammar 格式对象（字段名 `syntax`；
/// 具体嵌套层级由各适配器的线格式决定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GrammarFormat {
    /// `openai_lark` 变体。
    Lark,
    /// `openai_regex` 变体。
    Regex,
}

impl GrammarFormat {
    /// 字面量。
    ///
    /// 返回 `"lark"` / `"regex"`，即 provider 的线格式取值；不要换成枚举名或大写形式。
    pub fn as_str(self) -> &'static str {
        match self {
            GrammarFormat::Lark => "lark",
            GrammarFormat::Regex => "regex",
        }
    }
}

/// `GrammarConstrainedSampling`。
///
/// TS: `src/api/constrained-sampling.ts#GrammarConstrainedSampling`
///
/// resolver 的产物：适配器据此把工具改写成 custom/grammar 工具（`syntax` 取
/// `format.as_str()`、`definition` 取本结构的 `definition`），并在流式阶段按
/// `input_property` 把纯文本包成 JSON 参数。`definition` 是变体原文：只对「是否空白」做过
/// 判断，不会 trim 后再下发。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrammarConstrainedSampling {
    /// grammar 语法。
    pub format: GrammarFormat,
    /// grammar 定义。
    pub definition: String,
    /// 承载输入字符串的工具参数名。
    pub input_property: String,
}

/// `GrammarToolInputJsonBuffer`。
///
/// TS: `src/api/constrained-sampling.ts#GrammarToolInputJsonBuffer`
///
/// 一次 grammar 工具调用对应一个 buffer，由调用方持有；本模块不保存任何全局状态。
/// `(started, closed)` 构成单向状态机：
///
/// | `started` | `closed` | 含义 |
/// |---|---|---|
/// | false | false | 尚未收到输入；首次非空 append 会先输出 `{"<prop>":"` |
/// | true | false | 接收中；append 只输出转义后的增量 |
/// | true | true | 已收尾；append 报错，close 仅在 final 缺省或与已收文本相同时幂等返回 `None` |
///
/// 由本模块函数维护的不变量：`closed == true` 蕴含 `started == true`（close 一定会先补上
/// 开头），且状态不可回退。字段是 `pub` 的，调用方手工构造时需自行保持这些约束。
///
/// 不变量：`input` 是**原始**文本（未做 JSON 转义），可直接当作参数值使用；转义只出现在
/// append/close 的返回值里。结构本身不记录属性名，调用方必须对同一 buffer 始终传同一个
/// `input_property`，否则产出的 JSON 键与内容会不一致——函数不会检测这一点。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GrammarToolInputJsonBuffer {
    /// 已收到的输入。
    pub input: String,
    /// 是否已产出 JSON 参数文本的开头。
    pub started: bool,
    /// 是否已 close。
    pub closed: bool,
}

/// `getGrammarToolInput(toolName, arguments_, inputProperty)`。
///
/// TS: `src/api/constrained-sampling.ts#getGrammarToolInput`
///
/// 从**已解析**的工具参数里取出 grammar 输入文本：`arguments[input_property]` 必须是 JSON
/// 字符串，缺失或任何非字符串都报
/// `Grammar tool call "<toolName>" requires argument "<inputProperty>" to be a string.`。
/// `tool_name` 只进错误文本，不参与查找。
///
/// 用途：消息转换时把历史工具调用重新编码成 provider 的 custom tool 结构（见
/// `api::openai_completions` / `api::openai_responses_shared`）。
// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定，签名保持不变。
#[allow(clippy::result_large_err)]
pub fn get_grammar_tool_input(
    tool_name: &str,
    arguments: &Map<String, Value>,
    input_property: &str,
) -> Result<String, crate::error::AiError> {
    match arguments.get(input_property) {
        Some(Value::String(input)) => Ok(input.clone()),
        _ => Err(ai_error(format!(
            "Grammar tool call \"{tool_name}\" requires argument \"{input_property}\" to be a string."
        ))),
    }
}

/// The opening of the JSON arguments text, once.
///
/// 首次调用把 `started` 置 true，返回 `{"<JSON 转义的属性名>":"`（末尾已有一个开引号，
/// 后续内容由调用方拼接）；之后的调用返回空串。属性名走 `JSON.stringify`，任意键名都能
/// 生成合法的 JSON。
fn grammar_tool_input_json_start(
    buffer: &mut GrammarToolInputJsonBuffer,
    input_property: &str,
) -> String {
    if buffer.started {
        return String::new();
    }
    buffer.started = true;
    format!(
        "{{{}:\"",
        js_stringify(&Value::String(input_property.to_string()))
    )
}

/// `appendGrammarToolInputJsonDelta(buffer, inputProperty, inputDelta)`。
///
/// 返回追加片段对应的 JSON 参数文本；空 delta 返回 `None`。复杂度只与 delta 线性相关。
///
/// TS: `src/api/constrained-sampling.ts#appendGrammarToolInputJsonDelta`
///
/// # 行为
/// - `buffer.closed` → 返回错误（`...changed after it was closed`），buffer 不变。
/// - 空 `input_delta` → `Ok(None)`，且不改变任何状态（连 `started` 也不置位）。
/// - 否则先追加进 `buffer.input`，再返回「首次时的开头 + delta 的 JSON 转义内容」；该内容
///   **不含**外层引号，供调用方拼在已经打开的字符串里。
///
/// # 为什么分块转义是安全的
/// JSON 字符串转义是逐字符映射，没有跨字符上下文；`&str` 的块边界一定落在字符边界上，
/// 所以逐块转义后拼接与整段一次转义逐字节相同。历史内容从不重新序列化，这就是
/// 「复杂度只与 delta 线性相关」的原因。
// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定，签名保持不变。
#[allow(clippy::result_large_err)]
pub fn append_grammar_tool_input_json_delta(
    buffer: &mut GrammarToolInputJsonBuffer,
    input_property: &str,
    input_delta: &str,
) -> Result<Option<String>, crate::error::AiError> {
    if buffer.closed {
        return Err(ai_error(format!(
            "grammar tool input for property \"{input_property}\" changed after it was closed"
        )));
    }
    if input_delta.is_empty() {
        return Ok(None);
    }
    buffer.input.push_str(input_delta);
    let start = grammar_tool_input_json_start(buffer, input_property);
    Ok(Some(format!("{start}{}", json_string_inner(input_delta))))
}

/// `closeGrammarToolInputJson(buffer, inputProperty, finalInput)`。
///
/// 收尾并返回剩余 JSON 参数文本；重复 close 且输入不变返回 `None`。
///
/// TS: `src/api/constrained-sampling.ts#closeGrammarToolInputJson`
///
/// # 行为（所有错误都在修改 buffer 之前返回）
/// 1. 已 `closed`：`final_input` 为 `None` 或与 `buffer.input` 相同 → `Ok(None)`（幂等）；
///    否则报 `...changed after it was closed`。
/// 2. 未 `closed` 且给了 `final_input`：它必须以已收文本为前缀，否则报
///    `...changed non-monotonically`——provider 的最终文本与流式片段矛盾。
/// 3. 计算剩余后缀 `rest`（无 final 时为 `""`），有 final 时把 `buffer.input` 更新为它，置
///    `closed = true`，返回「开头（若从未输出过）+ 转义后的 rest + `"}`」。
///
/// 空 buffer 直接 close 会一次产出完整的 `{"prop":""}`。返回前 `buffer.input` 已更新为最终
/// 文本，调用方可以直接读取。
// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定，签名保持不变。
#[allow(clippy::result_large_err)]
pub fn close_grammar_tool_input_json(
    buffer: &mut GrammarToolInputJsonBuffer,
    input_property: &str,
    final_input: Option<&str>,
) -> Result<Option<String>, crate::error::AiError> {
    if buffer.closed {
        return match final_input {
            None => Ok(None),
            Some(final_input) if final_input == buffer.input => Ok(None),
            Some(_) => Err(ai_error(format!(
                "grammar tool input for property \"{input_property}\" changed after it was closed"
            ))),
        };
    }
    if let Some(final_input) = final_input
        && !final_input.starts_with(&buffer.input)
    {
        return Err(ai_error(format!(
            "grammar tool input for property \"{input_property}\" changed non-monotonically"
        )));
    }
    let rest = final_input.map_or("", |final_input| &final_input[buffer.input.len()..]);
    // 前缀已在上面用 starts_with 校验过；len() 是字节长度，而前缀比较按字节进行，
    // 所以这个下标一定落在字符边界上，切片安全。
    if let Some(final_input) = final_input {
        buffer.input = final_input.to_string();
    }
    buffer.closed = true;
    let start = grammar_tool_input_json_start(buffer, input_property);
    Ok(Some(format!("{start}{}\"}}", json_string_inner(rest))))
}

/// `inferGrammarInputProperty(tool)`。
///
/// 从工具 schema 推出「承载 grammar 输入的唯一必填字符串属性」：provider 的 grammar
/// custom tool 只有一个自由文本输入。要求依次为：
///
/// 1. `parameters` 是对象且 `type == "object"`；
/// 2. `required` 是**恰好一个**字符串；
/// 3. `properties[required[0]]` 存在且为 JS 真值；
/// 4. 该属性的 `type == "string"`。
///
/// 任一条不满足就返回 `AiError`（kind Request），message 见
/// `spec/ai-api-anthropic-openai.md` §2.7.10；调用方
/// [`resolve_grammar_constrained_sampling`] 会把 message 包进更长的错误文本。
///
/// # JS 兼容
/// `parameters` 为 JSON `null` 时，TS 在 `schema.type` 上抛 V8 `TypeError`，其 message 被
/// 外层 `catch` 原样拼接；这里显式复现。布尔/数字/字符串/数组的 `parameters` 在 JS 里属性
/// 访问不抛错（取到 `undefined`），因此落到第 1 条错误。Rust 侧 `Tool.parameters` 是必填
/// 字段，不存在 `undefined` 形态。
// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定，签名保持不变。
#[allow(clippy::result_large_err)]
fn infer_grammar_input_property(tool: &Tool) -> Result<String, crate::error::AiError> {
    let Some(schema) = tool.parameters.as_object() else {
        // TS 直接读 `schema.type`：`parameters` 为 `null` 时是 TypeError，其 message 被外层
        // catch 原样拼进错误文本；布尔/数字/字符串/数组则只是取到 `undefined`。
        if tool.parameters.is_null() {
            return Err(ai_error("Cannot read properties of null (reading 'type')"));
        }
        return Err(ai_error(
            "grammar constrained sampling requires an object parameter schema",
        ));
    };
    if !matches!(schema.get("type").and_then(Value::as_str), Some("object")) {
        return Err(ai_error(
            "grammar constrained sampling requires an object parameter schema",
        ));
    }
    let input_property = match schema.get("required") {
        Some(Value::Array(required)) if required.len() == 1 && required[0].is_string() => {
            required[0].as_str().expect("checked is_string").to_string()
        }
        _ => {
            return Err(ai_error(
                "grammar constrained sampling requires exactly one required string property",
            ));
        }
    };
    let property = schema
        .get("properties")
        .and_then(Value::as_object)
        .and_then(|properties| properties.get(&input_property));
    if !property.is_some_and(js_truthy) {
        return Err(ai_error(format!(
            "grammar constrained sampling requires a properties entry for {input_property}"
        )));
    }
    let property_type = property
        .and_then(Value::as_object)
        .and_then(|property| property.get("type"));
    if property_type.and_then(Value::as_str) != Some("string") {
        return Err(ai_error(format!(
            "grammar constrained sampling property {input_property} must have type string"
        )));
    }
    Ok(input_property)
}

/// `typeof value === "string" && value.trim().length > 0`。
///
/// JS 的 `String.prototype.trim` 去掉 U+FEFF（BOM），`str::trim` 不去；必须用 JS 空白集合。
///
/// 只用于判断变体「是否算已提供」；返回的 definition 保持原文（不 trim）。两个具体的差异点
/// 由测试 `javascript_trim_governs_grammar_variants` 锁定：U+FEFF 在 JS 里是空白（Rust 的
/// `str::trim` 不认为），U+0085（NEL）在 JS 里不是空白（Rust 认为是）。
fn grammar_variant_is_set(value: &str) -> bool {
    !kiana_json::js_string::js_trim(value).is_empty()
}

/// 取出 grammar 配置的变体。
///
/// TS: `const larkDefinition = config.variants.openai_lark;` / `config.variants.openai_regex;`
/// —— 这两行在 `try` **之前**，`variants` 缺失或为 `null` 时抛出未包装的 V8 TypeError，
/// 消息逐字保留。其他非对象 `variants`（字符串 / 数字 / 数组）的属性访问不抛错，视作未设置。
///
/// 返回值里的两份定义都是原文；非字符串值按「未设置」（`None`）处理，对应 TS 的
/// `typeof value === "string"`。
///
/// - `Config` 是反序列化时已归一的规范形状，直接克隆；
/// - `Raw` 保留任意 JSON，因此按 TS 的运行期属性访问语义手工取值（`variants` 是字符串、
///   数字、布尔、数组时取不到属性，等价于未设置）；
/// - 其余分支是防御：唯一调用方 [`resolve_grammar_constrained_sampling`] 已先按 grammar
///   过滤。
#[allow(clippy::result_large_err)]
fn grammar_variants_of(
    sampling: &ConstrainedSampling,
) -> Result<GrammarVariants, crate::error::AiError> {
    match sampling {
        ConstrainedSampling::Config(ConstrainedSamplingConfig::Grammar { variants }) => {
            Ok(variants.clone())
        }
        ConstrainedSampling::Raw(value) => {
            let Some(variants) = value.get("variants") else {
                return Err(ai_error(
                    "Cannot read properties of undefined (reading 'openai_lark')",
                ));
            };
            if variants.is_null() {
                return Err(ai_error(
                    "Cannot read properties of null (reading 'openai_lark')",
                ));
            }
            let object = variants.as_object();
            let variant = |key: &str| {
                object
                    .and_then(|object| object.get(key))
                    .and_then(crate::types::JsonValue::as_str)
                    .map(str::to_string)
            };
            Ok(GrammarVariants {
                openai_lark: variant("openai_lark"),
                openai_regex: variant("openai_regex"),
            })
        }
        _ => Ok(GrammarVariants {
            openai_lark: None,
            openai_regex: None,
        }),
    }
}

/// `resolveJsonSchemaStrictSampling(tool, supportsStrictMode, isUnsupportedKeyword?)`。
///
/// `prefer` 在 schema 无法收紧时回落到非 strict（`None`）；`require` 报错（消息逐字）。
///
/// TS: `src/api/constrained-sampling.ts#resolveJsonSchemaStrictSampling`
///
/// # 返回值
/// - `Some(true)`：转换成功，应当以 strict 下发。
/// - `None`：该工具没有 strict 结论（没有配置、`strict: "prefer"` 转换失败，或 provider
///   不支持 strict）。它不表示「最终非 strict」：适配器可以再叠加自己的默认值决定最终
///   是否 strict，例如 `openai_responses_shared::convert_responses_tools` 按
///   `constrainedStrict ?? defaultStrict`（spec §2.12.7）合并 `options.strict`（缺省
///   `false`）；当 `options.strict` 为 `Some(true)` 时，即使本函数返回 `None`，最终仍以
///   `strict: true` 下发并调用 [`make_strict_json_schema`]。
/// - 永不返回 `Some(false)`；是否真的写出 `strict` 字段由适配器决定。
///
/// # 判定顺序
/// 1. 只认 `constrained_sampling.type == "json_schema"`；`false`、缺省、未知 type、非对象值
///    都返回 `None`。非规范形状留在 `Raw` 里，按 TS 的运行期语义解释：只有字面
///    `"require"` 才算 require。
/// 2. `supports_strict_mode` 为真时试转换一次，**结果丢弃**，只为判断能否转换；失败时
///    `prefer` 静默回落，`require` 报 `...but <原因>.`。
/// 3. provider 不支持 strict 时，`require` 报 `...but strict tools are unsupported.`，
///    `prefer` 回落。
///
/// 同一工具稍后会由 [`get_json_schema_tool_parameters`] 再转换一次（不做缓存）。
// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定，签名保持不变。
#[allow(clippy::result_large_err)]
pub fn resolve_json_schema_strict_sampling(
    tool: &Tool,
    supports_strict_mode: bool,
    is_unsupported_keyword: Option<&UnsupportedStrictSchemaKeywordCheck>,
) -> Result<Option<bool>, crate::error::AiError> {
    let strict = match &tool.constrained_sampling {
        Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema { strict })) => {
            *strict
        }
        // 非规范形状（缺 `strict`、额外键……）保留在 `Raw` 里；TS 运行期读
        // `config.type` / `config.strict`，只看 `strict === "require"`。
        Some(ConstrainedSampling::Raw(value))
            if value.get("type").and_then(crate::types::JsonValue::as_str)
                == Some("json_schema") =>
        {
            match value
                .get("strict")
                .and_then(crate::types::JsonValue::as_str)
            {
                Some("require") => StrictMode::Require,
                _ => StrictMode::Prefer,
            }
        }
        _ => return Ok(None),
    };

    // 试转换只用于判定：成功与否决定 strict 能否启用，转换结果本身在这里丢弃。
    if supports_strict_mode {
        match make_strict_json_schema(&tool.parameters, is_unsupported_keyword) {
            Ok(_) => Ok(Some(true)),
            Err(error) => {
                // 只有 "require" 把「不可转换」升级为错误；"prefer" 回落到非 strict。
                if strict != StrictMode::Require {
                    return Ok(None);
                }
                Err(ai_error(format!(
                    "Tool \"{}\" requires JSON-schema constrained sampling, but {}.",
                    tool.name, error.message
                )))
            }
        }
    } else if strict == StrictMode::Require {
        Err(ai_error(format!(
            "Tool \"{}\" requires JSON-schema constrained sampling, but strict tools are unsupported.",
            tool.name
        )))
    } else {
        Ok(None)
    }
}

/// `resolveGrammarConstrainedSampling(tool, supportsOpenAIGrammarTools)`。
///
/// lark 优先于 regex；无可用变体或参数形状不合法时抛错（消息逐字）。
///
/// TS: `src/api/constrained-sampling.ts#resolveGrammarConstrainedSampling`
///
/// # 返回值
/// - `None`：不是 grammar 配置，或 provider 不支持 grammar 工具。
/// - `Some(...)`：`format` 取 lark/regex（lark 优先），`definition` 是变体原文。
///
/// # 顺序（决定错误文本）
/// 1. 先按 `type == "grammar"` 过滤，其它配置直接 `None`；
/// 2. 再判 `supports_openai_grammar_tools`——为 false 时直接 `None`，**不读 variants**
///    （因此缺失的 variants 不会触发 TypeError，见测试）；
/// 3. 读两个变体（`Raw` 缺 `variants`/为 `null` 时复现 V8 TypeError 文本）；
/// 4. 用 JS trim 判断「是否非空白」；都为空则报
///    `...no supported grammar variant was provided.`；
/// 5. 推断输入属性，错误被包成
///    `Tool "<name>" cannot use grammar constrained sampling: <原因>.`（注意外层追加的句点）。
// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定，签名保持不变。
#[allow(clippy::result_large_err)]
pub fn resolve_grammar_constrained_sampling(
    tool: &Tool,
    supports_openai_grammar_tools: bool,
) -> Result<Option<GrammarConstrainedSampling>, crate::error::AiError> {
    // TS 先判 `config.type !== "grammar"`，再判 `supportsOpenAIGrammarTools`，最后才读
    // `config.variants.openai_lark`——缺少/为 null 的 `variants` 只在支持 grammar 时才
    // 触发 TypeError（见 [`grammar_variants_of`]）。
    let sampling = match tool.constrained_sampling.as_ref() {
        Some(sampling @ ConstrainedSampling::Config(ConstrainedSamplingConfig::Grammar { .. })) => {
            sampling
        }
        Some(sampling @ ConstrainedSampling::Raw(value))
            if value.get("type").and_then(crate::types::JsonValue::as_str) == Some("grammar") =>
        {
            sampling
        }
        _ => return Ok(None),
    };

    if !supports_openai_grammar_tools {
        return Ok(None);
    }

    let variants = grammar_variants_of(sampling)?;

    // 只把「非空白」的变体视为已提供；定义本身保持原文（不 trim）。
    let lark = variants
        .openai_lark
        .as_deref()
        .filter(|value| grammar_variant_is_set(value));
    let regex = variants
        .openai_regex
        .as_deref()
        .filter(|value| grammar_variant_is_set(value));
    if lark.is_none() && regex.is_none() {
        return Err(ai_error(format!(
            "Tool \"{}\" cannot use grammar constrained sampling: no supported grammar variant was provided.",
            tool.name
        )));
    }

    match infer_grammar_input_property(tool) {
        Ok(input_property) => {
            let has_lark = lark.is_some();
            Ok(Some(GrammarConstrainedSampling {
                format: if has_lark {
                    GrammarFormat::Lark
                } else {
                    GrammarFormat::Regex
                },
                definition: lark
                    .or(regex)
                    .expect("one of the variants is set")
                    .to_string(),
                input_property,
            }))
        }
        Err(error) => Err(ai_error(format!(
            "Tool \"{}\" cannot use grammar constrained sampling: {}.",
            tool.name, error.message
        ))),
    }
}

/// `createGrammarToolInputProperties(tools, supportsOpenAIGrammarTools)`。
///
/// 返回 `工具名 → 输入参数名` 的有序映射；同名工具后者覆盖前者。
///
/// TS: `src/api/constrained-sampling.ts#createGrammarToolInputProperties`
///
/// `tools` 为 `None` 时返回空表。迭代顺序 = `tools` 的顺序；同名工具用 `IndexMap::insert`
/// 覆盖值但保留首次出现的位置（与 TS `Map.set` 一致）。
///
/// 任一工具解析失败都会立刻返回错误（`?`），不会跳过坏工具：调用点在请求构建阶段，
/// 错误应在发请求前暴露，而不是等到流式阶段。
// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定，签名保持不变。
#[allow(clippy::result_large_err)]
pub fn create_grammar_tool_input_properties(
    tools: Option<&[Tool]>,
    supports_openai_grammar_tools: bool,
) -> Result<indexmap::IndexMap<String, String>, crate::error::AiError> {
    let mut properties = indexmap::IndexMap::new();
    for tool in tools.unwrap_or(&[]) {
        if let Some(grammar) =
            resolve_grammar_constrained_sampling(tool, supports_openai_grammar_tools)?
        {
            properties.insert(tool.name.clone(), grammar.input_property);
        }
    }
    Ok(properties)
}

// 测试与 TS 的 `constrained-sampling.test.ts` 对照：fixture 在
// `tests/fixtures/api-shared/constrained_sampling.json`，逐字比对错误文本与序列化结果，
// 因此这里的断言是上面「错误文本/字节序」契约的执行者，不要为了让测试通过而改实现字符串。
#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    const FIXTURE: &str = include_str!("../../tests/fixtures/api-shared/constrained_sampling.json");

    fn fixture() -> Value {
        serde_json::from_str(FIXTURE).expect("fixture parses")
    }

    fn stringify(value: &Value) -> String {
        kiana_json::js_object::stringify(value)
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct GrammarExpected {
        format: String,
        definition: String,
        input_property: String,
    }

    fn assert_grammar_matches(
        expected: &Value,
        actual: Result<Option<GrammarConstrainedSampling>, crate::error::AiError>,
    ) {
        if let Some(error) = expected.get("error").and_then(Value::as_str) {
            assert_eq!(actual.expect_err("expected an error").message, error);
            return;
        }
        if expected.get("ok").is_none() {
            assert_eq!(actual.expect("expected success"), None);
            return;
        }
        let grammar = actual
            .expect("expected success")
            .expect("expected a grammar");
        let expected: GrammarExpected =
            serde_json::from_value(expected.get("ok").expect("expected.ok").clone())
                .expect("grammar shape");
        assert_eq!(grammar.format.as_str(), expected.format);
        assert_eq!(grammar.definition, expected.definition);
        assert_eq!(grammar.input_property, expected.input_property);
    }

    fn assert_resolve_matches(
        expected: &Value,
        actual: Result<Option<bool>, crate::error::AiError>,
    ) {
        if let Some(error) = expected.get("error").and_then(Value::as_str) {
            assert_eq!(actual.expect_err("expected an error").message, error);
            return;
        }
        match expected.get("ok").and_then(Value::as_bool) {
            Some(value) => assert_eq!(actual.expect("expected success"), Some(value)),
            None => assert_eq!(actual.expect("expected success"), None),
        }
    }

    fn tool(value: &Value) -> Tool {
        serde_json::from_value(value.clone()).expect("tool fixture parses")
    }

    #[test]
    fn strict_schema_matches_typescript_reference() {
        let fixture = fixture();
        let parameters = &fixture["parameters"];
        let strict = make_strict_json_schema(parameters, None).expect("schema converts");
        assert_eq!(
            stringify(&strict),
            fixture["strictString"].as_str().unwrap()
        );
        // 输入不被修改（`structuredClone`）。
        assert_eq!(
            stringify(parameters),
            fixture["strictAfterInputString"].as_str().unwrap()
        );
    }

    #[test]
    fn unsupported_cases_match_typescript_reference() {
        let fixture = fixture();
        for case in fixture["cases"].as_array().expect("cases array") {
            let name = case["name"].as_str().unwrap();
            let result = make_strict_json_schema(&case["parameters"], None);
            if let Some(error) = case.get("error").and_then(Value::as_str) {
                assert_eq!(
                    result.expect_err(name).message,
                    error,
                    "case {name} should fail with the reference message"
                );
            } else {
                let result = result.unwrap_or_else(|error| panic!("case {name}: {error}"));
                assert_eq!(
                    stringify(&result),
                    case["resultString"].as_str().unwrap(),
                    "case {name} should match the reference output"
                );
            }
        }
    }

    #[test]
    fn custom_keyword_check_matches_typescript_reference() {
        let fixture = fixture();
        let check = |key: &str, _value: &Value| key == "x";
        let error = make_strict_json_schema(
            &serde_json::json!({ "type": "object", "properties": { "b": { "type": "number", "x": { "y": 1 } } } }),
            Some(&check),
        )
        .expect_err("custom keyword rejected");
        assert_eq!(
            error.message,
            fixture["customKeyword"]["error"].as_str().unwrap()
        );

        // `Object.entries` 顺序 = 插入顺序：两个键都命中时先报先出现的那个。
        let ordered = |key: &str, _value: &Value| key == "zz" || key == "aa";
        let error = make_strict_json_schema(
            &serde_json::json!({
                "type": "object",
                "properties": { "a": { "type": "string", "zz": 1, "aa": 2 } }
            }),
            Some(&ordered),
        )
        .expect_err("custom keyword rejected");
        assert_eq!(
            error.message,
            fixture["customKeywordOrder"]["error"].as_str().unwrap()
        );

        let pass = |key: &str, _value: &Value| key == "zzz";
        let result = make_strict_json_schema(
            &serde_json::json!({ "type": "object", "properties": { "b": { "type": "number" } } }),
            Some(&pass),
        )
        .expect("converts");
        assert_eq!(
            stringify(&result),
            fixture["customKeywordPass"]["resultString"]
                .as_str()
                .unwrap()
        );
    }

    #[test]
    fn resolve_json_schema_strict_matches_typescript_reference() {
        let fixture = fixture();
        let resolve = &fixture["resolve"];
        for name in [
            "prefer",
            "preferNoStrictSupport",
            "requireNoStrictSupport",
            "preferUnsupported",
            "requireUnsupported",
            "noConfig",
            "disabled",
        ] {
            let case = &resolve[name];
            let supports_strict = !name.contains("NoStrictSupport");
            let actual =
                resolve_json_schema_strict_sampling(&tool(&case["tool"]), supports_strict, None);
            assert_resolve_matches(&case["expected"], actual);
        }
    }

    #[test]
    fn resolve_grammar_matches_typescript_reference() {
        let fixture = fixture();
        let grammar = &fixture["grammar"];
        for name in [
            "lark",
            "regex",
            "larkPreferred",
            "none",
            "unsupported",
            "badSchema",
            "noProps",
            "notString",
            "twoRequired",
            "propertiesEntryFalse",
            "notObjectParameters",
            // 缺失 / `null` 的 `variants`：TS 在 `try` 之前读 `config.variants.openai_lark`，
            // 抛出未包装的 V8 TypeError（逐字）。
            "missingVariants",
            "nullVariants",
        ] {
            let case = &grammar[name];
            let supports = name != "unsupported";
            let actual = resolve_grammar_constrained_sampling(&tool(&case["tool"]), supports);
            assert_grammar_matches(&case["expected"], actual);
        }

        let lark_tool = tool(&grammar["lark"]["tool"]);
        let map =
            create_grammar_tool_input_properties(Some(std::slice::from_ref(&lark_tool)), true)
                .expect("grammar map");
        let expected: Vec<(String, String)> =
            serde_json::from_value(grammar["createMap"].clone()).expect("entries");
        assert_eq!(map.into_iter().collect::<Vec<_>>(), expected);
        assert!(
            create_grammar_tool_input_properties(None, true)
                .expect("empty map")
                .is_empty()
        );
    }

    #[test]
    fn grammar_buffer_matches_typescript_reference() {
        let fixture = fixture();
        let steps = fixture["grammarBuffer"]["steps"].as_array().unwrap();
        let mut buffer = GrammarToolInputJsonBuffer::default();

        let first =
            append_grammar_tool_input_json_delta(&mut buffer, "payload", "a\"").expect("append");
        assert_eq!(first.as_deref(), steps[0]["result"].as_str());
        let empty =
            append_grammar_tool_input_json_delta(&mut buffer, "payload", "").expect("empty delta");
        assert_eq!(empty, None);
        let second =
            close_grammar_tool_input_json(&mut buffer, "payload", Some("a\"\nb")).expect("close");
        assert_eq!(second.as_deref(), steps[2]["result"].as_str());
        assert_eq!(
            close_grammar_tool_input_json(&mut buffer, "payload", Some("a\"\nb"))
                .expect("idempotent"),
            None
        );
        assert_eq!(
            buffer.input,
            fixture["grammarBuffer"]["stateAfter"]["input"]
                .as_str()
                .unwrap()
        );
        assert_eq!(
            close_grammar_tool_input_json(&mut buffer, "payload", Some("changed"))
                .expect_err("changed after close")
                .message,
            fixture["grammarBuffer"]["changed"].as_str().unwrap()
        );
        assert_eq!(
            append_grammar_tool_input_json_delta(&mut buffer, "payload", "x")
                .expect_err("append after close")
                .message,
            fixture["grammarBuffer"]["appended"].as_str().unwrap()
        );

        let mut empty_buffer = GrammarToolInputJsonBuffer::default();
        let closed = close_grammar_tool_input_json(&mut empty_buffer, "payload", None)
            .expect("close without final input");
        assert_eq!(
            closed.as_deref(),
            fixture["grammarBufferEmptyClose"]["result"].as_str()
        );
        assert!(empty_buffer.started);
        assert!(empty_buffer.closed);

        let mut non_monotonic = GrammarToolInputJsonBuffer::default();
        append_grammar_tool_input_json_delta(&mut non_monotonic, "payload", "abc").unwrap();
        assert_eq!(
            close_grammar_tool_input_json(&mut non_monotonic, "payload", Some("abd"))
                .expect_err("non monotonic")
                .message,
            fixture["grammarBufferNonMonotonic"]["error"]
                .as_str()
                .unwrap()
        );

        let mut unicode = GrammarToolInputJsonBuffer::default();
        let a = append_grammar_tool_input_json_delta(&mut unicode, "p", "é😀").unwrap();
        assert_eq!(a.as_deref(), fixture["grammarBufferUnicode"]["a"].as_str());
        let b = close_grammar_tool_input_json(&mut unicode, "p", None).unwrap();
        assert_eq!(b.as_deref(), fixture["grammarBufferUnicode"]["b"].as_str());
    }

    #[test]
    fn grammar_tool_input_matches_typescript_reference() {
        let fixture = fixture();
        let cases = &fixture["getGrammarToolInput"];
        let arguments: serde_json::Map<String, Value> =
            serde_json::from_value(serde_json::json!({ "payload": "abc" })).unwrap();
        assert_eq!(
            get_grammar_tool_input("sample_tool", &arguments, "payload").unwrap(),
            cases["string"]["ok"].as_str().unwrap()
        );
        let arguments: serde_json::Map<String, Value> =
            serde_json::from_value(serde_json::json!({ "payload": 42 })).unwrap();
        assert_eq!(
            get_grammar_tool_input("sample_tool", &arguments, "payload")
                .expect_err("number input")
                .message,
            cases["number"]["error"].as_str().unwrap()
        );
        let arguments: serde_json::Map<String, Value> = serde_json::Map::new();
        assert_eq!(
            get_grammar_tool_input("sample_tool", &arguments, "payload")
                .expect_err("missing input")
                .message,
            cases["missing"]["error"].as_str().unwrap()
        );
    }

    #[test]
    fn json_schema_tool_parameters_matches_typescript_reference() {
        let fixture = fixture();
        let cases = &fixture["getJsonSchemaToolParameters"];
        let tool = Tool {
            name: "t".into(),
            description: "d".into(),
            parameters: fixture["parameters"].clone(),
            constrained_sampling: None,
        };
        assert_eq!(
            stringify(&get_json_schema_tool_parameters(&tool, Some(true)).unwrap()),
            cases["strict"].as_str().unwrap()
        );
        assert_eq!(
            stringify(&get_json_schema_tool_parameters(&tool, None).unwrap()),
            cases["nonStrict"].as_str().unwrap()
        );
        assert_eq!(
            stringify(&get_json_schema_tool_parameters(&tool, Some(false)).unwrap()),
            cases["officialFalse"].as_str().unwrap()
        );
    }
}
