//! `withRemoteCatalog` / `withPersonalizedCatalog`。
//!
//! TS 源：`packages/coding-agent/src/core/remote-catalog-provider.ts`（240 行）。
//! 规范：`spec/ca-config-models.md` §2.24。
//!
//! ## 覆盖层语义
//!
//! - 合并键 `` `${getModelType(model)}\0${model.id}` ``：同键后插者覆盖，保留**首次插入位置**。
//! - 持久化条目只有比内置目录新（`lastModified > localGeneratedAt`）时才恢复/发布；
//!   `localGeneratedAt` 缺失时一律采用。
//! - `publish({update})` 是状态变更的**唯一入口**：返回 `false` 表示发布被取代，
//!   必须立即中止（不联网、不改状态）。
//! - 304 保留覆盖层只更新 `checkedAt`；404/501 视为“覆盖层不可用”并清掉 etag；
//!   其它失败保留 etag 与缓存 body 后抛错。
//! - 个性化目录（OpenRouter）：每次刷新把（内置 + 更新的 Kiana API 目录）交给 `personalize`，
//!   保存**个性化结果**；跳过刷新间隔与 etag 校验，空列表回退到内置模型。
//!
//! # 这个文件解决什么问题
//! 内置 provider 的模型目录是编译进二进制的静态快照；Kiana API 会发布更新的目录（新模型、
//! 下架旧模型、`image`/`classifier` 分片）。本文件把远端目录当作**覆盖层**叠加在静态目录上：
//! 内置条目永远保留，远端同 `(type, id)` 的条目替换它，其余追加。覆盖层通过 `ModelsStore`
//! 持久化，进程重启后先恢复缓存，再按 TTL 决定是否联网。
//!
//! 两个包装器对应两条产品路径：
//! - [`with_remote_catalog`]：普通内置 provider；带 4 小时 TTL、ETag 条件请求与
//!   `lastModified` 新旧比较。
//! - [`with_personalized_catalog`]：`ModelRuntime::create` 用它包装 `openrouter`；远端目录
//!   只是 `personalize` 回调的输入，持久化的是**按凭证个性化后的列表**。
//!
//! # 在系统中的位置
//! `crate::models::runtime` 的 `ModelRuntime::create` 为每个内置 provider 包一层：`radius`
//! 保持原样，`openrouter` 用个性化包装，其余用远程目录包装。HTTP 细节（URL、重试、超时、
//! 响应解析）在 `crate::models::catalog_http`；本文件只负责状态、缓存与合并。行为由
//! `crates/kiana-config/tests/remote_catalog_provider.rs` 锁定。
//!
//! # 刷新流程（两者都实现 [`Provider::refresh_models`]）
//! `Models` 最多分两个阶段调用同一个实现：先 `allow_network = false` 恢复缓存，再在允许
//! 联网且解析到凭证时以 `allow_network = true` 抓取。本文件的实现把“恢复”放在最前面，
//! 并且**先发布再联网**：
//! 1. 从 `context.stored` 取缓存，恢复本进程的内存覆盖层（`dynamic_models` /
//!    `personal_models`）；
//! 2. 这次发布的返回值是代际闸门：`Ok(false)` 表示已被更新一代的刷新或取消取代，立即
//!    返回 `Ok(())`，不联网、不改状态；
//! 3. 检查 `allow_network`、`signal` 与 `catalog_base_url`；远程目录还要检查 TTL 与
//!    `force`；
//! 4. 发请求并按状态码分流（304 / 404、501 / 其它非 2xx / 成功），最后再发布一次
//!    （持久化 + 更新内存）。
//!
//! # 并发与不变量
//! - 每个包装器只有一个 `Mutex<Vec<AnyModel>>` 保存覆盖层；锁只在 `get_*` 读取与发布
//!   回调的同步 `update` 里短暂持有，**不跨 `await`**。代码在锁中毒时用
//!   `PoisonError::into_inner` 继续使用内部值（被保护的数据只是一个 `Vec`）。
//! - 状态变更只能经由 `context.publish`：`update` 闭包由下层在代际仍当前、且持久化成功
//!   之后同步调用。迟到的旧请求最后一次发布会被丢弃，因此不会覆盖新目录（测试
//!   `newer_request_bypasses_a_stalled_older_request_without_stale_publication`）。
//! - `publish` 的 `Ok(false)` 在恢复阶段直接结束本次刷新；最终阶段忽略返回值——被取代时
//!   `update` 不会执行，内存状态自然保持新值。
//!
//! # 失败模式
//! - `request_catalog` 的传输/超时/取消错误以 `AiError` 传播；请求返回后本实现会再检查
//!   一次 `signal`，已中止则静默返回 `Ok(())`。取消导致的请求错误不会被吞掉（它在
//!   `?` 处已经返回）。
//! - 普通失败分支（非 2xx 且非 304/404/501）先把 `checked_at` 写回缓存（保留已有
//!   models 与 etag），再返回逐字 `Model catalog request failed for <id>: <status>`。
//! - 404/501 视为“覆盖层不可用”：`last_modified = 0`、清空 etag，但保留已缓存的 models。
//! - `read_catalog` 的解析错误与 `personalize` 的错误直接传播；个性化失败时不会保存新
//!   列表（此前恢复的旧列表仍在内存中，测试
//!   `keeps_the_saved_list_and_saves_nothing_when_personalization_fails`）。
//!
//! # 注意事项
//! - `catalog_base_url` 为 `None` 时两个包装器都不发请求，只使用内置与已缓存模型。
//! - [`REMOTE_CATALOG_MODEL_TYPES`] 只是对外的类型列表；实际请求的 `?types=` 由
//!   `catalog_http` 里的 `CATALOG_MODEL_TYPES` 拼接，改动时必须同步两处。
//! - `local_generated_at` 是内置目录的生成时间；为 `None` 时任何远端目录都视为“更新”。

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiana_ai::auth::types::Credential;
use kiana_ai::error::{AiError, AiErrorKind};
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::models::{
    ClassifierOptions, ImagesOptions, ModelsPublication, ModelsPublicationPersist,
    ModelsStoreEntry, Provider, RefreshModelsContext,
};
use kiana_ai::types::{
    AnyModel, AssistantImages, ClassifierContext, ClassifierResult, DeferredCancelOptions,
    DeferredFetchOptions, DeferredHandle, ImagesContext, Model, ModelType, ProviderHeaders,
    SimpleStreamOptions, StreamOptions, TranscriptContext,
};
use kiana_ai::utils::model_operations::is_model_type;

use crate::models::catalog_http::{model_type_key, read_catalog, request_catalog};

/// `REMOTE_CATALOG_REFRESH_INTERVAL_MS`（4 小时）。
///
/// 只被 [`with_remote_catalog`] 的刷新逻辑使用：`force` 不为 `true`、且缓存条目同时有
/// `checked_at` 与 `last_modified` 时，距上次检查不足该间隔就直接跳过网络。304 与 404/501
/// 会写回 `checked_at`（且条目带有/被写入 `last_modified`），因此会进入 TTL；普通失败也写
/// `checked_at`，但若条目没有 `last_modified`，下一次刷新仍会立即重试。
pub const REMOTE_CATALOG_REFRESH_INTERVAL_MS: i64 = 4 * 60 * 60 * 1000;
/// 本客户端能消费的模型类型（作为 `?types=` 发送）。
///
/// 这是对外的类型清单，值与 TS 的 `REMOTE_CATALOG_MODEL_TYPES` 一致
/// （`constants_match_the_typescript_source` 锁定）。**注意**：`catalog_http::request_catalog`
/// 实际拼接查询串时用的是 `catalog_http::CATALOG_MODEL_TYPES`，两个常量相互独立，修改时
/// 必须一起改。
pub const REMOTE_CATALOG_MODEL_TYPES: [ModelType; 3] =
    [ModelType::Chat, ModelType::Image, ModelType::Classifier];

/// 个性化回调的返回 future。
///
/// 用 `Pin<Box<dyn Future>>` 是因为 [`PersonalizeCatalog`] 需要作为 trait 对象存放，且
/// future 必须 `Send`（刷新过程可能被跨线程的刷新任务 await）。
pub type PersonalizeFuture = Pin<Box<dyn Future<Output = Result<Vec<AnyModel>, AiError>> + Send>>;

/// `PersonalizeCatalog = (catalog, context) => Promise<readonly AnyModel[]>`。
///
/// 第一个参数是“内置目录 + 本次抓到的远端目录”的合并结果，第二个参数是本次刷新的完整
/// 上下文（含凭证、signal 与 `publish`）。返回**完整替换列表**，不是增量。回调返回
/// `Err` 会让整次刷新失败且不保存结果；返回空列表则是合法的（保存后 `get_models()`
/// 会回退到内置目录，见 [`with_personalized_catalog`]）。
pub type PersonalizeCatalog =
    Arc<dyn Fn(Vec<AnyModel>, RefreshModelsContext) -> PersonalizeFuture + Send + Sync>;

/// `Date.now()`。
///
/// 系统时钟早于 UNIX_EPOCH 时返回 `0`（而不是 panic 或负数）。单位是毫秒，与缓存里的
/// `checked_at` / `last_modified` 一致。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

/// `mergeModels(baseline, dynamic)`：`(type, id)` 为键，dynamic 覆盖但保留首次位置。
///
/// 键的构造是 `"{type}\0{id}"`（类型字符串来自 `catalog_http::model_type_key`），因此同名
/// 但类型不同的模型是两个条目。`IndexMap::insert` 保留首次插入的下标、替换值，正好对应
/// TS `Map` 的语义；先遍历 baseline 再遍历 dynamic，所以输出顺序 = baseline 的原有顺序
/// （被覆盖者**原位**替换为 dynamic 的值）+ dynamic 中新增键的追加顺序。输入只被读取，
/// 返回新 `Vec`。
pub fn merge_models(baseline: &[AnyModel], dynamic: &[AnyModel]) -> Vec<AnyModel> {
    let mut merged: indexmap::IndexMap<String, AnyModel> = indexmap::IndexMap::new();
    for model in baseline.iter().chain(dynamic.iter()) {
        let key = format!("{}\0{}", model_type_key(model), model.model().id);
        merged.insert(key, model.clone());
    }
    merged.into_values().collect()
}

/// `isNewer(lastModified, localGeneratedAt)`。
///
/// 没有本地生成时间（`None`）时一律视为“更新”；有本地时间时，缺失的 `last_modified`
/// （`None`）不算更新。404/501 分支写入的 `0` 在配置了本地时间时同样不算更新。
fn is_newer(last_modified: Option<i64>, local_generated_at: Option<i64>) -> bool {
    match local_generated_at {
        None => true,
        Some(local) => last_modified.is_some_and(|last_modified| last_modified > local),
    }
}

/// `remoteModels(entry, localGeneratedAt)`。
///
/// 缓存条目只有在“确实比内置目录新”时才返回其模型，否则返回空列表。因此
/// `last_modified` 缺失或为 `0`（404/501 分支写入的值）的条目在配置了
/// `local_generated_at` 时不会被恢复；而 `local_generated_at` 为 `None` 时总会被恢复。
fn remote_models(
    entry: Option<&ModelsStoreEntry>,
    local_generated_at: Option<i64>,
) -> Vec<AnyModel> {
    match entry {
        Some(entry) if is_newer(entry.last_modified, local_generated_at) => entry.models.clone(),
        _ => Vec::new(),
    }
}

/// 把全类型列表投影成 chat `Model` 列表：`Chat` 原样、`Remote` 取尽力投影，其余类型丢弃。
///
/// 只用于个性化目录的 `get_models()`；远程目录包装器直接在 `AnyModel` 上合并（见其
/// `get_models` 实现）。
fn chat_models(models: Vec<AnyModel>) -> Vec<Model> {
    models
        .into_iter()
        .filter_map(|model| match model {
            AnyModel::Chat(chat) => Some(chat),
            AnyModel::Remote { model, .. } => Some(model),
            _ => None,
        })
        .collect()
}

/// 只保留 `model().provider == provider_id` 的条目（克隆）。
///
/// 用于缓存恢复与个性化目录：两者的输入都可能来自共享的 store，输出只包含本 provider 的
/// 条目。
fn provider_models(models: &[AnyModel], provider_id: &str) -> Vec<AnyModel> {
    models
        .iter()
        .filter(|model| model.model().provider == provider_id)
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// withRemoteCatalog
// ---------------------------------------------------------------------------

/// 给静态内置 provider 叠加 Kiana API 目录覆盖层。
///
/// 除 `get_models`、`get_all_models`、`refresh_models` 与 `has_get_all_models` /
/// `has_refresh_models`（恒为 `true`，不转发）外，其余方法原样委托给 `inner`；其余
/// `has_*` 谓词也逐个转发 `inner`。字段含义：
/// - `inner`：被包装的内置 provider（目录基线、auth、流实现都来自它）；
/// - `catalog_base_url`：`None` 时永不联网，只恢复缓存；
/// - `local_generated_at`：内置目录的生成时间，用于判断远端目录是否更新；
/// - `dynamic_models`：覆盖层（全类型）。`get_models` 只取其中的 chat 子集，
///   `get_all_models` 返回全部类型。刷新时整块替换，从不增量修改。
struct RemoteCatalogProvider {
    inner: Arc<dyn Provider>,
    catalog_base_url: Option<String>,
    local_generated_at: Option<i64>,
    dynamic_models: Arc<Mutex<Vec<AnyModel>>>,
}

impl std::fmt::Debug for RemoteCatalogProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteCatalogProvider")
            .field("id", &self.inner.id())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Provider for RemoteCatalogProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn base_url(&self) -> Option<&str> {
        self.inner.base_url()
    }

    fn headers(&self) -> Option<&ProviderHeaders> {
        self.inner.headers()
    }

    fn auth(&self) -> &kiana_ai::auth::types::ProviderAuth {
        self.inner.auth()
    }

    /// chat 目录 = 内置 chat + 覆盖层中的 chat 条目，按 `(type, id)` 合并。
    ///
    /// 只把 `AnyModel::Chat` 与投影类型为 chat 的 `AnyModel::Remote` 转回 `Model`（其余类型
    /// 在 `get_all_models` 里可见，但不会出现在 chat 目录）。锁只在构造 `dynamic` 时持有，
    /// 之后不再加锁。
    fn get_models(&self) -> Vec<Model> {
        let dynamic: Vec<AnyModel> = self
            .dynamic_models
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .filter(|model| is_model_type(model, ModelType::Chat))
            .cloned()
            .collect();
        merge_models(&chat_any(self.inner.get_models()), &dynamic)
            .into_iter()
            .filter_map(|model| match model {
                AnyModel::Chat(chat) => Some(chat),
                AnyModel::Remote { model, .. }
                    if model.model_type.is_none_or(|kind| kind == ModelType::Chat) =>
                {
                    Some(model)
                }
                _ => None,
            })
            .collect()
    }

    /// 全类型目录 = 内置全类型目录 + 覆盖层（全类型），合并规则同 `get_models`。
    ///
    /// 覆盖层里的 image/classifier 条目在这里可见；chat 的 `Model` 视图请用 `get_models`。
    fn get_all_models(&self) -> Vec<AnyModel> {
        let dynamic = self
            .dynamic_models
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        merge_models(&self.inner.get_all_models(), &dynamic)
    }

    fn has_get_all_models(&self) -> bool {
        true
    }

    fn has_refresh_models(&self) -> bool {
        true
    }

    /// 远程目录刷新：先恢复缓存覆盖层，再按 TTL/`force` 决定是否联网。
    ///
    /// 成功路径最后发布 `Entry`（持久化远端目录）并把 `dynamic_models` 替换为“比内置目录
    /// 新”的条目；若远端目录不比内置新，条目仍会被持久化，但覆盖层保持为空。
    async fn refresh_models(&self, context: RefreshModelsContext) -> Result<(), AiError> {
        let provider_id = self.inner.id().to_string();
        let stored = context.stored.clone();
        // 阶段 1：从持久化快照恢复覆盖层。只恢复比内置目录新的条目（`remote_models` 的
        // 语义），并按 provider 过滤。
        let restored = provider_models(
            &remote_models(stored.as_ref(), self.local_generated_at),
            &provider_id,
        );
        let dynamic = Arc::clone(&self.dynamic_models);
        // 发布 `Unchanged`（不改存储），只用 `update` 替换内存覆盖层。这一步同时是代际
        // 闸门：返回 `false` 说明本次刷新已被更新的一代或取消取代。
        let published = (context.publish)(ModelsPublication {
            persist: ModelsPublicationPersist::Unchanged,
            update: Some(Box::new(move || {
                *dynamic.lock().unwrap_or_else(|error| error.into_inner()) = restored;
            })),
        })
        .await?;
        if !published {
            return Ok(());
        }
        // 离线阶段（allow_network=false）到这里就结束；恢复已经发布。
        if !context.allow_network || context.signal.is_aborted() {
            return Ok(());
        }
        let Some(catalog_base_url) = self.catalog_base_url.as_deref() else {
            return Ok(());
        };
        // 阶段 2：决定是否真的发请求。跳过 4 小时内检查过的远端目录；没有 lastModified
        // 的条目总是重新抓取：remoteModels() 会忽略它，等待只会让覆盖层保持为空。
        if !context.force.unwrap_or(false)
            && let Some(stored) = &stored
            && stored.checked_at.is_some()
            && stored.last_modified.is_some()
            && now_ms() - stored.checked_at.unwrap_or(0) < REMOTE_CATALOG_REFRESH_INTERVAL_MS
        {
            return Ok(());
        }

        // 只有缓存 body 支撑时才发 if-none-match，避免 304 让覆盖层变空。
        let validator = match &stored {
            Some(stored) if !stored.models.is_empty() => stored.etag.as_deref(),
            _ => None,
        };
        let response =
            request_catalog(&provider_id, catalog_base_url, &context.signal, validator).await?;
        if context.signal.is_aborted() {
            return Ok(());
        }
        let checked_at = now_ms();
        let status = response.status().as_u16();
        // 状态分流（顺序即优先级）：
        // 304：缓存仍有效，只更新 checked_at；覆盖层不动。
        if status == 304
            && let Some(stored) = &stored
        {
            let mut entry = stored.clone();
            entry.checked_at = Some(checked_at);
            (context.publish)(ModelsPublication {
                persist: ModelsPublicationPersist::Entry(entry),
                update: None,
            })
            .await?;
            return Ok(());
        }
        // 404/501：服务端没有该目录或未实现。保留已缓存的 models，但把 last_modified 置 0
        // 并清空 etag：下一次条件请求不会再带旧 ETag；在配置了本地生成时间时，这份条目也
        // 不会再被恢复（`remote_models` 判为不够新）。
        if status == 404 || status == 501 {
            let mut entry = stored.clone().unwrap_or_default();
            entry.checked_at = Some(checked_at);
            entry.last_modified = Some(0);
            entry.etag = None;
            (context.publish)(ModelsPublication {
                persist: ModelsPublicationPersist::Entry(entry),
                update: None,
            })
            .await?;
            return Ok(());
        }
        // 其它非 2xx：瞬时失败的持久化语义与 404/501 不同，只刷新 checked_at，
        // 保留 models 与 etag（下次还能带 if-none-match 验证），然后把错误抛给上层。
        // 304 在 `stored` 为 None 时也会落到这里（正常流程不会发生：validator 只在有缓存
        // body 时发送）。
        if !response.status().is_success() {
            // 瞬时失败：缓存 body 与 validator 继续有效（保留 etag，下次再验证）。
            let mut entry = stored.clone().unwrap_or_default();
            entry.checked_at = Some(checked_at);
            (context.publish)(ModelsPublication {
                persist: ModelsPublicationPersist::Entry(entry),
                update: None,
            })
            .await?;
            return Err(AiError::new(
                AiErrorKind::Other,
                format!("Model catalog request failed for {provider_id}: {status}"),
            ));
        }
        // 成功响应：解析 body/头（body 被读走后无法再取状态码）。
        let catalog = read_catalog(&provider_id, response).await?;
        if context.signal.is_aborted() {
            return Ok(());
        }
        // 阶段 3：构造持久化条目。`raw_models` 保存 TS 形状的原始条目，重启后可以逐字写回；
        // 覆盖层只采用比内置目录新的模型，但**条目本身总会持久化**（测试
        // `prefers_the_newer_of_generated_and_remote_catalogs`）。
        let entry = ModelsStoreEntry {
            models: catalog.models,
            raw_models: Some(catalog.raw_models),
            last_modified: Some(catalog.last_modified),
            checked_at: Some(checked_at),
            etag: catalog.etag,
            ..ModelsStoreEntry::default()
        };
        // 覆盖层只取“比内置目录新”的条目；旧的远端目录仍写入存储，但内存里不叠加。
        let published = remote_models(Some(&entry), self.local_generated_at);
        let dynamic = Arc::clone(&self.dynamic_models);
        // 最终发布：持久化 entry 并替换覆盖层。返回值有意不检查——若发布在写盘期间被
        // 取代，`update` 不会执行（写盘可能已完成），内存状态仍是新值，不需要额外处理。
        (context.publish)(ModelsPublication {
            persist: ModelsPublicationPersist::Entry(entry),
            update: Some(Box::new(move || {
                *dynamic.lock().unwrap_or_else(|error| error.into_inner()) = published;
            })),
        })
        .await?;
        Ok(())
    }

    fn has_filter_models(&self) -> bool {
        self.inner.has_filter_models()
    }

    fn filter_models(&self, models: Vec<Model>, credential: Option<&Credential>) -> Vec<Model> {
        self.inner.filter_models(models, credential)
    }

    fn has_filter_all_models(&self) -> bool {
        self.inner.has_filter_all_models()
    }

    fn filter_all_models(
        &self,
        models: Vec<AnyModel>,
        credential: Option<&Credential>,
    ) -> Vec<AnyModel> {
        self.inner.filter_all_models(models, credential)
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        self.inner.stream(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        self.inner.stream_simple(model, context, options)
    }

    fn has_fetch_deferred(&self) -> bool {
        self.inner.has_fetch_deferred()
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: DeferredFetchOptions,
    ) -> Option<AssistantMessageEventStream> {
        self.inner.fetch_deferred(model, handle, options)
    }

    fn has_cancel_deferred(&self) -> bool {
        self.inner.has_cancel_deferred()
    }

    async fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: DeferredCancelOptions,
    ) -> Result<(), AiError> {
        self.inner.cancel_deferred(model, handle, options).await
    }

    fn has_generate_images(&self) -> bool {
        self.inner.has_generate_images()
    }

    async fn generate_images(
        &self,
        model: &Model,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> AssistantImages {
        self.inner.generate_images(model, context, options).await
    }

    fn has_classify(&self) -> bool {
        self.inner.has_classify()
    }

    async fn classify(
        &self,
        model: &Model,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> ClassifierResult {
        self.inner.classify(model, context, options).await
    }
}

/// `[Model]` → `[AnyModel]`。
///
/// 内置的 chat 基线全部标记为 `Chat`；远端条目保持 `Remote`（携带原始 JSON）。
fn chat_any(models: Vec<Model>) -> Vec<AnyModel> {
    models.into_iter().map(AnyModel::Chat).collect()
}

/// `withRemoteCatalog(provider, catalogBaseUrl?, localGeneratedAt?)`。
///
/// `catalog_base_url` 为 `None` 时不发起远端请求，只使用内置与已缓存模型。
///
/// # 参数
/// - `provider`：被包装的内置 provider；它的 `id` 同时作为缓存键与请求路径的一部分；
/// - `catalog_base_url`：catalog 服务的 base URL。构造后不可更改（包装器不提供 setter）；
/// - `local_generated_at`：内置目录的生成时间（epoch 毫秒）。为 `None` 时任何远端目录都
///   视为更新；为 `Some` 时只有 `last_modified` 更大的目录才会进入覆盖层。
///
/// # 返回
/// 一个新的 `Arc<dyn Provider>`，始终声明 `has_refresh_models`/`has_get_all_models` 为
/// `true`；构造本身不会失败，也不会发请求（请求只发生在 `refresh_models`）。
pub fn with_remote_catalog(
    provider: Arc<dyn Provider>,
    catalog_base_url: Option<&str>,
    local_generated_at: Option<i64>,
) -> Arc<dyn Provider> {
    Arc::new(RemoteCatalogProvider {
        inner: provider,
        catalog_base_url: catalog_base_url.map(str::to_string),
        local_generated_at,
        dynamic_models: Arc::new(Mutex::new(Vec::new())),
    })
}

// ---------------------------------------------------------------------------
// withPersonalizedCatalog
// ---------------------------------------------------------------------------

/// 每次网络刷新都用 credential 专属列表替换目录的 provider 包装。
///
/// 与 [`RemoteCatalogProvider`] 的关键差异：
/// - `personal_models` 非空时**完全替代**内置目录（不做合并），因此个性化结果里没有的
///   内置模型会不可见；为空时才回退到 `inner.get_all_models()`。空列表是合法的“尚无
///   个性化结果”状态（旧版本可能在目录缺失/失败后保存空列表，见测试
///   `restores_a_saved_list_as_is_and_falls_back_only_when_it_is_empty` 的注释）。
/// - 恢复缓存时**不比较** `last_modified`：`context.stored.models` 按 provider 过滤后原样
///   恢复（不经过 `is_newer` 过滤）。
/// - 联网时没有 TTL、`force` 与 ETag 逻辑：每次刷新都重新请求并重新个性化。
/// - 持久化条目只写 `models` 与 `checked_at`，不保存 `last_modified`/`raw_models`/`etag`。
struct PersonalizedCatalogProvider {
    inner: Arc<dyn Provider>,
    personalize: PersonalizeCatalog,
    catalog_base_url: Option<String>,
    local_generated_at: Option<i64>,
    personal_models: Arc<Mutex<Vec<AnyModel>>>,
}

impl std::fmt::Debug for PersonalizedCatalogProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PersonalizedCatalogProvider")
            .field("id", &self.inner.id())
            .finish_non_exhaustive()
    }
}

impl PersonalizedCatalogProvider {
    /// 内置目录的全类型视图（`inner` 的 `get_all_models`）。
    fn builtin_models(&self) -> Vec<AnyModel> {
        self.inner.get_all_models()
    }

    /// 当前对外目录：个性化列表非空时原样返回，否则回退到内置目录。
    ///
    /// 锁只在克隆期间持有；返回的是新 `Vec`。
    fn all_models(&self) -> Vec<AnyModel> {
        let personal = self
            .personal_models
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if personal.is_empty() {
            self.builtin_models()
        } else {
            personal
        }
    }
}

#[async_trait]
impl Provider for PersonalizedCatalogProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn base_url(&self) -> Option<&str> {
        self.inner.base_url()
    }

    fn headers(&self) -> Option<&ProviderHeaders> {
        self.inner.headers()
    }

    fn auth(&self) -> &kiana_ai::auth::types::ProviderAuth {
        self.inner.auth()
    }

    /// chat 目录：从当前对外目录里挑出 chat 类型并投影成 `Model`。
    ///
    /// 个性化列表存在但只含 image 模型时，这里会返回空列表（内置 chat 模型被
    /// “个性化列表是权威”的语义隐藏）。
    fn get_models(&self) -> Vec<Model> {
        chat_models(
            self.all_models()
                .into_iter()
                .filter(|model| is_model_type(model, ModelType::Chat))
                .collect(),
        )
    }

    /// 全类型目录：直接返回 `all_models()` 的结果。
    fn get_all_models(&self) -> Vec<AnyModel> {
        self.all_models()
    }

    fn has_get_all_models(&self) -> bool {
        true
    }

    fn has_refresh_models(&self) -> bool {
        true
    }

    /// 个性化目录刷新：恢复保存的列表 →（联网时）抓远端目录 → 合并内置 → 回调个性化 →
    /// 发布结果。
    ///
    /// 与远程目录包装器的差异都在这里体现：没有 TTL/ETag；404/501 不视为错误（远端目录按空
    /// 处理，仍然执行个性化）；其它非 2xx **直接返回 `Err` 且不写存储**；持久化条目只含
    /// 个性化列表与 `checked_at`。
    async fn refresh_models(&self, context: RefreshModelsContext) -> Result<(), AiError> {
        let provider_id = self.inner.id().to_string();
        // 阶段 1：原样恢复上一次保存的个性化列表。这里**不**比较 last_modified：保存的是
        // 个性化结果本身，不是远端目录条目。
        let restored = match &context.stored {
            Some(stored) => provider_models(&stored.models, &provider_id),
            None => Vec::new(),
        };
        let personal = Arc::clone(&self.personal_models);
        // 发布 `Unchanged`（不改存储），只替换内存列表；返回值是代际闸门。
        let published = (context.publish)(ModelsPublication {
            persist: ModelsPublicationPersist::Unchanged,
            update: Some(Box::new(move || {
                *personal.lock().unwrap_or_else(|error| error.into_inner()) = restored;
            })),
        })
        .await?;
        if !published {
            return Ok(());
        }
        if !context.allow_network || context.signal.is_aborted() {
            return Ok(());
        }
        let Some(catalog_base_url) = self.catalog_base_url.as_deref() else {
            return Ok(());
        };

        // 不传 validator：个性化目录不使用 ETag，也没有刷新间隔。
        let response =
            request_catalog(&provider_id, catalog_base_url, &context.signal, None).await?;
        if context.signal.is_aborted() {
            return Ok(());
        }
        // 远端目录按状态处理：404/501 当作“没有可用的远端目录”，remote 保持空列表，
        // 后面仍然会用内置目录执行个性化；其它非 2xx 直接失败，不发布也不写存储。
        let status = response.status().as_u16();
        let mut remote = Vec::new();
        if status != 404 && status != 501 {
            if !response.status().is_success() {
                return Err(AiError::new(
                    AiErrorKind::Other,
                    format!("Model catalog request failed for {provider_id}: {status}"),
                ));
            }
            let catalog = read_catalog(&provider_id, response).await?;
            // 与远程目录包装器一样：只有比内置目录新时才采用远端条目。
            if is_newer(Some(catalog.last_modified), self.local_generated_at) {
                remote = catalog.models;
            }
        }
        // 个性化回调拿到的是“内置 + 远端”的合并结果（远端同键覆盖），以及本次刷新的完整
        // 上下文。错误在这里向上传播，后面的发布不会执行。
        let personalized = (self.personalize)(
            merge_models(&self.builtin_models(), &remote),
            context.clone(),
        )
        .await?;
        if context.signal.is_aborted() {
            return Ok(());
        }
        // 持久化的是个性化结果本身，不是远端目录；不写 raw_models/last_modified/etag。
        let entry = ModelsStoreEntry {
            models: personalized.clone(),
            checked_at: Some(now_ms()),
            ..ModelsStoreEntry::default()
        };
        let personal = Arc::clone(&self.personal_models);
        // 最终发布：写存储并替换内存列表；返回值不检查（被取代时 update 不执行）。
        (context.publish)(ModelsPublication {
            persist: ModelsPublicationPersist::Entry(entry),
            update: Some(Box::new(move || {
                *personal.lock().unwrap_or_else(|error| error.into_inner()) = personalized;
            })),
        })
        .await?;
        Ok(())
    }

    fn has_filter_models(&self) -> bool {
        self.inner.has_filter_models()
    }

    fn filter_models(&self, models: Vec<Model>, credential: Option<&Credential>) -> Vec<Model> {
        self.inner.filter_models(models, credential)
    }

    fn has_filter_all_models(&self) -> bool {
        self.inner.has_filter_all_models()
    }

    fn filter_all_models(
        &self,
        models: Vec<AnyModel>,
        credential: Option<&Credential>,
    ) -> Vec<AnyModel> {
        self.inner.filter_all_models(models, credential)
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        self.inner.stream(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        self.inner.stream_simple(model, context, options)
    }

    fn has_fetch_deferred(&self) -> bool {
        self.inner.has_fetch_deferred()
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: DeferredFetchOptions,
    ) -> Option<AssistantMessageEventStream> {
        self.inner.fetch_deferred(model, handle, options)
    }

    fn has_cancel_deferred(&self) -> bool {
        self.inner.has_cancel_deferred()
    }

    async fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: DeferredCancelOptions,
    ) -> Result<(), AiError> {
        self.inner.cancel_deferred(model, handle, options).await
    }

    fn has_generate_images(&self) -> bool {
        self.inner.has_generate_images()
    }

    async fn generate_images(
        &self,
        model: &Model,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> AssistantImages {
        self.inner.generate_images(model, context, options).await
    }

    fn has_classify(&self) -> bool {
        self.inner.has_classify()
    }

    async fn classify(
        &self,
        model: &Model,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> ClassifierResult {
        self.inner.classify(model, context, options).await
    }
}

/// `withPersonalizedCatalog(provider, personalize, catalogBaseUrl?, localGeneratedAt?)`。
///
/// `catalog_base_url` 为 `None` 时不发起远端请求，也**不会调用** `personalize`；目录使用
/// 内置模型或上一次刷新恢复出来的缓存列表。
///
/// # 参数
/// - `provider`：被包装的内置 provider（`openrouter`），其 `id` 用于请求与缓存归属；
/// - `personalize`：每次网络刷新都会调用的回调，返回完整的替换列表；同一个 `Arc` 被所有
///   刷新共享；
/// - `catalog_base_url`：catalog 服务 base URL；`None` 时跳过网络与个性化，只恢复缓存；
/// - `local_generated_at`：内置目录生成时间；只在决定是否采用远端目录时使用。
///
/// # 返回
/// 新的 `Arc<dyn Provider>`，始终声明 `has_refresh_models`/`has_get_all_models` 为 `true`；
/// 构造不会失败、不发请求。
pub fn with_personalized_catalog(
    provider: Arc<dyn Provider>,
    personalize: PersonalizeCatalog,
    catalog_base_url: Option<&str>,
    local_generated_at: Option<i64>,
) -> Arc<dyn Provider> {
    Arc::new(PersonalizedCatalogProvider {
        inner: provider,
        personalize,
        catalog_base_url: catalog_base_url.map(str::to_string),
        local_generated_at,
        personal_models: Arc::new(Mutex::new(Vec::new())),
    })
}
