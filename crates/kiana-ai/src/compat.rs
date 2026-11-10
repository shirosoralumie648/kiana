//! `compat.ts`（302 行）：旧全局 `kiana-ai` 面的兼容入口。
//!
//! 现有应用把 `@earendil-works/kiana-ai` 的导入换成 `@earendil-works/kiana-ai/compat` 即可继续
//! 工作；新代码用 `createModels()` 与 provider 工厂。本模块在 `kiana-coding-agent` 迁移完成后
//! 删除（与 TS 的注释一致）。
//!
//! ## 再导出面
//!
//! 与 TS 相同：10 个 `api/*.lazy.ts`（Rust 侧是各适配器模块的 `*_api()` 工厂 +
//! `set_bedrock_provider_module`）、`env-api-keys.ts`、`image-models.ts`、`images.ts`、
//! `images-api-registry.ts`、`index.ts`、`legacy-api-aliases.ts`、
//! `providers/images/register-builtins.ts`，以及 type-only 的 `BuiltinProvider`。
//!
//! ## 模块级状态与顺序（易被忽略；已按 `spec/ai-core.md` §2.1.15b 核对）
//!
//! 1. 10 个内置 API 注册进 api-registry（[`register_built_in_api_providers`]）；
//! 2. 之后才构造 `compatModels = builtinModels()`（42 个 provider）。
//!
//! Rust 用两个 `LazyLock` 表达：`REGISTRY` 的初始化先注册内置 API，`COMPAT_MODELS`
//! 只在 `stream`/`streamSimple` 的「内置 provider」分支被访问，而该分支之前一定会读
//! `REGISTRY`（`get_builtin_provider_for_model` 先查注册表），因此 `REGISTRY` 总在
//! `COMPAT_MODELS` 之前就绪。
//!
//! ## 与 TS 的差异（均由 Rust 的「同步返回流」契约决定）
//!
//! - `wrapStream`/`wrapStreamSimple` 在 `model.api !== api` 时 TS **同步抛**
//!   `Error("Mismatched api: …")`；Rust 的 [`ProviderStreams`] 签名不能抛错，因此映射成
//!   流上的 `error` 事件（[`lazy_stream_error`]，`errorMessage` 是当前稳定契约，
//!   改动需同步测试）。
//! - `resolveApiProvider` 缺失时 TS 抛 `Error("No API provider registered for api: …")`；
//!   [`stream`]/[`stream_simple`] 同样把它映射成 `error` 事件流。
//! - `registerFauxProvider(options = {})` 的默认参数在 Rust 里是
//!   `RegisterFauxProviderOptions::default()`。
//!
//! ## `complete` 的 `result()`
//!
//! TS 的 `complete = stream(...).result()`；Rust 的
//! [`AssistantMessageEventStream::result`] 只在结果通道被丢弃时返回 `Err`
//! （持有流时不会发生），这里按 `Models::complete` 的同一方式合成为 `stopReason: "error"`
//! 的消息。
//!
//! ## 调用方与全局状态
//!
//! 迁移期仍有若干下游直接使用本模块。以下为已知调用点（不是完整清单，例如）：
//! `kiana-session` 调 `stream_simple` 与 `reset_api_providers`；`kiana-compaction` /
//! `kiana-resources` 调 `complete_simple`；`kiana-builtin-extensions` 的本地 llama
//! provider 转发 `stream`/`stream_simple`；`kiana-config` 的 composer 用 `get_api_provider`
//! 与 `extension_oauth_types`；`kiana-ext-api` 在公开的 `ProviderConfig.stream_simple`
//! 暴露 `ApiStreamSimpleFunction` 并引用 `extension_oauth_types`；`kiana-extensions` 的
//! loader/runner/virtual_modules 用 `ApiStreamSimpleFunction`、`get_api_provider` 与
//! `extension_oauth_types`。删除本文件前必须全仓搜索 `kiana_ai::compat::`（含测试），
//! 逐个迁移；只迁移上面列出的调用点不足以移除这些编译依赖。
//!
//! `REGISTRY` 是**进程级**全局状态，不按 `Models` 实例隔离：任何调用方注册/注销/重置都
//! 对其他调用方立刻可见。所有读写都经 `lock_registry` 串行化。持锁期间不调用流函数或
//! 用户代码，流函数一律在锁外调用；但临界区不只有索引操作（克隆 `Arc`、插入/删除条目）：
//! 初始化和重置会在锁内执行 `builtin_apis()` 的 10 个 `*_api()` 工厂
//! （`bedrock_converse_stream_api` 还会锁 `BEDROCK_MODULE_OVERRIDE` 并克隆模块），
//! `insert_provider` 也在锁内构造 `wrap_stream`/`wrap_stream_simple` 闭包。因此这些工厂
//! 必须保持轻量，且不得回调注册表（`std::sync::Mutex` 不可重入）。测试因此必须把涉及
//! 注册表的用例串行化（见文件末尾 `tests` 的 `REGISTRY_TEST_LOCK`）。
//!
//! ## 阅读顺序
//!
//! 1. 再导出面：文件顶部的 `pub use`（对应 TS 的 `export *`）；
//! 2. api-registry：[`ApiProvider`] → [`ApiProviderInternal`] → `RegistryState` 与
//!    `lock_registry`；
//! 3. [`register_faux_provider`]：注册表的主要写入方；
//! 4. 分派：[`stream`]/[`stream_simple`] 的内置快路径与注册表路径，最后是
//!    [`complete`]/[`complete_simple`] 对 `result()` 的收尾。
//!
//! ## 参数形态（非语义差异）
//!
//! TS 的 `options?` 是可选参数；Rust 入口一律要求传值，无选项时用
//! `StreamOptions::default()` / `SimpleStreamOptions::default()`，字段全为 `None`/空，
//! 与 TS 省略 options 时走到的分支相同。

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use indexmap::IndexMap;

// 这些名字同时经由 `index::*` 公开再导出（TS 的 compat 也同时 `export *` 了 index.ts），
// 因此这里用 `pub use` 而不是私有 `use`，避免「私有导入遮蔽公开 glob 再导出」。
pub use crate::api::lazy::lazy_stream_error;
pub use crate::error::{AiError, AiErrorKind};
pub use crate::event_stream::AssistantMessageEventStream;
pub use crate::models::{MutableModels, Provider, ProviderStreams};
// `builtinModels` 不在 TS compat 的导出面里（只用于内部构造 `compatModels`），因此
// 这里保持私有 `use`。
use crate::providers::all::builtin_models;
pub use crate::providers::faux::{
    FauxCore, FauxProviderState, FauxResponseStep, RegisterFauxProviderOptions, create_faux_core,
};
pub use crate::types::{
    Api, AssistantMessage, Context, Model, SimpleStreamOptions, StreamOptions, TranscriptContext,
};
pub use crate::utils::transcript::normalize_context;

/// `compat/extension-oauth-types.ts`（`oauth.ts` 的 7 个类型经由这里可达）。
pub mod extension_oauth_types;

// ---------------------------------------------------------------------------
// 再导出面（与 compat.ts 的 export * 顺序一致）
// ---------------------------------------------------------------------------

pub use crate::api::anthropic_messages::anthropic_messages_api;
pub use crate::api::azure_openai_responses::azure_openai_responses_api;
pub use crate::api::bedrock_converse_stream::{
    bedrock_converse_stream_api, set_bedrock_provider_module,
};
pub use crate::api::google_generative_ai::google_generative_ai_api;
pub use crate::api::google_vertex::google_vertex_api;
pub use crate::api::kiana_messages::kiana_messages_api;
pub use crate::api::mistral_conversations::mistral_conversations_api;
pub use crate::api::openai_codex_responses::openai_codex_responses_api;
pub use crate::api::openai_completions::openai_completions_api;
pub use crate::api::openai_responses::openai_responses_api;
pub use crate::env_api_keys::*;
pub use crate::image_models::*;
pub use crate::index::*;
pub use crate::legacy_api_aliases::*;
pub use crate::providers::images::*;

/// `kind of MODELS`：内置 provider id 的联合（TS 是字面量联合；Rust 保留开放字符串）。
///
/// TS: `providers/all.ts#BuiltinProvider`
pub type BuiltinProvider = String;

// ---------------------------------------------------------------------------
// 静态目录读取（@deprecated）
// ---------------------------------------------------------------------------

/// `getModel = getBuiltinModel`。
///
/// 返回静态目录中的模型副本；provider 或 model id 未知时 `None`（不查运行时注册表）。
///
/// @deprecated 用 `getBuiltinModel`（`providers/all`）或 `Models.getModel()`。
///
/// TS: `compat.ts#getModel`
pub fn get_model(provider: &str, model_id: &str) -> Option<Model> {
    crate::providers::all::get_builtin_model(provider, model_id)
}

/// `getModels = getBuiltinModels`。
///
/// 未知 provider 返回空 `Vec`（与 TS 的 `Object.values(MODELS[provider] ?? {})` 一致）。
///
/// @deprecated 用 `getBuiltinModels`（`providers/all`）或 `Models.getModels()`。
///
/// TS: `compat.ts#getModels`
pub fn get_models(provider: &str) -> Vec<Model> {
    crate::providers::all::get_builtin_models(provider)
}

/// `getProviders = getBuiltinProviders`。
///
/// 返回目录里 provider id 的固定顺序切片（顺序由 `providers/catalog` 锁定）。
///
/// @deprecated 用 `getBuiltinProviders`（`providers/all`）或 `Models.getProviders()`。
///
/// TS: `compat.ts#getProviders`
pub fn get_providers() -> &'static [String] {
    crate::providers::all::get_builtin_providers()
}

// ---------------------------------------------------------------------------
// api-registry
// ---------------------------------------------------------------------------

/// `ApiStreamFunction`：`(model, context, options?) => AssistantMessageEventStream`。
///
/// 同步返回流，签名没有 `Result`：实现内部的同步错误必须编码成流上的 `error` 事件
/// （[`lazy_stream_error`] 是现成的构造器）。TS 的可选 `options` 在 Rust 用传值 +
/// `Default` 表达。
pub type ApiStreamFunction = Arc<
    dyn Fn(&Model, &TranscriptContext, StreamOptions) -> AssistantMessageEventStream + Send + Sync,
>;

/// `ApiStreamSimpleFunction`。
///
/// 语义同 [`ApiStreamFunction`]，选项类型换成 [`SimpleStreamOptions`]。
pub type ApiStreamSimpleFunction = Arc<
    dyn Fn(&Model, &TranscriptContext, SimpleStreamOptions) -> AssistantMessageEventStream
        + Send
        + Sync,
>;

/// `ApiProvider`：一个 api id 与它的两个流函数。
///
/// 这是 [`register_api_provider`] 的输入；`api` 同时是注册表的 key 与调用时校验
/// `model.api` 的期望值。
///
/// TS: `compat.ts#ApiProvider`
#[derive(Clone)]
pub struct ApiProvider {
    /// `api`。
    pub api: Api,
    /// `stream`。
    pub stream: ApiStreamFunction,
    /// `streamSimple`。
    pub stream_simple: ApiStreamSimpleFunction,
}

impl ApiProvider {
    /// 由 [`ProviderStreams`] 实现构造（Rust 侧的常规入口）。
    ///
    /// 同一个 `Arc` 被克隆两次，分别装进两个函数对象，因此 `stream` 与 `stream_simple`
    /// 共享同一份实现（例如 `FauxCore` 的脚本队列）。`api` 与实现分开传入：注册时按它
    /// 建立注册表 key，并交给包装层做调用前的 `model.api` 校验。
    pub fn from_streams(api: impl Into<Api>, streams: Arc<dyn ProviderStreams>) -> Self {
        let stream_impl = Arc::clone(&streams);
        let simple_impl = Arc::clone(&streams);
        ApiProvider {
            api: api.into(),
            stream: Arc::new(move |model, context, options| {
                stream_impl.stream(model, context, options)
            }),
            stream_simple: Arc::new(move |model, context, options| {
                simple_impl.stream_simple(model, context, options)
            }),
        }
    }
}

/// `ApiProviderInternal`：包装后的注册项（`api` 不匹配检查在这里）。
///
/// 与 [`ApiProvider`] 的区别：两个函数字段已经过 `wrap_stream`/`wrap_stream_simple`
/// 包装，调用时会先比较 `model.api` 与注册时的 api；不匹配就返回 error 流，底层实现
/// 不会被调用。字段私有：外部只能经 [`register_api_provider`] 写入、[`get_api_provider`]
/// 取用（内置条目由内部注册流程产生）。
///
/// TS: `compat.ts#ApiProviderInternal`
#[derive(Clone)]
pub struct ApiProviderInternal {
    api: Api,
    stream: ApiStreamFunction,
    stream_simple: ApiStreamSimpleFunction,
}

impl ApiProviderInternal {
    /// `api`。
    pub fn api(&self) -> &str {
        &self.api
    }

    /// `stream(model, context, options)`。
    pub fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        (self.stream)(model, context, options)
    }

    /// `streamSimple(model, context, options)`。
    pub fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        (self.stream_simple)(model, context, options)
    }
}

/// `Mismatched api: ${model.api} expected ${api}`。
///
/// 归入 [`AiErrorKind::Other`]（TS 是普通 `Error`），与「api 未注册」使用的
/// [`AiErrorKind::Unsupported`] 区分：前者是实现与模型不匹配，后者是缺少实现。
fn mismatched_api(model: &Model, expected: &str) -> AiError {
    AiError::new(
        AiErrorKind::Other,
        format!("Mismatched api: {} expected {}", model.api, expected),
    )
}

/// `wrapStream(api, stream)`。
///
/// 返回的闭包必须 `'static`，因此把 `api` 复制成 owned `String` 捕获。每次调用先做
/// 字符串相等比较；不匹配时按模块文档的约定返回 [`lazy_stream_error`] 流（TS 在这里
/// 同步抛错）。
fn wrap_stream(api: &str, stream: ApiStreamFunction) -> ApiStreamFunction {
    let api = api.to_string();
    Arc::new(move |model, context, options| {
        if model.api != api {
            // TS 同步抛错；`ProviderStreams` 的冻结签名同步返回流，因此映射成 error 事件。
            let error = mismatched_api(model, &api);
            return lazy_stream_error(model, error);
        }
        stream(model, context, options)
    })
}

/// `wrapStreamSimple(api, streamSimple)`。
///
/// 与 `wrap_stream` 相同，只是选项类型为 [`SimpleStreamOptions`]。
fn wrap_stream_simple(
    api: &str,
    stream_simple: ApiStreamSimpleFunction,
) -> ApiStreamSimpleFunction {
    let api = api.to_string();
    Arc::new(move |model, context, options| {
        if model.api != api {
            // 同 `wrap_stream`：TS 同步抛错，这里映射成 error 事件。
            let error = mismatched_api(model, &api);
            return lazy_stream_error(model, error);
        }
        stream_simple(model, context, options)
    })
}

/// 注册表条目：包装后的实现 + 注册方标识。
///
/// `source_id` 是注册方可选提供的标识：内置 API 与「不需要单独注销」的注册都是 `None`。
/// [`unregister_api_providers`] 用 `Some(source_id)` 精确匹配，因此 `None` 条目不会被
/// 任何 sourceId 删除（对应 TS 的 `sourceId?: string` 与 `entry.sourceId === sourceId`）。
struct RegisteredApiProvider {
    provider: Arc<ApiProviderInternal>,
    source_id: Option<String>,
}

/// api-registry 的进程级状态。
///
/// - `providers` 用 `IndexMap`：迭代顺序等于首次注册顺序（JS `Map` 的模型）。
///   [`get_api_providers`] 的顺序与内置 API 的固定顺序都被测试锁定；对已有 key 再次
///   `insert` 只替换值、位置不变（与 `Map.set` 一致），所以覆盖不会打乱顺序。
/// - `builtin_instances` 保存最近一次内置注册后各 api 的实例（按 `Arc` 指针身份识别），
///   用于区分「仍是内置实现」与「已被扩展覆盖」，见 `is_builtin_api_instance`。
///
/// 锁约定：只能经 `lock_registry` 访问。`std::sync::Mutex` 不可重入，持锁时不得调用
/// [`register_api_provider`]/[`get_api_provider`]/[`reset_api_providers`] 等会再次
/// 加锁的函数。
struct RegistryState {
    providers: IndexMap<Api, RegisteredApiProvider>,
    builtin_instances: HashMap<Api, Arc<ApiProviderInternal>>,
}

impl RegistryState {
    fn empty() -> Self {
        RegistryState {
            providers: IndexMap::new(),
            builtin_instances: HashMap::new(),
        }
    }

    fn insert_provider(&mut self, provider: ApiProvider, source_id: Option<String>) {
        // 每次注册都新建包装层：包装本身无状态，重复包装只多一次 api 比较；新的
        // `Arc<ApiProviderInternal>` 也让 `is_builtin_api_instance` 的指针比较能识别
        // 「覆盖」。
        let api = provider.api.clone();
        let internal = Arc::new(ApiProviderInternal {
            api: api.clone(),
            stream: wrap_stream(&api, provider.stream),
            stream_simple: wrap_stream_simple(&api, provider.stream_simple),
        });
        // `IndexMap::insert`：key 已存在时只更新值并保留首次插入位置（`Map.set` 语义）。
        self.providers.insert(
            api,
            RegisteredApiProvider {
                provider: internal,
                source_id,
            },
        );
    }

    /// `registerBuiltInApiProviders()`：不覆盖已存在条目，但把当前实例记入实例表。
    ///
    /// 「已存在」分支服务于「测试/扩展先注册了内置 api 的覆盖」：此时不插入新的内置
    /// 实现，而是把**当前条目**记为内置实例（TS 同样是
    /// `builtinApiProviderInstances.set(api, getApiProvider(api))`）。因此函数结束后，
    /// 实例表里的引用就是随后 `is_builtin_api_instance` 要比对的对象。
    ///
    /// `expect` 的不变量：无论走插入还是跳过分支，此时 `providers` 里必有一条该 api 的
    /// 记录，所以不会触发。
    fn register_builtins(&mut self) {
        for (api, streams) in builtin_apis() {
            if !self.providers.contains_key(api) {
                self.insert_provider(ApiProvider::from_streams(api, streams), None);
            }
            let current = Arc::clone(
                &self
                    .providers
                    .get(api)
                    .expect("the builtin api was just registered")
                    .provider,
            );
            self.builtin_instances.insert(api.to_string(), current);
        }
    }
}

/// `BUILTIN_APIS`（顺序固定，`spec/ai-core.md` §2.1.11）。
///
/// 顺序是外部可见契约：`register_builtins` 按此顺序首次插入，[`get_api_providers`] 的
/// 返回顺序与测试 `builtin_apis_are_registered_in_the_fixed_order` 都依赖它。
///
/// 每次调用都会重新执行各 `*_api()` 工厂，返回的 `Arc` 不保证指针相等（若干工厂是
/// `lazy_api` 包装，`bedrock_converse_stream_api` 还可能返回 `set_bedrock_provider_module`
/// 注册的覆盖模块）。因此只在初始化/重置/显式注册内置 API 时调用它。
fn builtin_apis() -> Vec<(&'static str, Arc<dyn ProviderStreams>)> {
    vec![
        (
            "anthropic-messages",
            crate::api::anthropic_messages::anthropic_messages_api(),
        ),
        (
            "openai-completions",
            crate::api::openai_completions::openai_completions_api(),
        ),
        (
            "openai-responses",
            crate::api::openai_responses::openai_responses_api(),
        ),
        (
            "openai-codex-responses",
            crate::api::openai_codex_responses::openai_codex_responses_api(),
        ),
        (
            "azure-openai-responses",
            crate::api::azure_openai_responses::azure_openai_responses_api(),
        ),
        (
            "google-generative-ai",
            crate::api::google_generative_ai::google_generative_ai_api(),
        ),
        (
            "google-vertex",
            crate::api::google_vertex::google_vertex_api(),
        ),
        (
            "mistral-conversations",
            crate::api::mistral_conversations::mistral_conversations_api(),
        ),
        (
            "bedrock-converse-stream",
            crate::api::bedrock_converse_stream::bedrock_converse_stream_api(),
        ),
        (
            "kiana-messages",
            crate::api::kiana_messages::kiana_messages_api(),
        ),
    ]
}

/// 进程级 api-registry。
///
/// TS 的 `compat.ts` 在模块顶层同步注册内置 API；Rust 没有模块加载期副作用，改用
/// `LazyLock`：首次解引用时构造并注册。所有访问注册表的公开入口（[`register_api_provider`]、
/// [`get_api_provider`]、[`get_api_providers`]、[`unregister_api_providers`]、
/// [`register_built_in_api_providers`]、[`reset_api_providers`]、[`stream`]、
/// [`stream_simple`]、[`complete`]、[`complete_simple`]、[`register_faux_provider`]）
/// 第一次调用都会先触发它，因此「先注册内置 API」这一顺序仍然成立（见模块文档）。
/// 静态目录查询 [`get_model`]、[`get_models`]、[`get_providers`] 只调用 `providers::all`
/// （最终读 `providers::catalog`），不访问本静态量，因此不会触发初始化。
static REGISTRY: LazyLock<Mutex<RegistryState>> = LazyLock::new(|| {
    let mut state = RegistryState::empty();
    // 对应 TS 的模块加载副作用 1/2：先把 10 个内置 API 注册进注册表（TS 源码第 215 行）。
    state.register_builtins();
    Mutex::new(state)
});

/// 取注册表锁；poison 时取回内部值继续使用。
///
/// 持锁线程 panic 会让 `std::sync::Mutex` 进入 poison 状态；注册表只是进程内索引，
/// 这里选择忽略 poison（TS 的 `Map` 没有这个概念），代价是 panic 时可能留下不完整的
/// 注册状态。`std::sync::Mutex` 不可重入：持 guard 时不要调用会再次加锁的公开函数。
fn lock_registry() -> MutexGuard<'static, RegistryState> {
    REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `registerApiProvider(provider, sourceId?)`：后注册覆盖前者。
///
/// 覆盖只按 `provider.api` 匹配；`source_id` 不参与匹配，只供
/// [`unregister_api_providers`] 批量注销。重复注册同一 api 时迭代位置保持不变
/// （见 `RegistryState` 的说明）。包装在锁内完成，流函数本身在锁外调用。
///
/// TS: `compat.ts#registerApiProvider`
pub fn register_api_provider(provider: ApiProvider, source_id: Option<String>) {
    lock_registry().insert_provider(provider, source_id);
}

/// `getApiProvider(api)`。
///
/// 返回包装后的 [`ApiProviderInternal`]（不是底层 `ProviderStreams`）；`None` 表示该
/// api 未注册或已被注销。
///
/// TS: `compat.ts#getApiProvider`
pub fn get_api_provider(api: &str) -> Option<Arc<ApiProviderInternal>> {
    lock_registry()
        .providers
        .get(api)
        .map(|entry| Arc::clone(&entry.provider))
}

/// `getApiProviders()`。
///
/// 顺序为首次注册顺序：初始化后前 10 个是内置 API（按 `builtin_apis()` 的顺序），
/// 之后是自定义注册项。调用方不应依赖该顺序做分派，但测试用它锁定内置注册顺序。
///
/// TS: `compat.ts#getApiProviders`
pub fn get_api_providers() -> Vec<Arc<ApiProviderInternal>> {
    lock_registry()
        .providers
        .values()
        .map(|entry| Arc::clone(&entry.provider))
        .collect()
}

/// `unregisterApiProviders(sourceId)`：按 sourceId 删除（未带 sourceId 的条目保留）。
///
/// 先在锁内收集待删 key，再逐个删除，避免在迭代中修改 map。使用 `shift_remove` 而非
/// `swap_remove`：后者会把末位条目填进空位，破坏「注册顺序即迭代顺序」的契约。
///
/// TS: `compat.ts#unregisterApiProviders`
pub fn unregister_api_providers(source_id: &str) {
    let mut state = lock_registry();
    let keys: Vec<Api> = state
        .providers
        .iter()
        .filter(|(_, entry)| entry.source_id.as_deref() == Some(source_id))
        .map(|(api, _)| api.clone())
        .collect();
    for api in keys {
        state.providers.shift_remove(&api);
    }
}

/// `registerBuiltInApiProviders()`：**不覆盖**已存在的条目（测试/扩展可能已注册覆盖）。
///
/// 可在任意时刻调用：缺失的内置 api 被插入，已存在的条目保持原实现；两种情况下都会
/// 用当前条目刷新实例表。
///
/// TS: `compat.ts#registerBuiltInApiProviders`
pub fn register_built_in_api_providers() {
    lock_registry().register_builtins();
}

/// `resetApiProviders()`：清空注册表与实例表，再重新注册内置 API。
///
/// 自定义 api 与对内置 api 的覆盖都会被丢弃，调用方需重新注册；返回后的注册表顺序与
/// 进程刚初始化时一致（测试的 afterEach 依赖这一点）。
///
/// TS: `compat.ts#resetApiProviders`
pub fn reset_api_providers() {
    let mut state = lock_registry();
    state.providers.clear();
    state.builtin_instances.clear();
    state.register_builtins();
}

// ---------------------------------------------------------------------------
// registerFauxProvider
// ---------------------------------------------------------------------------

/// `FauxProviderRegistration`：`registerFauxProvider()` 的返回值。
///
/// 持有创建时的 [`FauxCore`] 与注册用的 `source_id`。脚本/状态操作都直接作用在 `core`
/// 上；注册表里的流实现是 `core.clone()` 出来的另一份句柄，但内部队列、状态、模型列表
/// 都是 `Arc` 共享，因此两边看到同一份数据。
///
/// TS: `providers/faux.ts#FauxProviderRegistration`
pub struct FauxProviderRegistration {
    core: FauxCore,
    source_id: String,
}

impl FauxProviderRegistration {
    /// `api`。
    pub fn api(&self) -> &str {
        self.core.api()
    }

    /// `models`。
    pub fn models(&self) -> &[Model] {
        self.core.models()
    }

    /// `getModel(id?)`。
    pub fn get_model(&self, requested_model_id: Option<&str>) -> Option<&Model> {
        self.core.get_model(requested_model_id)
    }

    /// `state`。
    pub fn state(&self) -> &FauxProviderState {
        self.core.state()
    }

    /// `setResponses(responses)`。
    pub fn set_responses(&self, responses: Vec<FauxResponseStep>) {
        self.core.set_responses(responses);
    }

    /// `appendResponses(responses)`。
    pub fn append_responses(&self, responses: Vec<FauxResponseStep>) {
        self.core.append_responses(responses);
    }

    /// `getPendingResponseCount()`。
    pub fn get_pending_response_count(&self) -> usize {
        self.core.get_pending_response_count()
    }

    /// `unregister()`：按注册时生成的 sourceId 删除该 api 条目。
    ///
    /// 幂等：再次调用时没有条目匹配该 sourceId，什么也不做；底层 `FauxCore` 仍可用，
    /// 只是不能再经注册表分派到它。
    pub fn unregister(&self) {
        unregister_api_providers(&self.source_id);
    }
}

/// `Math.random().toString(36).slice(2, 10)`：8 个 base36 字符。
///
/// TS 用 `Math.random()`；Rust 用 `getrandom` 的 8 个字节对 36 取模映射到 `0-9a-z`。
/// 取模有轻微偏差，但该值只用于生成进程内唯一的 sourceId，不参与安全判定。
fn random_base36_suffix() -> String {
    const ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        // 随机源不可用时退回时间戳混合（sourceId 只需在进程内唯一）。
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos() as u64);
        bytes = nanos.to_le_bytes();
    }
    bytes
        .iter()
        .map(|byte| ALPHABET[*byte as usize % ALPHABET.len()] as char)
        .collect()
}

/// `registerFauxProvider(options = {})`。
///
/// 创建脚本化 faux 核心并注册到 api-registry；返回的注册句柄可以修改脚本队列、
/// 读取状态并 [`unregister`](FauxProviderRegistration::unregister)。
///
/// 注册表的 key 是 `core.api()`（默认 `faux:<毫秒时间戳>:<随机串>`，每次注册唯一），
/// 而 `source_id` 是另一条 `faux-provider-<8 字符>` 随机串，仅用于注销。两条流入口是
/// 同一个 `core.clone()` 的 `Arc`，因此注册后对 `registration` 的修改立刻影响请求。
///
/// TS: `compat.ts#registerFauxProvider`
pub fn register_faux_provider(options: RegisterFauxProviderOptions) -> FauxProviderRegistration {
    let core = create_faux_core(options);
    let source_id = format!("faux-provider-{}", random_base36_suffix());
    let api = core.api().to_string();
    let streams: Arc<dyn ProviderStreams> = Arc::new(core.clone());
    register_api_provider(
        ApiProvider::from_streams(&api, streams),
        Some(source_id.clone()),
    );
    FauxProviderRegistration { core, source_id }
}

// ---------------------------------------------------------------------------
// 分派
// ---------------------------------------------------------------------------

/// `AMBIENT_AUTH_MARKER`。
///
/// [`crate::env_api_keys::get_env_api_key`] 对 google-vertex（ADC 文件 + project +
/// location 齐备）与 amazon-bedrock（ambient 凭证链）返回该字面量，表示「凭证由适配器
/// 自己解析」。它不是可发送的 key：写入 `options.api_key` 会把 `<authenticated>` 当
/// 真实密钥发给服务商，因此下面两个 `with_env_api_key*` 都显式跳过它。
const AMBIENT_AUTH_MARKER: &str = "<authenticated>";

/// 模块加载副作用 2/2：`const compatModels = builtinModels()`（TS 源码第 217 行）。
///
/// Rust 的 `LazyLock` 在首次访问 `compat_models` 时才构造（不是进程加载时）；顺序保证
/// 见模块文档：访问它的分支之前一定先读 `REGISTRY`，而 `REGISTRY` 初始化时已注册内置
/// API。`builtin_models(None)` 会注册全部 42 个内置 provider。
static COMPAT_MODELS: LazyLock<Arc<dyn MutableModels>> = LazyLock::new(|| builtin_models(None));

fn compat_models() -> &'static Arc<dyn MutableModels> {
    &COMPAT_MODELS
}

/// `hasExplicitApiKey(k)`：`typeof k === "string" && k.trim().length > 0`。
fn has_explicit_api_key(api_key: Option<&str>) -> bool {
    api_key.is_some_and(|key| !key.trim().is_empty())
}

/// `withEnvApiKey(model, options)`。
///
/// 调用方已给出非空 `api_key` 时原样返回；否则按 `model.provider` 查环境变量
/// （`options.env` 优先于进程环境，见 `env_api_keys`）。命中 [`AMBIENT_AUTH_MARKER`] 时
/// 视为没有 key（凭证由适配器解析），其余非空值写入 `api_key`。
fn with_env_api_key(model: &Model, mut options: StreamOptions) -> StreamOptions {
    if has_explicit_api_key(options.api_key.as_deref()) {
        return options;
    }
    match crate::env_api_keys::get_env_api_key(&model.provider, options.env.as_ref()) {
        Some(api_key) if api_key != AMBIENT_AUTH_MARKER => {
            options.api_key = Some(api_key);
            options
        }
        _ => options,
    }
}

/// `withEnvApiKey` 的 `SimpleStreamOptions` 形态（TS 的泛型 `TOptions`）。
///
/// 只改 `options.base`，`reasoning`/`tool_choice`/`deferred` 等字段原样保留；返回的是
/// 修改后的 owned 值（TS 是 `{ ...options, apiKey }` 浅拷贝）。
fn with_env_api_key_simple(model: &Model, mut options: SimpleStreamOptions) -> SimpleStreamOptions {
    if has_explicit_api_key(options.base.api_key.as_deref()) {
        return options;
    }
    match crate::env_api_keys::get_env_api_key(&model.provider, options.base.env.as_ref()) {
        Some(api_key) if api_key != AMBIENT_AUTH_MARKER => {
            options.base.api_key = Some(api_key);
            options
        }
        _ => options,
    }
}

/// `hasResolvedCloudflareAuth(options)`：显式 apiKey，或 `headers["cf-aig-authorization"]`
/// 是字符串。
///
/// `options.headers` 的类型是 `Option<ProviderHeaders>`，而
/// [`crate::types::ProviderHeaders`] 是 `IndexMap<String, Option<String>>`；
/// `headers.as_ref().and_then(|h| h.get("cf-aig-authorization"))` 得到
/// `Option<&Option<String>>`：外层 `Some` 表示 headers 中存在该键，内层 `None` 表示调用方
/// 显式「抑制」它（见 [`crate::models::merge_headers`]）。TS 的 `typeof value === "string"`
/// 因此对应 `is_some_and(Option::is_some)`——被抑制的头不算已解析鉴权。空白 apiKey 也不算
/// （`has_explicit_api_key` 先 trim）。
fn has_resolved_cloudflare_auth(options: &StreamOptions) -> bool {
    has_explicit_api_key(options.api_key.as_deref())
        || options
            .headers
            .as_ref()
            .and_then(|headers| headers.get("cf-aig-authorization"))
            .is_some_and(Option::is_some)
}

/// 当前注册项是否仍是内置实例。
///
/// 用 `Arc::ptr_eq` 比较指针身份（对应 TS 的 `!==` 对象引用比较）。两次取锁分成两条
/// 语句：`get_api_provider` 内部会再次加锁，`std::sync::Mutex` 不可重入，若在持 guard
/// 的表达式里调用会死锁或 panic。
///
/// `(None, None)` 返回 `true`：两次查询都为空表示该 api 从未注册，视为“未被替换”：
/// 对从未注册过的 api，两次查询都为空，此函数返回 true，随后由 `compatModels` 里是否
/// 有该 provider 决定最终结果。
fn is_builtin_api_instance(api: &str) -> bool {
    let builtin = lock_registry().builtin_instances.get(api).cloned();
    let current = get_api_provider(api);
    match (current, builtin) {
        (Some(current), Some(builtin)) => Arc::ptr_eq(&current, &builtin),
        // TS 的 `undefined !== undefined` 为 false（都有/都没有都算「相同」）。
        (None, None) => true,
        _ => false,
    }
}

/// `getBuiltinProviderForModel(model)`：仅当当前 api 实现仍是内置实例、且该模型的 provider
/// 在 `compatModels` 里存在同 api 的模型时返回该 provider。
///
/// 两道门分别对应两种回退：
/// 1. 注册表条目已被覆盖（`Arc` 指针不等）→ 返回 `None`，`stream` 改走注册表里的覆盖
///    实现；
/// 2. provider 不在内置目录，或目录里没有同 api 的模型（自定义模型）→ 返回 `None`，
///    同样走注册表。
///
/// 命中的快路径直接调用 provider 的 `stream`，**不经过** `Models` 的 credential store
/// 鉴权，只由调用方注入环境变量 key（与 TS 的 `builtinProvider.stream(...)` 一致）。
fn get_builtin_provider_for_model(model: &Model) -> Option<Arc<dyn Provider>> {
    if !is_builtin_api_instance(&model.api) {
        return None;
    }
    let provider = compat_models().get_provider(&model.provider)?;
    provider
        .get_models()
        .iter()
        .any(|candidate| candidate.api == model.api)
        .then_some(provider)
}

/// `resolveApiProvider(api)`：缺失时返回
/// `No API provider registered for api: ${api}`。
///
/// 用 [`AiErrorKind::Unsupported`] 表示「没有该 api 的实现」，与 `mismatched_api` 的
/// [`AiErrorKind::Other`] 区分；错误由 [`stream`]/[`stream_simple`] 转成 error 流。
#[allow(clippy::result_large_err)] // `AiError` 是共享契约类型，与 TS 的 `Error` 一一对应。
fn resolve_api_provider(api: &str) -> Result<Arc<ApiProviderInternal>, AiError> {
    get_api_provider(api).ok_or_else(|| {
        AiError::new(
            AiErrorKind::Unsupported,
            format!("No API provider registered for api: {api}"),
        )
    })
}

/// `stream(model, context, options?)`：旧全局分派。
///
/// 内置 provider 走 `compatModels`/provider 实现（含环境变量 key 注入与 Cloudflare 分支），
/// 否则走 api-registry。
///
/// 流程：
/// 1. `normalize_context` 把 system prompt/tools 收敛进消息列表，得到
///    [`TranscriptContext`]；
/// 2. `get_builtin_provider_for_model` 命中 → 内置快路径：Cloudflare provider 在鉴权
///    尚未解析时转交 `Models::stream`，其余直接调 provider 的 `stream` 并注入 env key；
/// 3. 未命中 → 取注册表实现；缺失时返回 error 流（见模块文档中与 TS 的差异）。
///
/// 快路径不经过 `Models` 的 credential store 鉴权，注册在凭证存储里的 OAuth/API key
/// 不会在这条路径生效（TS 行为相同）。
///
/// TS: `compat.ts#stream`
pub fn stream(
    model: &Model,
    context: &Context,
    options: StreamOptions,
) -> AssistantMessageEventStream {
    let transcript = normalize_context(context);
    if let Some(provider) = get_builtin_provider_for_model(model) {
        // `hasResolvedCloudflareAuth` 判断的是**注入前**的 options（TS 同样如此）。
        // 转交 `Models::stream` 是为了复用它完整的 auth 解析（credential store +
        // provider auth + header 合并）；传 `as_context()` 是因为该入口收 `&Context`，
        // 而 `normalize_context` 对已归一化上下文是幂等的（system_prompt/tools 已为 None）。
        if model.provider.starts_with("cloudflare-") && !has_resolved_cloudflare_auth(&options) {
            return compat_models().stream(model, transcript.as_context(), options.into());
        }
        // 快路径：直接调 provider，只注入环境变量 key，不经过 Models 的凭证解析。
        return provider.stream(model, &transcript, with_env_api_key(model, options));
    }
    match resolve_api_provider(&model.api) {
        Ok(provider) => provider.stream(model, &transcript, with_env_api_key(model, options)),
        Err(error) => lazy_stream_error(model, error),
    }
}

/// `streamSimple(model, context, options?)`。
///
/// 与 [`stream`] 相同的两条路径；Cloudflare 判断与 env key 注入读取的是 `options.base`，
/// 其余字段（`reasoning`、`tool_choice`…）原样传递给实现。
///
/// TS: `compat.ts#streamSimple`
pub fn stream_simple(
    model: &Model,
    context: &Context,
    options: SimpleStreamOptions,
) -> AssistantMessageEventStream {
    let transcript = normalize_context(context);
    if let Some(provider) = get_builtin_provider_for_model(model) {
        if model.provider.starts_with("cloudflare-") && !has_resolved_cloudflare_auth(&options.base)
        {
            // 同 `stream`：转交 Models 层做凭证解析；已归一化上下文再归一化无副作用。
            return compat_models().stream_simple(model, transcript.as_context(), options.into());
        }
        return provider.stream_simple(model, &transcript, with_env_api_key_simple(model, options));
    }
    match resolve_api_provider(&model.api) {
        Ok(provider) => {
            provider.stream_simple(model, &transcript, with_env_api_key_simple(model, options))
        }
        Err(error) => lazy_stream_error(model, error),
    }
}

/// `stream(...).result()` 的 Rust 形态（与 `Models::complete` 相同的兜底）。
///
/// [`AssistantMessageEventStream::result`] 的 `Err` 只在结果通道的发送端被丢弃时出现；
/// `result_tx`（`watch::Sender`）保存在流的 `Shared` 里，`result(&self)` 在等待期间持有
/// 流句柄，因此持有句柄时该分支不可达。用 `end_without_result()` 结束的流会让 `result()`
/// 永远 pending（与 TS 的 `finalResultPromise` 一致），`complete` 同样挂起；这里的合成
/// 消息并不覆盖该情况，只是防御性兜底。若真的走到 `Err`，错误文本与 `Models::complete`
/// 的兜底相同（两处需保持一致）。
async fn stream_result(stream: AssistantMessageEventStream, model: &Model) -> AssistantMessage {
    match stream.result().await {
        Ok(message) => message,
        Err(_) => crate::api::lazy::create_setup_error_message(
            model,
            &AiError::new(
                AiErrorKind::Other,
                "assistant message event stream ended without a result",
            ),
            now_ms(),
        ),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

/// `complete(model, context, options?)`：等待 `stream()` 的结果。
///
/// 返回 `AssistantMessage` 而不是 `Result`：鉴权/注册失败被编码为
/// `stop_reason: Error` 的消息（`error_message` 带原因），调用方必须检查该字段。
///
/// TS: `compat.ts#complete`
pub async fn complete(
    model: &Model,
    context: &Context,
    options: StreamOptions,
) -> AssistantMessage {
    stream_result(stream(model, context, options), model).await
}

/// `completeSimple(model, context, options?)`。
///
/// 与 [`complete`] 一样把错误编码在消息里（`stop_reason: Error`），不返回 `Result`。
///
/// TS: `compat.ts#completeSimple`
pub async fn complete_simple(
    model: &Model,
    context: &Context,
    options: SimpleStreamOptions,
) -> AssistantMessage {
    stream_result(stream_simple(model, context, options), model).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 注册表是进程全局状态；`cargo test` 默认并行，涉及注册/重置的用例必须串行化。
    /// 用 `tokio::sync::Mutex` 让 async 测试跨 `await` 持锁（`std` 的 guard 不是 `Send`，
    /// 且会触发 clippy 的 `await_holding_lock`）。
    static REGISTRY_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn lock_registry_tests() -> &'static tokio::sync::Mutex<()> {
        &REGISTRY_TEST_LOCK
    }
    use crate::types::{
        AssistantMessageEvent, AssistantRole, DoneReason, ModelCost, ModelInputModality,
        StopReason, Usage, UsageCost,
    };

    fn model(api: &str, provider: &str) -> Model {
        Model {
            id: "test-model".into(),
            name: "Test Model".into(),
            api: api.into(),
            provider: provider.into(),
            base_url: "https://example.test/v1".into(),
            reasoning: Some(false),
            input: vec![ModelInputModality::Text],
            cost: ModelCost::default(),
            context_window: Some(128_000),
            max_tokens: Some(4096),
            ..Model::default()
        }
    }

    fn ok_message(model: &Model, text: &str) -> AssistantMessage {
        AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![crate::types::AssistantContentBlock::Text(
                crate::types::TextContent {
                    kind_text: crate::types::TextKind::Text,
                    text: text.into(),
                    text_signature: None,
                },
            )],
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            usage: Usage {
                cost: UsageCost::default(),
                ..Usage::default()
            },
            stop_reason: StopReason::Stop,
            timestamp: now_ms(),
            ..AssistantMessage::default()
        }
    }

    fn ok_stream(text: &'static str) -> ApiStreamFunction {
        Arc::new(move |model, _context, _options| {
            let stream = AssistantMessageEventStream::new();
            let output = ok_message(model, text);
            let _ = stream.push(AssistantMessageEvent::Start {
                partial: Box::new(output.clone()),
            });
            let _ = stream.push(AssistantMessageEvent::Done {
                reason: DoneReason::Stop,
                message: Box::new(output.clone()),
            });
            stream.end(output);
            stream
        })
    }

    /// TS: `compat-env.test.ts`。
    #[tokio::test]
    async fn dispatches_unknown_providers_through_the_legacy_api_registry() {
        // 注册表是进程全局状态：不持锁时，并发用例的 `reset_api_providers()` 会在
        // `register_api_provider` 与本用例分派之间抹掉覆盖项（曾导致 None != Some(Some(...)))。
        let _guard = lock_registry_tests().lock().await;
        let captured: Arc<Mutex<Option<Option<String>>>> = Arc::new(Mutex::new(None));
        let captured_by_stream = Arc::clone(&captured);
        let stream_fn: ApiStreamFunction = Arc::new(move |model, _context, options| {
            *captured_by_stream.lock().unwrap() = Some(options.api_key.clone());
            ok_stream("ok")(model, _context, options)
        });
        let captured_by_simple = Arc::clone(&captured);
        let simple_fn: ApiStreamSimpleFunction = Arc::new(move |model, _context, options| {
            *captured_by_simple.lock().unwrap() = Some(options.base.api_key.clone());
            ok_stream("ok")(model, _context, options.base)
        });
        register_api_provider(
            ApiProvider {
                api: "openai-responses".into(),
                stream: stream_fn,
                stream_simple: simple_fn,
            },
            None,
        );

        let model = model("openai-responses", "custom-openai");
        let context = Context::default();
        let _ = complete(
            &model,
            &context,
            StreamOptions {
                api_key: Some("request-key".into()),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            *captured.lock().unwrap(),
            Some(Some("request-key".to_string()))
        );

        // TS 的 afterEach：恢复内置注册表。
        reset_api_providers();
    }

    #[test]
    fn explicit_api_key_survives_with_env_api_key() {
        let mut env = crate::types::ProviderEnv::new();
        env.insert("OPENAI_API_KEY".into(), "env-key".into());
        let options = StreamOptions {
            api_key: Some("explicit".into()),
            env: Some(env),
            ..Default::default()
        };
        let result = with_env_api_key(&model("openai-responses", "openai"), options);
        assert_eq!(result.api_key.as_deref(), Some("explicit"));
    }

    #[test]
    fn blank_api_key_is_not_explicit() {
        assert!(!has_explicit_api_key(Some("")));
        assert!(!has_explicit_api_key(Some("   ")));
        assert!(!has_explicit_api_key(None));
        assert!(has_explicit_api_key(Some("k")));
    }

    #[test]
    fn env_api_key_is_injected_when_absent() {
        let mut env = crate::types::ProviderEnv::new();
        env.insert("OPENAI_API_KEY".into(), "env-key".into());
        let options = StreamOptions {
            env: Some(env),
            ..Default::default()
        };
        let result = with_env_api_key(&model("openai-responses", "openai"), options);
        assert_eq!(result.api_key.as_deref(), Some("env-key"));
    }

    #[test]
    fn ambient_auth_marker_is_not_written_as_an_api_key() {
        // google-vertex 在 ADC 文件 + project + location 齐备时返回字面量 "<authenticated>"。
        let dir = std::env::temp_dir().join(format!("kiana-compat-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let credentials = dir.join("adc.json");
        std::fs::write(&credentials, "{}").expect("write adc");
        let mut env = crate::types::ProviderEnv::new();
        env.insert(
            "GOOGLE_APPLICATION_CREDENTIALS".into(),
            credentials.to_string_lossy().into_owned(),
        );
        env.insert("GOOGLE_CLOUD_PROJECT".into(), "p".into());
        env.insert("GOOGLE_CLOUD_LOCATION".into(), "us-central1".into());
        let options = StreamOptions {
            env: Some(env),
            ..Default::default()
        };
        let result = with_env_api_key(&model("google-vertex", "google-vertex"), options);
        assert_eq!(result.api_key, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn has_resolved_cloudflare_auth_checks_the_header() {
        let mut headers = crate::types::ProviderHeaders::new();
        headers.insert("cf-aig-authorization".into(), Some("Bearer x".into()));
        assert!(has_resolved_cloudflare_auth(&StreamOptions {
            headers: Some(headers),
            ..Default::default()
        }));
        let mut headers = crate::types::ProviderHeaders::new();
        headers.insert("cf-aig-authorization".into(), None);
        assert!(!has_resolved_cloudflare_auth(&StreamOptions {
            headers: Some(headers),
            ..Default::default()
        }));
        assert!(!has_resolved_cloudflare_auth(&StreamOptions::default()));
    }

    #[test]
    fn builtin_apis_are_registered_in_the_fixed_order() {
        let apis = get_api_providers()
            .iter()
            .map(|provider| provider.api().to_string())
            .collect::<Vec<_>>();
        let expected: Vec<String> = [
            "anthropic-messages",
            "openai-completions",
            "openai-responses",
            "openai-codex-responses",
            "azure-openai-responses",
            "google-generative-ai",
            "google-vertex",
            "mistral-conversations",
            "bedrock-converse-stream",
            "kiana-messages",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        assert_eq!(apis, expected);
    }

    #[tokio::test]
    async fn missing_api_provider_becomes_an_error_stream() {
        let model = model("no-such-api", "no-such-provider");
        let message = complete(&model, &Context::default(), StreamOptions::default()).await;
        assert_eq!(message.stop_reason, StopReason::Error);
        assert_eq!(
            message.error_message.as_deref(),
            Some("No API provider registered for api: no-such-api")
        );
    }

    #[tokio::test]
    async fn mismatched_api_is_reported_by_the_wrapper() {
        let _guard = lock_registry_tests().lock().await;
        // 注册一个 api 与模型不匹配的实现：wrapper 的检查在直接调用时生效。
        register_api_provider(
            ApiProvider {
                api: "openai-responses".into(),
                stream: ok_stream("ok"),
                stream_simple: Arc::new(|model, _context, options| {
                    ok_stream("ok")(model, _context, options.base)
                }),
            },
            Some("mismatch-test".into()),
        );
        let provider = get_api_provider("openai-responses").expect("registered");
        let other = model("anthropic-messages", "anthropic");
        let message = provider
            .stream(
                &other,
                &TranscriptContext::from_normalized(Context::default()),
                StreamOptions::default(),
            )
            .result()
            .await
            .expect("error stream ends");
        assert_eq!(message.stop_reason, StopReason::Error);
        assert_eq!(
            message.error_message.as_deref(),
            Some("Mismatched api: anthropic-messages expected openai-responses")
        );
        reset_api_providers();
    }

    #[test]
    fn unregister_removes_only_the_matching_source() {
        let _guard = lock_registry_tests().blocking_lock();
        register_api_provider(
            ApiProvider {
                api: "custom-a".into(),
                stream: ok_stream("a"),
                stream_simple: Arc::new(|model, _context, options| {
                    ok_stream("a")(model, _context, options.base)
                }),
            },
            Some("source-a".into()),
        );
        assert!(get_api_provider("custom-a").is_some());
        unregister_api_providers("source-a");
        assert!(get_api_provider("custom-a").is_none());
        // 内置条目没有 sourceId，任何 unregister 都不会删除它们。
        unregister_api_providers("source-a");
        assert!(get_api_provider("anthropic-messages").is_some());
    }

    #[tokio::test]
    async fn register_faux_provider_routes_through_the_registry_and_unregisters() {
        let _guard = lock_registry_tests().lock().await;
        let registration = register_faux_provider(RegisterFauxProviderOptions::default());
        let api = registration.api().to_string();
        // TS: `sourceId = faux-provider-*` 只是注册表的 sourceId；公开的 `api` 是
        // `core.api`（`faux:<timestamp>:<random>`，每次注册唯一）。
        assert!(api.starts_with("faux:"), "unexpected api: {api}");
        let model = registration.get_model(None).expect("default model").clone();
        registration.set_responses(vec![crate::providers::faux::FauxResponseStep::from(
            crate::providers::faux::faux_assistant_message(
                "hello",
                crate::providers::faux::FauxAssistantMessageOptions::default(),
            ),
        )]);
        let message = complete(&model, &Context::default(), StreamOptions::default()).await;
        assert_eq!(message.stop_reason, StopReason::Stop);
        registration.unregister();
        assert!(get_api_provider(&api).is_none());
        let message = complete(&model, &Context::default(), StreamOptions::default()).await;
        assert_eq!(message.stop_reason, StopReason::Error);
        assert_eq!(
            message.error_message.as_deref(),
            Some(format!("No API provider registered for api: {api}").as_str())
        );
    }
}
