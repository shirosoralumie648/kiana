//! models.ts / models-store.ts / models.generated.ts（`model-catalog.ts` 在 crate 根的 `model_catalog`）。
//!
//! `Models` 集合、`createModels`、`createProvider`、`calculateCost`、thinking level 的
//! 运行时核心。逐文件对应：
//! - [`cost`]：`calculateCost`；
//! - [`provider`]：`Provider` / `createProvider` / `hasApi` / `modelsAreEqual`；
//! - [`store`]：`ModelsStore` / `InMemoryModelsStore`；
//! - [`thinking`]：`getSupportedThinkingLevels` / `clampThinkingLevel`；
//! - 本文件：`Models` / `MutableModels` / `ModelsImpl` / `createModels` 与刷新协调算法。
//!
//! TS 的 `AbortSignal` 在本 crate 统一为 [`AbortScope`]；
//! `Promise` 拒绝为 `Result<_, AiError>` / `Result<_, AuthError>`（auth 路径保留
//! `error.name`/`code`，见 `auth/resolve.rs` 的约定）。
//!
//! # 这个文件负责什么
//!
//! TS 的 `models.ts` 是「运行时 provider 注册表 + 请求门面」：上层代码通常先创建一个
//! `Models` 对象、注册 provider，再按 provider/model 发请求。Rust 侧由 `ModelsImpl` 承担
//! 同一职责，可以分成四块：
//!
//! 1. **注册与查找**：`providers` 是 `id -> Arc<dyn Provider>` 的插入序表；读访问器同步
//!    返回，内部用 `std::sync::Mutex` 保护，临界区只做克隆、不跨 await。
//! 2. **鉴权**：`apply_auth` 在每次请求前解析凭证（OAuth 可刷新、API key 可来自 env），
//!    按顺序合并 session 头、provider auth 头、请求头与 `transformHeaders`，再填好 env 与
//!    客户端身份，最后交给 provider 适配器。
//! 3. **refresh 协调**：动态 provider（声明了 `refreshModels` 的）可以联网拉取模型目录；
//!    本文件用「代际（generation）+ 每 provider 一个中止 scope + 每 provider 一把发布锁」
//!    保证过期的 `update` 不执行、同一 provider 的发布串行化；若取代发生在存储写入期间，
//!    旧条目可能已落盘（随后被新一代覆盖），不会被同步进内存。
//! 4. **请求门面**：`stream*` 同步返回流，setup 的同步前缀在返回前执行、其余由后台任务
//!    继续；`generate_images` / `classify` 把一切错误折叠进返回值（不返回 `Err`），与 TS 的
//!    catch 分支一致。
//!
//! # 一次典型请求（stream 路径）
//!
//! 1. 调用方 `create_models(...)` 得到 `Arc<dyn MutableModels>`，用 `set_provider` 注册
//!    provider（对应 TS 的 `models.setProvider`）。
//! 2. `stream(model, context, options)` 同步执行 `normalizeContext` 的等价物
//!    `normalize_context` 与 telemetry 观察器构造，并同步 poll setup 一次（见下一步），
//!    随后立即返回 [`AssistantMessageEventStream`]。
//! 3. 返回前的同步 poll 让 setup 的同步前缀立即执行：`require_chat_provider`（未知 provider
//!    与非 chat 模型在这里失败）与 `apply_auth`（解析凭证、合并头/env/identity）的发起；
//!    异步的鉴权与 provider 调用（`provider.stream(...)`）由后台任务继续推进，不依赖调用方
//!    是否轮询外层流。
//! 4. setup 或 provider 抛出的错误由 [`lazy_stream`] 转成一条
//!    `stopReason = "error"` 的 assistant 消息事件，而不是让调用方拿到 `Err`。
//!
//! # 状态与并发约定
//!
//! - `ModelsState` 的 `providers`、`refresh_generations`、`refresh_controllers`、
//!   `publication_locks` 各有一把 `std::sync::Mutex`，全部只在**不跨 await** 的短临界区内
//!   使用（例外：`refresh_models` 在持有 `providers` 锁时调用 `Provider` 的只读谓词
//!   `has_refresh_models`/`id()`——实现不得回调 `Models`，否则可能重入同把锁）。锁中毒被
//!   显式容忍（`unwrap_or_else(|e| e.into_inner())`），poison 之后的数据仍可继续读取和修改。
//! - 同一 provider 的模型发布由 `tokio::sync::Mutex` 串行化，发布锁会跨存储 I/O（
//!   `models_store.write`/`delete` 的 `.await`）持有；`publish` 返回的 future 在存在
//!   tokio 运行时时会 `spawn` 出去，调用方即使丢弃它，持久化与内存更新也会完成（详见
//!   `run_provider_refresh_phase` 内的说明）。
//! - 取消是协作式的：`AbortScope` 只让等待方尽早返回，已经开始的凭证刷新或发布不会被强杀，
//!   只会在下一个检查点发现中止并放弃后续步骤。
//!
//! # 错误约定
//!
//! - 同步访问器从不返回错误；`get_models`/`get_all_models`（及其派生的 `get_model*`/
//!   `get_models_of_type`）用 `catch_unwind` 把 provider 的 panic 视为「无模型」（复刻 TS 的
//!   try/catch）。`get_available*` 直接调用 provider，不做这层保护。
//! - 查询与刷新返回 `Result<_, AiError>`；鉴权方法返回 `Result<_, AuthError>`。错误到
//!   `AiError` 的转换见 `ai_error_from_models_error` / `ai_error_from_auth_error`：`name`
//!   （`"ModelsError"` 或原始 `AuthError.name`）与 `detail.code` 都会被保留。
//! - `generate_images` / `classify` 是例外：所有失败都折叠进 `AssistantImages` /
//!   `ClassifierResult` 的 `stop_reason` 与 `error_message`，调用方永远拿到值而非 `Err`。
//!
//! # 阅读顺序
//!
//! 先读 [`Models`] trait 了解对外契约，再看 `ModelsImpl` 的 `new` 与 `ModelsState` 字段，
//! 然后按「访问器 → `apply_auth` → `stream`/`complete` → `generate_images`/`classify` →
//! `refresh` 协调」的顺序读实现。刷新算法是全文件最复杂的部分，涉及 `refresh_models`、
//! `begin_provider_refresh`、`publish_provider_models`、`run_provider_refresh_phase`、
//! `resolve_refresh_credential` 五个私有函数的协作。

// --- 子模块 ---
pub mod cost;
pub mod generated;
pub mod provider;
pub mod store;
pub mod thinking;

pub use crate::error::{ModelsError, ModelsErrorCode};
pub use crate::telemetry::AI_TELEMETRY_SCHEMA;
pub use crate::utils::model_operations::{get_model_type, is_model_type};
pub use cost::calculate_cost;
pub use provider::{
    ClassifierOptions, CreateProviderOptions, FetchModelsFn, FilterAllModelsFn, FilterModelsFn,
    ImagesOptions, Provider, ProviderApiImpl, ProviderClassifier, ProviderImages, ProviderStreams,
    create_provider, has_api, models_are_equal,
};
pub use store::{InMemoryModelsStore, ModelsStore, ModelsStoreEntry, ModelsStoreOperationOptions};
pub use thinking::{EXTENDED_THINKING_LEVELS, clamp_thinking_level, get_supported_thinking_levels};

use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::future::BoxFuture;
use indexmap::IndexMap;

use crate::api::lazy::{LazySetup, lazy_stream, lazy_stream_error};
use crate::auth::context::default_provider_auth_context;
use crate::auth::credential_store::InMemoryCredentialStore;
use crate::auth::resolve::{
    AuthResolutionOverrides, refresh_stored_oauth_credential, resolve_provider_auth,
};
use crate::auth::types::{
    ApiKeyAuthInput, ApiKeyCredential, AuthCheck, AuthContext, AuthError, AuthInteraction,
    AuthOperationOptions, AuthResult, AuthType, Credential, CredentialStore, LoginOptions,
    OAuthCredential, throw_if_aborted,
};
use crate::error::{AiError, AiErrorKind};
use crate::event_stream::AssistantMessageEventStream;
use crate::telemetry::observe_model_request;
use crate::types::ClientIdentity;
use crate::types::DiagnosticErrorInfo;
use crate::types::{
    AnyModel, AssistantImages, AssistantMessage, ClassifierContext, ClassifierResult, Context,
    DeferredCancelOptions, DeferredFetchOptions, DeferredHandle, ImagesContext, Model, ModelType,
    SimpleStreamOptions, StreamOptions, ThinkingLevel,
};
use crate::utils::abort::{AbortReason, AbortScope, operation_signal, race_with_abort_signal};
use crate::utils::abort_signals::combine_abort_signals;
use crate::utils::diagnostics::ThrownValue;
use crate::utils::identity::merge_client_identity;
use crate::utils::model_operations::image_error_result;
use crate::utils::models_error::{TsModelsErrorCode, models_error};
use crate::utils::transcript::normalize_context;

/// 一次 provider 目录发布的持久化策略。
///
/// TS: `ModelsPublication.persist` 的三态：缺省（不改动存储）/ `null`（删除）/ entry（写入）。
///
/// 三态由 provider 的 `refresh_models` 通过 [`RefreshModelsContext::publish`] 提交；只有发布
/// 真正执行到写盘步骤时才会调用 [`ModelsStore`]。写盘前后各有一次代际/取消检查：写盘前
/// 被丢弃的发布不触碰存储；若在写盘期间被取代，写盘可能已完成，但 `update` 不会执行
/// （`publish` 返回 `false`）。
#[derive(Default)]
pub enum ModelsPublicationPersist {
    /// 缺省：不改动存储。
    #[default]
    Unchanged,
    /// `persist: null`：删除持久化条目。
    Delete,
    /// 写入条目（异步写盘前会克隆，调用方之后修改原值不影响本次发布）。
    Entry(ModelsStoreEntry),
}

/// 同步更新内存状态的闭包（`ModelsPublication.update`）。
///
/// 拥有所有权并且只运行一次：只有在该次发布未被取消、代际仍当前、持久化步骤无错误时
/// （`Unchanged` 不写盘，直接进入这一步）才会调用。
pub type PublicationUpdateFn = Box<dyn FnOnce() + Send + 'static>;

/// provider 选择的一次发布。
///
/// TS: `src/models.ts#ModelsPublication`
///
/// 不变量：`update` 至多执行一次；执行前必须同时满足「signal 未中止」与「generation 仍是
/// 当前代际」，因此它看到的内存状态与本次发布的 persist 结果一致（`Entry`/`Delete` 已落盘，
/// `Unchanged` 时存储未变）。持久化失败时 `update` 不执行，错误由 `publish` 返回的 future
/// 带回给 provider。
#[derive(Default)]
pub struct ModelsPublication {
    /// 持久化策略。
    pub persist: ModelsPublicationPersist,
    /// 持久化 mutation 成功后同步执行的内存更新。
    pub update: Option<PublicationUpdateFn>,
}

/// `publish(publication)` 的回调类型。
///
/// 返回值三态（以 `publish_provider_models` 的实现为准）：
/// - `Ok(true)` = 通过写盘前后的两次代际/取消检查：persist 按策略完成（`Unchanged` 不写
///   存储），`update`（若有）已执行；
/// - `Ok(false)` = operation 自身发现 signal 已中止或代际已过期而放弃（写盘前放弃则无
///   副作用；在写盘期间被取代时写盘可能已发生，但 `update` 不会执行）；
/// - `Err` = 存储读写失败，或取消竞速（`race_with_abort_signal`）产生的中止错误——signal
///   中止时等待方立即拿到 `Err`，即使发布任务随后仍可能在后台完成写盘。
///
/// provider 应当等待这个 future，因为不存在 tokio 运行时时它会退化为「直接返回未启动的
/// operation」（有运行时时实现会 `spawn`，丢弃返回的 future 也不影响完成，见
/// `run_provider_refresh_phase`）。
pub type PublishFn =
    Arc<dyn Fn(ModelsPublication) -> BoxFuture<'static, Result<bool, AiError>> + Send + Sync>;

/// `refreshModels(context)` 的上下文。
///
/// TS: `src/models.ts#RefreshModelsContext`
///
/// 一次 `Models::refresh` 最多会对同一 provider 调用两次 `refresh_models`：先以
/// `allow_network = false` 恢复缓存，再在拿到凭证后以 `allow_network = true` 抓取远端
/// （无联网权限或无凭证时只执行第一次）。两次共享同一个 generation 与 signal，因此任一阶段
/// 的发布都受同一套过期检查约束。`publish` 是 provider 回写目录的唯一入口，它绑定本次刷新
/// 的 generation/signal：只有代际被取代或 signal 中止时，发布才会被丢弃。
#[derive(Clone)]
pub struct RefreshModelsContext {
    /// 生效的配置凭证（OAuth 在联网前已刷新）。
    pub credential: Option<Credential>,
    /// 本阶段开始前的 provider 目录快照（经 `with_known_model_types` 处理：未知类型不会
    /// 出现，`raw_models` 已清空，provider 若原样发布回去只会写过滤后的 `models`）。
    pub stored: Option<ModelsStoreEntry>,
    /// 带代际检查的发布入口：operation 自身发现 signal 已中止或代际被更新一代取代时返回
    /// `Ok(false)`；`Err` 则是存储错误或取消竞速产生的中止错误（发布任务仍可能在后台完成
    /// 写盘）。三态语义详见 [`PublishFn`]。
    pub publish: PublishFn,
    /// 离线/仅缓存初始化时为 false；为 false 时 provider 不应发起网络访问。
    pub allow_network: bool,
    /// 绕过 provider 的新鲜度检查立即抓取（仅联网时传递；离线阶段恒为 `None`）。
    pub force: Option<bool>,
    /// 恒存在的共享取消信号；刷新被 `set_provider`/`delete_provider` 取代时由
    /// `begin_provider_refresh` 中止。
    pub signal: AbortScope,
    /// 已注册 provider 的 base URL（含 wrapper 覆盖）。
    pub base_url: Option<String>,
}

impl std::fmt::Debug for RefreshModelsContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefreshModelsContext")
            .field("allow_network", &self.allow_network)
            .field("force", &self.force)
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

/// `models.refresh(options)`。
///
/// TS: `src/models.ts#ModelsRefreshOptions`
///
/// 只处理动态 provider（声明了 `refreshModels` 的）：未在 `providers` 中列出的、或静态
/// provider 会被静默跳过，也不会出现在结果的 `errors` 里。`allow_network = false` 时仍会
/// 执行缓存恢复阶段。
#[derive(Debug, Clone, Default)]
pub struct ModelsRefreshOptions {
    /// 是否允许联网（默认 true）。为 false 时仍会执行缓存恢复阶段。
    pub allow_network: Option<bool>,
    /// 限定刷新这些 provider id；未知与静态 provider 忽略。
    pub providers: Option<Vec<String>>,
    /// 绕过新鲜度检查立即抓取。
    pub force: Option<bool>,
    /// 取消信号；`None` 按永不中止的 scope 处理（`operation_signal` 语义）。
    pub signal: Option<AbortScope>,
}

/// `models.refresh()` 的结果。
///
/// TS: `src/models.ts#ModelsRefreshResult`
///
/// `errors` 只收集「未因中止被打断」的失败：signal 中止后的取消错误不写入。`aborted` 是
/// 结束时对调用方 signal 的快照；即使为 true，`errors` 仍可能含有中止前已经发生的失败。
#[derive(Debug, Clone, Default)]
pub struct ModelsRefreshResult {
    /// 调用方 signal 是否已中止。
    pub aborted: bool,
    /// provider id → 错误（插入序，即各 provider 任务的完成顺序）。
    pub errors: IndexMap<String, AiError>,
}

/// 请求头变换（`ModelsRequestTransforms.transformHeaders`）。
///
/// 拿到的是合并后的最终头集合（缺省为空），返回值原样交给 provider，`Models` 层不再修改。
/// 函数是 `Fn` 且可跨线程共享；panic 不受 `Result` 保护，实现应保证不 panic 并原样返回
/// 不需要修改的输入。
pub type TransformHeadersFn = Arc<
    dyn Fn(crate::types::ProviderHeaders) -> BoxFuture<'static, crate::types::ProviderHeaders>
        + Send
        + Sync,
>;

/// `ModelsRequestTransforms`：在 provider 分发前变换已组装好的请求头。
///
/// 变换发生在 session 头、provider auth 头与显式请求头合并之后、调用 provider 之前；因此
/// 它可以观察到最终值，也可以覆盖鉴权头（TS 的中心化「最后一道请求头加工」）。
#[derive(Clone, Default)]
pub struct ModelsRequestTransforms {
    /// 变换函数；可同步也可异步（Rust 统一为 future）。
    pub transform_headers: Option<TransformHeadersFn>,
}

/// `ModelsApiStreamOptions`。
///
/// TS 的 `ApiStreamOptions<T>` 是 `StreamOptions & Record<string, unknown>` 的具体化；Rust
/// 侧用 [`StreamOptions`]（含 API 专属/任意额外字段的 [`StreamOptions::extra`]）+
/// [`ModelsRequestTransforms`]，并额外保留 TS 请求对象上的 `reasoning` 字段：`Models` 用它做
/// telemetry 属性，并把它以同名键并进 `extra` 后透传给 provider（TS 的 `providerOptions`
/// 展开包含它）。
#[derive(Default)]
pub struct ModelsStreamOptions {
    /// 基础请求选项。
    pub base: StreamOptions,
    /// 头变换。
    pub transforms: ModelsRequestTransforms,
    /// 请求的 reasoning level（telemetry 属性；同时透传为 `extra["reasoning"]`）。
    pub reasoning: Option<String>,
    /// 请求级中止原因（TS 的 `signal.reason`）。
    ///
    /// 冻结的 [`StreamOptions::signal`] 只携带 `CancellationToken`，不携带 reason；调用方
    /// 如果中止时会带自定义 `reason`（TS 的 `controller.abort(error)`），把同一个 reason
    /// 放在这里，鉴权阶段的中止错误会使用它的 `name`/`message`。
    pub signal_reason: Option<AbortReason>,
}

impl From<StreamOptions> for ModelsStreamOptions {
    fn from(base: StreamOptions) -> Self {
        ModelsStreamOptions {
            base,
            ..Default::default()
        }
    }
}

/// `ModelsSimpleStreamOptions`。
///
/// TS: `SimpleStreamOptions & ModelsRequestTransforms`。`base.reasoning` 是枚举形式的 thinking
/// level（[`ThinkingLevel`]），telemetry 使用它的字符串名；tool choice 等简单选项原样交给
/// `Provider::stream_simple`，不再经过 `apply_auth` 的 extra 透传。
#[derive(Default)]
pub struct ModelsSimpleStreamOptions {
    /// 基础请求选项。
    pub base: SimpleStreamOptions,
    /// 头变换。
    pub transforms: ModelsRequestTransforms,
    /// 请求级中止原因（TS 的 `signal.reason`），见 [`ModelsStreamOptions::signal_reason`]。
    pub signal_reason: Option<AbortReason>,
}

impl From<SimpleStreamOptions> for ModelsSimpleStreamOptions {
    fn from(base: SimpleStreamOptions) -> Self {
        ModelsSimpleStreamOptions {
            base,
            ..Default::default()
        }
    }
}

impl ModelsSimpleStreamOptions {
    /// 逐字段复制（`SimpleStreamOptions` 没有 `Clone`）。
    ///
    /// 与 [`SimpleStreamOptions::duplicate`] 一样用列全字段的字面量：新增字段时编译失败，
    /// 强制同步。预热器、会话 SDK 等需要保留原请求配置的调用方统一走这里。
    pub fn duplicate(&self) -> Self {
        ModelsSimpleStreamOptions {
            base: self.base.duplicate(),
            transforms: self.transforms.clone(),
            signal_reason: self.signal_reason.clone(),
        }
    }
}

/// `ModelsDeferredFetchOptions`。
///
/// 延迟响应轮询的选项：`DeferredFetchOptions` 含 `wait` 与基础请求选项，外层再加头变换与
/// 中止原因。轮询失败由 provider 决定是否可重试，`Models` 层不重试。
#[derive(Default)]
pub struct ModelsDeferredFetchOptions {
    /// 基础请求选项。
    pub base: DeferredFetchOptions,
    /// 头变换。
    pub transforms: ModelsRequestTransforms,
    /// 请求级中止原因（TS 的 `signal.reason`），见 [`ModelsStreamOptions::signal_reason`]。
    pub signal_reason: Option<AbortReason>,
}

/// `ModelsDeferredCancelOptions`。
///
/// 取消延迟响应的选项；与轮询共用同一套请求头/env 鉴权流程，因此取消请求也带得上凭证。
#[derive(Default)]
pub struct ModelsDeferredCancelOptions {
    /// 基础请求选项。
    pub base: DeferredCancelOptions,
    /// 头变换。
    pub transforms: ModelsRequestTransforms,
    /// 请求级中止原因（TS 的 `signal.reason`），见 [`ModelsStreamOptions::signal_reason`]。
    pub signal_reason: Option<AbortReason>,
}

/// `ModelsImagesOptions`。
///
/// [`provider::ImagesOptions`] 复用 [`StreamOptions`]（含 `metadata`），外层再加头变换与
/// 中止原因。注意 `generate_images` 不返回 `Result`：`signal` 本身随 `base` 透传给鉴权与
/// provider（仍是请求的取消令牌），`Models` 在错误分支额外读取它，决定应记为 `aborted`
/// 还是 `error`。
#[derive(Default)]
pub struct ModelsImagesOptions {
    /// 基础请求选项。
    pub base: provider::ImagesOptions,
    /// 头变换。
    pub transforms: ModelsRequestTransforms,
    /// 请求级中止原因（TS 的 `signal.reason`），见 [`ModelsStreamOptions::signal_reason`]。
    pub signal_reason: Option<AbortReason>,
}

/// `ModelsClassifierOptions`。
///
/// [`provider::ClassifierOptions`] 复用 [`StreamOptions`]（含 `temperature`），外层再加头
/// 变换与中止原因；与图片一样，错误被折叠进结果而不是返回 `Err`。
#[derive(Default)]
pub struct ModelsClassifierOptions {
    /// 基础请求选项。
    pub base: provider::ClassifierOptions,
    /// 头变换。
    pub transforms: ModelsRequestTransforms,
    /// 请求级中止原因（TS 的 `signal.reason`），见 [`ModelsStreamOptions::signal_reason`]。
    pub signal_reason: Option<AbortReason>,
}

/// `getAuth` 的目标：provider id 或带静态头的模型。
///
/// 借用只在下一次 `get_auth` 调用期间有效（异步调用返回前必须保持存活）；选择 `Model`
/// 变体时，模型的 `headers` 会在 provider 级 auth 头之上再合并一层。
#[derive(Clone, Copy)]
pub enum GetAuthTarget<'a> {
    /// `models.getAuth(providerId)`。
    Provider(&'a str),
    /// `models.getAuth(model)`。
    Model(&'a Model),
}

/// 运行时 provider 集合 + auth 应用 + 请求便利接口。
///
/// TS: `src/models.ts#Models`。读访问器同步返回；请求路径同步返回流。
///
/// 实现必须 `Send + Sync`，因为一个 `Models` 通常被多个任务共享；所有方法只借用 `&self`，
/// 内部可变状态由实现自行同步（[`ModelsImpl`] 用 `Mutex` 与 `tokio::sync::Mutex`）。
///
/// 错误模型分三类：
/// - 查询与刷新：返回 `Result<_, AiError>`；
/// - 鉴权（`check_auth`/`get_available*`/`get_auth`/`login`/`logout`）：返回
///   `Result<_, AuthError>`，保留 TS 的 `error.name` 与 `code`；
/// - 请求（`stream*`/`complete*`/`generate_images`/`classify`）：错误写进结果对象或事件流，
///   不向调用方返回 `Err`；`cancel_deferred` 是唯一返回 `Result<(), AiError>` 的请求方法。
///
/// 所有带 `signal` 的方法都遵循同一约定：signal 中止后尽快返回中止错误，但已经开始的
/// 网络请求/凭证刷新是否继续由底层实现决定（协作式取消）。
#[allow(clippy::double_must_use, clippy::result_large_err)]
#[async_trait]
pub trait Models: Send + Sync {
    /// 所有 provider（插入序）。
    fn get_providers(&self) -> Vec<Arc<dyn Provider>>;

    /// 按 id 取 provider。
    fn get_provider(&self, id: &str) -> Option<Arc<dyn Provider>>;

    /// 一个或全部 provider 的 chat 模型（best-effort，provider 抛错视为无模型）。
    fn get_models(&self, provider: Option<&str>) -> Vec<Model>;

    /// 运行时 chat 模型查找。
    fn get_model(&self, provider: &str, id: &str) -> Option<Model>;

    /// 一个或全部 provider 的指定类型模型。
    ///
    /// 从 `get_all_models`（含 image/classifier）过滤，并把结果模型对象内的 `type` 补齐为
    /// 查询类型（chat 保持缺省，即不带 `type`）。
    fn get_models_of_type(&self, model_type: ModelType, provider: Option<&str>) -> Vec<Model>;

    /// 指定类型的运行时模型查找。
    fn get_model_of_type(&self, model_type: ModelType, provider: &str, id: &str) -> Option<Model>;

    /// 一个或全部 provider 的所有类型模型（chat/image/classifier）。
    ///
    /// 与 [`Models::get_models`] 相同的 best-effort 语义；返回顺序是 provider 注册顺序，
    /// 每个 provider 内部保持自己的模型顺序。
    fn get_all_models(&self, provider: Option<&str>) -> Vec<AnyModel>;

    /// 并发刷新选定的动态 provider。
    ///
    /// 对每个目标 provider 依次执行「缓存恢复（离线）→ 凭证解析 → 联网抓取」两阶段；各
    /// provider 之间并发，单个 provider 失败只记入 [`ModelsRefreshResult::errors`]，不会让整个
    /// 调用失败。`providers` 中未知或没有 `refreshModels` 的 id 被忽略。取消由
    /// `options.signal` 或并发的 `set_provider`/`delete_provider`（代际提升）触发。
    async fn refresh(&self, options: ModelsRefreshOptions) -> Result<ModelsRefreshResult, AiError>;

    /// 检查 provider 的 auth 配置（不刷新 OAuth）。
    ///
    /// 返回 `None` 表示未知 provider 或没有可用凭证。存储中的 OAuth 凭证只要 provider 仍声明
    /// 了 OAuth，就直接报告 `source = "OAuth"`，不联网刷新；API key 若带 `check` 处理器则交给
    /// 它校验（是否联网由 provider 实现决定），否则走通用解析。signal 中止时返回 `AuthError`
    /// （`name` 来自 `signal.reason`，缺失时为 `AbortError`）。
    async fn check_auth(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<Option<AuthCheck>, AuthError>;

    /// 返回 auth 配置完整的 provider 的 chat 模型。
    ///
    /// 先并发读取凭证并做 auth 检查，只保留已鉴权的 provider，再对每个 provider 调用
    /// `filter_models`（默认恒等）过滤。返回值保持 provider 注册顺序；`provider_id = None`
    /// 或空串时扫描全部 provider（空串是 TS 的 falsy 语义）。任一凭证读取/auth 检查失败即
    /// 整体返回 `Err`。
    async fn get_available(
        &self,
        provider_id: Option<&str>,
        options: AuthOperationOptions,
    ) -> Result<Vec<Model>, AuthError>;

    /// 返回 auth 配置完整的 provider 的指定类型模型。
    ///
    /// 在 [`Models::get_all_available`] 的结果上按类型过滤，并为非 chat 结果补齐模型对象内的
    /// `type` 字段（TS 的 image/classifier 模型对象总带 `type`）。
    async fn get_available_of_type(
        &self,
        model_type: ModelType,
        provider_id: Option<&str>,
        options: AuthOperationOptions,
    ) -> Result<Vec<Model>, AuthError>;

    /// 返回 auth 配置完整的 provider 的所有类型模型。
    ///
    /// 与 [`Models::get_available`] 相同地筛选已鉴权 provider，但每个 provider 的模型经过
    /// `filter_all_models`（默认实现复刻 TS 的 `filterAllModels` 缺失回退，见 `provider.rs`）：
    /// image/classifier 模型全部保留，chat 模型按 `filter_models` 的可用 id 过滤。
    async fn get_all_available(
        &self,
        provider_id: Option<&str>,
        options: AuthOperationOptions,
    ) -> Result<Vec<AnyModel>, AuthError>;

    /// 解析 provider 作用域的 auth（或 provider auth + 模型静态头）。
    ///
    /// 未知 provider 返回 `Ok(None)`。传入 [`GetAuthTarget::Model`] 时，模型的静态 `headers`
    /// 会在 provider 级 auth 头之上再做一次大小写不敏感覆盖；`overrides` 里的 api key/env
    /// 优先于存储凭证（语义见 [`AuthResolutionOverrides`]）。
    async fn get_auth(
        &self,
        target: GetAuthTarget<'_>,
        overrides: AuthResolutionOverrides,
    ) -> Result<Option<AuthResult>, AuthError>;

    /// 运行 provider 拥有的登录流程并持久化凭证。
    ///
    /// 未知 provider 返回 code `provider` 的 `AuthError`；provider 不支持所请求的
    /// `auth_type`（或没有 login 实现）返回 code `auth` 的错误。登录成功后通过
    /// `CredentialStore::modify` 写盘：写盘一旦开始就不可取消（避免因取消造成半写），开始前
    /// 的中止会让整个调用返回中止错误。
    async fn login(
        &self,
        provider_id: &str,
        auth_type: AuthType,
        interaction: &dyn AuthInteraction,
        options: LoginOptions,
    ) -> Result<Credential, AuthError>;

    /// 删除 provider 的存储凭证。
    ///
    /// 不校验 provider 是否已注册，直接调用 `CredentialStore::delete`（进程内默认实现对不存在
    /// 的条目静默成功）。存储删除失败包装成 code `auth` 的 `AuthError`；若失败同时 signal 已
    /// 中止，则优先返回中止错误。
    async fn logout(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<(), AuthError>;

    /// 流式请求（同步返回）。
    ///
    /// 返回前会同步 poll setup 一次（见 `api/lazy.rs` 的 `lazy_stream`）：chat/provider 检查
    /// 与鉴权调用的发起立即执行；异步的鉴权与 provider 调用在后台任务中继续推进，错误以流
    /// 事件给出（`stopReason = "error"`），而不是让本方法返回错误。setup 内先检查 chat 类型
    /// 与 provider 存在性，再解析 auth。
    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: ModelsStreamOptions,
    ) -> AssistantMessageEventStream;

    /// 流式请求的最终结果。
    ///
    /// 等价于 `stream(...).result()`；`result()` 的 `Err` 只对应结果通道发送端关闭（正常
    /// 调用中不会发生，见 `event_stream.rs`）。这里构造一条 `stopReason = "error"` 的
    /// assistant 消息兜底；该分支正常不可达，仅防御性处理。
    async fn complete(
        &self,
        model: &Model,
        context: &Context,
        options: ModelsStreamOptions,
    ) -> AssistantMessage;

    /// 简单流式请求（同步返回）。
    ///
    /// 与 [`Models::stream`] 相同的 setup 语义（返回前同步 poll 一次、其余在后台任务中
    /// 继续），区别是使用 [`SimpleStreamOptions`]（thinking
    /// level 为枚举、可选 tool choice）；`reasoning` 不并入 `extra`，而是保留在请求对象里交给
    /// `Provider::stream_simple`。
    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: ModelsSimpleStreamOptions,
    ) -> AssistantMessageEventStream;

    /// 简单流式请求的最终结果。
    ///
    /// 等价于 `stream_simple(...).result()`，兜底分支的语义同 [`Models::complete`]。
    async fn complete_simple(
        &self,
        model: &Model,
        context: &Context,
        options: ModelsSimpleStreamOptions,
    ) -> AssistantMessage;

    /// 延迟响应轮询（同步返回）。
    ///
    /// 惰性流：setup 内先检查 provider 支持 `fetch_deferred`（否则产生 code `provider` 的
    /// `error` 事件），再解析 auth 并调用 provider。telemetry 观察器以 `poll = true` 创建。
    fn stream_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: ModelsDeferredFetchOptions,
    ) -> AssistantMessageEventStream;

    /// 延迟响应轮询的最终结果。
    ///
    /// 等价于 `stream_deferred(...).result()`，兜底分支的语义同 [`Models::complete`]。
    async fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: ModelsDeferredFetchOptions,
    ) -> AssistantMessage;

    /// 取消延迟响应。
    ///
    /// 与流式入口不同，这里直接 `await` provider 的调用并返回真实错误；provider 未声明
    /// `cancel_deferred` 时返回 code `provider` 的 `AiError`（消息与轮询入口的能力错误相同）。
    async fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: ModelsDeferredCancelOptions,
    ) -> Result<(), AiError>;

    /// 生成图像（不 reject：错误以 `AssistantImages` 返回）。
    ///
    /// 内部顺序：校验图片模型 → 查 provider → 检查能力 → `apply_auth` → 调用 provider。
    /// 任一步失败都转成 `stop_reason = "aborted" | "error"` 的 [`AssistantImages`]；
    /// `aborted` 取调用方 signal 在**错误发生时**的状态。
    async fn generate_images(
        &self,
        model: &Model,
        context: &ImagesContext,
        options: ModelsImagesOptions,
    ) -> AssistantImages;

    /// 结构化分类（不 reject：错误以 `ClassifierResult` 返回）。
    ///
    /// 内部顺序：校验分类模型 → 校验输入受支持 → 查 provider → 检查能力 → `apply_auth` →
    /// 调用 provider。失败折叠方式与 [`Models::generate_images`] 相同。
    async fn classify(
        &self,
        model: &Model,
        context: &ClassifierContext,
        options: ModelsClassifierOptions,
    ) -> ClassifierResult;
}

/// 可变的 `Models` 集合。
///
/// TS: `src/models.ts#MutableModels`
///
/// 三个修改操作都会先「取代」该 provider 正在进行的刷新：自增代际并中止旧的 controller。
/// 因此已经开始的刷新之后发起的发布只会得到 `Ok(false)`，过期的 `update` 不会执行；若取代
/// 发生在存储写入期间，旧条目可能已落盘（随后被新一代覆盖）。
pub trait MutableModels: Models {
    /// 按 `provider.id` upsert（已存在则原位替换，保持插入位置）。
    fn set_provider(&self, provider: Arc<dyn Provider>);
    /// 删除 provider；不存在的 id 静默忽略。
    fn delete_provider(&self, id: &str);
    /// 清空 provider（同时中止所有在途刷新，包括 provider 已被删除但刷新尚未收尾的情况）。
    fn clear_providers(&self);
}

/// `createModels(options)`。
///
/// TS: `src/models.ts#CreateModelsOptions`
///
/// 所有字段都可缺省：凭证与模型目录回退到进程内存储（重启即丢失），auth 环境回退到读取
/// 进程环境变量的默认上下文。调用方可以注入持久化实现；测试与临时进程可直接用默认值。
#[derive(Clone, Default)]
pub struct CreateModelsOptions {
    /// 凭证存储；缺省为进程内实现（[`InMemoryCredentialStore`]）。
    pub credentials: Option<Arc<dyn CredentialStore>>,
    /// 模型目录存储；缺省为进程内实现（[`InMemoryModelsStore`]）。
    pub models_store: Option<Arc<dyn ModelsStore>>,
    /// auth 环境上下文（env 的读取来源）；缺省为 `default_provider_auth_context()`。
    pub auth_context: Option<Arc<dyn AuthContext>>,
    /// 每个请求的默认身份（请求的 `identity` 逐字段覆盖）。
    pub identity: Option<ClientIdentity>,
}

impl CreateModelsOptions {
    /// 空选项。
    pub fn new() -> Self {
        Self::default()
    }
}

/// `mergeHeaders(base, override)`：忽略大小写删除同名 base 键后写入 override（保留原大小写，
/// `null` 值保留供下游解释为「抑制」）。
///
/// TS: `src/models.ts#mergeHeaders`
///
/// 空集合与「没有头」是两种状态：只要任一侧是 `Some`（包括空 map），结果就是 `Some`，因此
/// `None, None` 之外不会退化成 `None`（测试 `empty_collections_are_still_some` 锁定此行为）。
/// 未被覆盖的键保持 base 的插入顺序；被覆盖的键以 override 的原始大小写重新插入到末尾。
pub fn merge_headers(
    base: Option<crate::types::ProviderHeaders>,
    override_headers: Option<crate::types::ProviderHeaders>,
) -> Option<crate::types::ProviderHeaders> {
    if base.is_none() && override_headers.is_none() {
        return None;
    }
    let mut merged = base.unwrap_or_default();
    for (name, value) in override_headers.unwrap_or_default() {
        let lower_name = name.to_lowercase();
        // IndexMap 不能边遍历 keys 边删除：先收集待删键（大小写不敏感的比较），再统一移除。
        let to_remove: Vec<String> = merged
            .keys()
            .filter(|existing| existing.to_lowercase() == lower_name)
            .cloned()
            .collect();
        for existing in to_remove {
            merged.shift_remove(&existing);
        }
        merged.insert(name, value);
    }
    Some(merged)
}

/// `Date.now()`。
fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_millis() as f64)
}

/// `hasKnownModelType(model)`。
fn has_known_model_type(model: &AnyModel) -> bool {
    matches!(
        model.model_type(),
        ModelType::Chat | ModelType::Image | ModelType::Classifier
    )
}

/// `withKnownModelTypes(entry)`：丢弃本版本不认识的类型（Rust 的枚举恒为已知）。
///
/// TS 返回一个新的 `{ ...entry, models: filtered }` 对象，原始数组里的未知模型不会跟着
/// 走；Rust 的 [`ModelsStoreEntry::raw_models`] 因此在这里清空（provider 若把 `stored`
/// 原样发布回去，持久化的也只有过滤后的模型）。
fn with_known_model_types(entry: ModelsStoreEntry) -> ModelsStoreEntry {
    ModelsStoreEntry {
        models: entry
            .models
            .into_iter()
            .filter(has_known_model_type)
            .collect(),
        raw_models: None,
        ..entry
    }
}

/// `ModelsError` → [`AiError`]（保留 `name = "ModelsError"` 与 `code` 细节）。
///
/// `ModelsError.code` 为 `auth`/`oauth` 时映射为 [`AiErrorKind::Auth`]，其余（`provider`、
/// `stream`、`model_source` 等）映射为 [`AiErrorKind::Other`]；原始 code 字符串放进
/// `detail.code`，供上层按 TS 的错误码分支。
#[allow(clippy::result_large_err)]
pub fn ai_error_from_models_error(error: ModelsError) -> AiError {
    let kind = match error.code {
        ModelsErrorCode::Auth | ModelsErrorCode::OAuth => AiErrorKind::Auth,
        _ => AiErrorKind::Other,
    };
    AiError::new(kind, error.message)
        .with_name("ModelsError")
        .with_detail(serde_json::json!({ "code": error.code.as_str() }))
}

/// `AuthError` → [`AiError`]（`ModelsError.code` 保留在 `detail.code`）。
///
/// 中止错误优先归为 [`AiErrorKind::Aborted`]（即使它同时带 code），这样上层能区分「用户
/// 取消」与「鉴权失败」；`name` 原样保留（通常是 `"AbortError"` 或 TS 自定义的 reason 名）。
fn ai_error_from_auth_error(error: AuthError) -> AiError {
    let kind = if error.is_aborted() {
        AiErrorKind::Aborted
    } else if error.code == Some(ModelsErrorCode::Auth)
        || error.code == Some(ModelsErrorCode::OAuth)
    {
        AiErrorKind::Auth
    } else {
        AiErrorKind::Other
    };
    let code = error.code;
    let converted = AiError::new(kind, error.message).with_name(error.name);
    match code {
        Some(code) => converted.with_detail(serde_json::json!({ "code": code.as_str() })),
        None => converted,
    }
}

/// 共享状态（`ModelsImpl` 的字段在闭包里按 `Arc` 捕获）。
///
/// 上面四个表字段（`providers`、`refresh_generations`、`refresh_controllers`、
/// `publication_locks`）的 `std::sync::Mutex` 只保护内存表、不跨 await；但有两处例外要
/// 注意：`refresh_models` 在持有 `providers` 锁期间会调用 `Provider` 的只读谓词
/// `has_refresh_models()`/`id()`（实现不得回调 `Models`，否则可能重入同把锁）；
/// `publication_locks` 里装的是 `tokio::sync::Mutex`，发布期间会跨存储 I/O 持有。
/// 中毒后的锁按 `into_inner` 继续使用（见模块文档的并发约定）。
struct ModelsState {
    // `id -> provider`，插入序即 `get_providers()` 的返回顺序（TS Map 语义）。
    providers: Mutex<IndexMap<String, Arc<dyn Provider>>>,
    // 凭证/目录/环境由调用方注入；未注入时用进程内默认实现（重启不保留）。
    credentials: Arc<dyn CredentialStore>,
    models_store: Arc<dyn ModelsStore>,
    auth_context: Arc<dyn AuthContext>,
    // 默认客户端身份；请求级 identity 在 `apply_auth` 里逐字段覆盖它。
    identity: Option<ClientIdentity>,
    // 每个 provider 的刷新代际，只增不减；发布时用它判断自己是否已被取代。
    refresh_generations: Mutex<HashMap<String, u64>>,
    // 当前在跑的刷新 controller；新刷新开始或 provider 被增删时移除并 abort 旧的。
    refresh_controllers: Mutex<HashMap<String, Arc<AbortScope>>>,
    // 每 provider 一把异步锁，代替 TS 的 `publicationChains` Promise 链。
    publication_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// `Models` 的运行时实现。
///
/// TS: `src/models.ts#ModelsImpl`
///
/// 只有一个字段：全部状态共享在 `Arc<ModelsState>` 里，方法通过 `&self` 并发访问。构造时
/// 未提供的组件使用进程内默认实现（[`InMemoryCredentialStore`] / [`InMemoryModelsStore`] /
/// `default_provider_auth_context`），因此不传任何选项也能工作，但进程重启后凭证与目录都
/// 会丢失。
pub struct ModelsImpl {
    state: Arc<ModelsState>,
}

impl std::fmt::Debug for ModelsImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelsImpl").finish_non_exhaustive()
    }
}

impl ModelsImpl {
    /// 新建集合。
    ///
    /// 未提供的 `credentials` / `models_store` / `auth_context` 分别回退到进程内实现与默认
    /// 环境上下文；`identity` 原样保存，请求时再与 `StreamOptions::identity` 逐字段合并
    /// （请求级字段覆盖集合级字段）。
    pub fn new(options: CreateModelsOptions) -> Self {
        let state = ModelsState {
            providers: Mutex::new(IndexMap::new()),
            credentials: options
                .credentials
                .unwrap_or_else(|| Arc::new(InMemoryCredentialStore::new())),
            models_store: options
                .models_store
                .unwrap_or_else(|| Arc::new(InMemoryModelsStore::new())),
            auth_context: options
                .auth_context
                .unwrap_or_else(default_provider_auth_context),
            identity: options.identity,
            refresh_generations: Mutex::new(HashMap::new()),
            refresh_controllers: Mutex::new(HashMap::new()),
            publication_locks: Mutex::new(HashMap::new()),
        };
        ModelsImpl {
            state: Arc::new(state),
        }
    }
}

/// `createModels(options?)`。
///
/// TS: `src/models.ts#createModels`
///
/// 返回 trait object 而非具体类型：调用方只依赖 [`MutableModels`] 契约，测试可以替换实现。
/// 集合本身没有全局注册表，调用方需要自己保存并共享返回的 `Arc`。
pub fn create_models(options: CreateModelsOptions) -> Arc<dyn MutableModels> {
    Arc::new(ModelsImpl::new(options))
}

// ---------------------------------------------------------------------------
// 刷新协调（ModelsImpl 的私有算法）
// ---------------------------------------------------------------------------

impl ModelsState {
    /// `supersedeProviderRefresh(providerId)`。
    ///
    /// 自增该 provider 的刷新代际（首次为 1），并中止仍登记的旧 controller；返回新代际。
    /// 代际只增不减，因此任何持有旧代际的发布在 `generation_is_current` 检查处都会失败。
    /// `set_provider`/`delete_provider`/`clear_providers` 与新一次 `refresh` 开始前都会调用它。
    fn supersede_provider_refresh(&self, provider_id: &str) -> u64 {
        let generation = {
            let mut generations = self
                .refresh_generations
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let generation = generations.get(provider_id).copied().unwrap_or(0) + 1;
            generations.insert(provider_id.to_owned(), generation);
            generation
        };
        let previous = {
            let mut controllers = self
                .refresh_controllers
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            controllers.remove(provider_id)
        };
        // 先移除、释放锁，再 abort（abort 会同步派发监听器）；这样监听器不会在持有
        // refresh_controllers 锁期间被执行。
        if let Some(previous) = previous {
            previous.abort();
        }
        generation
    }

    /// `beginProviderRefresh(providerId)`。
    ///
    /// 先取代旧刷新（自增代际并中止旧 controller），再安装本次刷新的新 controller。
    /// 返回 `(generation, controller)`；controller 同时用于组合调用方 signal（见
    /// `refresh_models`），以及在该 provider 被增删时中止本次刷新。
    fn begin_provider_refresh(&self, provider_id: &str) -> (u64, Arc<AbortScope>) {
        let controller = Arc::new(AbortScope::new());
        // 代际自增、移除旧 controller、安装新 controller 必须在同一个临界区完成。
        // 分成两段时，两个并发刷新都可能在对方插入前先 supersede，结果两个 controller
        // 同时存在，较早的那个既不会被 abort 也不会被后续 delete 清理。
        let (generation, previous) = {
            let mut generations = self
                .refresh_generations
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let generation = generations.get(provider_id).copied().unwrap_or(0) + 1;
            generations.insert(provider_id.to_owned(), generation);
            let previous = self
                .refresh_controllers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(provider_id.to_owned(), Arc::clone(&controller));
            (generation, previous)
        };
        // 先释放锁再 abort（abort 会同步派发监听器）。
        if let Some(previous) = previous {
            previous.abort();
        }
        (generation, controller)
    }

    /// 当前代际是否仍是 `generation`。
    ///
    /// 用于发布前的过期检查；provider 从未刷新过时表中没有条目，返回 false（调用方必须
    /// 先经过 `begin_provider_refresh` 才能拿到有效代际）。
    fn generation_is_current(&self, provider_id: &str, generation: u64) -> bool {
        self.refresh_generations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(provider_id)
            .copied()
            == Some(generation)
    }
}

/// `publishProviderModels(providerId, generation, signal, publication)`。
#[allow(clippy::result_large_err)]
///
/// 同一 provider 的发布串行化（每个 provider 一把 `tokio::sync::Mutex`，保证发布按序
/// 执行）；取消或代际变化会丢弃过期发布（过期的 `update` 不执行；若取代发生在存储
/// 写入期间，写盘可能已发生，随后由新一代覆盖）。
///
/// 执行顺序：取锁 → 检查取消/代际 → 按 persist 写/删存储 → 再检查一次 → 同步执行
/// `update` → 返回 `true`。两次检查都必要：写盘可能耗时，期间刷新可能被取代，此时不能
/// 再把旧条目同步进内存。返回的 future 与取消信号竞速，但取消只让等待方提前返回；已经
/// 开始的任务仍会跑完，并在下一个检查点发现中止。
fn publish_provider_models(
    state: Arc<ModelsState>,
    provider_id: String,
    generation: u64,
    signal: AbortScope,
    publication: ModelsPublication,
) -> BoxFuture<'static, Result<bool, AiError>> {
    let signal_for_race = signal.clone();
    let operation = async move {
        // 每个 provider 一把锁：把同一 provider 的发布排成队列（等价 TS 的 publicationChains）。
        let lock = {
            let mut locks = state
                .publication_locks
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            Arc::clone(
                locks
                    .entry(provider_id.clone())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        };
        let _guard = lock.lock().await;
        // 排队期间可能已被更新的刷新取代：先检查，避免旧目录覆盖新目录。
        if signal.is_aborted() || !state.generation_is_current(&provider_id, generation) {
            return Ok(false);
        }
        let options = ModelsStoreOperationOptions {
            signal: Some(signal.clone()),
        };
        // 持久化策略：Unchanged 不触碰存储；Delete/Entry 都带 signal，但默认内存存储只在
        // 方法入口检查一次，进入同步写入后不会因 abort 回滚；是否中途检查由具体存储实现
        // 决定——这正是下面写盘后仍需再做一次代际检查的原因。
        match publication.persist {
            ModelsPublicationPersist::Unchanged => {}
            ModelsPublicationPersist::Delete => {
                state.models_store.delete(&provider_id, options).await?;
            }
            ModelsPublicationPersist::Entry(entry) => {
                state
                    .models_store
                    .write(&provider_id, entry.clone(), options)
                    .await?;
            }
        }
        // 写盘后刷新可能已被取代：此时丢弃 update，保证内存状态不回退。
        if signal.is_aborted() || !state.generation_is_current(&provider_id, generation) {
            return Ok(false);
        }
        // update 是同步闭包（TS 的 publication.update），在发布锁内执行以维持顺序。
        if let Some(update) = publication.update {
            update();
        }
        Ok(true)
    };
    Box::pin(async move {
        match race_with_abort_signal(operation, &signal_for_race).await {
            Ok(result) => result,
            Err(error) => Err(error),
        }
    })
}

/// `runProviderRefreshPhase(provider, credential, allowNetwork, force, generation, signal)`。
///
/// 执行一次 `provider.refreshModels(context)`：先从存储读出上一条目录作为 `stored`（失败
/// 直接向上传播，由调用方记入 `errors`），再构造带代际检查的 `publish` 回调，最后把控制权
/// 交给 provider。本函数自身不写存储；provider 的缓存恢复与联网抓取都通过这个回调回写。
#[allow(clippy::result_large_err)]
async fn run_provider_refresh_phase(
    state: Arc<ModelsState>,
    provider: Arc<dyn Provider>,
    credential: Option<Credential>,
    allow_network: bool,
    force: Option<bool>,
    generation: u64,
    signal: AbortScope,
) -> Result<(), AiError> {
    let provider_id = provider.id().to_owned();
    // 读存储也带 signal：刷新的取消/取代会在这里就放弃，不必等到 provider 回调。
    let stored = state
        .models_store
        .read(
            &provider_id,
            ModelsStoreOperationOptions {
                signal: Some(signal.clone()),
            },
        )
        .await?;
    let publish_state = Arc::clone(&state);
    let publish_provider = provider_id.clone();
    let publish_signal = signal.clone();
    let publish: PublishFn = Arc::new(move |publication| {
        let operation = publish_provider_models(
            Arc::clone(&publish_state),
            publish_provider.clone(),
            generation,
            publish_signal.clone(),
            publication,
        );
        // TS 的 `publish()` 是立即执行的 async IIFE：即使 provider 丢弃返回的 promise，
        // 持久化与内存更新也会跑完。Rust 的 future 只在被轮询时执行，因此在有 runtime 时
        // 立刻 spawn 出去，返回一个仅等待结果的 future（调用方 drop 它也不会取消发布）。
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return operation;
        };
        let joined = handle.spawn(operation);
        Box::pin(async move {
            match joined.await {
                Ok(result) => result,
                // 与 `race_with_abort_signal` 的约定一致：被放弃的任务 panic 原样传播。
                Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
                Err(_) => Err(AiError::new(
                    AiErrorKind::Other,
                    "publication task was cancelled",
                )),
            }
        })
    });
    let context = RefreshModelsContext {
        credential,
        // 与 TS 的 `withKnownModelTypes(structuredClone(stored))` 一致：未知类型不进入本阶段。
        stored: stored.map(with_known_model_types),
        publish,
        allow_network,
        // 与 TS 的 `allowNetwork ? force : undefined` 一致：离线阶段不透传 force。
        force: if allow_network { force } else { None },
        signal,
        base_url: provider.base_url().map(str::to_owned),
    };
    provider.refresh_models(context).await
}

/// `resolveRefreshCredential(provider, stored, signal)`。
///
/// 从已存储凭证推导出「联网阶段可用的凭证」：
/// - 存储是 OAuth：provider 仍支持 OAuth 时，未过期直接复用；已过期才走
///   `refresh_stored_oauth_credential`（刷新会持久化轮换后的 token，且不会因本次刷新的取消
///   而中断已开始的内部请求）。provider 已不声明 OAuth 时返回 `None`。
/// - 否则尝试 provider 的 API key 处理器：存储凭证只有恰好是 api_key 时才作为输入，env 与
///   显式覆盖由处理器合并；处理器返回 `None` 表示未配置。
///
/// 返回 `Ok(None)` 表示「没有可联网凭证」，调用方会跳过联网阶段；`Err` 是鉴权失败的最终
/// 错误（已转成 [`AiError`]）。
#[allow(clippy::result_large_err)]
async fn resolve_refresh_credential(
    state: Arc<ModelsState>,
    provider: &Arc<dyn Provider>,
    stored: Option<&Credential>,
    signal: &AbortScope,
) -> Result<Option<Credential>, AiError> {
    if let Some(Credential::OAuth(stored_oauth)) = stored {
        // provider 不再支持 OAuth（例如配置被移除）时返回 None，不会回退到 API key 分支；
        // 这与 TS 的 `if (!provider.auth.oauth) return undefined` 一致。
        let Some(oauth) = provider.auth().oauth.clone() else {
            return Ok(None);
        };
        if now_ms() < stored_oauth.expires {
            return Ok(Some(Credential::OAuth(stored_oauth.clone())));
        }
        if signal.is_aborted() {
            return Ok(None);
        }
        // 在凭证存储的锁内二次判定过期：并发刷新可能已经换过 token，此时无需重复刷新。
        let needs_refresh: Arc<dyn Fn(&OAuthCredential) -> bool + Send + Sync> =
            Arc::new(|current| now_ms() >= current.expires);
        return match refresh_stored_oauth_credential(
            Arc::clone(&state.credentials),
            provider.id(),
            oauth,
            needs_refresh,
            signal,
        )
        .await
        {
            Ok(Some(credential)) => Ok(Some(Credential::OAuth(credential))),
            Ok(None) => Ok(None),
            Err(error) => Err(ai_error_from_auth_error(error)),
        };
    }

    let Some(api_key) = provider.auth().api_key.clone() else {
        return Ok(None);
    };
    // 存储的是 OAuth / 其他类型时不能喂给 API key 处理器；只有 api_key 才是其输入。
    let credential = match stored {
        Some(Credential::ApiKey(credential)) => Some(credential),
        _ => None,
    };
    let result = api_key
        .resolve(ApiKeyAuthInput {
            ctx: state.auth_context.as_ref(),
            credential,
            signal,
        })
        .await;
    match result {
        Ok(None) => Ok(None),
        Ok(Some(result)) => Ok(Some(Credential::ApiKey(ApiKeyCredential {
            key: result.auth.api_key,
            env: result.env,
            extra: IndexMap::new(),
        }))),
        Err(error) => Err(ai_error_from_auth_error(error)),
    }
}

/// `readCredential(providerId, signal)`（失败包装成 code `auth` 的 `ModelsError`）。
///
/// 存储读取失败不是致命的中止：调用方（`refresh_models`）会先完成缓存恢复阶段，再把这个
/// 错误作为该 provider 的刷新失败上报，因此已缓存的目录仍能生效。
#[allow(clippy::result_large_err)]
async fn read_credential(
    state: &ModelsState,
    provider_id: &str,
    signal: &AbortScope,
) -> Result<Option<Credential>, AuthError> {
    match state
        .credentials
        .read(
            provider_id,
            AuthOperationOptions {
                signal: Some(signal.clone()),
            },
        )
        .await
    {
        Ok(credential) => Ok(credential),
        Err(error) => Err(AuthError::models(
            ModelsErrorCode::Auth,
            format!("Credential store read failed for {provider_id}"),
            Some(&error.as_thrown_value()),
        )),
    }
}

/// 执行一次 `refresh()`（见 TS 的逐步算法）。
///
/// 步骤与失败语义：
/// 1. 调用方 signal 已中止 → 立即返回 `{ aborted: true, errors: {} }`，不产生副作用。
/// 2. 选出「有 `refreshModels`」且被 `providers` 限定命中的 provider；静态/未知 provider
///    静默跳过。**同步**为每个 provider 调用 `begin_provider_refresh`（提升代际并安装
///    controller），再 `spawn` 任务——顺序与 TS 的 `map(async …)` 一致，见循环内注释。
/// 3. 每个任务：读凭证（错误暂存）→ 离线缓存恢复 → 若有凭证错误则上报 → 不允许联网或已
///    中止则结束 → 解析凭证（OAuth 可能刷新）→ 联网阶段。
/// 4. 非中止错误写入 `errors`；无论成功失败都清理自己安装的 controller（只有仍是当前那个
///    时才删，避免误删后续刷新的 controller）。
/// 5. 等待全部任务；任务 panic 原样向上传播。调用方 signal 中止时吞掉中止错误并返回部分
///    结果，`aborted` 为最终快照。
///
/// 注意：缓存恢复阶段先于鉴权与联网，即使无凭证或离线也会执行，这是「启动即用缓存」的
/// 关键；`force` 只在联网阶段传递。
#[allow(clippy::result_large_err)]
async fn refresh_models(
    state: Arc<ModelsState>,
    options: ModelsRefreshOptions,
) -> Result<ModelsRefreshResult, AiError> {
    let allow_network = options.allow_network.unwrap_or(true);
    let caller_signal = operation_signal(options.signal.clone());
    let errors: Arc<Mutex<IndexMap<String, AiError>>> = Arc::new(Mutex::new(IndexMap::new()));
    // 第 1 步：已中止的调用不启动任何任务，也不触碰存储。
    if caller_signal.is_aborted() {
        return Ok(ModelsRefreshResult {
            aborted: true,
            errors: IndexMap::new(),
        });
    }
    // 第 2 步：`providers` 是白名单；空列表意味着「一个都不刷新」，不是「全部」。
    let selected: Option<HashSet<String>> = options
        .providers
        .as_ref()
        .map(|providers| providers.iter().cloned().collect());
    let refreshable: Vec<Arc<dyn Provider>> = {
        let providers = state.providers.lock().unwrap_or_else(|e| e.into_inner());
        providers
            .values()
            .filter(|provider| provider.has_refresh_models())
            .filter(|provider| {
                selected
                    .as_ref()
                    .is_none_or(|selected| selected.contains(provider.id()))
            })
            .cloned()
            .collect()
    };

    let mut tasks = Vec::with_capacity(refreshable.len());
    // 第 3 步：为每个 provider 建立本轮刷新的代际/取消边界，然后并发执行两阶段刷新。
    for provider in refreshable {
        // TS 的 `refreshable.map(async …)` 在首个 await 之前同步调用 `beginProviderRefresh`；
        // 代际提升与 controller 安装必须在本次 `refresh()` 的第一次轮询内完成，否则随后到达的
        // `setProvider`/`deleteProvider` 会先提升代际、再被任务里的 begin 覆盖，发布不再被取代。
        let provider_id = provider.id().to_owned();
        let (generation, controller) = state.begin_provider_refresh(&provider_id);
        let state = Arc::clone(&state);
        let caller_signal = caller_signal.clone();
        let errors = Arc::clone(&errors);
        let force = options.force;
        // 合并「调用方 signal」与「本 provider 的 controller」：任一来源中止都让任务收敛。
        // 丢弃 `CombinedAbortSignal.cleanup` 句柄是安全的——该模块的监听只在显式 cleanup
        // 时移除，句柄被 drop 不会停止传播（见 utils/abort_signals.rs 的模块文档）。
        let combined = combine_abort_signals(&[Some(caller_signal), Some((*controller).clone())])
            .signal
            .expect("two abort signals always combine");
        tasks.push(tokio::spawn(async move {
            let operation = {
                let state = Arc::clone(&state);
                let provider = Arc::clone(&provider);
                let provider_id = provider_id.clone();
                let combined = combined.clone();
                async move {
                    // 凭证读取失败先暂存而不是立即返回：缓存恢复阶段不依赖凭证，先让已缓存的
                    // 目录生效，再把错误作为本 provider 的刷新失败上报（与 TS 顺序一致）。
                    let mut stored_credential: Option<Credential> = None;
                    let mut credential_error: Option<AuthError> = None;
                    match read_credential(&state, &provider_id, &combined).await {
                        Ok(credential) => stored_credential = credential,
                        Err(error) => credential_error = Some(error),
                    }

                    // 先恢复缓存目录，再做 auth 解析或网络访问。
                    // 该阶段不依赖凭证，即使上面的读取失败也要先让已缓存的目录生效，错误在
                    // 阶段结束后再上报（与 TS 的步骤顺序一致）。
                    run_provider_refresh_phase(
                        Arc::clone(&state),
                        Arc::clone(&provider),
                        stored_credential.clone(),
                        false,
                        None,
                        generation,
                        combined.clone(),
                    )
                    .await?;
                    if let Some(error) = credential_error {
                        return Err(ai_error_from_auth_error(error));
                    }
                    if !allow_network || combined.is_aborted() {
                        return Ok(());
                    }

                    // 第二阶段（联网）：解析可用凭证；`Ok(None)` 表示没有可联网凭证，静默跳过。
                    let credential = resolve_refresh_credential(
                        Arc::clone(&state),
                        &provider,
                        stored_credential.as_ref(),
                        &combined,
                    )
                    .await?;
                    let Some(credential) = credential else {
                        return Ok(());
                    };
                    run_provider_refresh_phase(
                        state,
                        provider,
                        Some(credential),
                        true,
                        force,
                        generation,
                        combined.clone(),
                    )
                    .await
                }
            };

            let result = race_with_abort_signal(operation, &combined).await;
            let abort_error = match result {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error),
                Err(error) => Some(error),
            };
            // 只看「未被中止」的失败：中止引发的错误是预期控制流，不应污染 errors。
            if let Some(error) = abort_error
                && !combined.is_aborted()
            {
                errors
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(provider_id.clone(), error);
            }
            // finally：若当前 controller 仍是该 provider 的，则删除。
            // 用 Arc::ptr_eq 而不是 id 比较，避免误删后续刷新安装的新 controller。
            let mut controllers = state
                .refresh_controllers
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if controllers
                .get(&provider_id)
                .is_some_and(|current| Arc::ptr_eq(current, &controller))
            {
                controllers.remove(&provider_id);
            }
        }));
    }

    // 第 4 步：等待全部任务。调用方 signal 中止时 `race_with_abort_signal` 返回中止错误，
    // 这里不把它当成失败（各任务会自行收敛并清理 controller），而是继续返回部分结果。
    let all = futures::future::join_all(tasks);
    match race_with_abort_signal(all, &caller_signal).await {
        Ok(joined) => {
            for result in joined {
                if let Err(error) = result
                    && error.is_panic()
                {
                    // provider 代码 panic 属于实现 bug，按 TS 的未处理 rejection 原样传播。
                    std::panic::resume_unwind(error.into_panic());
                }
            }
        }
        Err(error) => {
            if !caller_signal.is_aborted() {
                return Err(error);
            }
        }
    }

    // 第 5 步：交出错误快照与中止状态；`aborted` 取结束时刻的 signal 状态（而非进入时）。
    let errors = std::mem::take(&mut *errors.lock().unwrap_or_else(|e| e.into_inner()));
    Ok(ModelsRefreshResult {
        aborted: caller_signal.is_aborted(),
        errors,
    })
}

// ---------------------------------------------------------------------------
// auth 辅助
// ---------------------------------------------------------------------------

/// 未知 provider → code `provider` 的 `ModelsError`。
///
/// 消息格式固定为 `Unknown provider: {id}`（TS 逐字），上层的错误展示依赖它；查找只需
/// 克隆一个 `Arc`，不持有 provider 的引用计数以外的资源。
fn require_provider(state: &ModelsState, model: &Model) -> Result<Arc<dyn Provider>, ModelsError> {
    state
        .providers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&model.provider)
        .cloned()
        .ok_or_else(|| {
            models_error(
                TsModelsErrorCode::Provider,
                &format!("Unknown provider: {}", model.provider),
                None,
            )
        })
}

/// `requireChatProvider(model)`：先断言 chat，再查 provider。
///
/// 顺序固定：非 chat 模型即使 provider 也未注册，报的是「不是 chat 模型」而不是
/// 「未知 provider」（先按模型类型给出更精确的错误）。两条错误分别是
/// `types::require_chat` 的错误与 `require_provider` 的 code `provider`。
fn require_chat_provider(
    state: &ModelsState,
    model: &Model,
) -> Result<Arc<dyn Provider>, ModelsError> {
    crate::types::require_chat(model).map_err(|error| error.into_models_error())?;
    require_provider(state, model)
}

/// `checkProviderAuth(provider, credential, signal)`。
///
/// 按凭证类型分三支，均**不主动刷新** OAuth：
/// 1. 存储的是 OAuth：provider 仍声明 OAuth 即视为已鉴权（`source = "OAuth"`），不校验
///    过期时间；provider 不再支持 OAuth 则返回 `None`。
/// 2. 有 API key 处理器且声明了 `check`：交给它校验（可联网）；错误包装成 code `auth` 的
///    `AuthError`，保留原始抛出值作为 cause。
/// 3. 其余情况：走通用 `resolve_provider_auth`，能解析出凭证即已鉴权，`type` 固定
///    `api_key`，`source` 取解析来源（env/存储等）。
async fn check_provider_auth(
    state: &ModelsState,
    provider: &Arc<dyn Provider>,
    credential: Option<&Credential>,
    signal: &AbortScope,
) -> Result<Option<AuthCheck>, AuthError> {
    if matches!(credential, Some(Credential::OAuth(_))) {
        return Ok(if provider.auth().oauth.is_some() {
            Some(AuthCheck {
                source: Some("OAuth".into()),
                r#type: AuthType::OAuth,
            })
        } else {
            None
        });
    }
    let Some(api_key) = provider.auth().api_key.clone() else {
        return Ok(None);
    };
    if api_key.has_check() {
        let credential = match credential {
            Some(Credential::ApiKey(credential)) => Some(credential),
            _ => None,
        };
        return match api_key
            .check(ApiKeyAuthInput {
                ctx: state.auth_context.as_ref(),
                credential,
                signal,
            })
            .await
        {
            Ok(check) => Ok(check),
            Err(error) => Err(AuthError::models(
                ModelsErrorCode::Auth,
                format!("API key auth check failed for provider {}", provider.id()),
                Some(&error.as_thrown_value()),
            )),
        };
    }

    match resolve_provider_auth(
        provider.id(),
        provider.auth(),
        Arc::clone(&state.credentials),
        Arc::clone(&state.auth_context),
        AuthResolutionOverrides {
            signal: Some(signal.clone()),
            ..Default::default()
        },
    )
    .await
    {
        Ok(Some(resolution)) => Ok(Some(AuthCheck {
            source: resolution.source,
            r#type: AuthType::ApiKey,
        })),
        Ok(None) => Ok(None),
        Err(error) => Err(error),
    }
}

/// `getAuthenticatedProviders(providerId, signal)`。
///
/// 返回 `(provider, 存储凭证, auth 检查结果)` 三元组，只保留检查结果为「已鉴权」的 provider；
/// 顺序与 provider 注册顺序一致。`provider_id` 为 `None`/空串时扫描全部；指定了不存在的
/// id 时返回空列表（不是错误）。任一凭证读取或 auth 检查失败都会让整个调用返回第一个
/// 错误（TS 的 `Promise.all` 语义）。
async fn get_authenticated_providers(
    state: &ModelsState,
    provider_id: Option<&str>,
    signal: &AbortScope,
) -> Result<Vec<(Arc<dyn Provider>, Option<Credential>, AuthCheck)>, AuthError> {
    throw_if_aborted(signal)?;
    // TS: `providerId ? [...] : this.getProviders()` —— 空串是 falsy，同样扫描全部 provider。
    let providers: Vec<Arc<dyn Provider>> = match provider_id.filter(|id| !id.is_empty()) {
        Some(provider_id) => state
            .providers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(provider_id)
            .cloned()
            .into_iter()
            .collect(),
        None => state
            .providers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect(),
    };
    // TS 的 `Promise.all(providers.map(async …))`：每个 provider 的凭证读取与 auth 检查并发，
    // 结果按 provider 顺序返回；任一拒绝按完成顺序传播（`try_join_all` 的语义）。
    let checks = providers.into_iter().map(|provider| {
        let signal = signal.clone();
        async move {
            let credential = read_credential(state, provider.id(), &signal).await?;
            let auth = check_provider_auth(state, &provider, credential.as_ref(), &signal).await?;
            Ok::<_, AuthError>((provider, credential, auth))
        }
    });
    let resolved = futures::future::try_join_all(checks).await?;
    Ok(resolved
        .into_iter()
        .filter_map(|(provider, credential, auth)| auth.map(|auth| (provider, credential, auth)))
        .collect())
}

/// 按 `model.type`（缺省 chat）构造 [`AnyModel`] 视图，供类型断言使用。
///
/// 只用于 `assert_image_model` / `assert_classifier_model` 等静态检查；模型的其它字段按
/// [`Model`] 原样克隆，不读取 `AnyModel::Remote` 的原始 JSON。
fn typed_any_model(model: &Model) -> AnyModel {
    match model.model_type.unwrap_or(ModelType::Chat) {
        ModelType::Chat => AnyModel::Chat(model.clone()),
        ModelType::Image => AnyModel::Image(model.clone()),
        ModelType::Classifier => AnyModel::Classifier(model.clone()),
    }
}

/// 把错误转成 `imageErrorResult` / `classifierErrorResult` 需要的抛出值。
///
/// 只保留 `name` 与 `message`（`stack`、`code` 置空）；`name` 随 [`ThrownValue`] 携带，但
/// 当前错误结果只消费 `message`（`thrown_message` 对 Error 分支只读 `info.message`），
/// `name` 不进入任何输出字段。
fn to_thrown_value(error: AiError) -> ThrownValue {
    ThrownValue::Error(DiagnosticErrorInfo {
        name: Some(error.name),
        message: error.message,
        stack: None,
        code: None,
    })
}

/// `signal.reason` → `AuthError`（缺省 `AbortError`）。
///
/// 有显式 reason（TS 的 `controller.abort(reason)`）时原样取它的 `name`/`message`；没有时用
/// `AbortReason::fallback_abort_error()`（`"The operation was aborted"`），对应 TS 的
/// `abortReason()` 对 `undefined` reason 的回退分支。
fn abort_auth_error(signal: &AbortScope) -> AuthError {
    let reason = signal
        .reason()
        .unwrap_or_else(crate::utils::abort::AbortReason::fallback_abort_error);
    AuthError::with_name(reason.name, reason.message)
}

/// `mergeClientIdentity(this.identity, options?.identity)`。
///
/// 集合级身份是可选的默认值，请求级身份逐字段覆盖；两者都缺省时返回 `None`（不构造空
/// 结构）。合并规则见 `utils::identity::merge_client_identity`。
fn merge_request_identity(
    state: &ModelsState,
    options_identity: Option<&ClientIdentity>,
) -> Option<ClientIdentity> {
    merge_client_identity([state.identity.clone(), options_identity.cloned()])
}

/// TS 的请求 signal 是 `AbortSignal`：`AbortController.abort()` 的缺省 reason 是
/// DOMException `"This operation was aborted"`；`abortReason()` 的
/// `"The operation was aborted"` 只用于 reason 为 `undefined` 的 signal。Rust 的
/// `StreamOptions.signal` 是裸 [`CancellationToken`]（不携带 reason），调用方用
/// `AbortScope::abort()` 的等价物取消，因此请求路径把回退文案归一为 Node 的缺省 reason。
///
/// 调用方通过 [`ModelsStreamOptions::signal_reason`] 等字段显式传入 TS 的 `signal.reason`
/// 时优先使用它（`controller.abort(new Error("..."))` 的 `name`/`message`）。
fn normalize_request_abort_error(
    error: AuthError,
    custom_reason: Option<&AbortReason>,
) -> AuthError {
    if error.is_aborted() {
        if let Some(reason) = custom_reason {
            return AuthError::with_name(reason.name.clone(), reason.message.clone());
        }
        if error.message == AbortReason::fallback_abort_error().message {
            return AuthError::with_name(error.name, AbortReason::default_abort_error().message);
        }
    }
    error
}

/// `applyAuth(model, options)`：解析 auth 并组装请求头/env/identity。
///
/// 返回 `(request_model, request_options)`：`request_model` 可能被 provider auth 解析出的
/// `baseUrl` 替换（空串视为未提供），`request_options` 是原样透传的 [`StreamOptions`] 加上
/// 解析出的 api key、合并后的头、env、identity 与 `extra["reasoning"]`。
///
/// 失败路径：未知 provider、未配置凭证（code `auth`）、鉴权解析错误都会转成 [`AiError`]；
/// 中止错误会先用 `normalize_request_abort_error` 还原 `signal.reason` 的文案。
#[allow(clippy::result_large_err)]
async fn apply_auth(
    state: &ModelsState,
    model: &Model,
    options: ModelsStreamOptions,
) -> Result<(Model, StreamOptions), AiError> {
    // 先确认 provider 存在：未知 provider 不应该走到凭证解析（错误消息也不同）。
    require_provider(state, model).map_err(ai_error_from_models_error)?;
    let signal_reason = options.signal_reason.clone();
    // `StreamOptions.signal` 是裸 CancellationToken：包一层 AbortScope 是为了交给
    // resolve_auth 的统一 race/throw_if_aborted 流程；由于 `AbortScope::from(token)` 新建的
    // reason 槽恒为 `None`（token 取消不会把调用方的 reason 带进来），中止文案由
    // `signal_reason` 或 `normalize_request_abort_error` 的缺省回退决定。
    // 克隆出来的 scope 与原 token 共享取消状态。
    let signal = options.base.signal.clone().map(AbortScope::from);
    // 请求级 apiKey/env 作为覆盖传给解析器；signal 决定中止语义。
    let resolution = resolve_auth_for_model(
        state,
        model,
        AuthResolutionOverrides {
            api_key: options.base.api_key.clone(),
            env: options.base.env.clone(),
            signal: signal.clone(),
            ..Default::default()
        },
    )
    .await
    .map_err(|error| normalize_request_abort_error(error, signal_reason.as_ref()))
    .map_err(ai_error_from_auth_error)?;
    let Some(resolution) = resolution else {
        return Err(ai_error_from_models_error(models_error(
            TsModelsErrorCode::Auth,
            &format!("Provider is not configured: {}", model.provider),
            None,
        )));
    };
    let auth = resolution.auth;

    // 显式请求选项逐字段优先；Models 专属的 transform 最后执行。
    let api_key = options.base.api_key.clone().or(auth.api_key);
    // TS: `auth.baseUrl ? {...model, baseUrl} : model` —— 空串是 falsy，沿用模型自身的 baseUrl。
    let request_model = match auth.base_url {
        Some(base_url) if !base_url.is_empty() => Model {
            base_url,
            ..model.clone()
        },
        _ => model.clone(),
    };
    let session_id = options.base.session_id.clone();
    // 头合并优先级（后者覆盖前者，大小写不敏感）：OpenCode session 头 < provider auth 头 <
    // 显式请求头 < transformHeaders。前三步的 `None`/空 map 语义由 merge_headers 保证。
    let mut headers = merge_headers(
        merge_headers(
            crate::utils::identity::get_open_code_session_headers(
                &crate::utils::identity::ClientIdentityTarget::new(
                    &request_model.provider,
                    &request_model.base_url,
                ),
                session_id.as_deref(),
            ),
            auth.headers.clone(),
        ),
        options.base.headers.clone(),
    );
    if let Some(transform) = &options.transforms.transform_headers {
        headers = Some(transform(headers.unwrap_or_default()).await);
    }
    // env 合并：请求级 env 覆盖解析出的 env（同 key 取请求值）；两边都空时保持 None，
    // 与「显式传入空 env」区分（两者在 TS 里分别是 `undefined` 与 `{}`）。
    let env = if resolution.env.is_some() || options.base.env.is_some() {
        let mut merged = resolution.env.unwrap_or_default();
        if let Some(request_env) = options.base.env.clone() {
            for (key, value) in request_env {
                merged.insert(key, value);
            }
        }
        Some(merged)
    } else {
        None
    };
    let identity = merge_request_identity(state, options.base.identity.as_ref());

    let reasoning = options.reasoning;
    let mut request_options = options.base;
    request_options.api_key = api_key;
    request_options.headers = headers;
    request_options.env = env;
    request_options.identity = identity;
    // TS 的 `providerOptions` 展开把 `reasoning` 一并交给适配器；Rust 侧它以
    // `ModelsStreamOptions.reasoning` 承载，这里按同名键并进 extra（显式字段优先）。
    if let Some(reasoning) = reasoning {
        request_options
            .extra
            .insert("reasoning".into(), serde_json::Value::String(reasoning));
    }
    Ok((request_model, request_options))
}

/// `Model.headers`（`Record<string, string>`）→ [`crate::types::ProviderHeaders`]（值全部为 Some）。
///
/// TS 的模型静态头是纯字符串映射，没有 `null` 抑制语义；因此这里每个值都包成 `Some`，
/// 合并时会覆盖同名的 provider 头，而不是删除它。
fn model_headers_to_provider_headers(
    headers: &IndexMap<String, String>,
) -> crate::types::ProviderHeaders {
    headers
        .iter()
        .map(|(name, value)| (name.clone(), Some(value.clone())))
        .collect()
}

/// `getAuth(model, overrides)`（含模型静态头合并），不经过 trait 的动态分派。
///
/// 请求路径（`apply_auth`）只持有 `&ModelsState` 而没有 `ModelsImpl` 实例，因此这里直接以
/// 状态为输入复刻 [`Models::get_auth`] 的 `Model` 分支。未知 provider 返回 `Ok(None)`；
/// 解析成功后，模型静态头在 provider 级 auth 头之上做一次大小写不敏感覆盖。
async fn resolve_auth_for_model(
    state: &ModelsState,
    model: &Model,
    overrides: AuthResolutionOverrides,
) -> Result<Option<AuthResult>, AuthError> {
    // 入参 signal 为 None 时具体化为永不中止的 scope，后续无需判空；它同时写回 overrides，
    // 保证解析器看到的是同一个 scope。
    let signal = operation_signal(overrides.signal.clone());
    let provider = state
        .providers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&model.provider)
        .cloned();
    let Some(provider) = provider else {
        return Ok(None);
    };
    let result = resolve_provider_auth(
        provider.id(),
        provider.auth(),
        Arc::clone(&state.credentials),
        Arc::clone(&state.auth_context),
        AuthResolutionOverrides {
            signal: Some(signal),
            ..overrides
        },
    )
    .await?;
    let Some(result) = result else {
        return Ok(None);
    };
    let Some(headers) = model.headers.as_ref() else {
        return Ok(Some(result));
    };
    Ok(Some(AuthResult {
        auth: crate::auth::types::ModelAuth {
            headers: merge_headers(
                result.auth.headers,
                Some(model_headers_to_provider_headers(headers)),
            ),
            ..result.auth
        },
        ..result
    }))
}

/// `stream()` 之后等待结果；结果通道发送端被丢弃（实现 bug）时用同一套 setup error 消息兜底。
///
/// `result()` 的 `Err` 只对应结果通道发送端关闭；`stream_result` 自己持有流句柄，因此该
/// 分支在正常调用中不可达（`end_without_result()` 结束流时句柄仍在，`result()` 会按
/// `event_stream.rs` 的约定永远 pending，而不是返回 `Err`）。这里构造
/// `stopReason = "error"` 的消息而不是 panic，仅作为防御性兜底；时间戳取当前毫秒。
async fn stream_result(stream: AssistantMessageEventStream, model: &Model) -> AssistantMessage {
    match stream.result().await {
        Ok(message) => message,
        Err(_) => crate::api::lazy::create_setup_error_message(
            model,
            &AiError::new(
                AiErrorKind::Other,
                "assistant message event stream ended without a result",
            ),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_millis() as i64),
        ),
    }
}

/// 请求选项中的 reasoning 名字（`SimpleStreamOptions.reasoning`）。
fn thinking_level_name(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::Xhigh => "xhigh",
        ThinkingLevel::Max => "max",
    }
}

/// `observeModelRequest` 的入参（TS 的 `options` 里被观察的两个字段）。
fn observe_options(
    telemetry: Option<Arc<dyn kiana_telemetry::TelemetrySpan>>,
    reasoning: Option<String>,
) -> crate::telemetry::ObserveModelRequestOptions {
    crate::telemetry::ObserveModelRequestOptions {
        telemetry,
        reasoning,
    }
}

#[async_trait]
impl Models for ModelsImpl {
    fn get_providers(&self) -> Vec<Arc<dyn Provider>> {
        self.state
            .providers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }

    fn get_provider(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.state
            .providers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    fn get_models(&self, provider: Option<&str>) -> Vec<Model> {
        let providers: Vec<Arc<dyn Provider>> = match provider {
            Some(id) => self.get_provider(id).into_iter().collect(),
            None => self.get_providers(),
        };
        let single = provider.is_some();
        let mut models = Vec::new();
        for provider in providers {
            // TS 的 try/catch：坏 provider 抛错只影响它自己。指定单个 provider 时直接返回
            // 空列表（TS 的 catch 返回 `[]`）；扫描全部时跳过出错的 provider 继续拼接。
            match std::panic::catch_unwind(AssertUnwindSafe(|| provider.get_models())) {
                Ok(list) => models.extend(list),
                // Best-effort: ill-behaved providers yield no models.
                Err(_) if single => return Vec::new(),
                Err(_) => {}
            }
        }
        models
    }

    fn get_model(&self, provider: &str, id: &str) -> Option<Model> {
        self.get_models(Some(provider))
            .into_iter()
            .find(|model| model.id == id)
    }

    fn get_models_of_type(&self, model_type: ModelType, provider: Option<&str>) -> Vec<Model> {
        self.get_all_models(provider)
            .into_iter()
            .filter(|model| model.model_type() == model_type)
            .map(|model| {
                let mut model = model.model().clone();
                // TS 的 image/classifier 模型对象带 `type` 字段；`AnyModel` 的变体可能没同步
                // 到内层 `Model`，这里按查询类型补齐（chat 保持原样：缺省即 chat）。
                if model_type != ModelType::Chat {
                    model.model_type = Some(model_type);
                }
                model
            })
            .collect()
    }

    fn get_model_of_type(&self, model_type: ModelType, provider: &str, id: &str) -> Option<Model> {
        self.get_models_of_type(model_type, Some(provider))
            .into_iter()
            .find(|model| model.id == id)
    }

    fn get_all_models(&self, provider: Option<&str>) -> Vec<AnyModel> {
        let providers: Vec<Arc<dyn Provider>> = match provider {
            Some(id) => self.get_provider(id).into_iter().collect(),
            None => self.get_providers(),
        };
        let single = provider.is_some();
        let mut models = Vec::new();
        for provider in providers {
            // 与 `get_models` 相同的 best-effort 语义；区别是保留 image/classifier 模型。
            let list = std::panic::catch_unwind(AssertUnwindSafe(|| provider.get_all_models()));
            match list {
                Ok(list) => models.extend(list),
                Err(_) if single => return Vec::new(),
                Err(_) => {}
            }
        }
        models
    }

    async fn refresh(&self, options: ModelsRefreshOptions) -> Result<ModelsRefreshResult, AiError> {
        // 全部协调逻辑在 `refresh_models`；先把状态 Arc 克隆出来传给它，避免其内部依赖
        // &self（返回的 future 本身仍借用 &self）。
        refresh_models(Arc::clone(&self.state), options).await
    }

    async fn check_auth(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<Option<AuthCheck>, AuthError> {
        let signal = operation_signal(options.signal);
        let state = Arc::clone(&self.state);
        let provider_id_owned = provider_id.to_owned();
        let check_signal = signal.clone();
        // 先检查中止，再查 provider：未知 provider 返回 `None`（不是错误）。整个内部
        // future 之后还要与 signal 竞速一次，覆盖检查开始后才到达的中止。
        let check = async move {
            throw_if_aborted(&check_signal)?;
            let provider = state
                .providers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&provider_id_owned)
                .cloned();
            let Some(provider) = provider else {
                return Ok::<_, AuthError>(None);
            };
            let credential = read_credential(&state, &provider_id_owned, &check_signal).await?;
            check_provider_auth(&state, &provider, credential.as_ref(), &check_signal).await
        };
        match race_with_abort_signal(check, &signal).await {
            Ok(result) => result,
            Err(error) => Err(AuthError::from_ai_error(&error)),
        }
    }

    async fn get_available(
        &self,
        provider_id: Option<&str>,
        options: AuthOperationOptions,
    ) -> Result<Vec<Model>, AuthError> {
        let signal = operation_signal(options.signal);
        let state = Arc::clone(&self.state);
        let provider_id_owned = provider_id.map(str::to_owned);
        let inner_signal = signal.clone();
        let available = async move {
            let providers =
                get_authenticated_providers(&state, provider_id_owned.as_deref(), &inner_signal)
                    .await?;
            Ok::<_, AuthError>(
                providers
                    .into_iter()
                    .flat_map(|(provider, credential, _)| {
                        // TS: `provider.filterModels?.(models, credential) ?? models`。默认
                        // `filter_models` 恒等，直接调用即等价（覆盖了方法的 provider 必生效）。
                        provider.filter_models(provider.get_models(), credential.as_ref())
                    })
                    .collect(),
            )
        };
        // 与调用方 signal 竞速；中止时把 AiError 转回 AuthError（保留 reason 的 name/message）。
        match race_with_abort_signal(available, &signal).await {
            Ok(result) => result,
            Err(error) => Err(AuthError::from_ai_error(&error)),
        }
    }

    async fn get_available_of_type(
        &self,
        model_type: ModelType,
        provider_id: Option<&str>,
        options: AuthOperationOptions,
    ) -> Result<Vec<Model>, AuthError> {
        let models = self
            .get_all_available(provider_id, options)
            .await?
            .into_iter()
            .filter(|model| model.model_type() == model_type)
            .map(|model| {
                let mut model = model.model().clone();
                // 与 `get_models_of_type` 一致：非 chat 模型按查询类型补齐内层 `type`
                // （TS 的 image/classifier 模型对象总带 `type`）。
                if model_type != ModelType::Chat {
                    model.model_type = Some(model_type);
                }
                model
            })
            .collect();
        Ok(models)
    }

    async fn get_all_available(
        &self,
        provider_id: Option<&str>,
        options: AuthOperationOptions,
    ) -> Result<Vec<AnyModel>, AuthError> {
        let signal = operation_signal(options.signal);
        let state = Arc::clone(&self.state);
        let provider_id_owned = provider_id.map(str::to_owned);
        let inner_signal = signal.clone();
        let available = async move {
            let providers =
                get_authenticated_providers(&state, provider_id_owned.as_deref(), &inner_signal)
                    .await?;
            Ok::<_, AuthError>(
                providers
                    .into_iter()
                    .flat_map(|(provider, credential, _)| {
                        // TS: `provider.getAllModels?.() ?? provider.getModels()` —— 默认
                        // `get_all_models` 就是 `getModels()` 的 chat 列表，直接调用即等价。
                        let models = provider.get_all_models();
                        // TS: `if (provider.filterAllModels) return ...; if (!provider.filterModels)
                        // return models;` 之后的回退。默认 `filter_all_models` 就是该回退；
                        // 默认 `filter_models` 恒等，因此直接调用与 TS 的分支等价。
                        provider.filter_all_models(models, credential.as_ref())
                    })
                    .collect(),
            )
        };
        // 与 `get_available` 相同的中止语义：内部任务与 signal 竞速，错误统一转回 AuthError。
        match race_with_abort_signal(available, &signal).await {
            Ok(result) => result,
            Err(error) => Err(AuthError::from_ai_error(&error)),
        }
    }

    async fn get_auth(
        &self,
        target: GetAuthTarget<'_>,
        overrides: AuthResolutionOverrides,
    ) -> Result<Option<AuthResult>, AuthError> {
        let signal = operation_signal(overrides.signal.clone());
        // 两种 target 都归结为 provider id；`GetAuthTarget::Model` 额外在末尾合并模型静态头。
        let provider_id = match target {
            GetAuthTarget::Provider(provider_id) => provider_id.to_owned(),
            GetAuthTarget::Model(model) => model.provider.clone(),
        };
        let provider = self
            .state
            .providers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&provider_id)
            .cloned();
        // 未知 provider 返回 `None`（TS 的 `getAuth` 同）；不报错，调用方可据此判断未注册。
        let Some(provider) = provider else {
            return Ok(None);
        };
        let result = resolve_provider_auth(
            provider.id(),
            provider.auth(),
            Arc::clone(&self.state.credentials),
            Arc::clone(&self.state.auth_context),
            AuthResolutionOverrides {
                signal: Some(signal),
                ..overrides
            },
        )
        .await?;
        // Provider 目标到此为止；Model 目标继续做静态头合并。
        let model = match target {
            GetAuthTarget::Provider(_) => return Ok(result),
            GetAuthTarget::Model(model) => model,
        };
        let Some(result) = result else {
            return Ok(None);
        };
        let Some(headers) = model.headers.clone() else {
            return Ok(Some(result));
        };
        Ok(Some(AuthResult {
            auth: crate::auth::types::ModelAuth {
                headers: merge_headers(
                    result.auth.headers,
                    Some(model_headers_to_provider_headers(&headers)),
                ),
                ..result.auth
            },
            ..result
        }))
    }

    async fn login(
        &self,
        provider_id: &str,
        auth_type: AuthType,
        interaction: &dyn AuthInteraction,
        options: LoginOptions,
    ) -> Result<Credential, AuthError> {
        let signal = operation_signal(Some(interaction.signal().clone()));
        throw_if_aborted(&signal)?;
        let provider = self
            .state
            .providers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(provider_id)
            .cloned();
        let Some(provider) = provider else {
            return Err(AuthError::models(
                ModelsErrorCode::Provider,
                format!("Unknown provider: {provider_id}"),
                None,
            ));
        };
        // 能力检查先于登录动作：provider 没有对应 auth 实现（或 API key 没有 login）时报
        // code `auth` 的错误，消息与 TS 的 `${provider.name} does not support ${type} login`
        // 一致。OAuth 分支用 `provider.name()`，API key 分支同样。
        let credential = match auth_type {
            AuthType::OAuth => {
                let Some(oauth) = provider.auth().oauth.clone() else {
                    return Err(AuthError::models(
                        ModelsErrorCode::Auth,
                        format!("{} does not support oauth login", provider.name()),
                        None,
                    ));
                };
                let operation = oauth.login(interaction, &options);
                tokio::pin!(operation);
                // TS: `raceWithAbortSignal(loginOperation, signal)` 在 signal 已中止时**无条件**
                // 先拒绝（即使登录 future 已经 settle）。
                if signal.is_aborted() {
                    return Err(abort_auth_error(&signal));
                }
                let result = tokio::select! {
                    result = &mut operation => result,
                    _ = signal.cancelled() => {
                        return Err(abort_auth_error(&signal));
                    }
                };
                // 登录 future 在轮询期间同步中止了 signal（TS 里 `method.login` 同步调用
                // `controller.abort()` 后仍返回凭证）：TS 的 race 同样以中止拒绝。
                if signal.is_aborted() {
                    return Err(abort_auth_error(&signal));
                }
                let credential = result?;
                Credential::OAuth(credential)
            }
            AuthType::ApiKey => {
                let Some(api_key) = provider.auth().api_key.clone() else {
                    return Err(AuthError::models(
                        ModelsErrorCode::Auth,
                        format!("{} does not support api_key login", provider.name()),
                        None,
                    ));
                };
                if !api_key.has_login() {
                    return Err(AuthError::models(
                        ModelsErrorCode::Auth,
                        format!("{} does not support api_key login", provider.name()),
                        None,
                    ));
                }
                let operation = api_key.login(interaction);
                tokio::pin!(operation);
                if signal.is_aborted() {
                    return Err(abort_auth_error(&signal));
                }
                let result = tokio::select! {
                    result = &mut operation => result,
                    _ = signal.cancelled() => {
                        return Err(abort_auth_error(&signal));
                    }
                };
                if signal.is_aborted() {
                    return Err(abort_auth_error(&signal));
                }
                let credential = result?;
                Credential::ApiKey(credential)
            }
        };

        // 登录成功后写盘（modify）一旦开始就不可取消；未开始时可因取消而放弃。
        //
        // `started` + oneshot 的握手用于区分这两种时刻：闭包真正被 `CredentialStore` 调用
        // 时先置位并发信号，随后 `select!` 的 started 分支改为等待 `modify` 完成；若取消先
        // 到且闭包还没开始，则直接返回中止错误，让存储保持原样（避免半写）。
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (started_tx, mut started_rx) = tokio::sync::oneshot::channel::<()>();
        let credential_for_modify = credential.clone();
        let started_for_modify = Arc::clone(&started);
        let modify = self.state.credentials.modify(
            provider_id,
            Box::new(move |_current| {
                started_for_modify.store(true, std::sync::atomic::Ordering::SeqCst);
                let _ = started_tx.send(());
                let credential = credential_for_modify.clone();
                Box::pin(async move { Ok(Some(credential)) })
            }),
            AuthOperationOptions {
                signal: Some(signal.clone()),
            },
        );
        tokio::pin!(modify);
        let result: Result<Option<Credential>, AuthError> = tokio::select! {
            result = &mut modify => result,
            _ = &mut started_rx => modify.await,
            _ = signal.cancelled() => {
                if !started.load(std::sync::atomic::Ordering::SeqCst) {
                    return Err(abort_auth_error(&signal));
                }
                modify.await
            }
        };
        match result {
            Ok(_) => Ok(credential),
            Err(error) => {
                throw_if_aborted(&signal)?;
                Err(AuthError::models(
                    ModelsErrorCode::Auth,
                    format!("Credential store modify failed for {provider_id}"),
                    Some(&error.as_thrown_value()),
                ))
            }
        }
    }

    async fn logout(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<(), AuthError> {
        let signal = operation_signal(options.signal);
        // 先检查中止；删除失败时若 signal 已中止，优先返回中止错误而不是存储错误
        // （与 TS 的 `catch { signal.throwIfAborted(); ... }` 顺序一致）。
        throw_if_aborted(&signal)?;
        match self
            .state
            .credentials
            .delete(
                provider_id,
                AuthOperationOptions {
                    signal: Some(signal.clone()),
                },
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => {
                throw_if_aborted(&signal)?;
                Err(AuthError::models(
                    ModelsErrorCode::Auth,
                    format!("Credential store delete failed for {provider_id}"),
                    Some(&error.as_thrown_value()),
                ))
            }
        }
    }

    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: ModelsStreamOptions,
    ) -> AssistantMessageEventStream {
        // `normalizeContext` 在返回流之前同步执行（TS 同）：调用方拿到流时 transcript 已固定，
        // 再修改原 context 不会影响本次请求。
        let transcript = normalize_context(context);
        // TS 的 `observeModelRequest` 读 `typeof options.reasoning === "string"`；同一字段
        // 也可能来自调用方直接放进 `extra` 的任意额外选项。
        let reasoning = options.reasoning.clone().or_else(|| {
            options
                .base
                .extra
                .get("reasoning")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
        let observer = observe_model_request(
            model,
            observe_options(options.base.telemetry.clone(), reasoning),
            false,
        );
        let state = Arc::clone(&self.state);
        let model_for_setup = model.clone();
        // setup 的同步前缀在 lazy_stream 返回前被 poll 一次（chat/provider 检查与鉴权调用
        // 的发起），其余部分由后台任务继续；顺序（chat 检查 → apply_auth → provider.stream）
        // 决定失败时的错误类型：未知 provider / 非 chat 在鉴权之前，未配置凭证在之后。
        let setup: LazySetup = Box::pin(async move {
            let provider = require_chat_provider(&state, &model_for_setup)
                .map_err(ai_error_from_models_error)?;
            let (request_model, request_options) =
                apply_auth(&state, &model_for_setup, options).await?;
            Ok::<_, AiError>(provider.stream(&request_model, &transcript, request_options))
        });
        lazy_stream(model, setup, Some(observer))
    }

    async fn complete(
        &self,
        model: &Model,
        context: &Context,
        options: ModelsStreamOptions,
    ) -> AssistantMessage {
        let stream = self.stream(model, context, options);
        stream_result(stream, model).await
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: ModelsSimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        // 简单选项里的 reasoning 是枚举，telemetry 使用它的字符串名（TS 的 thinking level）。
        let transcript = normalize_context(context);
        let reasoning = options
            .base
            .reasoning
            .map(thinking_level_name)
            .map(str::to_owned);
        let observer = observe_model_request(
            model,
            observe_options(options.base.base.telemetry.clone(), reasoning),
            false,
        );
        let state = Arc::clone(&self.state);
        let model_for_setup = model.clone();
        // `ModelsSimpleStreamOptions.base` 是 SimpleStreamOptions，其 `.base` 才是底层
        // StreamOptions；apply_auth 只处理后者（鉴权/头/env/identity），reasoning 与
        // tool_choice 等字段在下面重新组装进 SimpleStreamOptions 交给 provider。
        let setup: LazySetup = Box::pin(async move {
            let provider = require_chat_provider(&state, &model_for_setup)
                .map_err(ai_error_from_models_error)?;
            let (request_model, request_options) = apply_auth(
                &state,
                &model_for_setup,
                ModelsStreamOptions {
                    base: options.base.base,
                    transforms: options.transforms,
                    reasoning: None,
                    signal_reason: options.signal_reason.clone(),
                },
            )
            .await?;
            Ok::<_, AiError>(provider.stream_simple(
                &request_model,
                &transcript,
                SimpleStreamOptions {
                    tool_choice: options.base.tool_choice,
                    reasoning: options.base.reasoning,
                    deferred: options.base.deferred,
                    thinking_budgets: options.base.thinking_budgets,
                    base: request_options,
                },
            ))
        });
        lazy_stream(model, setup, Some(observer))
    }

    async fn complete_simple(
        &self,
        model: &Model,
        context: &Context,
        options: ModelsSimpleStreamOptions,
    ) -> AssistantMessage {
        let stream = self.stream_simple(model, context, options);
        stream_result(stream, model).await
    }

    fn stream_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: ModelsDeferredFetchOptions,
    ) -> AssistantMessageEventStream {
        let observer = observe_model_request(
            model,
            observe_options(options.base.base.telemetry.clone(), None),
            true,
        );
        let state = Arc::clone(&self.state);
        let model_for_setup = model.clone();
        let handle = handle.clone();
        // 能力检查放在 setup 内：`stream_deferred` 本身必须同步返回流，不能返回 `Err`；
        // 不支持时由 `lazy_stream` 把错误变成流上的 error 事件。
        let setup: LazySetup = Box::pin(async move {
            let provider = require_chat_provider(&state, &model_for_setup)
                .map_err(ai_error_from_models_error)?;
            if !provider.has_fetch_deferred() {
                return Err(ai_error_from_models_error(models_error(
                    TsModelsErrorCode::Provider,
                    &format!(
                        "Provider {} does not support deferred responses",
                        model_for_setup.provider
                    ),
                    None,
                )));
            }
            let (request_model, request_options) = apply_auth(
                &state,
                &model_for_setup,
                ModelsStreamOptions {
                    base: options.base.base,
                    transforms: options.transforms,
                    reasoning: None,
                    signal_reason: options.signal_reason.clone(),
                },
            )
            .await?;
            match provider.fetch_deferred(
                &request_model,
                &handle,
                DeferredFetchOptions {
                    base: request_options,
                    wait: options.base.wait,
                },
            ) {
                Some(stream) => Ok(stream),
                None => Err(ai_error_from_models_error(models_error(
                    TsModelsErrorCode::Provider,
                    &format!(
                        "Provider {} does not support deferred responses",
                        model_for_setup.provider
                    ),
                    None,
                ))),
            }
        });
        lazy_stream(model, setup, Some(observer))
    }

    async fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: ModelsDeferredFetchOptions,
    ) -> AssistantMessage {
        let stream = self.stream_deferred(model, handle, options);
        stream_result(stream, model).await
    }

    async fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: ModelsDeferredCancelOptions,
    ) -> Result<(), AiError> {
        // 与流式入口不同，这里是直接的 async 调用：能力缺失、鉴权失败都会作为 `Err` 返回。
        let provider =
            require_chat_provider(&self.state, model).map_err(ai_error_from_models_error)?;
        if !provider.has_cancel_deferred() {
            return Err(ai_error_from_models_error(models_error(
                TsModelsErrorCode::Provider,
                &format!(
                    "Provider {} does not support deferred responses",
                    model.provider
                ),
                None,
            )));
        }
        let (request_model, request_options) = apply_auth(
            &self.state,
            model,
            ModelsStreamOptions {
                base: options.base,
                transforms: options.transforms,
                reasoning: None,
                signal_reason: options.signal_reason.clone(),
            },
        )
        .await?;
        provider
            .cancel_deferred(&request_model, handle, request_options)
            .await
    }

    async fn generate_images(
        &self,
        model: &Model,
        context: &ImagesContext,
        options: ModelsImagesOptions,
    ) -> AssistantImages {
        // TS 在 catch 里读取 `options?.signal?.aborted`（错误发生时），因此中止发生在
        // applyAuth/适配器执行期间也要算数；这里保留 signal 到错误分支再判断。
        let signal = options.base.signal.clone();
        // 整个 async 块等价 TS 的 try：任何一步的 `?` 都跳到下面的 match 错误分支，
        // 最终不会把 `Err` 泄漏给调用方。
        let result: Result<AssistantImages, AiError> = async {
            // 顺序固定：先断言图片模型，再查 provider，再检查能力，最后才解析凭证
            // （能力缺失时会直接返回，不会走到鉴权/OAuth 刷新）。
            let any_model = typed_any_model(model);
            crate::utils::model_operations::assert_image_model(&any_model)
                .map_err(ai_error_from_models_error)?;
            let provider =
                require_provider(&self.state, model).map_err(ai_error_from_models_error)?;
            if !provider.has_generate_images() {
                return Err(ai_error_from_models_error(models_error(
                    TsModelsErrorCode::Provider,
                    &format!(
                        "Provider {} does not support image generation",
                        model.provider
                    ),
                    None,
                )));
            }
            let (request_model, request_options) = apply_auth(
                &self.state,
                model,
                ModelsStreamOptions {
                    base: options.base,
                    transforms: options.transforms,
                    reasoning: None,
                    signal_reason: options.signal_reason.clone(),
                },
            )
            .await?;
            Ok(provider
                .generate_images(&request_model, context, Some(&request_options))
                .await)
        }
        .await;
        // 错误折叠：`aborted` 取错误发生时刻的 signal 状态，决定 stop_reason 是 aborted
        // 还是 error；`to_thrown_value` 只保留 name/message 供 error_message 使用。
        match result {
            Ok(images) => images,
            Err(error) => {
                let aborted = signal
                    .as_ref()
                    .is_some_and(tokio_util::sync::CancellationToken::is_cancelled);
                image_error_result(model, &to_thrown_value(error), aborted)
            }
        }
    }

    async fn classify(
        &self,
        model: &Model,
        context: &ClassifierContext,
        options: ModelsClassifierOptions,
    ) -> ClassifierResult {
        // 同 generateImages：TS 在 catch 时读取 `options?.signal?.aborted`。
        let signal = options.base.signal.clone();
        // try 块的步骤顺序：分类模型断言 → 输入受支持校验 → provider 存在性 → 能力 →
        // 鉴权 → 调用；任一步失败都折叠进下面的 classifier_error_result。
        let result: Result<ClassifierResult, AiError> = async {
            let any_model = typed_any_model(model);
            crate::utils::model_operations::assert_classifier_model(&any_model)
                .map_err(ai_error_from_models_error)?;
            crate::utils::model_operations::assert_classifier_input_supported(model, context)
                .map_err(ai_error_from_models_error)?;
            let provider =
                require_provider(&self.state, model).map_err(ai_error_from_models_error)?;
            if !provider.has_classify() {
                return Err(ai_error_from_models_error(models_error(
                    TsModelsErrorCode::Provider,
                    &format!(
                        "Provider {} does not support classification",
                        model.provider
                    ),
                    None,
                )));
            }
            let (request_model, request_options) = apply_auth(
                &self.state,
                model,
                ModelsStreamOptions {
                    base: options.base,
                    transforms: options.transforms,
                    reasoning: None,
                    signal_reason: options.signal_reason.clone(),
                },
            )
            .await?;
            Ok(provider
                .classify(&request_model, context, Some(&request_options))
                .await)
        }
        .await;
        match result {
            Ok(result) => result,
            Err(error) => crate::utils::model_operations::classifier_error_result(
                model,
                &to_thrown_value(error),
                signal
                    .as_ref()
                    .is_some_and(tokio_util::sync::CancellationToken::is_cancelled),
            ),
        }
    }
}

impl MutableModels for ModelsImpl {
    fn set_provider(&self, provider: Arc<dyn Provider>) {
        // 先取代在途刷新，再插入：旧刷新之后的发布都会因代际过期被丢弃。顺序不能反——
        // 先插入会让旧刷新有机会在代际提升前完成发布。
        self.state.supersede_provider_refresh(provider.id());
        self.state
            .providers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(provider.id().to_owned(), provider);
    }

    fn delete_provider(&self, id: &str) {
        // 先中止在途刷新再删除；`shift_remove` 保持其余 provider 的插入顺序（TS Map 的 delete）。
        self.state.supersede_provider_refresh(id);
        self.state
            .providers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shift_remove(id);
    }

    fn clear_providers(&self) {
        // provider 表与 controller 表可能瞬时不一致（刷新进行中、provider 刚被删除等），
        // 取并集保证两类在途刷新都被中止；HashSet 去重后逐个 supersede，最后统一清空。
        let ids: Vec<String> = {
            let providers = self
                .state
                .providers
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let controllers = self
                .state
                .refresh_controllers
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            providers
                .keys()
                .cloned()
                .chain(controllers.keys().cloned())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect()
        };
        for id in ids {
            self.state.supersede_provider_refresh(&id);
        }
        self.state
            .providers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

// 单测锁定 `merge_headers` 的两条边界：大小写不敏感覆盖（保留 override 的原始大小写、
// `None` 值保留）与「Some(空 map) 不是 None」。请求路径的三级头合并依赖这两条语义。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_headers_is_case_insensitive_and_preserves_override_case() {
        let mut base = crate::types::ProviderHeaders::new();
        base.insert("Authorization".into(), Some("Bearer base".into()));
        base.insert("x-a".into(), Some("base".into()));
        let mut over = crate::types::ProviderHeaders::new();
        over.insert("authorization".into(), Some("Bearer over".into()));
        over.insert("x-b".into(), None);
        let merged = merge_headers(Some(base), Some(over)).unwrap();
        assert_eq!(
            merged.get("Authorization").cloned().flatten().as_deref(),
            None
        );
        assert_eq!(
            merged.get("authorization").cloned().flatten().as_deref(),
            Some("Bearer over")
        );
        assert_eq!(
            merged.get("x-a").cloned().flatten().as_deref(),
            Some("base")
        );
        assert!(merged.contains_key("x-b"));
        assert_eq!(merged.get("x-b").cloned().flatten(), None);
        assert_eq!(merge_headers(None, None), None);
    }

    #[test]
    fn empty_collections_are_still_some() {
        assert_eq!(
            merge_headers(Some(Default::default()), None),
            Some(Default::default())
        );
    }
}
