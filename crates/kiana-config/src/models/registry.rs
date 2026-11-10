//! `ModelRegistry` 门面。
//!
//! TS: `packages/coding-agent/src/core/model-registry.ts`（244 行）。规范：
//! `spec/ca-config-models.md` §2.22（含 §2.22.1-§2.22.2）。
//!
//! 暴露给扩展的**同步**兼容门面；coding-agent 内部直接用 [`ModelRuntime`]。读方法同步返回
//! `ModelRuntime` 的最后快照。
//!
//! 这里的“同步”指读接口：刷新、auth 解析、流、分类、图像等需要 IO 的操作仍然是 `async`。
//!
//! # 为什么需要这一层
//! TS 的扩展 API 直接暴露 `ModelRegistry` 对象，扩展与 JS 桥只依赖这组固定的方法名。
//! Rust 侧把「实现」与「对外门面」拆开：[`ModelRuntime`] 负责合成、快照与刷新；本类型只
//! 持有 `Arc<ModelRuntime>` 并把调用转发过去。本文件唯一做错误文案翻译的方法是
//! [`ModelRegistry::get_api_key_and_headers`]：它把内部 auth 错误折叠成 TS 的
//! `{ ok: false, error }` 形状，并对一条特定内部错误做逐字替换；注册类方法只额外做
//! 参数校验。
//!
//! # 调用方
//! - `kiana-extensions` 的 `runner/context.rs`：同步方法直接桥接到 JS `ctx.modelRegistry`，
//!   异步方法走 `__kiana.promise`；[`ResolvedRequestAuth`] 被序列化成
//!   `{ ok: true, apiKey?, headers?, baseUrl?, env? }` 或 `{ ok: false, error }`。
//! - `kiana-session` 的 `AgentSession`：构造一个交给 `ExtensionRunner`。
//! - `kiana-tools` 的 `placeholder_model_registry`：`ctx` 缺失时提供一个不联网、不读
//!   `models.json`、关闭初始刷新的进程级占位实例。
//! - `kiana-coding-agent` 的 `sdk_exports` 再导出本类型。
//!
//! # 状态与并发
//! 本类型自身不持有可变状态：`Clone` 只复制 `Arc`，所有克隆共享同一个 `ModelRuntime` 及其
//! 快照。同步查询只读当前快照、不做 IO；[`ModelRegistry::refresh`] 是唯一**由调用方直接
//! 发起**的重读 `models.json` 并（在允许时）联网的方法；`register_provider` /
//! `unregister_provider` 等注册类方法会在后台触发一次禁网刷新（同样重读 `models.json`）。
//! 其余异步方法（auth 解析、请求准备、流、分类、图像）每次调用都重新走一遍下层流程。
//!
//! # 失败模式
//! - 同步查询不会失败：未命中返回 `None` 或空列表。
//! - [`ModelRegistry::get_api_key_and_headers`] 永远返回 [`ResolvedRequestAuth`]，错误放在
//!   `Err` 变体；[`ModelRegistry::get_api_key_for_provider`] 把所有错误折叠为 `None`。
//! - [`ModelRegistry::classify`]、[`ModelRegistry::generate_images`] 与
//!   [`ModelRegistry::complete`] 的返回类型不是 `Result`：失败编码在返回结果里（与 TS 的
//!   “不 reject”一致），调用方不能靠 `?` 感知错误。
//! - 其余返回 `Result` 的方法原样传播 [`ModelRuntime`] 的错误；错误文案与 TS 的对齐由下层
//!   负责，本门面不翻译（除上面那一处 auth 文案）。

// 同 `runtime.rs`：冻结的 `AiError`/`ModelRuntimeError` 让其 Err 变体超过 clippy 阈值；
// 返回类型由 `Models`/TS 的接口固定，不装箱。
#![allow(clippy::result_large_err)]

use std::sync::Arc;

use kiana_ai::auth::types::{AuthError, AuthOperationOptions, AuthResult};
use kiana_ai::error::AiError;
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::models::{
    ModelsClassifierOptions, ModelsImagesOptions, ModelsRefreshOptions, ModelsRefreshResult,
    ModelsSimpleStreamOptions, ModelsStreamOptions, Provider,
};
use kiana_ai::types::{
    AssistantImages, AssistantMessage, ClassifierContext, ClassifierResult, Context, ImagesContext,
    Model, ModelType, ProviderEnv, ProviderHeaders,
};
use kiana_session_core::virtual_models::VirtualModelDefinition;

use crate::models::composer::AuthStatus;
use crate::models::runtime::{ModelRuntime, ModelRuntimeError};

// TS 的 `model-registry.ts` 还再导出了 `ProviderConfigInput` 与 `clearApiKeyCache`；Rust
// 保留同一入口，扩展侧只需要依赖 `models::registry` 一个模块。两者的实现在 `composer`。
pub use crate::models::composer::{ProviderConfigInput, clear_api_key_cache};

/// `ResolvedRequestAuth`：`{ ok: true, ... } | { ok: false, error }`。
///
/// TS: `model-registry.ts#ResolvedRequestAuth`
///
/// Rust 用枚举表达这个可辨识联合：成功分支携带 [`ResolvedAuth`] 的字段，失败分支只带
/// 可直接展示的文案。本类型只由 [`ModelRegistry::get_api_key_and_headers`] 产生，构造过程
/// 不会失败：底层所有 `Result`/`Option` 都已被折叠进两个变体，调用方不需要再处理错误。
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedRequestAuth {
    /// `{ ok: true }`。
    ///
    /// `ok` 字段本身由枚举变体表达，[`ResolvedAuth`] 不重复存它。
    Ok(ResolvedAuth),
    /// `{ ok: false, error }`。
    Err {
        /// 展示给调用方的错误文案。
        ///
        /// 若底层错误恰为 `authHeader requires a resolved API key`，这里已被替换成
        /// `No API key found for "<provider>"`；其余错误保留底层文本（可能是
        /// `error.cause` 的 message，见 [`ModelRegistry::get_api_key_and_headers`]）。
        error: String,
    },
}

impl ResolvedRequestAuth {
    /// `auth.ok`。
    pub fn is_ok(&self) -> bool {
        matches!(self, ResolvedRequestAuth::Ok(_))
    }

    /// `auth.error`（仅失败分支）。
    pub fn error(&self) -> Option<&str> {
        match self {
            ResolvedRequestAuth::Ok(_) => None,
            ResolvedRequestAuth::Err { error } => Some(error),
        }
    }
}

/// `{ ok: true }` 分支的字段。
///
/// TS: `model-registry.ts#ResolvedRequestAuth` 的 `ok: true` 变体。
///
/// 每个字段都是可选的“覆盖值”：`None` 表示本次解析没有提供该覆盖，调用方应保留自己的
/// 默认值。`base_url` 在构造时已过滤掉空串（TS 的 `baseUrl ? {baseUrl} : {}`），因此
/// 不会出现 `Some("")` 与 `None` 表达同一含义的歧义。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolvedAuth {
    /// `apiKey`。
    pub api_key: Option<String>,
    /// `headers`。
    pub headers: Option<ProviderHeaders>,
    /// `baseUrl`（仅在 auth 覆盖时出现）。
    pub base_url: Option<String>,
    /// `env`。
    pub env: Option<ProviderEnv>,
}

/// 同步兼容门面（暴露给扩展）。
///
/// TS: `model-registry.ts#ModelRegistry`
///
/// 只持有 `Arc<ModelRuntime>`：`Clone` 复制的是这个 `Arc`，不复制快照或配置，因此所有
/// 克隆观察同一份运行时状态。除 [`ModelRegistry::refresh`] 与 `register_*` / `unregister_*`
/// 方法外，其余方法要么读取当前快照，要么执行一次性的 auth/请求操作；注册/注销方法会重组
/// provider（重建该 id 的合成链）并触发一次后台禁网刷新。
#[derive(Clone)]
pub struct ModelRegistry {
    runtime: Arc<ModelRuntime>,
}

impl std::fmt::Debug for ModelRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModelRegistry")
            .finish_non_exhaustive()
    }
}

impl ModelRegistry {
    /// `new ModelRegistry(runtime)`。
    ///
    /// 只保存传入的 `Arc`，不克隆运行时、不触发刷新或 IO；调用方与门面之后共享同一份
    /// 状态。
    pub fn new(runtime: Arc<ModelRuntime>) -> Self {
        Self { runtime }
    }

    /// 底层 runtime（TS 的 `private readonly runtime`；Rust 侧测试与内部使用）。
    ///
    /// 返回引用，调用方通常再 `Arc::clone`。拿到它就可以使用门面未暴露的完整
    /// `ModelRuntime` API（如凭据操作、`get_all_models`），不再受本文件的兼容契约约束。
    pub fn runtime(&self) -> &Arc<ModelRuntime> {
        &self.runtime
    }

    /// `refresh(options?)`：异步重读 models.json；同步读之前先 await。
    ///
    /// `options` 决定是否联网、是否只刷新列出的 provider、是否绕过目录 TTL（`force`）以及
    /// 取消信号。返回的 [`ModelsRefreshResult`] 里 `aborted` 是调用方 signal 的结束状态，
    /// `errors` 收集未因中止被打断的 provider 级失败（不会中断其他 provider）。
    ///
    /// 注意错误分层：models.json 的读取/解析错误**不会**让本方法返回 `Err`，而是保存在
    /// 配置对象里由 [`ModelRegistry::get_error`] 暴露；只有底层 `Models::refresh` 自身的
    /// 失败才会以 `Err` 返回，且此时前面的配置替换与 provider 重组已经生效。
    pub async fn refresh(
        &self,
        options: ModelsRefreshOptions,
    ) -> Result<ModelsRefreshResult, AiError> {
        self.runtime.refresh(options).await
    }

    /// `getError()`。
    ///
    /// 汇总三类错误后按 `"\n\n"` 连接：models.json 配置错误、每个 provider 的合成错误
    /// （带 `Provider "<id>":` 前缀）、最近一次可用性刷新错误（`Availability refresh:` 前缀）。
    /// 没有任何错误时返回 `None`。
    pub fn get_error(&self) -> Option<String> {
        self.runtime.get_error()
    }

    /// `getAll()`。
    ///
    /// 返回当前快照里全部 provider 的 chat 模型（包含已注册的虚拟模型条目）。结果是新
    /// `Vec`，修改它不影响运行时状态。
    pub fn get_all(&self) -> Vec<Model> {
        self.runtime.get_models(None)
    }

    /// `getAvailable()`。
    ///
    /// 只复制**最近一次**可用性快照的 `available` 列表，不发起凭证读取或探测；因此结果
    /// 可能滞后于最近的凭证变更。需要现场检查时用 [`ModelRegistry::get_available_of_type`]
    /// 或底层 runtime 的异步查询。
    pub fn get_available(&self) -> Vec<Model> {
        self.runtime.get_available_snapshot()
    }

    /// `find(provider, modelId)`。
    ///
    /// 按 provider + id 查 chat 模型；结果包含虚拟模型条目（虚拟模型也是目录条目）。
    /// 未命中返回 `None`。
    pub fn find(&self, provider: &str, model_id: &str) -> Option<Model> {
        self.runtime.get_model(provider, model_id)
    }

    /// `findOfType(type, provider, modelId)`。
    ///
    /// 与 [`ModelRegistry::get_model_of_type`] 等价（TS 里保留了两个方法名，底层是同一个
    /// `runtime.getModelOfType`）。类型不匹配或未命中返回 `None`。
    pub fn find_of_type(
        &self,
        model_type: ModelType,
        provider: &str,
        model_id: &str,
    ) -> Option<Model> {
        self.runtime
            .get_model_of_type(model_type, provider, model_id)
    }

    /// `hasConfiguredAuth(model)`。
    ///
    /// 只看 provider：等价于 `runtime.has_configured_auth(model.provider)`，判定依据是快照里
    /// 的 `configured_providers` 集合。模型本身携带的 headers/apiKey 覆盖不参与该判定，
    /// 也不会触发凭证探测。
    pub fn has_configured_auth(&self, model: &Model) -> bool {
        self.runtime.has_configured_auth(&model.provider)
    }

    /// `getApiKeyAndHeaders(model)`：**永不抛错**。
    ///
    /// 底层 `authHeader requires a resolved API key` 被转换为
    /// `No API key found for "<provider>"`（§2.22.1 的两处同文案来源）。
    ///
    /// # 三条返回路径（严格按 §2.22.1 的顺序）
    /// 1. `runtime.get_auth_for_model(model)` 返回 `Some(resolution)`：组装
    ///    [`ResolvedRequestAuth::Ok`]，带上 `api_key`、合并后的 `headers`、`env`，以及
    ///    **非空**的 `base_url`（空串按 TS 的 falsy 语义被丢弃）。
    /// 2. 返回 `Ok(None)`（没有可解析的 auth）：查 `get_compatibility_request_config`；
    ///    `auth_header == true` 时返回 `Err { error: No API key found for "<provider>" }`，
    ///    否则返回只带 compatibility `headers` 的成功分支。兼容配置自身解析失败时返回
    ///    `Err { error }`，文案取 `ModelRuntimeError::message()`。
    /// 3. 返回 `Err`：取 `error.cause()` 的 message（没有 cause 时用外层 message）；若文案
    ///    恰为 `authHeader requires a resolved API key`，替换为同一句 `No API key found
    ///    for "<provider>"`，否则原样返回。
    ///
    /// 每次调用都会重新解析 auth：测试 `get_api_key_and_headers_resolves_auth_header_on_every_request`
    /// 里，`!command` 型 apiKey 在两次调用间被重新执行，凭证文件变更在第二次调用即生效；
    /// 同一次调用内只解析一次（`get_api_key_and_headers_resolves_configured_auth_exactly_once`）。
    pub async fn get_api_key_and_headers(&self, model: &Model) -> ResolvedRequestAuth {
        match self
            .runtime
            .get_auth_for_model(model, Default::default())
            .await
        {
            Ok(Some(resolution)) => {
                // TS 的 `...(resolution.auth.baseUrl ? { baseUrl } : {})`：空串不出现。
                let base_url = resolution.auth.base_url.filter(|url| !url.is_empty());
                ResolvedRequestAuth::Ok(ResolvedAuth {
                    api_key: resolution.auth.api_key,
                    headers: resolution.auth.headers,
                    base_url,
                    env: resolution.env,
                })
            }
            Ok(None) => match self.runtime.get_compatibility_request_config(model) {
                Ok(compatibility) => {
                    if compatibility.auth_header {
                        ResolvedRequestAuth::Err {
                            error: format!("No API key found for \"{}\"", model.provider),
                        }
                    } else {
                        ResolvedRequestAuth::Ok(ResolvedAuth {
                            headers: compatibility.headers,
                            ..Default::default()
                        })
                    }
                }
                Err(error) => ResolvedRequestAuth::Err {
                    error: error_message(&error),
                },
            },
            Err(error) => {
                let runtime_error = ModelRuntimeError::Auth(error);
                let message = cause_message(&runtime_error);
                ResolvedRequestAuth::Err {
                    error: if message == "authHeader requires a resolved API key" {
                        format!("No API key found for \"{}\"", model.provider)
                    } else {
                        message
                    },
                }
            }
        }
    }

    /// `getProviderAuthStatus(provider)`。
    ///
    /// 同步读取现有状态，不发起探测。判定顺序（先命中先返回）依次为：运行时 API key →
    /// 快照中的持久凭证 → models.json/扩展注册里的请求级配置 → 快照中的 auth 检查 →
    /// `configured: false`。详见底层实现。
    pub fn get_provider_auth_status(&self, provider: &str) -> AuthStatus {
        self.runtime.get_provider_auth_status(provider)
    }

    /// `getProvider(provider)`。
    ///
    /// 返回合成后的 provider 句柄（虚拟条目已由 `with_virtual_models` 包在内）；未注册返回
    /// `None`。句柄是共享的 `Arc`，调用方不应假设其身份（包装层可能每次重组更换）。
    pub fn get_provider(&self, provider: &str) -> Option<Arc<dyn Provider>> {
        self.runtime.get_provider(provider)
    }

    /// `stream(model, context, options?)`。
    ///
    /// 同步返回**惰性**流；setup 的同步前缀（模型类型校验等）在返回前被预 poll 一次，其余
    /// 部分在流被消费时执行。因此“未知 provider / 未配置 / 非 chat 模型”等失败都不会从本
    /// 方法返回 `Err`，而是变成流里的 `stopReason: "error"` assistant 消息。
    pub fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: ModelsStreamOptions,
    ) -> AssistantMessageEventStream {
        self.runtime.stream(model, context, options)
    }

    /// `streamSimple(model, context, options?)`。
    ///
    /// 与 [`ModelRegistry::stream`] 相同地惰性返回与流内报错；区别是当 `model` 是虚拟模型
    /// （`api == "kiana-virtual"`）时，底层的 setup 会先经路由回调把它换成物理模型。
    pub fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: ModelsSimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        self.runtime.stream_simple(model, context, options)
    }

    /// `complete(model, context, options?)`。
    ///
    /// 等待 `stream(...)` 的结果；若流结束却没有产出消息，底层会合成一条 setup 错误消息
    /// 返回，而不是 panic。因此本方法不返回 `Result`，失败信息在 `AssistantMessage` 里。
    pub async fn complete(
        &self,
        model: &Model,
        context: &Context,
        options: ModelsStreamOptions,
    ) -> AssistantMessage {
        self.runtime.complete(model, context, options).await
    }

    /// `getModelsOfType(type, provider?)`。
    ///
    /// 按 [`ModelType`] 过滤当前快照；`provider` 为 `None` 表示所有 provider。同步方法，
    /// 不做 auth 检查：返回的是“目录里有什么”，不是“当前凭证能用什么”。
    pub fn get_models_of_type(&self, model_type: ModelType, provider: Option<&str>) -> Vec<Model> {
        self.runtime.get_models_of_type(model_type, provider)
    }

    /// `getAvailableOfType(type, provider?, options?)`。
    ///
    /// 与 [`ModelRegistry::get_available`] 的区别：本方法是**现场查询**——底层会并发读取
    /// 凭证、做 auth 检查，只返回已鉴权 provider 的模型。任何凭证读取或 auth 检查失败都会
    /// 以 `AuthError` 返回（`provider` 为 `None` 时扫全部 provider）；`options.signal` 用于
    /// 取消。
    pub async fn get_available_of_type(
        &self,
        model_type: ModelType,
        provider: Option<&str>,
        options: AuthOperationOptions,
    ) -> Result<Vec<Model>, AuthError> {
        self.runtime
            .get_available_of_type(model_type, provider, options)
            .await
    }

    /// `getModelOfType(type, provider, modelId)`。
    ///
    /// 按类型 + provider + id 精确查找；类型不符或未命中返回 `None`。结果包含虚拟条目。
    pub fn get_model_of_type(
        &self,
        model_type: ModelType,
        provider: &str,
        model_id: &str,
    ) -> Option<Model> {
        self.runtime
            .get_model_of_type(model_type, provider, model_id)
    }

    /// `classify(model, context, options?)`：永不 reject。
    ///
    /// 失败（非 classifier 模型、provider 未声明分类能力、请求准备失败等）编码在返回的
    /// `ClassifierResult` 里（`stopReason`/`errorMessage`）；中止标志来自
    /// `options.signal`。
    pub async fn classify(
        &self,
        model: &Model,
        context: &ClassifierContext,
        options: ModelsClassifierOptions,
    ) -> ClassifierResult {
        self.runtime.classify(model, context, options).await
    }

    /// `generateImages(model, context, options?)`：永不 reject。
    ///
    /// 失败（非图片模型、provider 未声明图像能力、请求准备失败等）编码在返回的
    /// `AssistantImages` 里（`stopReason`/`errorMessage`）；中止标志来自 `options.signal`。
    pub async fn generate_images(
        &self,
        model: &Model,
        context: &ImagesContext,
        options: ModelsImagesOptions,
    ) -> AssistantImages {
        self.runtime.generate_images(model, context, options).await
    }

    /// `getProviderDisplayName(provider)`。
    ///
    /// 返回 provider 的展示名；provider 未注册时**回退为传入的 id**（不会返回空串或
    /// `None`）。自定义 provider 未给 `name` 时，`name()` 本身也等于 id。
    pub fn get_provider_display_name(&self, provider: &str) -> String {
        self.runtime
            .get_provider(provider)
            .map(|provider| provider.name().to_string())
            .unwrap_or_else(|| provider.to_string())
    }

    /// `getProviderAuth(provider)`：`runtime.getAuth(provider)`。
    ///
    /// `Ok(None)` 表示没有解析结果（未知 provider 也走这条）；解析失败（命令失败、配置值
    /// 无法求值等）以 `AuthError` 原样返回。会现场解析凭证，不读快照。
    pub async fn get_provider_auth(&self, provider: &str) -> Result<Option<AuthResult>, AuthError> {
        self.runtime
            .get_auth_for_provider(provider, Default::default())
            .await
    }

    /// `getApiKeyForProvider(provider)`：任何错误都返回 `None`。
    ///
    /// 三种情况都折叠成 `None` 且无法区分：auth 解析返回错误、没有解析结果、解析结果里
    /// 没有 `api_key`。需要区分原因时改用 [`ModelRegistry::get_provider_auth`]。
    pub async fn get_api_key_for_provider(&self, provider: &str) -> Option<String> {
        match self
            .runtime
            .get_auth_for_provider(provider, Default::default())
            .await
        {
            Ok(Some(resolution)) => resolution.auth.api_key,
            _ => None,
        }
    }

    /// `isUsingOAuth(model)`。
    ///
    /// 只读最近一次可用性快照里的 auth 检查结果，不触发探测；快照未建立或该 provider 没有
    /// auth 记录时返回 `false`。
    pub fn is_using_oauth(&self, model: &Model) -> bool {
        self.runtime.is_using_oauth(&model.provider)
    }

    /// `registerProvider(provider: Provider)`。
    ///
    /// 注册一个原生 provider 实例。id `trim()` 后为空时返回 `Err` 且不改变状态；成功后
    /// 覆盖同 id 的扩展注册配置、立即重组快照，并触发一次后台禁网刷新（返回值丢弃）。
    pub fn register_native_provider(
        &self,
        provider: Arc<dyn Provider>,
    ) -> Result<(), ModelRuntimeError> {
        self.runtime.register_native_provider(provider)
    }

    /// `registerProvider(providerName, config)`。
    ///
    /// TS 的重载在 Rust 拆成两个方法：本方法对应「按名字注册」；`config` 缺省（TS 的第二参数
    /// 为 `undefined`）时逐字抛 `Provider config is required when registering by name`。
    ///
    /// 配置先经校验（以内置 provider 与 models.json 为参考），失败时不改动已存配置；
    /// 成功后与旧配置按“已定义键覆盖”合并（显式空 `images`/`classifiers` 可清空旧实现），
    /// 再重组快照并触发后台禁网刷新。
    pub fn register_provider(
        &self,
        provider_name: &str,
        config: Option<ProviderConfigInput>,
    ) -> Result<(), ModelRuntimeError> {
        let Some(config) = config else {
            return Err(ModelRuntimeError::Message(
                "Provider config is required when registering by name".to_string(),
            ));
        };
        self.runtime.register_provider(provider_name, config)
    }

    /// `unregisterProvider(providerName)`。
    ///
    /// 同时移除扩展注册与原生 provider，然后重组、更新快照并触发后台禁网刷新。不存在的 id
    /// 不会报错（删除是幂等的），但仍会触发重组、快照更新与后台禁网刷新——这与
    /// [`ModelRegistry::unregister_virtual_model`] 不同，后者对不存在的条目提前返回、
    /// 真正不做任何事。本方法没有错误通道，失败不会向调用方报告。
    pub fn unregister_provider(&self, provider_name: &str) {
        self.runtime.unregister_provider(provider_name);
    }

    /// `registerVirtualModel(definition)`。
    ///
    /// 校验 provider id 与 model id 非空、且不与同 id 的**物理**模型冲突（同名虚拟模型允许
    /// 覆盖重注册）；失败返回 `Err` 且不改变状态。成功后目录里多一个虚拟条目，路由回调
    /// 保存在运行时里；注册本身不会调用回调。
    pub fn register_virtual_model(
        &self,
        definition: VirtualModelDefinition,
    ) -> Result<(), ModelRuntimeError> {
        self.runtime.register_virtual_model(definition)
    }

    /// `unregisterVirtualModel(providerName, id)`。
    ///
    /// 移除注册表中的路由回调并从目录中删除该条目；不存在时是空操作。
    pub fn unregister_virtual_model(&self, provider_name: &str, id: &str) {
        self.runtime.unregister_virtual_model(provider_name, id);
    }

    /// `getRegisteredProviderConfig(providerName)`。
    ///
    /// 只返回**扩展注册**的配置（合并后的有效值）；models.json 条目、原生 provider 与虚拟
    /// 模型都不在此列，未注册返回 `None`。
    pub fn get_registered_provider_config(
        &self,
        provider_name: &str,
    ) -> Option<ProviderConfigInput> {
        self.runtime.get_registered_provider_config(provider_name)
    }

    /// `getRegisteredNativeProvider(providerName)`。
    ///
    /// 只查原生注册表；扩展注册的配置用
    /// [`ModelRegistry::get_registered_provider_config`]。
    pub fn get_registered_native_provider(&self, provider_name: &str) -> Option<Arc<dyn Provider>> {
        self.runtime.get_registered_native_provider(provider_name)
    }

    /// `getRegisteredProviderIds()`。
    ///
    /// 扩展注册在前、原生 provider 在后并去重；只包含这两个来源，不含内置 provider、
    /// models.json 与虚拟模型。
    pub fn get_registered_provider_ids(&self) -> Vec<String> {
        self.runtime.get_registered_provider_ids()
    }
}

/// `error instanceof Error ? error.message : String(error)`。
///
/// [`ModelRuntimeError`] 恒为错误，因此只需要取 `message()`；用于兼容配置解析失败的
/// 兜底文案（`getApiKeyAndHeaders` 的 `Ok(None)` 分支）。
fn error_message(error: &ModelRuntimeError) -> String {
    error.message()
}

/// TS catch 分支的 `cause instanceof Error ? cause.message : error.message`。
///
/// 只取**一层** `cause`（与 TS 源码一致）：底层命令/凭证失败时，暴露给扩展的是内层原因，
/// 而不是外层包装文案。没有 `cause` 时退回外层 message。
fn cause_message(error: &ModelRuntimeError) -> String {
    match error.cause() {
        Some(cause) => cause.message.clone(),
        None => error.message(),
    }
}
