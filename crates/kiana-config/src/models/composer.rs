//! provider 四层合成（models.json → extension → OAuth modifyModels → modelOverrides）。
//!
//! TS 源：`packages/coding-agent/src/core/provider-composer.ts`（737 行）。
//! 规范：`spec/ca-config-models.md` §2.19。本文件**不读凭据**（与 TS 的
//! "Compose built-in, models.json, and extension layers without reading credentials" 一致）。
//!
//! ## 层顺序
//!
//! 1. 内置 provider（kiana-ai）的模型列表；
//! 2. `applyModelsJson`：provider 级 baseUrl/compat 覆盖 + 自定义模型 upsert；
//! 3. `applyExtension`：扩展注册（扩展给出 models 时是权威列表，否则 baseUrl 硬覆盖）；
//! 4. 扩展 OAuth 的 `modifyModels`（仅 chat）；
//! 5. models.json 的 `modelOverrides`（最后应用；typed 模型只有 chat 受影响）。
//!
//! ## 与 TS 的类型映射
//!
//! - 扩展注册的输入 [`ProviderConfigInput`] 对应 TS 的 `ProviderConfigInput`；`streamSimple`
//!   与 `refreshModels` 是 `Arc` 闭包，`images`/`classifiers` 是 `kiana_ai::models` 的实现 trait 对象。
//! - 扩展 OAuth 的登录回调 [`ExtensionOAuthCallbacks`] 借用 `AuthInteraction`，方法名与
//!   TS 的 `OAuthLoginCallbacks` 一致（`onAuth`/`onDeviceCode`/`onPrompt`/`onProgress`/
//!   `onManualCodeInput`/`onSelect`/`signal`）。
//! - `withConfiguredAuth` 的 `Authorization: Bearer <key>` 键名逐字保留大写 `A`。
//!
//! ## 为什么需要这一层
//!
//! 内置 provider 提供自己的模型目录、默认认证方式以及流/图像/分类/过滤等能力实现；用户要
//! 在 `models.json` 里改 baseUrl、
//! 增删模型、按模型覆盖参数；扩展还要在运行时注册 provider、OAuth 与流实现。本文件按固定
//! 优先级把这些来源叠成一个普通 [`Provider`]，让上层（`ModelRuntime`）不必在每次取模型或
//! 取认证时分别处理多层来源。
//!
//! ## 一次合成的执行顺序
//!
//! [`compose_model_provider`] 做四件事：
//!
//! 1. 取 models.json 的 provider 条目与扩展注册；两者都不存在且给了 base 时直接返回
//!    同一个 `Arc`（不复制、不包装、不校验）。
//! 2. 构造 [`ComposedProvider`] 后立刻调用一次 `try_compute_all_models`，让结构错误在
//!    注册/重载时就暴露，而不是等到第一次取模型。
//! 3. 组合 apiKey auth 与 OAuth auth；两者都拿不到时返回
//!    `Provider {id}: no authentication method configured.`。
//! 4. 计算展示名、baseUrl 与 `has_refresh`，返回 `Arc<dyn Provider>`。
//!
//! 运行期每次 `get_models`/`get_all_models` 都重新执行 `try_compute_all_models`（合成结果
//! 不缓存），所以 `refresh_models` 只需更新内部槽位，后续读取自然看到新目录。
//!
//! ## 并发与可变状态
//!
//! [`ComposedProvider`] 有两个运行期槽位，均由 `std::sync::Mutex` 保护；只在读取/替换时
//! 短暂持锁并克隆，锁内不 await、不调用外部代码，中毒时用 `into_inner` 恢复：
//!
//! - `refreshed_extension_models`：扩展 `refreshModels` 返回的模型定义；
//! - `extension_oauth_credential`：最近一次刷新拿到的 OAuth 凭证，供 chat 的
//!   `modifyModels` 在取模型时使用。
//!
//! 两个槽位是分别读取的，`try_compute_all_models` 不构成原子快照：刷新恰好发生在两次读取
//! 之间时，同一次合成可能用旧模型列表配新凭证。
//!
//! ## 失败模式
//!
//! - 构造期：models.json/扩展定义的结构错误、缺 api/baseUrl、无认证方式 → `AiError`；
//!   `ModelRuntime` 会记录错误并回退到 base（见 `runtime.rs` 的 `compose_provider`）。
//! - 取模型期：`Provider::get_models`/`get_all_models` 的签名不返回 `Result`；本实现用
//!   panic 复刻 TS 的 throw，`Models` 侧以 `catch_unwind` 兜底（见 `compute_all_models`）。
//! - 刷新期：刷新结果经 `context.publish` 提交，校验放在 `update` 闭包里；错误先存槽位、
//!   发布返回后再上抛，因为 `update` 的签名不能返回错误（见 `refresh_models`）。
//!
//! ## 阅读顺序
//!
//! 1. 数据类型与合并辅助（`spread_objects`、`merge_*`）；
//! 2. 模型构造与三层应用（`model_from_json` → `apply_models_json` → `apply_extension`）；
//! 3. auth 组合（`adapt_oauth`、`compose_api_key_auth`、`compose_oauth_auth`）；
//! 4. 公开辅助（[`validate_extension_provider`]、请求头与状态查询）；
//! 5. [`ComposedProvider`] 与 [`compose_model_provider`]。

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthCheck, AuthContext, AuthError,
    AuthInteraction, AuthPrompt, AuthPromptRequest, AuthResult, AuthSelectOption, AuthType,
    Credential, LoginOptions, ModelAuth, OAuthAuth, OAuthCredential, ProviderAuth,
};
use kiana_ai::compat::extension_oauth_types::{
    OAuthAuthInfo, OAuthCredentials, OAuthDeviceCodeInfo, OAuthPrompt, OAuthSelectPrompt,
};
use kiana_ai::error::{AiError, AiErrorKind};
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::models::{
    ClassifierOptions, ImagesOptions, ModelsPublication, ModelsPublicationPersist, Provider,
    ProviderClassifier, ProviderImages, PublicationUpdateFn, RefreshModelsContext,
    ai_error_from_models_error,
};
use kiana_ai::types::{
    AnyModel, AssistantImages, ClassifierContext, ClassifierResult, ImagesContext, Model,
    ModelCost, ModelInputLimits, ModelInputModality, ModelPromptCache, ModelThinkingLevel,
    ProviderHeaders, SamplingParamsByThinkingLevel, SimpleStreamOptions, StreamOptions,
    ThinkingLevelMap, TranscriptContext,
};
use kiana_ai::utils::abort::AbortScope;
use kiana_ai::utils::diagnostics::ThrownValue;
use kiana_ai::utils::model_operations::{
    classifier_error_result, get_model_type, image_error_result, is_model_type,
};
use kiana_ai::utils::models_error::{TsModelsErrorCode, models_error};

use crate::models::config_file::{
    JsonHeaders, JsonPromptCache, JsonSamplingParamsByThinkingLevel, JsonThinkingLevelMap,
    ModelConfig, ModelsJsonModel, ModelsJsonModelOverride, ModelsJsonOauth, ModelsJsonProvider,
};
use crate::resolve_config_value::{
    ConfigEnv, clear_config_value_cache, get_config_value_env_var_names, is_command_config_value,
    is_config_value_configured, resolve_config_value_or_throw, resolve_headers_or_throw,
};

/// `Arc<dyn Fn(..) -> future>` 的 boxed future（不引入 `futures` 依赖）。
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

// ---------------------------------------------------------------------------
// 扩展注册输入（TS 的 ProviderConfigInput）
// ---------------------------------------------------------------------------

/// 扩展 OAuth 登录回调（TS 的 `OAuthLoginCallbacks`）。
///
/// 方法名与字段适配逐字对应 `adaptOAuth`：`onAuth` → `auth_url`，`onDeviceCode` →
/// `device_code`，`onPrompt` → `text`，`onProgress` → `progress`，`onManualCodeInput` →
/// `manual_code`（固定提示 `Paste the authorization code`），`onSelect` → `select`。
///
/// 这些方法只是把扩展侧的回调转成 [`AuthInteraction`] 的 `notify`/`prompt` 调用：`notify`
/// 不返回错误，`prompt` 的 `Result` 原样透传（错误如何产生由 `AuthInteraction` 的实现
/// 决定，本文件不包装），由扩展的 `login` 实现决定是否中止。`signal` 返回取消信号的克隆，
/// 多个回调共享同一个取消源。
pub struct ExtensionOAuthCallbacks<'a> {
    interaction: &'a dyn AuthInteraction,
}

impl<'a> ExtensionOAuthCallbacks<'a> {
    /// `onAuth(info)`。
    pub fn on_auth(&self, info: OAuthAuthInfo) {
        self.interaction
            .notify(&kiana_ai::auth::types::AuthEvent::AuthUrl {
                url: info.url,
                instructions: info.instructions,
            });
    }

    /// `onDeviceCode(info)`。
    pub fn on_device_code(&self, info: OAuthDeviceCodeInfo) {
        self.interaction
            .notify(&kiana_ai::auth::types::AuthEvent::DeviceCode {
                user_code: info.user_code,
                verification_uri: info.verification_uri,
                open_browser_on_confirm: info.open_browser_on_confirm,
                copy_code_on_confirm: info.copy_code_on_confirm,
                interval_seconds: info.interval_seconds,
                expires_in_seconds: info.expires_in_seconds,
            });
    }

    /// `onProgress(message)`。
    pub fn on_progress(&self, message: impl Into<String>) {
        self.interaction
            .notify(&kiana_ai::auth::types::AuthEvent::Progress {
                message: message.into(),
            });
    }

    /// `onPrompt(prompt)`：`{ type: "text", ...prompt }`。
    ///
    /// 只转交 `message` 与 `placeholder`：扩展类型里的 `allow_empty` 在 canonical 的
    /// `AuthPrompt::Text` 中没有对应字段，因此不参与宿主是否接受空输入。
    pub async fn on_prompt(&self, prompt: OAuthPrompt) -> Result<String, AuthError> {
        self.interaction
            .prompt(AuthPromptRequest::new(AuthPrompt::Text {
                message: prompt.message,
                placeholder: prompt.placeholder,
            }))
            .await
    }

    /// `onManualCodeInput()`：`{ type: "manual_code", message: "Paste the authorization code" }`。
    ///
    /// 提示语逐字固定，不接收扩展传入的消息。
    pub async fn on_manual_code_input(&self) -> Result<String, AuthError> {
        self.interaction
            .prompt(AuthPromptRequest::new(AuthPrompt::ManualCode {
                message: "Paste the authorization code".to_string(),
                placeholder: None,
            }))
            .await
    }

    /// `onSelect(prompt)`：`{ type: "select", ...prompt }`。
    ///
    /// 返回选中项的 `id`。扩展选项类型没有 `description` 字段，转换时统一置 `None`。
    pub async fn on_select(&self, prompt: OAuthSelectPrompt) -> Result<String, AuthError> {
        self.interaction
            .prompt(AuthPromptRequest::new(AuthPrompt::Select {
                message: prompt.message,
                options: prompt
                    .options
                    .into_iter()
                    .map(|option| AuthSelectOption {
                        id: option.id,
                        label: option.label,
                        description: None,
                    })
                    .collect(),
            }))
            .await
    }

    /// `signal`：整个登录流程的取消信号。
    pub fn signal(&self) -> AbortScope {
        self.interaction.signal().clone()
    }

    /// Borrowed host interaction, for extension adapters which need to forward
    /// owned events/prompts across a Send boundary without extending its lifetime.
    pub fn interaction(&self) -> &'a dyn AuthInteraction {
        self.interaction
    }
}

/// 扩展 `login(callbacks)` 返回的 future（借用 callbacks）。
///
/// 返回的 future 允许在多次 `await` 之间继续借用同一个 callbacks，因此扩展可以在整个
/// 登录流程中反复提示用户；取消通过 `callbacks.signal()` 暴露（是否检查由扩展实现决定），
/// 错误统一为 `AuthError`。
pub type ExtensionLoginFuture<'a> = BoxFuture<'a, Result<OAuthCredentials, AuthError>>;
/// 扩展 `login(callbacks)`。
pub type ExtensionLoginFn =
    dyn for<'a> Fn(&'a ExtensionOAuthCallbacks<'a>) -> ExtensionLoginFuture<'a> + Send + Sync;
/// 扩展 `refreshToken(credentials, signal)`。
///
/// 接收凭证的所有权并返回 `'static` future；`signal` 是本次刷新的取消源，本文件只透传
/// 信号，不额外计时、不超时，也不主动中止 future。
pub type ExtensionRefreshTokenFn = dyn Fn(OAuthCredentials, AbortScope) -> BoxFuture<'static, Result<OAuthCredentials, AuthError>>
    + Send
    + Sync;
/// 扩展 `getApiKey(credentials)`。
///
/// 同步、无错误：从 OAuth 凭证推导出 `ModelAuth.api_key`，在 `OAuthAuth::to_auth` 时调用。
pub type ExtensionGetApiKeyFn = dyn Fn(&OAuthCredentials) -> String + Send + Sync;
/// 扩展 `modifyModels(models, credentials)`（仅 chat）。
///
/// 在 `ComposedProvider` 已持有 OAuth 凭证时、每次取模型列表都会调用；输入只是 chat 模型，
/// 输出被当作新的 chat 列表（非 chat 模型由调用方拼在后面）。
pub type ExtensionModifyModelsFn =
    dyn Fn(Vec<Model>, &OAuthCredentials) -> Vec<Model> + Send + Sync;

/// 扩展注册的 OAuth 配置（TS 的 `ExtensionOAuthConfig`）。
///
/// 本结构只保存闭包，不主动调用；调用时机由适配后的 `OAuthAuth` 决定：
/// `login` → [`OAuthAuth::login`]，`refresh_token` → [`OAuthAuth::refresh`]，
/// `get_api_key` → [`OAuthAuth::to_auth`]。`modify_models` 由 [`ComposedProvider`] 在
/// 持有 OAuth 凭证时、对 chat 模型调用（见 `try_compute_all_models`）。
///
/// `uses_callback_server` 仅为扩展源码兼容保留，**不参与任何逻辑**。
#[derive(Clone)]
pub struct ExtensionOAuthConfig {
    /// OAuth 登录方式的展示名（`ProviderAuth.oauth.name()` 的返回值）。
    pub name: String,
    /// 该 auth 方式是否由 provider 订阅支撑。
    pub is_subscription: Option<bool>,
    /// `@deprecated`：保留但忽略。
    pub uses_callback_server: Option<bool>,
    /// 登录流程。
    pub login: Arc<ExtensionLoginFn>,
    /// 刷新 token。
    pub refresh_token: Arc<ExtensionRefreshTokenFn>,
    /// 从凭证推导 API key。
    pub get_api_key: Arc<ExtensionGetApiKeyFn>,
    /// 登录后改写 chat 模型列表。
    pub modify_models: Option<Arc<ExtensionModifyModelsFn>>,
}

impl std::fmt::Debug for ExtensionOAuthConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtensionOAuthConfig")
            .field("name", &self.name)
            .field("is_subscription", &self.is_subscription)
            .field("modify_models", &self.modify_models.is_some())
            .finish_non_exhaustive()
    }
}

/// `ProviderModelConfigBase`（chat/image/classifier 共享字段）。
///
/// `Default` 表示所有字段取空值/零值，**不是**一份可用的模型定义：`id` 与 `name` 由调用方
/// 填写，`input`/`cost` 在 `extension_model_from_definition` 里原样取用。注意空的
/// `input` 会保持为空列表，不会像 models.json 的模型定义那样补成 `["text"]`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderModelConfigBase {
    /// `id`。
    pub id: String,
    /// `name`。
    pub name: String,
    /// `api`。
    pub api: Option<String>,
    /// `baseUrl`。
    pub base_url: Option<String>,
    /// 输入模态。
    pub input: Vec<ModelInputModality>,
    /// 输入上限。
    pub input_limits: Option<ModelInputLimits>,
    /// 价格。
    pub cost: ModelCost,
    /// 请求头。
    pub headers: Option<JsonHeaders>,
    /// 显式声明的 `type`（`None` == 未声明；chat 时 `Some(Chat)` 会写回 JSON）。
    pub declared_type: Option<kiana_ai::types::ModelType>,
}

/// `ProviderChatModelConfig`。
///
/// 扩展注册 chat 模型时给出的定义。TS 的可选布尔/数值字段在 Rust 里是具体类型，
/// `Default` 会给出 false/0，调用方需要自行填有效值。`context_window`/`max_tokens` 是
/// JSON number，构造 `Model` 时经 `number_to_u64` 截断成整数。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderChatModelConfig {
    /// 共享字段。
    pub base: ProviderModelConfigBase,
    /// 是否支持 thinking。
    pub reasoning: bool,
    /// thinking level 映射。
    pub thinking_level_map: Option<ThinkingLevelMap>,
    /// prompt cache 生命周期。
    pub prompt_cache: Option<ModelPromptCache>,
    /// 上下文窗口。
    pub context_window: f64,
    /// 最大输出 token。
    pub max_tokens: f64,
    /// 采样参数。
    pub sampling_params: Option<kiana_ai::types::SamplingParams>,
    /// 按 thinking level 的采样参数。
    pub sampling_params_by_thinking_level: Option<SamplingParamsByThinkingLevel>,
    /// 兼容性开关。
    pub compat: Option<kiana_ai::types::JsonValue>,
}

/// `ProviderImageModelConfig`。
///
/// `output` 是图像模型的输出模态；构造 `Model` 时会映射成 `"text"`/`"image"` 字符串存进
/// `Model.output`（见 `extension_model_from_definition`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderImageModelConfig {
    /// 共享字段。
    pub base: ProviderModelConfigBase,
    /// 输出模态。
    pub output: Vec<ModelInputModality>,
}

/// `ProviderClassifierModelConfig`。
///
/// 分类模型只需要上下文窗口与兼容开关；没有 reasoning/maxTokens 等 chat 字段。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderClassifierModelConfig {
    /// 共享字段。
    pub base: ProviderModelConfigBase,
    /// 上下文窗口。
    pub context_window: f64,
    /// 兼容性开关。
    pub compat: Option<kiana_ai::types::JsonValue>,
}

/// `ProviderModelConfig`（扩展注册的模型定义）。
///
/// 对应 TS 的联合类型，变体即 `type` 字段的判别结果；chat 的 `type` 可省略，其余两种必须
/// 显式声明（因此 Rust 用三个变体而不是一个带 `type` 字段的结构体）。
// chat 变体明显更大（TS 是普通对象）；保持 TS 的枚举形状而不是装箱。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderModelConfig {
    /// `type?: "chat"`。
    Chat(ProviderChatModelConfig),
    /// `type: "image"`。
    Image(ProviderImageModelConfig),
    /// `type: "classifier"`。
    Classifier(ProviderClassifierModelConfig),
}

impl ProviderModelConfig {
    /// `definition.type ?? "chat"`。
    pub fn model_type(&self) -> kiana_ai::types::ModelType {
        match self {
            ProviderModelConfig::Chat(_) => kiana_ai::types::ModelType::Chat,
            ProviderModelConfig::Image(_) => kiana_ai::types::ModelType::Image,
            ProviderModelConfig::Classifier(_) => kiana_ai::types::ModelType::Classifier,
        }
    }

    /// 共享字段。
    pub fn base(&self) -> &ProviderModelConfigBase {
        match self {
            ProviderModelConfig::Chat(config) => &config.base,
            ProviderModelConfig::Image(config) => &config.base,
            ProviderModelConfig::Classifier(config) => &config.base,
        }
    }
}

/// 扩展 `streamSimple(model, context, options)`。
///
/// 只在 `model.api == ProviderConfigInput.api` 时被调用（`stream` 的完整选项会被包成
/// `SimpleStreamOptions`）。同步返回流；本文件只把返回值交给 `lazy_stream`，不捕获实现里
/// 的 panic。
pub type ExtensionStreamSimpleFn = dyn Fn(&Model, &TranscriptContext, SimpleStreamOptions) -> AssistantMessageEventStream
    + Send
    + Sync;

/// 扩展 `refreshModels(context)`。
///
/// 由 `ComposedProvider::refresh_models` 在提交发布**之前**调用；返回值先经
/// `apply_models_json`/`apply_extension` 重新校验，通过后才替换刷新槽位。返回的
/// `AiError` 会直接上抛给 `Models.refresh`（其文档约定：中止引发的错误被丢弃，其余记入
/// 该 provider 的 errors）。扩展的 refresh 闭包会收到含 `publish` 的完整
/// `RefreshModelsContext`，但本文件的约定是扩展只返回模型定义、由组合层统一 `publish`
/// （扩展自行发布不在契约内）。
pub type ExtensionRefreshModelsFn = dyn Fn(RefreshModelsContext) -> BoxFuture<'static, Result<Vec<ProviderModelConfig>, AiError>>
    + Send
    + Sync;

/// 扩展注册输入（TS 的 `ProviderConfigInput`）。
///
/// 这是 `ModelRuntime::register_provider` 的入参：结构本身不解析、不校验，校验发生在
/// [`validate_extension_provider`] 与 [`compose_model_provider`]。`Clone` 逐字段复制：
/// 闭包与 trait 对象是 `Arc`（共享），`models`/`headers` 等容器会复制内容。重注册时 `ModelRuntime` 会把本结构里**已定义**的字段
/// 覆盖到旧注册上（`images`/`classifiers` 以显式置位或非空为准），未定义的字段保留旧值。
#[derive(Clone, Default)]
pub struct ProviderConfigInput {
    /// 展示名。
    pub name: Option<String>,
    /// 默认 baseUrl。
    pub base_url: Option<String>,
    /// API key 模板。
    pub api_key: Option<String>,
    /// 默认 api。
    pub api: Option<String>,
    /// 自定义 chat 流实现。
    pub stream_simple: Option<Arc<ExtensionStreamSimpleFn>>,
    /// 按 api 的图像实现。
    pub images: IndexMap<String, Arc<dyn ProviderImages>>,
    /// `images` 是否**显式给出**（TS 的 `images !== undefined`）。
    ///
    /// Rust 的 [`IndexMap`] 无法区分「未给出」与「显式空表」：重注册时前者保留旧实现，
    /// 后者清空（`registerProvider(id, { images: {} })`）。置 `true` 表示显式给出；
    /// 非空的 `images` 无需置位（按已给出处理）。
    pub images_defined: bool,
    /// 按 api 的分类实现。
    pub classifiers: IndexMap<String, Arc<dyn ProviderClassifier>>,
    /// `classifiers` 是否**显式给出**（TS 的 `classifiers !== undefined`），语义同
    /// [`ProviderConfigInput::images_defined`]。
    pub classifiers_defined: bool,
    /// provider 级请求头。
    pub headers: Option<JsonHeaders>,
    /// 是否发送 `Authorization: Bearer`。
    pub auth_header: Option<bool>,
    /// OAuth 配置。
    pub oauth: Option<ExtensionOAuthConfig>,
    /// 模型定义（给出时是权威列表）。
    pub models: Option<Vec<ProviderModelConfig>>,
    /// 动态模型刷新。
    pub refresh_models: Option<Arc<ExtensionRefreshModelsFn>>,
}

impl std::fmt::Debug for ProviderConfigInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderConfigInput")
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key)
            .field("api", &self.api)
            .field("stream_simple", &self.stream_simple.is_some())
            .field("images", &self.images.keys().collect::<Vec<_>>())
            .field("classifiers", &self.classifiers.keys().collect::<Vec<_>>())
            .field("headers", &self.headers)
            .field("auth_header", &self.auth_header)
            .field("oauth", &self.oauth)
            .field("models", &self.models)
            .field("refresh_models", &self.refresh_models.is_some())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// AuthStatus
// ---------------------------------------------------------------------------

/// `AuthStatus.source` 的六个取值。
///
/// 序列化为 snake_case（`models_json_key`/`models_json_command` 等），是当前稳定契约，
/// 改动需同步测试；`Runtime` 由 `ModelRuntime` 在运行时 key 优先时填充，`Stored` 来自凭证存储，
/// 本文件只产生 `Environment`/`Fallback`/`ModelsJsonKey`/`ModelsJsonCommand`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthStatusSource {
    /// 存储凭证。
    Stored,
    /// 运行时 key。
    Runtime,
    /// 环境变量。
    Environment,
    /// 扩展 apiKey 回退。
    Fallback,
    /// models.json 的 apiKey。
    ModelsJsonKey,
    /// models.json 的 `!command`。
    ModelsJsonCommand,
}

/// `AuthStatus`。
///
/// provider 的鉴权状态：`configured` 表示是否已配置，`source`/`label` 进一步说明来源。
/// `label` 是展示标签：本文件的 `Environment` 来源填逗号分隔的环境变量名；
/// `ModelRuntime` 的回退分支直接使用 `AuthCheck.source`。两个可选字段为 `None` 时序列化
/// 会省略。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AuthStatus {
    /// 是否已配置。
    pub configured: bool,
    /// 来源。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<AuthStatusSource>,
    /// 展示标签（环境变量名等）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// `clearApiKeyCache`：与 `resolve-config-value` 的命令缓存共享。
///
/// 进程级缓存的键是含 `!` 的完整命令字符串，成功与失败结果都会被缓存；测试在命令相关
/// 用例前后调用它隔离缓存。注意 `resolve` 路径上的 `resolve_config_value_or_throw` 走的是
/// **不缓存**版本，命令会现场执行；这里清的是 `resolve_config_value` 路径的缓存。
pub fn clear_api_key_cache() {
    clear_config_value_cache();
}

// ---------------------------------------------------------------------------
// 合并辅助
// ---------------------------------------------------------------------------

/// `getAllProviderModels(provider)`：优先 `getAllModels`（Rust 的默认实现即回退）。
fn get_all_provider_models(provider: Option<&Arc<dyn Provider>>) -> Vec<AnyModel> {
    provider
        .map(|provider| provider.get_all_models())
        .unwrap_or_default()
}

/// 把 `Option<&Map>` 展开成对象再逐键覆盖（JS `{...base, ...override}`，保序）。
fn spread_objects(
    base: Option<&serde_json::Map<String, serde_json::Value>>,
    override_: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut merged = serde_json::Map::new();
    if let Some(base) = base {
        for (key, value) in base {
            merged.insert(key.clone(), value.clone());
        }
    }
    for (key, value) in override_ {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

/// `mergeCompat(base, override)`。
fn merge_compat(
    base: Option<&kiana_ai::types::JsonValue>,
    override_: Option<&kiana_ai::types::JsonValue>,
) -> Option<kiana_ai::types::JsonValue> {
    let Some(override_) = override_ else {
        // TS `if (!override) return base;` —— schema 保证 override 只会是对象或缺失。
        return base.cloned();
    };
    let Some(override_object) = override_.as_object() else {
        return Some(override_.clone());
    };
    let base_object = base.and_then(serde_json::Value::as_object);
    // 第一层：普通键浅覆盖；下面四个键再各做一层嵌套合并。嵌套合并只在「base 或 override
    // 该键是对象」时进行，非对象的一侧按空对象参与（该侧的标量不会覆盖对象里的键）。
    let mut merged = spread_objects(base_object, override_object);
    for key in [
        "openRouterRouting",
        "vercelGatewayRouting",
        "chatTemplateKwargs",
        "chatTemplateArgs",
    ] {
        let base_value = base_object.and_then(|object| object.get(key));
        let override_value = override_object.get(key);
        let base_is_object = base_value.is_some_and(serde_json::Value::is_object);
        let override_is_object = override_value.is_some_and(serde_json::Value::is_object);
        if base_is_object || override_is_object {
            let merged_nested = spread_objects(
                base_value.and_then(serde_json::Value::as_object),
                override_value
                    .and_then(serde_json::Value::as_object)
                    .unwrap_or(&serde_json::Map::new()),
            );
            merged.insert(key.to_string(), serde_json::Value::Object(merged_nested));
        }
    }
    Some(serde_json::Value::Object(merged))
}

/// `mergeInputLimits(base, override)`。
fn merge_input_limits(
    base: Option<&ModelInputLimits>,
    override_: Option<&ModelInputLimits>,
) -> Option<ModelInputLimits> {
    let Some(override_) = override_ else {
        return base.cloned();
    };
    // 逐字段「override 优先，其次 base」；只有 `images`/`images.resize` 各再做一层合并，
    // 未给出 override.images 时整体保留 base.images。
    Some(ModelInputLimits {
        max_request_bytes: override_
            .max_request_bytes
            .or(base.and_then(|base| base.max_request_bytes)),
        images: match &override_.images {
            Some(override_images) => {
                let base_images = base.and_then(|base| base.images.as_ref());
                Some(kiana_ai::types::ModelImageInputLimits {
                    resize: match &override_images.resize {
                        Some(override_resize) => {
                            let base_resize = base_images.and_then(|images| images.resize.as_ref());
                            Some(kiana_ai::types::ModelImageResizeOptions {
                                max_width: override_resize
                                    .max_width
                                    .or(base_resize.and_then(|resize| resize.max_width)),
                                max_height: override_resize
                                    .max_height
                                    .or(base_resize.and_then(|resize| resize.max_height)),
                                max_bytes: override_resize
                                    .max_bytes
                                    .or(base_resize.and_then(|resize| resize.max_bytes)),
                                jpeg_quality: override_resize
                                    .jpeg_quality
                                    .or(base_resize.and_then(|resize| resize.jpeg_quality)),
                            })
                        }
                        None => base_images.and_then(|images| images.resize.clone()),
                    },
                    max_per_message: override_images
                        .max_per_message
                        .or(base_images.and_then(|images| images.max_per_message)),
                    max_per_request: override_images
                        .max_per_request
                        .or(base_images.and_then(|images| images.max_per_request)),
                })
            }
            None => base.and_then(|base| base.images.clone()),
        },
    })
}

/// `mergeSamplingParamsByThinkingLevel(base, override)`。
fn merge_sampling_params_by_thinking_level(
    base: Option<&SamplingParamsByThinkingLevel>,
    override_: Option<&SamplingParamsByThinkingLevel>,
) -> Option<SamplingParamsByThinkingLevel> {
    let Some(override_) = override_ else {
        return base.cloned();
    };
    // 遍历顺序固定（off → … → max），每个 level 的参数做一次浅合并；override 未提及的
    // level 保留 base 原值。固定顺序保证合并行为与遍历结果稳定。
    let mut merged = base.cloned().unwrap_or_default();
    for level in [
        ModelThinkingLevel::Off,
        ModelThinkingLevel::Minimal,
        ModelThinkingLevel::Low,
        ModelThinkingLevel::Medium,
        ModelThinkingLevel::High,
        ModelThinkingLevel::Xhigh,
        ModelThinkingLevel::Max,
    ] {
        let Some(params) = override_.get(&level) else {
            continue;
        };
        let mut entry = merged.get(&level).cloned().unwrap_or_default();
        for (key, value) in params {
            entry.insert(key.clone(), value.clone());
        }
        merged.insert(level, entry);
    }
    Some(merged)
}

/// TS 的 `...model.thinkingLevelMap` / `...override.thinkingLevelMap`（浅合并，`null` 覆盖）。
fn merge_thinking_level_map(
    base: Option<&ThinkingLevelMap>,
    override_: Option<&JsonThinkingLevelMap>,
) -> Option<ThinkingLevelMap> {
    let override_ = override_?;
    // 未知键直接丢弃；值本身是 `Option<String>`，`None`（JSON null）会覆盖 base 的同名
    // level，用来表达「该等级不支持」。
    let mut merged = base.cloned().unwrap_or_default();
    for (key, value) in override_ {
        if let Some(level) = parse_thinking_level(key) {
            merged.insert(level, value.clone());
        }
    }
    Some(merged)
}

/// TS 的 `{...model.promptCache, ...override.promptCache}`。
fn merge_prompt_cache(
    base: Option<&ModelPromptCache>,
    override_: Option<&JsonPromptCache>,
) -> Option<ModelPromptCache> {
    let Some(override_) = override_ else {
        return base.cloned();
    };
    // 只认 `short`/`long` 两档，未知键丢弃；给出的档位覆盖 base。
    let mut merged = base.cloned().unwrap_or_default();
    for (key, value) in override_ {
        if let Some(tier) = parse_prompt_cache_tier(key) {
            merged.insert(tier, *value);
        }
    }
    Some(merged)
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

/// `promptCache` 的 JSON 键 → [`kiana_ai::types::PromptCacheTier`]。
fn parse_prompt_cache_tier(key: &str) -> Option<kiana_ai::types::PromptCacheTier> {
    match key {
        "short" => Some(kiana_ai::types::PromptCacheTier::Short),
        "long" => Some(kiana_ai::types::PromptCacheTier::Long),
        _ => None,
    }
}

/// JSON 视图的 thinkingLevelMap → typed map（丢弃未知键）。
fn to_thinking_level_map(map: &JsonThinkingLevelMap) -> ThinkingLevelMap {
    let mut converted = ThinkingLevelMap::new();
    for (key, value) in map {
        if let Some(level) = parse_thinking_level(key) {
            converted.insert(level, value.clone());
        }
    }
    converted
}

/// JSON 视图的 promptCache → typed map（丢弃未知键）。
fn to_prompt_cache(map: &JsonPromptCache) -> ModelPromptCache {
    let mut converted = ModelPromptCache::new();
    for (key, value) in map {
        if let Some(tier) = parse_prompt_cache_tier(key) {
            converted.insert(tier, *value);
        }
    }
    converted
}

/// JSON 视图的 samplingParamsByThinkingLevel → typed map（丢弃未知键）。
fn to_sampling_params_by_thinking_level(
    map: &JsonSamplingParamsByThinkingLevel,
) -> SamplingParamsByThinkingLevel {
    let mut converted = SamplingParamsByThinkingLevel::new();
    for (key, value) in map {
        if let Some(level) = parse_thinking_level(key) {
            converted.insert(level, value.clone());
        }
    }
    converted
}

/// JS number → `Model` 的 `u64` 字段（小数截断；schema 保证 > 0）。
///
/// 非有限值或非正数返回 0；`f64 as u64` 对 NaN/负数本就为 0、对超范围值饱和到
/// `u64::MAX`，这里统一归 0。
fn number_to_u64(value: f64) -> u64 {
    if value.is_finite() && value > 0.0 {
        value as u64
    } else {
        0
    }
}

/// Clone an arbitrary model while retaining original remote JSON. Typed
/// projections remain useful for requests, but the public object is a TS spread
/// of the raw object, never a serde round-trip with defaults filled in.
///
/// 返回 `(类型化的 Model, 原变体标记, 原始 JSON 对象)`。只有 `AnyModel::Remote` 带原始
/// JSON；`Chat`/`Image`/`Classifier` 的第三个元素是 `None`，重建时会退回对应的具体变体，
/// 不会把整个 `Model` 序列化成 Remote（否则缺省字段会被写出，破坏稀疏性）。
fn clone_any_model(
    model: &AnyModel,
) -> (
    Model,
    AnyModelKindMarker,
    Option<kiana_ai::types::JsonValue>,
) {
    let raw = match model {
        AnyModel::Remote { json, .. } => Some(json.clone()),
        _ => None,
    };
    let (model, kind) = match model {
        AnyModel::Chat(inner) => (inner.clone(), AnyModelKindMarker::Chat),
        AnyModel::Image(inner) => (inner.clone(), AnyModelKindMarker::Image),
        AnyModel::Classifier(inner) => (inner.clone(), AnyModelKindMarker::Classifier),
        AnyModel::Remote { model, .. } => {
            match model.model_type.unwrap_or(kiana_ai::types::ModelType::Chat) {
                kiana_ai::types::ModelType::Chat => (model.clone(), AnyModelKindMarker::Chat),
                kiana_ai::types::ModelType::Image => (model.clone(), AnyModelKindMarker::Image),
                kiana_ai::types::ModelType::Classifier => {
                    (model.clone(), AnyModelKindMarker::Classifier)
                }
            }
        }
    };
    (model, kind, raw)
}

/// Rebuild a model, retaining original fields and insertion positions for
/// remote objects. New properties are appended like TypeScript object spread.
///
/// `changed_keys` 控制「原始 JSON 里本来没有的可选键要不要补写」：`None` 表示把序列化投影
/// 的全部键都写入；`Some(keys)` 只写 `keys` 里的键**或**原始 JSON 已存在的键。这样对稀疏
/// 的远端目录对象做覆盖时，不会把 `api`/`input`/`cost` 等缺省字段凭空物化。已有的键一律
/// 原位覆盖（顺序不变），新键追加在末尾，模拟 TS 的对象展开。
fn rebuild_any_model_with_keys(
    model: Model,
    kind: AnyModelKindMarker,
    raw: Option<kiana_ai::types::JsonValue>,
    changed_keys: Option<&[&str]>,
) -> AnyModel {
    let Some(kiana_ai::types::JsonValue::Object(mut raw)) = raw else {
        return match kind {
            AnyModelKindMarker::Chat => AnyModel::Chat(model),
            AnyModelKindMarker::Image => AnyModel::Image(model),
            AnyModelKindMarker::Classifier => AnyModel::Classifier(model),
        };
    };
    if let kiana_ai::types::JsonValue::Object(projected) =
        serde_json::to_value(&model).unwrap_or(kiana_ai::types::JsonValue::Null)
    {
        for (key, value) in projected {
            if changed_keys.is_none_or(|changed| changed.contains(&key.as_str()))
                || raw.contains_key(&key)
            {
                // The caller's changed-key list controls which absent optional
                // properties are appended. Original fields are overwritten in
                // place, including explicit [] / zero-cost overrides.
                raw.insert(key, value);
            }
        }
    }
    AnyModel::Remote {
        model,
        json: kiana_ai::types::JsonValue::Object(raw),
    }
}

/// Apply a chat override to the original raw object as `{...model, ...changes}`.
fn apply_model_override(model: &Model, override_: &ModelsJsonModelOverride) -> Model {
    // 逐字段「override 有则用，否则沿用 model」；`cost` 的五个子字段、`samplingParams`
    // 的键也是同样的语义。末尾的 `..model.clone()` 保证 `id`/`provider`/`api`/`base_url`/
    // `headers`/`type`/`output` 等本结构体未列出的字段原样保留——models.json 的 override
    // 不能改这些身份字段。
    //
    // `contextWindow`/`maxTokens` 走 `number_to_u64`：TS 允许 JSON number，Rust 的 Model
    // 字段是整数；override 为 0 时写入 0，而不是回退到 base。
    let cost = match &override_.cost {
        Some(override_cost) => ModelCost {
            input: override_cost.input.unwrap_or(model.cost.input),
            output: override_cost.output.unwrap_or(model.cost.output),
            cache_read: override_cost.cache_read.unwrap_or(model.cost.cache_read),
            cache_write: override_cost.cache_write.unwrap_or(model.cost.cache_write),
            tiers: override_cost
                .tiers
                .clone()
                .or_else(|| model.cost.tiers.clone()),
        },
        None => model.cost.clone(),
    };
    Model {
        name: override_.name.clone().unwrap_or_else(|| model.name.clone()),
        reasoning: override_.reasoning.or(model.reasoning),
        thinking_level_map: merge_thinking_level_map(
            model.thinking_level_map.as_ref(),
            override_.thinking_level_map.as_ref(),
        ),
        input: override_
            .input
            .clone()
            .unwrap_or_else(|| model.input.clone()),
        input_limits: merge_input_limits(
            model.input_limits.as_ref(),
            override_.input_limits.as_ref(),
        ),
        cost,
        prompt_cache: merge_prompt_cache(
            model.prompt_cache.as_ref(),
            override_.prompt_cache.as_ref(),
        ),
        context_window: override_
            .context_window
            .map(number_to_u64)
            .or(model.context_window),
        max_tokens: override_.max_tokens.map(number_to_u64).or(model.max_tokens),
        sampling_params: match &override_.sampling_params {
            Some(override_params) => {
                let mut merged = model.sampling_params.clone().unwrap_or_default();
                for (key, value) in override_params {
                    merged.insert(key.clone(), value.clone());
                }
                Some(merged)
            }
            None => model.sampling_params.clone(),
        },
        sampling_params_by_thinking_level: merge_sampling_params_by_thinking_level(
            model.sampling_params_by_thinking_level.as_ref(),
            override_
                .sampling_params_by_thinking_level
                .as_ref()
                .map(to_sampling_params_by_thinking_level)
                .as_ref(),
        ),
        compat: merge_compat(model.compat.as_ref(), override_.compat.as_ref()),
        ..model.clone()
    }
}

// ---------------------------------------------------------------------------
// 模型构造
// ---------------------------------------------------------------------------

/// `modelFromJson(providerId, definition, providerConfig, defaults)`。
#[allow(clippy::result_large_err)]
fn model_from_json(
    provider_id: &str,
    definition: &ModelsJsonModel,
    provider_config: &ModelsJsonProvider,
    defaults: Option<&Model>,
) -> Result<Model, AiError> {
    // 字段回退链：模型定义 → provider 条目 → 同 id/api 的内置模型（`defaults`）。空串视为
    // 未给出（`.filter(|api| !api.is_empty())`）。
    let api = definition
        .api
        .clone()
        .or_else(|| provider_config.api.clone())
        .or_else(|| defaults.map(|model| model.api.clone()))
        .filter(|api| !api.is_empty());
    let Some(api) = api else {
        return Err(AiError::new(
            AiErrorKind::Other,
            format!(
                "Provider {provider_id}, model {}: no \"api\" specified. Set at provider or model level.",
                definition.id
            ),
        ));
    };
    let base_url = definition
        .base_url
        .clone()
        .or_else(|| provider_config.base_url.clone())
        .or_else(|| defaults.map(|model| model.base_url.clone()))
        .filter(|base_url| !base_url.is_empty());
    let Some(base_url) = base_url else {
        return Err(AiError::new(
            AiErrorKind::Other,
            format!("Provider {provider_id}: \"baseUrl\" is required when defining custom models."),
        ));
    };
    // 校验顺序与文案按 TS 逐字复刻：先 api、再 baseUrl，最后两个数值字段。
    if definition.context_window.is_some_and(|value| value <= 0.0) {
        return Err(AiError::new(
            AiErrorKind::Other,
            format!(
                "Provider {provider_id}, model {}: invalid contextWindow",
                definition.id
            ),
        ));
    }
    if definition.max_tokens.is_some_and(|value| value <= 0.0) {
        return Err(AiError::new(
            AiErrorKind::Other,
            format!(
                "Provider {provider_id}, model {}: invalid maxTokens",
                definition.id
            ),
        ));
    }
    // 缺省值：reasoning=false、input=["text"]、contextWindow=128000、maxTokens=16384、
    // cost 全 0。`headers` 固定为 `None`——模型级请求头不在构造期解析，而是在请求/鉴权时由
    // `raw_model_headers` 从 models.json 定义与扩展定义里取出再解析。
    Ok(Model {
        id: definition.id.clone(),
        name: definition
            .name
            .clone()
            .unwrap_or_else(|| definition.id.clone()),
        api,
        provider: provider_id.to_string(),
        base_url,
        reasoning: Some(definition.reasoning.unwrap_or(false)),
        thinking_level_map: definition
            .thinking_level_map
            .as_ref()
            .map(to_thinking_level_map),
        input: definition
            .input
            .clone()
            .unwrap_or_else(|| vec![ModelInputModality::Text]),
        input_limits: definition.input_limits.clone(),
        cost: definition.cost.clone().unwrap_or_default(),
        prompt_cache: definition.prompt_cache.as_ref().map(to_prompt_cache),
        context_window: Some(number_to_u64(
            definition.context_window.unwrap_or(128_000.0),
        )),
        max_tokens: Some(number_to_u64(definition.max_tokens.unwrap_or(16_384.0))),
        sampling_params: definition.sampling_params.clone(),
        sampling_params_by_thinking_level: definition
            .sampling_params_by_thinking_level
            .as_ref()
            .map(to_sampling_params_by_thinking_level),
        headers: None,
        compat: merge_compat(provider_config.compat.as_ref(), definition.compat.as_ref()),
        ..Model::default()
    })
}

/// `findModelDefaults(models, modelId, api?)`。
///
/// 只在 chat 模型里找，依次短路：同 id → 给定 api 相同 → `openai-completions` →
/// 第一个 chat 模型。返回值只用于补 api/baseUrl，不是要复制的模板。
fn find_model_defaults<'a>(
    models: &'a [AnyModel],
    model_id: &str,
    api: Option<&str>,
) -> Option<&'a Model> {
    let chat_models: Vec<&Model> = models
        .iter()
        .filter_map(|model| match model {
            AnyModel::Chat(chat) => Some(chat),
            AnyModel::Remote { model, .. }
                if model.model_type.unwrap_or(kiana_ai::types::ModelType::Chat)
                    == kiana_ai::types::ModelType::Chat =>
            {
                Some(model)
            }
            _ => None,
        })
        .collect();
    chat_models
        .iter()
        .find(|model| model.id == model_id)
        .copied()
        .or_else(|| api.and_then(|api| chat_models.iter().find(|model| model.api == api).copied()))
        .or_else(|| {
            chat_models
                .iter()
                .find(|model| model.api == "openai-completions")
                .copied()
        })
        .or_else(|| chat_models.first().copied())
}

/// `findExtensionModelDefaults(models, definition)`。
///
/// 与 `find_model_defaults` 相同的回退顺序，但候选先按定义的 `type` 过滤；
/// `openai-completions` 这一步只对 chat 生效，image/classifier 不会借用它。
fn find_extension_model_defaults<'a>(
    models: &'a [AnyModel],
    definition: &ProviderModelConfig,
) -> Option<&'a AnyModel> {
    let model_type = definition.model_type();
    let candidates: Vec<&AnyModel> = models
        .iter()
        .filter(|model| model.model_type() == model_type)
        .collect();
    let id = &definition.base().id;
    let api = definition.base().api.as_deref();
    candidates
        .iter()
        .find(|model| model.model().id == *id)
        .copied()
        .or_else(|| {
            api.and_then(|api| {
                candidates
                    .iter()
                    .find(|model| model.model().api == api)
                    .copied()
            })
        })
        .or_else(|| {
            if model_type == kiana_ai::types::ModelType::Chat {
                candidates
                    .iter()
                    .find(|model| model.model().api == "openai-completions")
                    .copied()
            } else {
                None
            }
        })
        .or_else(|| candidates.first().copied())
}

/// `extensionModelFromDefinition(providerId, models, config, definition)`。
#[allow(clippy::result_large_err)]
fn extension_model_from_definition(
    provider_id: &str,
    models: &[AnyModel],
    config: &ProviderConfigInput,
    definition: &ProviderModelConfig,
) -> Result<AnyModel, AiError> {
    let model_type = definition.model_type();
    let base = definition.base();
    let defaults = find_extension_model_defaults(models, definition);
    // api 回退链：模型定义 → （仅 chat）provider 级 `config.api` → 同类型的内置模型。
    // image/classifier 不读取 provider 级 `config.api`。
    let api = base
        .api
        .clone()
        .or_else(|| {
            (model_type == kiana_ai::types::ModelType::Chat)
                .then(|| config.api.clone())
                .flatten()
        })
        .or_else(|| defaults.map(|model| model.model().api.clone()))
        .filter(|api| !api.is_empty());
    let Some(api) = api else {
        // chat 的错误文案多出 " or provider level"（因为 chat 可以读 provider 级 api），
        // image/classifier 没有这个后缀；错误文案是当前稳定契约，改动需同步测试。
        let suffix = if model_type == kiana_ai::types::ModelType::Chat {
            " or provider level"
        } else {
            ""
        };
        return Err(AiError::new(
            AiErrorKind::Other,
            format!(
                "Provider {provider_id}, model {}: no \"api\" specified. Set it at model level{suffix}.",
                base.id
            ),
        ));
    };
    // baseUrl 回退链：模型定义 → 扩展 provider 级 baseUrl → 同类型内置模型；空串视为未给出。
    let base_url = base
        .base_url
        .clone()
        .or_else(|| config.base_url.clone())
        .or_else(|| defaults.map(|model| model.model().base_url.clone()))
        .filter(|base_url| !base_url.is_empty());
    let Some(base_url) = base_url else {
        return Err(AiError::new(
            AiErrorKind::Other,
            format!("Provider {provider_id}: \"baseUrl\" is required when defining custom models."),
        ));
    };
    // 公共字段先落位；其余字段由下面的类型分支填充。`headers` 始终为 `None`：扩展模型的
    // 请求头在请求期由 `raw_model_headers` 从扩展定义里读取并解析，不存进 Model。
    let mut model = Model {
        id: base.id.clone(),
        name: base.name.clone(),
        api,
        provider: provider_id.to_string(),
        base_url,
        input: base.input.clone(),
        input_limits: base.input_limits.clone(),
        cost: base.cost.clone(),
        headers: None,
        ..Model::default()
    };
    match definition {
        // chat：`declared_type` 原样写入（通常为 None，表示按 chat 处理），其余字段不做
        // 合并——扩展定义里没有「可选缺失」的概念，数值字段由调用方给全。
        ProviderModelConfig::Chat(config_model) => {
            model.model_type = base.declared_type;
            model.reasoning = Some(config_model.reasoning);
            model.thinking_level_map = config_model.thinking_level_map.clone();
            model.prompt_cache = config_model.prompt_cache.clone();
            model.context_window = Some(number_to_u64(config_model.context_window));
            model.max_tokens = Some(number_to_u64(config_model.max_tokens));
            model.sampling_params = config_model.sampling_params.clone();
            model.sampling_params_by_thinking_level =
                config_model.sampling_params_by_thinking_level.clone();
            model.compat = config_model.compat.clone();
            Ok(AnyModel::Chat(model))
        }
        ProviderModelConfig::Image(config_model) => {
            model.model_type = Some(kiana_ai::types::ModelType::Image);
            // `Model.output` 存字符串（TS 的 `ImageApi` 输出模态），因此把枚举逐项映射回
            // "text"/"image"；顺序与扩展给出的顺序一致。
            model.output = Some(
                config_model
                    .output
                    .iter()
                    .map(|modality| match modality {
                        ModelInputModality::Text => "text".to_string(),
                        ModelInputModality::Image => "image".to_string(),
                    })
                    .collect(),
            );
            Ok(AnyModel::Image(model))
        }
        // classifier 只带 contextWindow 与 compat；reasoning/maxTokens 等保持默认。
        ProviderModelConfig::Classifier(config_model) => {
            model.model_type = Some(kiana_ai::types::ModelType::Classifier);
            model.context_window = Some(number_to_u64(config_model.context_window));
            model.compat = config_model.compat.clone();
            Ok(AnyModel::Classifier(model))
        }
    }
}

/// `applyModelsJson(providerId, baseModels, config)`。
#[allow(clippy::result_large_err)]
fn apply_models_json(
    provider_id: &str,
    base_models: &[AnyModel],
    config: Option<&ModelsJsonProvider>,
) -> Result<Vec<AnyModel>, AiError> {
    let Some(config) = config else {
        return Ok(base_models.to_vec());
    };
    // 阶段 1：provider 级结构校验（单个模型定义字段的校验在阶段 3 的 `model_from_json`
    // 里）。错误消息是当前稳定契约，改动需同步测试。
    if config.oauth.is_some() && config.base_url.is_none() {
        return Err(AiError::new(
            AiErrorKind::Other,
            format!("Provider {provider_id}: \"baseUrl\" is required when \"oauth\" is set."),
        ));
    }
    let has_overrides = config
        .model_overrides
        .as_ref()
        .is_some_and(|overrides| !overrides.is_empty());
    // 「空配置」判定逐字复刻 TS：这些键一个都没有（且 modelOverrides 为空）才报错。
    // `name`/`api` 不在判定里，因此只写它们仍会被视为无效 provider 配置。
    if config
        .models
        .as_ref()
        .is_none_or(|models| models.is_empty())
        && config.base_url.is_none()
        && config.headers.is_none()
        && config.compat.is_none()
        && !has_overrides
        && config.api_key.is_none()
        && config.oauth.is_none()
        && config.auth_header.is_none()
    {
        return Err(AiError::new(
            AiErrorKind::Other,
            format!(
                "Provider {provider_id}: must specify \"baseUrl\", \"headers\", \"compat\", \"modelOverrides\", or \"models\"."
            ),
        ));
    }

    // 阶段 2：把 provider 级覆盖应用到每个 base 模型。radius OAuth 是唯一例外——它保留
    // 模型原有的 baseUrl，不让 models.json 的 baseUrl 覆盖它。
    let radius_oauth = config.oauth == Some(ModelsJsonOauth::Radius);
    let mut models: Vec<AnyModel> = base_models
        .iter()
        .map(|model| {
            let base_url = if radius_oauth {
                model.model().base_url.clone()
            } else {
                config
                    .base_url
                    .clone()
                    .unwrap_or_else(|| model.model().base_url.clone())
            };
            let (mut inner, kind, raw) = clone_any_model(model);
            inner.base_url = base_url;
            match kind {
                AnyModelKindMarker::Chat => {
                    inner.compat = merge_compat(inner.compat.as_ref(), config.compat.as_ref());
                }
                AnyModelKindMarker::Classifier if config.compat.is_some() => {
                    // classifier 的 provider compat 是**浅合并**（与 chat 的嵌套合并不同）。
                    inner.compat =
                        shallow_merge_json(inner.compat.as_ref(), config.compat.as_ref());
                }
                _ => {}
            }
            rebuild_any_model_with_keys(inner, kind, raw, Some(&["baseUrl", "compat"]))
        })
        .collect();

    // 阶段 3：upsert 自定义模型（models.json 的 models 一律构造成 chat）。
    // 同 id 的 `AnyModel::Chat`（以及任意 `AnyModel::Remote`，这里不检查其 model_type）
    // 会被原地替换以保持顺序；image/classifier 变体即使 id 相同也只会追加。
    // `find_model_defaults` 读取的是**当前**列表，因此前面 upsert 的模型可以成为后面模型的
    // api/baseUrl 默认来源。
    for definition in config.models.iter().flatten() {
        let existing_index = models.iter().position(|model| match model {
            AnyModel::Chat(chat) => chat.id == definition.id,
            AnyModel::Remote { model, .. } => model.id == definition.id,
            _ => false,
        });
        let defaults_api = definition.api.as_deref().or(config.api.as_deref());
        let defaults = find_model_defaults(&models, &definition.id, defaults_api);
        let model = model_from_json(provider_id, definition, config, defaults)?;
        match existing_index {
            Some(index) => models[index] = AnyModel::Chat(model),
            None => models.push(AnyModel::Chat(model)),
        }
    }
    Ok(models)
}

/// `applyExtension(providerId, models, config)`。
#[allow(clippy::result_large_err)]
fn apply_extension(
    provider_id: &str,
    models: &[AnyModel],
    config: Option<&ProviderConfigInput>,
) -> Result<Vec<AnyModel>, AiError> {
    let Some(config) = config else {
        return Ok(models.to_vec());
    };
    // 扩展没有给出 `models`：baseUrl（若给出）对所有模型做**硬覆盖**（包括 image/classifier），
    // 否则原样返回传入列表。
    let Some(definitions) = &config.models else {
        return Ok(match &config.base_url {
            Some(base_url) => models
                .iter()
                .map(|model| {
                    let (mut inner, kind, raw) = clone_any_model(model);
                    inner.base_url = base_url.clone();
                    rebuild_any_model_with_keys(inner, kind, raw, Some(&["baseUrl"]))
                })
                .collect(),
            None => models.to_vec(),
        });
    };
    // 给出了 `models`：扩展定义是权威列表，传入的 models 被完全丢弃；每个定义独立构造，
    // 任一失败即整体失败（`collect::<Result<Vec<_>, _>>` 短路）。
    definitions
        .iter()
        .map(|definition| extension_model_from_definition(provider_id, models, config, definition))
        .collect()
}

/// `AnyModel` 的复制 + 类型标记。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnyModelKindMarker {
    Chat,
    Image,
    Classifier,
}

/// JS `{...base, ...override}`（顶层浅合并，保序）。
fn shallow_merge_json(
    base: Option<&kiana_ai::types::JsonValue>,
    override_: Option<&kiana_ai::types::JsonValue>,
) -> Option<kiana_ai::types::JsonValue> {
    let Some(override_) = override_ else {
        return base.cloned();
    };
    let Some(override_object) = override_.as_object() else {
        return Some(override_.clone());
    };
    Some(serde_json::Value::Object(spread_objects(
        base.and_then(serde_json::Value::as_object),
        override_object,
    )))
}

// ---------------------------------------------------------------------------
// OAuth 适配
// ---------------------------------------------------------------------------

/// 扩展 `OAuthCredentials`（无 `type`）→ 存储凭证。
fn to_credential(credentials: OAuthCredentials) -> OAuthCredential {
    OAuthCredential {
        access: credentials.access,
        refresh: credentials.refresh,
        expires: credentials.expires,
        extra: credentials.extra,
    }
}

/// 存储凭证 → 扩展 `OAuthCredentials`。
fn from_credential(credential: &OAuthCredential) -> OAuthCredentials {
    OAuthCredentials {
        refresh: credential.refresh.clone(),
        access: credential.access.clone(),
        expires: credential.expires,
        extra: credential.extra.clone(),
    }
}

/// `adaptOAuth(config)`。
///
/// 把扩展闭包接到 canonical 的 [`OAuthAuth`]：`login` 把交互对象包装成
/// [`ExtensionOAuthCallbacks`]；`refresh` 双向转换凭证的四个字段（access/refresh/expires/
/// extra）；`to_auth` 只产出 `api_key`（扩展的 OAuth 配置没有 provider 头/baseUrl 字段）。
/// 注意 `LoginOptions`（设备 ID、agent 名）不会转发给扩展，扩展只能通过回调与交互对象
/// 获取信息。
fn adapt_oauth(config: &ExtensionOAuthConfig) -> Arc<dyn OAuthAuth> {
    struct AdaptedOAuth {
        config: ExtensionOAuthConfig,
    }

    #[async_trait]
    impl OAuthAuth for AdaptedOAuth {
        fn name(&self) -> &str {
            &self.config.name
        }

        fn is_subscription(&self) -> bool {
            self.config.is_subscription.unwrap_or(false)
        }

        async fn login(
            &self,
            interaction: &dyn AuthInteraction,
            _options: &LoginOptions,
        ) -> Result<OAuthCredential, AuthError> {
            let callbacks = ExtensionOAuthCallbacks { interaction };
            let credentials = (self.config.login)(&callbacks).await?;
            Ok(to_credential(credentials))
        }

        async fn refresh(
            &self,
            credential: &OAuthCredential,
            signal: &AbortScope,
        ) -> Result<OAuthCredential, AuthError> {
            let refreshed =
                (self.config.refresh_token)(from_credential(credential), signal.clone()).await?;
            Ok(to_credential(refreshed))
        }

        async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
            Ok(ModelAuth {
                api_key: Some((self.config.get_api_key)(&from_credential(credential))),
                ..ModelAuth::default()
            })
        }
    }

    Arc::new(AdaptedOAuth {
        config: config.clone(),
    })
}

/// `withConfiguredAuth(auth, headers, authHeader)`。
#[allow(clippy::result_large_err)]
fn with_configured_auth(
    auth: ModelAuth,
    headers: Option<ConfigEnv>,
    auth_header: bool,
) -> Result<ModelAuth, AiError> {
    // 配置头来自解析后的 `$VAR`/`!command` 值，逐个包成 `Some(value)` 覆盖 auth 自带的头；
    // 只有两侧都没有头时才保持 `headers = None`（与 TS 的 `undefined` 对齐）。
    let has_headers = auth.headers.is_some() || headers.is_some();
    let mut merged_headers: ProviderHeaders = if has_headers {
        let mut merged: ProviderHeaders = auth.headers.clone().unwrap_or_default();
        for (name, value) in headers.unwrap_or_default() {
            merged.insert(name, Some(value));
        }
        merged
    } else {
        ProviderHeaders::new()
    };
    // authHeader 要求已有非空 api_key，否则返回错误而不是构造一份没有 Authorization 的
    // auth；头名 `Authorization` 逐字大写，并覆盖同名的已有配置头。
    if auth_header {
        let api_key = auth
            .api_key
            .clone()
            .filter(|api_key| !api_key.is_empty())
            .ok_or_else(|| {
                AiError::new(AiErrorKind::Other, "authHeader requires a resolved API key")
            })?;
        merged_headers.insert(
            "Authorization".to_string(),
            Some(format!("Bearer {api_key}")),
        );
    }
    Ok(ModelAuth {
        headers: if has_headers || auth_header {
            Some(merged_headers)
        } else {
            None
        },
        ..auth
    })
}

// ---------------------------------------------------------------------------
// 配置来源辅助
// ---------------------------------------------------------------------------

/// `configuredApiKey(config, extension)`（扩展优先）。
///
/// 注意判断的是 `Option` 而不是内容：扩展给出空串 `""` 也会遮住 models.json 的 key
/// （与 TS 的 `??` 一致，空串不是 nullish）。
fn configured_api_key(
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> Option<String> {
    extension
        .and_then(|extension| extension.api_key.clone())
        .or_else(|| config.and_then(|config| config.api_key.clone()))
}

/// `configuredHeaders(config, extension)`（扩展键覆盖 config 同名键）。
///
/// 两者都没有头时返回 `None`；只要有一侧给出（哪怕是显式空表）就返回 `Some`——显式空表
/// 保留「已给出」的信息，后续 `resolve_headers_or_throw` 会把空表归一成 `None`。
fn configured_headers(
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> Option<JsonHeaders> {
    let config_headers = config.and_then(|config| config.headers.as_ref());
    let extension_headers = extension.and_then(|extension| extension.headers.as_ref());
    if config_headers.is_none() && extension_headers.is_none() {
        return None;
    }
    let mut merged = config_headers.cloned().unwrap_or_default();
    if let Some(extension_headers) = extension_headers {
        for (name, value) in extension_headers {
            merged.insert(name.clone(), value.clone());
        }
    }
    Some(merged)
}

/// `configContextEnv(values, ctx, explicit?)`。
///
/// 收集配置值里引用的环境变量：先复制 `explicit`（显式给出的值即使为空串也算已提供，不再
/// 查询 `ctx`），再按 `values`/变量出现顺序去重，只对尚未提供的名字调用 `ctx.env(name)`。
/// 返回 `None` 表示最终没有任何变量；顺序保持首次出现顺序。
async fn config_context_env(
    values: &[String],
    ctx: &dyn AuthContext,
    explicit: Option<&ConfigEnv>,
) -> Option<ConfigEnv> {
    let mut env: ConfigEnv = explicit.cloned().unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    for value in values {
        for name in get_config_value_env_var_names(value) {
            if !seen.insert(name.clone()) {
                continue;
            }
            if env.contains_key(&name) {
                continue;
            }
            if let Some(resolved) = ctx.env(&name).await {
                env.insert(name, resolved);
            }
        }
    }
    if env.is_empty() { None } else { Some(env) }
}

/// `authHeader = extension?.authHeader ?? config?.authHeader ?? false`。
///
/// 逐层回退：扩展显式 `true`/`false` 都会遮住 models.json 的设置，只有扩展未给出时才用
/// config 的值。
fn configured_auth_header(
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> bool {
    extension
        .and_then(|extension| extension.auth_header)
        .or_else(|| config.and_then(|config| config.auth_header))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// composeApiKeyAuth / composeOAuthAuth
// ---------------------------------------------------------------------------

/// 组合 api-key auth（TS 的 `composeApiKeyAuth`）。
///
/// 结果把「base provider 自带的 `ApiKeyAuth`（`inherited`）」与「models.json/扩展配置的
/// `apiKey` + `headers` + `authHeader`」包在一起。`inherited` 可为 `None`；此时仍有默认的
/// “Enter API key” 登录方式（`has_login` 恒为 true），除非 provider 是纯 OAuth（见下方
/// 构造处的短路）。
///
/// `check`/`resolve` 都先看已存储凭证，再看配置的 apiKey，最后退回继承 auth；有已存储
/// 凭证时继承 auth 优先处理（它可能知道额外的来源）。
fn compose_api_key_auth(
    provider_id: &str,
    base: Option<&Arc<dyn Provider>>,
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> Option<Arc<dyn ApiKeyAuth>> {
    struct ComposedApiKeyAuth {
        name: String,
        inherited: Option<Arc<dyn ApiKeyAuth>>,
        raw_key: Option<String>,
        raw_headers: Option<JsonHeaders>,
        auth_header: bool,
        provider_id: String,
    }

    fn configured_check() -> AuthCheck {
        AuthCheck {
            source: Some("configured API key".to_string()),
            r#type: AuthType::ApiKey,
        }
    }

    fn inherited_check(resolved: Option<AuthResult>) -> Option<AuthCheck> {
        resolved.map(|result| AuthCheck {
            source: result.source,
            r#type: AuthType::ApiKey,
        })
    }

    #[async_trait]
    impl ApiKeyAuth for ComposedApiKeyAuth {
        fn name(&self) -> &str {
            &self.name
        }

        fn has_login(&self) -> bool {
            // 组合 auth 总是有 login：继承的或默认的 "Enter API key" 提示。
            true
        }

        async fn login(
            &self,
            interaction: &dyn AuthInteraction,
        ) -> Result<ApiKeyCredential, AuthError> {
            match &self.inherited {
                Some(inherited) if inherited.has_login() => inherited.login(interaction).await,
                _ => {
                    let key = interaction
                        .prompt(AuthPromptRequest::new(AuthPrompt::Secret {
                            message: "Enter API key".to_string(),
                            placeholder: None,
                        }))
                        .await?;
                    Ok(ApiKeyCredential {
                        key: Some(key),
                        env: None,
                        extra: IndexMap::new(),
                    })
                }
            }
        }

        fn has_check(&self) -> bool {
            true
        }

        async fn check(&self, input: ApiKeyAuthInput<'_>) -> Result<Option<AuthCheck>, AuthError> {
            // 档 1：有已存储凭证。继承 auth 若实现了 check 就完全委托；否则非空 key 直接算
            // 「已配置」，空 key 则退回继承 auth 的 resolve 看有没有别的来源。
            if let Some(credential) = input.credential {
                if let Some(inherited) = &self.inherited
                    && inherited.has_check()
                {
                    return inherited.check(input).await;
                }
                if credential.key.as_deref().is_some_and(|key| !key.is_empty()) {
                    return Ok(Some(AuthCheck {
                        source: Some("stored credential".to_string()),
                        r#type: AuthType::ApiKey,
                    }));
                }
                let resolved = match &self.inherited {
                    Some(inherited) => inherited.resolve(input).await?,
                    None => None,
                };
                return Ok(inherited_check(resolved));
            }
            // 档 2：配置的 apiKey。命令不执行、直接算已配置（失败要到 resolve 才会暴露）；
            // 模板则要求它引用的每个环境变量都能从 `input.ctx` 解析出来。
            if let Some(raw_key) = &self.raw_key {
                if is_command_config_value(raw_key) {
                    return Ok(Some(configured_check()));
                }
                for name in get_config_value_env_var_names(raw_key) {
                    if input.ctx.env(&name).await.is_none() {
                        return Ok(None);
                    }
                }
                return Ok(Some(configured_check()));
            }
            // 档 3：没有凭证也没有配置 key，退回继承 auth。
            if let Some(inherited) = &self.inherited
                && inherited.has_check()
            {
                return inherited.check(input).await;
            }
            let resolved = match &self.inherited {
                Some(inherited) => inherited.resolve(input).await?,
                None => None,
            };
            Ok(inherited_check(resolved))
        }

        async fn resolve(
            &self,
            input: ApiKeyAuthInput<'_>,
        ) -> Result<Option<AuthResult>, AuthError> {
            // 取来源的顺序与 check 相同；区别是这里要真正解析出值：
            // - 已存储凭证：继承 auth 优先，否则用凭证里的 key；
            // - 配置的 apiKey：先用 `input.ctx` 收集环境变量，再用 `resolve_config_value_or_throw`
            //   解析（命令在这里执行，失败转成 `AuthError`），然后交给继承 auth 处理；
            // - 都没有：继承 auth。
            let mut result: Option<AuthResult> = if let Some(credential) = input.credential {
                match &self.inherited {
                    Some(inherited) => inherited.resolve(input).await?,
                    None => credential
                        .key
                        .as_deref()
                        .filter(|key| !key.is_empty())
                        .map(|key| AuthResult {
                            auth: ModelAuth {
                                api_key: Some(key.to_string()),
                                ..ModelAuth::default()
                            },
                            env: credential.env.clone(),
                            source: Some("stored credential".to_string()),
                        }),
                }
            } else if let Some(raw_key) = &self.raw_key {
                let env = config_context_env(std::slice::from_ref(raw_key), input.ctx, None).await;
                let key = resolve_config_value_or_throw(
                    raw_key,
                    &format!("API key for provider \"{}\"", self.provider_id),
                    env.as_ref(),
                )
                .map_err(|error| AuthError::new(error.to_string()))?;
                match &self.inherited {
                    Some(inherited) => {
                        let credential = ApiKeyCredential {
                            key: Some(key),
                            env: None,
                            extra: IndexMap::new(),
                        };
                        inherited
                            .resolve(ApiKeyAuthInput {
                                ctx: input.ctx,
                                credential: Some(&credential),
                                signal: input.signal,
                            })
                            .await?
                    }
                    None => Some(AuthResult {
                        auth: ModelAuth {
                            api_key: Some(key),
                            ..ModelAuth::default()
                        },
                        env: None,
                        source: Some("configured API key".to_string()),
                    }),
                }
            } else {
                match &self.inherited {
                    Some(inherited) => inherited.resolve(input).await?,
                    None => None,
                }
            };
            let Some(found) = result.take() else {
                return Ok(None);
            };
            // 统一收尾：合并「凭证里的 env」与「解析结果里的 env」，再用它解析配置头，最后
            // 把头和 authHeader 叠加到返回的 `ModelAuth` 上。头解析失败（缺变量/命令失败）
            // 同样转成 `AuthError`，因此一次 resolve 要么给出完整认证，要么整体失败。
            let mut explicit_env = input
                .credential
                .and_then(|credential| credential.env.clone())
                .unwrap_or_default();
            if let Some(result_env) = &found.env {
                for (name, value) in result_env {
                    explicit_env.insert(name.clone(), value.clone());
                }
            }
            let header_values: Vec<String> = self
                .raw_headers
                .as_ref()
                .map(|headers| headers.values().cloned().collect())
                .unwrap_or_default();
            let header_env =
                config_context_env(&header_values, input.ctx, Some(&explicit_env)).await;
            let headers = resolve_headers_or_throw(
                self.raw_headers.as_ref(),
                &format!("provider \"{}\"", self.provider_id),
                header_env.as_ref(),
            )
            .map_err(|error| AuthError::new(error.to_string()))?;
            let auth = with_configured_auth(found.auth, headers, self.auth_header)
                .map_err(|error| AuthError::new(error.message))?;
            Ok(Some(AuthResult { auth, ..found }))
        }
    }

    // `oauth_present` 同时看扩展 OAuth 与 base 自带 OAuth：只有「既无继承 apiKey、又无配置
    // apiKey、却有 OAuth」时才不伪造 apiKey 登录方式。其余情况一律返回组合 auth。
    let inherited = base.and_then(|base| base.auth().api_key.clone());
    let raw_key = configured_api_key(config, extension);
    let oauth_present = extension
        .and_then(|extension| extension.oauth.as_ref())
        .is_some()
        || base.is_some_and(|base| base.auth().oauth.is_some());
    // OAuth-only providers get no fabricated API-key login method.
    if inherited.is_none() && raw_key.is_none() && oauth_present {
        return None;
    }
    let raw_headers = configured_headers(config, extension);
    let auth_header = configured_auth_header(config, extension);
    Some(Arc::new(ComposedApiKeyAuth {
        name: inherited
            .as_ref()
            .map(|inherited| inherited.name().to_string())
            .unwrap_or_else(|| "API key".to_string()),
        inherited,
        raw_key,
        raw_headers,
        auth_header,
        provider_id: provider_id.to_string(),
    }))
}

/// 组合 OAuth auth（TS 的 `composeOAuthAuth`）。
///
/// OAuth 来源只有一个：扩展 OAuth 优先（经 `adapt_oauth` 适配），否则用 base 自带的
/// OAuth；都没有时返回 `None`。本函数只在 `to_auth` 时解析配置头（`configured_headers`
/// 的值可能来自环境变量/命令），登录与刷新阶段不做头解析。
fn compose_oauth_auth(
    provider_id: &str,
    base: Option<&Arc<dyn Provider>>,
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> Option<Arc<dyn OAuthAuth>> {
    struct ComposedOAuthAuth {
        inner: Arc<dyn OAuthAuth>,
        raw_headers: Option<JsonHeaders>,
        auth_header: bool,
        provider_id: String,
    }

    #[async_trait]
    impl OAuthAuth for ComposedOAuthAuth {
        fn name(&self) -> &str {
            self.inner.name()
        }

        fn is_subscription(&self) -> bool {
            self.inner.is_subscription()
        }

        fn login_label(&self) -> Option<&str> {
            self.inner.login_label()
        }

        async fn login(
            &self,
            interaction: &dyn AuthInteraction,
            options: &LoginOptions,
        ) -> Result<OAuthCredential, AuthError> {
            self.inner.login(interaction, options).await
        }

        async fn refresh(
            &self,
            credential: &OAuthCredential,
            signal: &AbortScope,
        ) -> Result<OAuthCredential, AuthError> {
            self.inner.refresh(credential, signal).await
        }

        async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
            let auth = self.inner.to_auth(credential).await?;
            // TS 读取 `credential.env`（OAuthCredentials 的索引签名）；Rust 里它是 `extra`。
            // 只接受对象里值为字符串的项，其余类型静默丢弃；没有 `env` 键时得到 `None`，
            // 头解析将回退到进程环境。
            let env: Option<ConfigEnv> = credential
                .extra
                .get("env")
                .and_then(serde_json::Value::as_object)
                .map(|object| {
                    object
                        .iter()
                        .filter_map(|(name, value)| {
                            value
                                .as_str()
                                .map(|value| (name.clone(), value.to_string()))
                        })
                        .collect()
                });
            let headers = resolve_headers_or_throw(
                self.raw_headers.as_ref(),
                &format!("provider \"{}\"", self.provider_id),
                env.as_ref(),
            )
            .map_err(|error| AuthError::new(error.to_string()))?;
            with_configured_auth(auth, headers, self.auth_header)
                .map_err(|error| AuthError::new(error.message))
        }
    }

    let oauth = match extension.and_then(|extension| extension.oauth.as_ref()) {
        Some(oauth) => adapt_oauth(oauth),
        None => base.and_then(|base| base.auth().oauth.clone())?,
    };
    Some(Arc::new(ComposedOAuthAuth {
        inner: oauth,
        raw_headers: configured_headers(config, extension),
        auth_header: configured_auth_header(config, extension),
        provider_id: provider_id.to_string(),
    }))
}

// ---------------------------------------------------------------------------
// raw model headers
// ---------------------------------------------------------------------------

/// `rawModelHeaders(model, config, extension)`。
///
/// 收集模型级请求头的原始（未解析）值，供请求期展开。三个来源按优先级从低到高写入：
/// models.json `modelOverrides[model.id].headers` → models.json `models[].headers` →
/// 扩展模型中同 type+id 定义的 headers；同名键后者覆盖前者，`IndexMap` 保留键第一次出现
/// 的位置（测试 `configured_model_headers_merge_overrides_and_extension_headers` 锁定了
/// 输出键顺序）。`modelOverrides` 与 models.json 的模型定义都只对 chat 模型生效；扩展定义
/// 必须同时匹配 `model_type` 与 id，避免不同类型同 id 的模型共享头。
fn raw_model_headers(
    model: &AnyModel,
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> Option<JsonHeaders> {
    let is_chat = model.model_type() == kiana_ai::types::ModelType::Chat;
    let model_id = &model.model().id;
    let model_type = get_model_type(model);
    let chat_definition = if is_chat {
        config
            .and_then(|config| config.models.as_ref())
            .and_then(|models| models.iter().find(|entry| entry.id == *model_id))
    } else {
        None
    };
    let extension_model = extension
        .and_then(|extension| extension.models.as_ref())
        .and_then(|models| {
            models
                .iter()
                .find(|entry| entry.model_type() == model_type && entry.base().id == *model_id)
        });
    let mut headers = JsonHeaders::new();
    if is_chat
        && let Some(overrides) = config.and_then(|config| config.model_overrides.as_ref())
        && let Some(override_) = overrides.get(model_id)
        && let Some(override_headers) = &override_.headers
    {
        for (name, value) in override_headers {
            headers.insert(name.clone(), value.clone());
        }
    }
    if let Some(definition) = chat_definition
        && let Some(definition_headers) = &definition.headers
    {
        for (name, value) in definition_headers {
            headers.insert(name.clone(), value.clone());
        }
    }
    if let Some(definition) = extension_model
        && let Some(definition_headers) = &definition.base().headers
    {
        for (name, value) in definition_headers {
            headers.insert(name.clone(), value.clone());
        }
    }
    if headers.is_empty() {
        None
    } else {
        Some(headers)
    }
}

// ---------------------------------------------------------------------------
// 公开辅助
// ---------------------------------------------------------------------------

/// `validateExtensionProvider(providerId, base, modelsConfig, extension)`。
///
/// 只校验、不产生任何状态：先检查 `stream_simple` 必须同时给出 `api`（错误文案
/// `Provider {id}: "api" is required when registering streamSimple.`），再把 base 的全
/// 类型模型目录依次过一遍 `apply_models_json` 与 `apply_extension`，触发与真正合成相同的
/// 结构校验（models.json 空配置、缺 api/baseUrl、扩展定义不合法等），结果直接丢弃。
///
/// `ModelRuntime::register_provider` 在写入注册表**之前**调用它，所以失败时旧注册保持不变。
/// 本函数不检查认证是否可得——那是 `compose_model_provider` 的职责。
#[allow(clippy::result_large_err)]
pub fn validate_extension_provider(
    provider_id: &str,
    base: Option<&Arc<dyn Provider>>,
    models_config: Option<&ModelsJsonProvider>,
    extension: &ProviderConfigInput,
) -> Result<(), AiError> {
    if extension.stream_simple.is_some() && extension.api.is_none() {
        return Err(AiError::new(
            AiErrorKind::Other,
            format!("Provider {provider_id}: \"api\" is required when registering streamSimple."),
        ));
    }
    apply_extension(
        provider_id,
        &apply_models_json(provider_id, &get_all_provider_models(base), models_config)?,
        Some(extension),
    )?;
    Ok(())
}

/// `resolveConfiguredModelHeaders(model, config, extension, env?)`。
///
/// 取 `raw_model_headers` 的原始头并逐项展开 `$VAR`/`!command`。`env` 是调用方注入的显式
/// 环境（通常来自本次认证解析出的 env）；显式表里没有的名字会回退到进程环境。返回
/// `Ok(None)` 表示没有配置头；解析失败返回 `ConfigValueError`，描述形如
/// `model "{provider}/{id}" header "{name}"`（provider/id 取自 `model` 对象）。
pub fn resolve_configured_model_headers(
    model: &AnyModel,
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
    env: Option<&ConfigEnv>,
) -> Result<Option<ConfigEnv>, crate::resolve_config_value::ConfigValueError> {
    resolve_headers_or_throw(
        raw_model_headers(model, config, extension).as_ref(),
        &format!("model \"{}/{}\"", model.model().provider, model.model().id),
        env,
    )
}

/// `CompatibilityRequestConfig`。
///
/// 兼容层构造请求时用到的最终头与 `authHeader` 开关。配置来源的头已解析成具体字符串；
/// 模型对象上的静态头（`model.headers`）原样带入，本类型不保证它们被展开过。值统一包成
/// `Some` 以适配 `ProviderHeaders`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompatibilityRequestConfig {
    /// 请求头。
    pub headers: Option<ProviderHeaders>,
    /// 是否发送 `Authorization: Bearer`。
    pub auth_header: bool,
}

/// `resolveCompatibilityRequestConfig(model, config, extension)`。
///
/// 合并顺序（后者覆盖前者）：
/// 1. provider 级配置头（`configured_headers`：models.json 与扩展，扩展优先）；
/// 2. 模型级配置头（`raw_model_headers`，见其文档）；
/// 3. 把以上配置值统一解析（**只读进程环境**，本函数没有 env 参数）；
/// 4. 模型对象上已解析好的静态头 `model.headers`；
/// 5. 再叠加第 3 步的结果。
///
/// 返回的头用 `ProviderHeaders`（值类型是 `Option<String>`），两侧都为空时才返回 `None`。
/// 解析失败（缺环境变量/命令失败）返回 `ConfigValueError`，描述指向具体模型的 header。
pub fn resolve_compatibility_request_config(
    model: &AnyModel,
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> Result<CompatibilityRequestConfig, crate::resolve_config_value::ConfigValueError> {
    let mut combined = configured_headers(config, extension).unwrap_or_default();
    if let Some(model_headers) = raw_model_headers(model, config, extension) {
        for (name, value) in model_headers {
            combined.insert(name, value);
        }
    }
    // 空表不送进解析器：`resolve_headers_or_throw(Some(empty))` 也会返回 `Ok(None)`，
    // 这里提前归一只是省一次调用。
    let configured = if combined.is_empty() {
        None
    } else {
        Some(combined)
    };
    let configured = resolve_headers_or_throw(
        configured.as_ref(),
        &format!("model \"{}/{}\"", model.model().provider, model.model().id),
        None,
    )?;
    let model_headers = model.model().headers.as_ref();
    let headers = if model_headers.is_some() || configured.is_some() {
        let mut merged: ProviderHeaders = model_headers
            .map(|headers| {
                headers
                    .iter()
                    .map(|(name, value)| (name.clone(), Some(value.clone())))
                    .collect()
            })
            .unwrap_or_default();
        for (name, value) in configured.unwrap_or_default() {
            merged.insert(name, Some(value));
        }
        Some(merged)
    } else {
        None
    };
    Ok(CompatibilityRequestConfig {
        headers,
        auth_header: configured_auth_header(config, extension),
    })
}

/// `configuredRequestAuthStatus(config, extension)`。
///
/// 只根据 models.json/扩展配置判断 apiKey 是否可用，不访问凭证存储：
///
/// - `configured_api_key` 为 `None` → 返回 `None`（交给调用方继续看运行时/存储凭证）；
/// - `!command` → 视为已配置（`ModelsJsonCommand`），**不执行命令**，命令失败要等到
///   `resolve` 才暴露；
/// - `$VAR` 模板 → 只用**进程环境**判断（本函数没有注入 env 的入口）：全部变量都有非空
///   值时返回 `Environment` 且 `label` 是逗号分隔的变量名，否则 `configured: false` 且
///   无来源；
/// - 其他（字面量 key，含空串）→ 已配置；扩展提供 `api_key` 时来源是 `Fallback`，否则
///   `ModelsJsonKey`。
pub fn configured_request_auth_status(
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> Option<AuthStatus> {
    let value = configured_api_key(config, extension)?;
    if is_command_config_value(&value) {
        return Some(AuthStatus {
            configured: true,
            source: Some(AuthStatusSource::ModelsJsonCommand),
            label: None,
        });
    }
    let names = get_config_value_env_var_names(&value);
    if !names.is_empty() {
        // 传 `None` 表示只按进程环境判断；`is_config_value_configured` 内部对空值也算缺失。
        return Some(if is_config_value_configured(&value, None) {
            AuthStatus {
                configured: true,
                source: Some(AuthStatusSource::Environment),
                label: Some(names.join(", ")),
            }
        } else {
            AuthStatus {
                configured: false,
                source: None,
                label: None,
            }
        });
    }
    Some(AuthStatus {
        configured: true,
        source: Some(
            if extension.is_some_and(|extension| extension.api_key.is_some()) {
                AuthStatusSource::Fallback
            } else {
                AuthStatusSource::ModelsJsonKey
            },
        ),
        label: None,
    })
}

// ---------------------------------------------------------------------------
// composeModelProvider
// ---------------------------------------------------------------------------

/// 组合后的 provider（四层 + auth + 能力分派）。
///
/// 所有字段私有，对外只通过 [`Provider`] trait 暴露。`base`/`config`/`extension` 是构造
/// 时的快照（`Arc` 共享，不可变）；两个 `Mutex` 槽位保存刷新期可变状态（见模块文档的
/// 「并发与可变状态」）。`get_models`/`get_all_models` 每次调用都重新合成，不缓存，因此
/// 槽位一更新，下一次读取就能看到新目录；这也意味着重复调用会重复做完整合成与校验。
pub struct ComposedProvider {
    provider_id: String,
    name: String,
    base_url: Option<String>,
    headers: Option<ProviderHeaders>,
    auth: ProviderAuth,
    base: Option<Arc<dyn Provider>>,
    config: Option<Arc<ModelsJsonProvider>>,
    extension: Option<Arc<ProviderConfigInput>>,
    extension_oauth_credential: Arc<Mutex<Option<OAuthCredential>>>,
    refreshed_extension_models: Arc<Mutex<Option<Vec<ProviderModelConfig>>>>,
    has_refresh: bool,
}

impl std::fmt::Debug for ComposedProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ComposedProvider")
            .field("provider_id", &self.provider_id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl ComposedProvider {
    /// 当前生效的扩展配置（含刷新后的模型列表）。
    ///
    /// 刷新结果只替换 `models` 字段，其余字段（baseUrl/api/apiKey/streamSimple 等）仍来自
    /// 原始注册；没有刷新结果时返回原扩展的克隆。
    fn current_extension(&self) -> Option<ProviderConfigInput> {
        let extension = self.extension.as_deref()?;
        let refreshed = self
            .refreshed_extension_models
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        Some(match refreshed {
            Some(models) => {
                let mut current = extension.clone();
                current.models = Some(models);
                current
            }
            None => extension.clone(),
        })
    }

    /// 内置 provider 的全类型模型目录。
    fn base_models(&self) -> Vec<AnyModel> {
        get_all_provider_models(self.base.as_ref())
    }

    /// 计算四层合成后的模型列表（TS 的 `getAllModels()`），错误可传播。
    ///
    /// 阶段：base → models.json（`apply_models_json`）→ 扩展（`apply_extension`）→
    /// OAuth `modifyModels`（仅 chat）→ models.json `modelOverrides`。前三个阶段
    /// 的结构错误在这里以 `AiError` 返回；后两个阶段是纯变换，不返回错误。
    #[allow(clippy::result_large_err)]
    fn try_compute_all_models(&self) -> Result<Vec<AnyModel>, AiError> {
        // 阶段 1-3：base（含非 chat）→ models.json → 扩展（扩展给 models 时是权威列表）。
        let config = self.config.as_deref();
        let extension = self.current_extension();
        let mut models = apply_extension(
            &self.provider_id,
            &apply_models_json(&self.provider_id, &self.base_models(), config)?,
            extension.as_ref(),
        )?;

        // 阶段 4：OAuth modifyModels。只有「槽位里有凭证」且「原始扩展配置（不是刷新后的
        // 克隆）声明了 modifyModels」时才执行。凭证与扩展列表各自加锁/取快照，不是原子读取。
        let credential = self
            .extension_oauth_credential
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if let (Some(credential), Some(extension)) = (credential, self.extension.as_deref())
            && let Some(modify_models) = extension
                .oauth
                .as_ref()
                .and_then(|oauth| oauth.modify_models.as_ref())
        {
            // 只把 chat 模型（`AnyModel::Chat`，或 type 缺省的 Remote）交给扩展；非 chat
            // 模型原样保留。扩展返回的 chat 列表排到前面，因此这一步会改变模型顺序。
            let mut chat_models = Vec::new();
            let mut other_models = Vec::new();
            for model in models {
                match model {
                    AnyModel::Chat(chat) => chat_models.push(chat),
                    AnyModel::Remote { model, .. }
                        if model.model_type.unwrap_or(kiana_ai::types::ModelType::Chat)
                            == kiana_ai::types::ModelType::Chat =>
                    {
                        chat_models.push(model)
                    }
                    other => other_models.push(other),
                }
            }
            models = modify_models(chat_models, &from_credential(&credential))
                .into_iter()
                .map(AnyModel::Chat)
                .chain(other_models)
                .collect();
        }

        // 阶段 5：modelOverrides 最后应用，按模型 id 匹配。typed 模型只有 chat 受影响；
        // Remote 条目按 id 命中时不论 model_type 都会被覆盖并保留原始 JSON
        // （`rebuild_any_model_with_keys`）；普通 Chat 直接替换 typed 模型。
        let overrides = config.and_then(|config| config.model_overrides.as_ref());
        Ok(models
            .into_iter()
            .map(|model| match model {
                AnyModel::Remote { model: chat, json } => {
                    match overrides.and_then(|overrides| overrides.get(&chat.id)) {
                        Some(override_) => {
                            let updated = apply_model_override(&chat, override_);
                            // 固定列表是 `apply_model_override` 一定会产出的字段；
                            // `input`/`cost` 只在 override 显式给出时才写入，避免把缺省值
                            // 物化进稀疏的 Remote JSON（见 `rebuild_any_model_with_keys`）。
                            let mut changed = vec![
                                "name",
                                "reasoning",
                                "thinkingLevelMap",
                                "inputLimits",
                                "promptCache",
                                "contextWindow",
                                "maxTokens",
                                "samplingParams",
                                "samplingParamsByThinkingLevel",
                                "compat",
                            ];
                            if override_.input.is_some() {
                                changed.push("input");
                            }
                            if override_.cost.is_some() {
                                changed.push("cost");
                            }
                            rebuild_any_model_with_keys(
                                updated,
                                AnyModelKindMarker::Chat,
                                Some(json),
                                Some(&changed),
                            )
                        }
                        None => AnyModel::Remote { model: chat, json },
                    }
                }
                AnyModel::Chat(chat) => {
                    match overrides.and_then(|overrides| overrides.get(&chat.id)) {
                        Some(override_) => AnyModel::Chat(apply_model_override(&chat, override_)),
                        None => AnyModel::Chat(chat),
                    }
                }
                other => other,
            })
            .collect())
    }

    /// 计算四层合成后的模型列表（TS 的 `getAllModels()`）。
    ///
    /// `Provider::get_models`/`get_all_models` 的冻结签名不返回 `Result`；TS 里这里抛出的
    /// 错误会被 `Models` 的 try/catch 吞掉，Rust 侧用 panic 复刻（`Models` 用 `catch_unwind`）。
    /// 可达的失败已由 `compose_model_provider` 的 eager 校验与 refresh 的发布前校验拦住。
    ///
    /// panic 的 payload 就是 `AiError.message`。
    fn compute_all_models(&self) -> Vec<AnyModel> {
        self.try_compute_all_models()
            .unwrap_or_else(|error| panic!("{}", error.message))
    }

    /// `streamWith(model, context, options, simple)`。
    ///
    /// 返回 `lazy_stream`。`lazy_stream` 会在构造时先同步 poll 一次 setup；本函数的 setup
    /// 没有 await 点，因此“选实现并创建内层流”这一步在 `stream_with` 返回前就完成，之后
    /// 只是把内层事件转发出去。分派优先级：
    ///
    /// 1. 扩展提供了 `stream_simple` 且 `model.api == extension.api` → 调扩展；
    /// 2. base provider 的 `get_models()` 中有同 `api` 的模型 → 交给 base 的
    ///    `stream`/`stream_simple`（按传入的选项种类选择）；
    /// 3. 全局 API provider 注册表 `kiana_ai::compat::get_api_provider(model.api)`；
    ///    未注册时返回逐字错误 `No API provider registered for api: {api}`。
    ///
    /// 注意第 2 步用的是 `get_models()`（只看 chat 目录），不是 `get_all_models()`。
    /// 扩展只提供 simple 版本，完整选项会在 `into_simple` 里包成 `SimpleStreamOptions`。
    fn stream_with(
        &self,
        model: &Model,
        context: &TranscriptContext,
        dispatch: DispatchOptions,
    ) -> AssistantMessageEventStream {
        let base = self.base.clone();
        let extension = self.extension.clone();
        let model = model.clone();
        let context = context.clone();
        let stream_model = model.clone();
        let setup: kiana_ai::api::lazy::LazySetup = Box::pin(async move {
            // 1. 扩展流实现：只匹配它声明的单一 api（`ProviderConfigInput::api`）。
            if let Some(extension) = &extension
                && let Some(stream_simple) = &extension.stream_simple
                && extension.api.as_deref() == Some(model.api.as_str())
            {
                return Ok(stream_simple(&model, &context, dispatch.into_simple()));
            }
            // 2. base 自带同 api 的 chat 模型 → 委托 base；base 未覆盖该 api 时继续。
            if let Some(base) = &base
                && base.get_models().iter().any(|entry| entry.api == model.api)
            {
                return Ok(match dispatch {
                    DispatchOptions::Full(options) => base.stream(&model, &context, options),
                    DispatchOptions::Simple(options) => {
                        base.stream_simple(&model, &context, options)
                    }
                });
            }
            // 3. 最后按 api 名查全局注册表；找不到时把错误放进流（而不是 panic）。
            let Some(api) = kiana_ai::compat::get_api_provider(&model.api) else {
                return Err(AiError::new(
                    AiErrorKind::Other,
                    format!("No API provider registered for api: {}", model.api),
                ));
            };
            Ok(match dispatch {
                DispatchOptions::Full(options) => api.stream(&model, &context, options),
                DispatchOptions::Simple(options) => api.stream_simple(&model, &context, options),
            })
        });
        kiana_ai::api::lazy::lazy_stream(&stream_model, setup, None)
    }

    /// 替代 trait 默认的 `filter_all_models`（TS：只有 base 提供时才转发）。
    ///
    /// 与 `Provider` 的默认实现同构（默认函数在 kiana-ai 私有，无法直接调用）：可用 chat
    /// id 集合来自 `self.filter_models(self.get_models(), credential)`——`get_models` 会做
    /// 一次完整合成，`filter_models` 在 base 实现时委托给它；不在集合里的 chat 模型被过滤，
    /// image/classifier 一律保留。输出保持输入顺序。
    fn fallback_filter_all_models(
        &self,
        models: Vec<AnyModel>,
        credential: Option<&Credential>,
    ) -> Vec<AnyModel> {
        let available_ids: std::collections::HashSet<String> = self
            .filter_models(self.get_models(), credential)
            .into_iter()
            .map(|model| model.id)
            .collect();
        models
            .into_iter()
            .filter(|model| {
                !is_model_type(model, kiana_ai::types::ModelType::Chat)
                    || available_ids.contains(&model.model().id)
            })
            .collect()
    }
}

/// `streamWith` 的类型化分派参数。
///
/// 两个变体对应 `Provider::stream` / `Provider::stream_simple` 的两种入参；选择哪种由调用
/// 哪个 trait 方法决定，组合层不会把 simple 升级成 full。
enum DispatchOptions {
    Full(StreamOptions),
    Simple(SimpleStreamOptions),
}

impl DispatchOptions {
    /// TS 把同一个 options 对象传给 `extension.streamSimple`；Rust 的完整选项需要包一层。
    fn into_simple(self) -> SimpleStreamOptions {
        match self {
            DispatchOptions::Full(base) => SimpleStreamOptions {
                base,
                ..SimpleStreamOptions::default()
            },
            DispatchOptions::Simple(options) => options,
        }
    }
}

#[async_trait]
impl Provider for ComposedProvider {
    fn id(&self) -> &str {
        &self.provider_id
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

    /// 只保留 chat 模型；`AnyModel::Remote` 的 `model_type` 缺失时按 chat 处理。
    fn get_models(&self) -> Vec<Model> {
        self.compute_all_models()
            .into_iter()
            .filter_map(|model| match model {
                AnyModel::Chat(chat) => Some(chat),
                AnyModel::Remote { model, .. }
                    if model.model_type.unwrap_or(kiana_ai::types::ModelType::Chat)
                        == kiana_ai::types::ModelType::Chat =>
                {
                    Some(model)
                }
                _ => None,
            })
            .collect()
    }

    /// 全部类型（chat/image/classifier）的合成目录，不缓存。
    fn get_all_models(&self) -> Vec<AnyModel> {
        self.compute_all_models()
    }

    /// 恒为 true：本类型确实实现了“全类型目录”，包装/上层可据此转发。
    fn has_get_all_models(&self) -> bool {
        true
    }

    /// 构造时算好的刷新能力快照（base 或扩展提供刷新/`modifyModels` 时为 true），
    /// 之后不会变化。
    fn has_refresh_models(&self) -> bool {
        self.has_refresh
    }

    /// 刷新 base 与扩展目录，并把结果写回内部槽位。
    ///
    /// 步骤：
    /// 1. 无刷新能力（构造时 `has_refresh == false`）直接返回；
    /// 2. 先让 base 刷新（若它实现了），同一个 `context` 克隆过去，错误立即上抛；
    /// 3. 调扩展 `refreshModels` 拿到新的模型定义（失败同样上抛）；
    /// 4. 检查取消信号：已中止则返回 `Ok(())`，不更新本层槽位、也不再提交本层发布
    ///    （base 自己的刷新可能已经完成）；
    /// 5. 把「新模型定义 + 本次 OAuth 凭证」写进 `update` 闭包，经 `context.publish` 提交。
    ///    闭包内先重跑 `apply_models_json`/`apply_extension` 校验新定义，失败时把错误存进
    ///    槽位并放弃本次更新；发布被取代（`update` 不执行）时错误不会产生；
    /// 6. 发布返回后取出校验错误并上抛（`update` 自身不能返回错误）。
    async fn refresh_models(&self, context: RefreshModelsContext) -> Result<(), AiError> {
        if !self.has_refresh {
            return Ok(());
        }
        // 2. base 的刷新会自行 publish 它的目录；这里不替它保存结果。
        if let Some(base) = &self.base
            && base.has_refresh_models()
        {
            base.refresh_models(context.clone()).await?;
        }
        // 3. 扩展刷新只是拿到定义，真正生效在下面的 update 闭包里。
        let mut refreshed = None;
        if let Some(extension) = self.extension.as_deref()
            && let Some(refresh) = &extension.refresh_models
        {
            refreshed = Some(refresh(context.clone()).await?);
        }
        // 4. 两次刷新都可能已经执行；取消后不再提交新的发布。
        if context.signal.is_aborted() {
            return Ok(());
        }
        let oauth_credential = match &context.credential {
            Some(Credential::OAuth(credential)) => Some(credential.clone()),
            _ => None,
        };
        let refreshed_slot = Arc::clone(&self.refreshed_extension_models);
        let credential_slot = Arc::clone(&self.extension_oauth_credential);
        // TS 把校验放在 publication 的 `update` 里：被取代/已中止的发布不会执行 update，
        // 因此也不会因为刷新结果不合法而报错。Rust 的 update 不能返回错误，先把错误存进
        // slot，发布返回后再上抛。
        let validation_error: Arc<Mutex<Option<AiError>>> = Arc::new(Mutex::new(None));
        let validation_error_slot = Arc::clone(&validation_error);
        let provider_id = self.provider_id.clone();
        let base = self.base.clone();
        let config = self.config.clone();
        let extension = self.extension.clone();
        let update: PublicationUpdateFn = Box::new(move || {
            if let Some(refreshed) = refreshed {
                if let Some(extension) = &extension {
                    // 用当前 base 目录重放「models.json + 扩展」两层，只为校验新定义：
                    // modifyModels 与 modelOverrides 是纯变换，不会产生结构错误。
                    let mut with_models = (**extension).clone();
                    with_models.models = Some(refreshed.clone());
                    let applied = match apply_models_json(
                        &provider_id,
                        &get_all_provider_models(base.as_ref()),
                        config.as_deref(),
                    ) {
                        Ok(models) => apply_extension(&provider_id, &models, Some(&with_models)),
                        Err(error) => Err(error),
                    };
                    if let Err(error) = applied {
                        *validation_error_slot
                            .lock()
                            .unwrap_or_else(|error| error.into_inner()) = Some(error);
                        return;
                    }
                }
                // 校验通过才写模型槽位；失败时上面已提前 return，凭证槽位也保持不变。
                *refreshed_slot
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(refreshed);
            }
            // 即使没有新模型定义也要写凭证：OAuth `modifyModels` 依赖这个槽位。
            *credential_slot
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = oauth_credential;
        });
        // persist: Unchanged —— 组合层不写 ModelsStore，目录只存在内存槽位里。`publish`
        // 返回 Ok(false) 表示被取代/已中止；此时 update 不执行，validation_error 保持 None。
        (context.publish)(ModelsPublication {
            persist: ModelsPublicationPersist::Unchanged,
            update: Some(update),
        })
        .await?;
        // 只有 update 真正执行过才可能留下错误；被取代的刷新按成功返回（测试
        // `superseded_refresh_does_not_validate_refreshed_models` 锁定了该行为）。
        if let Some(error) = validation_error
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            return Err(error);
        }
        Ok(())
    }

    /// 只有 base 实现了过滤才报告能力；models.json/扩展层不提供过滤实现。
    fn has_filter_models(&self) -> bool {
        self.base
            .as_ref()
            .is_some_and(|base| base.has_filter_models())
    }

    /// base 实现了 `filter_models` 就原样委托，否则原样返回输入。
    fn filter_models(&self, models: Vec<Model>, credential: Option<&Credential>) -> Vec<Model> {
        match &self.base {
            Some(base) if base.has_filter_models() => base.filter_models(models, credential),
            _ => models,
        }
    }

    fn has_filter_all_models(&self) -> bool {
        self.base
            .as_ref()
            .is_some_and(|base| base.has_filter_all_models())
    }

    /// base 实现了全类型过滤就委托；否则走本文件的 `fallback_filter_all_models`。
    fn filter_all_models(
        &self,
        models: Vec<AnyModel>,
        credential: Option<&Credential>,
    ) -> Vec<AnyModel> {
        match &self.base {
            Some(base) if base.has_filter_all_models() => {
                base.filter_all_models(models, credential)
            }
            _ => self.fallback_filter_all_models(models, credential),
        }
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        self.stream_with(model, context, DispatchOptions::Full(options))
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        self.stream_with(model, context, DispatchOptions::Simple(options))
    }

    fn has_fetch_deferred(&self) -> bool {
        self.base
            .as_ref()
            .is_some_and(|base| base.has_fetch_deferred())
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &kiana_ai::types::DeferredHandle,
        options: kiana_ai::types::DeferredFetchOptions,
    ) -> Option<AssistantMessageEventStream> {
        self.base
            .as_ref()
            .and_then(|base| base.fetch_deferred(model, handle, options))
    }

    fn has_cancel_deferred(&self) -> bool {
        self.base
            .as_ref()
            .is_some_and(|base| base.has_cancel_deferred())
    }

    async fn cancel_deferred(
        &self,
        model: &Model,
        handle: &kiana_ai::types::DeferredHandle,
        options: kiana_ai::types::DeferredCancelOptions,
    ) -> Result<(), AiError> {
        // 只有 base 支持时才委托；否则构造 `ModelsError`（code=provider），文案使用
        // `model.provider`（而不是 `self.provider_id`）。
        match &self.base {
            Some(base) if base.has_cancel_deferred() => {
                base.cancel_deferred(model, handle, options).await
            }
            _ => Err(ai_error_from_models_error(models_error(
                TsModelsErrorCode::Provider,
                &format!(
                    "Provider {} does not support deferred responses",
                    model.provider
                ),
                None,
            ))),
        }
    }

    fn has_generate_images(&self) -> bool {
        self.base
            .as_ref()
            .is_some_and(|base| base.has_generate_images())
            || self
                .extension
                .as_ref()
                .is_some_and(|extension| !extension.images.is_empty())
    }

    async fn generate_images(
        &self,
        model: &Model,
        context: &ImagesContext,
        options: Option<&ImagesOptions>,
    ) -> AssistantImages {
        // 扩展实现按 `model.api` 精确查表，优先于 base；扩展表为空时这一来源不贡献能力
        // （`has_generate_images` 仍可能因 base 为 true），直接调用则落到 base 分支或错误结果。
        if let Some(extension) = self.extension.as_deref()
            && let Some(implementation) = extension.images.get(&model.api)
        {
            return implementation
                .generate_images(model, context, options)
                .await;
        }
        if let Some(base) = &self.base
            && base.has_generate_images()
        {
            return base.generate_images(model, context, options).await;
        }
        // TS 调用 `imageErrorResult(model, error)`，`aborted` 默认 `false`（不读 signal）。
        image_error_result(
            model,
            &ThrownValue::Text(format!(
                "Provider {} has no image implementation for \"{}\"",
                self.provider_id, model.api
            )),
            false,
        )
    }

    fn has_classify(&self) -> bool {
        self.base.as_ref().is_some_and(|base| base.has_classify())
            || self
                .extension
                .as_ref()
                .is_some_and(|extension| !extension.classifiers.is_empty())
    }

    async fn classify(
        &self,
        model: &Model,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> ClassifierResult {
        // 与图像同构：扩展按 api 精确匹配并优先，其次 base，最后错误结果。
        if let Some(extension) = self.extension.as_deref()
            && let Some(implementation) = extension.classifiers.get(&model.api)
        {
            return implementation.classify(model, context, options).await;
        }
        if let Some(base) = &self.base
            && base.has_classify()
        {
            return base.classify(model, context, options).await;
        }
        // TS 调用 `classifierErrorResult(model, error)`，`aborted` 默认 `false`（不读 signal）。
        classifier_error_result(
            model,
            &ThrownValue::Text(format!(
                "Provider {} has no classifier implementation for \"{}\"",
                self.provider_id, model.api
            )),
            false,
        )
    }
}

/// `composeModelProvider(providerId, base, modelConfig, extension)`。
///
/// 参数：
/// - `provider_id`：provider 在注册表中的 id，也是结果的 `Provider::id()`；
/// - `base`：内置/原生 provider，可为 `None`（此时只有 models.json/扩展层）；
/// - `model_config`：整个 models.json 配置，函数内部用 `get_provider` 按 id 取条目；
/// - `extension`：扩展注册输入，可为 `None`。
///
/// 无 models.json 覆盖层且无扩展时，直接返回 base（TS `ModelRuntime.composeProvider`
/// 的短路；见 `ca-config-models.md` §2.20.6）。
///
/// 没有 base 且没有覆盖层时**不是错误**：会合成一个空模型列表、只有默认 apiKey 登录
/// 方式的 provider。
///
/// # 错误
///
/// - 构造后的 eager 合成失败（models.json/扩展定义缺字段或组合非法）；
/// - apiKey auth 与 OAuth auth 都为 `None`（返回
///   `Provider {id}: no authentication method configured.`）。
///
/// 错误类型是 `AiError`（`AiErrorKind::Other`）；调用方 `ModelRuntime` 会记录消息并回退到
/// base，因此本函数成功返回的 provider 一定至少有一种认证方式。
#[allow(clippy::result_large_err)]
pub fn compose_model_provider(
    provider_id: &str,
    base: Option<Arc<dyn Provider>>,
    model_config: &ModelConfig,
    extension: Option<&ProviderConfigInput>,
) -> Result<Arc<dyn Provider>, AiError> {
    // 短路：没有覆盖层时原样返回同一个 `Arc`（调用方可用 `Arc::ptr_eq` 判断未包装）。
    // 这时不做任何校验，也不检查 base 的认证配置。
    let config = model_config.get_provider(provider_id);
    if config.is_none()
        && extension.is_none()
        && let Some(base) = &base
    {
        return Ok(Arc::clone(base));
    }

    let provider = ComposedProvider {
        provider_id: provider_id.to_string(),
        name: String::new(),
        base_url: None,
        headers: base.as_ref().and_then(|base| base.headers().cloned()),
        auth: ProviderAuth::default(),
        base: base.clone(),
        config: config.clone(),
        extension: extension.map(|extension| Arc::new(extension.clone())),
        extension_oauth_credential: Arc::new(Mutex::new(None)),
        refreshed_extension_models: Arc::new(Mutex::new(None)),
        has_refresh: false,
    };

    // Validate eagerly so registration/reload reports structural errors immediately.
    // 结果丢弃：这里只想要错误。成功后每次 get_models 仍会重新合成。
    let _all_models = provider.try_compute_all_models()?;
    let api_key = compose_api_key_auth(
        provider_id,
        provider.base.as_ref(),
        config.as_deref(),
        extension,
    );
    let oauth = compose_oauth_auth(
        provider_id,
        provider.base.as_ref(),
        config.as_deref(),
        extension,
    );
    // 两种认证都组合不出来时不创建 provider；纯 OAuth provider 的 apiKey 为 None 是正常的。
    if api_key.is_none() && oauth.is_none() {
        return Err(AiError::new(
            AiErrorKind::Other,
            format!("Provider {provider_id}: no authentication method configured."),
        ));
    }

    // 展示名回退链：扩展名 → models.json name → base 名 → 扩展 OAuth 名 → provider id。
    // 注意 `headers` 只继承 base 的头；配置头在 auth/请求解析时单独合并，不放进这里。
    let name = extension
        .and_then(|extension| extension.name.clone())
        .or_else(|| config.as_ref().and_then(|config| config.name.clone()))
        .or_else(|| provider.base.as_ref().map(|base| base.name().to_string()))
        .or_else(|| {
            extension.and_then(|extension| extension.oauth.as_ref().map(|oauth| oauth.name.clone()))
        })
        .unwrap_or_else(|| provider_id.to_string());
    // provider 级 baseUrl 与每个模型的 baseUrl 是两套值：模型自身的 baseUrl 已在合成阶段
    // 逐个写入。这里的回退链：扩展 → models.json → base。
    let base_url = extension
        .and_then(|extension| extension.base_url.clone())
        .or_else(|| config.as_ref().and_then(|config| config.base_url.clone()))
        .or_else(|| {
            provider
                .base
                .as_ref()
                .and_then(|base| base.base_url().map(str::to_string))
        });
    // 刷新能力是构造时快照：base 可刷新、扩展有 refreshModels、或扩展 OAuth 有
    // modifyModels（后者需要 refresh 阶段把 OAuth 凭证写进槽位）都算。
    let has_refresh = provider
        .base
        .as_ref()
        .is_some_and(|base| base.has_refresh_models())
        || extension.is_some_and(|extension| {
            extension.refresh_models.is_some()
                || extension
                    .oauth
                    .as_ref()
                    .is_some_and(|oauth| oauth.modify_models.is_some())
        });

    let composed = ComposedProvider {
        provider_id: provider_id.to_string(),
        name,
        base_url,
        headers: provider.headers,
        auth: ProviderAuth { api_key, oauth },
        base: provider.base,
        config: provider.config,
        extension: provider.extension,
        extension_oauth_credential: provider.extension_oauth_credential,
        refreshed_extension_models: provider.refreshed_extension_models,
        has_refresh,
    };
    Ok(Arc::new(composed))
}
