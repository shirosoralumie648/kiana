//! `ModelRuntime`（实现 `kiana_ai::models::Models`）。
//!
//! TS: `packages/coding-agent/src/core/model-runtime.ts`（1049 行）。规范：
//! `spec/ca-config-models.md` §2.20（含 §2.20.1-§2.20.15）。
//!
//! 组合层：内置 provider + models.json + 原生扩展 provider + 扩展注册 + 虚拟模型。读访问同步
//! 返回最后一份快照；网络访问只发生在 [`ModelRuntime::refresh`] 里。
//!
//! ## 并发模型（`ARCHITECTURE.md` §8.2）
//!
//! - 快照与 provider 映射用 `std::sync::Mutex` 保护；**任何锁都不跨 `await` 持有**。
//! - 三个序号（`availability_refresh_seq` / `availability_error_seq` /
//!   `provider_availability_seq`）用于过期结果判定：前两个是 [`AtomicU64`]，第三个是
//!   `Mutex<IndexMap<String, u64>>`（按 provider 保存局部序号）；每个入口的增减与比对顺序
//!   逐条对齐 §2.20.7（过期结果被丢弃；单 provider 刷新使全量刷新失效；`available` 的
//!   最终顺序由 `all` 的顺序决定）。
//! - 凭据操作按 provider 串行：每个 provider 一把 [`tokio::sync::Mutex`]。调用方在**轮到自己
//!   开始**时 resolve（`race_with_abort_signal` 等待 `started`）。可 `'static` 的任务
//!   （logout/setRuntimeApiKey/removeRuntimeApiKey 与 owned-interaction 的 login）在
//!   `tokio::spawn` 的任务里执行：调用方 future 被丢弃也不会取消任务（§2.20.10「任务仍在
//!   链上」）。借用 interaction 的 login 只能内联执行（Rust 借用规则；见
//!   [`IntoAuthInteraction`]）。
//!
//! TS 的 `ModelRuntime` 持有可变的 `Models` 集合；Rust 侧把全部状态放进 `Arc<Inner>`，这样
//! `lazy_stream` / `tokio::spawn` 里的 `'static` future 也能访问同一份状态。
//!
//! # 负责的边界
//!
//! 本文件是**编排层**，不自己实现目录解析、provider 合成、HTTP 与凭证持久化：
//!
//! - models.json 的读取与错误收集在 [`ModelConfig`]（`models/config_file.rs`）；本文件在
//!   `create`/`refresh` 时重新加载它，并把 `get_error` 汇总进 [`ModelRuntime::get_error`]。
//! - provider 的覆盖/合成在 `compose_model_provider`（`models/composer.rs`）；本文件决定
//!   哪些 id 需要合成、合成失败时回退到谁。
//! - 远程目录与个性化包装在 `models/remote_catalog.rs`；本文件只在 `create` 时给内置
//!   provider 套上包装。
//! - 凭证持久化与运行时覆盖在 `auth/runtime_credentials.rs` / `auth/storage.rs`；本文件
//!   负责“凭证提交后如何让本进程的目录与可用性快照跟上”（见 `synchronize_credential_state`）。
//! - 底层的 `Models` 集合由 `kiana_ai::models::create_models` 构建；查询类方法多数直接转发，
//!   只有虚拟模型、可用性快照与错误聚合是本文件额外维护的。
//! - 主要使用者：[`crate::models::registry::ModelRegistry`]（暴露给扩展的同步兼容门面，
//!   逐方法转发到本文件）与 `kiana-session` 的 `AgentSession`（请求前用
//!   [`ModelRuntime::resolve_model`] 处理虚拟模型）。
//!
//! # 状态与快照
//!
//! [`ModelRuntime`] 只是 `Arc<ModelRuntimeInner>` 的句柄（克隆廉价）；全部可变状态在
//! `ModelRuntimeInner` 里：
//!
//! - `default_builtins` / `builtins`：内置 provider 的出厂映射与当前映射。models.json 中
//!   声明 `oauth: "radius"` 的 provider 会让 `builtins` 里的对应条目被替换成 gateway 指向
//!   其 `baseUrl` 的 radius provider；`default_builtins` 保留原值以便每次重建。
//! - `native_extension_providers` / `extension_providers`：原生扩展 provider 实例与扩展
//!   注册的配置；合成时原生实例优先于内置 provider。
//! - `virtual_models`：provider id → model id → 目录条目 + 路由回调。
//! - `composition_errors`：最近一次合成失败的错误文案，由 `get_error` 暴露。
//! - `snapshot`：最近一次可用性刷新的结果（`all`/`available`、已配置与已存储 provider
//!   集合、auth 检查）。同步查询方法只读它，**不会**触发刷新。
//! - 三个序号与 `availability_error`：过期结果判定，见上面的并发模型。
//!
//! # 一次典型流程
//!
//! 1. [`ModelRuntime::create`] 构造凭证存储与 [`ModelConfig`]，包装内置 provider，构建底层
//!    `Models`，然后（默认）执行一次 [`ModelRuntime::refresh`] 建立快照。
//! 2. 请求路径（如 [`ModelRuntime::stream_simple`]）先经 `prepare_request_core` 解析 auth
//!    并合并请求头/env，再转交 provider。
//! 3. 凭据变更（login/logout/setRuntimeApiKey/removeRuntimeApiKey）按 provider 串行；提交后
//!    做一次局部同步：重组该 provider → 只刷新该 provider 的目录 → 单 provider 可用性刷新。
//! 4. 注册/注销 provider 与虚拟模型只改内存状态，并触发一次后台
//!    `refresh({ allowNetwork: false })`，不阻塞调用方。
//!
//! # 注意事项
//!
//! - `std::sync::Mutex` 的毒化由 `lock()` 辅助函数吞掉（取回内部值），避免一次 panic 让后续
//!   所有读取都失败；锁内不 `await`，因此 `MutexGuard` 不会跨挂起点。
//! - `compose_provider` 在持有 `config` 锁时再取 `composition_errors` 锁（顺序
//!   config → composition_errors）；新增的加锁路径应保持同一顺序。
//! - 快照里的 `available` 与 `all` 是两份独立的 `Vec`；局部可用性刷新最终会按 `all` 的顺序
//!   重排 `available`。
//! - `spawn_background_refresh` 丢弃 `refresh` 的返回值，后台失败不会传播给注册调用方；
//!   需要确定性时请显式调用 [`ModelRuntime::refresh`]。从注册到刷新落地之间存在窗口，
//!   期间同步查询看到的仍是旧快照。

// 冻结的 kiana-ai 错误类型（`AiError`/`AuthError`）与 `Credential` 让 `Result<_, _>` 的 Err
// 变体超过 clippy 的 128 字节阈值。签名由 `Models` trait 与 TS 的可观察错误语义固定，装箱
// 会改变公开签名（`Models` 的返回类型不可改），因此与 kiana-ai `models`/`auth` 的同类例外一致。
#![allow(clippy::result_large_err)]

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use futures::future::try_join3;
use indexmap::IndexMap;
use kiana_ai::api::lazy::{LazySetup, create_setup_error_message, lazy_stream};
use kiana_ai::auth::resolve::AuthResolutionOverrides;
use kiana_ai::auth::types::{
    ApiKeyCredential, AuthCheck, AuthError, AuthInteraction, AuthOperationOptions, AuthResult,
    AuthType, Credential, CredentialInfo, CredentialStore, LoginOptions, ModelAuth,
    throw_if_aborted,
};
use kiana_ai::error::{AiError, AiErrorKind};
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::models::{
    CreateModelsOptions, GetAuthTarget, Models, ModelsClassifierOptions,
    ModelsDeferredCancelOptions, ModelsDeferredFetchOptions, ModelsImagesOptions,
    ModelsRefreshOptions, ModelsRefreshResult, ModelsSimpleStreamOptions, ModelsStore,
    ModelsStreamOptions, MutableModels, Provider, ai_error_from_models_error, create_models,
    merge_headers,
};
use kiana_ai::providers::all::{
    builtin_providers, get_builtin_model_data_generated_at, radius_provider,
};
use kiana_ai::providers::openrouter::personalize_open_router_catalog;
use kiana_ai::types::{
    AnyModel, AssistantImages, AssistantMessage, ClassifierContext, ClassifierResult, Context,
    DeferredFetchOptions, DeferredHandle, ImagesContext, JsonValue, Message, Model,
    ModelThinkingLevel, ModelType, ProviderEnv, ProviderHeaders, SimpleStreamOptions,
    StreamOptions, ThinkingLevel, TranscriptContext,
};
use kiana_ai::utils::abort::{AbortReason, AbortScope, operation_signal, race_with_abort_signal};
use kiana_ai::utils::abort_signals::combine_abort_signals;
use kiana_ai::utils::diagnostics::ThrownValue;
use kiana_ai::utils::identity::{ClientIdentityTarget, get_open_code_session_headers};
use kiana_ai::utils::model_operations::{
    assert_chat_model, assert_classifier_input_supported, assert_classifier_model,
    assert_image_model, classifier_error_result, image_error_result,
};
use kiana_ai::utils::models_error::{TsModelsErrorCode, models_error};
use kiana_ai::utils::transcript::normalize_context;
use kiana_session_core::virtual_models::{
    ModelRoute, ModelRouteReason, ModelRouteRequest, RouteFailure, RouteFuture, RouteModelRef,
    VirtualModelDefinition, VirtualModelRouteError, create_virtual_model, find_latest_response,
    is_virtual_model, with_virtual_models,
};

use crate::auth::models_store::{FileModelsStore, InMemoryCodingAgentModelsStore};
use crate::auth::runtime_credentials::RuntimeCredentials;
use crate::auth::storage::AuthStorage;
use crate::models::composer::{
    AuthStatus, AuthStatusSource, CompatibilityRequestConfig, ProviderConfigInput,
    compose_model_provider, configured_request_auth_status, resolve_compatibility_request_config,
    resolve_configured_model_headers, validate_extension_provider,
};
use crate::models::config_file::ModelConfig;
use crate::models::remote_catalog::{
    PersonalizeCatalog, with_personalized_catalog, with_remote_catalog,
};

/// `ModelRuntimeAuthOverrides extends AuthOperationOptions`。
///
/// TS: `model-runtime.ts#ModelRuntimeAuthOverrides` —— 字段与 kiana-ai 的
/// [`AuthResolutionOverrides`] 完全一致（`apiKey`/`env`/`minOAuthValidityMs`/`signal`）。
pub type ModelRuntimeAuthOverrides = AuthResolutionOverrides;

/// 传给 [`ModelRuntime::login`] 的 interaction 来源。
///
/// TS 的登录 operation 与调用方 promise 解耦（调用方被放弃后任务仍在链上跑完）。Rust 中
/// 只有**拥有**的 interaction 才能跨 `tokio::spawn` 分离；`&dyn AuthInteraction` 借用调用方
/// 栈上的对象，只能退回调用方内联执行。两种形式都保留：借用形式与旧调用点源码兼容，拥有
/// 形式（`Arc<dyn AuthInteraction>` / `Arc<T>`）获得 TS 的分离语义。
pub trait IntoAuthInteraction {
    /// 借用视图。
    fn as_auth_interaction(&self) -> &dyn AuthInteraction;

    /// 可跨任务持有的句柄（借用形式返回 `None`）。
    fn owned_auth_interaction(&self) -> Option<Arc<dyn AuthInteraction>>;
}

impl<T: AuthInteraction + 'static> IntoAuthInteraction for &T {
    fn as_auth_interaction(&self) -> &dyn AuthInteraction {
        &**self
    }

    fn owned_auth_interaction(&self) -> Option<Arc<dyn AuthInteraction>> {
        None
    }
}

impl IntoAuthInteraction for &dyn AuthInteraction {
    fn as_auth_interaction(&self) -> &dyn AuthInteraction {
        *self
    }

    fn owned_auth_interaction(&self) -> Option<Arc<dyn AuthInteraction>> {
        None
    }
}

impl<T: AuthInteraction + 'static> IntoAuthInteraction for Arc<T> {
    fn as_auth_interaction(&self) -> &dyn AuthInteraction {
        &**self
    }

    fn owned_auth_interaction(&self) -> Option<Arc<dyn AuthInteraction>> {
        Some(Arc::clone(self) as Arc<dyn AuthInteraction>)
    }
}

impl IntoAuthInteraction for Arc<dyn AuthInteraction> {
    fn as_auth_interaction(&self) -> &dyn AuthInteraction {
        &**self
    }

    fn owned_auth_interaction(&self) -> Option<Arc<dyn AuthInteraction>> {
        Some(Arc::clone(self))
    }
}

// ---------------------------------------------------------------------------
// 选项与公开类型
// ---------------------------------------------------------------------------

/// `modelsPath?: string | null` 的三态。
///
/// TS 的 `undefined`（缺省 → `getAgentDir()/models.json`）、`null`（禁用 models.json）与字符串
/// 在 Rust 里用三叉枚举表达，避免 `Option<Option<String>>` 的歧义。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ModelsPathOption {
    /// 未提供：`join(getAgentDir(), "models.json")`。
    #[default]
    Default,
    /// 显式 `null`：不读任何 models.json（`undefined` 路径）。
    Disabled,
    /// 显式路径。
    Path(String),
}

/// `CreateModelRuntimeOptions`。
///
/// TS: `model-runtime.ts#CreateModelRuntimeOptions`
#[derive(Default)]
pub struct CreateModelRuntimeOptions {
    /// `credentials`：凭证存储；缺省是 `authPath` 处的文件。
    ///
    /// 提供该字段时 `auth_path` 被忽略。
    pub credentials: Option<Arc<dyn CredentialStore>>,
    /// `authPath`：仅在 `credentials` 未提供时生效；再缺省则用
    /// `AuthStorage::create_default()`。
    pub auth_path: Option<String>,
    /// `modelsPath`（三态）；`Default` 用 `getAgentDir()/models.json`，`Disabled` 与显式
    /// 空串路径都等价于“没有 models.json”。
    pub models_path: ModelsPathOption,
    /// `modelsStore`：动态刷新的模型目录缓存（`ModelsStore`）；缺省按 `models_path` 是否为
    /// 非空路径自动选择 `FileModelsStore` 或内存实现。
    pub models_store: Option<Arc<dyn ModelsStore>>,
    /// `modelsStorePath`：只在自动创建 `FileModelsStore` 时生效；缺省是 models.json
    /// 同目录下的 `models-store.json`。
    pub models_store_path: Option<String>,
    /// 允许 `create()` 联网刷新模型目录；默认 `false`。
    ///
    /// 实际是否联网还要看环境变量 `KIANA_OFFLINE`：它存在时无条件禁用网络，即使这里为
    /// `true`。
    pub allow_model_network: bool,
    /// create 时网络刷新的超时（毫秒）。
    ///
    /// 只在真正会联网（`allow_model_network` 为 `true` 且 `KIANA_OFFLINE` 未设置）且当前
    /// 有 tokio 运行时时启动计时任务；到点会中止内部 AbortScope，而不是取消外层 future。
    pub model_refresh_timeout_ms: Option<u64>,
    /// `catalogBaseUrl`：远程目录基地址；缺省读 `KIANA_API_BASE_URL`。
    pub catalog_base_url: Option<String>,
    /// 调用方的取消信号（初始缓存恢复与可用性检查）。
    ///
    /// 与内部超时控制器合并（`combine_abort_signals`）后传给 create 的首次刷新。
    pub signal: Option<AbortScope>,
    /// 跳过初始目录与可用性刷新（`None` = 默认 `true`）。
    ///
    /// 设为 `false` 时 `create` 不执行 `refresh`：`available`/`configured_providers` 保持
    /// 空，但 `all` 仍由 `new()` 与 `create` 的 `rebuild_providers` → `update_model_snapshot`
    /// 用底层目录（`models.get_models(None)`）填充；需要可用性快照时调用
    /// [`ModelRuntime::refresh`] 或 [`ModelRuntime::get_available`]。
    pub refresh_on_create: Option<bool>,
}

impl CreateModelRuntimeOptions {
    /// 只含凭证与 `modelsPath` 的最小选项（测试与内部使用的便捷构造）。
    ///
    /// `models_path` 被设为 [`ModelsPathOption::Disabled`]，因此不会读 models.json，
    /// `models_store` 也会落到内存实现。
    pub fn with_credentials(credentials: Arc<dyn CredentialStore>) -> Self {
        Self {
            credentials: Some(credentials),
            models_path: ModelsPathOption::Disabled,
            ..Default::default()
        }
    }
}

/// `CredentialSynchronizationOperation`。
///
/// TS: `model-runtime.ts#CredentialSynchronizationOperation`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialSynchronizationOperation {
    /// `"login"`。
    Login,
    /// `"logout"`。
    Logout,
    /// `"setRuntimeApiKey"`。
    SetRuntimeApiKey,
    /// `"removeRuntimeApiKey"`。
    RemoveRuntimeApiKey,
}

impl CredentialSynchronizationOperation {
    /// TS 的字面量（也用于错误消息里的 `${operation}`）。
    pub fn as_str(self) -> &'static str {
        match self {
            CredentialSynchronizationOperation::Login => "login",
            CredentialSynchronizationOperation::Logout => "logout",
            CredentialSynchronizationOperation::SetRuntimeApiKey => "setRuntimeApiKey",
            CredentialSynchronizationOperation::RemoveRuntimeApiKey => "removeRuntimeApiKey",
        }
    }
}

impl std::fmt::Display for CredentialSynchronizationOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 凭证已提交，但本地模型/auth 快照无法同步。
///
/// TS: `model-runtime.ts#CredentialSynchronizationError`。`Display` 逐字为
/// `Credential ${operation} committed for ${providerId}, but local synchronization failed`，
/// `name` 保持 `"CredentialSynchronizationError"`（见 [`CredentialSynchronizationError::name`]）。
#[derive(Debug, Clone, PartialEq)]
pub struct CredentialSynchronizationError {
    /// `providerId`。
    pub provider_id: String,
    /// 已提交的操作。
    pub operation: CredentialSynchronizationOperation,
    /// 已提交的凭证（`logout`/`removeRuntimeApiKey` 为 `None`）。
    pub credential: Option<Credential>,
    /// TS 的 `Error.cause`。
    pub cause: Option<AuthError>,
}

impl CredentialSynchronizationError {
    /// `error.name`。
    pub fn name(&self) -> &'static str {
        "CredentialSynchronizationError"
    }

    /// `error.message`（`Display` 的输出）。
    pub fn message(&self) -> String {
        format!(
            "Credential {} committed for {}, but local synchronization failed",
            self.operation, self.provider_id
        )
    }
}

impl std::fmt::Display for CredentialSynchronizationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl std::error::Error for CredentialSynchronizationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
            .as_ref()
            .map(|cause| cause as &(dyn std::error::Error + 'static))
    }
}

/// `ModelRuntime` 公开方法抛出的错误。
///
/// TS 里这些是三种 `Error`：普通 `Error`、`ModelsError`（保存为 [`AuthError`] 的
/// `name`/`code`）与 [`CredentialSynchronizationError`]。Rust 用一个枚举保留 `name` 与
/// `cause` 语义（`ModelRegistry.getApiKeyAndHeaders` 依赖 `cause.message`）。
#[derive(Debug, Clone)]
pub enum ModelRuntimeError {
    /// auth 子系统错误（`Error` / `ModelsError` / `AbortError`）。
    Auth(AuthError),
    /// 已提交凭证但本地同步失败。
    CredentialSync(CredentialSynchronizationError),
    /// 普通 `Error`（注册校验、虚拟模型路由等）。
    Message(String),
}

impl ModelRuntimeError {
    /// `error.name`。
    pub fn name(&self) -> &str {
        match self {
            ModelRuntimeError::Auth(error) => &error.name,
            ModelRuntimeError::CredentialSync(error) => error.name(),
            ModelRuntimeError::Message(_) => "Error",
        }
    }

    /// `error.message`。
    pub fn message(&self) -> String {
        match self {
            ModelRuntimeError::Auth(error) => error.message.clone(),
            ModelRuntimeError::CredentialSync(error) => error.message(),
            ModelRuntimeError::Message(message) => message.clone(),
        }
    }

    /// `error.cause`（TS 的 `error instanceof Error ? error.cause : undefined`）。
    pub fn cause(&self) -> Option<&AuthError> {
        match self {
            ModelRuntimeError::Auth(error) => error.cause(),
            ModelRuntimeError::CredentialSync(error) => error.cause.as_ref(),
            ModelRuntimeError::Message(_) => None,
        }
    }

    /// 转成 [`AuthError`]（`name`/`message`/`cause` 保留）。
    ///
    /// TS 的 `CredentialSynchronizationError` 通过 `super(message, { cause })` 保留
    /// `Error.cause`；经由冻结的 `Models` trait 传播时也必须保留，否则消费方的
    /// `error.cause` 链会丢失底层失败。
    pub fn to_auth_error(&self) -> AuthError {
        match self {
            ModelRuntimeError::Auth(error) => error.clone(),
            ModelRuntimeError::CredentialSync(error) => {
                let auth = AuthError::with_name(error.name(), error.message());
                match &error.cause {
                    Some(cause) => auth.with_cause(cause.clone()),
                    None => auth,
                }
            }
            ModelRuntimeError::Message(message) => AuthError::new(message.clone()),
        }
    }
}

impl std::fmt::Display for ModelRuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelRuntimeError::Auth(error) => formatter.write_str(&error.message),
            ModelRuntimeError::CredentialSync(error) => error.fmt(formatter),
            ModelRuntimeError::Message(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ModelRuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ModelRuntimeError::Auth(error) => Some(error),
            ModelRuntimeError::CredentialSync(error) => Some(error),
            ModelRuntimeError::Message(_) => None,
        }
    }
}

impl From<AuthError> for ModelRuntimeError {
    fn from(error: AuthError) -> Self {
        ModelRuntimeError::Auth(error)
    }
}

impl From<CredentialSynchronizationError> for ModelRuntimeError {
    fn from(error: CredentialSynchronizationError) -> Self {
        ModelRuntimeError::CredentialSync(error)
    }
}

impl From<AiError> for ModelRuntimeError {
    fn from(error: AiError) -> Self {
        ModelRuntimeError::Auth(AuthError::from_ai_error(&error))
    }
}

/// `prepareRequest` 的结果。
///
/// TS 的 `{ provider, model, options }`。这里的 `provider` 取自底层 `Models`（可能已被
/// `with_virtual_models` 包装）；`options` 里的 `api_key`/`headers`/`env` 已完成合并，
/// 可直接传给 provider。
pub struct PreparedRequest {
    /// 组合后的 provider（含虚拟模型 wrapper）。
    pub provider: Arc<dyn Provider>,
    /// 应用 `baseUrl` 覆盖后的请求模型。
    pub model: Model,
    /// 已合并 auth / session 头 / env / apiKey 的请求选项。
    pub options: StreamOptions,
}

/// `resolveModel(model, messages, options)` 的选项。
///
/// TS: `model-runtime.ts#resolveModel` 的第三个参数。`reason`/`thinking_level`/`signal`/
/// `state` 会原样进入 `ModelRouteRequest`（`state` 是路由器可跨请求保存的 JSON，
/// `None` = 没有状态）；`failed` 不是原样——只有能解析为物理模型时才被投影成
/// `RouteFailure` 传入，通常只在 `retry` 路径上有值。
#[derive(Clone)]
pub struct ResolveModelOptions {
    /// 路由原因。
    pub reason: ModelRouteReason,
    /// 调用方选择的 thinking level。
    pub thinking_level: ModelThinkingLevel,
    /// 取消信号。
    pub signal: Option<AbortScope>,
    /// `retry` 的失败消息。
    pub failed: Option<AssistantMessage>,
    /// 会话分支上保存的路由器状态。
    pub state: Option<JsonValue>,
}

// ---------------------------------------------------------------------------
// 私有状态
// ---------------------------------------------------------------------------

/// 已注册的虚拟模型。
struct RegisteredVirtualModel {
    /// 对外展示/匹配用的目录条目（由 `create_virtual_model` 生成）。
    model: Model,
    /// 路由回调；`None` 时 `resolve_model` 报 `has no route.`。
    route: Option<Arc<dyn Fn(ModelRouteRequest) -> RouteFuture + Send + Sync>>,
}

/// `ModelRuntimeSnapshot`。
#[derive(Clone, Default)]
struct ModelRuntimeSnapshot {
    /// 底层目录的全量模型（含虚拟条目）。
    all: Vec<Model>,
    /// 可用模型：全量刷新时取底层 `get_available(None)`，局部刷新时由旧快照与本次结果合并
    /// 后再按 `all` 的顺序重排。
    available: Vec<Model>,
    /// 被视为已配置的 provider（可用性刷新与注册/凭据同步路径都会维护）。
    configured_providers: HashSet<String>,
    /// 凭证列表中出现过的 provider（持久存储与运行时覆盖合并，见 `RuntimeCredentials`）。
    stored_providers: HashSet<String>,
    /// provider → auth 检查结果（只在有结果时插入）。
    auth: IndexMap<String, AuthCheck>,
}

/// `ModelRuntime` 的全部可变状态（`ModelRuntime` 只持有一个 `Arc`）。
struct ModelRuntimeInner {
    /// 底层 `Models`：目录、provider 注册与 auth 解析都委托给它。
    models: Arc<dyn MutableModels>,
    /// 运行时 API key 覆盖层 + 持久凭证存储。
    credentials: Arc<RuntimeCredentials>,
    /// 出厂内置 provider；每次 radius 重配置都从这里重建 `builtins`。
    default_builtins: IndexMap<String, Arc<dyn Provider>>,
    /// 当前内置 provider（可能被 radius 配置替换过）。
    builtins: Mutex<IndexMap<String, Arc<dyn Provider>>>,
    /// 原生扩展 provider（由扩展直接给出 `Provider` 实例）。
    native_extension_providers: Mutex<IndexMap<String, Arc<dyn Provider>>>,
    /// 扩展注册的 provider 配置（`registerProvider` 的输入）。
    extension_providers: Mutex<IndexMap<String, ProviderConfigInput>>,
    /// 虚拟模型：provider id → model id → 条目。
    virtual_models: Mutex<IndexMap<String, IndexMap<String, RegisteredVirtualModel>>>,
    /// provider id → 最近一次合成失败的文案；下次合成成功或不再需要合成时清除。
    composition_errors: Mutex<IndexMap<String, String>>,
    /// models.json 路径（`None` 表示不读）。
    models_path: Option<String>,
    /// 是否允许联网刷新目录（来自 `KIANA_OFFLINE` 的反面）。
    model_network_enabled: bool,
    /// 最近加载的 models.json；`refresh` 会整体替换。
    config: Mutex<ModelConfig>,
    /// 可用性快照；同步查询只读它。
    snapshot: Mutex<ModelRuntimeSnapshot>,
    /// 全量可用性刷新序号（每次入队 +1）。
    availability_refresh_seq: AtomicU64,
    /// 可用性错误序号：只有最新一轮可以清除/写入 `availability_error`。
    availability_error_seq: AtomicU64,
    /// 每个 provider 的局部刷新序号（`refresh_provider_availability` 使用）。
    provider_availability_seq: Mutex<IndexMap<String, u64>>,
    /// 最近一次可用性错误文案（`get_error` 加前缀后展示）。
    availability_error: Mutex<Option<String>>,
    /// 每个 provider 一把凭据操作锁；条目只增不删。
    credential_locks: Mutex<IndexMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl std::fmt::Debug for ModelRuntimeInner {
    // 只输出 models_path 与 model_network_enabled；其余状态不进入调试输出。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModelRuntime")
            .field("models_path", &self.models_path)
            .field("model_network_enabled", &self.model_network_enabled)
            .finish_non_exhaustive()
    }
}

/// 取锁并忽略毒化。
///
/// 毒化只可能来自持有锁的线程 panic；这里取回内部值继续运行，避免一次 panic 让之后所有
/// 查询都失败。
fn lock<T>(slot: &Mutex<T>) -> MutexGuard<'_, T> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `error instanceof Error ? error.message : String(error)`（AuthError 的 `Display` 即 message）。
fn auth_error_message(error: &AuthError) -> String {
    error.message.clone()
}

/// `AuthError` → 请求路径的 [`AiError`]，并按调用方的 `signal.reason` 归一化中止文案。
///
/// 仅对 `is_aborted()` 的错误生效：有自定义 reason 时整体替换 name/message；否则若 message
/// 仍是 `AbortReason::fallback_abort_error()` 的通用文案（“The operation was aborted”），
/// 换成 `default_abort_error()` 的文案（“This operation was aborted”）。非中止错误原样转换。
fn normalize_request_abort_error(error: AuthError, custom_reason: Option<&AbortReason>) -> AiError {
    let error = if error.is_aborted() {
        if let Some(reason) = custom_reason {
            AuthError::with_name(reason.name.clone(), reason.message.clone())
        } else if error.message == AbortReason::fallback_abort_error().message {
            AuthError::with_name(
                error.name.clone(),
                AbortReason::default_abort_error().message,
            )
        } else {
            error
        }
    } else {
        error
    };
    error.to_ai_error()
}

/// `ModelThinkingLevel` → `ThinkingLevel`（`"off"` 映射为 `None`）。
///
/// kiana-ai 的 [`SimpleStreamOptions`] 用无 `"off"` 的 [`ThinkingLevel`] 表达请求等级。
fn thinking_level_of(level: ModelThinkingLevel) -> Option<ThinkingLevel> {
    match level {
        ModelThinkingLevel::Off => None,
        ModelThinkingLevel::Minimal => Some(ThinkingLevel::Minimal),
        ModelThinkingLevel::Low => Some(ThinkingLevel::Low),
        ModelThinkingLevel::Medium => Some(ThinkingLevel::Medium),
        ModelThinkingLevel::High => Some(ThinkingLevel::High),
        ModelThinkingLevel::Xhigh => Some(ThinkingLevel::Xhigh),
        ModelThinkingLevel::Max => Some(ThinkingLevel::Max),
    }
}

/// `ThinkingLevel` → `ModelThinkingLevel`。
fn model_thinking_level(level: ThinkingLevel) -> ModelThinkingLevel {
    match level {
        ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
        ThinkingLevel::Low => ModelThinkingLevel::Low,
        ThinkingLevel::Medium => ModelThinkingLevel::Medium,
        ThinkingLevel::High => ModelThinkingLevel::High,
        ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
        ThinkingLevel::Max => ModelThinkingLevel::Max,
    }
}

/// TS 的 `Model<any>` → [`AnyModel`]（按内层 `type` 选择变体）。
///
/// `Model::model_type` 为 `None` 时按 chat 处理。
fn typed_any_model(model: &Model) -> AnyModel {
    match model.model_type.unwrap_or(ModelType::Chat) {
        ModelType::Chat => AnyModel::Chat(model.clone()),
        ModelType::Image => AnyModel::Image(model.clone()),
        ModelType::Classifier => AnyModel::Classifier(model.clone()),
    }
}

/// [`AiError`] → `imageErrorResult`/`classifierErrorResult` 的抛出值。
///
/// 只搬运 name/message；`stack`/`code` 固定为 `None`（`AiError` 本身不携带 stack/code；
/// 另见 spec/ai-api-other.md §A.3.1：V8 的 `Error.stack` 不可逐字节复现）。
fn to_thrown_value(error: &AiError) -> ThrownValue {
    ThrownValue::Error(kiana_ai::types::DiagnosticErrorInfo {
        name: Some(error.name.clone()),
        message: error.message.clone(),
        stack: None,
        code: None,
    })
}

/// `Date.now()`。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

/// `modelsError(code, message)` → [`AiError`]。
fn models_error_ai(code: TsModelsErrorCode, message: &str) -> AiError {
    ai_error_from_models_error(models_error(code, message, None))
}

/// `join(dirname(path), name)`（`node:path` 的 `dirname`：`"a"` → `"."`，根 → 根）。
fn dirname_join(path: &str, name: &str) -> String {
    let path_ref = std::path::Path::new(path);
    let parent = match path_ref.parent() {
        Some(parent) => parent.to_path_buf(),
        None => path_ref.to_path_buf(),
    };
    parent.join(name).to_string_lossy().into_owned()
}

/// `baseUrl.replace(/\/v1\/?$/u, "")`。
fn strip_trailing_v1(url: &str) -> String {
    if let Some(value) = url.strip_suffix("/v1/") {
        return value.to_string();
    }
    if let Some(value) = url.strip_suffix("/v1") {
        return value.to_string();
    }
    url.to_string()
}

/// 按插入序去重。
fn unique_ids(providers: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for provider in providers {
        if seen.insert(provider.clone()) {
            unique.push(provider.clone());
        }
    }
    unique
}

/// 只校验 `id().trim()` 非空用的 provider id。
fn provider_id_trimmed(provider: &Arc<dyn Provider>) -> bool {
    !provider.id().trim().is_empty()
}

// ---------------------------------------------------------------------------
// ModelRuntime
// ---------------------------------------------------------------------------

/// 配置好的 kiana-ai `Models` 集合（coding-agent 与 SDK 使用）。
///
/// TS: `model-runtime.ts#ModelRuntime`。读访问同步返回最后快照；网络只发生在 `refresh()`。
///
/// 克隆只复制内部 `Arc`，所有克隆共享同一份状态与序号；[`ModelRuntime::identity`] 正是用
/// 内部地址充当进程内的“对象同一性”。方法按用途分为：构造与刷新 → 查询 → auth → 凭据
/// 操作 → 请求准备 → 流式接口 → 注册/注销 → 虚拟模型路由。同步查询只读内存状态；需要
/// 重读 models.json、刷新目录、解析 auth 或发起请求的方法是 `async`。
#[derive(Clone)]
pub struct ModelRuntime {
    inner: Arc<ModelRuntimeInner>,
}

impl std::fmt::Debug for ModelRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&*self.inner, formatter)
    }
}

impl ModelRuntime {
    /// `static async create(options)`。
    ///
    /// 逐条对齐 §2.20.4（包括构造后**两次** `rebuildProviders`、`KIANA_OFFLINE` 的存在性判断、
    /// create 时网络刷新的超时与组合取消信号）。
    ///
    /// 输入 `options` 决定凭证来源、models.json 路径、目录缓存与是否允许联网；输出是一个
    /// 已建立初始快照（除非 `refresh_on_create == Some(false)`）的运行时句柄。
    ///
    /// 失败模式：
    /// - 首次刷新失败时返回 `Err`，已构造的运行时被丢弃；
    /// - models.json 读取/解析失败**不会**让 `create` 失败：错误保存在 [`ModelConfig`] 里，
    ///   之后由 [`ModelRuntime::get_error`] 暴露；
    /// - 联网刷新是否真的发生同时受 `allow_model_network` 与 `KIANA_OFFLINE` 约束。
    pub async fn create(options: CreateModelRuntimeOptions) -> Result<Self, AiError> {
        // 凭证来源优先级：显式注入的 credentials > auth_path 指向的文件 > 默认 auth.json。
        let store: Arc<dyn CredentialStore> = match &options.credentials {
            Some(credentials) => Arc::clone(credentials),
            None => Arc::new(match &options.auth_path {
                Some(path) => AuthStorage::create(path),
                None => AuthStorage::create_default(),
            }),
        };
        let credentials = Arc::new(RuntimeCredentials::new(store));
        // modelsPath 三态 → 具体路径或 None；None/空串会让 ModelConfig 得到空配置。
        let models_path = match &options.models_path {
            ModelsPathOption::Disabled => None,
            ModelsPathOption::Path(path) => Some(path.clone()),
            ModelsPathOption::Default => Some(
                kiana_core::config::get_models_path()
                    .to_string_lossy()
                    .into_owned(),
            ),
        };
        let config = ModelConfig::load(models_path.as_deref()).await;
        let models_store: Arc<dyn ModelsStore> = match &options.models_store {
            Some(store) => Arc::clone(store),
            // TS 的 `modelsPath ? new FileModelsStore(...) : new InMemoryCodingAgentModelsStore()`：
            // 空串是 falsy，选择内存 store（配置加载同样把空串当作未给出）。
            None => match models_path.as_deref().filter(|path| !path.is_empty()) {
                Some(path) => {
                    let store_path = options
                        .models_store_path
                        .clone()
                        .unwrap_or_else(|| dirname_join(path, "models-store.json"));
                    Arc::new(FileModelsStore::new(&store_path))
                }
                None => Arc::new(InMemoryCodingAgentModelsStore::new()),
            },
        };

        let generated_at = get_builtin_model_data_generated_at();
        let catalog_base_url = options
            .catalog_base_url
            .clone()
            .or_else(kiana_core::config::get_api_base_url);
        let personalize: PersonalizeCatalog = Arc::new(|catalog, context| {
            Box::pin(personalize_open_router_catalog(catalog, context))
        });
        // 内置 provider 的目录包装：radius 保持原样；openrouter 额外接个性化回调
        // （`personalize_open_router_catalog`）；其余接远程目录。base URL 与内置目录的
        // 生成时间作为包装上下文传入。
        let providers: Vec<Arc<dyn Provider>> = builtin_providers()
            .into_iter()
            .map(|provider| {
                let id = provider.id().to_string();
                if id == "radius" {
                    return provider;
                }
                if id == "openrouter" {
                    return with_personalized_catalog(
                        provider,
                        Arc::clone(&personalize),
                        catalog_base_url.as_deref(),
                        generated_at,
                    );
                }
                with_remote_catalog(provider, catalog_base_url.as_deref(), generated_at)
            })
            .collect();

        let runtime = Self::new(
            credentials,
            config,
            models_path,
            models_store,
            providers,
            std::env::var_os("KIANA_OFFLINE").is_none(),
        );
        runtime.configure_radius_providers();
        runtime.rebuild_providers();

        let refresh_from_network =
            runtime.inner.model_network_enabled && options.allow_model_network;
        // 超时任务只在“确实要联网 + 给了超时 + 当前有 tokio 运行时”时创建；到点后中止的是
        // 内部 AbortScope：`refresh` 会把该次刷新标记为 `aborted`，但 `create` 用
        // `.map(|_| ())` 丢弃 `ModelsRefreshResult`，因此超时不会让 `create` 返回 `Err`。
        let controller = (refresh_from_network && options.model_refresh_timeout_ms.is_some())
            .then(AbortScope::new);
        let mut timeout_task = None;
        if let (Some(controller), Some(timeout_ms)) =
            (controller.as_ref(), options.model_refresh_timeout_ms)
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            let controller = controller.clone();
            timeout_task = Some(handle.spawn(async move {
                tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
                controller.abort();
            }));
        }
        let signal = match &controller {
            Some(controller) => {
                combine_abort_signals(&[options.signal.clone(), Some(controller.clone())]).signal
            }
            None => options.signal.clone(),
        };
        // TS 的 `try { … } finally { clearTimeout(timeout) }`：刷新失败时也必须取消超时任务，
        // 不能让 sleep 一直存活到 `modelRefreshTimeoutMs`。
        let refresh_result = if options.refresh_on_create.unwrap_or(true) {
            runtime
                .refresh(ModelsRefreshOptions {
                    allow_network: Some(refresh_from_network),
                    signal,
                    ..Default::default()
                })
                .await
                .map(|_| ())
        } else {
            Ok(())
        };
        if let Some(task) = timeout_task {
            task.abort();
        }
        refresh_result?;
        Ok(runtime)
    }

    /// 构造（TS 的 `private constructor`）。
    ///
    /// 建立全部字段并先做一次 `rebuild_providers`；[`ModelRuntime::create`] 随后还会
    /// 先 `configure_radius_providers` 再重建一次，因此初建阶段共有两次重建。
    fn new(
        credentials: Arc<RuntimeCredentials>,
        config: ModelConfig,
        models_path: Option<String>,
        models_store: Arc<dyn ModelsStore>,
        providers: Vec<Arc<dyn Provider>>,
        model_network_enabled: bool,
    ) -> Self {
        let default_builtins: IndexMap<String, Arc<dyn Provider>> = providers
            .into_iter()
            .map(|provider| (provider.id().to_string(), provider))
            .collect();
        let builtins = default_builtins.clone();
        let models = create_models(CreateModelsOptions {
            credentials: Some(Arc::clone(&credentials) as Arc<dyn CredentialStore>),
            models_store: Some(models_store),
            auth_context: None,
            identity: None,
        });
        let runtime = ModelRuntime {
            inner: Arc::new(ModelRuntimeInner {
                models,
                credentials,
                default_builtins,
                builtins: Mutex::new(builtins),
                native_extension_providers: Mutex::new(IndexMap::new()),
                extension_providers: Mutex::new(IndexMap::new()),
                virtual_models: Mutex::new(IndexMap::new()),
                composition_errors: Mutex::new(IndexMap::new()),
                models_path,
                model_network_enabled,
                config: Mutex::new(config),
                snapshot: Mutex::new(ModelRuntimeSnapshot::default()),
                availability_refresh_seq: AtomicU64::new(0),
                availability_error_seq: AtomicU64::new(0),
                provider_availability_seq: Mutex::new(IndexMap::new()),
                availability_error: Mutex::new(None),
                credential_locks: Mutex::new(IndexMap::new()),
            }),
        };
        runtime.rebuild_providers();
        runtime
    }

    /// `configureRadiusProviders()`。
    ///
    /// models.json 里声明 `oauth: "radius"` 且给了 `base_url` 的 provider，会用
    /// `radius_provider` 替换 `builtins` 中的条目：id 保持配置里的键，name 取配置的
    /// `name`（缺省同 id），gateway 是去掉尾部 `/v1` 的 `base_url`。缺 `base_url` 的条目
    /// 保持内置实现。
    ///
    /// 每次进入都先清空 `builtins` 再由 `default_builtins` 重建，因此 `refresh` 重读配置后
    /// 不会残留上一轮替换过的 provider。
    fn configure_radius_providers(&self) {
        let mut builtins = lock(&self.inner.builtins);
        builtins.clear();
        for (provider_id, provider) in &self.inner.default_builtins {
            builtins.insert(provider_id.clone(), Arc::clone(provider));
        }
        let config = lock(&self.inner.config);
        for provider_id in config.get_provider_ids() {
            let Some(provider_config) = config.get_provider(&provider_id) else {
                continue;
            };
            if provider_config.oauth != Some(crate::models::config_file::ModelsJsonOauth::Radius) {
                continue;
            }
            let Some(base_url) = &provider_config.base_url else {
                continue;
            };
            let gateway = strip_trailing_v1(base_url);
            builtins.insert(
                provider_id.clone(),
                radius_provider(kiana_ai::providers::radius::RadiusProviderOptions {
                    id: Some(provider_id.clone()),
                    name: Some(
                        provider_config
                            .name
                            .clone()
                            .unwrap_or_else(|| provider_id.clone()),
                    ),
                    gateway: Some(gateway),
                }),
            );
        }
    }

    /// `providerIds()`：五个来源按插入序的并集。
    ///
    /// 顺序固定为 `builtins` → 原生扩展 → models.json → 扩展注册 → 虚拟模型；重复 id 保留
    /// 第一次出现的位置。`rebuild_providers` 按这个顺序逐个重组。
    fn provider_ids(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut ids = Vec::new();
        let push = |id: String, seen: &mut HashSet<String>, ids: &mut Vec<String>| {
            if seen.insert(id.clone()) {
                ids.push(id);
            }
        };
        for id in lock(&self.inner.builtins)
            .keys()
            .cloned()
            .collect::<Vec<_>>()
        {
            push(id, &mut seen, &mut ids);
        }
        for id in lock(&self.inner.native_extension_providers)
            .keys()
            .cloned()
            .collect::<Vec<_>>()
        {
            push(id, &mut seen, &mut ids);
        }
        for id in lock(&self.inner.config).get_provider_ids() {
            push(id, &mut seen, &mut ids);
        }
        for id in lock(&self.inner.extension_providers)
            .keys()
            .cloned()
            .collect::<Vec<_>>()
        {
            push(id, &mut seen, &mut ids);
        }
        for id in lock(&self.inner.virtual_models)
            .keys()
            .cloned()
            .collect::<Vec<_>>()
        {
            push(id, &mut seen, &mut ids);
        }
        ids
    }

    /// `composeProvider(providerId)`：无覆盖层时原样返回 base。
    ///
    /// base 的选择次序是“原生扩展 provider → 内置 provider”；都没有时为 `None`，纯虚拟
    /// provider 会由 `recompose_provider` 另行生成。只有 models.json 或扩展注册提供了该
    /// id 时才真正调用 `compose_model_provider`。
    ///
    /// 合成失败时返回 `base`，错误文案写进 `composition_errors`；`base` 为 `None` 时本函数
    /// 也返回 `None`（随后 `recompose_provider` 在该 id 没有虚拟模型时会把它从 `Models`
    /// 删除）。也就是说只有存在 base（原生扩展或内置实例）时才能回退到默认实现——models.json
    /// 或扩展注册新增的 provider 没有内置/原生实例时，合成失败（如未配置 auth）就没有兜底。
    /// 一旦确认不再需要合成，历史错误会被清除。
    fn compose_provider(&self, provider_id: &str) -> Option<Arc<dyn Provider>> {
        let native = lock(&self.inner.native_extension_providers)
            .get(provider_id)
            .cloned();
        let base = native.or_else(|| lock(&self.inner.builtins).get(provider_id).cloned());
        let extension = lock(&self.inner.extension_providers)
            .get(provider_id)
            .cloned();
        // `config` 的保护范围覆盖下面的 compose 调用与 composition_errors 更新；
        // 加锁顺序固定为 config → composition_errors。
        let config = lock(&self.inner.config);
        if config.get_provider(provider_id).is_none() && extension.is_none() {
            lock(&self.inner.composition_errors).shift_remove(provider_id);
            return base;
        }
        match compose_model_provider(provider_id, base.clone(), &config, extension.as_ref()) {
            Ok(provider) => {
                lock(&self.inner.composition_errors).shift_remove(provider_id);
                Some(provider)
            }
            Err(error) => {
                lock(&self.inner.composition_errors).insert(provider_id.to_string(), error.message);
                base
            }
        }
    }

    /// `recomposeProvider(providerId)`：返回**不含**虚拟模型的 provider。
    ///
    /// 把合成结果写进底层 `Models` 的 provider 表：
    /// - 有虚拟模型时用 `with_virtual_models` 包装（`provider` 为 `None` 时生成只列虚拟
    ///   条目的 provider）；
    /// - 没有虚拟模型但存在物理 provider 时直接登记；
    /// - 两者都没有时从 `Models` 删除该 id。
    ///
    /// 返回值是未包装的合成 provider，`None` 表示该 id 没有任何物理 provider。
    fn recompose_provider(&self, provider_id: &str) -> Option<Arc<dyn Provider>> {
        let provider = self.compose_provider(provider_id);
        let virtual_models: Vec<Model> = lock(&self.inner.virtual_models)
            .get(provider_id)
            .map(|models| models.values().map(|entry| entry.model.clone()).collect())
            .unwrap_or_default();
        if !virtual_models.is_empty() {
            self.inner.models.set_provider(with_virtual_models(
                provider_id,
                provider.clone(),
                virtual_models,
            ));
        } else if let Some(provider) = &provider {
            self.inner.models.set_provider(Arc::clone(provider));
        } else {
            self.inner.models.delete_provider(provider_id);
        }
        provider
    }

    /// `rebuildProviders()`。
    ///
    /// 清空底层 provider 表与全部历史合成错误，按 `provider_ids` 的顺序逐个重组，最后刷新
    /// 快照。`refresh` 没有指定 provider 列表时走这条全量路径。
    fn rebuild_providers(&self) {
        self.inner.models.clear_providers();
        lock(&self.inner.composition_errors).clear();
        for provider_id in self.provider_ids() {
            self.recompose_provider(&provider_id);
        }
        self.update_model_snapshot();
    }

    /// `updateModelSnapshot()`：`available` 由**旧** `configuredProviders` 推导。
    ///
    /// 仅凭本函数，新注册的 provider 在下一次可用性刷新落地前不会把模型带进 `available`。
    /// 这里更新 `all`，并按**旧** `configured_providers` 重算 `available`；
    /// `configured_providers`/`auth`/`stored_providers` 由可用性刷新以及注册路径维护
    /// （native/扩展注册走 `mark_provisionally_configured`，`register_virtual_model`
    /// 直接写快照）。
    fn update_model_snapshot(&self) {
        let all = self.inner.models.get_models(None);
        let mut snapshot = lock(&self.inner.snapshot);
        let configured = snapshot.configured_providers.clone();
        let available = all
            .iter()
            .filter(|model| configured.contains(&model.provider))
            .cloned()
            .collect();
        snapshot.all = all;
        snapshot.available = available;
    }

    /// 依次检查所有 provider 的 auth（`Promise.all(providers.map(...))`）。
    ///
    /// 每个 provider 一个 `tokio::spawn` 任务，结果带原始下标回传，再按下标还原顺序；
    /// 任意任务报错就立即返回该错误（其余任务已 detach，会继续跑完）。`signal` 克隆给每个
    /// 任务，是调用方取消传播到单项检查的途径。
    async fn check_provider_auths(
        models: Arc<dyn MutableModels>,
        providers: Vec<Arc<dyn Provider>>,
        signal: AbortScope,
    ) -> Result<Vec<(String, Option<AuthCheck>)>, AuthError> {
        let count = providers.len();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        for (index, provider) in providers.into_iter().enumerate() {
            let models = Arc::clone(&models);
            let signal = signal.clone();
            let sender = sender.clone();
            tokio::spawn(async move {
                let provider_id = provider.id().to_string();
                let result = models
                    .check_auth(
                        &provider_id,
                        AuthOperationOptions {
                            signal: Some(signal),
                        },
                    )
                    .await;
                let _ = sender.send((index, provider_id, result));
            });
        }
        drop(sender);
        let mut results: Vec<Option<(String, Option<AuthCheck>)>> =
            (0..count).map(|_| None).collect();
        while let Some((index, provider_id, result)) = receiver.recv().await {
            match result {
                Ok(check) => results[index] = Some((provider_id, check)),
                // TS 的 `Promise.all` 在首个拒绝时返回；其余检查继续在后台跑完。
                Err(error) => return Err(error),
            }
        }
        Ok(results.into_iter().flatten().collect())
    }

    /// `runAvailabilityRefresh(seq, errorSeq, signal)`。
    ///
    /// 阶段：
    /// 1. `try_join3` 并行等待底层全量 `get_available`、逐 provider auth 检查与凭证列表；
    ///    任一失败直接返回 `AuthError`。
    /// 2. 若 `seq` 已不是最新（期间有别的刷新入队），丢弃整批结果并返回 `Ok`。
    /// 3. 用三项结果，并重新读取 `models.get_models(None)` 作为 `all`，整体替换快照。
    /// 4. 仅当 `error_seq` 仍是最新时清除 `availability_error`，避免旧刷新抹掉新错误。
    async fn run_availability_refresh(
        &self,
        seq: u64,
        error_seq: u64,
        signal: &AbortScope,
    ) -> Result<(), AuthError> {
        let providers = self.inner.models.get_providers();
        let options = AuthOperationOptions {
            signal: Some(signal.clone()),
        };
        let (available, checks, credentials) = try_join3(
            self.inner.models.get_available(None, options.clone()),
            Self::check_provider_auths(Arc::clone(&self.inner.models), providers, signal.clone()),
            self.inner.credentials.list(options),
        )
        .await?;
        if seq != self.inner.availability_refresh_seq.load(Ordering::SeqCst) {
            return Ok(());
        }
        let auth: IndexMap<String, AuthCheck> = checks
            .iter()
            .filter_map(|(provider_id, check)| {
                check
                    .as_ref()
                    .map(|check| (provider_id.clone(), check.clone()))
            })
            .collect();
        let configured_providers: HashSet<String> = checks
            .iter()
            .filter(|(_, check)| check.is_some())
            .map(|(provider_id, _)| provider_id.clone())
            .collect();
        let stored_providers: HashSet<String> = credentials
            .into_iter()
            .map(|entry| entry.provider_id)
            .collect();
        {
            let mut snapshot = lock(&self.inner.snapshot);
            *snapshot = ModelRuntimeSnapshot {
                all: self.inner.models.get_models(None),
                available,
                configured_providers,
                stored_providers,
                auth,
            };
        }
        if error_seq == self.inner.availability_error_seq.load(Ordering::SeqCst) {
            *lock(&self.inner.availability_error) = None;
        }
        Ok(())
    }

    /// `queueAvailabilityRefresh(signal?)`。
    ///
    /// 先把全量序号 +1，并把 `provider_availability_seq` 中**已登记**的每个 provider 的局部
    /// 序号 +1：这会让在途的全量刷新与这些 provider 的在途局部刷新结果作废（局部刷新落地前
    /// 会比对 provider 序号）。失败时只有 `error_seq` 仍是最新且调用方未中止，才记录错误文案
    /// （`get_error` 展示时加 `Availability refresh:` 前缀）。
    async fn queue_availability_refresh(
        &self,
        signal: Option<AbortScope>,
    ) -> Result<(), AuthError> {
        let seq = self
            .inner
            .availability_refresh_seq
            .fetch_add(1, Ordering::SeqCst)
            + 1;
        for provider_seq in lock(&self.inner.provider_availability_seq).values_mut() {
            *provider_seq += 1;
        }
        let error_seq = self
            .inner
            .availability_error_seq
            .fetch_add(1, Ordering::SeqCst)
            + 1;
        let effective_signal = operation_signal(signal);
        match self
            .run_availability_refresh(seq, error_seq, &effective_signal)
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => {
                if error_seq == self.inner.availability_error_seq.load(Ordering::SeqCst)
                    && !effective_signal.is_aborted()
                {
                    *lock(&self.inner.availability_error) = Some(auth_error_message(&error));
                }
                Err(error)
            }
        }
    }

    /// `refreshProviderAvailability(providerId, signal)`。
    ///
    /// 单 provider 的可用性刷新，竞态处理都在序号上：
    /// 1. 全量序号 +1，使任何更早开始的全量刷新在落地时被丢弃。
    /// 2. 取该 provider 的局部序号并 +1；刷新前后都靠它判断“结果是否还属于最新一轮”。
    /// 3. 并行等待 `get_available(provider)`、`check_auth` 与 `credentials.read`，之后再用
    ///    `throw_if_aborted` 检查一次取消。
    /// 4. 失败时只有“局部序号仍是我、错误序号仍最新、信号未中止”三者同时成立才记录错误。
    /// 5. 成功但局部序号已变：丢弃结果并返回 `Ok`（更新的刷新会负责落地）。
    /// 6. 合并快照后按 `all` 的顺序重排 `available`，保证全量模型列表顺序稳定。
    /// 7. 错误序号仍最新时清除历史错误。
    async fn refresh_provider_availability(
        &self,
        provider_id: &str,
        signal: &AbortScope,
    ) -> Result<(), AuthError> {
        // 使任何在本凭据变更之前开始的全量可用性刷新失效。
        self.inner
            .availability_refresh_seq
            .fetch_add(1, Ordering::SeqCst);
        let provider_seq = {
            let mut sequences = lock(&self.inner.provider_availability_seq);
            let provider_seq = sequences.get(provider_id).copied().unwrap_or(0) + 1;
            sequences.insert(provider_id.to_string(), provider_seq);
            provider_seq
        };
        let error_seq = self
            .inner
            .availability_error_seq
            .fetch_add(1, Ordering::SeqCst)
            + 1;
        let options = AuthOperationOptions {
            signal: Some(signal.clone()),
        };
        let result = async {
            let (available, auth, credential) = try_join3(
                self.inner
                    .models
                    .get_available(Some(provider_id), options.clone()),
                self.inner.models.check_auth(provider_id, options.clone()),
                self.inner.credentials.read(provider_id, options),
            )
            .await?;
            throw_if_aborted(signal)?;
            Ok::<_, AuthError>((available, auth, credential))
        }
        .await;
        let (available, auth, credential) = match result {
            Ok(value) => value,
            Err(error) => {
                let current_provider_seq = lock(&self.inner.provider_availability_seq)
                    .get(provider_id)
                    .copied();
                if current_provider_seq == Some(provider_seq)
                    && error_seq == self.inner.availability_error_seq.load(Ordering::SeqCst)
                    && !signal.is_aborted()
                {
                    *lock(&self.inner.availability_error) = Some(auth_error_message(&error));
                }
                return Err(error);
            }
        };
        if lock(&self.inner.provider_availability_seq)
            .get(provider_id)
            .copied()
            != Some(provider_seq)
        {
            return Ok(());
        }
        let mut snapshot = lock(&self.inner.snapshot);
        let mut configured_providers = snapshot.configured_providers.clone();
        let mut stored_providers = snapshot.stored_providers.clone();
        let mut auth_by_provider = snapshot.auth.clone();
        match &auth {
            Some(auth) => {
                configured_providers.insert(provider_id.to_string());
                auth_by_provider.insert(provider_id.to_string(), auth.clone());
            }
            None => {
                configured_providers.remove(provider_id);
                auth_by_provider.shift_remove(provider_id);
            }
        }
        if credential.is_some() {
            stored_providers.insert(provider_id.to_string());
        } else {
            stored_providers.remove(provider_id);
        }
        let all = self.inner.models.get_models(None);
        // 以 “provider\0model” 为键合并：先保留其他 provider 的旧可用条目，再并入本次结果
        // （同 provider 的旧条目被整体替换）。用 NUL 分隔避免两个字符串拼接后产生歧义。
        let mut available_by_id: IndexMap<String, Model> = IndexMap::new();
        for model in snapshot
            .available
            .iter()
            .filter(|model| model.provider != provider_id)
            .chain(available.iter())
        {
            available_by_id.insert(format!("{}\0{}", model.provider, model.id), model.clone());
        }
        let available_ordered: Vec<Model> = all
            .iter()
            .filter_map(|model| {
                available_by_id
                    .get(&format!("{}\0{}", model.provider, model.id))
                    .cloned()
            })
            .collect();
        *snapshot = ModelRuntimeSnapshot {
            all,
            available: available_ordered,
            configured_providers,
            stored_providers,
            auth: auth_by_provider,
        };
        drop(snapshot);
        if error_seq == self.inner.availability_error_seq.load(Ordering::SeqCst) {
            *lock(&self.inner.availability_error) = None;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 查询方法
    // -----------------------------------------------------------------------

    /// `getProviders()`。
    ///
    /// 返回底层 `Models` 当前登记的全部 provider；纯虚拟 provider 也在其中（包装后）。
    pub fn get_providers(&self) -> Vec<Arc<dyn Provider>> {
        self.inner.models.get_providers()
    }

    /// `getProvider(providerId)`。
    ///
    /// 取合成后的 provider（虚拟条目已由 `with_virtual_models` 包装在内）；未注册时为
    /// `None`。
    pub fn get_provider(&self, provider_id: &str) -> Option<Arc<dyn Provider>> {
        self.inner.models.get_provider(provider_id)
    }

    /// `getModels(providerId?)`。
    ///
    /// `None` 表示所有 provider。结果包含已注册的虚拟条目；需要物理模型时配合
    /// [`ModelRuntime::get_physical_model`]。
    pub fn get_models(&self, provider_id: Option<&str>) -> Vec<Model> {
        self.inner.models.get_models(provider_id)
    }

    /// `getModel(providerId, modelId)`。
    ///
    /// 含虚拟条目；过滤虚拟条目请用 [`ModelRuntime::get_physical_model`]。
    pub fn get_model(&self, provider_id: &str, model_id: &str) -> Option<Model> {
        self.inner.models.get_model(provider_id, model_id)
    }

    /// `getModelsOfType(type, providerId?)`。
    ///
    /// 按 `Model::model_type` 过滤；`provider_id` 为 `None` 表示所有 provider。
    pub fn get_models_of_type(
        &self,
        model_type: ModelType,
        provider_id: Option<&str>,
    ) -> Vec<Model> {
        self.inner
            .models
            .get_models_of_type(model_type, provider_id)
    }

    /// `getModelOfType(type, providerId, modelId)`。
    ///
    /// 类型不匹配时返回 `None`（不会回退到其它类型）。
    pub fn get_model_of_type(
        &self,
        model_type: ModelType,
        provider_id: &str,
        model_id: &str,
    ) -> Option<Model> {
        self.inner
            .models
            .get_model_of_type(model_type, provider_id, model_id)
    }

    /// `getAllModels(providerId?)`。
    ///
    /// 与 [`ModelRuntime::get_models`] 使用相同的 provider 选择（`None` = 全部），但按
    /// `Model::model_type` 包装成 [`AnyModel`] 变体（chat/image/classifier）。
    pub fn get_all_models(&self, provider_id: Option<&str>) -> Vec<AnyModel> {
        self.inner.models.get_all_models(provider_id)
    }

    /// `getAvailableOfType(type, providerId?, options?)`。
    ///
    /// 转发底层实现；与 [`ModelRuntime::get_available`] 不同，它不更新本运行时的
    /// `availability_error` 或快照。
    pub async fn get_available_of_type(
        &self,
        model_type: ModelType,
        provider_id: Option<&str>,
        options: AuthOperationOptions,
    ) -> Result<Vec<Model>, AuthError> {
        self.inner
            .models
            .get_available_of_type(model_type, provider_id, options)
            .await
    }

    /// `getAllAvailable(providerId?, options?)`。
    ///
    /// 转发底层实现并包装成 [`AnyModel`]；同样不更新本运行时的快照或错误状态。
    pub async fn get_all_available(
        &self,
        provider_id: Option<&str>,
        options: AuthOperationOptions,
    ) -> Result<Vec<AnyModel>, AuthError> {
        self.inner
            .models
            .get_all_available(provider_id, options)
            .await
    }

    /// `checkAuth(providerId, options?)`。
    ///
    /// 直接转交底层 auth 检查；`None` 表示该 provider 未配置可用凭证。本方法不写快照。
    pub async fn check_auth(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<Option<AuthCheck>, AuthError> {
        self.inner.models.check_auth(provider_id, options).await
    }

    /// `getAvailable(providerId?, options?)`。
    ///
    /// - `provider_id` 为 `Some` 时只向底层询问该 provider 的可用模型，不重建全量快照，但会
    ///   更新 `availability_error`；
    /// - 为 `None`（或空串，等价 TS 的 falsy）时执行一次全量可用性刷新，然后返回快照中
    ///   `available` 的副本。
    pub async fn get_available(
        &self,
        provider_id: Option<&str>,
        options: AuthOperationOptions,
    ) -> Result<Vec<Model>, AuthError> {
        // TS 的 `if (providerId)`：空串是 falsy，与未给出一样走全量刷新分支。
        let provider_id = provider_id.filter(|provider_id| !provider_id.is_empty());
        match provider_id {
            Some(provider_id) => {
                let error_seq = self
                    .inner
                    .availability_error_seq
                    .fetch_add(1, Ordering::SeqCst)
                    + 1;
                match self
                    .inner
                    .models
                    .get_available(Some(provider_id), options.clone())
                    .await
                {
                    Ok(available) => {
                        if error_seq == self.inner.availability_error_seq.load(Ordering::SeqCst) {
                            *lock(&self.inner.availability_error) = None;
                        }
                        Ok(available)
                    }
                    Err(error) => {
                        let aborted = options.signal.as_ref().is_some_and(AbortScope::is_aborted);
                        if error_seq == self.inner.availability_error_seq.load(Ordering::SeqCst)
                            && !aborted
                        {
                            *lock(&self.inner.availability_error) =
                                Some(auth_error_message(&error));
                        }
                        Err(error)
                    }
                }
            }
            None => {
                self.queue_availability_refresh(options.signal).await?;
                Ok(lock(&self.inner.snapshot).available.clone())
            }
        }
    }

    /// 运行时的稳定身份（内部 `Arc` 的数据地址）。
    ///
    /// TS 的 `ModelCatalogRefreshCoordinator` 用 `WeakMap<ModelRuntime, …>` 以对象同一性作键；
    /// Rust 的 [`ModelRuntime`] 是 `Arc<ModelRuntimeInner>` 的克隆句柄，同一运行时的所有克隆
    /// 共享该地址，因此这里是「对象同一性」的等价物。只用于进程内映射的键，不参与序列化。
    pub fn identity(&self) -> usize {
        Arc::as_ptr(&self.inner) as usize
    }

    /// `getAvailableSnapshot()`：不触发刷新。
    pub fn get_available_snapshot(&self) -> Vec<Model> {
        lock(&self.inner.snapshot).available.clone()
    }

    /// `getError()`：config 错误 + 每个 provider 合成错误 + 可用性刷新错误，`"\n\n"` 连接。
    ///
    /// 顺序固定：models.json 的加载/解析错误 → `composition_errors`（按插入序）→ 最近一次
    /// 可用性错误。全部为空时返回 `None`。
    pub fn get_error(&self) -> Option<String> {
        let mut errors: Vec<String> = Vec::new();
        if let Some(config_error) = lock(&self.inner.config).get_error() {
            errors.push(config_error.to_string());
        }
        for (provider_id, error) in lock(&self.inner.composition_errors).iter() {
            errors.push(format!("Provider \"{provider_id}\": {error}"));
        }
        if let Some(error) = lock(&self.inner.availability_error).as_ref() {
            errors.push(format!("Availability refresh: {error}"));
        }
        if errors.is_empty() {
            None
        } else {
            Some(errors.join("\n\n"))
        }
    }

    /// `getRegisteredProviderConfig(providerId)`。
    ///
    /// 只返回扩展注册的配置；models.json 里的条目与原生 provider 不在此列。
    pub fn get_registered_provider_config(&self, provider_id: &str) -> Option<ProviderConfigInput> {
        lock(&self.inner.extension_providers)
            .get(provider_id)
            .cloned()
    }

    /// `getRegisteredProviderIds()`：扩展注册在前、原生 provider 在后，去重。
    ///
    /// 只包含这两个来源（不含内置 provider、models.json 与虚拟模型）。
    pub fn get_registered_provider_ids(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut ids = Vec::new();
        for id in lock(&self.inner.extension_providers)
            .keys()
            .cloned()
            .collect::<Vec<_>>()
        {
            if seen.insert(id.clone()) {
                ids.push(id);
            }
        }
        for id in lock(&self.inner.native_extension_providers)
            .keys()
            .cloned()
            .collect::<Vec<_>>()
        {
            if seen.insert(id.clone()) {
                ids.push(id);
            }
        }
        ids
    }

    /// `getRegisteredNativeProvider(providerId)`。
    ///
    /// 只查原生注册表；扩展注册的配置请用
    /// [`ModelRuntime::get_registered_provider_config`]。
    pub fn get_registered_native_provider(&self, provider_id: &str) -> Option<Arc<dyn Provider>> {
        lock(&self.inner.native_extension_providers)
            .get(provider_id)
            .cloned()
    }

    /// `getCompatibilityRequestConfig(model)`（`@internal`）。
    ///
    /// 用 models.json/扩展配置解析该模型的请求头与 `authHeader`；解析失败（配置值无法求值）
    /// 转成 [`ModelRuntimeError::Message`]。供 [`crate::models::registry::ModelRegistry`] 的
    /// 兼容性查询使用。
    pub fn get_compatibility_request_config(
        &self,
        model: &Model,
    ) -> Result<CompatibilityRequestConfig, ModelRuntimeError> {
        let any_model = typed_any_model(model);
        let config = lock(&self.inner.config).get_provider(&model.provider);
        let extension = lock(&self.inner.extension_providers)
            .get(&model.provider)
            .cloned();
        resolve_compatibility_request_config(&any_model, config.as_deref(), extension.as_ref())
            .map_err(|error| ModelRuntimeError::Message(error.to_string()))
    }

    /// `isUsingOAuth(providerId)`。
    ///
    /// 只读最近一次可用性快照里的 auth 检查，不触发探测；快照未建立时返回 `false`。
    pub fn is_using_oauth(&self, provider_id: &str) -> bool {
        lock(&self.inner.snapshot)
            .auth
            .get(provider_id)
            .is_some_and(|check| check.r#type == AuthType::OAuth)
    }

    /// `isUsingSubscription(providerId)`。
    ///
    /// 先要求快照判定为 OAuth，再看该 provider 声明的 oauth 配置是否 `is_subscription()`；
    /// provider 不存在或没有 oauth 声明时为 `false`。
    pub fn is_using_subscription(&self, provider_id: &str) -> bool {
        self.is_using_oauth(provider_id)
            && self
                .inner
                .models
                .get_provider(provider_id)
                .and_then(|provider| provider.auth().oauth.clone())
                .is_some_and(|oauth| oauth.is_subscription())
    }

    /// `hasConfiguredAuth(providerId)`。
    ///
    /// 只读快照中的 `configured_providers`，不触发探测。注册/凭据同步路径会提前写入这个
    /// 集合（见 `mark_provisionally_configured` 与 `register_virtual_model`）。
    pub fn has_configured_auth(&self, provider_id: &str) -> bool {
        lock(&self.inner.snapshot)
            .configured_providers
            .contains(provider_id)
    }

    // -----------------------------------------------------------------------
    // getAuth
    // -----------------------------------------------------------------------

    /// `getAuth(providerId, overrides?)`。
    ///
    /// 直接转交底层的 provider 级 auth 解析；`overrides` 里的 `apiKey`/`env` 等按
    /// [`ModelRuntimeAuthOverrides`] 的语义生效，错误原样返回。
    pub async fn get_auth_for_provider(
        &self,
        provider_id: &str,
        overrides: ModelRuntimeAuthOverrides,
    ) -> Result<Option<AuthResult>, AuthError> {
        self.inner
            .models
            .get_auth(GetAuthTarget::Provider(provider_id), overrides)
            .await
    }

    /// `getAuth(model, overrides?)`：在 provider auth 之上合并配置的模型/提供商请求头。
    ///
    /// 底层按 `GetAuthTarget::Model` 解析，返回 `None` 时直接返回 `None`。解析成功后：
    /// - 配置头的解析环境是 `resolution.env` 再叠加 `overrides.env`（后者覆盖前者）；
    /// - 配置头与 auth 头通过 `merge_headers` 合并，同名（大小写不敏感）时**配置头优先**；
    /// - 配置头解析失败转成普通 [`AuthError`] 返回；
    /// - 返回的 `env` 仍是底层解析结果，不包含 `overrides.env`（那只是解析配置头的输入）。
    pub async fn get_auth_for_model(
        &self,
        model: &Model,
        overrides: ModelRuntimeAuthOverrides,
    ) -> Result<Option<AuthResult>, AuthError> {
        let model = model.clone();
        let resolution = self
            .inner
            .models
            .get_auth(GetAuthTarget::Model(&model), overrides.clone())
            .await?;
        let Some(resolution) = resolution else {
            return Ok(None);
        };
        let any_model = typed_any_model(&model);
        let config = lock(&self.inner.config).get_provider(&model.provider);
        let extension = lock(&self.inner.extension_providers)
            .get(&model.provider)
            .cloned();
        let mut env: ProviderEnv = resolution.env.clone().unwrap_or_default();
        if let Some(overrides_env) = &overrides.env {
            for (key, value) in overrides_env {
                env.insert(key.clone(), value.clone());
            }
        }
        let configured = resolve_configured_model_headers(
            &any_model,
            config.as_deref(),
            extension.as_ref(),
            Some(&env),
        )
        .map_err(|error| AuthError::new(error.to_string()))?;
        let configured_headers: Option<ProviderHeaders> = configured.map(|headers| {
            headers
                .into_iter()
                .map(|(name, value)| (name, Some(value)))
                .collect()
        });
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: resolution.auth.api_key,
                headers: merge_headers(resolution.auth.headers, configured_headers),
                base_url: resolution.auth.base_url,
            },
            env: resolution.env,
            source: resolution.source,
        }))
    }

    /// `getAuth` 的 trait 版本：按目标分派到上面两个方法。
    async fn get_auth_target(
        &self,
        target: GetAuthTarget<'_>,
        overrides: ModelRuntimeAuthOverrides,
    ) -> Result<Option<AuthResult>, AuthError> {
        match target {
            GetAuthTarget::Provider(provider_id) => {
                self.get_auth_for_provider(provider_id, overrides).await
            }
            GetAuthTarget::Model(model) => {
                let model = model.clone();
                self.get_auth_for_model(&model, overrides).await
            }
        }
    }

    // -----------------------------------------------------------------------
    // 凭据操作
    // -----------------------------------------------------------------------

    /// 每个 provider 一把按需创建的 `tokio::sync::Mutex`；条目在运行时生命周期内不清理，
    /// 因此排队中的任务与后续操作看到的是同一把锁。
    fn credential_lock(&self, provider_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        lock(&self.inner.credential_locks)
            .entry(provider_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// TS 的 `enqueueCredentialOperation`：同 provider 串行；调用方在**轮到自己开始**时
    /// resolve；任务与调用方解耦——调用方的 future 被丢弃（Rust 的取消方式）后任务仍在
    /// 后台跑完，并照常提交/回报结果（§2.20.10「任务仍在链上」）。
    ///
    /// 只有 `'static` 的任务能这样分离；借用调用方栈上 interaction 的 `login` 走
    /// [`Self::enqueue_credential_operation_inline`]。
    async fn enqueue_credential_operation<T, F, Fut>(
        &self,
        provider_id: &str,
        signal: &AbortScope,
        task: F,
    ) -> Result<T, ModelRuntimeError>
    where
        T: Send + 'static,
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, ModelRuntimeError>> + Send + 'static,
    {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            // 没有 tokio 运行时（例如 `futures::executor::block_on`）：无法 spawn，
            // 退回调用方内联执行。
            return self
                .enqueue_credential_operation_inline(provider_id, signal, task)
                .await;
        };
        let lock = self.credential_lock(provider_id);
        let task_signal = signal.clone();
        let (started_sender, started) = tokio::sync::oneshot::channel::<()>();
        // `operation`：先等前一个操作的 tail（互斥锁）；排队本身不因调用方取消而放弃。
        let operation = handle.spawn(async move {
            let permit = lock.lock_owned().await;
            // `signal.throwIfAborted()`：轮到自己开始时已中止 → 不执行任务。
            let aborted = throw_if_aborted(&task_signal).err();
            let _ = started_sender.send(());
            let result = match aborted {
                Some(error) => Err(ModelRuntimeError::Auth(error)),
                None => task().await,
            };
            drop(permit);
            result
        });
        // `raceWithAbortSignal(started, signal)`：轮到自己之前可被调用方取消。
        if let Err(error) = race_with_abort_signal(started, signal).await {
            return Err(ModelRuntimeError::from(error));
        }
        // 调用方在这里被丢弃不影响后台任务（JoinHandle 被丢弃即 detach）。
        match operation.await {
            Ok(result) => result,
            // 被放弃的任务 panic：与 `race_with_abort_signal` 一致，原样传播。
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(_) => Err(ModelRuntimeError::Auth(AuthError::new(
                "Credential operation task was cancelled",
            ))),
        }
    }

    /// 借用型任务的串行执行（以及没有 tokio 运行时时 [`Self::enqueue_credential_operation`]
    /// 的回退）。
    ///
    /// 任务在调用方 future 里执行：signal 中止只让调用方先行返回，任务仍会跑完；但调用方
    /// future 被丢弃（`timeout`/`select!`）会中断任务——借用 interaction 的 login 无法安全
    /// `spawn`（Rust 借用规则；spec §8.2 已注明这一不可完全等价处）。
    async fn enqueue_credential_operation_inline<T, F, Fut>(
        &self,
        provider_id: &str,
        signal: &AbortScope,
        task: F,
    ) -> Result<T, ModelRuntimeError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, ModelRuntimeError>>,
    {
        let lock = self.credential_lock(provider_id);
        // `raceWithAbortSignal(permiso, signal)`：等锁期间可被调用方取消；被放弃的等锁
        // 任务在后台继续、拿到许可后立即释放（等价 TS 里被放弃但仍排队的 operation）。
        let permit = if tokio::runtime::Handle::try_current().is_ok() {
            match race_with_abort_signal(lock.lock_owned(), signal).await {
                Ok(permit) => permit,
                Err(error) => return Err(ModelRuntimeError::from(error)),
            }
        } else {
            // 无运行时：不能 spawn 等锁任务，只能在当前 future 里 select（中止即放弃排队）。
            tokio::select! {
                _ = signal.cancelled() => {
                    let reason = signal
                        .reason()
                        .unwrap_or_else(AbortReason::fallback_abort_error);
                    return Err(ModelRuntimeError::Auth(AuthError::with_name(
                        reason.name,
                        reason.message,
                    )));
                }
                permit = lock.lock_owned() => permit,
            }
        };
        // `signal.throwIfAborted()`：轮到自己开始时已中止 → 不执行任务。
        if let Err(error) = throw_if_aborted(signal) {
            drop(permit);
            return Err(ModelRuntimeError::Auth(error));
        }
        let result = task().await;
        drop(permit);
        result
    }

    /// `synchronizeCredentialState(providerId, operation, credential, signal)`。
    ///
    /// 前置条件：凭证**已经**提交（login 已写入 store、set/removeRuntimeApiKey 已改覆盖层），
    /// 本函数只负责让本地目录与快照跟上，不再写凭证。
    ///
    /// 阶段：检查取消 → 重组该 provider（有合成错误立即失败）→ 只对该 provider 做一次禁网
    /// 目录刷新 → 刷新被中止或该 provider 报错则失败 → 更新模型快照 → 单 provider 可用性
    /// 刷新。任何失败都包成 [`CredentialSynchronizationError`]（携带 `operation` 与已提交的
    /// `credential` 及 `cause`），调用方据此区分“凭证已提交但本地同步失败”。
    async fn synchronize_credential_state(
        &self,
        provider_id: &str,
        operation: CredentialSynchronizationOperation,
        credential: Option<Credential>,
        signal: &AbortScope,
    ) -> Result<(), CredentialSynchronizationError> {
        let result: Result<(), AiError> = async {
            throw_if_aborted(signal).map_err(|error| error.to_ai_error())?;
            self.recompose_provider(provider_id);
            if let Some(composition_error) = lock(&self.inner.composition_errors)
                .get(provider_id)
                .cloned()
            {
                return Err(AiError::new(AiErrorKind::Other, composition_error));
            }
            let result = self
                .inner
                .models
                .refresh(ModelsRefreshOptions {
                    allow_network: Some(false),
                    providers: Some(vec![provider_id.to_string()]),
                    force: None,
                    signal: Some(signal.clone()),
                })
                .await?;
            if result.aborted {
                throw_if_aborted(signal).map_err(|error| error.to_ai_error())?;
            }
            if let Some(refresh_error) = result.errors.get(provider_id) {
                return Err(refresh_error.clone());
            }
            self.update_model_snapshot();
            self.refresh_provider_availability(provider_id, signal)
                .await
                .map_err(|error| error.to_ai_error())?;
            Ok(())
        }
        .await;
        result.map_err(|cause| CredentialSynchronizationError {
            provider_id: provider_id.to_string(),
            operation,
            credential,
            cause: Some(auth_error_from_ai(&cause)),
        })
    }

    /// `setRuntimeApiKey(providerId, apiKey, options?)`。
    ///
    /// 任务开始后先同步写入运行时覆盖层（不会失败），再在串行队列里做本地同步；同步失败时
    /// 返回 [`CredentialSynchronizationError`]，但 runtime key 已经生效，后续
    /// [`ModelRuntime::get_provider_auth_status`] 会优先报告 `Runtime` 来源。
    pub async fn set_runtime_api_key(
        &self,
        provider_id: &str,
        api_key: &str,
        options: AuthOperationOptions,
    ) -> Result<(), ModelRuntimeError> {
        let signal = operation_signal(options.signal);
        let provider_id = provider_id.to_string();
        let api_key = api_key.to_string();
        let queue_id = provider_id.clone();
        let runtime = self.clone();
        let task_signal = signal.clone();
        self.enqueue_credential_operation(&queue_id, &signal, move || async move {
            runtime
                .inner
                .credentials
                .set_runtime_api_key(&provider_id, &api_key);
            runtime
                .synchronize_credential_state(
                    &provider_id,
                    CredentialSynchronizationOperation::SetRuntimeApiKey,
                    Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some(api_key.clone()),
                        env: None,
                        extra: IndexMap::new(),
                    })),
                    &task_signal,
                )
                .await
                .map_err(ModelRuntimeError::CredentialSync)
        })
        .await
    }

    /// `removeRuntimeApiKey(providerId, options?)`。
    ///
    /// 任务开始后先从覆盖层移除（同步、不会失败），再串行做本地同步；失败只可能来自同步
    /// 阶段，此时覆盖项已经不存在。
    pub async fn remove_runtime_api_key(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<(), ModelRuntimeError> {
        let signal = operation_signal(options.signal);
        let provider_id = provider_id.to_string();
        let queue_id = provider_id.clone();
        let runtime = self.clone();
        let task_signal = signal.clone();
        self.enqueue_credential_operation(&queue_id, &signal, move || async move {
            runtime
                .inner
                .credentials
                .remove_runtime_api_key(&provider_id);
            runtime
                .synchronize_credential_state(
                    &provider_id,
                    CredentialSynchronizationOperation::RemoveRuntimeApiKey,
                    None,
                    &task_signal,
                )
                .await
                .map_err(ModelRuntimeError::CredentialSync)
        })
        .await
    }

    /// `listCredentials(options?)`。
    ///
    /// 转交 `RuntimeCredentials::list`：持久层条目与运行时覆盖项合并后返回，同一 provider
    /// 只出现一次。
    pub async fn list_credentials(
        &self,
        options: AuthOperationOptions,
    ) -> Result<Vec<CredentialInfo>, AuthError> {
        self.inner.credentials.list(options).await
    }

    /// `getProviderAuthStatus(providerId)`（顺序固定，见 §2.20.10）。
    ///
    /// 判定顺序（先命中先返回）：
    /// 1. 运行时 API key → [`AuthStatusSource::Runtime`]；
    /// 2. 快照的 `stored_providers`（`RuntimeCredentials::list` 的持久层 + 运行时覆盖）→
    ///    `Stored`；
    /// 3. models.json / 扩展注册里的请求级 API key 配置 →
    ///    `configured_request_auth_status` 给出的来源（命令、环境变量、models.json key 等）；
    /// 4. 快照里的 auth 检查（环境变量等）→ `Environment`，`label` 为检查的 `source`；
    /// 5. 都没有 → `configured: false`。
    pub fn get_provider_auth_status(&self, provider_id: &str) -> AuthStatus {
        if self.inner.credentials.has_runtime_api_key(provider_id) {
            return AuthStatus {
                configured: true,
                source: Some(AuthStatusSource::Runtime),
                label: None,
            };
        }
        if lock(&self.inner.snapshot)
            .stored_providers
            .contains(provider_id)
        {
            return AuthStatus {
                configured: true,
                source: Some(AuthStatusSource::Stored),
                label: None,
            };
        }
        let config = lock(&self.inner.config).get_provider(provider_id);
        let extension = lock(&self.inner.extension_providers)
            .get(provider_id)
            .cloned();
        if let Some(configured) =
            configured_request_auth_status(config.as_deref(), extension.as_ref())
        {
            return configured;
        }
        let check = lock(&self.inner.snapshot).auth.get(provider_id).cloned();
        match check {
            Some(check) => AuthStatus {
                configured: true,
                source: Some(AuthStatusSource::Environment),
                label: check.source,
            },
            None => AuthStatus {
                configured: false,
                source: None,
                label: None,
            },
        }
    }

    // -----------------------------------------------------------------------
    // prepareRequest
    // -----------------------------------------------------------------------

    /// `prepareRequest(model, options)`：解析 auth、合并 session/auth/请求头、应用
    /// `transformHeaders`、合并 env。
    ///
    /// 头合并次序（逐字）：OpenCode session 头 → auth 头（含模型静态头与配置头）→ 请求选项头
    /// → `transformHeaders`（只调用一次，且不转发给 provider）。
    ///
    /// 返回的 [`PreparedRequest`] 中：`provider` 来自底层 `Models`；`model` 可能因 auth 解析
    /// 出的 `baseUrl` 被替换；`options` 已填好合并后的 `api_key`/`headers`/`env`。
    /// `reasoning` 取显式参数，否则沿用 `options.extra["reasoning"]` 的字符串值。
    ///
    /// 错误：未知 provider 与“provider 未配置”都是 [`AiError`]（分别用
    /// `TsModelsErrorCode::Provider` / `TsModelsErrorCode::Auth`）；auth 解析错误会先经过
    /// `normalize_request_abort_error` 归一化中止文案。
    pub async fn prepare_request(
        &self,
        model: &Model,
        options: ModelsStreamOptions,
    ) -> Result<PreparedRequest, AiError> {
        let ModelsStreamOptions {
            base,
            transforms,
            reasoning,
            signal_reason,
        } = options;
        let mut prepared = self
            .prepare_request_core(model, base, &transforms, signal_reason.as_ref())
            .await?;
        let effective_reasoning = reasoning.or_else(|| {
            prepared
                .options
                .extra
                .get("reasoning")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
        if let Some(reasoning) = effective_reasoning {
            prepared
                .options
                .extra
                .insert("reasoning".to_string(), JsonValue::String(reasoning));
        }
        Ok(prepared)
    }

    /// `prepareRequest` 的核心：所有选项形态共用。
    async fn prepare_request_core(
        &self,
        model: &Model,
        base: StreamOptions,
        transforms: &kiana_ai::models::ModelsRequestTransforms,
        signal_reason: Option<&AbortReason>,
    ) -> Result<PreparedRequest, AiError> {
        // 未知 provider 在这里就失败（错误码 provider），避免继续用空 auth 发请求。
        let provider = self
            .inner
            .models
            .get_provider(&model.provider)
            .ok_or_else(|| {
                models_error_ai(
                    TsModelsErrorCode::Provider,
                    &format!("Unknown provider: {}", model.provider),
                )
            })?;
        // 请求选项里的 apiKey/env 作为 auth 覆盖传入；cancel 信号也来自请求选项。
        let signal_scope = base.signal.clone().map(AbortScope::from);
        let resolution = self
            .get_auth_for_model(
                model,
                ModelRuntimeAuthOverrides {
                    api_key: base.api_key.clone(),
                    env: base.env.clone(),
                    min_oauth_validity_ms: None,
                    signal: signal_scope,
                },
            )
            .await
            .map_err(|error| normalize_request_abort_error(error, signal_reason))?;
        let Some(resolution) = resolution else {
            return Err(models_error_ai(
                TsModelsErrorCode::Auth,
                &format!("Provider is not configured: {}", model.provider),
            ));
        };
        // auth 解析出的 baseUrl 只替换本次请求模型的 base_url，不回写目录。
        let request_model = match &resolution.auth.base_url {
            Some(base_url) if !base_url.is_empty() => Model {
                base_url: base_url.clone(),
                ..model.clone()
            },
            _ => model.clone(),
        };
        let target = ClientIdentityTarget::new(&request_model.provider, &request_model.base_url);
        let session_id = base.session_id.as_deref();
        // 合并次序（后者覆盖同名头）：session 头 → auth 头 → 请求选项头。
        let mut headers = merge_headers(
            merge_headers(
                get_open_code_session_headers(&target, session_id),
                resolution.auth.headers.clone(),
            ),
            base.headers.clone(),
        );
        // transform 只在本地应用一次；结果放进 options，不会交给 provider 再调用。
        if let Some(transform) = &transforms.transform_headers {
            headers = Some(transform(headers.unwrap_or_default()).await);
        }
        // env 同样是“请求级覆盖 auth 级”；两边都没有时保持 None。
        let env = if resolution.env.is_some() || base.env.is_some() {
            let mut merged = resolution.env.clone().unwrap_or_default();
            if let Some(request_env) = base.env.clone() {
                for (key, value) in request_env {
                    merged.insert(key, value);
                }
            }
            Some(merged)
        } else {
            None
        };
        // 调用方显式给的 apiKey 优先于 auth 解析结果。
        let api_key = base
            .api_key
            .clone()
            .or_else(|| resolution.auth.api_key.clone());
        let mut options = base;
        options.api_key = api_key;
        options.headers = headers;
        options.env = env;
        Ok(PreparedRequest {
            provider,
            model: request_model,
            options,
        })
    }

    /// `prepareRequest` 的 `SimpleStreamOptions` 形态。
    ///
    /// 复用 `prepare_request_core` 处理内层 `StreamOptions`，再把 `SimpleStreamOptions`
    /// 独有的 `tool_choice`/`reasoning`/`deferred`/`thinking_budgets` 原样拼回。
    async fn prepare_simple_request(
        &self,
        model: &Model,
        options: ModelsSimpleStreamOptions,
    ) -> Result<(Arc<dyn Provider>, Model, SimpleStreamOptions), AiError> {
        let ModelsSimpleStreamOptions {
            base,
            transforms,
            signal_reason,
        } = options;
        let SimpleStreamOptions {
            base: stream_options,
            tool_choice,
            reasoning,
            deferred,
            thinking_budgets,
        } = base;
        let prepared = self
            .prepare_request_core(model, stream_options, &transforms, signal_reason.as_ref())
            .await?;
        Ok((
            prepared.provider,
            prepared.model,
            SimpleStreamOptions {
                base: prepared.options,
                tool_choice,
                reasoning,
                deferred,
                thinking_budgets,
            },
        ))
    }

    // -----------------------------------------------------------------------
    // 流式接口
    // -----------------------------------------------------------------------

    /// `stream(model, context, options?)`。
    ///
    /// 同步返回惰性流。返回前 `lazy_stream` 会同步 poll setup 一次：`assert_chat_model`
    /// 立即执行，[`ModelRuntime::prepare_request`] 在 setup 内启动到第一个 await，其余在
    /// 后台任务完成。setup 失败（未知 provider、未配置、非 chat 模型等）不会从本方法返回
    /// `Err`，而是由 `lazy_stream` 转成 `stopReason: "error"` 的 assistant 消息。
    pub fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: ModelsStreamOptions,
    ) -> AssistantMessageEventStream {
        let transcript = normalize_context(context);
        let inner = Arc::clone(&self.inner);
        let model_for_setup = model.clone();
        let setup: LazySetup = Box::pin(async move {
            assert_chat_model(&typed_any_model(&model_for_setup))
                .map_err(ai_error_from_models_error)?;
            let prepared = ModelRuntime { inner }
                .prepare_request(&model_for_setup, options)
                .await?;
            Ok(prepared
                .provider
                .stream(&prepared.model, &transcript, prepared.options))
        });
        lazy_stream(model, setup, None)
    }

    /// `complete(model, context, options?)`。
    ///
    /// 等价于 `stream(...)` 后等待 `result()`；若流结束却没有结果，`stream_result` 会合成一条
    /// setup 错误消息返回（而不是 panic）。
    pub async fn complete(
        &self,
        model: &Model,
        context: &Context,
        options: ModelsStreamOptions,
    ) -> AssistantMessage {
        let stream = self.stream(model, context, options);
        stream_result(stream, model).await
    }

    /// `streamSimple(model, context, options?)`。
    ///
    /// 与 [`ModelRuntime::stream`] 一样同步返回惰性流、setup 错误进入流内错误消息。区别是
    /// 模型被 `is_virtual_model` 判定为虚拟模型时改走 `stream_simple_virtual`（先路由到物理
    /// 模型再发请求）；该判定只看 `Model::api` 是否为虚拟目录标记，与 provider 字段无关。
    pub fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: ModelsSimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        let transcript = normalize_context(context);
        let inner = Arc::clone(&self.inner);
        let model_for_setup = model.clone();
        if is_virtual_model(model) {
            let transcript_for_setup = transcript.clone();
            let setup: LazySetup = Box::pin(async move {
                let runtime = ModelRuntime { inner };
                runtime
                    .stream_simple_virtual(&model_for_setup, &transcript_for_setup, options)
                    .await
            });
            return lazy_stream(model, setup, None);
        }
        let setup: LazySetup = Box::pin(async move {
            assert_chat_model(&typed_any_model(&model_for_setup))
                .map_err(ai_error_from_models_error)?;
            let runtime = ModelRuntime { inner };
            let (provider, request_model, request_options) = runtime
                .prepare_simple_request(&model_for_setup, options)
                .await?;
            Ok(provider.stream_simple(&request_model, &transcript, request_options))
        });
        lazy_stream(model, setup, None)
    }

    /// `streamSimple` 的虚拟模型分派（§2.20.12）：路由、裁剪 `maxTokens`、跨 provider 丢弃
    /// 调用方凭据。
    ///
    /// 步骤：以 `Direct` 原因调用 [`ModelRuntime::resolve_model`]（thinking level 取请求里的
    /// `reasoning`，缺省 `Off`）→ 按目标模型声明裁剪 `max_tokens` → 路由目标与虚拟模型
    /// provider 不同时清空调用方传入的 `api_key`/`headers`/`env`（避免把 A provider 的凭据
    /// 发给 B）→ 再走普通 `prepare_simple_request`。
    async fn stream_simple_virtual(
        &self,
        model: &Model,
        transcript: &TranscriptContext,
        options: ModelsSimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, AiError> {
        let route = self
            .resolve_model(
                model,
                &transcript.0.messages,
                ResolveModelOptions {
                    reason: ModelRouteReason::Direct,
                    thinking_level: options
                        .base
                        .reasoning
                        .map(model_thinking_level)
                        .unwrap_or(ModelThinkingLevel::Off),
                    signal: options.base.base.signal.clone().map(AbortScope::from),
                    failed: None,
                    state: None,
                },
            )
            .await
            .map_err(|error| error_to_ai(&error))?;
        // 只裁剪到目标模型声明的上限：调用方没给 maxTokens 或目标没有上限时保持原值。
        let limit = route.model.max_tokens.unwrap_or(0);
        let max_tokens = match options.base.base.max_tokens {
            Some(value) if value != 0 && limit > 0 => Some(value.min(limit)),
            other => other,
        };
        let reasoning = thinking_level_of(route.thinking_level);
        let ModelsSimpleStreamOptions {
            base,
            transforms,
            signal_reason,
        } = options;
        let mut stream_base = base.base;
        // 跨 provider 路由时不能把原 provider 的凭据/请求头带过去：置空后由目标 provider
        // 重新解析 auth。同 provider 时保留调用方的显式覆盖。
        if route.model.provider != model.provider {
            stream_base.api_key = None;
            stream_base.headers = None;
            stream_base.env = None;
        }
        stream_base.max_tokens = max_tokens;
        let next = ModelsSimpleStreamOptions {
            base: SimpleStreamOptions {
                base: stream_base,
                tool_choice: base.tool_choice,
                reasoning,
                deferred: base.deferred,
                thinking_budgets: base.thinking_budgets,
            },
            transforms,
            signal_reason,
        };
        assert_chat_model(&typed_any_model(&route.model)).map_err(ai_error_from_models_error)?;
        let (provider, request_model, request_options) =
            self.prepare_simple_request(&route.model, next).await?;
        Ok(provider.stream_simple(&request_model, transcript, request_options))
    }

    /// `completeSimple(model, context, options?)`。
    ///
    /// 等价于 `stream_simple(...)` 后等待结果；虚拟模型路由错误同样以流内错误消息返回。
    pub async fn complete_simple(
        &self,
        model: &Model,
        context: &Context,
        options: ModelsSimpleStreamOptions,
    ) -> AssistantMessage {
        let stream = self.stream_simple(model, context, options);
        stream_result(stream, model).await
    }

    /// `streamDeferred(model, handle, options?)`。
    ///
    /// 惰性流；setup 阶段在 prepare 之后检查 provider 的 `has_fetch_deferred()`，不支持时
    /// 的流内错误消息与 provider 返回 `None` 时相同（`does not support deferred responses`）。
    /// `options.wait` 原样传给 `fetch_deferred`。
    pub fn stream_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: ModelsDeferredFetchOptions,
    ) -> AssistantMessageEventStream {
        let inner = Arc::clone(&self.inner);
        let model_for_setup = model.clone();
        let handle = handle.clone();
        let setup: LazySetup = Box::pin(async move {
            assert_chat_model(&typed_any_model(&model_for_setup))
                .map_err(ai_error_from_models_error)?;
            let runtime = ModelRuntime { inner };
            let ModelsDeferredFetchOptions {
                base,
                transforms,
                signal_reason,
            } = options;
            let prepared = runtime
                .prepare_request_core(
                    &model_for_setup,
                    base.base,
                    &transforms,
                    signal_reason.as_ref(),
                )
                .await?;
            if !prepared.provider.has_fetch_deferred() {
                return Err(models_error_ai(
                    TsModelsErrorCode::Provider,
                    &format!(
                        "Provider {} does not support deferred responses",
                        model_for_setup.provider
                    ),
                ));
            }
            prepared
                .provider
                .fetch_deferred(
                    &prepared.model,
                    &handle,
                    DeferredFetchOptions {
                        base: prepared.options,
                        wait: base.wait,
                    },
                )
                .ok_or_else(|| {
                    models_error_ai(
                        TsModelsErrorCode::Provider,
                        &format!(
                            "Provider {} does not support deferred responses",
                            model_for_setup.provider
                        ),
                    )
                })
        });
        lazy_stream(model, setup, None)
    }

    /// `fetchDeferred(model, handle, options?)`。
    ///
    /// `stream_deferred` 之后等待结果；流结束却没有结果时同样由 `stream_result` 兜底。
    pub async fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: ModelsDeferredFetchOptions,
    ) -> AssistantMessage {
        let stream = self.stream_deferred(model, handle, options);
        stream_result(stream, model).await
    }

    /// `cancelDeferred(model, handle, options?)`。
    ///
    /// 先做与流式接口相同的模型类型断言与请求准备，再要求 provider `has_cancel_deferred()`，
    /// 最后把准备好的选项交给 provider。与 `stream_deferred` 不同，这里是普通 `async`，
    /// 错误直接以 `Err(AiError)` 返回。
    pub async fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: ModelsDeferredCancelOptions,
    ) -> Result<(), AiError> {
        assert_chat_model(&typed_any_model(model)).map_err(ai_error_from_models_error)?;
        let ModelsDeferredCancelOptions {
            base,
            transforms,
            signal_reason,
        } = options;
        let prepared = self
            .prepare_request_core(model, base, &transforms, signal_reason.as_ref())
            .await?;
        if !prepared.provider.has_cancel_deferred() {
            return Err(models_error_ai(
                TsModelsErrorCode::Provider,
                &format!(
                    "Provider {} does not support deferred responses",
                    model.provider
                ),
            ));
        }
        prepared
            .provider
            .cancel_deferred(&prepared.model, handle, prepared.options)
            .await
    }

    /// `generateImages(model, context, options?)`：**永不 reject**（错误以结果返回）。
    ///
    /// 依次做图片模型断言、请求准备、provider 能力检查（`has_generate_images`），然后转交
    /// provider。任何失败都经 `image_error_result` 变成带 `error` 信息的 `AssistantImages`；
    /// 结果的“是否中止”标志来自 `options.base.signal` 的取消状态。
    pub async fn generate_images(
        &self,
        model: &Model,
        context: &ImagesContext,
        options: ModelsImagesOptions,
    ) -> AssistantImages {
        let signal = options.base.signal.clone();
        let result: Result<AssistantImages, AiError> = async {
            assert_image_model(&typed_any_model(model)).map_err(ai_error_from_models_error)?;
            let ModelsImagesOptions {
                base,
                transforms,
                signal_reason,
            } = options;
            let prepared = self
                .prepare_request_core(model, base, &transforms, signal_reason.as_ref())
                .await?;
            if !prepared.provider.has_generate_images() {
                return Err(models_error_ai(
                    TsModelsErrorCode::Provider,
                    &format!(
                        "Provider {} does not support image generation",
                        model.provider
                    ),
                ));
            }
            Ok(prepared
                .provider
                .generate_images(&prepared.model, context, Some(&prepared.options))
                .await)
        }
        .await;
        match result {
            Ok(images) => images,
            Err(error) => {
                let aborted = signal
                    .as_ref()
                    .is_some_and(tokio_util::sync::CancellationToken::is_cancelled);
                image_error_result(model, &to_thrown_value(&error), aborted)
            }
        }
    }

    /// `classify(model, context, options?)`：**永不 reject**。
    ///
    /// 额外要求 `assert_classifier_input_supported`（输入必须能被分类器接受）与 provider 的
    /// `has_classify`。失败经 `classifier_error_result` 返回；中止标志同样取自
    /// `options.base.signal`。
    pub async fn classify(
        &self,
        model: &Model,
        context: &ClassifierContext,
        options: ModelsClassifierOptions,
    ) -> ClassifierResult {
        let signal = options.base.signal.clone();
        let result: Result<ClassifierResult, AiError> = async {
            assert_classifier_model(&typed_any_model(model)).map_err(ai_error_from_models_error)?;
            assert_classifier_input_supported(model, context)
                .map_err(ai_error_from_models_error)?;
            let ModelsClassifierOptions {
                base,
                transforms,
                signal_reason,
            } = options;
            let prepared = self
                .prepare_request_core(model, base, &transforms, signal_reason.as_ref())
                .await?;
            if !prepared.provider.has_classify() {
                return Err(models_error_ai(
                    TsModelsErrorCode::Provider,
                    &format!(
                        "Provider {} does not support classification",
                        model.provider
                    ),
                ));
            }
            Ok(prepared
                .provider
                .classify(&prepared.model, context, Some(&prepared.options))
                .await)
        }
        .await;
        match result {
            Ok(result) => result,
            Err(error) => {
                let aborted = signal
                    .as_ref()
                    .is_some_and(tokio_util::sync::CancellationToken::is_cancelled);
                classifier_error_result(model, &to_thrown_value(&error), aborted)
            }
        }
    }

    /// `login(providerId, type, interaction, options?)`。
    ///
    /// `interaction` 传 `&dyn AuthInteraction`/`&T` 时任务在调用方 future 内执行（借用无法
    /// `spawn`）；传 `Arc<dyn AuthInteraction>`/`Arc<T>` 时凭据操作被分离，调用方 future 被
    /// 丢弃（`timeout`/`select!`）后任务仍会跑完并提交凭证（TS 的语义）。
    ///
    /// 登录本身由底层 `Models::login` 完成并写入凭证存储；随后调用
    /// `synchronize_credential_state`。同步失败时返回
    /// [`ModelRuntimeError::CredentialSync`]（凭证已持久化，`cause` 是底层失败），登录失败
    /// 则是 [`ModelRuntimeError::Auth`]。
    pub async fn login<I>(
        &self,
        provider_id: &str,
        auth_type: AuthType,
        interaction: I,
        options: LoginOptions,
    ) -> Result<Credential, ModelRuntimeError>
    where
        I: IntoAuthInteraction,
    {
        let signal = operation_signal(Some(interaction.as_auth_interaction().signal().clone()));
        if let Some(interaction) = interaction.owned_auth_interaction() {
            let runtime = self.clone();
            let provider_id_owned = provider_id.to_string();
            let queue_id = provider_id_owned.clone();
            let task_signal = signal.clone();
            return self
                .enqueue_credential_operation(&queue_id, &signal, move || async move {
                    let credential = runtime
                        .inner
                        .models
                        .login(&provider_id_owned, auth_type, interaction.as_ref(), options)
                        .await
                        .map_err(ModelRuntimeError::Auth)?;
                    runtime
                        .synchronize_credential_state(
                            &provider_id_owned,
                            CredentialSynchronizationOperation::Login,
                            Some(credential.clone()),
                            &task_signal,
                        )
                        .await
                        .map_err(ModelRuntimeError::CredentialSync)?;
                    Ok(credential)
                })
                .await;
        }
        let provider_id_owned = provider_id.to_string();
        self.enqueue_credential_operation_inline(&provider_id_owned, &signal, || async {
            let credential = self
                .inner
                .models
                .login(
                    &provider_id_owned,
                    auth_type,
                    interaction.as_auth_interaction(),
                    options.clone(),
                )
                .await
                .map_err(ModelRuntimeError::Auth)?;
            self.synchronize_credential_state(
                &provider_id_owned,
                CredentialSynchronizationOperation::Login,
                Some(credential.clone()),
                &signal,
            )
            .await
            .map_err(ModelRuntimeError::CredentialSync)?;
            Ok(credential)
        })
        .await
    }

    /// `logout(providerId, options?)`。
    ///
    /// 在串行队列里先调用底层 `Models::logout` 删除凭证，再做局部同步；同步失败返回
    /// [`ModelRuntimeError::CredentialSync`]（凭证已删除，本地快照未跟上）。
    pub async fn logout(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<(), ModelRuntimeError> {
        let signal = operation_signal(options.signal);
        let provider_id_owned = provider_id.to_string();
        let queue_id = provider_id_owned.clone();
        let runtime = self.clone();
        let task_signal = signal.clone();
        self.enqueue_credential_operation(&queue_id, &signal, move || async move {
            runtime
                .inner
                .models
                .logout(
                    &provider_id_owned,
                    AuthOperationOptions {
                        signal: Some(task_signal.clone()),
                    },
                )
                .await
                .map_err(ModelRuntimeError::Auth)?;
            runtime
                .synchronize_credential_state(
                    &provider_id_owned,
                    CredentialSynchronizationOperation::Logout,
                    None,
                    &task_signal,
                )
                .await
                .map_err(ModelRuntimeError::CredentialSync)
        })
        .await
    }

    // -----------------------------------------------------------------------
    // refresh
    // -----------------------------------------------------------------------

    /// `refresh(options?)`：每次重读 models.json，重组 provider 并刷新可用性。
    ///
    /// 步骤：
    /// 1. 重读 models.json（`ModelConfig::load` 不返回 `Result`，IO/解析错误记录在配置对象
    ///    里，之后由 [`ModelRuntime::get_error`] 暴露）；
    /// 2. 按新配置重新配置 radius provider；
    /// 3. `options.providers` 为 `Some` 时只重组这些 id（先按插入序去重），否则全量
    ///    `rebuild_providers`；
    /// 4. 调底层 `Models::refresh`（`allow_network` 缺省取运行时的网络开关）；
    /// 5. 局部刷新：对每个 provider 做可用性刷新，把非中止错误并入返回的 `errors`；全量：
    ///    忽略可用性刷新错误（由全局错误字段记录）。
    ///
    /// 注意：配置对象被整体替换，但局部路径只有列出的 provider 会重新合成/登记，其余
    /// provider 保持旧组合直到下一次全量重建。底层 `Models::refresh` 返回 `Err` 时，前面的
    /// 配置替换与重组**已经生效**。
    pub async fn refresh(
        &self,
        options: ModelsRefreshOptions,
    ) -> Result<ModelsRefreshResult, AiError> {
        // 整体替换配置：即使后续刷新失败，内存里的配置也已经是新读到的这一份。
        let config = ModelConfig::load(self.inner.models_path.as_deref()).await;
        *lock(&self.inner.config) = config;
        self.configure_radius_providers();
        let providers = options
            .providers
            .as_ref()
            .map(|providers| unique_ids(providers));
        // 有 provider 列表时只重组这些 id；否则清空后全量重建。
        match &providers {
            Some(providers) => {
                for provider_id in providers {
                    self.recompose_provider(provider_id);
                }
                self.update_model_snapshot();
            }
            None => self.rebuild_providers(),
        }
        // 网络开关：调用方显式值优先，否则用运行时构建时的 KIANA_OFFLINE 结论。
        let refresh_options = ModelsRefreshOptions {
            allow_network: Some(
                options
                    .allow_network
                    .unwrap_or(self.inner.model_network_enabled),
            ),
            providers: options.providers.clone(),
            force: options.force,
            signal: options.signal.clone(),
        };
        let result = self.inner.models.refresh(refresh_options).await?;
        let mut errors = result.errors.clone();
        self.update_model_snapshot();
        match &providers {
            Some(providers) => {
                let signal = operation_signal(options.signal.clone());
                // TS 在 catch 时检查 `!options.signal?.aborted`（而不是在进入 refresh 前预先
                // 取值）：若调用方在 provider 可用性阶段中止，错误被**抑制**。
                let caller_signal = options.signal.clone();
                let tasks = providers.iter().map(|provider_id| {
                    let provider_id = provider_id.clone();
                    let signal = signal.clone();
                    let caller_signal = caller_signal.clone();
                    async move {
                        match self
                            .refresh_provider_availability(&provider_id, &signal)
                            .await
                        {
                            Ok(()) => None,
                            Err(error) => {
                                let aborted =
                                    caller_signal.as_ref().is_some_and(AbortScope::is_aborted);
                                (!aborted).then(|| (provider_id, error.to_ai_error()))
                            }
                        }
                    }
                });
                for entry in futures::future::join_all(tasks).await.into_iter().flatten() {
                    errors.insert(entry.0, entry.1);
                }
            }
            None => {
                // 可用性错误由最新一轮记录；刷新出的模型仍然可用。
                let _ = self
                    .queue_availability_refresh(options.signal.clone())
                    .await;
            }
        }
        let aborted = result.aborted || options.signal.as_ref().is_some_and(AbortScope::is_aborted);
        Ok(ModelsRefreshResult { aborted, errors })
    }

    // -----------------------------------------------------------------------
    // 注册 / 注销
    // -----------------------------------------------------------------------

    /// `registerNativeProvider(provider)`。
    ///
    /// - id `trim()` 后为空 → `Message` 错误，状态不变；
    /// - 同 id 的扩展注册配置会被移除（该 id 之后只保留原生实例 + models.json 覆盖）；
    /// - 登记后立即重组、更新快照，并按 provider 声明的 auth 能力（有 oauth 且无 api_key
    ///   记为 OAuth，否则 ApiKey）做临时配置标记；
    /// - 最后触发一次后台禁网刷新（返回值被丢弃）。
    pub fn register_native_provider(
        &self,
        provider: Arc<dyn Provider>,
    ) -> Result<(), ModelRuntimeError> {
        if !provider_id_trimmed(&provider) {
            return Err(ModelRuntimeError::Message(
                "Provider id must not be empty.".to_string(),
            ));
        }
        let provider_id = provider.id().to_string();
        lock(&self.inner.extension_providers).shift_remove(&provider_id);
        lock(&self.inner.native_extension_providers)
            .insert(provider_id.clone(), Arc::clone(&provider));
        self.recompose_provider(&provider_id);
        self.update_model_snapshot();
        let auth = provider.auth();
        let auth_type = if auth.oauth.is_some() && auth.api_key.is_none() {
            AuthType::OAuth
        } else {
            AuthType::ApiKey
        };
        let config = lock(&self.inner.config).get_provider(&provider_id);
        self.mark_provisionally_configured(
            &provider_id,
            configured_request_auth_status(config.as_deref(), None),
            auth_type,
        );
        self.spawn_background_refresh();
        Ok(())
    }

    /// `markProvisionallyConfigured(providerId, configuredStatus, type)`。
    ///
    /// 只有“快照里已有持久凭证”或“配置状态为已配置”时才把 provider 记入
    /// `configured_providers`；auth 检查缺失时补一条 `source: "configured provider"` 的占位，
    /// 随后按新集合重算 `available`。这是注册后、真实可用性刷新落地前的过渡状态。
    fn mark_provisionally_configured(
        &self,
        provider_id: &str,
        configured_status: Option<AuthStatus>,
        auth_type: AuthType,
    ) {
        let mut snapshot = lock(&self.inner.snapshot);
        if !snapshot.stored_providers.contains(provider_id)
            && !configured_status.is_some_and(|status| status.configured)
        {
            return;
        }
        let mut configured_providers = snapshot.configured_providers.clone();
        configured_providers.insert(provider_id.to_string());
        let mut auth = snapshot.auth.clone();
        if !auth.contains_key(provider_id) {
            auth.insert(
                provider_id.to_string(),
                AuthCheck {
                    source: Some("configured provider".to_string()),
                    r#type: auth_type,
                },
            );
        }
        let available = snapshot
            .all
            .iter()
            .filter(|model| configured_providers.contains(&model.provider))
            .cloned()
            .collect();
        snapshot.auth = auth;
        snapshot.configured_providers = configured_providers;
        snapshot.available = available;
    }

    /// `registerProvider(providerId, config)`：**先**校验，失败时不改动已存配置。
    ///
    /// 校验用 `validate_extension_provider`（以内置 provider 与 models.json 配置为参考）。
    /// 成功后：
    /// - 移除同 id 的原生 provider，使该 id 的 base 回退到内置实现（若存在）再叠加扩展配置；
    /// - 新旧配置按“已定义键覆盖”合并（见 `merge_provider_config`）；`images`/`classifiers`
    ///   显式空表也视为已定义，可用来清除旧实现；
    /// - 重组、更新快照、临时配置标记，再触发后台禁网刷新。
    pub fn register_provider(
        &self,
        provider_id: &str,
        config: ProviderConfigInput,
    ) -> Result<(), ModelRuntimeError> {
        let base = lock(&self.inner.builtins).get(provider_id).cloned();
        let models_config = lock(&self.inner.config).get_provider(provider_id);
        validate_extension_provider(
            provider_id,
            base.as_ref(),
            models_config.as_deref(),
            &config,
        )
        .map_err(|error| ModelRuntimeError::Message(error.message))?;
        lock(&self.inner.native_extension_providers).shift_remove(provider_id);
        // 重注册把已定义的键覆盖到旧注册上；未定义的键保留（legacy ModelRegistry 契约）。
        let previous = lock(&self.inner.extension_providers)
            .get(provider_id)
            .cloned()
            .unwrap_or_default();
        let effective = merge_provider_config(previous, &config);
        lock(&self.inner.extension_providers).insert(provider_id.to_string(), effective.clone());
        self.recompose_provider(provider_id);
        self.update_model_snapshot();
        // TS 的 `effective.oauth && !effective.apiKey`：空串 apiKey 是 falsy。
        let auth_type = if effective.oauth.is_some()
            && effective.api_key.as_deref().is_none_or(str::is_empty)
        {
            AuthType::OAuth
        } else {
            AuthType::ApiKey
        };
        let models_config = lock(&self.inner.config).get_provider(provider_id);
        self.mark_provisionally_configured(
            provider_id,
            configured_request_auth_status(models_config.as_deref(), Some(&effective)),
            auth_type,
        );
        self.spawn_background_refresh();
        Ok(())
    }

    /// `unregisterProvider(providerId)`。
    ///
    /// 同时移除扩展注册与原生 provider，然后重组（可能回退到内置 provider，或彻底删除该
    /// id）、更新快照并触发后台禁网刷新。
    pub fn unregister_provider(&self, provider_id: &str) {
        lock(&self.inner.extension_providers).shift_remove(provider_id);
        lock(&self.inner.native_extension_providers).shift_remove(provider_id);
        self.recompose_provider(provider_id);
        self.update_model_snapshot();
        self.spawn_background_refresh();
    }

    /// `registerVirtualModel(definition)`。
    ///
    /// 校验：
    /// - provider id 与 model id `trim()` 后都非空；
    /// - 同 id 已存在且是**物理**模型 → 冲突错误（同名虚拟模型允许覆盖重注册）。
    ///
    /// 成功后用 `create_virtual_model` 生成目录条目，连同路由回调存入注册表；若该 provider
    /// 重组后没有可用的物理/合成 provider（`recompose_provider` 返回 `None`）且快照未视为
    /// 已配置，则立即标记为已配置（`source: "virtual"`，纯虚拟 provider 不需要凭证）；最后
    /// 更新快照并触发后台禁网刷新。注册本身不会调用路由回调。
    pub fn register_virtual_model(
        &self,
        definition: VirtualModelDefinition,
    ) -> Result<(), ModelRuntimeError> {
        let provider_id = definition.provider.clone();
        let model_id = definition.id.clone();
        if provider_id.trim().is_empty() || model_id.trim().is_empty() {
            return Err(ModelRuntimeError::Message(
                "Virtual model provider and id must not be empty.".to_string(),
            ));
        }
        if let Some(existing) = self.inner.models.get_model(&provider_id, &model_id)
            && !is_virtual_model(&existing)
        {
            return Err(ModelRuntimeError::Message(format!(
                "Virtual model {provider_id}/{model_id} conflicts with a physical model."
            )));
        }
        let model = create_virtual_model(&definition);
        let route = definition.route.clone();
        {
            let mut virtual_models = lock(&self.inner.virtual_models);
            virtual_models
                .entry(provider_id.clone())
                .or_default()
                .insert(model_id.clone(), RegisteredVirtualModel { model, route });
        }
        if self.recompose_provider(&provider_id).is_none()
            && !lock(&self.inner.snapshot)
                .configured_providers
                .contains(&provider_id)
        {
            // 纯虚拟 provider 不需要凭据：立即视为已配置（会话恢复在刷新落地前检查）。
            let mut snapshot = lock(&self.inner.snapshot);
            let mut auth = snapshot.auth.clone();
            auth.insert(
                provider_id.clone(),
                AuthCheck {
                    source: Some("virtual".to_string()),
                    r#type: AuthType::ApiKey,
                },
            );
            let mut configured_providers = snapshot.configured_providers.clone();
            configured_providers.insert(provider_id.clone());
            snapshot.auth = auth;
            snapshot.configured_providers = configured_providers;
        }
        self.update_model_snapshot();
        self.spawn_background_refresh();
        Ok(())
    }

    /// `unregisterVirtualModel(providerId, id)`。
    ///
    /// 未知 provider 或未知 model 时直接返回（不触发刷新）；移除最后一个虚拟模型时会同时
    /// 删除该 provider 的注册表条目，然后重组、更新快照并触发后台禁网刷新。
    pub fn unregister_virtual_model(&self, provider_id: &str, id: &str) {
        {
            let mut virtual_models = lock(&self.inner.virtual_models);
            let Some(models) = virtual_models.get_mut(provider_id) else {
                return;
            };
            if models.shift_remove(id).is_none() {
                return;
            }
            if models.is_empty() {
                virtual_models.shift_remove(provider_id);
            }
        }
        self.recompose_provider(provider_id);
        self.update_model_snapshot();
        self.spawn_background_refresh();
    }

    /// `void this.refresh({ allowNetwork: false })`：后台触发，不 await。
    ///
    /// TS 的 `void` 刷新与调用方的执行环境无关；Rust 在有 tokio 运行时时 `spawn`，在纯
    /// `std::thread`（`Handle::try_current()` 为 `Err`）里用一个临时 current-thread 运行时
    /// 把刷新跑完，而不是静默跳过。
    ///
    /// `refresh` 的返回值被丢弃：后台失败不会反馈给触发注册的调用方。从触发到刷新落地之间
    /// 存在窗口，期间同步查询（如 `has_configured_auth`）看到的仍是旧快照；需要确定性时请
    /// 显式 `await refresh(...)`。
    fn spawn_background_refresh(&self) {
        let runtime = self.clone();
        let refresh = || async move {
            let _ = runtime
                .refresh(ModelsRefreshOptions {
                    allow_network: Some(false),
                    ..Default::default()
                })
                .await;
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(refresh());
            }
            Err(_) => {
                std::thread::spawn(move || {
                    if let Ok(handle) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        handle.block_on(refresh());
                    }
                });
            }
        }
    }

    // -----------------------------------------------------------------------
    // 虚拟模型路由
    // -----------------------------------------------------------------------

    /// 询问虚拟模型的路由器，返回物理模型与裁剪后的 thinking level。
    ///
    /// TS: `model-runtime.ts#resolveModel`
    ///
    /// 流程：
    /// 1. 从注册表取该 provider/model 的条目与路由回调；未注册 → `is not registered.`。
    /// 2. 用 `find_latest_response` 找最近一条非 `error`/`aborted` 的 assistant 消息，投影成
    ///    `previous`（物理模型 + 当时的 thinking level）；`options.failed` 若仍能解析成物理
    ///    模型，则投影成 `failed` 交给路由器。投影不到（模型已不在目录/是虚拟模型）时为
    ///    `None`，不会报错。
    /// 3. 没有路由回调 → `has no route.`。
    /// 4. 调用回调（`options.signal` 可取消），回调错误转成 `Message`。
    /// 5. 目标必须能在目录里解析为**物理**模型，且其 provider 已有配置（`has_configured_auth`
    ///    读快照），否则分别报 `which is not a physical model.` / `which has no credentials.`。
    /// 6. 返回的 thinking level 经 `clamp_thinking_level` 钳制到目标支持的档位（同档优先，
    ///    其次尝试更高档，最后才降档）。
    pub async fn resolve_model(
        &self,
        model: &Model,
        messages: &[Message],
        options: ResolveModelOptions,
    ) -> Result<ModelRoute, ModelRuntimeError> {
        // 注册表里同时存目录条目与路由回调；条目本身用于对外列表，回调用于本次路由。
        let name = format!("Virtual model {}/{}", model.provider, model.id);
        let virtual_model = lock(&self.inner.virtual_models)
            .get(&model.provider)
            .and_then(|models| models.get(&model.id))
            .map(|entry| (entry.model.clone(), entry.route.clone()));
        let Some((_, route)) = virtual_model else {
            return Err(ModelRuntimeError::Message(format!(
                "{name} is not registered."
            )));
        };
        let agent_messages: Vec<kiana_agent_core::AgentMessage> = messages
            .iter()
            .cloned()
            .map(kiana_agent_core::AgentMessage::Standard)
            .collect();
        // `previous`/`failed` 都只是“能投影就提供”的辅助信息：解析失败时为 None。
        let latest = find_latest_response(&agent_messages);
        let previous = latest.as_ref().and_then(|latest| {
            self.get_physical_model(&latest.provider, &latest.model)
                .map(|previous_model| RouteModelRef {
                    model: previous_model,
                    thinking_level: latest.thinking_level,
                })
        });
        let failed = options.failed.as_ref().and_then(|failed| {
            self.get_physical_model(&failed.provider, &failed.model)
                .map(|failed_model| RouteFailure {
                    model: failed_model,
                    thinking_level: failed.thinking_level,
                    message: failed.clone(),
                })
        });
        let Some(route) = route else {
            return Err(ModelRuntimeError::Message(format!("{name} has no route.")));
        };
        let route = route(ModelRouteRequest {
            model: model.clone(),
            thinking_level: options.thinking_level,
            reason: options.reason,
            previous,
            failed,
            state: options.state.clone(),
            messages: messages.to_vec(),
            signal: options.signal.clone(),
        })
        .await
        .map_err(|error: VirtualModelRouteError| ModelRuntimeError::Message(error.message))?;
        let target = self.get_physical_model(&route.model.provider, &route.model.id);
        let routed = format!(
            "{name} routed to {}/{}",
            route.model.provider, route.model.id
        );
        let Some(target) = target else {
            return Err(ModelRuntimeError::Message(format!(
                "{routed}, which is not a physical model."
            )));
        };
        if !self.has_configured_auth(&target.provider) {
            return Err(ModelRuntimeError::Message(format!(
                "{routed}, which has no credentials."
            )));
        }
        Ok(ModelRoute {
            thinking_level: kiana_ai::models::clamp_thinking_level(&target, route.thinking_level),
            model: target,
            state: route.state,
        })
    }

    /// `getPhysicalModel(providerId, modelId)`。
    ///
    /// 与 [`ModelRuntime::get_model`] 的区别：`is_virtual_model` 为真的条目一律返回 `None`。
    pub fn get_physical_model(&self, provider_id: &str, model_id: &str) -> Option<Model> {
        self.inner
            .models
            .get_model(provider_id, model_id)
            .filter(|model| !is_virtual_model(model))
    }
}

/// `merge_extension_config`：TS 的 `{...previous}` + 已定义键覆盖。
///
/// 逐字段判断“本次是否定义”：`Option` 字段以 `Some` 为准；`images`/`classifiers` 用
/// `*_defined` 标记或非空表表示已定义——因此显式空表也能覆盖掉旧实现，而缺省（未定义）
/// 则保留旧值。
fn merge_provider_config(
    mut previous: ProviderConfigInput,
    config: &ProviderConfigInput,
) -> ProviderConfigInput {
    if config.name.is_some() {
        previous.name = config.name.clone();
    }
    if config.base_url.is_some() {
        previous.base_url = config.base_url.clone();
    }
    if config.api_key.is_some() {
        previous.api_key = config.api_key.clone();
    }
    if config.api.is_some() {
        previous.api = config.api.clone();
    }
    if config.stream_simple.is_some() {
        previous.stream_simple = config.stream_simple.clone();
    }
    if config.images_defined || !config.images.is_empty() {
        // TS 的 `value !== undefined`：显式空表也覆盖（用于清除旧实现）。
        previous.images = config.images.clone();
        previous.images_defined = true;
    }
    if config.classifiers_defined || !config.classifiers.is_empty() {
        previous.classifiers = config.classifiers.clone();
        previous.classifiers_defined = true;
    }
    if config.headers.is_some() {
        previous.headers = config.headers.clone();
    }
    if config.auth_header.is_some() {
        previous.auth_header = config.auth_header;
    }
    if config.oauth.is_some() {
        previous.oauth = config.oauth.clone();
    }
    if config.models.is_some() {
        previous.models = config.models.clone();
    }
    if config.refresh_models.is_some() {
        previous.refresh_models = config.refresh_models.clone();
    }
    previous
}

/// `ModelRuntimeError` → [`AiError`]（保留 name/message）。
///
/// `Auth` 变体若本身是中止错误，则 kind 记为 `Aborted`；其余一律 `Other`。该转换把虚拟
/// 模型路由失败等错误转成 [`AiError`]，由 `lazy_stream` 合成为流内错误消息（见
/// `stream_simple` 的 setup）；非流式路径也可复用。
fn error_to_ai(error: &ModelRuntimeError) -> AiError {
    let kind = match error {
        ModelRuntimeError::Auth(auth) if auth.is_aborted() => AiErrorKind::Aborted,
        _ => AiErrorKind::Other,
    };
    AiError::new(kind, error.message()).with_name(error.name())
}

/// `AiError` → `AuthError`，并恢复 `ModelsError.code`。
///
/// [`AuthError::from_ai_error`] 只带 `name`/`message`；而 `ai_error_from_models_error` /
/// `ai_error_from_auth_error` 会把 `ModelsError.code` 存进 `detail.code`。TS 抛出的
/// `Error` 对象保留 `code`，所以转回 `AuthError` 时从 detail 还原。
fn auth_error_from_ai(error: &AiError) -> AuthError {
    let mut auth = AuthError::from_ai_error(error);
    if let Some(code) = error
        .detail
        .as_ref()
        .and_then(|detail| detail.get("code"))
        .and_then(serde_json::Value::as_str)
        .and_then(kiana_ai::error::ModelsErrorCode::from_str)
    {
        auth.code = Some(code);
    }
    auth
}

/// `stream(...).result()` 的等价物：流没有结果时返回 setup 错误消息。
///
/// 只有 `result()` 返回 `Err`（流结束却没有提交结果）才兜底；正常的中止/错误已经以
/// assistant 消息的形式由流本身给出，不会走到这里。兜底消息的时间戳取调用时的当前时间。
async fn stream_result(stream: AssistantMessageEventStream, model: &Model) -> AssistantMessage {
    match stream.result().await {
        Ok(message) => message,
        Err(_) => create_setup_error_message(
            model,
            &AiError::new(
                AiErrorKind::Other,
                "assistant message event stream ended without a result",
            ),
            now_ms(),
        ),
    }
}

// ---------------------------------------------------------------------------
// Models trait 实现
// ---------------------------------------------------------------------------

// `Models` 是 kiana-ai 冻结的 trait：这里只做同名转发，保证经由 trait 与直接调用
// `ModelRuntime` 的行为一致。两处例外：
// - `get_auth` 需要按 `GetAuthTarget` 分派到 provider/model 两个版本；
// - trait 的 `login`/`logout` 返回 `AuthError`，因此用 `to_auth_error` 保留 name/cause。
// 另外 trait 的 `login` 只接受借用的 interaction，所以总是走内联（不可分离）路径。
#[async_trait]
impl Models for ModelRuntime {
    fn get_providers(&self) -> Vec<Arc<dyn Provider>> {
        ModelRuntime::get_providers(self)
    }

    fn get_provider(&self, id: &str) -> Option<Arc<dyn Provider>> {
        ModelRuntime::get_provider(self, id)
    }

    fn get_models(&self, provider: Option<&str>) -> Vec<Model> {
        ModelRuntime::get_models(self, provider)
    }

    fn get_model(&self, provider: &str, id: &str) -> Option<Model> {
        ModelRuntime::get_model(self, provider, id)
    }

    fn get_models_of_type(&self, model_type: ModelType, provider: Option<&str>) -> Vec<Model> {
        ModelRuntime::get_models_of_type(self, model_type, provider)
    }

    fn get_model_of_type(&self, model_type: ModelType, provider: &str, id: &str) -> Option<Model> {
        ModelRuntime::get_model_of_type(self, model_type, provider, id)
    }

    fn get_all_models(&self, provider: Option<&str>) -> Vec<AnyModel> {
        ModelRuntime::get_all_models(self, provider)
    }

    async fn refresh(&self, options: ModelsRefreshOptions) -> Result<ModelsRefreshResult, AiError> {
        ModelRuntime::refresh(self, options).await
    }

    async fn check_auth(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<Option<AuthCheck>, AuthError> {
        ModelRuntime::check_auth(self, provider_id, options).await
    }

    async fn get_available(
        &self,
        provider_id: Option<&str>,
        options: AuthOperationOptions,
    ) -> Result<Vec<Model>, AuthError> {
        ModelRuntime::get_available(self, provider_id, options).await
    }

    async fn get_available_of_type(
        &self,
        model_type: ModelType,
        provider_id: Option<&str>,
        options: AuthOperationOptions,
    ) -> Result<Vec<Model>, AuthError> {
        ModelRuntime::get_available_of_type(self, model_type, provider_id, options).await
    }

    async fn get_all_available(
        &self,
        provider_id: Option<&str>,
        options: AuthOperationOptions,
    ) -> Result<Vec<AnyModel>, AuthError> {
        ModelRuntime::get_all_available(self, provider_id, options).await
    }

    async fn get_auth(
        &self,
        target: GetAuthTarget<'_>,
        overrides: AuthResolutionOverrides,
    ) -> Result<Option<AuthResult>, AuthError> {
        self.get_auth_target(target, overrides).await
    }

    async fn login(
        &self,
        provider_id: &str,
        auth_type: AuthType,
        interaction: &dyn AuthInteraction,
        options: LoginOptions,
    ) -> Result<Credential, AuthError> {
        ModelRuntime::login(self, provider_id, auth_type, interaction, options)
            .await
            .map_err(|error| error.to_auth_error())
    }

    async fn logout(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<(), AuthError> {
        ModelRuntime::logout(self, provider_id, options)
            .await
            .map_err(|error| error.to_auth_error())
    }

    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: ModelsStreamOptions,
    ) -> AssistantMessageEventStream {
        ModelRuntime::stream(self, model, context, options)
    }

    async fn complete(
        &self,
        model: &Model,
        context: &Context,
        options: ModelsStreamOptions,
    ) -> AssistantMessage {
        ModelRuntime::complete(self, model, context, options).await
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: ModelsSimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        ModelRuntime::stream_simple(self, model, context, options)
    }

    async fn complete_simple(
        &self,
        model: &Model,
        context: &Context,
        options: ModelsSimpleStreamOptions,
    ) -> AssistantMessage {
        ModelRuntime::complete_simple(self, model, context, options).await
    }

    fn stream_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: ModelsDeferredFetchOptions,
    ) -> AssistantMessageEventStream {
        ModelRuntime::stream_deferred(self, model, handle, options)
    }

    async fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: ModelsDeferredFetchOptions,
    ) -> AssistantMessage {
        ModelRuntime::fetch_deferred(self, model, handle, options).await
    }

    async fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: ModelsDeferredCancelOptions,
    ) -> Result<(), AiError> {
        ModelRuntime::cancel_deferred(self, model, handle, options).await
    }

    async fn generate_images(
        &self,
        model: &Model,
        context: &ImagesContext,
        options: ModelsImagesOptions,
    ) -> AssistantImages {
        ModelRuntime::generate_images(self, model, context, options).await
    }

    async fn classify(
        &self,
        model: &Model,
        context: &ClassifierContext,
        options: ModelsClassifierOptions,
    ) -> ClassifierResult {
        ModelRuntime::classify(self, model, context, options).await
    }
}
