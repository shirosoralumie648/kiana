//! Provider trait 与 CreateProviderOptions。
//!
//! TS: `src/models.ts` 的 `Provider` 接口与 `createProvider()`。Rust 侧：
//! - [`Provider`] 是对象安全的 trait（`Arc<dyn Provider>` 在 `Models` 集合里保存）；
//! - [`ProviderStreams`] / [`ProviderImages`] / [`ProviderClassifier`] 对应 `types.ts` 的
//!   同名接口（API 实现模块的统一契约，`createProvider` 按 `model.api` 分派）；
//! - [`create_provider`] 复刻动态模型合并、按 api 分发与「至少一个实现」的校验。
//!
//! 与 TS 的差异（均因 Rust 的类型系统）：
//! - TS 的 `getModels` 抛错在 Rust 里不可能表达（方法签名不返回 `Result`）；坏 provider
//!   由 `get_models` 的实现自己决定（`Models` 仍用 `catch_unwind` 复刻 TS 的 try/catch）。
//! - TS 用 `method !== undefined` 探测可选方法；Rust 的 trait 方法恒存在，因此：
//!   * **行为等价**的三处（`getAllModels`、`filterModels`、`filterAllModels`）由默认实现
//!     直接复刻 TS 的 `??` 回退（默认 `get_all_models` == chat 列表、默认 `filter_models`
//!     == 原样返回、默认 `filter_all_models` == TS 的 `filterModels` 回退），`Models`
//!     **不再**用谓词分支，任何实现覆盖方法即生效；
//!   * 其余可选操作（`refreshModels`、`fetchDeferred`、`cancelDeferred`、`generateImages`、
//!     `classify`）的 `has_*` 谓词默认 `true`，让「只覆盖操作、不覆盖谓词」的实现也能被调用；
//!     对应默认方法返回 TS 里「方法不存在」时的错误/空结果。唯一的残差是 `Models` 的
//!     预检查顺序（TS 在鉴权前判定能力缺失，Rust 在鉴权后由默认方法报同一错误）。
//!   * `ProviderStreams` 的 `has_fetch_deferred` / `has_cancel_deferred` 只声明「provider 级
//!     能力」（TS 的 `streams.some(entry => entry.fetchDeferred !== undefined)`，用于区分
//!     「for api」错误与通用错误）；覆盖了 `fetch_deferred` / `cancel_deferred` 的实现即使
//!     不覆盖谓词也会在调用时被分派。
//! - TS 的 `createProvider` 对空实现集合抛普通 `Error`；Rust 返回 `Err(AiError)`，
//!   错误消息沿用既定文本（外部契约）。
//!
//! # 这个文件负责什么
//!
//! 本文件是 provider 层的运行时核心，同时承担三件事：
//! - 定义 api 适配器实现的三个契约：[`ProviderStreams`]（chat 流）、[`ProviderImages`]
//!   （图像生成）、[`ProviderClassifier`]（结构化分类）；
//! - 定义上层 `Models` 只依赖的 [`Provider`] 对象安全 trait；
//! - 用 [`create_provider`] 把「静态基线模型 + 动态目录 + 按 `model.api` 分派的实现」
//!   组装成一个 provider。
//!
//! # 谁在调用
//!
//! - 构造：`providers/*.rs` 的内置工厂与测试直接调用 [`create_provider`]；配置/会话层
//!   拿到的是这些 provider 再包装后的对象（例如 `kiana-config` 的 composer）；
//! - 使用：`models::ModelsImpl` 持有 `Arc<dyn Provider>`，用它列目录、过滤、刷新和分派
//!   请求；下游 crate 也会直接包装 provider（见 [`Provider`] 的 `has_*` 谓词说明）。
//!
//! # 主要流程（`CreatedProvider`）
//!
//! 1. 构造：[`create_provider`] 统计三类实现的数量，全为 0 返回 `Err`；把静态模型存为
//!    基线，动态模型置空，并**快照**各实现声明的 deferred 能力。
//! 2. 读目录：`get_models()` / `get_all_models()` 每次调用都重新合并「基线 + 动态覆盖
//!    层」，因此刷新后不需要失效任何缓存。
//! 3. 刷新：`refresh_models` 先恢复持久化缓存（只提交内存更新），再在允许联网时抓取
//!    目录并把结果写回存储；两次提交都走 `context.publish`。
//! 4. 分派：`stream`/`stream_simple` 按 `model.api` 找 chat 实现，找不到返回惰性错误流；
//!    deferred/图像/分类在 provider 完全没有对应实现时，返回与 TS「方法不存在」相同的
//!    错误或错误结果。
//!
//! # 为什么拆成三个 trait
//!
//! [`Provider`] 是给 `Models` 用的聚合视图，而 [`ProviderStreams`] /
//! [`ProviderImages`] / [`ProviderClassifier`] 是 api 适配器实际实现的能力切片：一个
//! 适配器可以只做 chat，也可以同时提供图像或分类。`create_provider` 允许三者任意组合
//! （至少一个），再按 `model.api` 选择对应切片；这样同一个 provider 能同时暴露多种
//! 模型类型，而不要求每个适配器实现全部方法。
//!
//! # 并发与状态
//!
//! - [`Provider`] 要求 `Send + Sync`，`Models` 用 `Arc` 跨任务共享，所有方法都取
//!   `&self`；实现不能依赖独占访问。
//! - `CreatedProvider` 唯一的可变状态是 `dynamic_models`（`Arc<Mutex<Vec<AnyModel>>>`），
//!   锁只覆盖这一次 `Vec` 的读写；一次发布是否生效由 `Models` 侧的代际与 per-provider
//!   发布锁决定（见 `models/mod.rs` 的 `publish_provider_models`），不由这把锁决定。
//! - `supports_fetch_deferred` / `supports_cancel_deferred` 是构造时的快照：实现对象在
//!   构造之后改变 `has_*` 的返回值，不会影响已经建好的 provider。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::auth::types::{Credential, ProviderAuth};
use crate::error::{AiError, AiErrorKind, ModelsErrorCode};
use crate::event_stream::AssistantMessageEventStream;
use crate::types::{
    AnyModel, ClassifierContext, ClassifierResult, DeferredCancelOptions, DeferredFetchOptions,
    DeferredHandle, ImagesContext, Model, ModelType, ProviderHeaders, SimpleStreamOptions,
    StreamOptions, TranscriptContext,
};
use crate::utils::diagnostics::ThrownValue;
use crate::utils::model_operations::{
    classifier_error_result, get_model_type, image_error_result, is_model_type,
};
use crate::utils::models_error::{TsModelsErrorCode, models_error};

use super::{ModelsPublication, RefreshModelsContext};

/// TS 的 `ImagesOptions`（`ProviderRequestOptions + metadata`）；
/// Rust 的 [`StreamOptions`] 已包含同名字段（含 `metadata`），因此直接复用。
pub type ImagesOptions = StreamOptions;

/// TS 的 `ClassifierOptions`（`ProviderRequestOptions + temperature`）；
/// [`StreamOptions`] 已包含 `temperature`，因此直接复用。
pub type ClassifierOptions = StreamOptions;

/// 统一的 chat 流实现契约。
///
/// TS: `src/types.ts#ProviderStreams`
///
/// 由 `api/*` 的适配器实现：`create_provider` 要么把单个实现用于全部 chat 模型，要么
/// 按 `model.api` 组成索引。`stream`/`stream_simple` 必须**同步返回**事件流——实现通常
/// 在后台任务里发请求，把网络/协议错误作为流上的 `error` 事件推送；调用方拿到流即可
/// 开始读，不需要等待响应。
///
/// `has_fetch_deferred`/`has_cancel_deferred` 的默认值 `false` 表示「该 api 适配器没有
/// 声明此能力」，`create_provider` 用它们汇总 provider 级能力。注意与 [`Provider`] 上
/// 同名谓词的默认值 `true` 不同：那边是为了让「只覆盖操作、不覆盖谓词」的 provider 实现
/// 仍能被调用。
// `async_trait` 展开出的方法自带 `#[must_use]`；与 `auth/types.rs` 的处理一致。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait ProviderStreams: Send + Sync {
    /// 流式请求（完整选项）。
    ///
    /// 输入是已归一化的对话上下文、目标模型和完整请求选项；立即返回可读的事件流。
    /// 返回是同步的：具体请求通常在实现的后台任务里进行；同步阶段的配置错误也应通过
    /// [`crate::api::lazy::lazy_stream_error`] 之类的方式变成流上的 `error` 事件，而不是
    /// panic（本 trait 的返回类型没有 `Result` 通道）。
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream;

    /// 流式请求（简单选项）。
    ///
    /// 与 [`ProviderStreams::stream`] 相同，只是额外接受 `tool_choice`/`reasoning`/
    /// `deferred`/`thinking_budgets` 等便捷字段（见 `SimpleStreamOptions`）；适配器负责
    /// 把它们映射成自己的请求参数。
    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream;

    /// 是否声明了 `fetch_deferred`（TS 的 `fetchDeferred !== undefined`）。
    ///
    /// 仅用于 `create_provider` 区分 TS 的两条错误路径（provider 级不支持 vs 某个 api
    /// 不支持）；**不**决定调用是否发生——覆盖了 [`ProviderStreams::fetch_deferred`] 的实现
    /// 即使不覆盖本谓词也会被调用。
    fn has_fetch_deferred(&self) -> bool {
        false
    }

    /// 延迟响应轮询；`None` 表示该实现不支持。
    ///
    /// `handle` 携带第一次请求返回的 provider token（响应 id / batch id 等），
    /// `options.wait` 是长轮询上限（缺省只查一次状态）。返回的流与
    /// [`ProviderStreams::stream`] 同语义：轮询结果（含最终消息）通过事件推送。
    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: DeferredFetchOptions,
    ) -> Option<AssistantMessageEventStream> {
        let _ = (model, handle, options);
        None
    }

    /// 是否声明了 `cancel_deferred`（语义同 [`ProviderStreams::has_fetch_deferred`]）。
    fn has_cancel_deferred(&self) -> bool {
        false
    }

    /// 取消延迟响应；`None` 表示该实现不支持。
    ///
    /// 与外层 [`Provider::cancel_deferred`] 的区别：这里用 `Option` 让聚合层区分「该 api
    /// 不提供取消」与「取消失败」，前者在 provider 级存在取消能力声明时转成带 `for "api"`
    /// 的错误，否则转成通用 `does not support deferred responses`；后者原样返回 `Err`。
    async fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: DeferredCancelOptions,
    ) -> Option<Result<(), AiError>> {
        let _ = (model, handle, options);
        None
    }
}

/// 统一的图像生成实现契约。
///
/// TS: `src/types.ts#ProviderImages`
///
/// 返回 `AssistantImages` 而不是 `Result`：失败时用 `stop_reason = Error`（中止时为
/// `Aborted`）加 `error_message` 表达，对应 TS 的「实现不得 reject」。调用方是
/// `Models.generateImages`，它会把任何 `Err` 也包成同形的错误结果。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait ProviderImages: Send + Sync {
    /// 生成图像。TS 的 `Promise<AssistantImages>`；实现不得 reject。
    ///
    /// `options` 为 `None` 表示调用方没有传入额外选项；需要中止信号/自定义 HTTP 客户端
    /// 的实现应从 `options` 里读取。
    async fn generate_images(
        &self,
        model: &Model,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> crate::types::AssistantImages;
}

/// 统一的结构化分类实现契约。
///
/// TS: `src/types.ts#ProviderClassifier`
///
/// 与 [`ProviderImages`] 相同：错误编码在 `ClassifierResult` 的 `stop_reason` /
/// `error_message` 里，而不是返回 `Result`。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait ProviderClassifier: Send + Sync {
    /// 分类。TS 的 `Promise<ClassifierResult>`；实现不得 reject。
    ///
    /// `context` 包含状态、可选图片和问题定义；`options` 为 `None` 表示没有额外选项。
    async fn classify(
        &self,
        model: &Model,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> ClassifierResult;
}

/// `CreateProviderOptions.api` 的两种形态：单一实现，或按 `model.api` 的映射。
///
/// TS 用「`api.stream` 是不是函数」区分两者；Rust 用显式枚举。
///
/// `Single` 不检查 `model.api`，所有 chat 模型都走同一个实现；`ByApi` 用 `model.api`
/// 精确查找（区分大小写），未命中时 `stream`/`stream_simple` 返回错误流。
#[derive(Clone)]
pub enum ProviderApiImpl {
    /// 所有 chat 模型共用一个实现。
    Single(Arc<dyn ProviderStreams>),
    /// 按 `model.api` 分派。
    ByApi(indexmap::IndexMap<String, Arc<dyn ProviderStreams>>),
}

/// 让工厂可以写 `api: Some(openai_completions_api().into())`，等价于
/// `ProviderApiImpl::Single(...)`。
impl From<Arc<dyn ProviderStreams>> for ProviderApiImpl {
    fn from(value: Arc<dyn ProviderStreams>) -> Self {
        ProviderApiImpl::Single(value)
    }
}

/// 动态模型抓取函数（TS `fetchModels(context)`）。
///
/// 只在 `refresh_models` 的联网阶段调用，参数带凭证、`allow_network`、`force`、取消信号
/// 与 `base_url`。返回的是**完整目录快照**（不是增量）；未被中止的 `Err` 会被
/// `Models.refresh()` 记入该 provider 的 `errors`（按 provider id 记录；中止引发的错误
/// 会被丢弃），不会中断其他 provider。
pub type FetchModelsFn = Arc<
    dyn Fn(
            RefreshModelsContext,
        ) -> futures::future::BoxFuture<'static, Result<Vec<AnyModel>, AiError>>
        + Send
        + Sync,
>;

/// `filterModels(models, credential)`。
///
/// 输入是 provider 自己的 chat 目录（即 `get_models()` 的结果），返回按当前凭证判定为
/// 可用的子集；`credential` 为 `None` 表示没有可用凭证。未设置时 `filter_models` 原样
/// 返回。结果还会被默认 `filter_all_models` 当作「可用 chat id 集合」使用。
pub type FilterModelsFn = Arc<dyn Fn(Vec<Model>, Option<&Credential>) -> Vec<Model> + Send + Sync>;

/// `filterAllModels(models, credential)`。
///
/// 输入/输出都是全类型模型；设置后会**替代**默认回退逻辑（默认逻辑见
/// `default_filter_all_models`）。非 chat 模型是否会因凭证被过滤完全由实现决定。
pub type FilterAllModelsFn =
    Arc<dyn Fn(Vec<AnyModel>, Option<&Credential>) -> Vec<AnyModel> + Send + Sync>;

/// Provider 的静态/动态模型与操作实现。
///
/// TS: `src/models.ts#CreateProviderOptions`
///
/// [`create_provider`] 的唯一输入。至少要有 `api`、`images`、`classifiers` 中的一项
/// （实现数为 0 视为没有）；`models` 是静态基线，`fetch_models` 存在时 provider 才会
/// 参与 `Models.refresh()` 的动态目录刷新。
#[derive(Clone, Default)]
pub struct CreateProviderOptions {
    /// Provider id。
    pub id: String,
    /// 展示名；缺省等于 `id`。
    pub name: Option<String>,
    /// 默认 base URL。
    pub base_url: Option<String>,
    /// provider 级默认请求头。
    pub headers: Option<ProviderHeaders>,
    /// auth 方法。构造期不校验，缺失的 auth 由后续 `Models.login` 等路径报错。
    pub auth: ProviderAuth,
    /// 静态基线模型（所有类型；不带 `type` 的是 chat）。
    ///
    /// 动态刷新结果按 `(type, id)` 覆盖同一条目，不会删除基线；基线中未被覆盖的模型
    /// 始终可见。
    pub models: Vec<AnyModel>,
    /// 动态模型覆盖层抓取函数。
    ///
    /// 存在时 `has_refresh_models` 为 `true`（TS 的 `refreshModels` 只在有 `fetchModels`
    /// 时安装）；为 `None` 的 provider 是静态目录，`Models.refresh()` 不会调用它。
    pub fetch_models: Option<FetchModelsFn>,
    /// credential 相关的 chat 模型可用性过滤。
    ///
    /// 未设置时 `has_filter_models` 为 `false`、过滤为恒等函数；`Models.getAvailable()`
    /// 直接调用 `filter_models()`，因此覆盖即生效。
    pub filter_models: Option<FilterModelsFn>,
    /// credential 相关的全类型可用性过滤。
    ///
    /// 设置后 `filter_all_models()` 直接使用它；未设置时走默认回退
    /// `default_filter_all_models`（chat 按 `filter_models(get_models())` 的 id 集合过滤，
    /// 非 chat 原样保留）。
    pub filter_all_models: Option<FilterAllModelsFn>,
    /// chat 实现（单一或按 api）。
    ///
    /// `None` 表示没有 chat 流能力；此时 `images`/`classifiers` 至少要有一个，否则
    /// [`create_provider`] 返回 `Err`。
    pub api: Option<ProviderApiImpl>,
    /// 图像实现（按 `model.api`）。
    ///
    /// 键必须与 `model.api` 逐字相同；空 map 视为没有图像能力（`has_generate_images`
    /// 为 `false`）。
    pub images: Option<indexmap::IndexMap<String, Arc<dyn ProviderImages>>>,
    /// 分类实现（按 `model.api`）。
    ///
    /// 键必须与 `model.api` 逐字相同；空 map 视为没有分类能力（`has_classify` 为
    /// `false`）。
    pub classifiers: Option<indexmap::IndexMap<String, Arc<dyn ProviderClassifier>>>,
}

/// 一个具体的 provider 运行时单元。
///
/// TS: `src/models.ts#Provider`。方法是同步的读访问 + 异步的刷新/延迟/图像/分类操作；
/// `stream`/`stream_simple` 与 TS 一样**同步返回**流。
///
/// # 实现契约
///
/// - 对象安全：所有方法都不带泛型、取 `&self`，因此可以存成 `Arc<dyn Provider>`。
/// - `Send + Sync`：`Models` 会把同一个 provider 共享给并发任务，`get_models` /
///   `get_all_models` / `stream` 等可能在任意线程被调用。
/// - `get_models`/`get_all_models` 必须同步返回。panic 会被 `Models` 的 `catch_unwind`
///   捕获：查询单个 provider 时得到空列表，查询全部时该 provider 被跳过；但实现不应
///   依赖这个兜底。
/// - `stream`/`stream_simple` 也同步返回流，真正的请求工作由返回的流承担。
///
/// # `has_*` 谓词的含义
///
/// TS 用「方法是否为 `undefined`」表达能力，Rust 的 trait 方法恒存在，于是用一组
/// `has_*` 谓词承载同一信息。分派规则分两类：
///
/// - 本 crate 的 `Models` 会读取：`has_refresh_models`（决定 `refresh()` 是否包含该
///   provider）、`has_fetch_deferred`/`has_cancel_deferred`（决定错误是「provider 不
///   支持」还是「for api」）、`has_generate_images`/`has_classify`（决定「不支持」错误
///   的发出时机与文案）。
/// - `Models` 不读取，供包装/组合 provider 判断内层是否真的提供该操作：
///   `has_get_all_models`、`has_filter_models`、`has_filter_all_models`。例如
///   `kiana-session-core` 的 virtual provider 会转发 `has_get_all_models`，
///   `kiana-config` 的 remote-catalog/composer 会用 `has_filter_*` 决定过滤策略。
///   默认实现已复刻 TS 的回退语义，所以直接调用方法本身也等价。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait Provider: Send + Sync {
    /// Provider id。
    fn id(&self) -> &str;
    /// 展示名。
    fn name(&self) -> &str;
    /// 默认 base URL。
    fn base_url(&self) -> Option<&str> {
        None
    }
    /// provider 级默认请求头。
    fn headers(&self) -> Option<&ProviderHeaders> {
        None
    }
    /// auth 方法。
    fn auth(&self) -> &ProviderAuth;

    /// 当前已知的 chat 模型（同步；不得抛错/panic）。
    ///
    /// 主要调用方是 `Models.get_models()` 与默认 `filter_all_models`（后者把这里的 id
    /// 集合当作「可用 chat 目录」）。
    fn get_models(&self) -> Vec<Model>;

    /// 当前已知的全部类型模型（默认由 chat 模型组成）。
    ///
    /// 默认实现把 `get_models()` 的每个模型包装为 `AnyModel::Chat`；`Models` 直接调用
    /// 本方法（不再看 `has_get_all_models`），因此覆盖即生效。
    ///
    /// 注意目录一致性：默认 `filter_all_models` 只用 `get_models()` 的 id 集合判断 chat
    /// 模型是否可用，若本方法返回了 `get_models()` 之外的 chat 模型，它们会被默认过滤掉
    /// （测试 `default_filter_all_models_derives_available_ids_from_the_chat_catalog`
    /// 锁定了该行为）。
    fn get_all_models(&self) -> Vec<AnyModel> {
        self.get_models().into_iter().map(AnyModel::Chat).collect()
    }

    /// 是否实现了 `getAllModels`（TS 的 `provider.getAllModels !== undefined`）。
    ///
    /// TS 用 `getAllModels?.() ?? getModels()` 区分「未实现」与「实现但返回空数组」；
    /// Rust 的默认 `get_all_models` 就是 TS 的回退（`getModels()` 的 chat 列表），因此
    /// [`Models`](super::Models) 直接调用 `get_all_models()`，语义与 TS 的 `??` 相同。
    /// 该谓词保留给实现者表达意图，**不再**参与 `Models` 的分派（默认 `false`）；包装
    /// provider 仍可用它判断是否需要把目录委托给内层（见 trait 文档）。
    fn has_get_all_models(&self) -> bool {
        false
    }

    /// 是否实现了 `refreshModels`（TS 的 `provider.refreshModels !== undefined`）。
    ///
    /// 默认 `true`：覆盖了 [`Provider::refresh_models`] 的实现即使不覆盖本谓词也会被
    /// `Models.refresh()` 调用。默认的 `refresh_models` 是什么都不做，因此未覆盖该操作的
    /// 实现只在 refresh 时多一次空的缓存/凭证读取。
    fn has_refresh_models(&self) -> bool {
        true
    }

    /// 动态 provider：恢复缓存并（在允许联网时）抓取新目录。
    ///
    /// `Models` 的刷新协调最多调用本方法两次：先以 `allow_network = false` 调一次恢复
    /// 持久化缓存（此阶段不解析新凭证），再在允许联网且解析到凭证时以
    /// `allow_network = true` 调一次做网络抓取。实现必须遵守：
    ///
    /// - 状态更新只能放进 [`ModelsPublication`] 的 `update` 闭包，通过 `context.publish`
    ///   提交；`context.publish(...).await?` 返回 `Ok(false)` 表示该次发布已被取代或
    ///   取消，此时不应再继续工作；返回 `Err(AiError)` 表示存储/发布任务失败，应继续
    ///   向上传播。
    /// - `context.signal` 中止后不应再发起新工作（网络实现可能无法中断，但返回前要检查）。
    /// - 未被中止的 `Err` 会被 `Models.refresh()` 记入该 provider 的 `errors`（中止引发
    ///   的错误会被丢弃），不会中断其他 provider。
    async fn refresh_models(&self, context: RefreshModelsContext) -> Result<(), AiError> {
        let _ = context;
        Ok(())
    }

    /// 是否实现了 `filterModels`（TS 的 `provider.filterModels !== undefined`）。
    ///
    /// 默认 `filter_models` 原样返回，与 TS 的 `filterModels?.(...) ?? models` 等价；`Models`
    /// 直接调用 `filter_models()`，因此该谓词只表达意图，**不再**参与 `Models` 的分派；
    /// 包装 provider 仍用它判断是否把过滤委托给内层。
    fn has_filter_models(&self) -> bool {
        false
    }

    /// credential 相关的 chat 模型可用性过滤。
    ///
    /// `models` 是 provider 自己的 chat 目录；`credential` 为 `None` 表示当前没有可用
    /// 凭证。默认实现原样返回（恒等函数），因此「未实现」与「实现但全部放行」对调用方
    /// 没有区别。
    fn filter_models(&self, models: Vec<Model>, credential: Option<&Credential>) -> Vec<Model> {
        let _ = credential;
        models
    }

    /// 是否实现了 `filterAllModels`（TS 的 `provider.filterAllModels !== undefined`）。
    ///
    /// 默认 `filter_all_models` 即 TS 在 `filterAllModels` 缺失时的回退；`Models` 直接调用
    /// `filter_all_models()`，因此该谓词只表达意图，**不再**参与 `Models` 的分派；包装
    /// provider 仍用它判断是否把全类型过滤委托给内层。
    fn has_filter_all_models(&self) -> bool {
        false
    }

    /// credential 相关的全类型可用性过滤（默认：chat 走 `filterModels`，其余保留）。
    ///
    /// TS: `Models.getAllAvailable` 的 `if (!provider.filterModels) return models;` 之后的分支。
    /// 默认 `filter_models` 是恒等函数，因此「未实现 filterModels」与「实现了但原样返回」
    /// 在这里得到相同结果，不需要谓词判断。详尽的默认逻辑见
    /// `default_filter_all_models`。
    fn filter_all_models(
        &self,
        models: Vec<AnyModel>,
        credential: Option<&Credential>,
    ) -> Vec<AnyModel> {
        default_filter_all_models(self, models, credential)
    }

    /// 流式请求（`Models` 已归一化 context 并解析 auth）。
    ///
    /// 调用前 `Models` 已完成：确认模型是 chat、找到 provider、解析凭证，并把解析结果
    /// 写进请求参数（凭证里的 base URL 覆盖 `model.base_url`；会话头、auth 头、调用方头
    /// 依次合并进 `options.headers`；env/identity 同理）。实现只需按 `model.api` 发请求，
    /// 同步返回事件流。
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream;

    /// 流式请求（简单选项）。
    ///
    /// 语义同 [`Provider::stream`]，区别只在 `SimpleStreamOptions` 携带
    /// `tool_choice`/`reasoning`/`deferred`/`thinking_budgets` 等便捷字段。
    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream;

    /// 是否实现了 `fetchDeferred`（TS 的 `provider.fetchDeferred !== undefined`）。
    ///
    /// 默认 `true`：覆盖了 [`Provider::fetch_deferred`] 的实现即使不覆盖本谓词也会被
    /// `Models.streamDeferred` 分派。默认方法返回 `None`，`Models` 据此给出与 TS
    /// 「方法不存在」相同的 `ModelsError`（provider 代码）。
    fn has_fetch_deferred(&self) -> bool {
        true
    }

    /// 延迟响应轮询；未实现时 `None`。
    ///
    /// 返回 `None` 表示该 provider 没有延迟响应能力；`Models.streamDeferred` 会把它
    /// 转成 `provider` 代码的 `ModelsError`。`CreatedProvider` 用 `None` 与带 api 名的
    /// 错误流区分「provider 级不支持」和「该 api 不支持」。
    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: DeferredFetchOptions,
    ) -> Option<AssistantMessageEventStream> {
        let _ = (model, handle, options);
        None
    }

    /// 是否实现了 `cancelDeferred`（TS 的 `provider.cancelDeferred !== undefined`）。
    ///
    /// 默认 `true`，理由同 [`Provider::has_fetch_deferred`]。
    fn has_cancel_deferred(&self) -> bool {
        true
    }

    /// 取消延迟响应；未实现时返回与 TS `Models.cancelDeferred` 相同的 `ModelsError`。
    async fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: DeferredCancelOptions,
    ) -> Result<(), AiError> {
        let _ = (handle, options);
        Err(super::ai_error_from_models_error(models_error(
            TsModelsErrorCode::Provider,
            &format!(
                "Provider {} does not support deferred responses",
                model.provider
            ),
            None,
        )))
    }

    /// 是否实现了 `generateImages`（TS 的 `provider.generateImages !== undefined`）。
    ///
    /// 默认 `true`，理由同 [`Provider::has_fetch_deferred`]。
    fn has_generate_images(&self) -> bool {
        true
    }

    /// 生成图像；未实现时返回与 TS `Models.generateImages` 相同的错误结果。
    ///
    /// 默认实现的错误结果读 `options.signal` 判断是否已中止（`stop_reason` 在
    /// `Aborted` 与 `Error` 之间切换），消息为
    /// `Provider {model.provider} does not support image generation`。注意默认
    /// `has_generate_images` 是 `true`：未覆盖 `generate_images`（能力缺失）的实现会通过
    /// `Models` 的检查，并在鉴权后由这里的默认实现报错（模块文档列出的残差）；覆盖了
    /// `generate_images` 但不覆盖谓词的实现则直接执行覆盖版本，不会进入本默认实现。
    async fn generate_images(
        &self,
        model: &Model,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> crate::types::AssistantImages {
        let _ = context;
        let aborted = options
            .and_then(|options| options.signal.as_ref())
            .is_some_and(tokio_util::sync::CancellationToken::is_cancelled);
        image_error_result(
            model,
            &ThrownValue::Text(format!(
                "Provider {} does not support image generation",
                model.provider
            )),
            aborted,
        )
    }

    /// 是否实现了 `classify`（TS 的 `provider.classify !== undefined`）。
    ///
    /// 默认 `true`，理由同 [`Provider::has_fetch_deferred`]。
    fn has_classify(&self) -> bool {
        true
    }

    /// 分类；未实现时返回与 TS `Models.classify` 相同的错误结果。
    ///
    /// 与 [`Provider::generate_images`] 同构：错误编码在 `ClassifierResult` 的
    /// `stop_reason`/`error_message`，中止时 `stop_reason` 为 `Aborted`。默认
    /// `has_classify` 为 `true`：未覆盖 `classify`（能力缺失）的实现会通过 `Models` 的
    /// 检查，并在鉴权后由这里的默认实现报错；覆盖了 `classify` 但不覆盖谓词的实现则直接
    /// 执行覆盖版本，不会进入本默认实现。
    async fn classify(
        &self,
        model: &Model,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> ClassifierResult {
        let _ = context;
        let aborted = options
            .and_then(|options| options.signal.as_ref())
            .is_some_and(tokio_util::sync::CancellationToken::is_cancelled);
        classifier_error_result(
            model,
            &ThrownValue::Text(format!(
                "Provider {} does not support classification",
                model.provider
            )),
            aborted,
        )
    }
}

/// TS 的 `getModelType(model)`：没有 `type` 的是 chat。
///
/// `AnyModel` 的变体优先（`Chat`/`Image`/`Classifier`），`Remote` 变体则读内层
/// `Model.model_type`，缺省为 chat（见 `AnyModel::model_type`）。
fn known_model_type(model: &AnyModel) -> ModelType {
    get_model_type(model)
}

/// `hasKnownModelType(model)`：本版本认识的三种类型。
///
/// 注意：`AnyModel` 有 `Chat`/`Image`/`Classifier`/`Remote` 四个变体，但 [`ModelType`]
/// 只有三个值（`Remote` 的 `model_type()` 也只会归一到这三者之一），所以这里的
/// `matches!` 恒为 `true`。保留该函数是为了对齐 TS 的过滤点：`refresh_models` 与
/// `models/mod.rs` 的 `withKnownModelTypes` 都保留了「丢弃更新版本才认识的类型」这一步，
/// Rust 侧目前不会真的丢弃任何模型。
fn has_known_model_type(model: &AnyModel) -> bool {
    matches!(
        known_model_type(model),
        ModelType::Chat | ModelType::Image | ModelType::Classifier
    )
}

/// `Date.now()`：Unix 毫秒时间戳。
///
/// 系统时钟早于 1970 时返回 `0`（`map_or` 的兜底）；`as_millis()` 是 `u128`，返回值
/// 截断为 `i64`（`ModelsStoreEntry.checked_at` 是 `Option<i64>`）。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

/// 由 `create_provider` 构造的 provider。
///
/// 字段基本是构造时的快照，只有 `dynamic_models` 会在 `refresh_models` 中被整体替换；
/// 对外目录 = `baseline_models` + `dynamic_models` 的合并结果（见
/// `current_models`）。
struct CreatedProvider {
    id: String,
    name: String,
    base_url: Option<String>,
    headers: Option<ProviderHeaders>,
    auth: ProviderAuth,
    /// 静态基线，来自 `CreateProviderOptions.models`，刷新不会改动它。
    baseline_models: Vec<AnyModel>,
    /// 动态覆盖层：刷新成功后整体替换。用 `Mutex` 让 `&self` 下的读取/替换都安全，
    /// 锁中毒时用 `into_inner()` 取回内部值（目录只是一个 `Vec`，不会因 panic 处于
    /// 半更新状态）。
    dynamic_models: Arc<Mutex<Vec<AnyModel>>>,
    /// 抓取函数；同时决定 `has_refresh_models` 的返回值。
    fetch_models: Option<FetchModelsFn>,
    filter_models_fn: Option<FilterModelsFn>,
    filter_all_models_fn: Option<FilterAllModelsFn>,
    /// chat 实现（单一或按 api）；`None` 表示没有 chat 流能力。
    api: Option<ProviderApiImpl>,
    /// api → 图像实现；空 map 与 `None` 在能力判定上都表示「没有」。
    images: Option<indexmap::IndexMap<String, Arc<dyn ProviderImages>>>,
    /// api → 分类实现；空 map 与 `None` 在能力判定上都表示「没有」。
    classifiers: Option<indexmap::IndexMap<String, Arc<dyn ProviderClassifier>>>,
    /// 至少一个 chat 实现（TS 的 `streams.length > 0`）。
    has_stream_implementations: bool,
    /// 有实现声明了 `fetchDeferred`（决定「for api」错误 vs 通用错误）。
    supports_fetch_deferred: bool,
    /// 有实现声明了 `cancelDeferred`。
    supports_cancel_deferred: bool,
}

impl CreatedProvider {
    /// `currentModels()`：基线浅拷贝 + 动态覆盖（按 (type, id) 原位替换，否则追加）。
    ///
    /// 合并语义是「覆盖」而不是「替换整个目录」：基线中有、动态结果没有的模型仍然保留；
    /// 动态新增的模型追加到末尾。基线的相对顺序不变，命中替换的条目不移动位置。
    /// 锁只覆盖这次合并；中毒时取回内部值继续，避免一次 panic 让目录永久不可读。
    fn current_models(&self) -> Vec<AnyModel> {
        let mut merged = self.baseline_models.clone();
        let dynamic = self
            .dynamic_models
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for model in dynamic.iter() {
            let target = (known_model_type(model), model.model().id.as_str());
            let index = merged.iter().position(|entry| {
                known_model_type(entry) == target.0 && entry.model().id == target.1
            });
            match index {
                Some(index) => merged[index] = model.clone(),
                None => merged.push(model.clone()),
            }
        }
        merged
    }

    /// `apiFor(model)`。
    ///
    /// `Single` 不检查 `model.api`（所有 chat 模型共用它）；`ByApi` 用 `model.api`
    /// 精确查找（区分大小写），未命中返回 `None`；`stream`/`stream_simple` 将其转成
    /// `no_api_stream` 错误流，`fetch_deferred`/`cancel_deferred` 分别转成
    /// `unsupported_fetch_deferred`（可能为 `None`）与 `unsupported_cancel_deferred` 的
    /// `Err`。
    fn api_for(&self, model: &Model) -> Option<&Arc<dyn ProviderStreams>> {
        match &self.api {
            Some(ProviderApiImpl::Single(single)) => Some(single),
            Some(ProviderApiImpl::ByApi(by_api)) => by_api.get(&model.api),
            None => None,
        }
    }

    /// provider 没有对应 api 的 chat 实现时返回的错误流。
    ///
    /// TS 的 `dispatch` 用 `lazyStream(... throw new ModelsError("stream", ...))`：错误码
    /// 是 `stream`（不是 `provider`），错误在流的 setup 阶段出现而不是同步返回。这里用
    /// `lazy_stream_error` 复刻，消息中的 provider 取 `self.id`（错误文案是外部契约，
    /// 见 `spec/ai-core.md` §A.8 第 4 条）。
    fn no_api_stream(&self, model: &Model) -> AssistantMessageEventStream {
        let error = models_error(
            TsModelsErrorCode::Stream,
            &format!(
                "Provider {} has no API implementation for \"{}\"",
                self.id, model.api
            ),
            None,
        );
        super::lazy_stream_error(
            model,
            AiError::new(AiErrorKind::Other, error.message)
                .with_name("ModelsError")
                .with_detail(serde_json::json!({ "code": error.code.as_str() })),
        )
    }
}

// 手写 `Debug`：`dyn ProviderStreams` / `dyn ProviderImages` / `dyn ProviderClassifier`
// 这些 trait 对象没有实现 `Debug`，派生会编译失败。这里只打印 `id`，其余字段由
// `finish_non_exhaustive()` 显示为 `..`。
impl std::fmt::Debug for CreatedProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreatedProvider")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Provider for CreatedProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    fn headers(&self) -> Option<&ProviderHeaders> {
        self.headers.as_ref()
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        // 每次调用都从 current_models() 现算，所以刷新后无需维护任何缓存。
        self.current_models()
            .into_iter()
            .filter(|model| is_model_type(model, ModelType::Chat))
            .map(|model| model.model().clone())
            .collect()
    }

    fn get_all_models(&self) -> Vec<AnyModel> {
        self.current_models()
    }

    fn has_get_all_models(&self) -> bool {
        // `createProvider` 总会安装 `getAllModels`（TS 侧恒为函数）。
        true
    }

    fn has_refresh_models(&self) -> bool {
        // TS 的 `refreshModels` 只在有 `fetchModels` 时安装：没有抓取函数的 provider
        // 是静态目录，`Models.refresh()` 不应把它算作 refreshable。
        self.fetch_models.is_some()
    }

    async fn refresh_models(&self, context: RefreshModelsContext) -> Result<(), AiError> {
        // 防御性分支：`has_refresh_models` 已经保证有 fetch_models；保留它是为了让直接
        // 调用（绕过 Models）时也安全。
        let Some(fetch_models) = self.fetch_models.clone() else {
            return Ok(());
        };
        // 阶段 1：恢复持久化缓存。只取属于本 provider 的条目，通过 `publish` 的
        // `update` 写进动态覆盖层；`Unchanged` 表示「只更新内存、不回写存储」，存储里
        // 的 etag/lastModified 等字段保持原样。
        // `publish` 返回 Ok(false) = 本次发布已被取代或已取消，此时应停止后续工作；
        // 返回 Err(AiError) = 存储/发布任务失败，由下面的 `?` 继续向上传播。
        if let Some(stored) = &context.stored {
            let restored: Vec<AnyModel> = stored
                .models
                .iter()
                .filter(|model| model.model().provider == self.id)
                .cloned()
                .collect();
            let dynamic_models = Arc::clone(&self.dynamic_models);
            let published = (context.publish)(ModelsPublication {
                persist: super::ModelsPublicationPersist::Unchanged,
                update: Some(Box::new(move || {
                    *dynamic_models.lock().unwrap_or_else(|e| e.into_inner()) = restored;
                })),
            })
            .await?;
            if !published {
                return Ok(());
            }
        }
        // 阶段 2：网络抓取。离线刷新（allow_network=false）到此为止；`signal` 可能由
        // 上层在两次阶段之间取消。
        if !context.allow_network || context.signal.is_aborted() {
            return Ok(());
        }
        let fetched = fetch_models(context.clone()).await?;
        // 抓取函数可能不响应取消信号，因此 await 之后必须重新检查，避免把过期结果
        // 写进当前代际。
        if context.signal.is_aborted() {
            return Ok(());
        }
        // `has_known_model_type` 当前恒为 true（ModelType 只有三种值），保留过滤点以
        // 对齐 TS 的 `withKnownModelTypes`。
        let refreshed: Vec<AnyModel> = fetched.into_iter().filter(has_known_model_type).collect();
        // `update` 闭包要求 `'static`，持久化条目又需要同一份数据，因此克隆一次。
        let dynamic_models = Arc::clone(&self.dynamic_models);
        let to_store = refreshed.clone();
        (context.publish)(ModelsPublication {
            // 写入的条目只带 models 与 checkedAt，其余字段为默认值（TS 的 persist 对象
            // 也只有这两个字段）；返回值不再检查：这里已是本阶段的最后一步。
            persist: super::ModelsPublicationPersist::Entry(super::ModelsStoreEntry {
                models: refreshed,
                checked_at: Some(now_ms()),
                ..Default::default()
            }),
            update: Some(Box::new(move || {
                *dynamic_models.lock().unwrap_or_else(|e| e.into_inner()) = to_store;
            })),
        })
        .await?;
        Ok(())
    }

    fn has_filter_models(&self) -> bool {
        // 对应 TS 的 `provider.filterModels !== undefined`：只有配置了过滤函数才为 true。
        // 未配置时 `filter_models` 是恒等函数，但谓词仍为 false，包装 provider 据此走
        // 「不委托过滤」的分支。
        self.filter_models_fn.is_some()
    }

    fn filter_models(&self, models: Vec<Model>, credential: Option<&Credential>) -> Vec<Model> {
        match &self.filter_models_fn {
            Some(filter) => filter(models, credential),
            None => models,
        }
    }

    fn has_filter_all_models(&self) -> bool {
        // 默认回退（`default_filter_all_models`）也会过滤 chat，但谓词只表达「显式配置了
        // filterAllModels」（TS 的 `!== undefined` 语义）：包装 provider 用它在「委托内层
        // 过滤」与「用自己的回退」之间选择，默认回退的存在不会让谓词变成 true。
        self.filter_all_models_fn.is_some()
    }

    fn filter_all_models(
        &self,
        models: Vec<AnyModel>,
        credential: Option<&Credential>,
    ) -> Vec<AnyModel> {
        match &self.filter_all_models_fn {
            Some(filter) => filter(models, credential),
            None => default_filter_all_models(self, models, credential),
        }
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        // 无实现时不 panic、也不返回 Result：统一返回惰性错误流（错误码 stream）。
        match self.api_for(model) {
            Some(streams) => streams.stream(model, context, options),
            None => self.no_api_stream(model),
        }
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        match self.api_for(model) {
            Some(streams) => streams.stream_simple(model, context, options),
            None => self.no_api_stream(model),
        }
    }

    fn has_fetch_deferred(&self) -> bool {
        // TS: `streams.some(entry => entry.fetchDeferred !== undefined)` 时才安装
        // provider.fetchDeferred。Rust 无法探测覆盖，因此只要存在 chat 实现就让 `Models`
        // 先走鉴权再分派：覆盖了 `fetch_deferred` 的实现一定被调用；没覆盖时由
        // `fetch_deferred` 返回 `None`（通用错误）或「for api」错误流。
        self.has_stream_implementations
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: DeferredFetchOptions,
    ) -> Option<AssistantMessageEventStream> {
        // 两层回退：api 实现返回 None（未覆盖）与 api 缺失（`api_for` 为 None）都交给
        // `unsupported_fetch_deferred`，由构造时快照的 provider 级能力决定是「通用错误」
        // （返回 None）还是「for "api"」错误流。
        let implementation = self.api_for(model);
        match implementation {
            Some(implementation) => match implementation.fetch_deferred(model, handle, options) {
                Some(stream) => Some(stream),
                None => self.unsupported_fetch_deferred(model),
            },
            None => self.unsupported_fetch_deferred(model),
        }
    }

    fn has_cancel_deferred(&self) -> bool {
        // 与 `has_fetch_deferred` 同理：只要有 chat 实现就让 `Models` 先鉴权再分派，
        // 覆盖了 `cancel_deferred` 的实现一定能被调用。
        self.has_stream_implementations
    }

    async fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: DeferredCancelOptions,
    ) -> Result<(), AiError> {
        // `Option<Result<..>>` 的 `None` 表示「该 api 没实现取消」，转成错误；`Some`
        // 则原样返回实现自己的结果（成功或失败）。
        let implementation = self.api_for(model);
        match implementation {
            Some(implementation) => {
                match implementation.cancel_deferred(model, handle, options).await {
                    Some(result) => result,
                    None => Err(self.unsupported_cancel_deferred(model)),
                }
            }
            None => Err(self.unsupported_cancel_deferred(model)),
        }
    }

    fn has_generate_images(&self) -> bool {
        // `None` 与空 map 都表示没有图像能力（TS 的 `imageImplementations.length > 0`）。
        self.images
            .as_ref()
            .is_some_and(|images| !images.is_empty())
    }

    async fn generate_images(
        &self,
        model: &Model,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> crate::types::AssistantImages {
        let Some(images) = &self.images else {
            // TS 里没有 images 实现时 provider 根本没有 `generateImages`；直接调用本方法
            // 等价于 `Models.generateImages` 的能力缺失错误。
            let aborted = options
                .and_then(|options| options.signal.as_ref())
                .is_some_and(tokio_util::sync::CancellationToken::is_cancelled);
            return image_error_result(
                model,
                &ThrownValue::Text(format!(
                    "Provider {} does not support image generation",
                    model.provider
                )),
                aborted,
            );
        };
        match images.get(&model.api) {
            Some(implementation) => {
                implementation
                    .generate_images(model, context, options)
                    .await
            }
            // provider 有图像能力、但该 api 没有实现。TS 在 createProvider 层用
            // `imageErrorResult(model, new ModelsError("provider", ...))`，第三个参数
            // `aborted` 缺省为 false（这里同样传 false）：该错误不是「中止」。
            None => image_error_result(
                model,
                &ThrownValue::Error(crate::types::DiagnosticErrorInfo {
                    name: Some("ModelsError".into()),
                    message: format!(
                        "Provider {} has no image generation implementation for \"{}\"",
                        self.id, model.api
                    ),
                    stack: None,
                    code: None,
                }),
                false,
            ),
        }
    }

    fn has_classify(&self) -> bool {
        // `None` 与空 map 都表示没有分类能力。
        self.classifiers
            .as_ref()
            .is_some_and(|classifiers| !classifiers.is_empty())
    }

    async fn classify(
        &self,
        model: &Model,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> ClassifierResult {
        let Some(classifiers) = &self.classifiers else {
            // 同 `generate_images`：TS 里没有实现时方法不存在。
            let aborted = options
                .and_then(|options| options.signal.as_ref())
                .is_some_and(tokio_util::sync::CancellationToken::is_cancelled);
            return classifier_error_result(
                model,
                &ThrownValue::Text(format!(
                    "Provider {} does not support classification",
                    model.provider
                )),
                aborted,
            );
        };
        match classifiers.get(&model.api) {
            Some(implementation) => implementation.classify(model, context, options).await,
            None => classifier_error_result(
                model,
                &ThrownValue::Error(crate::types::DiagnosticErrorInfo {
                    name: Some("ModelsError".into()),
                    message: format!(
                        "Provider {} has no classifier implementation for \"{}\"",
                        self.id, model.api
                    ),
                    stack: None,
                    code: None,
                }),
                false,
            ),
        }
    }
}

impl CreatedProvider {
    /// 把错误包成惰性错误流：错误作为流的 `error` 事件出现，而不是同步返回。
    fn deferred_error_stream(&self, model: &Model, message: &str) -> AssistantMessageEventStream {
        super::lazy_stream_error(model, self.provider_models_error(message))
    }

    /// `fetch_deferred` 找不到实现时的结果。
    ///
    /// TS：provider 级没有 `fetchDeferred`（没有任何实现声明支持）时 `Models.streamDeferred`
    /// 抛通用错误；provider 级有、但这个 api 没有时抛「... for "api"」。Rust 的 `None`
    /// 让 `Models` 产生通用错误（同样的文案与 code `provider`），`Some` 则是带 api 名
    /// 的错误流。
    fn unsupported_fetch_deferred(&self, model: &Model) -> Option<AssistantMessageEventStream> {
        if !self.supports_fetch_deferred {
            return None;
        }
        Some(self.deferred_error_stream(
            model,
            &format!(
                "Provider {} does not support deferred responses for \"{}\"",
                self.id, model.api
            ),
        ))
    }

    /// `cancel_deferred` 找不到实现时的错误（分支语义同
    /// [`CreatedProvider::unsupported_fetch_deferred`]）。
    ///
    /// 注意两段文案是外部契约（改动需同步测试）：provider 级有支持但 api 缺失时是
    /// `cannot cancel deferred responses for "api"`；provider 级完全没有支持时是通用的
    /// `does not support deferred responses`（与 `Models.cancelDeferred` 的预检查同文案）。
    fn unsupported_cancel_deferred(&self, model: &Model) -> AiError {
        if self.supports_cancel_deferred {
            return self.provider_models_error(&format!(
                "Provider {} cannot cancel deferred responses for \"{}\"",
                self.id, model.api
            ));
        }
        super::ai_error_from_models_error(models_error(
            TsModelsErrorCode::Provider,
            &format!(
                "Provider {} does not support deferred responses",
                model.provider
            ),
            None,
        ))
    }

    /// 构造 `name = "ModelsError"`、`detail.code = "provider"` 的 [`AiError`]。
    ///
    /// 与 `models::ai_error_from_models_error` 的约定一致：`name` 表示错误类型，
    /// `detail.code` 保存 TS 的 `error.code`；`AiErrorKind::Other` 是因为
    /// `ModelsErrorCode::Provider` 不属于鉴权类错误。
    fn provider_models_error(&self, message: &str) -> AiError {
        AiError::new(AiErrorKind::Other, message.to_owned())
            .with_name("ModelsError")
            .with_detail(serde_json::json!({ "code": ModelsErrorCode::Provider.as_str() }))
    }
}

/// `Models.getAllAvailable` 的默认 `filterModels` 回退逻辑（trait 默认
/// [`Provider::filter_all_models`] 与 `CreatedProvider` 共用）。
///
/// TS: `ModelsImpl.getAllAvailable` 的 `if (!provider.filterModels) return models;` 之后分支。
/// 可用 chat id 集合来自 `provider.getModels()`（provider 的 chat 目录），不是被过滤的
/// `getAllModels()` 列表；后者可能包含 `getModels()` 里没有的 chat 模型，TS 会把它们过滤掉。
/// 因为默认 `filter_models` 是恒等函数，这里无需谓词判断。
///
/// 只过滤 chat：image/classifier 模型不会因为不在 chat 目录里而被删掉。输出保持输入
/// 顺序；id 去重只用 `HashSet` 做成员判断，不改变模型对象本身。
fn default_filter_all_models<P: Provider + ?Sized>(
    provider: &P,
    models: Vec<AnyModel>,
    credential: Option<&Credential>,
) -> Vec<AnyModel> {
    let available_ids: std::collections::HashSet<String> = provider
        .filter_models(provider.get_models(), credential)
        .into_iter()
        .map(|model| model.id)
        .collect();
    models
        .into_iter()
        .filter(|model| {
            !is_model_type(model, ModelType::Chat) || available_ids.contains(&model.model().id)
        })
        .collect()
}

/// 构建 provider（内置 provider 工厂与 models.json 自定义 provider 共用）。
///
/// 实现集合为空时返回 `Err`；错误消息是外部契约（改动需同步测试）。
///
/// 校验只覆盖「至少一个实现」：`auth` 是否配置、模型列表是否为空都不在这里检查
/// （缺失的 auth 由后续 `Models.login` 等路径报错）。`api` 为 `ByApi` 时按
/// map 的条目数计（空 map 等于 0），`Single` 恒计 1。成功时返回 `Arc<dyn Provider>`，
/// 可直接交给 `Models.set_provider`。
///
/// # 步骤
///
/// 1. 统计 `api`/`images`/`classifiers` 的实现数，全 0 返回 `Err`；
/// 2. 收集 chat 实现，并快照 `has_fetch_deferred`/`has_cancel_deferred`；
/// 3. `name` 缺省回退到 `id`，静态模型进入基线、动态模型置空。
///
/// 能力快照只在构造时计算一次；实现若在构造后改变谓词的返回值，provider 级能力判定
/// 不会跟着变。
///
/// TS: `src/models.ts#createProvider`
// TS 的 `throw new Error` 对应 `AiError`；冻结的错误类型较大，按既有约定关闭 lint。
#[allow(clippy::result_large_err)]
pub fn create_provider(input: CreateProviderOptions) -> Result<Arc<dyn Provider>, AiError> {
    // TS 的 `api` 是「单一函数」或「api → 实现」的 map 二选一；空 map 与未提供都算 0。
    let stream_count = match &input.api {
        None => 0,
        Some(ProviderApiImpl::Single(_)) => 1,
        Some(ProviderApiImpl::ByApi(by_api)) => by_api.len(),
    };
    let image_count = input.images.as_ref().map_or(0, |images| images.len());
    let classifier_count = input
        .classifiers
        .as_ref()
        .map_or(0, |classifiers| classifiers.len());
    // 普通 `Error`（不是 ModelsError）：TS 这里 throw new Error，code 不存在；
    // `AiErrorKind::Other` + 原样消息即其等价物。
    if stream_count == 0 && image_count == 0 && classifier_count == 0 {
        return Err(AiError::new(
            AiErrorKind::Other,
            format!(
                "Provider {}: at least one of \"api\", \"images\", or \"classifiers\" is required.",
                input.id
            ),
        ));
    }

    // TS 的 `streams.some((entry) => entry.fetchDeferred !== undefined)`：`has_*` 只声明
    // 「provider 级能力」（用于区分「for api」与通用错误）；方法的覆盖本身在调用时生效。
    // 注意这里是**构造时快照**，之后实现对象改变谓词返回值不会更新它。
    let implementations: Vec<&Arc<dyn ProviderStreams>> = match &input.api {
        Some(ProviderApiImpl::Single(single)) => vec![single],
        Some(ProviderApiImpl::ByApi(by_api)) => by_api.values().collect(),
        None => Vec::new(),
    };
    let has_stream_implementations = !implementations.is_empty();
    let supports_fetch_deferred = implementations
        .iter()
        .any(|implementation| implementation.has_fetch_deferred());
    let supports_cancel_deferred = implementations
        .iter()
        .any(|implementation| implementation.has_cancel_deferred());

    // TS: `input.name ?? input.id`。
    let name = input.name.clone().unwrap_or_else(|| input.id.clone());
    let provider = CreatedProvider {
        id: input.id,
        name,
        base_url: input.base_url,
        headers: input.headers,
        auth: input.auth,
        baseline_models: input.models,
        dynamic_models: Arc::new(Mutex::new(Vec::new())),
        fetch_models: input.fetch_models,
        filter_models_fn: input.filter_models,
        filter_all_models_fn: input.filter_all_models,
        api: input.api,
        images: input.images,
        classifiers: input.classifiers,
        has_stream_implementations,
        supports_fetch_deferred,
        supports_cancel_deferred,
    };
    Ok(Arc::new(provider))
}

/// `hasApi(model, api)`：仅 chat 模型匹配。
///
/// 非 chat 模型即使 `api` 相同也不匹配（TS 的 type guard 行为）。`api` 按字符串精确
/// 比较，区分大小写。
///
/// TS: `src/models.ts#hasApi`
pub fn has_api(model: &AnyModel, api: &str) -> bool {
    is_model_type(model, ModelType::Chat) && model.model().api == api
}

/// `modelsAreEqual(a, b)`：类型、id、provider 三者相等；任一为空返回 false。
///
/// `None` 对应 TS 的 `null`/`undefined`：任一为 `None` 都返回 `false`，即使两边都是
/// `None`（TS 的 falsy 语义）。只比较 `ModelType`/`id`/`provider`，其它字段（base URL、
/// 价格等）不影响结果。调用方（配置解析、模型选择器、扩展 API）用它判断两个模型引用
/// 是否指向同一个模型。
///
/// TS: `src/models.ts#modelsAreEqual`
pub fn models_are_equal(a: Option<&AnyModel>, b: Option<&AnyModel>) -> bool {
    let (Some(a), Some(b)) = (a, b) else {
        return false;
    };
    known_model_type(a) == known_model_type(b)
        && a.model().id == b.model().id
        && a.model().provider == b.model().provider
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Model, ModelType};

    /// `getAllModels()` 里带有一个 `getModels()` 没有的 chat 模型（目录不一致的 provider）。
    struct InconsistentDirectoryProvider {
        auth: ProviderAuth,
    }

    impl InconsistentDirectoryProvider {
        fn model(id: &str) -> Model {
            Model {
                id: id.into(),
                provider: "inconsistent".into(),
                ..Model::default()
            }
        }
    }

    #[async_trait]
    impl Provider for InconsistentDirectoryProvider {
        fn id(&self) -> &str {
            "inconsistent"
        }

        fn name(&self) -> &str {
            "Inconsistent"
        }

        fn auth(&self) -> &ProviderAuth {
            &self.auth
        }

        fn get_models(&self) -> Vec<Model> {
            vec![Self::model("visible"), Self::model("hidden")]
        }

        fn get_all_models(&self) -> Vec<AnyModel> {
            vec![
                AnyModel::Chat(Self::model("visible")),
                AnyModel::Chat(Self::model("extra")),
                AnyModel::Image(Model {
                    model_type: Some(ModelType::Image),
                    ..Self::model("image")
                }),
            ]
        }

        fn filter_models(
            &self,
            models: Vec<Model>,
            _credential: Option<&Credential>,
        ) -> Vec<Model> {
            models
                .into_iter()
                .filter(|model| model.id != "hidden")
                .collect()
        }

        fn stream(
            &self,
            _model: &Model,
            _context: &TranscriptContext,
            _options: StreamOptions,
        ) -> AssistantMessageEventStream {
            AssistantMessageEventStream::new()
        }

        fn stream_simple(
            &self,
            _model: &Model,
            _context: &TranscriptContext,
            _options: SimpleStreamOptions,
        ) -> AssistantMessageEventStream {
            AssistantMessageEventStream::new()
        }
    }

    /// 默认 `filter_all_models` 的可用 chat id 集合必须来自 `get_models()`（TS:
    /// `provider.filterModels(provider.getModels(), credential)`），而不是被过滤的
    /// `get_all_models()` 列表本身。
    #[test]
    fn default_filter_all_models_derives_available_ids_from_the_chat_catalog() {
        let provider = InconsistentDirectoryProvider {
            auth: ProviderAuth::default(),
        };
        let filtered: Vec<String> = provider
            .filter_all_models(provider.get_all_models(), None)
            .into_iter()
            .map(|model| model.model().id.clone())
            .collect();
        // availableChatIds = filterModels([visible, hidden]) = {visible}，因此只在
        // getAllModels() 中出现的 chat 模型 "extra" 被过滤，image 模型保留。
        assert_eq!(filtered, ["visible", "image"]);
    }
}
