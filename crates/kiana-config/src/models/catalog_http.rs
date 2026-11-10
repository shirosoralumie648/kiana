//! `requestCatalog` / `readCatalog` / `parseCatalog`（基于 reqwest）。
//!
//! TS 源：`packages/coding-agent/src/core/remote-catalog-provider.ts` 的辅助函数。
//! 规范：`spec/ca-config-models.md` §2.24.2、§A.1.5（307 重定向契约）。
//!
//! - URL：`<catalogBaseUrl>/api/models/providers/<encodeURIComponent(id)>?types=chat%2Cimage%2Cclassifier`。
//!   `encodeURIComponent` 的保留集是 `A-Za-z0-9-_.!~*'()`（**不是** form-urlencoded），
//!   这里显式实现；请求本身**不**拼 `kiana-version`（服务端按 User-Agent 识别版本并 307 重定向，
//!   `reqwest` 的共享 client 默认跟随 307/308）。
//! - 请求头：`accept: application/json`、`User-Agent: getKianaUserAgent(VERSION)`，有 validator 时
//!   附 `if-none-match`。
//! - 每次尝试 4 000 ms 超时；重试/状态码策略复用 [`kiana_core::management_http::fetch_with_retry`]。
//! - `parseCatalog`：接受数组、`{ models: [...] }` 或以 id 为键的对象；条目必须是有 `id` 的对象
//!   且 `type` 缺失或属于 `chat`/`image`/`classifier`；`provider` 强制改写为请求的 providerId。
//! - `readCatalog`：`Last-Modified` 用 `Date.parse` 语义解析为 epoch 毫秒，缺失/非法为 `0`；
//!   `ETag` 原样保存（含引号）。
//!
//! # 这个文件负责什么、不负责什么
//! 负责：发出远端目录请求，并把一个**成功**响应的头与 body 解析为 [`CatalogResponse`]。
//! 不负责：状态码分支（304/404/501/非 2xx）、刷新间隔、`checkedAt`、持久化与发布——这些都在
//! 调用方 `crate::models::remote_catalog` 里。拆分后 [`read_catalog`] 可以假定响应已经成功，
//! 只需关心头与 body；[`request_catalog`] 只管发请求，把重试/超时/取消委托给
//! `kiana_core::management_http::fetch_with_retry`。
//!
//! # 调用方与调用顺序
//! 生产代码里只有 `crate::models::remote_catalog` 的两个 provider 调用：`RemoteCatalogProvider`
//! （带 etag validator）与个性化目录 provider（不传 validator）。两者都是：
//! 1. [`request_catalog`] 拿到原始响应；
//! 2. 调用方检查 `status()`，两条路径不同：`RemoteCatalogProvider` 对 304 提前返回
//!    （视为无更新）、404/501 清空条目，其它非 2xx 报错；个性化目录 provider 对
//!    404/501 视为“没有可用的远端目录”并继续用内置目录个性化，其它非 2xx（含 304，
//!    因为该路径不传 validator）报错；
//! 3. 只有成功响应才交给 [`read_catalog`]（它会读走 body，之后无法再取状态码）。
//!
//! # 为什么同时保留强类型和原始 JSON
//! [`AnyModel`] 的必填字段必须完整，而目录条目可能只有 `id`。因此 [`ParsedCatalog`] 与
//! [`CatalogResponse`] 都同时给出 `models`（请求侧读字段用的尽力投影）和 `raw_models`
//! （TS 形状的原始对象）。[`AnyModel::Remote`] 序列化时写出的是原始 `json`，所以持久化
//! 不会丢未知字段、显式 `type` 或键顺序。两个列表由同一个过滤结果按相同顺序生成，
//! 长度相等、下标一一对应。
//!
//! # 为什么自己实现 `Date.parse`
//! `Last-Modified` 是服务端生成的任意日期字符串，并不保证是标准 HTTP 日期格式。chrono
//! 的 RFC 2822 解析器不接受 `UTC` 后缀等 V8 接受的形式（见下方日期测试的注释），而解析
//! 失败会被记为 `0`；在配置了本地生成时间（`localGeneratedAt`）时，目录不会被应用到
//! 覆盖层（主 provider 仍会持久化该条目，只是内存覆盖层为空）；个性化目录路径则直接
//! 忽略远端模型。因此这里按 V8 的两级解析器（ISO + legacy）对齐其常用形式，而不是直接
//! 调用 chrono。
//!
//! # 边界与失败模式
//! - catalog base URL 无法解析 → `AiError { name: "TypeError", message: "Invalid URL" }`；
//! - 网络失败、超时或取消 → `AiErrorKind::Other`，message 为 `fetch_with_retry` 的展示文案
//!   （取消返回错误，不是静默的 `Ok`）；
//! - 响应体不是合法 JSON → `AiErrorKind::Other`，message 为 JS `JSON.parse` 的错误文案；
//! - 顶层值既不是数组也不是对象 → 逐字错误
//!   `Invalid model catalog for provider "<id>"`；
//! - **单个条目**不合格（非对象、无 `id`、`type` 不支持）只会被过滤，不会让整次刷新失败；
//!   `Last-Modified` 解析失败同样不报错，只是记为 `0`。
//!
//! # 阅读顺序
//! [`request_catalog`] → [`read_catalog`] → [`parse_catalog`] → `parse_catalog_entry` /
//! `lenient_model`（条目投影）→ `parse_http_date` 及其 legacy 解析器（只影响
//! `last_modified`）。

use kiana_ai::error::{AiError, AiErrorKind};
use kiana_ai::models::get_model_type;
use kiana_ai::types::{
    AnyModel, Model, ModelCost, ModelCostTier, ModelInputModality, ModelPromptCache,
    ModelThinkingLevel, ModelType, PromptCacheTier, SamplingParams, SamplingParamsByThinkingLevel,
    ThinkingLevelMap,
};
use kiana_ai::utils::abort::AbortScope;
use kiana_core::config::VERSION;
use kiana_core::http_dispatcher::shared_http_client;
use kiana_core::management_http::{FetchRetryOptions, fetch_with_retry};
use reqwest::{Response, Url};
use serde_json::{Map, Value};

use crate::models::config_file::{input_limits_from_value, js_integer_to_u64};

/// 每次尝试的超时（毫秒）。TS: `REMOTE_CATALOG_ATTEMPT_TIMEOUT_MS = 4_000`。
///
/// 只限制**单次**尝试；[`request_catalog`] 的其余重试参数取 `FetchRetryOptions` 的默认值
/// （首次 + 最多 2 次重试、可重试状态码也重试、无总预算），所以一次刷新最坏会做 3 次
/// 各 4 000 ms 的尝试（`fetch_with_retry` 在两次尝试之间不等待）。
pub const CATALOG_ATTEMPT_TIMEOUT_MS: u64 = 4_000;

/// `?types=` 的取值（逗号在 URL 里编码为 `%2C`）。
///
/// 同时是条目过滤的白名单：`type` 缺失视为 chat，显式 `type: null` 或其它字符串会被
/// 丢弃（见 `is_supported_model_type`）。数组顺序决定查询串内容，测试
/// `builds_the_catalog_url` 锁定了逐字结果。
pub const CATALOG_MODEL_TYPES: [&str; 3] = ["chat", "image", "classifier"];

/// `parseCatalog` 的结果。
///
/// TS 把目录条目当作 `AnyModel` 原样保留；Rust 的 [`AnyModel`] 无法表示缺失必填字段的
/// 条目，因此同时给出强类型投影 [`Self::models`] 与逐字保留的 [`Self::raw_models`]
/// （持久化时写回原始条目，未知字段与显式 `type` 不丢失）。
///
/// `models` 与 `raw_models` 由同一个过滤结果按相同顺序生成：长度相等，下标对应；
/// `raw_models[i]` 是覆盖了 `provider` 的原对象，`models[i]` 是它的尽力投影。
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCatalog {
    /// 强类型模型（`provider` 已强制改写）。
    pub models: Vec<AnyModel>,
    /// TS 形状的原始条目（`provider` 已强制改写），用于持久化。
    pub raw_models: Vec<Value>,
}

/// 一次成功目录响应的解析结果。
///
/// 由 [`read_catalog`] 构造；`models` 与 `raw_models` 的关系同 [`ParsedCatalog`]，
/// 每个条目的 `provider` 都已被改写为请求的 provider id。
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogResponse {
    /// 全类型模型列表（`provider` 已强制改写）。
    pub models: Vec<AnyModel>,
    /// 原始条目（`provider` 已强制改写），用于 `ModelsStoreEntry::raw_models`。
    pub raw_models: Vec<Value>,
    /// `Last-Modified` 的 epoch 毫秒；缺失/非法为 0。
    pub last_modified: i64,
    /// `ETag`（逐字，含引号）。
    pub etag: Option<String>,
}

/// `encodeURIComponent(value)`：保留 `A-Za-z0-9-_.!~*'()`，其余按 UTF-8 字节大写十六进制编码。
///
/// 不借用表单编码：空格是 `%20` 而不是 `+`，`/`、`?`、`#`、`,` 也一律转义。逗号被转义
/// 正是 `?types=` 里出现 `%2C` 的原因。
///
/// 输入是 Rust `&str`（保证合法 UTF-8），不存在 JS 里孤立代理项触发 `URIError` 的情况，
/// 因此函数不会失败、也不需要错误返回。
pub fn encode_uri_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        let character = byte as char;
        if character.is_ascii_alphanumeric()
            || matches!(
                character,
                '-' | '_' | '.' | '!' | '~' | '*' | '\'' | '(' | ')'
            )
        {
            encoded.push(character);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// 由 catalog base URL 构造请求 URL（`new URL('/api/...', base)` + `types` 查询参数）。
#[allow(clippy::result_large_err)]
fn catalog_url(provider_id: &str, catalog_base_url: &str) -> Result<Url, AiError> {
    // Node `new URL(path, base)` 失败时抛 `TypeError("Invalid URL")`，文案逐字固定；
    // `url` crate 的解析细节不进入面向用户的消息。
    let base = Url::parse(catalog_base_url)
        .map_err(|_| AiError::new(AiErrorKind::Other, "Invalid URL").with_name("TypeError"))?;
    // 路径段只由 `encode_uri_component` 生成，provider id 里的 `/`、`?`、`#` 不会改变
    // URL 结构；查询串用 `set_query` 整体设置，已编码的 `%2C` 不会被再次转义
    // （测试 `builds_the_catalog_url` 锁定最终字符串）。
    let mut url = base
        .join(&format!(
            "/api/models/providers/{}",
            encode_uri_component(provider_id)
        ))
        .map_err(|_| AiError::new(AiErrorKind::Other, "Invalid URL").with_name("TypeError"))?;
    url.set_query(Some(&format!(
        "types={}",
        encode_uri_component(&CATALOG_MODEL_TYPES.join(","))
    )));
    Ok(url)
}

/// `isSupportedModelType(model)`：`type` 缺失或属于 `chat`/`image`/`classifier`。
///
/// 注意与 TS 的严格等价：显式 `type: null` 也**不**支持（只有键不存在才算缺失），
/// 未知字符串与非字符串同样被拒绝。
fn is_supported_model_type(entry: &serde_json::Map<String, Value>) -> bool {
    match entry.get("type") {
        None => true,
        Some(Value::String(name)) => CATALOG_MODEL_TYPES.contains(&name.as_str()),
        Some(_) => false,
    }
}

/// 目录条目的 `type`（`chat`/`image`/`classifier`）；缺省为 `None`（即 chat）。
fn declared_model_type(entry: &Map<String, Value>) -> Option<ModelType> {
    match entry.get("type").and_then(Value::as_str) {
        Some("chat") => Some(ModelType::Chat),
        Some("image") => Some(ModelType::Image),
        Some("classifier") => Some(ModelType::Classifier),
        _ => None,
    }
}

/// 把条目映射成 [`AnyModel`]（`provider` 强制覆盖）。
///
/// TS 只做浅层形状过滤并把对象断言成 `AnyModel`；完整条目走 `serde` 强类型反序列化，
/// 残缺/类型不符的条目用宽松解析补齐（TS 里这些字段是 `undefined`，Rust 只能取字段
/// 默认值），保证条目不会从目录里消失。显式 `type: "chat"` 原样保留。
///
/// 两份数据都来自同一个覆盖过 `provider` 的对象：强类型路径用它反序列化，原始 JSON
/// 也直接保存它，因此两者看到的 `provider` 一定相同。
fn parse_catalog_entry(provider_id: &str, entry: &Map<String, Value>) -> AnyModel {
    let mut object = entry.clone();
    object.insert(
        "provider".to_string(),
        Value::String(provider_id.to_string()),
    );
    // 优先严格反序列化：能成功就保留 `api` 等字段的类型信息。
    // `Model` 的 `id`/`name`/`api`/`provider`/`base_url`/`input`/`cost` 都没有 serde
    // 默认值，因此像 `{"id": "x"}` 这样只有 id 的条目必然失败并落到宽松路径。
    let model = serde_json::from_value::<Model>(Value::Object(object.clone()))
        .unwrap_or_else(|_| lenient_model(provider_id, entry));
    AnyModel::Remote {
        model,
        json: Value::Object(object),
    }
}

/// 宽松构造 [`Model`]：只取类型正确的已知字段，其余用结构体默认值。
///
/// 调用方保证条目已通过 `parse_catalog` 的过滤（是 JSON 对象且含 `id`）。这里刻意不返回
/// `Result`：缺失或类型不符的字段退化为默认值，而不是让整个条目从目录里消失（TS 侧这些
/// 字段只是 `undefined`）。几处不直观的转换：
/// - `id` 走 `js_string`，即 JS `String()` 语义：数字 id 变成十进制字符串；
/// - 数值字段（`contextWindow`/`maxTokens`）走 `js_integer_to_u64`，负数、小数、
///   非有限值与非数字都变成 `None`；
/// - 未知键一律忽略，只有下面显式读取的键会进入 [`Model`]。
fn lenient_model(provider_id: &str, entry: &Map<String, Value>) -> Model {
    let string = |key: &str| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let number = |key: &str| entry.get(key).and_then(js_integer_to_u64);
    let model_type = declared_model_type(entry);
    Model {
        id: entry
            .get("id")
            .map(|value| kiana_json::js_string::js_string(Some(value)))
            .unwrap_or_default(),
        name: string("name"),
        api: string("api"),
        provider: provider_id.to_string(),
        base_url: string("baseUrl"),
        input: entry
            .get("input")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| match item.as_str() {
                        Some("text") => Some(ModelInputModality::Text),
                        Some("image") => Some(ModelInputModality::Image),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        input_limits: entry.get("inputLimits").map(input_limits_from_value),
        cost: cost_from_value(entry.get("cost")),
        headers: entry
            .get("headers")
            .and_then(Value::as_object)
            .map(|headers| {
                headers
                    .iter()
                    .filter_map(|(name, value)| {
                        value
                            .as_str()
                            .map(|value| (name.clone(), value.to_string()))
                    })
                    .collect()
            }),
        model_type,
        reasoning: entry.get("reasoning").and_then(Value::as_bool),
        thinking_level_map: thinking_level_map_from_value(entry.get("thinkingLevelMap")),
        prompt_cache: prompt_cache_from_value(entry.get("promptCache")),
        context_window: number("contextWindow"),
        max_tokens: number("maxTokens"),
        sampling_params: sampling_params_from_value(entry.get("samplingParams")),
        sampling_params_by_thinking_level: sampling_params_by_thinking_level_from_value(
            entry.get("samplingParamsByThinkingLevel"),
        ),
        compat: entry.get("compat").cloned(),
        output: entry.get("output").and_then(Value::as_array).map(|output| {
            output
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        }),
    }
}

/// 宽松解析 `cost`：缺失或不是对象时返回全 0 的 [`ModelCost`]；对象内缺失或非数值的费率
/// 同样按 `0.0` 处理；`tiers` 里不是对象的元素直接跳过（不会让整个模型失败）。
fn cost_from_value(value: Option<&Value>) -> ModelCost {
    let Some(object) = value.and_then(Value::as_object) else {
        return ModelCost::default();
    };
    let rate = |key: &str| object.get(key).and_then(Value::as_f64).unwrap_or(0.0);
    ModelCost {
        input: rate("input"),
        output: rate("output"),
        cache_read: rate("cacheRead"),
        cache_write: rate("cacheWrite"),
        tiers: object.get("tiers").and_then(Value::as_array).map(|tiers| {
            tiers
                .iter()
                .filter_map(|tier| {
                    let tier = tier.as_object()?;
                    let rate = |key: &str| tier.get(key).and_then(Value::as_f64).unwrap_or(0.0);
                    Some(ModelCostTier {
                        input_tokens_above: rate("inputTokensAbove"),
                        input: rate("input"),
                        output: rate("output"),
                        cache_read: rate("cacheRead"),
                        cache_write: rate("cacheWrite"),
                    })
                })
                .collect()
        }),
    }
}

/// `thinkingLevelMap` 的 JSON 键 → [`ModelThinkingLevel`]（未知键丢弃）。
fn parse_thinking_level(key: &str) -> Option<ModelThinkingLevel> {
    match key {
        "off" => Some(ModelThinkingLevel::Off),
        "minimal" => Some(ModelThinkingLevel::Minimal),
        "low" => Some(ModelThinkingLevel::Low),
        "medium" => Some(ModelThinkingLevel::Medium),
        "high" => Some(ModelThinkingLevel::High),
        "xhigh" => Some(ModelThinkingLevel::Xhigh),
        "max" => Some(ModelThinkingLevel::Max),
        _ => None,
    }
}

/// 解析 `thinkingLevelMap`：未知键丢弃，值必须是字符串（映射到 `Some(text)`）或 `null`
/// （映射到 `None`），其它类型丢弃。输入是对象时，即使所有键都被丢弃也返回 `Some(空表)`；
/// 输入缺失或不是对象返回 `None`。
fn thinking_level_map_from_value(value: Option<&Value>) -> Option<ThinkingLevelMap> {
    let object = value?.as_object()?;
    let mut map = ThinkingLevelMap::new();
    for (key, value) in object {
        let Some(level) = parse_thinking_level(key) else {
            continue;
        };
        match value {
            Value::String(text) => {
                map.insert(level, Some(text.clone()));
            }
            Value::Null => {
                map.insert(level, None);
            }
            _ => {}
        }
    }
    Some(map)
}

/// 解析 `promptCache`：只认 `short`/`long` 两个键，值必须是数字。输入是对象时即使没有
/// 任何有效项也返回 `Some(空表)`；不是对象返回 `None`。
fn prompt_cache_from_value(value: Option<&Value>) -> Option<ModelPromptCache> {
    let object = value?.as_object()?;
    let mut map = ModelPromptCache::new();
    for (key, value) in object {
        let tier = match key.as_str() {
            "short" => PromptCacheTier::Short,
            "long" => PromptCacheTier::Long,
            _ => continue,
        };
        if let Some(number) = value.as_f64() {
            map.insert(tier, number);
        }
    }
    Some(map)
}

/// 解析 `samplingParams`：对象按键值原样克隆（键名不做白名单校验），不是对象返回 `None`。
fn sampling_params_from_value(value: Option<&Value>) -> Option<SamplingParams> {
    let object = value?.as_object()?;
    Some(
        object
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect(),
    )
}

/// 解析 `samplingParamsByThinkingLevel`：级别键按 `parse_thinking_level` 过滤，值必须是
/// 对象（否则跳过该级别），对象内容同样原样克隆。输入是对象时返回 `Some`（可能为空表）。
fn sampling_params_by_thinking_level_from_value(
    value: Option<&Value>,
) -> Option<SamplingParamsByThinkingLevel> {
    let object = value?.as_object()?;
    let mut map = SamplingParamsByThinkingLevel::new();
    for (key, value) in object {
        let Some(level) = parse_thinking_level(key) else {
            continue;
        };
        let Some(params) = value.as_object() else {
            continue;
        };
        map.insert(
            level,
            params
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
        );
    }
    Some(map)
}

/// `parseCatalog(providerId, value)`。
///
/// 把顶层 JSON 归一化成条目列表，按 TS 的规则过滤，并为每个接受条目强制写入
/// `provider = provider_id`（调用方之后按 provider id 过滤合并结果，条目里自带的
/// `provider` 不允许决定归属）。
///
/// # 支持的顶层形状
/// - 数组：元素即条目；
/// - 对象且 `models` 是数组：取 `models`；
/// - 其它对象：取所有值（因此“以 id 为键的对象映射”也能用）。注意 `models` 存在但不是
///   数组时会落入第三个分支，那个非数组的 `models` 值本身也会作为条目参与检查（通常不是
///   对象而被丢弃）。
///
/// # 过滤规则（依次执行）
/// 1. 条目必须是 JSON 对象（`null`、数组、标量丢弃）；
/// 2. 必须包含 `id` 键（值为 `null` 也算；判据是键存在而非值非空）；
/// 3. `type` 缺失，或为 `chat`/`image`/`classifier` 之一。
///
/// # 返回
/// [`ParsedCatalog`]，`models` 与 `raw_models` 按下标对应。对象值的遍历顺序与原始条目的
/// 键顺序都是插入顺序（workspace 的 `serde_json` 启用了 `preserve_order`）。
///
/// # 错误
/// 只有顶层值不是数组也不是对象时返回错误：`AiErrorKind::Other`，message 逐字为
/// `Invalid model catalog for provider "<provider_id>"`。条目级问题只过滤、不报错。
#[allow(clippy::result_large_err)]
pub fn parse_catalog(provider_id: &str, value: &Value) -> Result<ParsedCatalog, AiError> {
    // 第一阶段：把三种顶层形状归一成条目引用列表，不做任何校验。
    let entries: Vec<&Value> = match value {
        Value::Array(entries) => entries.iter().collect(),
        Value::Object(object) => match object.get("models") {
            Some(Value::Array(models)) => models.iter().collect(),
            _ => object.values().collect(),
        },
        _ => {
            return Err(AiError::new(
                AiErrorKind::Other,
                format!("Invalid model catalog for provider \"{provider_id}\""),
            ));
        }
    };
    // 第二阶段：过滤。`contains_key` 对应 TS 的 `"id" in entry`（值本身不校验）；
    // `as_object` 同时排除了 `null` 与数组。
    let accepted: Vec<&Map<String, Value>> = entries
        .into_iter()
        .filter_map(Value::as_object)
        .filter(|entry| entry.contains_key("id"))
        .filter(|entry| is_supported_model_type(entry))
        .collect();
    // 第三阶段：同一 accepted 列表按同一顺序产出两份表示，保证下标对应。
    // `provider` 在 clone 上覆盖，原条目（借用的 JSON）不被修改。
    let raw_models = accepted
        .iter()
        .map(|entry| {
            let mut object = (*entry).clone();
            object.insert(
                "provider".to_string(),
                Value::String(provider_id.to_string()),
            );
            Value::Object(object)
        })
        .collect();
    let models = accepted
        .iter()
        .map(|entry| parse_catalog_entry(provider_id, entry))
        .collect();
    Ok(ParsedCatalog { models, raw_models })
}

/// `Date.parse` 语义的日期解析（V8 的 ES5 ISO 解析器 + legacy 解析器子集）。
///
/// V8 先尝试 ES5 / ISO-8601 形式（`YYYY[-MM[-DD]]`、
/// `YYYY-MM-DDTHH:mm[:ss[.sss]][Z|±HH:MM|±HHMM]`），失败后回退到 legacy 解析器
/// （RFC 2822、asctime、美式数字日期、月名日期、am/pm、时区缩写）。
///
/// 关键语义：
/// - ISO 纯日期/年月按 UTC；带 `T` 的 ISO 日期时间无时区时按本地时间，有时区按对应偏移。
///   `2015-10-21T07:28:00 UTC` 在 V8 里是 `NaN`（`T` 形式由 ISO 解析器独占）。
/// - legacy 形式无时区按本地时间；`GMT`/`UT`/`UTC`/`Z`/美国时区缩写/数字偏移按对应时区。
/// - legacy 缺失年份取 V8 的 2001；年份数值 0-49 → 20xx、50-99 → 19xx；缺失日取 1。
/// - 月 1..=12、日 1..=31 校验后允许日期溢出（`2015-02-30` → 3 月 2 日）；
///   时 0..=24（24 仅限 0 分 0 秒），分/秒 0..=59；小数秒截断到毫秒。
///
/// 解析失败返回 `None`；[`read_catalog`] 会把它记为 `0`。后果：设置 `local_generated_at`
/// 时目录不会被采用（主 provider 仍会持久化条目，见 `crate::models::remote_catalog`）。
/// 实现覆盖 V8 的常见形式（仍是 legacy 解析器子集）。
fn parse_http_date(value: &str) -> Option<i64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    // 两级解析器的分派与 V8 一致：形状像 ISO 日期时间就**只**走 ISO 解析器，即使解析
    // 失败也不回退 legacy（所以 `2015-1-1T00:00:00` 是 None，而 `2015-1-1` 是本地时间）。
    // 其余形式先试 ISO 纯日期，失败再交给 legacy 解析器。
    if is_iso_datetime_form(value) {
        return parse_iso_datetime(value);
    }
    parse_iso_date_only(value).or_else(|| parse_legacy_datetime(value))
}

/// 读取恰好 `count` 个 ASCII 数字。
fn take_digits<'a>(bytes: &'a [u8], index: &mut usize, count: usize) -> Option<&'a [u8]> {
    let end = index.checked_add(count)?;
    let slice = bytes.get(*index..end)?;
    if !slice.iter().all(u8::is_ascii_digit) {
        return None;
    }
    *index = end;
    Some(slice)
}

/// 读取 1-2 个 ASCII 数字（legacy 日期允许非零填充）。
fn take_short_digits(bytes: &[u8], index: &mut usize) -> Option<()> {
    let start = *index;
    while bytes.get(*index).is_some_and(u8::is_ascii_digit) && *index - start < 2 {
        *index += 1;
    }
    (*index > start).then_some(())
}

fn expect_byte(bytes: &[u8], index: &mut usize, expected: u8) -> Option<()> {
    if bytes.get(*index) != Some(&expected) {
        return None;
    }
    *index += 1;
    Some(())
}

fn digits_value(digits: &[u8]) -> Option<u32> {
    std::str::from_utf8(digits).ok()?.parse().ok()
}

/// 小数秒 → 毫秒（V8 截断到 3 位）。
fn fraction_millis(digits: &[u8]) -> u32 {
    let mut millis = 0;
    for (index, digit) in digits.iter().take(3).enumerate() {
        let weight = match index {
            0 => 100,
            1 => 10,
            _ => 1,
        };
        millis += u32::from(*digit - b'0') * weight;
    }
    millis
}

/// 是否可能是带 `T` 的 ISO 日期时间（月份/日允许 1-2 位，真实解析仍要求 2 位，
/// 因此 `2015-1-1T...` 与 V8 一样是 `NaN`）。
fn is_iso_datetime_form(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    if take_digits(bytes, &mut index, 4).is_none() || expect_byte(bytes, &mut index, b'-').is_none()
    {
        return false;
    }
    if take_short_digits(bytes, &mut index).is_none()
        || expect_byte(bytes, &mut index, b'-').is_none()
        || take_short_digits(bytes, &mut index).is_none()
    {
        return false;
    }
    matches!(bytes.get(index), Some(&b'T') | Some(&b't'))
}

/// `YYYY-MM-DDTHH:mm[:ss[.sss]][Z|±HH:MM|±HHMM]`：无时区 → 本地，有时区 → 对应偏移。
///
/// 调用方的形状探测允许 1-2 位的月/日，这里却严格要求 2 位，因此 `2015-1-1T...` 进入本
/// 函数后失败并直接返回 `None`（不会回退 legacy），与 V8 的 `NaN` 一致。
fn parse_iso_datetime(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    let mut index = 0;
    // 日期部分：YYYY-MM-DD，全部定长。
    let year = i32::try_from(digits_value(take_digits(bytes, &mut index, 4)?)?).ok()?;
    expect_byte(bytes, &mut index, b'-')?;
    let month = digits_value(take_digits(bytes, &mut index, 2)?)?;
    expect_byte(bytes, &mut index, b'-')?;
    let day = digits_value(take_digits(bytes, &mut index, 2)?)?;
    if !matches!(bytes.get(index), Some(&b'T') | Some(&b't')) {
        return None;
    }
    index += 1;
    let hour = digits_value(take_digits(bytes, &mut index, 2)?)?;
    expect_byte(bytes, &mut index, b':')?;
    let minute = digits_value(take_digits(bytes, &mut index, 2)?)?;
    let mut second = 0;
    let mut millis = 0;
    if bytes.get(index) == Some(&b':') {
        index += 1;
        second = digits_value(take_digits(bytes, &mut index, 2)?)?;
        if bytes.get(index) == Some(&b'.') {
            index += 1;
            let start = index;
            while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                index += 1;
            }
            if index == start {
                return None;
            }
            millis = fraction_millis(&bytes[start..index]);
        }
    }
    // 时区缺失（走 `_` 分支得到 `None`）不是错误：这里只记录“没有偏移”，由函数末尾的
    // `index != bytes.len()` 检查兜住所有未消费的字符；无偏移时最终按本地时间解释。
    let offset_minutes = match bytes.get(index) {
        Some(&b'Z') | Some(&b'z') => {
            index += 1;
            Some(0)
        }
        Some(&sign) if sign == b'+' || sign == b'-' => {
            index += 1;
            let hours = digits_value(take_digits(bytes, &mut index, 2)?)?;
            // V8 同时接受 `±HH:MM` 与不带冒号的 `±HHMM`，但拒绝只有小时的 `±HH`。
            let minutes = if bytes.get(index) == Some(&b':') {
                index += 1;
                digits_value(take_digits(bytes, &mut index, 2)?)?
            } else {
                digits_value(take_digits(bytes, &mut index, 2)?)?
            };
            if hours > 23 || minutes > 59 {
                return None;
            }
            let offset = i32::try_from(hours * 60 + minutes).ok()?;
            Some(if sign == b'+' { offset } else { -offset })
        }
        _ => None,
    };
    if index != bytes.len() {
        return None;
    }
    // 有偏移：先当成 UTC 求时间戳，再减去偏移分钟数（例如 +08:00 要减 480 分钟）。
    // 无偏移：交给本地时区换算，DST 的歧义与空档由 `local_timestamp_millis` 处理。
    let datetime = build_datetime(year, month, day, hour, minute, second, millis)?;
    Some(match offset_minutes {
        Some(offset_minutes) => {
            datetime.and_utc().timestamp_millis() - i64::from(offset_minutes) * 60_000
        }
        None => local_timestamp_millis(datetime)?,
    })
}

/// ISO 纯日期/年月：`YYYY`、`YYYY-MM`、`YYYY-MM-DD`，均按 UTC（V8 语义）。
///
/// 月份/日必须是 2 位数字，否则交给 legacy 解析器（`2015-1-1` 在 V8 里是本地时间）。
fn parse_iso_date_only(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    let mut index = 0;
    let year = i32::try_from(digits_value(take_digits(bytes, &mut index, 4)?)?).ok()?;
    if index == bytes.len() {
        return utc_midnight(year, 1, 1);
    }
    expect_byte(bytes, &mut index, b'-')?;
    let month = digits_value(take_digits(bytes, &mut index, 2)?)?;
    if index == bytes.len() {
        return utc_midnight(year, month, 1);
    }
    expect_byte(bytes, &mut index, b'-')?;
    let day = digits_value(take_digits(bytes, &mut index, 2)?)?;
    if index != bytes.len() {
        return None;
    }
    utc_midnight(year, month, day)
}

fn utc_midnight(year: i32, month: u32, day: u32) -> Option<i64> {
    build_datetime(year, month, day, 0, 0, 0, 0)
        .map(|datetime| datetime.and_utc().timestamp_millis())
}

/// 构造可能溢出的日期时间：日 1..=31、月 1..=12，超出的日顺延到下个月
/// （V8 的 `MakeDate` 语义，例如 `2015-02-30` → 3 月 2 日）；`24:00:00` → 次日零点。
fn build_datetime(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    millis: u32,
) -> Option<chrono::NaiveDateTime> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if minute > 59 || second > 59 {
        return None;
    }
    if hour > 24 || (hour == 24 && (minute != 0 || second != 0 || millis != 0)) {
        return None;
    }
    // 先只校验到「当月 1 日」，再把 `day - 1` 天加上去：这样 2 月 30 日会自然顺延到 3 月，
    // 复现 V8 `MakeDate` 的溢出语义，而不是像严格日期解析那样报错。
    let first = chrono::NaiveDate::from_ymd_opt(year, month, 1)?;
    let date = first.checked_add_days(chrono::Days::new(u64::from(day - 1)))?;
    let datetime = date.and_hms_milli_opt(hour % 24, minute, second, millis)?;
    // 24:00 已由上面的条件限制为 0 分 0 秒 0 毫秒，等价于次日零点。
    if hour == 24 {
        datetime.checked_add_days(chrono::Days::new(1))
    } else {
        Some(datetime)
    }
}

/// legacy 解析器识别出的日期分量：纯数字，或已经换算过的月名。
enum LegacyPart {
    /// 数字的原始文本，含义（年/月/日）由它在序列中的位置决定。
    Number(String),
    /// 月名换算出的 1..=12。
    Month(u32),
}

/// 去掉 V8 legacy 解析器忽略的括号注释。
///
/// 支持嵌套：左括号之后的所有字符（含内层括号）都丢弃；不配对的 `)` 原样保留。
fn strip_comments(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut depth = 0usize;
    for character in value.chars() {
        match character {
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            _ if depth > 0 => {}
            _ => result.push(character),
        }
    }
    result
}

/// legacy token：空白/逗号分隔。
fn legacy_tokens(value: &str) -> Vec<String> {
    strip_comments(value)
        .split(|character: char| character.is_whitespace() || character == ',')
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

/// 星期名（大小写不敏感，允许缩写与结尾的点，如 `Mon.`）。
fn is_weekday_name(token: &str) -> bool {
    matches!(
        token.trim_end_matches('.').to_ascii_lowercase().as_str(),
        "mon"
            | "monday"
            | "tue"
            | "tues"
            | "tuesday"
            | "wed"
            | "weds"
            | "wednesday"
            | "thu"
            | "thur"
            | "thurs"
            | "thursday"
            | "fri"
            | "friday"
            | "sat"
            | "saturday"
            | "sun"
            | "sunday"
    )
}

/// 英文月名 → 1..=12（大小写不敏感，允许缩写与结尾的点）。
fn month_number(token: &str) -> Option<u32> {
    match token.trim_end_matches('.').to_ascii_lowercase().as_str() {
        "jan" | "january" => Some(1),
        "feb" | "february" => Some(2),
        "mar" | "march" => Some(3),
        "apr" | "april" => Some(4),
        "may" => Some(5),
        "jun" | "june" => Some(6),
        "jul" | "july" => Some(7),
        "aug" | "august" => Some(8),
        "sep" | "sept" | "september" => Some(9),
        "oct" | "october" => Some(10),
        "nov" | "november" => Some(11),
        "dec" | "december" => Some(12),
        _ => None,
    }
}

/// 时区 token → 相对 UTC 的分钟偏移。
///
/// 认三类写法：`Z`/`UT`/`UTC`/`GMT`；RFC 2822 的美国时区缩写（`EST`/`EDT`/...，大小写
/// 不敏感）；数字偏移（可带 `GMT`/`UTC` 前缀，如 `GMT+0800`）。无法识别的 token 返回
/// `None`，调用方会把它留在列表里；像 `BST` 这样的未知缩写随后会在月名查找处失败，
/// 使整次解析返回 `None`（V8 同样是 `NaN`）。
fn parse_zone_token(token: &str) -> Option<i32> {
    match token.to_ascii_lowercase().as_str() {
        "z" | "ut" | "utc" | "gmt" => return Some(0),
        "edt" => return Some(-4 * 60),
        "est" | "cdt" => return Some(-5 * 60),
        "cst" | "mdt" => return Some(-6 * 60),
        "mst" | "pdt" => return Some(-7 * 60),
        "pst" => return Some(-8 * 60),
        _ => {}
    }
    let lower = token.to_ascii_lowercase();
    if let Some(rest) = lower
        .strip_prefix("gmt")
        .or_else(|| lower.strip_prefix("utc"))
        && (rest.starts_with('+') || rest.starts_with('-'))
    {
        return parse_numeric_offset(rest);
    }
    parse_numeric_offset(token)
}

/// 数字时区偏移：`+HH:MM`、`+HHMM`、`+HH`（V8 也接受 `+008:00` 这种多位小时）。
///
/// 无冒号时按数字串长度解释：1-2 位是小时（`+8` → UTC+8），3 位是 `HMM`，4 位是
/// `HHMM`，更长则拒绝。带冒号时小时允许 1-3 位。只有带冒号形式最后校验小时 ≤ 23、
/// 分钟 ≤ 59；无冒号形式不做上界校验（`+99`、`+2399` 都会被接受，V8 同样如此）。
fn parse_numeric_offset(token: &str) -> Option<i32> {
    let sign = match token.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let body = &token[1..];
    if body.is_empty() {
        return None;
    }
    let (hours, minutes) = match body.split_once(':') {
        Some((hours, minutes)) => (hours, minutes),
        None => {
            if !body.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            return match body.len() {
                1 | 2 => Some(sign * body.parse::<i32>().ok()? * 60),
                3 => Some(
                    sign * (body[..1].parse::<i32>().ok()? * 60 + body[1..].parse::<i32>().ok()?),
                ),
                4 => Some(
                    sign * (body[..2].parse::<i32>().ok()? * 60 + body[2..].parse::<i32>().ok()?),
                ),
                _ => None,
            };
        }
    };
    if hours.is_empty()
        || hours.len() > 3
        || minutes.is_empty()
        || minutes.len() > 2
        || !hours.bytes().all(|byte| byte.is_ascii_digit())
        || !minutes.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let hours: i32 = hours.parse().ok()?;
    let minutes: i32 = minutes.parse().ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 60 + minutes))
}

/// legacy 时间 token：`H:mm[:ss[.sss]]`；返回 `(时, 分, 秒, 毫秒)`。
fn parse_legacy_time(token: &str) -> Option<(u32, u32, u32, u32)> {
    let mut pieces = token.split(':');
    let hour = parse_time_piece(pieces.next()?)?;
    let minute = parse_time_piece(pieces.next()?)?;
    let mut second = 0;
    let mut millis = 0;
    if let Some(piece) = pieces.next() {
        let (seconds, fraction) = match piece.split_once('.') {
            Some((seconds, fraction)) => (seconds, Some(fraction)),
            None => (piece, None),
        };
        second = parse_time_piece(seconds)?;
        if let Some(fraction) = fraction {
            if fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            millis = fraction_millis(fraction.as_bytes());
        }
    }
    if pieces.next().is_some() {
        return None;
    }
    Some((hour, minute, second, millis))
}

/// 1-2 位数字的时间分量。
fn parse_time_piece(piece: &str) -> Option<u32> {
    if piece.is_empty() || piece.len() > 2 || !piece.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    piece.parse().ok()
}

/// legacy 年份转换（V8）：数值 0-49 → 20xx、50-99 → 19xx，其它原样。
fn legacy_year(digits: &str) -> Option<i32> {
    let value: i32 = digits.parse().ok()?;
    Some(if value < 50 {
        value + 2000
    } else if value < 100 {
        value + 1900
    } else {
        value
    })
}

/// legacy 数字日期的首分量何时是年份：`0`（V8 把 `0/12` 当作 2000 年 12 月）或大于 31。
fn is_year_component(value: u32) -> bool {
    value == 0 || value > 31
}

/// legacy 解析器：RFC 2822 / asctime / 美式数字日期 / 月名日期 / am/pm。
///
/// 处理顺序（任何一步失败都返回 `None`，即 V8 的 `NaN`）：
/// 1. 去掉括号注释，按空白/逗号切 token，开头的星期名丢弃；
/// 2. 扫描并摘掉所有时区 token，同时处理粘在数值末尾的 `Z`；
/// 3. 找到第一个时间 token（`H:mm[:ss[.sss]]`）并摘掉，紧邻的 `am`/`pm` 一并消费；
/// 4. 剩余 token 按 `/`、`-`、`.` 拆成数字与月名分量；
/// 5. 月名最多一个；有月名时数字按 `日[/年]` 解释，否则按分量个数套美式规则；
/// 6. 组装日期时间，最后按显式偏移或本地时区换算。
///
/// 缺省值：无年份 → 2001，无日 → 1，无时间 → 00:00:00。时间 token 必须带冒号，
/// 因此纯数字 token 不会被误认成时间。
fn parse_legacy_datetime(value: &str) -> Option<i64> {
    let mut tokens = legacy_tokens(value);
    // 第 1 步：开头的星期名不参与日期计算，直接丢弃。
    if tokens.first().is_some_and(|token| is_weekday_name(token)) {
        tokens.remove(0);
    }
    if tokens.is_empty() {
        return None;
    }

    // 第 2 步：时区可以出现在任意位置，先全部摘掉再解析日期/时间，避免 `EST` 之类的
    // token 被当成月名。注意这里没有 break：循环结束后每个 token 都已检查过。
    let mut offset_minutes: Option<i32> = None;
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index].clone();
        if let Some(offset) = parse_zone_token(&token) {
            offset_minutes = Some(offset);
            tokens.remove(index);
            continue;
        }
        // `07:28:00Z` / `2015-10-21Z`：粘在数值 token 末尾的 `Z` 视为 UTC。
        if token.len() > 1
            && (token.ends_with('Z') || token.ends_with('z'))
            && token[..token.len() - 1]
                .bytes()
                .any(|byte| byte.is_ascii_digit())
        {
            offset_minutes = Some(0);
            tokens[index] = token[..token.len() - 1].to_string();
        }
        index += 1;
    }

    // 第 3 步：找到第一个能解析成 `H:mm[:ss[.sss]]` 的 token，把它从列表里摘掉。
    // 找到后立即 break：后面若还有第二个时间 token，它会留在日期分量里按“非数字单词”
    // 处理（月名查找失败），通常使整次解析返回 `None`。
    let (mut hour, mut minute, mut second, mut millis) = (0, 0, 0, 0);
    let mut am_pm: Option<bool> = None;
    let mut index = 0;
    while index < tokens.len() {
        if let Some((token_hour, token_minute, token_second, token_millis)) =
            parse_legacy_time(&tokens[index])
        {
            (hour, minute, second, millis) = (token_hour, token_minute, token_second, token_millis);
            tokens.remove(index);
            // `am`/`pm` 只有在时间 token 紧邻之后才被消费（允许 `pm.` 这种带结尾点的写法）。
            if index < tokens.len() {
                let suffix = tokens[index].trim_end_matches('.').to_ascii_lowercase();
                if suffix == "am" || suffix == "pm" {
                    am_pm = Some(suffix == "pm");
                    tokens.remove(index);
                }
            }
            break;
        }
        index += 1;
    }
    // 12 小时制换算：pm 且小时 < 12 加 12；am 的 12 点改成 0。换算后越界即非法。
    if let Some(pm) = am_pm {
        if pm {
            if hour < 12 {
                hour += 12;
            }
        } else if hour == 12 {
            hour = 0;
        }
        if hour > 23 {
            return None;
        }
    }

    // 按日期分隔符拆分（`2015-10-21`、`10/21/2015`、`06-Nov-94`、`2015.10` 等）。
    let mut parts: Vec<LegacyPart> = Vec::new();
    for token in &tokens {
        for piece in token.split(['/', '-', '.']) {
            if piece.is_empty() {
                continue;
            }
            if piece.bytes().all(|byte| byte.is_ascii_digit()) {
                parts.push(LegacyPart::Number(piece.to_string()));
            } else {
                // 未知单词（例如 `BST`、`extra`）在 V8 里同样是 NaN。
                parts.push(LegacyPart::Month(month_number(piece)?));
            }
        }
    }

    let months: Vec<u32> = parts
        .iter()
        .filter_map(|part| match part {
            LegacyPart::Month(month) => Some(*month),
            LegacyPart::Number(_) => None,
        })
        .collect();
    let numbers: Vec<&str> = parts
        .iter()
        .filter_map(|part| match part {
            LegacyPart::Number(number) => Some(number.as_str()),
            LegacyPart::Month(_) => None,
        })
        .collect();
    if months.len() > 1 {
        return None;
    }

    // 第 5 步：确定年/月/日。有月名时，数字按 `日[/年]` 解释；首分量 > 31 时改按
    // `年/日`（如 `Oct 2015 21` → 2015-10-21）；没有月名时按美式
    // `月/日[/年]` 解释——除非首分量“像年份”（`0` 或 `> 31`），此时改按 `年/月[/日]`。
    let (year, month, day) = if let Some(&month) = months.first() {
        let (day, year) = match numbers.as_slice() {
            [] => (1, 2001),
            [only] => {
                let value = digits_value(only.as_bytes())?;
                if value <= 31 {
                    (value, 2001)
                } else {
                    (1, legacy_year(only)?)
                }
            }
            [first, second] => {
                let first_value = digits_value(first.as_bytes())?;
                if first_value <= 31 {
                    (first_value, legacy_year(second)?)
                } else {
                    (digits_value(second.as_bytes())?, legacy_year(first)?)
                }
            }
            _ => return None,
        };
        (year, month, day)
    } else {
        match numbers.as_slice() {
            // 两个分量：首分量是年份（`0` 或 `> 31`，如 `2015/10`、`00/12`）→ 年/月；
            // 否则美式 月/日，年份取 2001。
            [first, second] if is_year_component(digits_value(first.as_bytes())?) => {
                (legacy_year(first)?, digits_value(second.as_bytes())?, 1)
            }
            [first, second] => (
                2001,
                digits_value(first.as_bytes())?,
                digits_value(second.as_bytes())?,
            ),
            // 三个分量：首分量是年份 → 年/月/日（`2015/10/21`、`0/1/2`），否则美式 月/日/年。
            [first, second, third] if is_year_component(digits_value(first.as_bytes())?) => (
                legacy_year(first)?,
                digits_value(second.as_bytes())?,
                digits_value(third.as_bytes())?,
            ),
            [first, second, third] => (
                legacy_year(third)?,
                digits_value(first.as_bytes())?,
                digits_value(second.as_bytes())?,
            ),
            _ => return None,
        }
    };

    // 第 6 步：校验/顺延日期时间（见 `build_datetime`），最后套显式偏移或本地时区。
    let datetime = build_datetime(year, month, day, hour, minute, second, millis)?;
    match offset_minutes {
        Some(offset_minutes) => {
            Some(datetime.and_utc().timestamp_millis() - i64::from(offset_minutes) * 60_000)
        }
        None => local_timestamp_millis(datetime),
    }
}

/// 本地时间 → epoch 毫秒（DST 歧义取较早的偏移，与 V8 的 `MakeDate` 一致）。
///
/// `LocalResult::None`（本地时钟跳过的空档，例如夏令时开始的那一小时）返回 `None`，
/// 即视为非法日期；`Ambiguous`（回拨时重复的一小时）取第一个（较早的）偏移。
fn local_timestamp_millis(naive: chrono::NaiveDateTime) -> Option<i64> {
    use chrono::TimeZone;
    match chrono::Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(value) => Some(value.timestamp_millis()),
        chrono::LocalResult::Ambiguous(first, _) => Some(first.timestamp_millis()),
        chrono::LocalResult::None => None,
    }
}

/// `readCatalog(providerId, response)`。
///
/// 消费响应体，返回 [`CatalogResponse`]。
///
/// # 参数与前置条件
/// - `provider_id`：既用于错误文案，也用于覆盖每个条目的 `provider` 字段；
/// - `response`：调用方**已经确认成功**的响应。本函数不检查状态码：304/404/501 与其它
///   非 2xx 都由调用方在传入前分流；调用后 body 已被读走，无法再读取。
///
/// # 返回
/// 先读头：`Last-Modified` 缺失、非法或为空时 `last_modified = 0`；`ETag` 逐字保留
/// （含引号），缺失为 `None`。再解析 body。
///
/// # 错误
/// - 读 body 失败 → `AiErrorKind::Other`，message 固定 `terminated`（reqwest 的错误
///   细节被丢弃）；
/// - body 不是合法 JSON → `AiErrorKind::Other`，message 为 V8 `JSON.parse` 的文案
///   （由 `kiana_json::js_parse` 提供）；
/// - [`parse_catalog`] 的错误原样传播（只有顶层形状非法时才会出现）。
///
/// 条目级问题不在这里报错，只由 [`parse_catalog`] 过滤。
#[allow(clippy::result_large_err)]
pub async fn read_catalog(
    provider_id: &str,
    response: Response,
) -> Result<CatalogResponse, AiError> {
    // 头必须在读 body 之前取：`response.text()` 会消费整个 Response。
    let last_modified = response
        .headers()
        .get("last-modified")
        .and_then(|value| value.to_str().ok())
        .and_then(parse_http_date)
        .unwrap_or(0);
    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    // 读取阶段的失败统一映射成固定文本 `terminated`，不保留 reqwest 的细节；调用方只能
    // 看到这一条消息。
    let text = response
        .text()
        .await
        .map_err(|_| AiError::new(AiErrorKind::Other, "terminated"))?;
    // 用 V8 兼容的 `JSON.parse`（错误消息与位置也逐字对齐）而不是 serde_json。
    let value = kiana_json::js_parse::parse(&text)
        .map_err(|error| AiError::new(AiErrorKind::Other, error.message))?;
    let catalog = parse_catalog(provider_id, &value)?;
    Ok(CatalogResponse {
        models: catalog.models,
        raw_models: catalog.raw_models,
        last_modified,
        etag,
    })
}

/// `requestCatalog(providerId, catalogBaseUrl, signal, validator?)`。
///
/// 发起 GET 并返回**未检查状态码**的响应；重试、单次超时与取消都交给
/// `kiana_core::management_http::fetch_with_retry`。
///
/// # 参数
/// - `catalog_base_url`：catalog 的 base URL，请求路径与查询串由 `catalog_url` 拼接；
/// - `signal`：父取消令牌（取自 `AbortScope::token`）。`fetch_with_retry` 在每次尝试
///   开始前以及等待响应期间检查它，取消以 `FetchError::Aborted` 结束；
/// - `validator`：`Some("")` 与 `None` 等效——TS 里空字符串是假值，不会发送
///   `if-none-match`（测试 `empty_etag_is_not_forwarded` 锁定该行为）。
///
/// # 重试与超时
/// 其余选项取 `FetchRetryOptions::default()`：首次 + 最多 2 次重试、可重试状态码也计入
/// 重试额度、没有总预算；每次尝试最多 [`CATALOG_ATTEMPT_TIMEOUT_MS`] 毫秒。重试额度
/// 耗尽后，可重试状态码的响应仍作为 `Ok` 返回，由调用方按状态码处理。
///
/// # 错误
/// `fetch_with_retry` 的任何错误都转成 `AiErrorKind::Other`，message 为其 `Display`
/// 文案：超时是 `The operation was aborted due to timeout`，取消是
/// `This operation was aborted`，传输失败固定为 `fetch failed`（底层 reqwest 错误
/// 只存在于 `FetchError::Transport::cause`，不会出现在 AiError.message）。取消同样返回
/// `Err`，调用方需要自行判断是否静默结束。
#[allow(clippy::result_large_err)]
pub async fn request_catalog(
    provider_id: &str,
    catalog_base_url: &str,
    signal: &AbortScope,
    validator: Option<&str>,
) -> Result<Response, AiError> {
    let url = catalog_url(provider_id, catalog_base_url)?;
    let user_agent = kiana_core::changelog::get_kiana_user_agent(VERSION);
    let client = shared_http_client();
    let validator = validator.map(str::to_string);
    let signal_token = signal.token();
    // `fetch_with_retry` 每次尝试前都会重新调用 `build`：`RequestBuilder` 是一次性的，
    // 请求头与 URL 必须在这里重建，不能复用第一次的 builder。
    let build = move || {
        let mut request = client
            .get(url.clone())
            .header("accept", "application/json")
            .header("User-Agent", user_agent.clone());
        // TS: `...(validator ? { "if-none-match": validator } : {})` —— 空字符串为假值，
        // 因此空 ETag（`etag: ""`）不会发送 `if-none-match`。
        if let Some(validator) = &validator
            && !validator.is_empty()
        {
            request = request.header("if-none-match", validator);
        }
        request
    };
    fetch_with_retry(
        build,
        FetchRetryOptions {
            attempt_timeout_ms: Some(CATALOG_ATTEMPT_TIMEOUT_MS),
            signal: Some(signal_token),
            ..FetchRetryOptions::default()
        },
    )
    .await
    .map_err(|error| AiError::new(AiErrorKind::Other, error.to_string()))
}

/// `getModelType` 的字符串形式（供 `mergeModels` 的 `Map` 键使用）。
///
/// 返回值直接参与 `remote_catalog::merge_models` 的键 `"{type}\0{id}"`；取值必须是固定的
/// `chat`/`image`/`classifier` 之一，改字面量会改变合并键；[`AnyModel::Remote`] 缺少 `type`
/// 时按 chat 处理（即 [`AnyModel::model_type`] 的语义）。
pub(crate) fn model_type_key(model: &AnyModel) -> &'static str {
    match get_model_type(model) {
        ModelType::Chat => "chat",
        ModelType::Image => "image",
        ModelType::Classifier => "classifier",
    }
}

// 期望值来源：绝对时间用例取自 V8（node 26）`Date.parse` 的实测输出（见各测试的文档
// 注释）；本地时间用例用 `chrono::Local` 现场计算期望值，因此不依赖测试机器的时区。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_uri_components_like_javascript() {
        assert_eq!(encode_uri_component("test-provider"), "test-provider");
        assert_eq!(encode_uri_component("chat,image"), "chat%2Cimage");
        assert_eq!(encode_uri_component("a b/c?d#e"), "a%20b%2Fc%3Fd%23e");
        assert_eq!(encode_uri_component("-_.!~*'()"), "-_.!~*'()");
        assert_eq!(encode_uri_component("ü"), "%C3%BC");
    }

    #[test]
    fn builds_the_catalog_url() {
        let url = catalog_url("test-provider", "https://kiana.dev").unwrap();
        assert_eq!(
            url.as_str(),
            "https://kiana.dev/api/models/providers/test-provider?types=chat%2Cimage%2Cclassifier"
        );
    }

    #[test]

    fn parses_http_dates() {
        assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(parse_http_date("not a date"), None);
        assert_eq!(parse_http_date(""), None);
    }

    /// 期望值来自 V8（node 26）`Date.parse` 的实测输出，时区无关的形式。
    #[test]
    fn parses_v8_absolute_date_forms() {
        assert_eq!(
            parse_http_date("Wed, 21 Oct 2015 07:28:00 GMT"),
            Some(1_445_412_480_000)
        );
        // RFC 2822 的 `UTC` / `UT` 后缀（chrono 的 RFC 2822 解析器不接受）。
        assert_eq!(
            parse_http_date("Wed, 21 Oct 2015 07:28:00 UTC"),
            Some(1_445_412_480_000)
        );
        assert_eq!(
            parse_http_date("Wed, 21 Oct 2015 07:28:00 Utc"),
            Some(1_445_412_480_000)
        );
        assert_eq!(
            parse_http_date("Wed, 21 Oct 2015 07:28:00 UT"),
            Some(1_445_412_480_000)
        );
        // 带冒号的数字偏移。
        assert_eq!(
            parse_http_date("Wed, 21 Oct 2015 07:28:00 +00:00"),
            Some(1_445_412_480_000)
        );
        assert_eq!(
            parse_http_date("Wed, 21 Oct 2015 07:28:00 -07:00"),
            Some(1_445_437_680_000)
        );
        assert_eq!(
            parse_http_date("Wed, 21 Oct 2015 07:28:00 +0800"),
            Some(1_445_383_680_000)
        );
        assert_eq!(
            parse_http_date("Wed, 21 Oct 2015 07:28:00 GMT+0800"),
            Some(1_445_383_680_000)
        );
        // RFC 2822 的美国时区缩写（大小写不敏感）。
        assert_eq!(
            parse_http_date("Wed, 21 Oct 2015 07:28:00 EST"),
            Some(1_445_430_480_000)
        );
        assert_eq!(
            parse_http_date("wed, 21 oct 2015 07:28:00 edt"),
            Some(1_445_426_880_000)
        );
        // ISO 日期时间：`Z` 与数字偏移；无时区按本地时间。
        assert_eq!(
            parse_http_date("2015-10-21T07:28:00Z"),
            Some(1_445_412_480_000)
        );
        assert_eq!(
            parse_http_date("2015-10-21T07:28:00+00:00"),
            Some(1_445_412_480_000)
        );
        assert_eq!(
            parse_http_date("2015-10-21t07:28:00z"),
            Some(1_445_412_480_000)
        );
        assert_eq!(
            parse_http_date("2015-10-21T07:28:00.123Z"),
            Some(1_445_412_480_123)
        );
        assert_eq!(
            parse_http_date("2015-10-21T07:28:00+0800"),
            Some(1_445_383_680_000)
        );
        assert_eq!(
            parse_http_date("2015-10-21T07:28:00.123456789Z"),
            Some(1_445_412_480_123)
        );
        // ISO 纯日期/年月按 UTC；asctime 按本地时间（下面另有本地时间测试）。
        assert_eq!(parse_http_date("2015-10-21"), Some(1_445_385_600_000));
        assert_eq!(parse_http_date("2015-10"), Some(1_443_657_600_000));
        assert_eq!(parse_http_date("2015"), Some(1_420_070_400_000));
        assert_eq!(
            parse_http_date("2015-10-21 07:28:00 UTC"),
            Some(1_445_412_480_000)
        );
        assert_eq!(parse_http_date("2015-10-21Z"), Some(1_445_385_600_000));
        // V8 视为 NaN 的形式。
        assert_eq!(parse_http_date("2015-10-21T07:28:00 UTC"), None);
        assert_eq!(parse_http_date("2015-10-21T07:28:00 GMT"), None);
        assert_eq!(parse_http_date("Wed, 21 Oct 2015 07:28:00 BST"), None);
        assert_eq!(parse_http_date("2015-13-01"), None);
    }

    /// 无时区的形式按本地时间解释（期望值用 `chrono::Local` 计算，与机器时区无关）。
    #[test]
    fn parses_v8_local_date_forms() {
        use chrono::TimeZone;

        let local = |year, month, day, hour, minute, second| {
            chrono::Local
                .with_ymd_and_hms(year, month, day, hour, minute, second)
                .single()
                .expect("unambiguous local time")
                .timestamp_millis()
        };

        assert_eq!(
            parse_http_date("Wed, 21 Oct 2015 07:28:00"),
            Some(local(2015, 10, 21, 7, 28, 0))
        );
        assert_eq!(
            parse_http_date("21 Oct 2015 07:28:00"),
            Some(local(2015, 10, 21, 7, 28, 0))
        );
        assert_eq!(
            parse_http_date("Sun Nov  6 08:49:37 1994"),
            Some(local(1994, 11, 6, 8, 49, 37))
        );
        assert_eq!(
            parse_http_date("Sunday, 06-Nov-94 08:49:37 GMT"),
            Some(784_111_777_000)
        );
        // 美式数字日期（月/日/年）。
        assert_eq!(
            parse_http_date("10/21/2015"),
            Some(local(2015, 10, 21, 0, 0, 0))
        );
        assert_eq!(
            parse_http_date("10-21-2015 07:28:00"),
            Some(local(2015, 10, 21, 7, 28, 0))
        );
        assert_eq!(
            parse_http_date("2015/10/21 07:28:00"),
            Some(local(2015, 10, 21, 7, 28, 0))
        );
        assert_eq!(
            parse_http_date("2015.10.21"),
            Some(local(2015, 10, 21, 0, 0, 0))
        );
        // am/pm。
        assert_eq!(
            parse_http_date("Oct 21, 2015 7:28 pm"),
            Some(local(2015, 10, 21, 19, 28, 0))
        );
        assert_eq!(
            parse_http_date("10/21/2015 12:30 am"),
            Some(local(2015, 10, 21, 0, 30, 0))
        );
        // 缺失年份 → 2001；缺失日 → 1；两位年份 0-49 → 20xx、50-99 → 19xx。
        assert_eq!(parse_http_date("10/21"), Some(local(2001, 10, 21, 0, 0, 0)));
        assert_eq!(
            parse_http_date("Oct 2015"),
            Some(local(2015, 10, 1, 0, 0, 0))
        );
        assert_eq!(
            parse_http_date("10/21/99"),
            Some(local(1999, 10, 21, 0, 0, 0))
        );
        assert_eq!(
            parse_http_date("10/21/49"),
            Some(local(2049, 10, 21, 0, 0, 0))
        );
        // 日允许溢出（V8 由 MakeDate 顺延）：2015-02-30 → 3 月 2 日。
        assert_eq!(
            parse_http_date("Feb 29, 2015"),
            Some(local(2015, 3, 1, 0, 0, 0))
        );
        // 24:00:00 → 次日零点。
        assert_eq!(
            parse_http_date("10/21/2015 24:00:00"),
            Some(local(2015, 10, 22, 0, 0, 0))
        );
        // 非法值。
        assert_eq!(parse_http_date("10/21/2015 24:01:00"), None);
        assert_eq!(parse_http_date("21/10/2015"), None);
        assert_eq!(parse_http_date("Oct 32, 2015"), None);
        assert_eq!(parse_http_date("10/21/2015 07:28:00."), None);
    }
}
