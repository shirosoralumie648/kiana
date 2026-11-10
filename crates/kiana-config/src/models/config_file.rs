//! `ModelConfig` / `ModelsConfigSchema` / `strip_json_comments` / `format_validation_path`。
//!
//! TS 源：`packages/coding-agent/src/core/model-config.ts`（219 行）。
//! 规范：`spec/ca-config-models.md` §2.18、§4.3（models.json JSONC）、§8.3 第 3 条。
//!
//! ## 与 TS 的映射
//!
//! - TypeBox 的 `Compile(ModelsConfigSchema)` 在 Rust 侧换成一个**小型 schema 校验器**：
//!   schema 仍是 `serde_json::Value` 形状（子 schema 直接复用 `kiana-ai` 里逐字嵌入的
//!   `providers/model-schema.ts` / `providers/compat-schema.ts` 快照），校验器只实现本项目
//!   用到的关键字（`type`/`properties`/`required`/`patternProperties`/`items`/`maxItems`/
//!   `minItems`/`enum`/`const`/`anyOf`/`minLength`/`maxLength`/`minimum`/`maximum`/
//!   `exclusiveMinimum`/`exclusiveMaximum`）。错误文案、关键字求值顺序与
//!   `Settings.maxErrors = 8` 的上限逐字对齐 TypeBox 1.3（`must be object`、
//!   `must have required properties …`、`must be > 0` …，先 `required` 再
//!   `patternProperties`、`properties`，`type` 失败后仍求值 `const`/`enum`/`anyOf`）。
//! - `structuredClone(provider)` + `deepFreeze` → 解析成不可变的 [`ModelsJsonProvider`] 并存入
//!   `Arc`；未知字段被忽略（TS 的克隆会保留它们，但没有任何读取点）。
//! - `readFile(path, "utf-8")` 的 IO 错误文案按 Node 的 `<CODE>: <description>, <syscall> '<path>'`
//!   形状生成；`ENOENT` 返回空配置（无错误）。
//! - `JSON.parse` 用 [`kiana_json::js_parse::parse`]，错误消息逐字等于 V8 的 `SyntaxError.message`。
//! - `stripJsonComments` / `stripBom` 复用 foundation 的唯一实现
//!   （[`kiana_core::json::strip_json_comments`] / [`kiana_core::text::strip_bom`]）。
//!
//! ## 在 crate 中的位置
//!
//! 本文件是 `models.json` 的唯一读入口：[`ModelConfig::load`] 完成「读取 → JSONC 解析 →
//! schema 校验 → 反序列化」。它**不**做环境变量展开、apiKey 解析，也不补模型默认值：
//! 这些分别属于 `resolve_config_value` 与 `composer`（`model_from_json` / `apply_models_json`）。
//! `ModelRuntime` 在 `create` 与 `refresh` 时调用 `load`，把结果整体替换进内部的
//! `Mutex<ModelConfig>`；provider 条目以 `Arc` 共享，加载完成后只读。
//!
//! ## 一次加载的执行流程
//!
//! 1. `None` 或空串视为未提供路径（TS 的 `!modelsJsonPath`），直接返回空配置；
//! 2. `kiana_core::paths::normalize_path` 归一化路径（默认选项会展开 `~`、把 `file://`
//!    形式的 URL 转成路径）；之后错误文案里的 `File:` 用的是归一化后的路径；
//! 3. `tokio::fs::read`：`ErrorKind::NotFound` → 空配置（无错误），其余 IO 错误 →
//!    `Failed to load models.json: <Node 文案>\n\nFile: <path>`；
//! 4. 字节按 UTF-8 有损解码（非法序列变 U+FFFD），再去 BOM、去 `//` 行注释与尾逗号；
//! 5. `kiana_json::js_parse::parse`（`JSON.parse` 语义）→ 失败时
//!    `Failed to parse models.json: <V8 message>...`；
//! 6. [`validate_models_config`] 用本文件内嵌的 schema 校验，最多记录 8 条错误；
//! 7. 校验通过后 `serde_json::from_value::<ModelsConfig>`，provider 按 JSON 中出现的顺序
//!    存入 `IndexMap`，每个 provider 包一层 `Arc`。
//!
//! ## 失败语义
//!
//! `models.json` 是可选配置：未提供路径或文件不存在都返回空配置且不报错。除此之外的任何
//! 失败（IO、JSON 语法、schema、反序列化）都会得到 `providers` 为空、`error` 为 `Some`
//! 的配置：没有「坏条目跳过、其余保留」的降级路径（TS 侧同样整份拒绝）。调用方拿到的
//! 模型集合要么完整、要么为空，错误由 `ModelRuntime::get_error` 汇总展示。
//!
//! ## 修改时的注意事项
//!
//! - schema 构造器里 `properties` 的**声明顺序就是错误报告顺序**，
//!   `tests/fixtures/models/model_config_cases.json` 逐字锁定这些文案与顺序，不要重排；
//! - `ModelsJson*` 结构体的字段类型与 `#[serde]` 属性决定「schema 通过之后」还能接受什么；
//!   例如 `inputLimits` 的整数走自定义反序列化（饱和），与普通 `u64` 字段的行为不同；
//! - 新增关键字或错误文案时必须同时对照 TypeBox 的求值顺序（见 `validate_into` 的文档与
//!   `crates/kiana-config/tests/model_config.rs`）。

use std::sync::{Arc, LazyLock};

use indexmap::IndexMap;
use kiana_ai::providers::compat_schema::provider_compat_schema;
use kiana_ai::providers::model_schema::{
    model_cost_schema, model_input_limits_schema, model_input_modality_schema,
    model_prompt_cache_schema, thinking_level_map_schema,
};
use kiana_ai::types::JsonValue;
use kiana_ai::types::{ModelCost, ModelCostTier, ModelInputLimits, ModelInputModality};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// 复用 foundation 的 `stripJsonComments` 实现（去 `//` 行注释与尾逗号，不支持块注释）。
///
/// `ModelConfig::load` 与 `tests/model_config.rs` 都通过这个路径使用同一实现，不要在
/// 本 crate 内另写一份。
pub use kiana_core::json::strip_json_comments;

/// `Record<string, unknown>` / `Record<string, string>` 的 Rust 视图（保序）。
///
/// 这里只保存 JSON 原文；`$NAME` / `!command` 形式的取值由 composer 在解析认证或请求
/// 配置时通过 `resolve_headers_or_throw` 用与 apiKey 相同的规则解析。
pub type JsonHeaders = IndexMap<String, String>;
/// `ThinkingLevelMap` 的 JSON 视图（schema 允许未知键，构造 `Model` 时丢弃）。
///
/// 反序列化只保留字符串值与显式 `null`，其他类型的值被丢弃；未知 level 键会留在 map 里，
/// 直到 composer 的 `to_thinking_level_map` 转换时再丢。
pub type JsonThinkingLevelMap = IndexMap<String, Option<String>>;
/// `ModelPromptCache` 的 JSON 视图（schema 允许未知键，构造 `Model` 时丢弃）。
///
/// 只保留数字值；`short`/`long` 之外的键同样留到 composer 转换时丢弃。
pub type JsonPromptCache = IndexMap<String, f64>;
/// `SamplingParams`。
///
/// 键值原样保留、原样合并进请求体，本文件不解释其内容。
pub type JsonSamplingParams = IndexMap<String, JsonValue>;
/// `samplingParamsByThinkingLevel`。
///
/// 外层键是 thinking level，内层是原样保留的采样参数；未知 level 键在 composer 转换时丢弃。
pub type JsonSamplingParamsByThinkingLevel = IndexMap<String, JsonSamplingParams>;

// ---------------------------------------------------------------------------
// 宽松反序列化
// ---------------------------------------------------------------------------
//
// TS 只把 schema 校验当作唯一的接受门槛，校验通过后直接使用原始对象；`thinkingLevelMap`
// 等 `Record` 的**未知键**不被 TypeBox 约束，值可以是任意 JSON。Rust 的强类型 map 会拒绝
// 这些值，因此这些字段用自定义反序列化：已知键的类型由 schema 保证，未知键的条目在类型
// 不匹配时丢弃（TS 侧同样从不读取未知键）。`inputLimits` 的整数同理接受任意 JS 整数
// （`Integer` 只要求 `Number.isInteger`），超出 `u64` 的值饱和。
//
// 这些反序列化都先读成 `Value` 再手工挑字段：若直接交给强类型 map/struct，一个未知键的
// 值就可能让整份配置反序列化失败，而 TS 在 schema 通过后根本不会读取这些键。

/// `Number.isInteger(value)` 且非负的 JSON 数字 → `u64`（饱和转换）。
///
/// 非数字、`NaN`/`Infinity`、负数和带小数部分的值返回 `None`；超过 `u64::MAX` 的值按
/// Rust 的浮点→整数 `as` 转换饱和到 `u64::MAX`（如 `1e300`）。TypeBox 的 `Integer`
/// 只要求 `Number.isInteger`、没有上界，所以这里不能要求值落在 `u64` 范围内。
pub(crate) fn js_integer_to_u64(value: &Value) -> Option<u64> {
    let number = value.as_f64()?;
    if !number.is_finite() || number.fract() != 0.0 || number < 0.0 {
        return None;
    }
    Some(number as u64)
}

/// `ModelInputLimits` 的宽松反序列化（整数饱和；未知字段忽略）。
///
/// 非对象输入返回全为 `None` 的默认值；嵌套的 `images`/`resize` 若存在但不是对象，
/// 对应层级整体为 `None`，不会部分保留。`jpegQuality` 转 `u8`：schema 上限是 100，
/// 正常校验路径不会越界；但 `catalog_http` 的宽松目录解析也会直接调用本函数且不经过
/// 本文件的 schema 校验，那条路径上超出 `u8` 的整数会按 `as u8` 截断。
pub(crate) fn input_limits_from_value(value: &Value) -> ModelInputLimits {
    let Some(object) = value.as_object() else {
        return ModelInputLimits::default();
    };
    ModelInputLimits {
        max_request_bytes: object.get("maxRequestBytes").and_then(js_integer_to_u64),
        images: object
            .get("images")
            .and_then(Value::as_object)
            .map(|images| kiana_ai::types::ModelImageInputLimits {
                resize: images
                    .get("resize")
                    .and_then(Value::as_object)
                    .map(|resize| kiana_ai::types::ModelImageResizeOptions {
                        max_width: resize.get("maxWidth").and_then(js_integer_to_u64),
                        max_height: resize.get("maxHeight").and_then(js_integer_to_u64),
                        max_bytes: resize.get("maxBytes").and_then(js_integer_to_u64),
                        jpeg_quality: resize
                            .get("jpegQuality")
                            .and_then(js_integer_to_u64)
                            .map(|value| value as u8),
                    }),
                max_per_message: images.get("maxPerMessage").and_then(js_integer_to_u64),
                max_per_request: images.get("maxPerRequest").and_then(js_integer_to_u64),
            }),
    }
}

/// `inputLimits` 的 serde 适配：字段出现就返回 `Some`（内部各字段再各自宽松处理）。
///
/// 在 `load` 的路径上 schema 已保证取值为对象，因此实际不会走「非对象 → 默认值」分支；
/// 保留该分支是为了让反序列化本身不产生额外错误（与 TS 的「schema 是唯一门槛」一致）。
fn deserialize_input_limits<'de, D>(deserializer: D) -> Result<Option<ModelInputLimits>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(Some(input_limits_from_value(&value)))
}

/// `thinkingLevelMap` 的 serde 适配：只保留 `string` 与 `null` 值，非对象 → `None`。
///
/// 未知键（非七个 level 的键）不会被这里过滤，它们在 schema 里不受约束、也不会被 composer
/// 读取；真正的丢弃发生在 `to_thinking_level_map`。
fn deserialize_thinking_level_map<'de, D>(
    deserializer: D,
) -> Result<Option<JsonThinkingLevelMap>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    let mut map = JsonThinkingLevelMap::new();
    for (key, value) in object {
        match value {
            Value::String(text) => {
                map.insert(key.clone(), Some(text.clone()));
            }
            Value::Null => {
                map.insert(key.clone(), None);
            }
            _ => {}
        }
    }
    Ok(Some(map))
}

/// `promptCache` 的 serde 适配：只保留数字值，非对象 → `None`。
///
/// 未知键保留在 map 中（schema 不约束），composer 转换时丢弃；值不是数字的条目在这里
/// 直接消失，与 TS「未知键从不读取」的行为一致。
fn deserialize_prompt_cache<'de, D>(deserializer: D) -> Result<Option<JsonPromptCache>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    let mut map = JsonPromptCache::new();
    for (key, value) in object {
        if let Some(number) = value.as_f64() {
            map.insert(key.clone(), number);
        }
    }
    Ok(Some(map))
}

/// `samplingParamsByThinkingLevel` 的 serde 适配：值不是对象的条目丢弃，非对象 → `None`。
///
/// 外层键不做枚举校验（schema 也没有把七个 level 设为 required），未知 level 会保留到
/// composer 的 `to_sampling_params_by_thinking_level` 再丢弃。
fn deserialize_sampling_params_by_thinking_level<'de, D>(
    deserializer: D,
) -> Result<Option<JsonSamplingParamsByThinkingLevel>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    let mut map = JsonSamplingParamsByThinkingLevel::new();
    for (key, value) in object {
        if let Some(params) = value.as_object() {
            map.insert(
                key.clone(),
                params
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect(),
            );
        }
    }
    Ok(Some(map))
}

/// `ModelOverrideSchema.cost`（`Type.Partial(ModelCostSchema)`）。
///
/// 所有字段都是 `Option`，`None` 表示「不覆盖、沿用基础模型的价格」而不是清零；
/// composer 的 `apply_model_override` 逐字段合并，`tiers` 是整组替换（不会按元素合并）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialModelCost {
    /// `input`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<f64>,
    /// `output`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<f64>,
    /// `cacheRead`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    /// `cacheWrite`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
    /// `tiers`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<ModelCostTier>>,
}

/// models.json 的 `models[]` 条目（`ModelDefinitionSchema`）。
///
/// 默认值（`reasoning: false`、`input: ["text"]`、`contextWindow: 128000`、`maxTokens: 16384`）
/// 不在这里应用，由 composer 的 `modelFromJson` 逐字复刻。
///
/// `id` 是 schema 唯一要求的字段（`required: ["id"]`，且 `minLength: 1`）；未知 JSON 键
/// 被 serde 忽略。composer 的 `apply_models_json` 以 `id` 做 upsert，但只对同 id 的
/// Chat/Remote 条目原位替换（Remote 不检查 model_type）；Image/Classifier 即使 id 相同
/// 也会追加，最终列表可能出现同 id 的两条模型。字段为 `None` 表示配置没写该字段，
/// 与显式 `false`/`0` 不是一回事。`context_window`/`max_tokens` 用 `f64` 保存
/// （TS 的 number 允许小数），composer 转成 `u64` 时会截断小数部分。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonModel {
    /// `Model ID sent to the API.`
    pub id: String,
    /// `Display name, also matched by --model patterns.`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `Overrides the provider api.`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    /// `Overrides the provider baseUrl.`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// `Whether the model supports thinking.`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    /// 每个 thinking level 的 provider 取值（`null` 表示不支持）。
    #[serde(
        default,
        deserialize_with = "deserialize_thinking_level_map",
        skip_serializing_if = "Option::is_none"
    )]
    pub thinking_level_map: Option<JsonThinkingLevelMap>,
    /// 输入模态。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<ModelInputModality>>,
    /// 输入上限。
    #[serde(
        default,
        deserialize_with = "deserialize_input_limits",
        skip_serializing_if = "Option::is_none"
    )]
    pub input_limits: Option<ModelInputLimits>,
    /// 价格。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<ModelCost>,
    /// prompt cache 生命周期。
    #[serde(
        default,
        deserialize_with = "deserialize_prompt_cache",
        skip_serializing_if = "Option::is_none"
    )]
    pub prompt_cache: Option<JsonPromptCache>,
    /// 上下文窗口（TS 是 number，允许小数）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<f64>,
    /// 最大输出 token。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<f64>,
    /// 合并进每个请求体的字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<JsonSamplingParams>,
    /// 按 thinking level 的采样参数。
    #[serde(
        default,
        deserialize_with = "deserialize_sampling_params_by_thinking_level",
        skip_serializing_if = "Option::is_none"
    )]
    pub sampling_params_by_thinking_level: Option<JsonSamplingParamsByThinkingLevel>,
    /// 请求头（值按 apiKey 规则解析）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<JsonHeaders>,
    /// 兼容性开关。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<JsonValue>,
}

/// models.json 的 `modelOverrides` 条目（`ModelOverrideSchema`；无默认值）。
///
/// 每个 `Option` 的 `None` 表示「不覆盖」（不是清空）；composer 在模型合成的最后阶段
/// 按模型 `id` 应用。对命中的 Remote 条目会保留原始 JSON，重建 `apply_model_override`
/// 产出的固定字段集（name、reasoning、thinkingLevelMap、inputLimits、promptCache、
/// contextWindow、maxTokens、samplingParams、samplingParamsByThinkingLevel、compat），
/// `input`/`cost` 仅在 override 显式给出时写入——即使 override 未提及某字段，
/// 重建也会覆盖原始 JSON 里的对应键；若原始 JSON 缺某键而 typed 模型有值，还会补写进去。
/// 匹配不到任何模型的键会被静默忽略。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonModelOverride {
    /// 展示名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 是否支持 thinking。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    /// thinking level 映射。
    #[serde(
        default,
        deserialize_with = "deserialize_thinking_level_map",
        skip_serializing_if = "Option::is_none"
    )]
    pub thinking_level_map: Option<JsonThinkingLevelMap>,
    /// 输入模态。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<ModelInputModality>>,
    /// 输入上限。
    #[serde(
        default,
        deserialize_with = "deserialize_input_limits",
        skip_serializing_if = "Option::is_none"
    )]
    pub input_limits: Option<ModelInputLimits>,
    /// 价格（部分字段）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<PartialModelCost>,
    /// prompt cache 生命周期。
    #[serde(
        default,
        deserialize_with = "deserialize_prompt_cache",
        skip_serializing_if = "Option::is_none"
    )]
    pub prompt_cache: Option<JsonPromptCache>,
    /// 上下文窗口。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<f64>,
    /// 最大输出 token。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<f64>,
    /// 采样参数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<JsonSamplingParams>,
    /// 按 thinking level 的采样参数。
    #[serde(
        default,
        deserialize_with = "deserialize_sampling_params_by_thinking_level",
        skip_serializing_if = "Option::is_none"
    )]
    pub sampling_params_by_thinking_level: Option<JsonSamplingParamsByThinkingLevel>,
    /// 请求头。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<JsonHeaders>,
    /// 兼容性开关。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<JsonValue>,
}

/// `oauth: Type.Literal("radius")`。
///
/// schema 用 `const` 把取值固定为 `"radius"`，所以本枚举只有一个变体。`oauth` 要求同时
/// 提供 `baseUrl` 的检查不在加载阶段（schema 表达不了这种依赖），而在 composer 的
/// `apply_models_json`；单独写 `oauth` 的配置能通过加载，直到合成 provider 时才报错。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelsJsonOauth {
    /// 内置 radius OAuth 流程。
    Radius,
}

/// models.json 的 provider 条目（`ProviderConfigSchema`）。
///
/// 所有字段都是可选的。`api_key` 只保存原文，`$NAME` / `!command` 由 `resolve_config_value`
/// 在 composer 解析认证（`ApiKeyAuth::resolve`）时展开；`models` 按 `id` upsert，但只对
/// 同 id 的 Chat/Remote 条目原位替换（Remote 不检查 model_type），Image/Classifier 同 id
/// 会追加；`model_overrides` 以模型 id 为键。加载阶段不检查
/// 「provider 至少配置了一项」，空对象 `{}` 也能通过 schema；composer 的 `apply_models_json`
/// 才会在以下条件同时成立时报错：没有 baseUrl/headers/compat/apiKey/oauth/authHeader
/// （这些字段只要出现即可，哪怕是空对象），且 models 为空或缺失、modelOverrides 为空或缺失。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonProvider {
    /// `Display name.`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `API base URL. Also applies to the provider's built-in models.`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// `Literal key, "$NAME" environment interpolation, or "!command" output.`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// `API for the provider's models …`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    /// `OAuth login flow. Requires baseUrl.`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<ModelsJsonOauth>,
    /// 请求头。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<JsonHeaders>,
    /// 兼容性开关。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<JsonValue>,
    /// `Whether to send the resolved API key as Authorization: Bearer.`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<bool>,
    /// 自定义模型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<ModelsJsonModel>>,
    /// 模型覆盖。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_overrides: Option<IndexMap<String, ModelsJsonModelOverride>>,
}

/// models.json 的根对象（`ModelsConfigSchema`）。
///
/// `providers` 是 schema 的必填项；`$schema` 可选且只校验「是字符串」。未知顶层键被忽略。
/// provider 之间的顺序就是 JSON 文本中的出现顺序（`IndexMap`），并被 `get_provider_ids`
/// 与运行时的 provider 合并顺序沿用。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsConfig {
    /// `$schema`。
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// provider 映射（插入顺序）。
    pub providers: IndexMap<String, ModelsJsonProvider>,
}

/// `ModelsJsonSchemas = { provider, model, modelOverride }`（`generate-references.ts` 的数据源）。
///
/// 三个字段分别是独立 schema 的 JSON 树，构建时从各自的 `LazyLock` 克隆；整个结构体由
/// [`models_json_schemas`] 以 `&'static` 暴露。校验内部并不直接用它，而是用合并后的
/// [`models_config_schema`]。
#[derive(Debug)]
pub struct ModelsJsonSchemas {
    /// `ProviderConfigSchema`。
    pub provider: Value,
    /// `ModelDefinitionSchema`。
    pub model: Value,
    /// `ModelOverrideSchema`。
    pub model_override: Value,
}

static MODELS_CONFIG_SCHEMA: LazyLock<Value> = LazyLock::new(build_models_config_schema);
static MODELS_JSON_SCHEMAS: LazyLock<ModelsJsonSchemas> = LazyLock::new(|| ModelsJsonSchemas {
    provider: build_provider_config_schema(),
    model: build_model_definition_schema(),
    model_override: build_model_override_schema(),
});

/// `ModelsConfigSchema`（`$schema` + `providers`）。
///
/// 首次调用时构建并缓存整棵树（`LazyLock`），之后返回同一个 `&'static Value`；调用方只能
/// 读取。schema 完全由本文件的构造器与 `kiana-ai` 的逐字快照拼出，不读任何文件。
pub fn models_config_schema() -> &'static Value {
    &MODELS_CONFIG_SCHEMA
}

/// `ModelsJsonSchemas`。
///
/// 与 [`models_config_schema`] 一样是进程内单次构建的只读快照；仓库内当前由
/// `tests/model_config.rs` 直接断言其形状，TS 侧的消费者是 `generate-references.ts`。
pub fn models_json_schemas() -> &'static ModelsJsonSchemas {
    &MODELS_JSON_SCHEMAS
}

// ---------------------------------------------------------------------------
// schema 构造
// ---------------------------------------------------------------------------

/// `Type.Record(Type.String(), value)` 的 JSON 形状。
///
/// 用 `patternProperties` 的单条模式 `^.*$` 表示「每个键都套同一个 value schema」；
/// 它匹配所有键（包括空键），所以 record 里未知键的值同样会被校验，这与 TS 的
/// `Type.Record` 一致。
fn record_schema(value: Value) -> Value {
    let mut pattern_properties = Map::new();
    pattern_properties.insert("^.*$".to_string(), value);
    let mut schema = Map::new();
    schema.insert("type".to_string(), Value::String("object".to_string()));
    schema.insert(
        "patternProperties".to_string(),
        Value::Object(pattern_properties),
    );
    Value::Object(schema)
}

/// `Type.Object(properties, required)` 的 JSON 形状（`properties` 顺序即错误顺序）。
///
/// `required` 为空时整个键都省略（而不是写空数组），因此校验器不会对空 required 产生
/// 「must have required properties」错误。`properties` 用保序 map 构造，值遍历顺序就是
/// 这里的声明顺序；fixture 中的错误顺序依赖这一点。
fn object_schema(properties: Vec<(&str, Value)>, required: Vec<&str>) -> Value {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String("object".to_string()));
    let mut props = Map::new();
    for (name, schema) in properties {
        props.insert(name.to_string(), schema);
    }
    map.insert("properties".to_string(), Value::Object(props));
    if !required.is_empty() {
        map.insert(
            "required".to_string(),
            Value::Array(
                required
                    .into_iter()
                    .map(|name| Value::String(name.to_string()))
                    .collect(),
            ),
        );
    }
    Value::Object(map)
}

fn string_schema(min_length: Option<u64>) -> Value {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String("string".to_string()));
    if let Some(min_length) = min_length {
        map.insert("minLength".to_string(), Value::from(min_length));
    }
    Value::Object(map)
}

fn number_schema(exclusive_minimum: Option<f64>) -> Value {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String("number".to_string()));
    if let Some(exclusive_minimum) = exclusive_minimum {
        map.insert(
            "exclusiveMinimum".to_string(),
            serde_json::Number::from_f64(exclusive_minimum)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        );
    }
    Value::Object(map)
}

fn boolean_schema() -> Value {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String("boolean".to_string()));
    Value::Object(map)
}

fn array_schema(items: Value) -> Value {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String("array".to_string()));
    map.insert("items".to_string(), items);
    Value::Object(map)
}

/// `SamplingParamsByThinkingLevelSchema`：七个 level 都是可选属性，值是不受约束的对象。
///
/// 未列出的 level 键不匹配任何 `properties`，schema 不会报错（fixture 的 success 用例
/// `sampling unknown level` 锁定）；它们会在反序列化时保留、composer 转换时丢弃。
fn sampling_params_by_thinking_level_schema() -> Value {
    let mut properties = Vec::new();
    for level in ["off", "minimal", "low", "medium", "high", "xhigh", "max"] {
        properties.push((level, record_schema(Value::Object(Map::new()))));
    }
    object_schema(properties, Vec::new())
}

/// `ModelDefinitionSchema`：models[] 条目的 schema。
///
/// 只有 `id` 是 required；`contextWindow`/`maxTokens` 用 `exclusiveMinimum: 0`，因此
/// `0` 和负数都过不了校验（`tests/model_config.rs` 覆盖）。末尾的 `compat` 直接嵌入
/// `kiana-ai` 的 provider 级兼容 schema 快照。
fn model_definition_schema() -> Value {
    object_schema(
        vec![
            ("id", string_schema(Some(1))),
            ("name", string_schema(Some(1))),
            ("api", string_schema(Some(1))),
            ("baseUrl", string_schema(Some(1))),
            ("reasoning", boolean_schema()),
            ("thinkingLevelMap", thinking_level_map_schema().clone()),
            ("input", array_schema(model_input_modality_schema().clone())),
            ("inputLimits", model_input_limits_schema().clone()),
            ("cost", model_cost_schema().clone()),
            ("promptCache", model_prompt_cache_schema().clone()),
            ("contextWindow", number_schema(Some(0.0))),
            ("maxTokens", number_schema(Some(0.0))),
            ("samplingParams", record_schema(Value::Object(Map::new()))),
            (
                "samplingParamsByThinkingLevel",
                sampling_params_by_thinking_level_schema(),
            ),
            ("headers", record_schema(string_schema(None))),
            ("compat", provider_compat_schema().clone()),
        ],
        vec!["id"],
    )
}

/// `build_model_definition_schema` 的惰性包装（`LazyLock` 需要函数项）。
///
/// 独立缓存的原因：`ModelsJsonSchemas` 和 `ModelsConfigSchema` 两棵树都会用到同一份子
/// schema；每次返回 `clone()`，方便调用方把它嵌进自己的父节点。`build_model_override_schema`
/// 与 `build_provider_config_schema` 同理。
fn build_model_definition_schema() -> Value {
    static VALUE: LazyLock<Value> = LazyLock::new(model_definition_schema);
    VALUE.clone()
}

/// `ModelOverrideSchema`：modelOverrides 条目的 schema，没有任何 required。
///
/// `cost` 用 `Type.Partial(ModelCostSchema)`：只删掉**顶层** `required`，嵌套的 `tiers`
/// 条目仍然要求四个费率字段齐全（fixture 的 `tiers missing` 用例锁定）。
fn model_override_schema() -> Value {
    // `Type.Partial(ModelCostSchema)`：去掉顶层 required，其余形状不变。
    let mut cost = model_cost_schema().clone();
    if let Value::Object(map) = &mut cost {
        map.shift_remove("required");
    }
    object_schema(
        vec![
            ("name", string_schema(Some(1))),
            ("reasoning", boolean_schema()),
            ("thinkingLevelMap", thinking_level_map_schema().clone()),
            ("input", array_schema(model_input_modality_schema().clone())),
            ("inputLimits", model_input_limits_schema().clone()),
            ("cost", cost),
            ("promptCache", model_prompt_cache_schema().clone()),
            ("contextWindow", number_schema(Some(0.0))),
            ("maxTokens", number_schema(Some(0.0))),
            ("samplingParams", record_schema(Value::Object(Map::new()))),
            (
                "samplingParamsByThinkingLevel",
                sampling_params_by_thinking_level_schema(),
            ),
            ("headers", record_schema(string_schema(None))),
            ("compat", provider_compat_schema().clone()),
        ],
        Vec::new(),
    )
}

fn build_model_override_schema() -> Value {
    static VALUE: LazyLock<Value> = LazyLock::new(model_override_schema);
    VALUE.clone()
}

/// `ProviderConfigSchema`：provider 条目的 schema。
///
/// 没有 required；`oauth` 用 `const: "radius"` 表达枚举，`models`/`modelOverrides` 分别
/// 引用 model / modelOverride schema 的克隆。这里的属性顺序就是 provider 校验错误的
/// 输出顺序（`tests/fixtures/models/model_config_cases.json` 的 `max errors cap` 用例）。
fn provider_config_schema() -> Value {
    object_schema(
        vec![
            ("name", string_schema(Some(1))),
            ("baseUrl", string_schema(Some(1))),
            ("apiKey", string_schema(Some(1))),
            ("api", string_schema(Some(1))),
            ("oauth", {
                let mut map = Map::new();
                map.insert("type".to_string(), Value::String("string".to_string()));
                map.insert("const".to_string(), Value::String("radius".to_string()));
                Value::Object(map)
            }),
            ("headers", record_schema(string_schema(None))),
            ("compat", provider_compat_schema().clone()),
            ("authHeader", boolean_schema()),
            ("models", array_schema(build_model_definition_schema())),
            (
                "modelOverrides",
                record_schema(build_model_override_schema()),
            ),
        ],
        Vec::new(),
    )
}

fn build_provider_config_schema() -> Value {
    static VALUE: LazyLock<Value> = LazyLock::new(provider_config_schema);
    VALUE.clone()
}

/// `ModelsConfigSchema` 的完整构造：`$schema?`（任意字符串）+ `providers`（必填 Record）。
///
/// `required: ["providers"]` 是「空对象 / 缺 providers」两类配置报错的来源；错误路径为
/// `providers`，文案是 TypeBox 的 `must have required properties providers`。
fn build_models_config_schema() -> Value {
    object_schema(
        vec![
            ("$schema", string_schema(None)),
            ("providers", record_schema(build_provider_config_schema())),
        ],
        vec!["providers"],
    )
}

// ---------------------------------------------------------------------------
// TypeBox 兼容校验器
// ---------------------------------------------------------------------------

/// 一条校验错误（TypeBox `TLocalizedValidationError` 的可观察子集）。
///
/// `message` 只是 TypeBox 的报文本身，不含路径；用户可见的 `路径: 报文` 由
/// [`format_validation_path`] 拼出。`instance_path` 是遍历栈直接拼接的结果（见 `pointer`），
/// **不是** RFC 6901 转义后的 JSON Pointer。一次校验最多产生 8 条错误（`MAX_ERRORS`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelsJsonValidationError {
    /// 触发关键字（`"required"`、`"type"`、`"anyOf"` …）。
    pub keyword: String,
    /// JSON Pointer 形式的实例路径（根为 `""`）。
    pub instance_path: String,
    /// TypeBox 的 `error.message`（逐字）。
    pub message: String,
    /// `keyword === "required"` 时缺失的属性（`params.requiredProperties`）。
    pub required_properties: Vec<String>,
}

/// `formatValidationPath(error)`：错误路径的逐字格式化。
///
/// `required` 错误把 `requiredProperties[0]` 拼到父路径后；其余把 JSON Pointer 的 `/` 换成 `.`，
/// 空路径返回 `root`。
///
/// 只使用第一个缺失属性（TS 的逐字规则）。若 `keyword == "required"` 但
/// `required_properties` 为空，则退回普通路径格式化。路径段不做转义：模型 id 或 provider
/// id 里含 `.`、`/` 时，输出路径会有歧义。这是当前稳定契约，错误的黄金测试依赖它，
/// 改动需同步测试。
pub fn format_validation_path(error: &ModelsJsonValidationError) -> String {
    if error.keyword == "required"
        && let Some(first) = error.required_properties.first()
    {
        let base = format_instance_path(&error.instance_path);
        return if base.is_empty() {
            first.clone()
        } else {
            format!("{base}.{first}")
        };
    }
    let path = format_instance_path(&error.instance_path);
    if path.is_empty() {
        "root".to_string()
    } else {
        path
    }
}

/// `instancePath.replace(/^\//, "").replace(/\//g, ".")`。
fn format_instance_path(instance_path: &str) -> String {
    instance_path
        .strip_prefix('/')
        .unwrap_or(instance_path)
        .replace('/', ".")
}

/// TypeBox 的 `Settings.maxErrors`；`ErrorContext` 达到上限后不再记录错误。
const MAX_ERRORS: usize = 8;

fn pointer(path: &[String]) -> String {
    // TypeBox 用 `${instancePath}/${key}` 原样拼接，**不**做 JSON Pointer 转义
    // （`~`/`/` 保留为裸字符，`format_validation_path` 再把 `/` 换成 `.`）。
    let mut result = String::new();
    for segment in path {
        result.push('/');
        result.push_str(segment);
    }
    result
}

/// `ErrorContext.AddError`：达到 [`MAX_ERRORS`] 后静默丢弃。
fn push_error(
    errors: &mut Vec<ModelsJsonValidationError>,
    path: &[String],
    keyword: &str,
    message: impl Into<String>,
) {
    if errors.len() >= MAX_ERRORS {
        return;
    }
    errors.push(ModelsJsonValidationError {
        keyword: keyword.to_string(),
        instance_path: pointer(path),
        message: message.into(),
        required_properties: Vec::new(),
    });
}

/// `String(value)`（JS 数字文本）。
fn js_number_text(value: f64) -> String {
    kiana_json::format_js_number(value)
}

fn value_as_f64(value: &Value) -> Option<f64> {
    value.as_f64()
}

/// 校验 `value` 是否符合 `schema`（TypeBox `Compile(schema).Errors(value)` 的子集）。
///
/// 关键字顺序逐字对齐 TypeBox 1.3 的 `ErrorSchema`：`type` → 对象关键字
/// （`required` → `patternProperties` → `properties`）→ 数组关键字（`items` → `maxItems`
/// → `minItems`）→ 字符串（`maxLength` → `minLength`）→ 数字
/// （`exclusiveMaximum` → `exclusiveMinimum` → `maximum` → `minimum`）→ `const` → `enum`
/// → `anyOf`。关键字**不短路**：`type` 失败后 `const`/`enum`/`anyOf` 仍会求值
/// （对象/数组/字符串/数字关键字按值的实际类型门控）。
///
/// `ErrorContext` 在 [`MAX_ERRORS`] 条后不再记录，`anyOf` 的每个分支使用独立上下文。
///
/// 各关键字按 JSON 值的实际类型分派：对象关键字只在值是对象时执行，数组/字符串/数字
/// 同理；`const`/`enum`/`anyOf` 不看类型、总会求值。`path` 是当前遍历栈（首次调用传空
/// `Vec`），函数内部只做 `push`/`pop`，因此错误里的 `instance_path` 就是出错节点在文档
/// 中的位置。`type` 名不认识时按通过处理（防御分支；内嵌 schema 只使用 `object`/`array`/
/// `string`/`number`/`integer`/`boolean`/`null`）。
fn validate_into(
    schema: &Value,
    value: &Value,
    path: &mut Vec<String>,
    errors: &mut Vec<ModelsJsonValidationError>,
) {
    // `ErrorSchema` 开头的 `context.AtCapacity()`：达到上限后整棵子树都不再校验。
    if errors.len() >= MAX_ERRORS {
        return;
    }
    let Some(schema) = schema.as_object() else {
        return;
    };

    if let Some(type_name) = schema.get("type").and_then(Value::as_str) {
        // 只判断 JSON 形状；未知 type 名视为通过（内嵌 schema 只用到下面七种）。
        let matches = match type_name {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "number" => value.is_number(),
            // JS `Number.isInteger`：有限且无小数部分，没有整数位宽的概念。
            "integer" => value
                .as_f64()
                .is_some_and(|number| number.is_finite() && number.fract() == 0.0),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => true,
        };
        if !matches {
            push_error(errors, path, "type", format!("must be {type_name}"));
        }
    }

    if let Value::Object(object) = value {
        // `required` 的错误要携带 `requiredProperties`，所以不能走 `push_error`，
        // 上限检查在这里重复一次。缺失项按 schema 中 required 数组的顺序收集。
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            let mut missing = Vec::new();
            for name in required {
                if let Some(name) = name.as_str()
                    && !object.contains_key(name)
                {
                    missing.push(name.to_string());
                }
            }
            if !missing.is_empty() && errors.len() < MAX_ERRORS {
                errors.push(ModelsJsonValidationError {
                    keyword: "required".to_string(),
                    instance_path: pointer(path),
                    message: format!("must have required properties {}", missing.join(", ")),
                    required_properties: missing,
                });
            }
        }
        // TypeBox 先求值 `patternProperties` 再求值 `properties`。
        // 每个 pattern 对对象的所有键逐一匹配；map 保序（workspace 的 serde_json 开了
        // preserve_order），所以同一 pattern 下的错误顺序与文档中的键顺序一致。
        // 正则来自内嵌 schema，编译失败时跳过该 pattern（防御分支，正常不会发生）。
        if let Some(patterns) = schema.get("patternProperties").and_then(Value::as_object) {
            for (pattern, property_schema) in patterns {
                let Ok(regex) = regex::Regex::new(pattern) else {
                    continue;
                };
                for (name, property_value) in object {
                    if regex.is_match(name) {
                        path.push(name.clone());
                        validate_into(property_schema, property_value, path, errors);
                        path.pop();
                    }
                }
            }
        }
        // `properties` 在 `patternProperties` 之后求值，只校验对象上实际存在的键。
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (name, property_schema) in properties {
                if let Some(property_value) = object.get(name) {
                    path.push(name.clone());
                    validate_into(property_schema, property_value, path, errors);
                    path.pop();
                }
            }
        }
    }

    if let Value::Array(items) = value {
        // 数组元素用十进制下标作为路径段（`models/0/id`）。
        if let Some(item_schema) = schema.get("items") {
            for (index, item) in items.iter().enumerate() {
                path.push(index.to_string());
                validate_into(item_schema, item, path, errors);
                path.pop();
            }
        }
        if let Some(max_items) = schema.get("maxItems").and_then(Value::as_u64)
            && items.len() as u64 > max_items
        {
            push_error(
                errors,
                path,
                "maxItems",
                format!("must not have more than {max_items} items"),
            );
        }
        if let Some(min_items) = schema.get("minItems").and_then(Value::as_u64)
            && (items.len() as u64) < min_items
        {
            push_error(
                errors,
                path,
                "minItems",
                format!("must not have fewer than {min_items} items"),
            );
        }
    }

    // TypeBox 的 maxLength/minLength 按 UTF-16 码元计数（JS `string.length`），
    // 不是 Unicode 字符数，也不是字节数。
    if let Some(text) = value.as_str() {
        if let Some(max_length) = schema.get("maxLength").and_then(Value::as_u64) {
            let length = text.encode_utf16().count() as u64;
            if length > max_length {
                push_error(
                    errors,
                    path,
                    "maxLength",
                    format!("must not have more than {max_length} characters"),
                );
            }
        }
        if let Some(min_length) = schema.get("minLength").and_then(Value::as_u64) {
            let length = text.encode_utf16().count() as u64;
            if length < min_length {
                push_error(
                    errors,
                    path,
                    "minLength",
                    format!("must not have fewer than {min_length} characters"),
                );
            }
        }
    }

    // 数字比较统一提升为 f64，与 JS 的 Number 语义一致；超大整数会像 JS 一样丢精度。
    if value.is_number() {
        let number = value_as_f64(value).unwrap_or(f64::NAN);
        if let Some(maximum) = schema.get("exclusiveMaximum").and_then(Value::as_f64)
            && number >= maximum
        {
            push_error(
                errors,
                path,
                "exclusiveMaximum",
                format!("must be < {}", js_number_text(maximum)),
            );
        }
        if let Some(minimum) = schema.get("exclusiveMinimum").and_then(Value::as_f64)
            && number <= minimum
        {
            push_error(
                errors,
                path,
                "exclusiveMinimum",
                format!("must be > {}", js_number_text(minimum)),
            );
        }
        if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64)
            && number > maximum
        {
            push_error(
                errors,
                path,
                "maximum",
                format!("must be <= {}", js_number_text(maximum)),
            );
        }
        if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64)
            && number < minimum
        {
            push_error(
                errors,
                path,
                "minimum",
                format!("must be >= {}", js_number_text(minimum)),
            );
        }
    }

    // `const`/`enum` 用 JSON 结构相等比较；即使上面的 `type` 已经失败，也会继续求值
    // （fixture 的 `union const after type error` 用例锁定这一行为）。
    if let Some(constant) = schema.get("const")
        && constant != value
    {
        push_error(errors, path, "const", "must be equal to constant");
    }

    if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
        && !allowed.iter().any(|entry| entry == value)
    {
        push_error(
            errors,
            path,
            "enum",
            "must be equal to one of the allowed values",
        );
    }

    // `anyOf`：TypeBox 给每个分支一个独立的 `ErrorContext`（各自的 8 条上限），
    // 全部失败时把分支错误按顺序合并进父上下文，再追加一条 `anyOf` 错误。
    // 任一分支产出零错误即视为通过并立即返回（后续分支不再求值）。合并回父上下文时
    // 同样受父上下文上限约束：若合并后父上下文已满，最后那条 `anyOf` 错误会被
    // `push_error` 丢弃。
    if let Some(branches) = schema.get("anyOf").and_then(Value::as_array) {
        let mut branch_errors = Vec::new();
        for branch in branches {
            let mut current = Vec::new();
            validate_into(branch, value, path, &mut current);
            if current.is_empty() {
                return;
            }
            branch_errors.extend(current);
        }
        for error in branch_errors {
            if errors.len() >= MAX_ERRORS {
                break;
            }
            errors.push(error);
        }
        push_error(errors, path, "anyOf", "must match a schema in anyOf");
    }
}

/// 校验 models.json 根对象，返回 TypeBox 形状的错误列表（按遍历顺序）。
///
/// 空 `Vec` 表示通过（`ModelConfig::load` 以此为接受门槛）。返回顺序是 schema 的深度优先
/// 遍历顺序，不是排序结果；最多 8 条（`MAX_ERRORS`）。
pub fn validate_models_config(value: &Value) -> Vec<ModelsJsonValidationError> {
    let mut errors = Vec::new();
    validate_into(models_config_schema(), value, &mut Vec::new(), &mut errors);
    errors
}

// ---------------------------------------------------------------------------
// ModelConfig
// ---------------------------------------------------------------------------

/// `ModelConfig.load` 的错误前缀（逐字）。
const LOAD_ERROR_PREFIX: &str = "Failed to load models.json: ";
/// `ModelConfig.load` 的解析错误前缀（逐字）。
const PARSE_ERROR_PREFIX: &str = "Failed to parse models.json: ";
/// `ModelConfig.load` 的 schema 错误前缀（逐字）。
const SCHEMA_ERROR_PREFIX: &str = "Invalid models.json schema:\n";

/// One immutable load of models.json.
///
/// TS: `model-config.ts#ModelConfig`。
///
/// 一次加载的不可变快照。`error == None` 时 provider 表可信（可以合法为空，例如文件不存在
/// 或 `providers: {}`）；`error == Some` 时 provider 表必为空，**不能**当作部分配置使用
/// （见 [`ModelConfig::load`]）。没有 setter 或按需重载接口；运行时把它放在
/// `Mutex` 里，`load` 返回后整体替换（即使 `error` 为 `Some`，旧配置也会被替换）。
/// `Clone` 便宜：provider 条目是
/// `Arc<ModelsJsonProvider>`，克隆只复制 `IndexMap` 与 `Arc` 指针，不复制 provider 内容。
#[derive(Debug, Clone, Default)]
pub struct ModelConfig {
    providers: IndexMap<String, Arc<ModelsJsonProvider>>,
    error: Option<String>,
}

impl ModelConfig {
    /// 空配置（无错误）。等价于 `ModelConfig::default()`。
    pub fn empty() -> Self {
        Self::default()
    }

    /// `ModelConfig.load(modelsJsonPath)`。
    ///
    /// `None`（或空串，对应 TS 的 `!modelsJsonPath`）→ 空配置。`ENOENT` → 空配置（无错误）；
    /// 其他 IO 错误、JSON 解析错误与 schema 错误分别使用逐字前缀，并以
    /// `\n\nFile: {path}` 结尾。
    ///
    /// # 路径
    /// 路径经 `kiana_core::paths::normalize_path` 归一化（默认选项：不 trim、展开 `~`、
    /// 把 `file://` URL 转成路径）。错误文案里的 `path` 是这个归一化后的值，不是原始入参。
    ///
    /// # 解析与校验
    /// 文件内容按 UTF-8 有损解码（非法字节 → U+FFFD），再去 BOM、去 `//` 行注释与尾逗号
    /// （`/* */` 块注释**不**支持，会变成 JSON 语法错误），然后用与 `JSON.parse` 一致的
    /// 解析器解析。schema 错误按 `  - <path>: <message>` 逐条拼接，最多 8 条（顺序即
    /// [`validate_models_config`] 的遍历顺序）。
    ///
    /// # 失败语义
    /// 除「未提供路径」与「文件不存在」外，任何错误都返回空 provider 表 + `Some(error)`；
    /// 调用方不应假设还能拿到部分配置。IO 错误文案见 `node_read_error_message`。
    pub async fn load(models_json_path: Option<&str>) -> Self {
        let Some(models_json_path) = models_json_path.filter(|path| !path.is_empty()) else {
            return Self::empty();
        };
        // 与 TS 的 normalizePath 对齐；后续错误文案里的 path 也是这个归一化值。
        let path = kiana_core::paths::normalize_path(
            models_json_path,
            &kiana_core::paths::PathInputOptions::default(),
        );

        // models.json 是可选配置：文件不存在不算错误（TS 的 ENOENT 分支）。
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Self::empty();
            }
            Err(error) => {
                return Self {
                    providers: IndexMap::new(),
                    error: Some(format!(
                        "{LOAD_ERROR_PREFIX}{}\n\nFile: {path}",
                        node_read_error_message(&error, &path)
                    )),
                };
            }
        };
        // Node `readFile(path, "utf-8")`：非法字节替换为 U+FFFD。
        let content = String::from_utf8_lossy(&bytes).into_owned();

        // JSONC 处理：先去 BOM，再删 `//` 行注释与尾逗号（不支持块注释）。
        let json = strip_json_comments(&kiana_core::text::strip_bom(&content));
        let parsed = match kiana_json::js_parse::parse(&json) {
            Ok(parsed) => parsed,
            Err(error) => {
                return Self {
                    providers: IndexMap::new(),
                    error: Some(format!(
                        "{PARSE_ERROR_PREFIX}{}\n\nFile: {path}",
                        error.message
                    )),
                };
            }
        };

        // 唯一接受门槛：TypeBox 子集校验；返回顺序就是错误文案的顺序。
        let validation_errors = validate_models_config(&parsed);
        if !validation_errors.is_empty() {
            let messages = validation_errors
                .iter()
                .map(|error| format!("  - {}: {}", format_validation_path(error), error.message))
                .collect::<Vec<_>>()
                .join("\n");
            return Self {
                providers: IndexMap::new(),
                error: Some(format!("{SCHEMA_ERROR_PREFIX}{messages}\n\nFile: {path}")),
            };
        }

        // 校验通过后反序列化。正常路径上形状已由 schema 保证；这里是防御分支：若校验器的
        // 关键字子集与 serde 类型判断出现分歧（或将来新增了 schema 不约束的字段类型），
        // 错误会以同一条 schema 错误文案报出，而不是 panic。
        // 注意 `inputLimits` 的整数由自定义反序列化饱和到 u64，不会在这里失败。
        let config: ModelsConfig = match serde_json::from_value(parsed) {
            Ok(config) => config,
            Err(error) => {
                return Self {
                    providers: IndexMap::new(),
                    error: Some(format!(
                        "{SCHEMA_ERROR_PREFIX}  - root: {error}\n\nFile: {path}"
                    )),
                };
            }
        };

        // provider 顺序由保序 map 保持（JSON 中出现顺序）；Arc 供运行时只读共享。
        let providers = config
            .providers
            .into_iter()
            .map(|(provider_id, provider)| (provider_id, Arc::new(provider)))
            .collect();
        Self {
            providers,
            error: None,
        }
    }

    /// `getProvider(providerId)`。
    ///
    /// 按 provider id 精确查找（区分大小写）；找不到返回 `None`。返回的是共享 `Arc` 克隆，
    /// 调用方持有它不会影响下次 `load` 对配置的整体替换。
    pub fn get_provider(&self, provider_id: &str) -> Option<Arc<ModelsJsonProvider>> {
        self.providers.get(provider_id).cloned()
    }

    /// `getProviderIds()`（插入顺序）。
    ///
    /// 返回 JSON 文本中的 provider 出现顺序快照（每次调用新建 `Vec`）。运行时的 provider
    /// 合并顺序依赖它。
    pub fn get_provider_ids(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    /// `getError()`。
    ///
    /// 返回加载时记录的错误（已包含 `File: <path>` 后缀）；加载成功或空配置返回 `None`。
    /// 配置不可变，因此错误在被替换前不会变化。
    pub fn get_error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// Node `fs.readFile` 的 IO 错误文案（`<CODE>: <description>, <syscall> '<path>'`）。
///
/// 文案取自 libuv 的 `uv_strerror`（Node 的 `error.message` 用的是它）。`ENOENT` 在上层被
/// 单独处理（返回空配置）。`readFile` 先 `open` 再 `read`：Linux 上对目录的读取失败是
/// `EISDIR`（syscall `read`），其余错误都来自 `open`。
///
/// 只映射下面列出的 errno。`raw_os_error()` 为 `None`（非 OS 错误）或 errno 不在列表中时，
/// 退回 `error.to_string()`，此时不保证 Node 的形状（调用方仍会得到一条可读错误）。
fn node_read_error_message(error: &std::io::Error, path: &str) -> String {
    let Some(errno) = error.raw_os_error() else {
        // 没有 OS errno 就无法还原 Node 文案，只能用标准库的 Display。
        return error.to_string();
    };
    // 表只覆盖 Node 读文件时可能暴露的 errno；未列出的落到下面的 `_` 分支。
    let (code, description) = match errno {
        libc::ENOENT => ("ENOENT", "no such file or directory"),
        libc::EACCES => ("EACCES", "permission denied"),
        libc::EPERM => ("EPERM", "operation not permitted"),
        libc::EISDIR => ("EISDIR", "illegal operation on a directory"),
        libc::ENOTDIR => ("ENOTDIR", "not a directory"),
        libc::ELOOP => ("ELOOP", "too many symbolic links encountered"),
        libc::ENAMETOOLONG => ("ENAMETOOLONG", "name too long"),
        libc::EMFILE => ("EMFILE", "too many open files"),
        libc::ENFILE => ("ENFILE", "file table overflow"),
        libc::ENOMEM => ("ENOMEM", "not enough memory"),
        libc::EINVAL => ("EINVAL", "invalid argument"),
        libc::EROFS => ("EROFS", "read-only file system"),
        _ => return error.to_string(),
    };
    // readFile 先 open 后 read：目录的读取失败发生在 read 阶段，其余 errno 都来自 open。
    let syscall = if errno == libc::EISDIR {
        "read"
    } else {
        "open"
    };
    format!("{code}: {description}, {syscall} '{path}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_required_paths_like_typebox() {
        let error = ModelsJsonValidationError {
            keyword: "required".to_string(),
            instance_path: "/providers/demo/models/0".to_string(),
            message: "must have required properties id".to_string(),
            required_properties: vec!["id".to_string()],
        };
        assert_eq!(format_validation_path(&error), "providers.demo.models.0.id");
        let root = ModelsJsonValidationError {
            keyword: "required".to_string(),
            instance_path: String::new(),
            message: "must have required properties providers".to_string(),
            required_properties: vec!["providers".to_string()],
        };
        assert_eq!(format_validation_path(&root), "providers");
        let plain = ModelsJsonValidationError {
            keyword: "type".to_string(),
            instance_path: String::new(),
            message: "must be object".to_string(),
            required_properties: Vec::new(),
        };
        assert_eq!(format_validation_path(&plain), "root");
    }
}
