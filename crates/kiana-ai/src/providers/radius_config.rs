//! providers/radius-config.ts——Radius 网关目录与凭证配置。
//!
//! Radius 的模型目录由网关返回（`GET <gateway>/v1/config`），并缓存在 OAuth 凭证的
//! `gatewayConfig` 字段里（旧版实现遗留）。本模块按 TS 源逐函数移植：
//!
//! - [`normalize_radius_gateway_url`]：无 scheme 补 `https://`，去掉全部尾随 `/`；
//! - [`sanitize_radius_gateway_config`] / [`is_radius_gateway_model`]：校验网关返回的模型；
//! - [`get_radius_credential_config`] / [`get_radius_models`]：读取凭证里缓存的目录；
//! - [`get_radius_models_from_config`]：把网关模型转成 `api: "kiana-messages"` 的 [`Model`]；
//! - [`load_radius_gateway_config`]：拉取并校验 `/v1/config`。
//!
//! 数据结构：[`RadiusGatewayConfig`] 是 `{ baseUrl, models }`；每个模型是未类型化的
//! [`RadiusGatewayModel`]（原始 JSON 对象），只有转 [`Model`] 时才按固定结构清洗。
//!
//! 与 TS 的差异：
//! - TS 的模型对象用 `{...model, api, provider, baseUrl}` 保留全部字段；Rust 的 [`Model`]
//!   是固定结构体，**Model 支持的全部字段**（`headers`、`inputLimits`、`type`、`promptCache`、
//!   `samplingParams*`、`compat`、`output`…）都会保留，只有 Model 无法表达的未知键被丢弃。
//!   做法与 TS 的展开等价：把 `api`/`provider`/`baseUrl` 覆盖进 JSON 对象后按 [`Model`]
//!   反序列化；类型不符的可选字段会被移除而不是让整条模型消失。
//! - `input` 里的未知取值在 TS 里原样保留；Rust 的 [`ModelInputModality`] 只表达
//!   `"text"`/`"image"`，其余条目被跳过（模型本身仍保留）。
//! - `contextWindow`/`maxTokens` 在 TS 里是任意 number；[`Model`] 的字段是 `Option<u64>`，
//!   小数被截断、负数饱和为 0（无法在不改冻结类型的前提下表达）。
//!
//! # 在系统中的位置与执行流程
//!
//! 调用方是 [`crate::providers::radius`] 的 `refresh_models`：
//!
//! 1. 没有持久化目录且凭证是 OAuth 时，用 [`get_radius_models`] 读取凭证里遗留的
//!    `gatewayConfig` 缓存（旧版 Radius 实现的目录），作为动态目录导入；
//! 2. 允许联网时用 [`load_radius_gateway_config`] 请求 `<gateway>/v1/config`，Bearer
//!    token 由调用方从凭证解析（OAuth 用 `access`，API key 用 `key`）；
//! 3. 用 [`get_radius_models_from_config`] 把网关模型转成 [`Model`]，交给 `Models` 发布。
//!
//! 本模块不持有状态：`gatewayConfig` 只是历史缓存的读取入口，实时目录由
//! `providers/radius.rs` 的 `dynamic_models` 保存，并通过 `publish` 持久化到 `ModelsStore`。
//!
//! # 失败路径
//!
//! - `gateway` 不是合法 URL：`resolve_url` 返回 `TypeError: Invalid URL` 语义的 [`AuthError`]；
//! - 非 2xx：`AuthError`，消息含状态码与截断后的响应正文（行为见私有函数
//!   `truncate_http_body` 的说明）；
//! - 响应体不是合法 JSON：保留 V8 `SyntaxError` 的文案；
//! - JSON 形状不是 `{baseUrl: string, models: array}`：`Invalid Radius config from <gateway>`；
//! - 单个模型字段不全：被 [`sanitize_radius_gateway_config`] 过滤掉，其余模型仍然保留。
//!
//! # 注意事项
//!
//! - 本模块所有函数都不做网络层重试或缓存；重复调用 [`load_radius_gateway_config`] 会真实
//!   重复请求，限流/去重由调用方负责（`radius.rs` 只在刷新的联网阶段调用一次）。
//! - `api_key` 为空串时不发送 `Authorization` 头（TS 的 `if (apiKey)` 是 falsy 判断）。
//! - 错误类型是 [`AuthError`]；`radius.rs` 负责转换成 `AiError`，并把中止映射为
//!   `AiErrorKind::Aborted`。
//! - 网关返回的未知键不报错，但在转 [`Model`] 时被丢弃；需要无损保存原始 JSON 的调用方
//!   应使用 `ModelsStoreEntry::raw_models`（见 `providers/all.rs` 的说明）。
//!
//! TS: `src/providers/radius-config.ts`

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::oauth::{OAuthFetch, OAuthFetchRequest, default_oauth_fetch};
use crate::auth::types::AuthError;
use crate::types::{
    Model, ModelCost, ModelCostTier, ModelInputLimits, ModelInputModality, ModelPromptCache,
    ModelType, SamplingParams, SamplingParamsByThinkingLevel, ThinkingLevelMap,
};
use crate::utils::abort::AbortScope;
use crate::utils::text::owned_slice;

/// `DEFAULT_RADIUS_GATEWAY`。
///
/// 默认网关地址。[`crate::providers::radius::radius_provider`] 在调用方未提供 `gateway`
/// 时使用它，并且只有规范化后的 gateway 等于该默认值时才装载内置的公开目录；自定义网关
/// 从空目录开始，等 `refresh_models` 拉取。常量本身不做 URL 校验，规范化见
/// [`normalize_radius_gateway_url`]。
///
/// TS: `src/providers/radius-config.ts#DEFAULT_RADIUS_GATEWAY`
pub const DEFAULT_RADIUS_GATEWAY: &str = "https://radius.kiana.dev";

/// `RadiusGatewayModel`：网关返回的单个模型（已通过 [`is_radius_gateway_model`] 校验）。
///
/// 保留原始 JSON 对象（未知字段在转 [`Model`] 时丢弃）。
///
/// 这是类型别名而不是结构体，编译器不会替你校验字段：手工构造
/// [`RadiusGatewayConfig`] 前必须显式调用 [`is_radius_gateway_model`]，否则
/// [`get_radius_models_from_config`] 会静默跳过缺少 `id`/`name` 的条目。
pub type RadiusGatewayModel = Value;

/// `RadiusGatewayConfig`：`{ baseUrl, models }`。
///
/// 序列化形状与 TS 的同名类型一致（camelCase），用于把网关响应或凭证里的
/// `gatewayConfig` 反序列化；`models` 保持网关返回的顺序。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadiusGatewayConfig {
    /// 模型请求的基地址。
    pub base_url: String,
    /// 网关目录里的模型（顺序保持）。
    pub models: Vec<RadiusGatewayModel>,
}

/// `isRadiusGatewayModel(value)`：对象、非数组，且已知字段类型正确。
///
/// 判断条件与 TS 的类型守卫逐条对应：必须是 JSON 对象（`null` 与数组都不算），且
/// `id`/`name` 为字符串、`reasoning` 为布尔、`input` 为数组、`cost` 为对象、
/// `contextWindow`/`maxTokens` 为数字。未知键允许存在，不影响结果。
///
/// 返回 `false` 只表示「这条模型不可用」，调用方通常选择过滤而不是报错；本模块的
/// [`sanitize_radius_gateway_config`] 就是这样用的。
///
/// TS: `src/providers/radius-config.ts#isRadiusGatewayModel`
pub fn is_radius_gateway_model(value: &Value) -> bool {
    let Value::Object(model) = value else {
        return false;
    };
    let has_string = |key: &str| model.get(key).is_some_and(Value::is_string);
    has_string("id")
        && has_string("name")
        && model.get("reasoning").is_some_and(Value::is_boolean)
        && model.get("input").is_some_and(Value::is_array)
        && model
            .get("cost")
            .is_some_and(|cost| matches!(cost, Value::Object(_)))
        && model.get("contextWindow").is_some_and(Value::is_number)
        && model.get("maxTokens").is_some_and(Value::is_number)
}

/// `sanitizeRadiusGatewayConfig(config)`。
///
/// 输入任意 JSON（网关响应或凭证字段）：顶层必须是对象，`baseUrl` 必须是字符串，
/// `models` 必须是数组；任一不满足返回 `None`。模型数组逐条用
/// [`is_radius_gateway_model`] 过滤后克隆，因此返回的配置与输入不共享内存，坏模型被丢弃
/// 而不会让整体失败。
///
/// 不校验 `baseUrl` 是否为空串或合法 URL：空串会原样传给
/// [`get_radius_models_from_config`]，最终写进每条模型的 `base_url`。
///
/// TS: `src/providers/radius-config.ts#sanitizeRadiusGatewayConfig`
pub fn sanitize_radius_gateway_config(config: &Value) -> Option<RadiusGatewayConfig> {
    let Value::Object(map) = config else {
        return None;
    };
    let base_url = map.get("baseUrl")?.as_str()?.to_string();
    let models = map.get("models")?.as_array()?;
    Some(RadiusGatewayConfig {
        base_url,
        models: models
            .iter()
            .filter(|model| is_radius_gateway_model(model))
            .cloned()
            .collect(),
    })
}

/// `normalizeRadiusGatewayUrl(value)`：`/^https?:\/\//iu` 不匹配则前缀 `https://`，再去掉尾部 `/`。
///
/// 只做两件事：补 scheme 前缀、删除**所有**尾随 `/`。不解析主机、不改大小写，也不校验
/// 其余部分是否合法——例如 `httpx://x` 会被当成没有 scheme，得到 `https://httpx://x`。
/// scheme 判定按 JS `i` + `u` 的简单大小写折叠，`U+017F`（LATIN SMALL LETTER LONG S）
/// 等价于 `s`（见私有函数 `eq_unicode_case` 的说明）。
///
/// TS: `src/providers/radius-config.ts#normalizeRadiusGatewayUrl`
pub fn normalize_radius_gateway_url(value: &str) -> String {
    let with_scheme = if has_http_scheme(value) {
        value.to_string()
    } else {
        format!("https://{value}")
    };
    with_scheme.trim_end_matches('/').to_string()
}

/// `char` 在 `/iu` 正则里是否等价于 ASCII 小写字母 `lower`（Unicode 简单大小写折叠）。
///
/// 四个 scheme 字母里只有 `s` 有额外的折叠等价字符：U+017F LATIN SMALL LETTER LONG S
/// （`/^https?:\/\//iu.test("httpſ://x") === true`，实测 Node 26）。
fn eq_unicode_case(ch: char, lower: char) -> bool {
    ch == lower || ch == lower.to_ascii_uppercase() || (lower == 's' && ch == '\u{017F}')
}

/// `/^https?:\/\//iu.test(value)`：只匹配 scheme 前缀，大小写不敏感（含 Unicode 折叠）。
///
/// 分两步实现：先逐字符比较 `http`，再尝试可选的 `s`（正则的 `s?`），最后要求剩余部分
/// 以 `://` 开头。`s?` 用 `chars.clone()` 试探，避免在失败时回退已消费的字符；
/// `starts_with("://")` 只在字符边界上比较，因此不会接受全角斜杠之类的近似字符。
fn has_http_scheme(value: &str) -> bool {
    let mut chars = value.chars();
    for target in ['h', 't', 't', 'p'] {
        match chars.next() {
            Some(ch) if eq_unicode_case(ch, target) => {}
            _ => return false,
        }
    }
    // `s?`
    let mut rest = chars.clone();
    let after_s = match rest.next() {
        Some(ch) if eq_unicode_case(ch, 's') => rest,
        _ => chars,
    };
    after_s.as_str().starts_with("://")
}

/// `getRadiusCredentialConfig(credential)`：读取 `credential.gatewayConfig` 并校验。
///
/// 只读 [`crate::auth::types::OAuthCredential`] 的扩展字段 `gatewayConfig`（`extra` 里的
/// JSON 值），再交给 [`sanitize_radius_gateway_config`]。凭证为 `None`、字段缺失或形状
/// 不对都返回 `None`，调用方无法区分这三种情况。
///
/// TS: `src/providers/radius-config.ts#getRadiusCredentialConfig`
pub fn get_radius_credential_config(
    credential: Option<&crate::auth::types::OAuthCredential>,
) -> Option<RadiusGatewayConfig> {
    let gateway_config = credential?.get("gatewayConfig")?;
    sanitize_radius_gateway_config(gateway_config)
}

/// `getRadiusModelsFromConfig(providerId, config)`：每个模型加 `api`/`provider`/`baseUrl`。
///
/// 对 `config.models` 逐条转换：覆盖 `api`/`provider`/`baseUrl` 后按 [`Model`]
/// 反序列化，无法映射的条目被跳过，因此返回的 `Vec` 可能短于输入。顺序与
/// `config.models` 一致；空目录返回空 `Vec`。
///
/// 转换后的 `provider` 恒等于 `provider_id`，与原始 JSON 里自带的 `provider` 无关。
///
/// TS: `src/providers/radius-config.ts#getRadiusModelsFromConfig`
pub fn get_radius_models_from_config(
    provider_id: &str,
    config: &RadiusGatewayConfig,
) -> Vec<Model> {
    config
        .models
        .iter()
        .filter_map(|model| radius_model_to_model(provider_id, &config.base_url, model))
        .collect()
}

/// 单个网关模型 → [`Model`]（`api: "kiana-messages"`）。
///
/// TS 是 `{ ...model, api, provider, baseUrl }`：先把三个字段覆盖进原始 JSON 对象，
/// 再按 [`Model`] 反序列化，因此 Model 支持的全部字段（含未知的 Model 键）都被保留。
fn radius_model_to_model(provider_id: &str, base_url: &str, value: &Value) -> Option<Model> {
    let Value::Object(model) = value else {
        return None;
    };
    // `id`/`name` 是 Model 的必填字段；校验条件由 `is_radius_gateway_model` 保证，
    // 但手写的 config 也可能直接进这里（TS 会原样展开）。
    model.get("id")?.as_str()?;
    model.get("name")?.as_str()?;
    let normalized = normalize_radius_gateway_model(provider_id, base_url, model);
    match serde_json::from_value(Value::Object(normalized)) {
        Ok(model) => Some(model),
        // 理论上不可达（必需字段已兜底）；保底回落到逐字段映射，绝不丢弃模型。
        Err(_) => radius_model_to_model_manual(provider_id, base_url, value),
    }
}

/// `{ ...model, api, provider, baseUrl }` + 针对固定 [`Model`] 形状的清洗。
///
/// 承担三件事：① 覆盖 `api`/`provider`/`baseUrl` 三个固定字段；② 把 [`Model`] 无法表达
/// 的取值清洗掉（`input` 只保留 text/image、数字字段转 `u64`、`cost` 补全必填费率）；
/// ③ 逐个可选字段验证，类型不符时删除该键而不是丢弃整条模型。顺序依赖
/// `serde_json::Map` 保序：已存在的键覆盖后留在原位置，新键追加到末尾，与 TS 的展开顺序
/// 等价。
fn normalize_radius_gateway_model(
    provider_id: &str,
    base_url: &str,
    model: &serde_json::Map<String, Value>,
) -> serde_json::Map<String, Value> {
    let mut normalized = model.clone();
    // 覆盖已存在的同名键（`serde_json::Map` 保序：已存在的键保持原位置）。
    normalized.insert("api".into(), Value::String("kiana-messages".into()));
    normalized.insert("provider".into(), Value::String(provider_id.into()));
    normalized.insert("baseUrl".into(), Value::String(base_url.into()));

    // `input`：ModelInputModality 只表达 text/image；TS 原样保留的其它取值只能跳过。
    let input = normalized
        .get("input")
        .and_then(Value::as_array)
        .map(|values| {
            Value::Array(
                values
                    .iter()
                    .filter(|value| matches!(value.as_str(), Some("text" | "image")))
                    .cloned()
                    .collect(),
            )
        })
        .unwrap_or_else(|| Value::Array(Vec::new()));
    normalized.insert("input".into(), input);

    // `contextWindow`/`maxTokens`：Model 用 Option<u64>（小数在 TS 里原样保留，这里截断）。
    for key in ["contextWindow", "maxTokens"] {
        match normalized.get(key).and_then(Value::as_f64) {
            Some(number) => {
                normalized.insert(key.into(), Value::Number((number as u64).into()));
            }
            None => {
                normalized.remove(key);
            }
        }
    }

    // `cost`：ModelCost 的四个费率是必填字段；部分对象必须保留已给出的值（缺失 → 0）。
    let mut cost = match normalized.get("cost") {
        Some(Value::Object(cost)) => cost.clone(),
        _ => serde_json::Map::new(),
    };
    for key in ["input", "output", "cacheRead", "cacheWrite"] {
        let number = cost.get(key).and_then(Value::as_f64).unwrap_or(0.0);
        let number = serde_json::Number::from_f64(number)
            .unwrap_or_else(|| serde_json::Number::from_f64(0.0).expect("0.0 is finite"));
        cost.insert(key.into(), Value::Number(number));
    }
    if let Some(tiers) = cost.get("tiers")
        && serde_json::from_value::<Vec<ModelCostTier>>(tiers.clone()).is_err()
    {
        cost.remove("tiers");
    }
    normalized.insert("cost".into(), Value::Object(cost));

    // Model 支持的可选字段：类型不符时移除该键（TS 保留原始值，固定结构体无法表达）；
    // 删除单个可选字段比丢弃整条模型更接近 TS 的行为。
    sanitize_optional::<IndexMap<String, String>>(&mut normalized, "headers");
    sanitize_optional::<ModelType>(&mut normalized, "type");
    sanitize_optional::<ModelPromptCache>(&mut normalized, "promptCache");
    sanitize_optional::<SamplingParams>(&mut normalized, "samplingParams");
    sanitize_optional::<SamplingParamsByThinkingLevel>(
        &mut normalized,
        "samplingParamsByThinkingLevel",
    );
    sanitize_optional::<ModelInputLimits>(&mut normalized, "inputLimits");
    sanitize_optional::<ThinkingLevelMap>(&mut normalized, "thinkingLevelMap");
    sanitize_optional::<Vec<String>>(&mut normalized, "output");
    sanitize_optional::<bool>(&mut normalized, "reasoning");
    // `compat` 是任意 JSON（`JsonValue`），无需清洗；未知键由 serde 忽略。
    normalized
}

/// 若 `key` 存在但不能按 `T` 反序列化（`null` 视为合法的 Option），移除它。
///
/// `null` 会被成功解析为 `Option::None`，因此保留给后续的 [`Model`] 反序列化；只有类型
/// 确实不符（例如 `headers` 是数组）才删除该键。删除粒度是单个字段：其余字段与整条模型
/// 仍然保留，这比丢弃整条模型更接近 TS「原样展开」的行为。
fn sanitize_optional<T: serde::de::DeserializeOwned>(
    map: &mut serde_json::Map<String, Value>,
    key: &str,
) {
    if let Some(value) = map.get(key).cloned()
        && serde_json::from_value::<Option<T>>(value).is_err()
    {
        map.remove(key);
    }
}

/// 逐字段映射（[`Model`] 反序列化意外失败时的保底路径；保留 Model 支持的已知字段）。
///
/// 这是降级路径：只映射核心字段（id/name/api/provider/baseUrl/input/cost/reasoning/
/// thinkingLevelMap/contextWindow/maxTokens），其余可选字段保持默认。主路径
/// `radius_model_to_model` 正常时会保留它们；只有主路径反序列化失败才会走到这里。
fn radius_model_to_model_manual(provider_id: &str, base_url: &str, value: &Value) -> Option<Model> {
    let Value::Object(model) = value else {
        return None;
    };
    let input = model
        .get("input")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| match value.as_str() {
                    Some("text") => Some(ModelInputModality::Text),
                    Some("image") => Some(ModelInputModality::Image),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let cost = radius_model_cost(model.get("cost"));
    let thinking_level_map = model
        .get("thinkingLevelMap")
        .filter(|value| !value.is_null())
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    Some(Model {
        id: model.get("id")?.as_str()?.to_string(),
        name: model.get("name")?.as_str()?.to_string(),
        api: "kiana-messages".to_string(),
        provider: provider_id.to_string(),
        base_url: base_url.to_string(),
        input,
        cost,
        reasoning: model.get("reasoning").and_then(Value::as_bool),
        thinking_level_map,
        context_window: model
            .get("contextWindow")
            .and_then(Value::as_f64)
            .map(|value| value as u64),
        max_tokens: model
            .get("maxTokens")
            .and_then(Value::as_f64)
            .map(|value| value as u64),
        ..Model::default()
    })
}

/// 网关模型 `cost` 对象 → [`ModelCost`]，逐字段取值（缺失/非数值 → 0）。
///
/// TS 的 `isRadiusGatewayModel` 只要求 `cost` 是非 null、非数组的对象，随后
/// `{...model}` 原样保留；因此**部分字段**的 cost（例如只有 `input`/`output`）必须保留
/// 已给出的值，而不是整体回落到全 0。未知键在固定结构体里仍然丢弃（见模块文档）。
///
/// `tiers` 只在能反序列化为 [`ModelCostTier`] 列表时保留；`null` 或坏形状视为缺失。
fn radius_model_cost(cost: Option<&Value>) -> ModelCost {
    let Some(Value::Object(cost)) = cost else {
        return ModelCost::default();
    };
    let number = |key: &str| cost.get(key).and_then(Value::as_f64).unwrap_or(0.0);
    ModelCost {
        input: number("input"),
        output: number("output"),
        cache_read: number("cacheRead"),
        cache_write: number("cacheWrite"),
        tiers: cost
            .get("tiers")
            .filter(|value| !value.is_null())
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
    }
}

/// `getRadiusModels(providerId, credential)`。
///
/// [`get_radius_credential_config`] 与 [`get_radius_models_from_config`] 的组合；凭证没有
/// 可用的 `gatewayConfig` 时返回空 `Vec`，**不会**回落到内置的 Radius 目录。
///
/// TS: `src/providers/radius-config.ts#getRadiusModels`
pub fn get_radius_models(
    provider_id: &str,
    credential: Option<&crate::auth::types::OAuthCredential>,
) -> Vec<Model> {
    match get_radius_credential_config(credential) {
        Some(config) => get_radius_models_from_config(provider_id, &config),
        None => Vec::new(),
    }
}

/// `truncateHttpBody(body)`：JS trim 后超过 512 个 UTF-16 码元则截断并加 `…`（U+2026）。
///
/// 长度按 UTF-16 码元计（JS 的 `string.length`），`owned_slice` 在切点落在代理对中间时
/// 会把配对码元一并保留，因此极少数情况下结果会比 512 个码元多一个，但不会产生孤立
/// 代理项（`ARCHITECTURE.md` §4.6）。
fn truncate_http_body(body: &str) -> String {
    let trimmed = crate::auth::oauth::js_trim(body);
    if trimmed.encode_utf16().count() > 512 {
        format!("{}…", owned_slice(trimmed, 0, Some(512)))
    } else {
        trimmed.to_string()
    }
}

/// `loadRadiusGatewayConfig(gateway, apiKey?, signal?)`。
///
/// 发一个 `GET <gateway>/v1/config`，请求头 `accept: application/json`；`api_key` 非空时
/// 追加 `authorization: Bearer <key>`。成功要求响应为 2xx，且正文通过
/// [`sanitize_radius_gateway_config`]。
///
/// `signal` 是取消作用域；`None` 时新建一个不会被取消的作用域。本函数不设置超时，也不
/// 做重试；取消与网络错误由传输层上报。
///
/// # 错误
///
/// 所有失败都返回 [`AuthError`]：非法 URL、传输错误、非 2xx、JSON 语法错误、配置形状
/// 不符。调用方（`providers/radius.rs`）负责转换成 `AiError`。
///
/// 默认传输见 [`crate::auth::oauth::default_oauth_fetch`]。
///
/// TS: `src/providers/radius-config.ts#loadRadiusGatewayConfig`
pub async fn load_radius_gateway_config(
    gateway: &str,
    api_key: Option<&str>,
    signal: Option<&AbortScope>,
) -> Result<RadiusGatewayConfig, AuthError> {
    let fetch = default_oauth_fetch();
    load_radius_gateway_config_with(fetch.as_ref(), gateway, api_key, signal).await
}

/// 带注入传输的 [`load_radius_gateway_config`]（测试用）。
///
/// `fetch` 由调用方提供（单测用 `MockFetch`）；生产入口
/// [`load_radius_gateway_config`] 传 `default_oauth_fetch()`。除传输实现外行为完全一致。
pub async fn load_radius_gateway_config_with(
    fetch: &dyn OAuthFetch,
    gateway: &str,
    api_key: Option<&str>,
    signal: Option<&AbortScope>,
) -> Result<RadiusGatewayConfig, AuthError> {
    // `new URL("/v1/config", gateway)`：绝对路径会丢弃 gateway 的路径部分，因此即使
    // 自定义网关带子路径（如 `https://host/api`），请求也落在 `<origin>/v1/config`。
    let url = crate::auth::oauth::resolve_url(gateway, "/v1/config")?;
    // TS 的 `signal` 可选；Rust 用 AbortScope 表达，未提供时新建一个不会被取消的实例。
    let scope = signal.cloned().unwrap_or_default();
    let mut request =
        OAuthFetchRequest::new(url.as_str(), &scope).header("accept", "application/json");
    // TS `if (apiKey)`：空串不发送 Authorization 头。
    if let Some(api_key) = api_key.filter(|key| !key.is_empty()) {
        request = request.header("authorization", format!("Bearer {api_key}"));
    }
    let response = fetch.fetch(request).await?;
    if !response.ok() {
        return Err(AuthError::new(format!(
            "Could not load Radius config from {gateway}: {}: {}",
            response.status,
            truncate_http_body(&response.text())
        )));
    }
    let body: Value = parse_json_body(&response)?;
    sanitize_radius_gateway_config(&body)
        .ok_or_else(|| AuthError::new(format!("Invalid Radius config from {gateway}")))
}

/// `await response.json()`：JSON 解析失败时保留 V8 的 `SyntaxError` 文案。
fn parse_json_body(response: &crate::auth::oauth::OAuthFetchResponse) -> Result<Value, AuthError> {
    response.json()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_gateway_urls() {
        assert_eq!(
            normalize_radius_gateway_url("radius.example"),
            "https://radius.example"
        );
        assert_eq!(
            normalize_radius_gateway_url("https://radius.example///"),
            "https://radius.example"
        );
        assert_eq!(
            normalize_radius_gateway_url("HTTP://Radius.Example/"),
            "HTTP://Radius.Example"
        );
        assert_eq!(
            normalize_radius_gateway_url("https://radius.example/path///"),
            "https://radius.example/path"
        );
        assert_eq!(normalize_radius_gateway_url("http://x"), "http://x");
        assert_eq!(
            normalize_radius_gateway_url("httpx://x"),
            "https://httpx://x"
        );
        // 回归（评审发现）：`/^https?:\/\//iu` 的 `u` 让 U+017F（LONG S）也等价于 `s`。
        assert_eq!(
            normalize_radius_gateway_url("http\u{017f}://x"),
            "http\u{017f}://x"
        );
        assert_eq!(
            normalize_radius_gateway_url("HTTP\u{017f}://x"),
            "HTTP\u{017f}://x"
        );
    }

    #[test]
    fn validates_and_sanitizes_gateway_configs() {
        let valid_model = json!({
            "id": "m1",
            "name": "Model 1",
            "reasoning": true,
            "input": ["text", "image"],
            "cost": {"input": 1.0, "output": 2.0, "cacheRead": 0.5, "cacheWrite": 0.0},
            "contextWindow": 128000,
            "maxTokens": 4096
        });
        let config =
            json!({"baseUrl": "https://radius.example/v1", "models": [valid_model, {"id": "bad"}]});
        let sanitized = sanitize_radius_gateway_config(&config).expect("config");
        assert_eq!(sanitized.base_url, "https://radius.example/v1");
        assert_eq!(sanitized.models.len(), 1);
        assert!(is_radius_gateway_model(&sanitized.models[0]));
        assert!(sanitize_radius_gateway_config(&json!({"models": []})).is_none());
        assert!(sanitize_radius_gateway_config(&json!({"baseUrl": "x", "models": {}})).is_none());
        assert!(sanitize_radius_gateway_config(&json!(null)).is_none());
        assert!(sanitize_radius_gateway_config(&json!([1])).is_none());
        assert!(!is_radius_gateway_model(&json!([1])));
    }

    #[test]
    fn converts_gateway_models_to_kiana_messages_models() {
        let config = RadiusGatewayConfig {
            base_url: "https://radius.example/v1".into(),
            models: vec![json!({
                "id": "m1",
                "name": "Model 1",
                "reasoning": false,
                "input": ["text", "audio"],
                "cost": {"input": 1.0, "output": 2.0, "cacheRead": 0.5, "cacheWrite": 0.25},
                "contextWindow": 128000,
                "maxTokens": 4096
            })],
        };
        let models = get_radius_models_from_config("radius", &config);
        assert_eq!(models.len(), 1);
        let model = &models[0];
        assert_eq!(model.api, "kiana-messages");
        assert_eq!(model.provider, "radius");
        assert_eq!(model.base_url, "https://radius.example/v1");
        assert_eq!(model.input, vec![ModelInputModality::Text]);
        assert_eq!(model.context_window, Some(128000));
        assert_eq!(model.max_tokens, Some(4096));
        assert_eq!(model.reasoning, Some(false));
    }

    /// 回归（评审发现）：TS 的 `{...model, api, provider, baseUrl}` 保留 Model 支持的全部
    /// 字段；Rust 曾只映射 9 个字段，丢掉 headers/inputLimits/type/promptCache/samplingParams/
    /// compat/output。
    #[test]
    fn preserves_every_model_field_in_the_gateway_payload() {
        let config = RadiusGatewayConfig {
            base_url: "https://radius.example/v1".into(),
            models: vec![json!({
                "id": "m1",
                "name": "Model 1",
                "reasoning": true,
                "input": ["text", "image", "audio"],
                "cost": {
                    "input": 1.0,
                    "output": 2.0,
                    "cacheRead": 0.5,
                    "cacheWrite": 0.25,
                    "tiers": [{
                        "inputTokensAbove": 200000,
                        "input": 2.0,
                        "output": 4.0,
                        "cacheRead": 1.0,
                        "cacheWrite": 0.5
                    }]
                },
                "contextWindow": 200000,
                "maxTokens": 8192,
                "thinkingLevelMap": {"low": "low", "high": "high"},
                "headers": {"x-extra": "1"},
                "inputLimits": {"maxRequestBytes": 1000, "images": {"maxPerMessage": 2}},
                "type": "chat",
                "promptCache": {"short": 300, "long": 3600},
                "samplingParams": {"temperature": 0.5},
                "samplingParamsByThinkingLevel": {"high": {"temperature": 0.9}},
                "compat": {"foo": true},
                "output": ["text"],
            })],
        };
        let models = get_radius_models_from_config("radius", &config);
        assert_eq!(models.len(), 1);
        let model = &models[0];
        assert_eq!(
            model.input,
            vec![ModelInputModality::Text, ModelInputModality::Image]
        );
        let value = serde_json::to_value(model).expect("serialize model");
        assert_eq!(value["api"], json!("kiana-messages"));
        assert_eq!(value["provider"], json!("radius"));
        assert_eq!(value["baseUrl"], json!("https://radius.example/v1"));
        assert_eq!(value["headers"]["x-extra"], json!("1"));
        assert_eq!(value["inputLimits"]["maxRequestBytes"], json!(1000));
        assert_eq!(value["inputLimits"]["images"]["maxPerMessage"], json!(2));
        assert_eq!(value["type"], json!("chat"));
        assert_eq!(value["promptCache"]["short"], json!(300.0));
        assert_eq!(value["samplingParams"]["temperature"], json!(0.5));
        assert_eq!(
            value["samplingParamsByThinkingLevel"]["high"]["temperature"],
            json!(0.9)
        );
        assert_eq!(value["compat"]["foo"], json!(true));
        assert_eq!(value["output"], json!(["text"]));
        assert_eq!(value["thinkingLevelMap"]["low"], json!("low"));
        assert_eq!(
            value["cost"]["tiers"][0]["inputTokensAbove"],
            json!(200000.0)
        );
    }

    /// 回归（评审发现）：TS 用 `if (apiKey)` 判断，空串不发送 Authorization 头。
    #[tokio::test]
    async fn empty_api_keys_do_not_send_an_authorization_header() {
        let fetch = crate::auth::oauth::testing::MockFetch::new(
            |request: crate::auth::oauth::OAuthFetchRequest| {
                assert_eq!(request.header_value("authorization"), None);
                Box::pin(async {
                    Ok(crate::auth::oauth::OAuthFetchResponse::json_response(
                        200,
                        &json!({"baseUrl": "https://r.example", "models": []}),
                    ))
                })
            },
        );
        let config =
            load_radius_gateway_config_with(&*fetch, "https://gateway.example", Some(""), None)
                .await
                .expect("config");
        assert_eq!(config.base_url, "https://r.example");
    }

    #[test]
    fn keeps_partial_gateway_cost_objects() {
        // TS 只要求 cost 是非 null、非数组的对象；`{...model}` 保留部分字段。
        let config = RadiusGatewayConfig {
            base_url: "https://radius.example/v1".into(),
            models: vec![json!({
                "id": "m1",
                "name": "Model 1",
                "reasoning": false,
                "input": ["text"],
                "cost": {"input": 3.0, "output": 15.0},
                "contextWindow": 200000,
                "maxTokens": 8192
            })],
        };
        let models = get_radius_models_from_config("radius", &config);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].cost.input, 3.0);
        assert_eq!(models[0].cost.output, 15.0);
        assert_eq!(models[0].cost.cache_read, 0.0);
        assert_eq!(models[0].cost.cache_write, 0.0);
    }

    #[test]
    fn reads_gateway_config_from_credentials() {
        let credential = crate::auth::types::OAuthCredential::new("access", "refresh", 0.0);
        assert!(get_radius_models("radius", Some(&credential)).is_empty());
        let mut credential = credential;
        credential.extra.insert(
            "gatewayConfig".into(),
            json!({"baseUrl": "https://r.example", "models": []}),
        );
        assert_eq!(
            get_radius_credential_config(Some(&credential)).map(|config| config.base_url),
            Some("https://r.example".to_string())
        );
        assert!(get_radius_models("radius", Some(&credential)).is_empty());
    }

    #[test]
    fn truncates_http_bodies_with_the_js_ellipsis() {
        let short = truncate_http_body("  hello  ");
        assert_eq!(short, "hello");
        let long = "x".repeat(513);
        let truncated = truncate_http_body(&long);
        assert_eq!(truncated.chars().count(), 513);
        assert!(truncated.ends_with('…'));
    }
}
