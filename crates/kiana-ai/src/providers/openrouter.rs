//! providers/openrouter.ts：`openrouter_provider` provider 工厂与目录个性化。
//!
//! TS: `packages/ai/src/providers/openrouter.ts`
//!
//! # 这个文件负责什么
//!
//! 1. `openrouter_provider` 构造内置 provider `openrouter`：目录来自随仓库 vendored 的
//!    `data/openrouter.json`（chat + image + classifier），`build.rs` 在编译期把它摊平成
//!    静态表。chat 请求按 `model.api` 在
//!    `anthropic-messages` / `openai-completions` 之间分派，图像走 `openrouter-images`，
//!    分类走 `typesafe-system-one`。鉴权支持 `OPENROUTER_API_KEY` 与 OpenRouter OAuth。
//! 2. `personalize_open_router_catalog` 用当前 key 的视角裁剪目录：`GET
//!    <base>/models/user` 返回的条目用于筛选该 key 可用的 chat 模型，并更新其限额、价格。
//!
//! # 什么时候做个性化
//!
//! 本工厂**不设置** `fetch_models`，所以直接构造出来的 provider 是静态目录
//! （`has_refresh_models()` 为 `false`）。动态刷新由 `kiana-config` 的
//! `PersonalizedCatalogProvider` 包装层负责：它在允许联网的刷新阶段合并内置目录与远端
//! 目录，再调用本模块导出的 `personalize_open_router_catalog`
//! （见 `kiana-config/src/models/remote_catalog.rs` 与 `runtime.rs`）。
//!
//! # 个性化的语义
//!
//! - 没有可用 key（既非 OAuth、API key 也为空）→ 原样返回；
//! - 401/403/404 → 原样返回（自定义 models.json 代理可能没有该端点）；
//! - 其他非 2xx、响应体不是合法 JSON、或 JSON 形状不符（`data` 不是数组）→
//!   `Err(AiError)`，由刷新层记入该 provider 的 `errors`，不会中断其他 provider；
//! - chat 模型：只保留 `/models/user` 返回的 id（不在列表里的会被**丢弃**）；缺失的
//!   limit 保留目录原值；pricing 为真值时用 `open_router_cost` 整体替换 `cost`；
//! - image/classifier 模型原样保留：本函数只对 chat 条目做个性化，其余条目直接透传。
//!
//! # 编码差异
//!
//! `Model` 的 `context_window`/`max_tokens` 是 `Option<u64>`（TS 是任意 `number`），
//! 换算规则见 `to_token_count`；`AnyModel::Remote` 分支会把更新后的字段写回原始 JSON，
//! 以保留远端目录的未知字段与键序。

use std::sync::Arc;

use indexmap::IndexMap;
use kiana_json::is_truthy;
use serde_json::Value;

use crate::api::anthropic_messages::anthropic_messages_api;
use crate::api::openai_completions::openai_completions_api;
use crate::api::openrouter_images::openrouter_images_api;
use crate::api::typesafe_system_one::typesafe_system_one_api;
use crate::auth::helpers::{LazyOAuthInput, env_api_key_auth, lazy_oauth};
use crate::auth::oauth::load::load_openrouter_oauth;
use crate::auth::oauth::{OAuthFetchRequest, default_oauth_fetch};
use crate::auth::types::{Credential, ProviderAuth};
use crate::error::{AiError, AiErrorKind};
use crate::models::provider::ProviderApiImpl;
use crate::models::{CreateProviderOptions, Provider, RefreshModelsContext, create_provider};
use crate::providers::openrouter_api::open_router_cost;
use crate::providers::{catalog_chat_models, catalog_classifier_models, catalog_image_models};
use crate::types::{AnyModel, ModelType};
use crate::utils::model_operations::is_model_type;

/// `OPENROUTER_BASE_URL`。
///
/// 既是 provider 的默认 `base_url`，也是个性化请求在 `RefreshModelsContext::base_url`
/// 为 `None` 时的兜底地址。拼接 `/models/user` 前会去掉尾部 `/`，因此带不带尾斜杠都
/// 得到同一路径。
pub const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";
/// `OPENROUTER_USER_MODELS_TIMEOUT_MS`。
///
/// 对应 TS 的 `AbortSignal.timeout(15_000)`：只约束 `/models/user` 这一次请求，不是
/// provider 其他请求的超时。超时以 `TimeoutError` 拒绝，最终被映射为
/// [`AiErrorKind::Other`]（见 `fetch_error_to_ai_error`）。
const OPENROUTER_USER_MODELS_TIMEOUT_MS: u64 = 15_000;

/// JS 数字真值（`0`、`NaN` 为假）。
///
/// 用于复刻 TS 里 `top_provider.context_length || context_length` 与
/// `top_provider.max_completion_tokens` 的判定：OpenRouter 把“没有限额”表示为缺键、
/// `null` 或 `0`，这三种都必须回落到下一个来源，所以不能只判 `Option::is_some`。
/// 它只处理数字限额；pricing 的真值判断走 [`is_truthy`]（见调用处），后者还能表达
/// 对象/数组为真、空串为假等本函数做不到的判定。
fn truthy_number(value: Option<f64>) -> Option<f64> {
    value.filter(|value| *value != 0.0 && !value.is_nan())
}

/// 把 JS 的 `number` 写进 `Model` 的 `Option<u64>` 字段。
///
/// TS 的 `contextWindow`/`maxTokens` 是 `number`，原样保存任意数值；Rust 的 `Model` 用 `u64`
/// 承载 token 数（spec §2.1.2 的类型选择），所以小数被截断、负数与超过 `u64::MAX` 的值饱和。
/// 只影响上游返回非法数字的情况（OpenRouter 的 `context_length`/`max_completion_tokens`）。
/// 非数值的 JSON 值（如字符串 `"1000"`）在 TS 里是真值并被原样赋值，Rust 的 `as_f64` 取不到
/// 数字，因此退回目录里的原值（固定结构体无法表达非数字）。
///
/// `as u64` 是 Rust 的饱和转换：负数变 `0`、超过 `u64::MAX` 的值封顶、小数向零截断。
/// 调用方（`personalize_open_router_catalog`）已先用 `truthy_number` 过滤掉 0/NaN，
/// 但负数与 `+∞` 仍可能传进来（`truthy_number` 不拦它们），由上面的饱和转换兜底。
fn to_token_count(value: Option<f64>) -> Option<u64> {
    value.map(|value| value as u64)
}

/// Restrict an OpenRouter catalog to the chat models the key may use, with the key's limits and
/// prices from `GET /models/user`. Image and classifier models are returned unchanged. A missing
/// limit keeps the catalog value. The request uses the provider's configured base URL, so regional
/// endpoints such as `https://us.openrouter.ai/api/v1` apply their region's filtering. When the
/// endpoint rejects the key (401/403) or does not exist (404), for example with a proxy configured
/// in models.json, the catalog is returned unchanged.
///
/// # 执行流程
///
/// 1. 从凭证取 key：OAuth 用 `access`，API key 用 `key`；没有 key 或空串直接返回原目录。
/// 2. 用 `context.base_url`（缺省 [`OPENROUTER_BASE_URL`]）拼 `/models/user`，带
///    `authorization: Bearer <key>`；`context.signal` 参与取消，另有 15 秒超时。
/// 3. 把 `data` 数组按 `id` 建索引（只收对象且 `id` 为字符串的条目）。
/// 4. 逐条处理目录：非 chat 原样保留；chat 不在索引里则**丢弃**；命中的条目覆盖限额：
///    `context_length` 按 `top_provider.context_length || entry.context_length || 目录原值`，
///    `max_completion_tokens` 按 `top_provider.max_completion_tokens || 目录原值`；
///    pricing 为真值时整体替换成本。
///
/// # 失败与取消
///
/// - 请求失败/超时：`AbortError` → [`AiErrorKind::Aborted`]，其余（含 `TimeoutError`）
///   → [`AiErrorKind::Other`]，错误名保留（见 `fetch_error_to_ai_error`）；
/// - 401/403/404 不算失败：返回原目录，保留内置/远端合并结果；
/// - 响应体不是合法 JSON：`response.json()` 失败，按 `SyntaxError` 处理（归
///   [`AiErrorKind::Other`]，错误名保留）；
/// - JSON 有效但不是对象、没有 `data`、或 `data` 不是数组：`Invalid OpenRouter model
///   list response`；
/// - 其他非 2xx：`OpenRouter model list request failed: <status>`。
///
/// 函数本身不检查 `allow_network`：是否联网由调用它的刷新包装层决定。副作用只有一次
/// HTTP GET；输入目录被消费，返回新 `Vec`。
///
/// TS: `providers/openrouter.ts#personalizeOpenRouterCatalog`
#[allow(clippy::result_large_err)]
pub async fn personalize_open_router_catalog(
    catalog: Vec<AnyModel>,
    context: RefreshModelsContext,
) -> Result<Vec<AnyModel>, AiError> {
    // OAuth 凭证把 access token 当作 bearer key；API key 凭证取 key（可能为 None）。
    let key = match context.credential.as_ref() {
        Some(Credential::OAuth(credential)) => Some(credential.access.clone()),
        Some(Credential::ApiKey(credential)) => credential.key.clone(),
        None => None,
    };
    let Some(key) = key.filter(|key| !key.is_empty()) else {
        return Ok(catalog);
    };
    // `context.base_url` 是刷新层看到的 provider base URL（含 wrapper 覆盖），因此区域
    // 端点会按自己的区域过滤；只在缺省时回落到公开地址。去掉尾斜杠后再拼路径。
    let base_url = context
        .base_url
        .clone()
        .unwrap_or_else(|| OPENROUTER_BASE_URL.to_string());
    let url = format!("{}/models/user", base_url.trim_end_matches('/'));
    let request = OAuthFetchRequest::new(url, &context.signal)
        .header("accept", "application/json")
        .header("authorization", format!("Bearer {key}"))
        .timeout_ms(OPENROUTER_USER_MODELS_TIMEOUT_MS);
    let fetch = default_oauth_fetch();
    let response = fetch
        .fetch(request)
        .await
        .map_err(fetch_error_to_ai_error)?;
    if response.status == 401 || response.status == 403 || response.status == 404 {
        return Ok(catalog);
    }
    if !response.ok() {
        return Err(AiError::new(
            AiErrorKind::Other,
            format!("OpenRouter model list request failed: {}", response.status),
        ));
    }
    let body = response.json().map_err(fetch_error_to_ai_error)?;
    let Some(data) = body
        .as_object()
        .and_then(|body| body.get("data"))
        .and_then(Value::as_array)
    else {
        return Err(AiError::new(
            AiErrorKind::Other,
            "Invalid OpenRouter model list response",
        ));
    };
    // 只把格式正确的条目放进索引；重复 id 时后出现的覆盖先出现的（与 TS 的 Map 一致）。
    let mut allowed: IndexMap<String, Value> = IndexMap::new();
    for entry in data {
        let Some(entry_object) = entry.as_object() else {
            continue;
        };
        let Some(id) = entry_object.get("id").and_then(Value::as_str) else {
            continue;
        };
        allowed.insert(id.to_string(), entry.clone());
    }
    let mut personalized = Vec::with_capacity(catalog.len());
    for model in catalog {
        // 本函数只对 chat 条目做个性化：image/classifier 不参与过滤，原样保留。
        if !is_model_type(&model, ModelType::Chat) {
            personalized.push(model);
            continue;
        }
        // key 的可用列表是权威：目录里有、但列表里没有的 chat 模型被直接丢弃。
        let entry = match allowed.get(model.model().id.as_str()) {
            Some(entry) => entry,
            None => continue,
        };
        let entry = entry.as_object().expect("allowed entries are objects");
        // 限额优先级与生成器一致：context_length 走 top_provider → 条目级 → 目录原值；
        // max_completion_tokens 只有 top_provider 一处来源，缺失时保留目录原值。
        let top_provider = entry.get("top_provider").and_then(Value::as_object);
        let context_window = truthy_number(
            top_provider
                .and_then(|top_provider| top_provider.get("context_length"))
                .and_then(Value::as_f64),
        )
        .or_else(|| truthy_number(entry.get("context_length").and_then(Value::as_f64)));
        let max_tokens = truthy_number(
            top_provider
                .and_then(|top_provider| top_provider.get("max_completion_tokens"))
                .and_then(Value::as_f64),
        );
        let pricing = entry.get("pricing");
        let (mut chat, raw) = match model {
            AnyModel::Chat(chat) => (chat, None),
            AnyModel::Remote { model, json } => (model, Some(json)),
            _ => continue,
        };
        // `||`：OpenRouter reports a missing limit as absent, null or 0, like the generator's fallback.
        if let Some(context_window) = context_window {
            chat.context_window = to_token_count(Some(context_window));
        }
        if let Some(max_tokens) = max_tokens {
            chat.max_tokens = to_token_count(Some(max_tokens));
        }
        // pricing 只有为“真值”时才整体替换 cost：对象（即使空对象）为真，
        // null / 0 / "" / false 为假，保留目录里的成本。
        if let Some(pricing) = pricing.filter(|pricing| is_truthy(Some(pricing))) {
            chat.cost = open_router_cost(Some(pricing));
        }
        // `AnyModel::Remote` 保留着远端目录的原始 JSON：除了已知字段，还要把改过的
        // 三个字段写回去，否则持久化/展示时会丢掉这次个性化结果（未知字段因此不丢）。
        if let Some(Value::Object(mut json)) = raw {
            // TS `{...model, contextWindow, maxTokens, cost}` overwrites these
            // existing keys in place and preserves every unknown remote field.
            if let Some(value) = chat.context_window {
                json.insert("contextWindow".into(), Value::from(value));
            }
            if let Some(value) = chat.max_tokens {
                json.insert("maxTokens".into(), Value::from(value));
            }
            if let Ok(value) = serde_json::to_value(&chat.cost) {
                json.insert("cost".into(), value);
            }
            personalized.push(AnyModel::Remote {
                model: chat,
                json: Value::Object(json),
            });
        } else {
            personalized.push(AnyModel::Chat(chat));
        }
    }
    Ok(personalized)
}

/// `fetch` 的拒绝值（`AbortError`/`TimeoutError`/`TypeError`）→ [`AiError`]。
///
/// 只把名字为 `AbortError` 的失败归入 [`AiErrorKind::Aborted`]；`TimeoutError`（调用方
/// [`personalize_open_router_catalog`] 为 `/models/user` 请求设了 15 秒超时）与传输层
/// `TypeError` 都归 `Other`。原始 `name` 通过
/// `with_name` 保留，便于调用方区分失败种类。
fn fetch_error_to_ai_error(error: crate::auth::types::AuthError) -> AiError {
    let kind = if error.is_aborted() {
        AiErrorKind::Aborted
    } else {
        AiErrorKind::Other
    };
    AiError::new(kind, error.message).with_name(error.name)
}

/// 构造 OpenRouter provider（id `"openrouter"`，默认 base URL [`OPENROUTER_BASE_URL`]）。
///
/// 目录 = chat + image + classifier。chat 按 `model.api` 在 `anthropic-messages` 与
/// `openai-completions` 之间分派，image 用 `openrouter-images`，classifier 用
/// `typesafe-system-one`。鉴权同时注册 API key（`OPENROUTER_API_KEY`）与 OAuth
/// （懒加载，登录标签 "Sign in with OpenRouter"）。
///
/// 本函数**不安装** `fetch_models`：目录个性化由 `kiana-config` 的包装层在刷新时调用
/// [`personalize_open_router_catalog`] 完成（见模块文档）。
///
/// # Panics
///
/// `create_provider` 只在三类实现全为空时返回 `Err`；这里 `api`/`images`/`classifiers`
/// 都非空，`expect` 不可达。触发即说明本文件的实现注册被改坏。
///
/// TS: `providers/openrouter.ts#openrouterProvider`
pub fn openrouter_provider() -> Arc<dyn Provider> {
    // 三张独立目录表按 chat → image → classifier 拼接；类型信息在 AnyModel 变体里。
    let mut models = catalog_chat_models("openrouter");
    models.extend(catalog_image_models("openrouter"));
    models.extend(catalog_classifier_models("openrouter"));
    let mut api = IndexMap::new();
    // 键必须与目录里 chat 模型的 `api` 值逐字一致；未命中的模型会得到错误流。
    api.insert("anthropic-messages".to_string(), anthropic_messages_api());
    api.insert("openai-completions".to_string(), openai_completions_api());
    // 图像与分类是独立的能力表，同样按模型的 `api` 精确查表。
    let mut images = IndexMap::new();
    images.insert(
        "openrouter-images".to_string(),
        openrouter_images_api() as Arc<dyn crate::models::ProviderImages>,
    );
    let mut classifiers = IndexMap::new();
    classifiers.insert(
        "typesafe-system-one".to_string(),
        typesafe_system_one_api() as Arc<dyn crate::models::ProviderClassifier>,
    );
    create_provider(CreateProviderOptions {
        id: "openrouter".into(),
        name: Some("OpenRouter".into()),
        base_url: Some(OPENROUTER_BASE_URL.into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth(
                "OpenRouter API key",
                &["OPENROUTER_API_KEY"],
            )),
            oauth: Some(lazy_oauth(LazyOAuthInput {
                name: "OpenRouter OAuth".into(),
                is_subscription: None,
                login_label: Some("Sign in with OpenRouter".into()),
                load: Arc::new(load_openrouter_oauth),
            })),
        },
        models,
        api: Some(ProviderApiImpl::ByApi(api)),
        images: Some(images),
        classifiers: Some(classifiers),
        ..Default::default()
    })
    .expect("openrouter provider declares an api implementation")
}
