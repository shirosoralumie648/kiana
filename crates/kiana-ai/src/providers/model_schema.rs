//! providers/model-schema.ts：模型目录的 TypeBox schema 与 JS 默认值语义。
//!
//! TS: `packages/ai/src/providers/model-schema.ts`
//!
//! schema 以 TS 侧 `JSON.stringify(schema)` 的逐字快照嵌入（键序、描述、枚举值都不重排），
//! 用 [`serde_json`] 解析成 [`Value`]；`Type.Enum` / `Type.Union` / `Type.Partial(Type.Record)`
//! 的运行时形状与 TypeBox 1.3 完全一致（见 `tests/fixtures/providers/schemas.json`）。
//!
//! [`apply_defaults`] 复刻 `Compile(schema).Default(value)` 的 `Value.Default` 语义（TypeBox
//! `value/default`）：对象按 `properties` 声明顺序补 `default`、record 只对 pattern 命中的键递归、
//! union 选第一个通过校验的分支。
//!
//! # 为什么需要这一层
//!
//! `kiana-config` 用这些 schema 拼出 `models.json` 的校验 schema（cost、inputLimits、
//! promptCache、thinkingLevelMap 等字段）。[`apply_defaults`] 复刻 TS 运行时
//! `Compile(schema).Default(value)` 的默认值填充，当前仓库内由 `tests/compat_schema.rs`
//! 与导出的 fixture 对照；需要与 fixture 一致的补全结果时应使用它，而不是自行遍历
//! 对象、见 `default` 就填。补齐语义的差异会直接改变最终对象形状：缺失的键必须保持缺失
//! （不是写 `null`），record 的递归条件与 union 的分支选择也必须一致。
//!
//! # 实现约束
//!
//! - schema 是 TypeBox 输出经 `JSON.stringify` 得到的逐字快照，键序有意义；修改 schema
//!   必须同步 `tests/fixtures/providers/schemas.json`；
//! - JSON 无法表达 `undefined`：Rust 用 `Option<Value>` 的 `None` 表示缺失，`Value::Null`
//!   表示显式 `null`，两者在所有递归里都必须区分；
//! - [`check`] 只是 JSON Schema 的子集，仅用于 union 分支选择，不能当作通用校验器。

use std::sync::LazyLock;

use serde_json::Value;

/// 解析嵌入的 schema 快照（失败即快照损坏，属于不变量）。
fn parse_schema(text: &str) -> Value {
    serde_json::from_str(text).expect("embedded schema snapshot is valid JSON")
}

/// `THINKING_LEVELS`。
///
/// 不含 `off` 的六个思考档位；顺序与 `ThinkingLevelSchema` 的枚举顺序一致。
pub const THINKING_LEVELS: [&str; 6] = ["minimal", "low", "medium", "high", "xhigh", "max"];
/// `MODEL_THINKING_LEVELS`。
///
/// 模型可接受的七个档位：在 [`THINKING_LEVELS`] 前面加上 `off`。两者不是同一个集合，
/// 不要混用（对应两张不同的 schema）。
pub const MODEL_THINKING_LEVELS: [&str; 7] =
    ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
/// `MODEL_INPUT_MODALITIES`。
///
/// 模型声明的输入模态，目前只有文本与图片。
pub const MODEL_INPUT_MODALITIES: [&str; 2] = ["text", "image"];
/// `CACHE_RETENTIONS`。
///
/// 请求侧可选的缓存保留档位（包含 `none`）。
pub const CACHE_RETENTIONS: [&str; 3] = ["none", "short", "long"];
/// `MODEL_PROMPT_CACHE_RETENTIONS`。
///
/// `ModelPromptCache` 只描述 `short`/`long` 两档的寿命；`none` 不需要缓存寿命。
pub const MODEL_PROMPT_CACHE_RETENTIONS: [&str; 2] = ["short", "long"];

static THINKING_LEVEL_SCHEMA_VALUE: LazyLock<Value> =
    LazyLock::new(|| parse_schema(r#"{"enum":["minimal","low","medium","high","xhigh","max"]}"#));

/// TS: `providers/model-schema.ts#ThinkingLevelSchema`
///
/// 请求侧思考档位枚举，取值即 [`THINKING_LEVELS`]。
pub fn thinking_level_schema() -> &'static Value {
    &THINKING_LEVEL_SCHEMA_VALUE
}

static MODEL_THINKING_LEVEL_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
    parse_schema(r#"{"enum":["off","minimal","low","medium","high","xhigh","max"]}"#)
});

/// TS: `providers/model-schema.ts#ModelThinkingLevelSchema`
///
/// 模型侧思考档位枚举，取值即 [`MODEL_THINKING_LEVELS`]（含 `off`）。
pub fn model_thinking_level_schema() -> &'static Value {
    &MODEL_THINKING_LEVEL_SCHEMA_VALUE
}

static MODEL_INPUT_MODALITY_SCHEMA_VALUE: LazyLock<Value> =
    LazyLock::new(|| parse_schema(r#"{"enum":["text","image"]}"#));

/// TS: `providers/model-schema.ts#ModelInputModalitySchema`
///
/// 模型输入模态枚举，取值即 [`MODEL_INPUT_MODALITIES`]。
pub fn model_input_modality_schema() -> &'static Value {
    &MODEL_INPUT_MODALITY_SCHEMA_VALUE
}

static CACHE_RETENTION_SCHEMA_VALUE: LazyLock<Value> =
    LazyLock::new(|| parse_schema(r#"{"enum":["none","short","long"]}"#));

/// TS: `providers/model-schema.ts#CacheRetentionSchema`
///
/// 请求侧缓存保留档位枚举，取值即 [`CACHE_RETENTIONS`]。
pub fn cache_retention_schema() -> &'static Value {
    &CACHE_RETENTION_SCHEMA_VALUE
}

static THINKING_LEVEL_MAP_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
    parse_schema(
        r#"{"type":"object","properties":{"off":{"anyOf":[{"type":"string"},{"type":"null"}]},"minimal":{"anyOf":[{"type":"string"},{"type":"null"}]},"low":{"anyOf":[{"type":"string"},{"type":"null"}]},"medium":{"anyOf":[{"type":"string"},{"type":"null"}]},"high":{"anyOf":[{"type":"string"},{"type":"null"}]},"xhigh":{"anyOf":[{"type":"string"},{"type":"null"}]},"max":{"anyOf":[{"type":"string"},{"type":"null"}]}},"description":"Provider value sent for each thinking level. null marks a level unsupported. Levels the model does not map use the provider's default value through high; xhigh and max are unsupported."}"#,
    )
});

/// TS: `providers/model-schema.ts#ThinkingLevelMapSchema`
///
/// 档位 → provider 具体取值的映射；`null` 表示该档不受支持，缺失表示使用 provider 的
/// 默认值（`xhigh`/`max` 默认不支持，见 schema 描述）。
pub fn thinking_level_map_schema() -> &'static Value {
    &THINKING_LEVEL_MAP_SCHEMA_VALUE
}

static MODEL_PROMPT_CACHE_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
    parse_schema(
        r#"{"type":"object","properties":{"short":{"type":"number","exclusiveMinimum":0},"long":{"type":"number","exclusiveMinimum":0}},"description":"Best-effort prompt cache lifetime in seconds for the short and long retention tiers. Kiana warms only tiers with a known lifetime."}"#,
    )
});

/// TS: `providers/model-schema.ts#ModelPromptCacheSchema`
///
/// `short`/`long` 两档提示缓存的尽力而为寿命（秒，`exclusiveMinimum: 0`）。
pub fn model_prompt_cache_schema() -> &'static Value {
    &MODEL_PROMPT_CACHE_SCHEMA_VALUE
}

static MODEL_COST_RATES_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
    parse_schema(
        r#"{"type":"object","required":["input","output","cacheRead","cacheWrite"],"properties":{"input":{"type":"number","description":"Input cost in USD per million tokens."},"output":{"type":"number","description":"Output cost in USD per million tokens."},"cacheRead":{"type":"number","description":"Cache-read cost in USD per million tokens."},"cacheWrite":{"type":"number","description":"Cache-write cost in USD per million tokens."}}}"#,
    )
});

/// TS: `providers/model-schema.ts#ModelCostRatesSchema`
///
/// 四项基础单价（输入/输出/缓存读/缓存写），单位 USD / 百万 token，四项都必填。
pub fn model_cost_rates_schema() -> &'static Value {
    &MODEL_COST_RATES_SCHEMA_VALUE
}

static MODEL_COST_TIER_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
    parse_schema(
        r#"{"type":"object","required":["inputTokensAbove","input","output","cacheRead","cacheWrite"],"properties":{"inputTokensAbove":{"type":"number","description":"Use this tier when total request input exceeds this token count."},"input":{"type":"number","description":"Input cost in USD per million tokens."},"output":{"type":"number","description":"Output cost in USD per million tokens."},"cacheRead":{"type":"number","description":"Cache-read cost in USD per million tokens."},"cacheWrite":{"type":"number","description":"Cache-write cost in USD per million tokens."}}}"#,
    )
});

/// TS: `providers/model-schema.ts#ModelCostTierSchema`
///
/// 阶梯价条目：请求输入总量超过 `inputTokensAbove` 后，整条请求按这一档计价。
pub fn model_cost_tier_schema() -> &'static Value {
    &MODEL_COST_TIER_SCHEMA_VALUE
}

static MODEL_COST_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
    parse_schema(
        r#"{"type":"object","required":["input","output","cacheRead","cacheWrite"],"properties":{"input":{"type":"number","description":"Input cost in USD per million tokens."},"output":{"type":"number","description":"Output cost in USD per million tokens."},"cacheRead":{"type":"number","description":"Cache-read cost in USD per million tokens."},"cacheWrite":{"type":"number","description":"Cache-write cost in USD per million tokens."},"tiers":{"type":"array","items":{"type":"object","required":["inputTokensAbove","input","output","cacheRead","cacheWrite"],"properties":{"inputTokensAbove":{"type":"number","description":"Use this tier when total request input exceeds this token count."},"input":{"type":"number","description":"Input cost in USD per million tokens."},"output":{"type":"number","description":"Output cost in USD per million tokens."},"cacheRead":{"type":"number","description":"Cache-read cost in USD per million tokens."},"cacheWrite":{"type":"number","description":"Cache-write cost in USD per million tokens."}}},"description":"Request-wide pricing tiers. The highest matching input threshold applies to the full request."}}}"#,
    )
});

/// TS: `providers/model-schema.ts#ModelCostSchema`
///
/// 模型计费：四项基础单价 + 可选 `tiers`；`tiers` 的匹配规则见每条目的 description。
pub fn model_cost_schema() -> &'static Value {
    &MODEL_COST_SCHEMA_VALUE
}

static MODEL_IMAGE_RESIZE_OPTIONS_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
    parse_schema(
        r#"{"type":"object","properties":{"maxWidth":{"type":"integer","minimum":1},"maxHeight":{"type":"integer","minimum":1},"maxBytes":{"type":"integer","minimum":1,"description":"Maximum base64-encoded payload size in bytes."},"jpegQuality":{"type":"integer","minimum":1,"maximum":100}}}"#,
    )
});

/// TS: `providers/model-schema.ts#ModelImageResizeOptionsSchema`
///
/// 图片进入对话历史前的缩放参数：宽/高/字节上限与 JPEG 质量，字段全部可选且下界为 1。
pub fn model_image_resize_options_schema() -> &'static Value {
    &MODEL_IMAGE_RESIZE_OPTIONS_SCHEMA_VALUE
}

static MODEL_IMAGE_INPUT_LIMITS_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
    parse_schema(
        r#"{"type":"object","properties":{"resize":{"type":"object","properties":{"maxWidth":{"type":"integer","minimum":1},"maxHeight":{"type":"integer","minimum":1},"maxBytes":{"type":"integer","minimum":1,"description":"Maximum base64-encoded payload size in bytes."},"jpegQuality":{"type":"integer","minimum":1,"maximum":100}},"description":"Cache-safe resize profile applied before a new image enters conversation history."},"maxPerMessage":{"type":"integer","minimum":1,"description":"Maximum images accepted in one provider message."},"maxPerRequest":{"type":"integer","minimum":1,"description":"Maximum images accepted across one provider request."}}}"#,
    )
});

/// TS: `providers/model-schema.ts#ModelImageInputLimitsSchema`
///
/// 图片输入限制：缩放 profile 与「单条消息/单次请求最多几张图」。
pub fn model_image_input_limits_schema() -> &'static Value {
    &MODEL_IMAGE_INPUT_LIMITS_SCHEMA_VALUE
}

static MODEL_INPUT_LIMITS_SCHEMA_VALUE: LazyLock<Value> = LazyLock::new(|| {
    parse_schema(
        r#"{"type":"object","properties":{"maxRequestBytes":{"type":"integer","minimum":1,"description":"Maximum serialized provider request size in bytes."},"images":{"type":"object","properties":{"resize":{"type":"object","properties":{"maxWidth":{"type":"integer","minimum":1},"maxHeight":{"type":"integer","minimum":1},"maxBytes":{"type":"integer","minimum":1,"description":"Maximum base64-encoded payload size in bytes."},"jpegQuality":{"type":"integer","minimum":1,"maximum":100}},"description":"Cache-safe resize profile applied before a new image enters conversation history."},"maxPerMessage":{"type":"integer","minimum":1,"description":"Maximum images accepted in one provider message."},"maxPerRequest":{"type":"integer","minimum":1,"description":"Maximum images accepted across one provider request."}}}}}"#,
    )
});

/// TS: `providers/model-schema.ts#ModelInputLimitsSchema`
///
/// 请求级输入限制：序列化后请求体字节上限，以及嵌套的图片限制。
pub fn model_input_limits_schema() -> &'static Value {
    &MODEL_INPUT_LIMITS_SCHEMA_VALUE
}

/// `Compile(schema).Default(value)`（TypeBox 的 `Value.Default`）：补全默认值。
///
/// TS 的 `value` 允许 `undefined`；Rust 用 `None` 表达缺失（`Value::Null` 是 `null`，不是缺失）。
/// 返回值同样是 `Option`：`None` 就是 JS 的 `undefined`——输入缺失且 schema 没有根 `default`
/// 时 `Value.Default` 返回 `undefined`，而不是 `null`。
///
/// # 参数与返回
///
/// - `schema`：TypeBox schema 快照；不是 JSON 对象时直接返回 `None`（无法判断默认值）。
/// - `value`：`None` 表示 JS 的 `undefined`。
/// - 返回 `None` 有两种情况：schema 不是对象，或输入缺失且 schema 没有根 `default`。
///
/// 对象按 `properties` 的声明顺序补默认值；array 逐项递归；`anyOf` 取第一个通过
/// [`check`] 的分支；没有 `default` 的字段保持缺失，不会被写成 `null`。
pub fn apply_defaults(schema: &Value, value: Option<Value>) -> Option<Value> {
    from_type(schema, value)
}

/// `Value.Default` 的「值可能缺失」版本；`None` == JavaScript `undefined`。
fn from_type(schema: &Value, value: Option<Value>) -> Option<Value> {
    let schema = schema.as_object()?;
    // `FromDefault`：只有 value 是 undefined 时才用 schema 的 default。
    let value = match value {
        Some(value) => value,
        None => schema.get("default")?.clone(),
    };
    // TypeBox 用 schema 的 `~kind` 区分 Object 与 Record；JSON 快照里没有 `~kind`。
    // `Type.Record` 的形状是 `{type:"object",patternProperties:{...}}`（没有 `properties`），
    // 而 `Type.Object` 一定有 `properties`，据此分派到 `FromRecord`。
    let is_record = schema.contains_key("patternProperties") && !schema.contains_key("properties");
    match schema.get("type").and_then(Value::as_str) {
        _ if is_record => from_record(schema, value),
        Some("object") => {
            let Some(object) = value.as_object() else {
                return Some(value);
            };
            let mut object = object.clone();
            if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                for (key, property_schema) in properties {
                    // 注意：`object.get(key)` 为 `None` 时先看 property 的 default；
                    // 仍为 `None`（optional 或没有 default）就保持缺失——JS 里给键赋
                    // `undefined` 在 JSON 序列化后同样消失。
                    if let Some(property_value) =
                        from_type(property_schema, object.get(key).cloned())
                    {
                        object.insert(key.clone(), property_value);
                    }
                }
            }
            // `FromObject` 的 additionalProperties：只处理 schema 形状（boolean 已在上面跳过）。
            if let Some(additional) = schema
                .get("additionalProperties")
                .filter(|additional| additional.is_object())
            {
                let known: Vec<String> = schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .map(|properties| properties.keys().cloned().collect())
                    .unwrap_or_default();
                let keys: Vec<String> = object.keys().cloned().collect();
                for key in keys {
                    if known.contains(&key) {
                        continue;
                    }
                    if let Some(property_value) = from_type(additional, object.get(&key).cloned()) {
                        object.insert(key, property_value);
                    }
                }
            }
            Some(Value::Object(object))
        }
        Some("array") => {
            let Some(items) = schema.get("items") else {
                return Some(value);
            };
            let Some(array) = value.as_array() else {
                return Some(value);
            };
            // TypeBox 的 FromArray 对每个元素调用 Value.Default；元素没有可用默认值时
            // 在 JS 里是 `undefined`，JSON 序列化后成为 `null`，因此这里显式写 Null。
            let array = array
                .iter()
                .map(|item| from_type(items, Some(item.clone())).unwrap_or(Value::Null))
                .collect();
            Some(Value::Array(array))
        }
        _ if schema.contains_key("patternProperties") => from_record(schema, value),
        _ if schema.contains_key("anyOf") => {
            // anyOf：按分支顺序尝试，第一个通过 `check` 的结果获胜；全部不通过时保留
            // 原值（与 TypeBox 的 union 默认值行为一致）。分支里的 `from_type` 可能返回
            // `None`，此时用 `null` 作为候选值参与校验。
            let branches = schema
                .get("anyOf")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for branch in &branches {
                let candidate = from_type(branch, Some(value.clone())).unwrap_or(Value::Null);
                if check(branch, &candidate) {
                    return Some(candidate);
                }
            }
            Some(value)
        }
        _ => Some(value),
    }
}

/// TypeBox `FromRecord`：只对 pattern 命中且值 schema 带根 `default` 的键递归。
///
/// `FromRecord` 的 `if (!(recordKey.test(key) && IsDefault(recordValue))) continue;`：值 schema
/// 没有根 `default` 时不递归（否则会把嵌套对象的默认值提前补上）。
fn from_record(schema: &serde_json::Map<String, Value>, value: Value) -> Option<Value> {
    let Some(pattern_properties) = schema.get("patternProperties").and_then(Value::as_object)
    else {
        return Some(value);
    };
    let Some(object) = value.as_object() else {
        return Some(value);
    };
    let mut object = object.clone();
    for (pattern, property_schema) in pattern_properties {
        if !record_pattern_matches(pattern) {
            continue;
        }
        if !property_schema
            .as_object()
            .is_some_and(|schema| schema.contains_key("default"))
        {
            continue;
        }
        let keys: Vec<String> = object.keys().cloned().collect();
        for key in keys {
            if let Some(property_value) = from_type(property_schema, object.get(&key).cloned()) {
                object.insert(key, property_value);
            }
        }
    }
    Some(Value::Object(object))
}

/// `RecordPattern(type)`：TypeBox `Type.Record` 的默认 pattern 是 `^.*$`。
fn record_pattern_matches(pattern: &str) -> bool {
    let pattern = pattern.trim();
    pattern.is_empty() || pattern == "^.*$" || pattern == ".*"
}

/// `Value.Check` 的常用子集（union 分支选择用）：类型标签、`const`/`enum`、`anyOf`、
/// `required`、object 属性/`additionalProperties` 与 array `items` 的递归。
///
/// 未覆盖的 JSON-Schema 关键字（数值边界、`pattern`、`format` 等）在 union 分支选择里
/// 不参与判断；本 crate 用到的 union 分支都没有这些关键字（见 `tests/fixtures/providers/schemas.json`）。
///
/// 因此 `true` 只表示「在本子集下没有发现不匹配」，不代表完整 JSON Schema 校验通过；
/// 除 [`apply_defaults`] 的 union 分支选择与测试外，不要把它当作通用校验器。
pub fn check(schema: &Value, value: &Value) -> bool {
    let Some(schema) = schema.as_object() else {
        return true;
    };
    if let Some(any_of) = schema.get("anyOf").and_then(Value::as_array) {
        return any_of.iter().any(|branch| check(branch, value));
    }
    if let Some(expected) = schema.get("const") {
        return expected == value;
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return values.contains(value);
    }
    let properties = schema.get("properties").and_then(Value::as_object);
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {
            let Some(object) = value.as_object() else {
                return false;
            };
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                for key in required.iter().filter_map(Value::as_str) {
                    if !object.contains_key(key) {
                        return false;
                    }
                }
            }
            if let Some(properties) = properties {
                for (key, property_schema) in properties {
                    if let Some(property_value) = object.get(key)
                        && !check(property_schema, property_value)
                    {
                        return false;
                    }
                }
            }
            match schema.get("additionalProperties") {
                // `false`：对象里不允许出现 properties 之外的键。
                Some(Value::Bool(false)) => {
                    for key in object.keys() {
                        if !properties.is_some_and(|properties| properties.contains_key(key)) {
                            return false;
                        }
                    }
                }
                // 对象形式：properties 之外的每个值都要满足该 schema。
                Some(additional) if additional.is_object() => {
                    for (key, property_value) in object {
                        if properties.is_some_and(|properties| properties.contains_key(key)) {
                            continue;
                        }
                        if !check(additional, property_value) {
                            return false;
                        }
                    }
                }
                _ => {}
            }
            true
        }
        Some("array") => {
            let Some(array) = value.as_array() else {
                return false;
            };
            if let Some(items) = schema.get("items").filter(|items| items.is_object()) {
                return array.iter().all(|item| check(items, item));
            }
            true
        }
        Some("string") => value.is_string(),
        Some("number") => value.is_number(),
        Some("integer") => value.as_f64().is_some_and(|number| number.fract() == 0.0),
        Some("boolean") => value.is_boolean(),
        Some("null") => value.is_null(),
        _ => true,
    }
}
