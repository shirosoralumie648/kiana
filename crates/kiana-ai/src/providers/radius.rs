//! providers/radius.ts：Radius 网关 provider（持久化、可动态刷新的目录）。
//!
//! TS: `packages/ai/src/providers/radius.ts`
//!
//! # 为什么手写 [`Provider`] 而不用 `create_provider`
//!
//! Radius 的目录语义是**整体替换**，而 `create_provider` 是「静态基线 + 动态覆盖」：
//! 未被覆盖的基线条目始终可见。Radius 的组织管理员可以在网关上禁用模型，因此一旦拿到
//! 账号目录，出厂基线就不能再把被禁用的模型带回来。本文件直接实现 [`Provider`]，用
//! `Option<Vec<Model>>`（包在共享锁里）表达「未知 / 已知非空 / 已知为空」三态。
//!
//! # 目录的三个来源（`refresh_models` 的阶段）
//!
//! 1. `context.stored`（ModelsStore 中的缓存）→ 过滤出本 provider 的条目，只更新内存；
//! 2. 没有缓存且凭证是 OAuth → 从凭证的 `gatewayConfig` 字段（旧版 Radius 实现的缓存）
//!    导入，并写入 ModelsStore；
//! 3. 允许联网时 → `GET <gateway>/v1/config` 拉取，写入 ModelsStore 并更新内存。
//!
//! # 关键不变量与失败模式
//!
//! - `dynamic_models` 是 `Arc<Mutex<Option<Vec<Model>>>>`：`None` 表示目录尚未知（回落
//!   到出厂基线），`Some(vec![])` 表示已知且为空（组织禁用了全部模型，必须显示为空）。
//!   锁只在同步的克隆/赋值里持有，不跨 `.await`。
//! - 所有目录更新都必须放进 `context.publish` 的 `update` 闭包；`publish` 返回 `false`
//!   表示本次刷新已被更新的一代或取消取代，此时立即停止后续阶段（不继续发布或发请求）。
//! - 网络阶段在 `.await` 之后必须重新检查 `context.signal`：HTTP 请求可能无法及时中断，
//!   过期结果不得写回当前代际。
//! - 错误：`load_radius_gateway_config` 的 `AuthError` 转成 `AiError`（只有
//!   `AbortError` 归为 [`AiErrorKind::Aborted`]）；`publish` 的存储错误原样传播，
//!   `Models.refresh()` 把它记入该 provider 的 `errors`，不会中断其他 provider。
//!
//! # 调用方
//!
//! `providers::all::builtin_providers` 用默认 options 构造一次；`kiana-config` 的
//! `configure_radius_providers` 按 models.json 里 `oauth: "radius"` 的条目重建，id、
//! name、gateway 都可覆盖（网关地址会去掉尾随 `/v1`）。

use std::sync::{Arc, Mutex};

/// 动态目录（`dynamicModels` 闭包变量）。
///
/// `None` = 尚未拿到账号目录，`current_models` 回落到出厂基线；`Some(vec![])` = 已知
/// 且为空，必须保持空（不能回落到基线，否则被组织禁用的模型会重新出现）。
type DynamicModels = Arc<Mutex<Option<Vec<Model>>>>;

use async_trait::async_trait;

use crate::api::kiana_messages::kiana_messages_api;
use crate::auth::helpers::{LazyOAuthInput, env_api_key_auth, lazy_oauth};
use crate::auth::oauth::RadiusOAuthOptions;
use crate::auth::oauth::load::load_radius_oauth;
use crate::auth::types::{AuthError, Credential, ProviderAuth};
use crate::error::{AiError, AiErrorKind};
use crate::event_stream::AssistantMessageEventStream;
use crate::models::{
    ModelsPublication, ModelsPublicationPersist, ModelsStoreEntry, Provider, ProviderStreams,
    RefreshModelsContext,
};
use crate::providers::catalog_chat_models;
use crate::providers::radius_config::{
    DEFAULT_RADIUS_GATEWAY, RadiusGatewayConfig, get_radius_models, get_radius_models_from_config,
    load_radius_gateway_config, normalize_radius_gateway_url,
};
use crate::types::{AnyModel, Model, SimpleStreamOptions, StreamOptions, TranscriptContext};

/// `RadiusProviderOptions`。
///
/// 对应 TS `radiusProvider(options?)` 的可选参数。所有字段为 `None` 时使用默认值；
/// `gateway` 在构造时会经 `normalize_radius_gateway_url` 规范化（无 scheme 补
/// `https://`、去掉全部尾随 `/`），规范化后的值同时用于 OAuth 流程与配置拉取。
#[derive(Debug, Clone, Default)]
pub struct RadiusProviderOptions {
    /// provider id（默认 `"radius"`）。
    pub id: Option<String>,
    /// 展示名（默认 `"Radius"`）。
    pub name: Option<String>,
    /// 网关地址（默认 [`DEFAULT_RADIUS_GATEWAY`]）。
    ///
    /// 只有规范化后等于默认网关时才加载出厂目录 `catalog_chat_models("radius")`；自定义
    /// 网关一律以空基线启动，等待刷新拿到该网关的真实目录（见 `radius_provider`）。
    pub gateway: Option<String>,
}

/// `Date.now()`。
///
/// 返回 Unix 毫秒时间戳，用于 `ModelsStoreEntry.checked_at`；系统时钟早于 epoch 时
/// 返回 0（不 panic）。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

/// `AuthError` → [`AiError`]（与 `models` 内部的转换一致）。
///
/// 只把 `AbortError` 归为 [`AiErrorKind::Aborted`]；`TimeoutError` 与其他失败都归
/// `Other`，原始 `name` 用 `with_name` 保留。
fn auth_error_to_ai_error(error: AuthError) -> AiError {
    let kind = if error.is_aborted() {
        AiErrorKind::Aborted
    } else {
        AiErrorKind::Other
    };
    AiError::new(kind, error.message).with_name(error.name)
}

/// `credential?.type === "oauth" ? credential.access : credential?.key`。
///
/// Radius OAuth 登录把 access token 当作网关 bearer key（见 `auth::oauth::radius` 的
/// `to_auth`）；API key 凭证直接取 `key`。`None` 表示当前没有可用凭证，拉配置时不带
/// `authorization` 头；非 2xx 会变成 `AuthError`，由刷新层记录。
fn credential_key(credential: Option<&Credential>) -> Option<String> {
    match credential {
        Some(Credential::OAuth(credential)) => Some(credential.access.clone()),
        Some(Credential::ApiKey(credential)) => credential.key.clone(),
        None => None,
    }
}

/// Radius gateway provider with a persisted, dynamically refreshed catalog.
///
/// 与 `CreatedProvider` 的关键差异是目录合并策略：这里没有「基线 + 覆盖层」，只有一份
/// `Option` 动态目录，`None` 时才使用 `baseline_models`。`streams` 固定为
/// `kiana_messages_api()`，因为网关只提供 `kiana-messages` 协议。
///
/// 结构体字段在构造后不再改变；刷新只替换 `dynamic_models` 里的内容，因此读取方法
/// 取 `&self` 即可，不需要额外锁保护其他字段。
struct RadiusProvider {
    id: String,
    name: String,
    gateway: String,
    auth: ProviderAuth,
    /// 出厂目录；只在默认网关、且尚未拿到账号目录时使用。
    baseline_models: Vec<Model>,
    /// Gateway catalog for this account. Radius org owners can disable models, so once known it
    /// replaces the shipped baseline instead of overlaying it. The baseline only covers the time
    /// before any catalog exists.
    dynamic_models: DynamicModels,
    /// 实际 chat 实现（`kiana-messages`）。
    streams: Arc<dyn ProviderStreams>,
}

impl RadiusProvider {
    /// 返回当前目录快照：已知动态目录优先，否则回落出厂基线。
    ///
    /// 锁中毒（持锁线程 panic）时取回内部值继续，避免一次 panic 让目录永久不可读。
    /// 两个分支都克隆 `Vec`，调用方拿到的是独立快照，不会看到之后的刷新。
    ///
    /// 注意 `Some(vec![])` 与 `None` 语义不同：前者是“已知为空”，不会再回落到基线。
    fn current_models(&self) -> Vec<Model> {
        match self.dynamic_models.lock() {
            Ok(dynamic) => dynamic
                .clone()
                .unwrap_or_else(|| self.baseline_models.clone()),
            // 中毒不代表数据失效；取回内部值并按未中毒处理。
            Err(poisoned) => poisoned
                .into_inner()
                .clone()
                .unwrap_or_else(|| self.baseline_models.clone()),
        }
    }
}

/// `dynamicModels = models`（在 `publish` 的 update 回调里执行）。
///
/// 语义是**整体替换**：空 `Vec` 同样是合法结果（组织禁用了全部模型），不能当成
/// “没有更新”。函数保持同步，因为它运行在 `ModelsPublication::update` 的
/// `FnOnce()` 闭包里，该闭包不允许 `.await`；锁只在赋值期间持有。
fn set_dynamic_models(dynamic_models: DynamicModels, models: Vec<Model>) {
    match dynamic_models.lock() {
        Ok(mut dynamic) => *dynamic = Some(models),
        Err(poisoned) => *poisoned.into_inner() = Some(models),
    }
}

// `Provider` 的实现刻意很薄：目录读写只操作内存快照，请求全部转发给
// `kiana_messages_api()`。`has_refresh_models` 保持默认 `true`（Radius 永远是动态
// provider），`get_all_models` 保持默认的“chat 列表”，因为网关目录只有 chat 模型。
#[async_trait]
impl Provider for RadiusProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        self.current_models()
    }

    async fn refresh_models(&self, context: RefreshModelsContext) -> Result<(), AiError> {
        // 阶段 1：恢复持久化缓存。`stored` 由 ModelsStore 按 provider id 读出；这里再按
        // `provider` 字段过滤一次，确保只会把属于本 provider 的模型放进动态目录。
        // 即使过滤结果为空也会写入 `Some(vec![])`：这表示“缓存已知、本 provider 没有
        // 模型”，按替换语义不再回落到出厂基线（与 TS 的 `dynamicModels = restored` 一致）。
        if let Some(stored) = &context.stored {
            let restored: Vec<Model> = stored
                .models
                .iter()
                .filter(|model| model.model().provider == self.id)
                .map(|model| model.model().clone())
                .collect();
            let dynamic_models = Arc::clone(&self.dynamic_models);
            let update_models = restored.clone();
            // `Unchanged`：只更新内存，不回写存储，保留 store 里的原条目（避免刷新
            // checkedAt、etag 等字段，也避免无谓写盘）。
            let published = (context.publish)(ModelsPublication {
                persist: ModelsPublicationPersist::Unchanged,
                update: Some(Box::new(move || {
                    set_dynamic_models(dynamic_models, update_models);
                })),
            })
            .await?;
            // 发布被更新的一代或取消取代：立即停止，不再导入旧缓存/联网。
            if !published {
                return Ok(());
            }
        }

        // 阶段 2：导入旧版 Radius 实现缓存在 OAuth 凭证里的目录（models-store 出现之前
        // 的升级路径）。只有“没有 ModelsStore 条目 + 凭证是 OAuth”时才需要：该缓存字段
        // 只存在于 OAuth 凭证上（`get_radius_models` 只接受 OAuthCredential）。
        // Import catalogs cached by the pre-ModelsStore Radius implementation.
        if context.stored.is_none()
            && let Some(Credential::OAuth(credential)) = context.credential.as_ref()
        {
            let legacy = get_radius_models(&self.id, Some(credential));
            if !legacy.is_empty() {
                let dynamic_models = Arc::clone(&self.dynamic_models);
                let update_models = legacy.clone();
                let published = (context.publish)(ModelsPublication {
                    persist: ModelsPublicationPersist::Entry(ModelsStoreEntry {
                        models: legacy.into_iter().map(AnyModel::Chat).collect(),
                        checked_at: Some(now_ms()),
                        ..Default::default()
                    }),
                    update: Some(Box::new(move || {
                        set_dynamic_models(dynamic_models, update_models);
                    })),
                })
                .await?;
                if !published {
                    return Ok(());
                }
            }
        }

        // 阶段 3：联网拉取。离线刷新（allow_network=false）到此为止；`signal` 也可能在
        // 两个阶段之间被上层取消。本实现不检查 `context.force`：与 TS 的 refreshModels
        // 一致，只要进入联网阶段就重新拉取。
        if !context.allow_network || context.signal.is_aborted() {
            return Ok(());
        }
        let api_key = credential_key(context.credential.as_ref());
        let config: RadiusGatewayConfig =
            load_radius_gateway_config(&self.gateway, api_key.as_deref(), Some(&context.signal))
                .await
                .map_err(auth_error_to_ai_error)?;
        // 传输层可能无法及时中断，await 返回后必须复查，避免把过期配置写进当前代际。
        if context.signal.is_aborted() {
            return Ok(());
        }
        let refreshed = get_radius_models_from_config(&self.id, &config);
        let dynamic_models = Arc::clone(&self.dynamic_models);
        let update_models = refreshed.clone();
        // 返回值不再检查：这是阶段 3 的最后一步，即使发布被取代也没有后续动作可停；
        // 存储错误仍由 `?` 向上传播到 Models.refresh() 的 errors。
        (context.publish)(ModelsPublication {
            persist: ModelsPublicationPersist::Entry(ModelsStoreEntry {
                models: refreshed.into_iter().map(AnyModel::Chat).collect(),
                checked_at: Some(now_ms()),
                ..Default::default()
            }),
            update: Some(Box::new(move || {
                set_dynamic_models(dynamic_models, update_models);
            })),
        })
        .await?;
        Ok(())
    }

    // 请求路径不经过 `create_provider` 的 api 分派：所有模型都交给同一个
    // `kiana_messages_api()` 适配器（网关只支持 `kiana-messages` 协议）。
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        self.streams.stream(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        self.streams.stream_simple(model, context, options)
    }
}

/// Radius gateway provider with a persisted, dynamically refreshed catalog.
///
/// # 参数与默认值
///
/// - `id` 缺省 `"radius"`，`name` 缺省 `"Radius"`；
/// - `gateway` 缺省 [`DEFAULT_RADIUS_GATEWAY`]，并先经 `normalize_radius_gateway_url`
///   规范化（无 scheme 补 `https://`、去掉尾随 `/`）。
///
/// # 出厂基线
///
/// 只有规范化后的 gateway 等于默认网关时，才把 `data/radius.json` 的 chat 目录作为
/// 基线，并把每条模型的 `provider` 改写成 `id`（目录里的值固定是 `"radius"`；自定义
/// id 时必须覆盖，否则模型归属与 provider 不一致）。自定义网关的基线为空：在刷新拿到
/// 该网关的目录之前不展示任何模型（测试
/// `does_not_apply_the_public_radius_catalog_to_custom_gateways` 锁定）。
///
/// # 鉴权与流
///
/// API key 读取 `RADIUS_API_KEY`；OAuth 用懒加载闭包捕获构造时的 `name` 与 `gateway`，
/// 因此登录流程访问的正是同一个网关。所有请求走 `kiana_messages_api()`。
///
/// TS: `providers/radius.ts#radiusProvider`
pub fn radius_provider(options: RadiusProviderOptions) -> Arc<dyn Provider> {
    let id = options.id.unwrap_or_else(|| "radius".to_string());
    let name = options.name.unwrap_or_else(|| "Radius".to_string());
    let gateway =
        normalize_radius_gateway_url(options.gateway.as_deref().unwrap_or(DEFAULT_RADIUS_GATEWAY));
    // 只有默认网关才加载出厂目录；比较的是规范化后的字符串，所以带不带尾斜杠、
    // 写不写 https:// 都等价。
    let baseline_models: Vec<Model> =
        if gateway == normalize_radius_gateway_url(DEFAULT_RADIUS_GATEWAY) {
            catalog_chat_models("radius")
                .into_iter()
                .map(|model| {
                    let mut model = model.model().clone();
                    model.provider = id.clone();
                    model
                })
                .collect()
        } else {
            Vec::new()
        };
    // `lazy_oauth` 的加载闭包要求 `'static`，因此把 name/gateway 克隆进闭包；之后的
    // 刷新只读 `self.gateway`，两者在构造时已经一致。
    let oauth_name = name.clone();
    let oauth_gateway = gateway.clone();
    Arc::new(RadiusProvider {
        id,
        name,
        gateway,
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("Radius API key", &["RADIUS_API_KEY"])),
            oauth: Some(lazy_oauth(LazyOAuthInput {
                name: oauth_name.clone(),
                is_subscription: None,
                login_label: None,
                load: Arc::new(move || {
                    load_radius_oauth(RadiusOAuthOptions {
                        name: oauth_name.clone(),
                        gateway: oauth_gateway.clone(),
                    })
                }),
            })),
        },
        baseline_models,
        // 初始 None：账号目录未知，get_models 先返回出厂基线（自定义网关则为空）。
        dynamic_models: Arc::new(Mutex::new(None)),
        streams: kiana_messages_api(),
    })
}
