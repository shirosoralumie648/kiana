//! extensions/llama/provider.ts。
//!
//! 原生 `llama.cpp` provider（router 模式）：动态模型目录、api-key auth、`/models` 刷新、
//! chat 与 classifier 模型映射。TS 的 `Provider`/`ApiKeyAuth` 接口在 Rust 是
//! [`kiana_ai::models::Provider`] 与 [`kiana_ai::auth::types::ApiKeyAuth`]（spec §2.17、§4.9）。
//!
//! 关键映射（spec §2.17.5/§2.17.6）：
//! - 可选模型 = `loaded` | `sleeping` | （router autoload 时）未加载且未失败的 `preset`；
//! - chat 模型 `baseUrl = <server>/v1`，classifier 模型在非 decision 时用服务器根；
//! - decision 模型（`output_modalities` 含 `"decisions"`）走 `typesafe-system-one`，
//!   输出文本的同一模型同时是 chat 模型；
//! - 思考支持只从**已加载**模型的 `chat_template` 推断（含 `enable_thinking`）。
//!
//! # 在 crate 中的位置
//!
//! `llama::index::llama_extension` 在注册扩展时调用 [`create_llama_provider`]，把
//! [`LlamaProviderController::provider`] 注册为 native provider；`/llama` 管理器每次重读目录后
//! 调用 [`LlamaProviderController::set_catalog`] 同步内存目录。`kiana-ai` 的 `Models` 在刷新时
//! 调用 `Provider::refresh_models`，在登录/解析凭证时调用 `auth()` 返回的 `ApiKeyAuth`。
//!
//! # 执行流程
//!
//! `refresh_models` 分两个阶段，共享同一张“上次已知上下文窗口”表：
//!
//! 1. **缓存恢复**（不联网）：过滤 `context.stored` 中本 provider 的条目，恢复 chat
//!    （`Chat` 类型 + `openai-completions`）与 classifier 两个列表，以 `persist: Unchanged`
//!    发布（只更新内存，不写盘）；发布返回 `Ok(false)` 时直接结束。
//! 2. **联网刷新**（`allow_network`、未中止且凭证是 api-key 三者都成立时）：`GET /models`
//!    拉目录，仅当目录里有未加载 preset 时 `GET /props` 判断 router autoload；再对每个可选的
//!    **已加载** chat 模型并发 `GET /props?model=…` 读取 chat template；最后以
//!    `persist: Entry` 发布并持久化。
//!
//! 读路径（`get_models`/`get_all_models`）每次在锁内克隆快照；写路径（`set_catalog` 与两次
//! publish 的 `update` 闭包）整表替换两个 `Vec`，不做增量合并。
//!
//! # 注意事项
//!
//! - **目录为空是正常初始状态**：provider 创建后尚未刷新/配置时 `get_models()` 返回空，不是
//!   错误；“是否已配置”由 `ApiKeyAuth` 的 `check`/`resolve` 判断。
//! - **联网阶段只认凭据里的服务器地址**：`refresh_models` 从凭证的
//!   `env.LLAMA_BASE_URL` 构造客户端，不读 `AuthContext::env`，也不使用
//!   `RefreshModelsContext::base_url`。`AuthContext::env` 的回退只出现在
//!   `check`/`resolve`；`login` 另有基于进程环境变量的默认值。
//! - **发布返回 `Ok(false)` 表示本次刷新已被取代或取消**：缓存恢复阶段据此停止、不再联网；
//!   最终发布不检查返回值（其后没有更多工作），但 `Err` 仍会向 `Models` 传播。
//! - **互斥锁不跨 `await`**：只在同步的克隆/整表替换期间持有；锁中毒时取回内部值继续使用。
//! - 同一个模型 id 会同时出现在 chat 与 classifier 两个列表中（`api` 不同），缓存上下文窗口
//!   按 id 合并。

// `AiError` 是 `kiana-ai` 的共享错误契约（128 字节）；本模块的公开签名由 TS 的
// `Promise`/`throw` 语义直接映射而来，不装箱（与 `kiana-config`/`kiana-ai` 的同类模块一致）。
#![allow(clippy::result_large_err)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthCheck, AuthContext, AuthError,
    AuthInteraction, AuthPrompt, AuthPromptRequest, AuthResult, AuthType, ModelAuth, ProviderAuth,
};
use kiana_ai::error::AiError;
use kiana_ai::models::ModelsStoreEntry;
use kiana_ai::models::{
    ClassifierOptions, ModelsPublication, ModelsPublicationPersist, Provider, ProviderClassifier,
    RefreshModelsContext,
};
use kiana_ai::types::{
    AnyModel, ClassifierContext, ClassifierResult, Model, ModelCost, ModelInputModality,
    ModelThinkingLevel, ModelType, SimpleStreamOptions, StreamOptions, ThinkingLevelMap,
    TranscriptContext,
};
use kiana_ai::utils::abort::AbortScope;
use serde_json::Value;

use super::client::{
    LlamaClient, LlamaModelInfo, LlamaServerProps, default_http_client, llama_inference_url,
    normalize_llama_server_url,
};

/// `LLAMA_PROVIDER_ID`。
///
/// 注册到 `kiana-ai` `Models` 的 provider id（`"llama.cpp"`）。同时是凭证存储与持久化目录的
/// 键：`refresh_models` 只恢复 `model.provider` 等于该值的条目，因此改字面量会让已有缓存
/// 目录失效；字面量是当前稳定契约，改动需同步测试。
pub const LLAMA_PROVIDER_ID: &str = "llama.cpp";

/// `DEFAULT_LLAMA_SERVER_URL`。
///
/// 本机默认 llama.cpp server 地址（`http://127.0.0.1:8080`）。只作为 `login` 的提示占位与
/// 空输入回退；`resolve`/`check` 不会回落到它——没有配置 `LLAMA_BASE_URL` 时 provider 视为
/// 未配置。
pub const DEFAULT_LLAMA_SERVER_URL: &str = "http://127.0.0.1:8080";

/// 默认推理端点（`llamaInferenceUrl(DEFAULT_LLAMA_SERVER_URL)`）。
///
/// 仅由 `Provider::base_url` 返回；模型实际使用的 base URL 来自配置的服务器（chat 为
/// `<server>/v1`），不读取这个常量。
const DEFAULT_INFERENCE_URL: &str = "http://127.0.0.1:8080/v1";

/// `credentialServerUrl(credential)`。
///
/// 只读凭据的 `env.LLAMA_BASE_URL`：缺失或为空都返回 `Ok(None)`。空判断使用 JS
/// `String.prototype.trim()` 语义（`kiana_json::js_string::js_trim`，包含 BOM 等 Unicode
/// 空白），与 `str::trim` 不完全相同。非空值必须能规范化为 http(s) URL，否则
/// `normalize_llama_server_url` 的错误（`Invalid URL` 或 `Server URL must use http or https`）
/// 原样返回。
fn credential_server_url(credential: Option<&ApiKeyCredential>) -> Result<Option<String>, AiError> {
    let Some(value) = credential
        .and_then(|credential| credential.env.as_ref())
        .and_then(|env| env.get("LLAMA_BASE_URL"))
    else {
        return Ok(None);
    };
    if kiana_json::js_string::js_trim(value).is_empty() {
        return Ok(None);
    }
    Ok(Some(normalize_llama_server_url(value)?))
}

/// `resolveServerUrl(ctx, credential)`：凭据 `env.LLAMA_BASE_URL` → `ctx.env("LLAMA_BASE_URL")`
/// （trim 后非空）→ `None`（**不**回落到默认值）。
///
/// 两个来源最终都经过 `normalize_llama_server_url`，非法 URL 以 `AiError` 返回而不是被当作
/// “未配置”；`Ok(None)` 才表示确实没有配置任何服务器地址。
async fn resolve_server_url(
    ctx: &dyn AuthContext,
    credential: Option<&ApiKeyCredential>,
) -> Result<Option<String>, AiError> {
    if let Some(configured) = credential_server_url(credential)? {
        return Ok(Some(normalize_llama_server_url(&configured)?));
    }
    let configured = ctx
        .env("LLAMA_BASE_URL")
        .await
        .map(|value| kiana_json::js_string::js_trim(&value).to_string())
        .filter(|value| !value.is_empty());
    match configured {
        Some(configured) => Ok(Some(normalize_llama_server_url(&configured)?)),
        None => Ok(None),
    }
}

/// `error instanceof Error` 的 message/name 投影为 auth 子系统的错误。
///
/// `AuthError` 的必填字段只有 name/message，`AiError` 的 kind/detail 在这条边界上丢失；
/// auth UI 与 `Models` 看到的是原始文案与错误名，分类信息不再可用。
fn auth_error(error: AiError) -> AuthError {
    AuthError::with_name(error.name, error.message)
}

/// `modelIsSelectable(model, routerAutoload)`。
///
/// - `loaded` / `sleeping`：总是可选（sleeping 的请求会由 router 自动唤醒）；
/// - `unloaded`：仅当 `routerAutoload` 为 true、`source == "preset"` 且 `status.failed` 为
///   JS 假值时可选。`failed` 按原始 JSON 的真值判断：`0`/`""`/`false`/`null`/缺失为假，
///   `[]` 与 `{}` 为真（测试 `selectable_uses_js_truthiness_for_failed` 锁定该行为）；
/// - 其它状态（`loading`、`downloading` …）不可选。
fn model_is_selectable(model: &LlamaModelInfo, router_autoload: bool) -> bool {
    if model.status.value == "loaded" {
        return true;
    }
    // llama.cpp 把休眠的模型报告为 "sleeping"；请求会自动唤醒它们。
    if model.status.value == "sleeping" {
        return true;
    }
    // 未加载的 preset 只有在 router autoload 能按需加载时才可路由。
    router_autoload
        && model.status.value == "unloaded"
        // TS: `!model.status.failed`——原始 JSON 的真值，不是严格的 `=== true`。
        && !model.status.failed_truthy()
        && model.source.as_deref() == Some("preset")
}

/// `routerAutoloadEnabled(client, catalog, signal)`：只有存在未加载 preset 时才查 `/props`。
///
/// 目录里没有 `unloaded` + `source == "preset"` 的条目时直接返回 false，避免刷新阶段无谓地
/// 请求 `/props`。探测失败（网络错误、取消等）也返回 false，错误不会中断目录刷新本身；
/// 只有 `models_autoload == Some(true)` 才算开启（`false` 或字段缺失都算关闭）。
async fn router_autoload_enabled(
    client: &LlamaClient,
    catalog: &[LlamaModelInfo],
    signal: &AbortScope,
) -> bool {
    let has_unloaded_preset = catalog
        .iter()
        .any(|model| model.status.value == "unloaded" && model.source.as_deref() == Some("preset"));
    if !has_unloaded_preset {
        return false;
    }
    match client
        .props(super::client::PropsOptions {
            model: None,
            signal: Some(signal),
        })
        .await
    {
        Ok(props) => props.models_autoload == Some(true),
        Err(_) => false,
    }
}

/// `configuredContextWindow(model)`：`status.args` 里的 `--ctx-size|-c|-ctx`（第一个合法值）。
///
/// 只扫描到倒数第二个元素（`index < length - 1`），末尾孤立标志会被跳过；值经
/// `Number(value)` 转换后必须同时满足 `Number.isSafeInteger` 且 `> 0`。`args` 不是数组时按
/// JS 的 `.length`/数字键语义读取（见 `client` 模块的 `LlamaModelStatusInfo`），例如对象形态
/// `{"length": 3, "0": …, "1": "--ctx-size", "2": "16384"}` 也参与
/// （测试 `context_window_priority`）。
fn configured_context_window(model: &LlamaModelInfo) -> Option<f64> {
    // TS: `const args = model.status.args ?? []; index < args.length - 1`——非数组也按
    // `.length`/下标（JS 语义）读取。
    let length = model.status.args_length();
    let mut index = 0usize;
    while (index as f64) < length - 1.0 {
        let flag = model.status.arg_at(index);
        if !matches!(flag.as_str(), Some("--ctx-size" | "-c" | "-ctx")) {
            index += 1;
            continue;
        }
        let context_window = super::client::js_number(&model.status.arg_at(index + 1));
        if super::client::is_safe_integer(context_window) && context_window > 0.0 {
            return Some(context_window);
        }
        index += 1;
    }
    None
}

/// `contextWindowOf(model, cachedContextWindow?)`。
///
/// 优先级：`meta.n_ctx`（>0）→ `status.args` 的 ctx-size 标志 → 上次缓存的窗口（>0）→
/// `meta.n_ctx_train`（>0）→ 128000。缓存值是“模型卸载后目录不再报告 `n_ctx`”时的兜底
/// （测试 `preserves_cached_llama_cpp_context_for_unloaded_autoload_presets`）；所有来源最终
/// 都经 `js_u64` 转成 u64。
fn context_window_of(model: &LlamaModelInfo, cached_context_window: Option<u64>) -> u64 {
    let runtime_context_window = model.meta.as_ref().and_then(|meta| meta.n_ctx);
    if let Some(value) = runtime_context_window
        && value > 0.0
    {
        return js_u64(value);
    }
    if let Some(value) = configured_context_window(model) {
        return js_u64(value);
    }
    if let Some(value) = cached_context_window
        && value > 0
    {
        return value;
    }
    let training = model.meta.as_ref().and_then(|meta| meta.n_ctx_train);
    match training {
        Some(value) if value > 0.0 => js_u64(value),
        _ => 128_000,
    }
}

/// JS number → 模型字段的 `u64`（非负有限值；超出范围时饱和）。
///
/// TS 的模型字段是 number，Rust 的 `Model.context_window`/`max_tokens` 是 `u64`，这里是两者
/// 的转换点：非有限值或 `<= 0` 返回 0，小数部分向零截断，`>= u64::MAX as f64` 饱和到
/// `u64::MAX`。
fn js_u64(value: f64) -> u64 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    if value >= u64::MAX as f64 {
        u64::MAX
    } else {
        value as u64
    }
}

/// llama.cpp 报告的 decision 模型（`output_modalities` 含 `"decisions"`，0.6.0+）。
///
/// 缺少 `architecture` 的旧服务器一律视为非 decision（测试里的 `legacy` 条目走 chat 路径）。
fn is_decision_model(model: &LlamaModelInfo) -> bool {
    model.architecture.as_ref().is_some_and(|architecture| {
        architecture
            .output_modalities
            .iter()
            .any(|value| value == "decisions")
    })
}

/// 只输出 decision 的模型不能生成文本，不列入 chat。
///
/// 非 decision 模型（含没有 `architecture` 的旧服务器）一律是 chat；同时声明 `"text"` 与
/// `"decisions"` 的混合模型两边都出现——既是 chat 模型，也有 classifier 形态
/// （测试 `lists_decision_models_that_also_output_text_for_chat`）。
fn is_chat_model(model: &LlamaModelInfo) -> bool {
    !is_decision_model(model)
        || model.architecture.as_ref().is_some_and(|architecture| {
            architecture
                .output_modalities
                .iter()
                .any(|value| value == "text")
        })
}

/// `llamaInferenceUrl(serverUrl)`，输入已规范化时直接拼接。
///
/// 正常路径委托 `llama_inference_url`（再次规范化后拼 `/v1`）；只有该函数报错时才退化为
/// “去掉尾部 `/` 后拼 `/v1`”。`set_catalog`/`refresh_models` 的服务器 URL 通常来自
/// `LlamaClient::server_url`（已规范化）。
fn inference_url(server_url: &str) -> String {
    llama_inference_url(server_url)
        .unwrap_or_else(|_| format!("{}/v1", server_url.trim_end_matches('/')))
}

/// 思考等级映射（只有 `off` 与 `medium` 有字符串值）。
///
/// 映射形状：`off`/`medium` 有字符串值，其余等级显式写 `None`（JSON `null`）。
/// `ThinkingLevelMap` 的缺键与显式 `None` 在运行期由各 API 适配器解释，因此这里保留
/// 全部键而不是只插入两个有值的键。
fn llama_thinking_level_map() -> ThinkingLevelMap {
    let mut map = ThinkingLevelMap::new();
    map.insert(ModelThinkingLevel::Off, Some("off".to_string()));
    map.insert(ModelThinkingLevel::Minimal, None);
    map.insert(ModelThinkingLevel::Low, None);
    map.insert(ModelThinkingLevel::Medium, Some("medium".to_string()));
    map.insert(ModelThinkingLevel::High, None);
    map.insert(ModelThinkingLevel::Xhigh, None);
    map
}

/// `toKianaModel(model, serverUrl, props?, cachedContextWindow?)`。
///
/// 把一个 router 目录条目映射成 chat 模型：`api` 固定 `openai-completions`、`provider` 固定
/// `LLAMA_PROVIDER_ID`、`baseUrl` 是 `<server>/v1`；`contextWindow` 同时写入 `maxTokens`
/// （与 TS 的映射一致）。`props` 为 `None`（`set_catalog` 与未加载模型）时
/// `reasoning` 为 `Some(false)`。
///
/// `compat` 是对 OpenAI 兼容端点的能力声明，全部由本函数固定，不来自服务器。
fn to_kiana_model(
    model: &LlamaModelInfo,
    server_url: &str,
    props: Option<&LlamaServerProps>,
    cached_context_window: Option<u64>,
) -> Model {
    let context_window = context_window_of(model, cached_context_window);
    // 思考支持只看已加载模型的 chat template：`/props` 的 `chat_template` 含
    // `enable_thinking` 才算开启；未探测过（props 为 None）时保持关闭。
    let reasoning = props
        .and_then(|props| props.chat_template.as_deref())
        .is_some_and(|template| template.contains("enable_thinking"));
    // llama.cpp 的 OpenAI 兼容端点能力表：不支持 store / developer role / reasoning effort /
    // strict mode；声明流式响应携带 usage；token 上限字段用 `max_tokens`。开启思考时补上
    // qwen 风格的 thinking 开关字段。
    let mut compat = serde_json::Map::new();
    compat.insert("supportsStore".into(), Value::Bool(false));
    compat.insert("supportsDeveloperRole".into(), Value::Bool(false));
    compat.insert("supportsReasoningEffort".into(), Value::Bool(false));
    compat.insert("supportsUsageInStreaming".into(), Value::Bool(true));
    compat.insert("supportsStrictMode".into(), Value::Bool(false));
    compat.insert("maxTokensField".into(), Value::String("max_tokens".into()));
    if reasoning {
        compat.insert(
            "thinkingFormat".into(),
            Value::String("qwen-chat-template".into()),
        );
    }
    // 只有目录明确报告 `image` 输入模态时才加多模态；否则纯文本。
    let image = model.architecture.as_ref().is_some_and(|architecture| {
        architecture
            .input_modalities
            .iter()
            .any(|value| value == "image")
    });
    Model {
        id: model.id.clone(),
        name: model.id.clone(),
        api: "openai-completions".to_string(),
        provider: LLAMA_PROVIDER_ID.to_string(),
        base_url: inference_url(server_url),
        input: if image {
            vec![ModelInputModality::Text, ModelInputModality::Image]
        } else {
            vec![ModelInputModality::Text]
        },
        cost: ModelCost::default(),
        reasoning: Some(reasoning),
        thinking_level_map: if reasoning {
            Some(llama_thinking_level_map())
        } else {
            None
        },
        context_window: Some(context_window),
        max_tokens: Some(context_window),
        compat: Some(Value::Object(compat)),
        ..Default::default()
    }
}

/// `toKianaClassifierModel(model, serverUrl, cachedContextWindow?)`。
///
/// 同一批可选模型会各自生成一个 classifier 形态，`api` 与 `baseUrl` 取决于是否 decision：
/// decision 模型走 `typesafe-system-one`（base URL 为 `<server>/v1`，原生 /v1/systemone 协议），
/// 其余走 `llama-cpp-classify`（base URL 为服务器根，用于 /tokenize、/apply-template、
/// /completion）。这就是两种分类器 base URL 差异的来源。
///
/// 输入固定为纯文本、cost 全 0、`model_type` 为 `Classifier`；上下文窗口与 chat 模型共用
/// `context_window_of`（同样会用到缓存值）。
fn to_kiana_classifier_model(
    model: &LlamaModelInfo,
    server_url: &str,
    cached_context_window: Option<u64>,
) -> Model {
    let decision = is_decision_model(model);
    Model {
        id: model.id.clone(),
        name: model.id.clone(),
        api: if decision {
            "typesafe-system-one".to_string()
        } else {
            "llama-cpp-classify".to_string()
        },
        provider: LLAMA_PROVIDER_ID.to_string(),
        base_url: if decision {
            inference_url(server_url)
        } else {
            server_url.to_string()
        },
        model_type: Some(ModelType::Classifier),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(context_window_of(model, cached_context_window)),
        ..Default::default()
    }
}

/// `isModelType(model, "classifier") && (api === "llama-cpp-classify" || api === "typesafe-system-one")`。
///
/// 用于从持久化目录里挑出本 provider 之前写出的 classifier 条目；api 白名单同时排除了未来
/// 新增或其它实现的 classifier api，避免把无法识别的模型恢复进目录。
fn is_llama_classifier_model(model: &AnyModel) -> bool {
    model.model_type() == ModelType::Classifier
        && (model.model().api == "llama-cpp-classify" || model.model().api == "typesafe-system-one")
}

/// `Date.now()`。
///
/// 用作持久化条目的 `checked_at`（毫秒）。系统时间早于 `UNIX_EPOCH` 时返回 0，不 panic、
/// 也不产生负值。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

/// provider 的当前目录（`setCatalog`/`refreshModels` 的 `update` 闭包写入）。
///
/// 两个列表由同一次目录映射产生，但筛选规则不同：chat 列表丢弃只输出 decision 的模型，
/// classifier 列表包含全部可选模型。写路径整表替换（不做增量合并），读路径在同一个锁内
/// 克隆两个列表，因此读者看到的是一次完整发布的结果。
#[derive(Debug, Default)]
struct LlamaCatalogState {
    /// 可选 chat 模型（`api = "openai-completions"`）。
    models: Vec<Model>,
    /// classifier 模型（`llama-cpp-classify`/`typesafe-system-one`），可能包含 chat 列表中
    /// 不存在的 decision-only 模型。
    classifiers: Vec<Model>,
}

/// `LlamaProviderController`。
///
/// TS 的 `provider` 字段与 `setCatalog` 闭包共享同一份捕获变量；Rust 用共享
/// `Arc<Mutex<LlamaCatalogState>>` 复刻。
///
/// 目录的来源与写入路径：
///
/// - 创建时为空（[`create_llama_provider`] / [`create_llama_provider_with_http`]）；
/// - [`set_catalog`](LlamaProviderController::set_catalog) 供 `/llama` 管理器在重读目录后同步；
/// - `Provider::refresh_models` 由 `kiana-ai` 的 `Models` 调用，负责缓存恢复与联网刷新。
///
/// clone 只复制两个 `Arc`，不会复制目录状态。
#[derive(Clone)]
pub struct LlamaProviderController {
    /// 原生 provider。
    ///
    /// 注册给 `Models` 的句柄；与 `state` 共享目录。
    pub provider: Arc<dyn Provider>,
    /// 共享目录状态（与 `provider` 内部是同一个 `Arc`）。
    state: Arc<Mutex<LlamaCatalogState>>,
}

impl LlamaProviderController {
    /// `setCatalog(models, serverUrl, options?)`。
    ///
    /// `routerAutoload` 缺省 false（TS 的 `options?.routerAutoload === true`）。
    ///
    /// 同步写入内存目录：不发网络请求、不发布、不持久化，也不触碰凭证。chat 列表丢弃只输出
    /// decision 的模型，classifier 列表则包含**所有**可选模型（包括 decision-only 模型）。
    /// `router_autoload` 决定 `unloaded` + `preset`（且未失败）的模型是否入选。
    ///
    /// 因为不读取 `/props`，这里产出的 chat 模型 `reasoning` 恒为 `Some(false)` 且没有
    /// `thinking_level_map`（测试 `reasoning_only_when_template_mentions_enable_thinking`）。
    ///
    /// `server_url` 期望是已规范化的服务器根（调用方通常传 `LlamaClient::server_url`）：chat
    /// 模型的 base URL 会再经 `inference_url` 规范化，非 decision classifier 的 base URL 则
    /// 原样使用该字符串。
    pub fn set_catalog(&self, models: &[LlamaModelInfo], server_url: &str, router_autoload: bool) {
        let selectable: Vec<&LlamaModelInfo> = models
            .iter()
            .filter(|model| model_is_selectable(model, router_autoload))
            .collect();
        let chat: Vec<Model> = selectable
            .iter()
            .filter(|model| is_chat_model(model))
            .map(|model| to_kiana_model(model, server_url, None, None))
            .collect();
        let classifiers: Vec<Model> = selectable
            .iter()
            .map(|model| to_kiana_classifier_model(model, server_url, None))
            .collect();
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.models = chat;
        state.classifiers = classifiers;
    }
}

/// provider 本体。
///
/// 除共享的 `state` 外都是不可变依赖（HTTP 客户端与两个分类器），因此句柄可以跨任务共享
/// （`Provider` 要求 `Send + Sync`）；目录读写在 `state` 的 `Mutex` 内完成。
struct LlamaProvider {
    /// 与 controller 共享的目录状态。
    state: Arc<Mutex<LlamaCatalogState>>,
    /// 只有 api-key auth（没有 OAuth）。
    auth: ProviderAuth,
    /// 目录刷新复用的 HTTP 客户端（与 `LlamaApiKeyAuth` 共用）。
    http: kiana_ai::http::SharedHttpClient,
    /// 兜底分类器（`llama-cpp-classify`）。
    fallback_classifier: Arc<dyn ProviderClassifier>,
    /// decision 模型分类器（`typesafe-system-one`）。
    decision_classifier: Arc<dyn ProviderClassifier>,
}

impl LlamaProvider {
    /// 在锁内克隆一份完整目录；两个列表来自同一把锁，保证属于同一次发布。
    ///
    /// 锁中毒（持锁线程 panic）时取回内部值继续使用，读路径不会因此失败。
    fn current(&self) -> LlamaCatalogState {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        LlamaCatalogState {
            models: state.models.clone(),
            classifiers: state.classifiers.clone(),
        }
    }
}

/// `llamaCppClassifyApi()` / `typesafeSystemOneApi()`（Rust 侧静态链接）。
///
/// 两个实现都是无状态的 `Arc<dyn ProviderClassifier>`；`LlamaProvider::classify` 按模型的
/// `api` 字段二选一，与 TS 一样没有运行期注册或查找。
fn classifier_implementations() -> (Arc<dyn ProviderClassifier>, Arc<dyn ProviderClassifier>) {
    (
        kiana_ai::api::llama_cpp_classify::llama_cpp_classify_api(),
        kiana_ai::api::typesafe_system_one::typesafe_system_one_api(),
    )
}

#[async_trait]
impl Provider for LlamaProvider {
    /// `LLAMA_PROVIDER_ID`；`Models` 与凭证存储都用它做键。
    fn id(&self) -> &str {
        LLAMA_PROVIDER_ID
    }

    /// 固定展示名 `"llama.cpp"`。
    fn name(&self) -> &str {
        "llama.cpp"
    }

    /// 固定返回默认推理端点（`http://127.0.0.1:8080/v1`），与当前配置的服务器无关。
    ///
    /// 请求实际使用的 base URL 来自 auth `resolve` 的 `auth.base_url`（`<server>/v1`）；
    /// 本 provider 的 `refresh_models` 也不读取 `RefreshModelsContext::base_url`。
    fn base_url(&self) -> Option<&str> {
        Some(DEFAULT_INFERENCE_URL)
    }

    /// 只有 api-key auth（`LlamaApiKeyAuth`，无 OAuth）。
    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    /// 当前 chat 目录快照；尚未刷新/配置时为空 `Vec`。
    fn get_models(&self) -> Vec<Model> {
        self.current().models
    }

    /// true：`get_all_models` 额外返回 classifier 模型，无法由 chat 列表推导。
    fn has_get_all_models(&self) -> bool {
        true
    }

    /// chat 模型在前、classifier 模型在后；二者都是克隆，调用方修改不影响内部状态。
    fn get_all_models(&self) -> Vec<AnyModel> {
        let state = self.current();
        let mut models: Vec<AnyModel> = state.models.into_iter().map(AnyModel::Chat).collect();
        models.extend(state.classifiers.into_iter().map(AnyModel::Classifier));
        models
    }

    /// 先恢复持久化缓存，再在允许联网且凭证是 api-key 时重新抓取目录。
    ///
    /// 错误传播：`/models`、`/props`、发布（包括持久化）失败都会以 `AiError` 返回，由
    /// `Models.refresh()` 记入该 provider 的 `errors`（中止引发的错误会被丢弃）。网络阶段
    /// 发布前失败时，内存目录保持缓存恢复阶段的结果（没有缓存时保持上一次的内容）。
    ///
    /// 取消：每次网络调用返回后都检查 `context.signal`，若此时已中止则返回 `Ok(())`；调用
    /// 进行中被中止仍会以错误形式返回，由 `Models` 当作可忽略的中止处理。`context.force`
    /// 不被使用（TS 的 `refreshModels` 同样不读它）。
    async fn refresh_models(&self, context: RefreshModelsContext) -> Result<(), AiError> {
        // 整次刷新共享的“上次已知上下文窗口”表：先由持久化目录填充，供两个阶段使用。
        let mut cached_context_windows: IndexMap<String, u64> = IndexMap::new();
        if let Some(stored) = &context.stored {
            // 阶段 1：恢复持久化缓存。只读 `context.stored`，不发网络请求；发布用
            // `Unchanged`，因此不会把恢复结果再次写盘。
            //
            // 只认本 provider 自己写出的条目：chat 限定 `Chat` + `openai-completions`，
            // classifier 限定两个本 provider 的 api；其它类型直接丢弃。
            let stored: Vec<&AnyModel> = stored
                .models
                .iter()
                .filter(|model| model.model().provider == LLAMA_PROVIDER_ID)
                .collect();
            let restored: Vec<Model> = stored
                .iter()
                .filter(|model| {
                    model.model_type() == ModelType::Chat
                        && model.model().api == "openai-completions"
                })
                .map(|model| model.model().clone())
                .collect();
            let restored_classifiers: Vec<Model> = stored
                .iter()
                .filter(|model| is_llama_classifier_model(model))
                .map(|model| model.model().clone())
                .collect();
            // 缓存上下文窗口按 id 记录：chat 与 classifier 同 id，后写的覆盖先写的（两者来自
            // 同一次映射，值相同）。这些值用于模型卸载后目录不再报告 `meta.n_ctx` 的场景。
            for model in restored.iter().chain(restored_classifiers.iter()) {
                if let Some(context_window) = model.context_window {
                    cached_context_windows.insert(model.id.clone(), context_window);
                }
            }
            // `update` 闭包由发布方在代际/取消检查之后执行，只改内存；`Ok(false)` 表示本次
            // 刷新已被更新的代际取代或被取消——按 TS 直接结束，不再联网。闭包类型是
            // `FnOnce + Send + 'static`，所以先把 `Arc` 与两个列表克隆出来交给它。
            let state = Arc::clone(&self.state);
            let update_models = restored.clone();
            let update_classifiers = restored_classifiers.clone();
            let published = (context.publish)(ModelsPublication {
                persist: ModelsPublicationPersist::Unchanged,
                update: Some(Box::new(move || {
                    let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
                    state.models = update_models;
                    state.classifiers = update_classifiers;
                })),
            })
            .await?;
            if !published {
                return Ok(());
            }
        }

        // 阶段 2：联网刷新。三个前置条件缺一不可：允许联网、未中止、凭证是 api-key；
        // 不满足时保持缓存结果（OAuth 或没有凭据的 provider 不会访问网络）。
        if !context.allow_network
            || context.signal.is_aborted()
            || !matches!(
                context.credential.as_ref(),
                Some(kiana_ai::auth::types::Credential::ApiKey(_))
            )
        {
            return Ok(());
        }
        let Some(credential) = context
            .credential
            .as_ref()
            .and_then(|credential| credential.as_api_key())
        else {
            return Ok(());
        };
        // 服务器地址只从凭据的 `env.LLAMA_BASE_URL` 取（`credentialServerUrl`），不回退到
        // 进程环境变量或 `context.base_url`；没有配置地址时保持缓存结果。
        let Some(server_url) = credential_server_url(Some(credential))? else {
            return Ok(());
        };
        let client = LlamaClient::with_http_client(
            &server_url,
            credential.key.clone(),
            Arc::clone(&self.http),
        )?;
        // 目录请求本身可能耗到 15 s 总超时（`LlamaClient` 的 `REQUEST_TIMEOUT_MS`）；
        // 返回后立刻检查中止，因为后面还有多次串行网络请求。
        let catalog = client
            .list(super::client::ListOptions {
                reload: false,
                signal: Some(&context.signal),
            })
            .await?;
        if context.signal.is_aborted() {
            return Ok(());
        }
        // router autoload 探测只在目录出现未加载 preset 时才真正发请求（见
        // `router_autoload_enabled`）；探测失败按关闭处理。
        let router_autoload = router_autoload_enabled(&client, &catalog, &context.signal).await;
        if context.signal.is_aborted() {
            return Ok(());
        }
        let selectable: Vec<&LlamaModelInfo> = catalog
            .iter()
            .filter(|model| model_is_selectable(model, router_autoload))
            .collect();
        // 只有已加载模型能在无副作用的情况下暴露 chat template；未加载 autoload preset 需要加载，
        // 查询 sleeping 模型可能唤醒它们——这些模型在加载并被后续刷新发现前没有思考支持。
        // decision 模型不需要模板，llama.cpp 无论如何都会在目录里报告它们。
        let chat_for_props: Vec<&LlamaModelInfo> = selectable
            .iter()
            .copied()
            .filter(|model| is_chat_model(model))
            .collect();
        let props_futures = chat_for_props.iter().map(|model| {
            // 每个探测 future 都取得自己的 `client`/`signal` 副本：future 会被整体交给
            // `try_join_all`，`async move` 不能借用迭代变量。
            let client = client.clone();
            let server_url = server_url.clone();
            let signal = context.signal.clone();
            let cached_context_window = cached_context_windows.get(&model.id).copied();
            async move {
                // 非 loaded：不探测，直接映射为无思考支持；loaded：先取 `/props`，失败会让
                // 整次刷新失败（`try_join_all` 的 `?`）。
                if model.status.value != "loaded" {
                    return Ok::<Model, AiError>(to_kiana_model(
                        model,
                        &server_url,
                        None,
                        cached_context_window,
                    ));
                }
                let props = client
                    .props(super::client::PropsOptions {
                        model: Some(&model.id),
                        signal: Some(&signal),
                    })
                    .await?;
                Ok(to_kiana_model(
                    model,
                    &server_url,
                    Some(&props),
                    cached_context_window,
                ))
            }
        });
        // TS 是 `await Promise.all(...)`：任一 `/props` 探测**按完成顺序**拒绝时立即失败，
        // 其余请求在后台继续；`join_all` 会等所有探测结束再按输入顺序取第一个错误。
        // `try_join_all` 同时保留结果顺序与“首个拒绝立即传播”的语义。
        let refreshed: Vec<Model> = futures::future::try_join_all(props_futures).await?;
        // classifier 形态覆盖全部可选模型（包括 decision-only 模型），不需要 chat template，
        // 因此不做 `/props` 探测。
        let refreshed_classifiers: Vec<Model> = selectable
            .iter()
            .map(|model| {
                to_kiana_classifier_model(
                    model,
                    &server_url,
                    cached_context_windows.get(&model.id).copied(),
                )
            })
            .collect();
        // 发布前最后一次检查：并发探测期间 signal 可能已中止，之后不应再写目录/持久化。
        if context.signal.is_aborted() {
            return Ok(());
        }
        // 持久化条目按 chat 在前、classifier 在后写入；`checked_at` 是本次刷新时间。
        let persisted_models: Vec<AnyModel> = refreshed
            .iter()
            .cloned()
            .map(AnyModel::Chat)
            .chain(
                refreshed_classifiers
                    .iter()
                    .cloned()
                    .map(AnyModel::Classifier),
            )
            .collect();
        let state = Arc::clone(&self.state);
        let update_models = refreshed.clone();
        let update_classifiers = refreshed_classifiers.clone();
        // `update` 与缓存恢复阶段一样整表替换。最终发布的 `Ok(false)` 被忽略——发布后没有
        // 更多工作，与 TS 相同；`Err` 仍会向上传播。
        (context.publish)(ModelsPublication {
            persist: ModelsPublicationPersist::Entry(ModelsStoreEntry {
                models: persisted_models,
                checked_at: Some(now_ms()),
                ..Default::default()
            }),
            update: Some(Box::new(move || {
                let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
                state.models = update_models;
                state.classifiers = update_classifiers;
            })),
        })
        .await?;
        Ok(())
    }

    /// 委托 `kiana_ai::compat::stream`：按 `model.api`（chat 模型恒为 `openai-completions`）选择
    /// 实现并**同步**返回事件流，网络工作由返回的流承担。
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> kiana_ai::event_stream::AssistantMessageEventStream {
        kiana_ai::compat::stream(model, context.as_context(), options)
    }

    /// 同 [`Provider::stream`]，但携带 `SimpleStreamOptions`（`reasoning`/`tool_choice` 等
    /// 便捷字段），同样委托 `kiana_ai::compat::stream_simple`。
    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> kiana_ai::event_stream::AssistantMessageEventStream {
        kiana_ai::compat::stream_simple(model, context.as_context(), options)
    }

    /// 按 `model.api` 分派分类实现：`typesafe-system-one` 走 decision classifier，其余（含
    /// `llama-cpp-classify`）走兜底分类器。
    ///
    /// 这里不校验 `model.provider`/`model.api` 是否属于本 provider：未知 api 同样落到兜底
    /// 实现。
    async fn classify(
        &self,
        model: &Model,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> ClassifierResult {
        let classifier = if model.api == "typesafe-system-one" {
            &self.decision_classifier
        } else {
            &self.fallback_classifier
        };
        classifier.classify(model, context, options).await
    }
}

/// llama.cpp server 的 api-key auth。
///
/// 这里的核心配置是服务器 URL（存在凭据的 `env.LLAMA_BASE_URL`），
/// api key 可选：`resolve` 在没有 key 时回退 `LLAMA_API_KEY` → 字面量 `"local"`。结构体只
/// 持有共享 HTTP 客户端，没有内部可变状态。
struct LlamaApiKeyAuth {
    /// 与 provider 共享的客户端，用于 `login` 的连通性验证。
    http: kiana_ai::http::SharedHttpClient,
}

#[async_trait]
impl ApiKeyAuth for LlamaApiKeyAuth {
    /// 展示名 `"llama.cpp server"`。
    fn name(&self) -> &str {
        "llama.cpp server"
    }

    /// true：实现了 `login`（交互式录入服务器地址与可选 API key，并验证连通）。
    fn has_login(&self) -> bool {
        true
    }

    /// `login(interaction)`：两次提示（服务器 URL、可选 API key），用 `GET /models` 验证连通后
    /// 返回凭据。
    ///
    /// - URL：提示的 placeholder 来自进程环境变量 `LLAMA_BASE_URL`（未设置时用
    ///   [`DEFAULT_LLAMA_SERVER_URL`]）；输入 trim 后为空则回退进程环境变量（非空），再回退
    ///   默认值，随后规范化（非法 URL 以 `AuthError` 返回）。
    /// - API key：trim 后为空存 `None`。
    /// - 验证请求失败时把 `AiError` 投影成 `AuthError`，不会返回未验证的凭据。
    /// - 返回凭据把规范化后的 URL 放进 `env.LLAMA_BASE_URL`，key 放在 `key` 字段。
    ///
    /// 注意这里读的是**进程环境**（`std::env`）而不是注入的 `AuthContext`，与 TS 的
    /// `process.env` 一致；测试用 `FakeInteraction` + 回环服务器覆盖该路径。
    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<ApiKeyCredential, AuthError> {
        let placeholder = std::env::var("LLAMA_BASE_URL")
            .ok()
            .unwrap_or_else(|| DEFAULT_LLAMA_SERVER_URL.to_string());
        let entered_url = interaction
            .prompt(AuthPromptRequest::new(AuthPrompt::Text {
                message: "llama.cpp server URL".to_string(),
                placeholder: Some(placeholder),
            }))
            .await?;
        let trimmed = kiana_json::js_string::js_trim(&entered_url);
        let candidate = if trimmed.is_empty() {
            std::env::var("LLAMA_BASE_URL")
                .ok()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| DEFAULT_LLAMA_SERVER_URL.to_string())
        } else {
            trimmed.to_string()
        };
        let server_url = normalize_llama_server_url(&candidate).map_err(auth_error)?;
        let api_key = kiana_json::js_string::js_trim(
            &interaction
                .prompt(AuthPromptRequest::new(AuthPrompt::Secret {
                    message: "API key (optional)".to_string(),
                    placeholder: None,
                }))
                .await?,
        )
        .to_string();
        let client = LlamaClient::with_http_client(
            &server_url,
            if api_key.is_empty() {
                None
            } else {
                Some(api_key.clone())
            },
            Arc::clone(&self.http),
        )
        .map_err(auth_error)?;
        // 用一次轻量目录请求验证地址与 key；`/models` 是 router 模式的最小可用性探针，
        // 失败会把 `AiError` 投影成 `AuthError` 返回给登录 UI。
        client
            .list(super::client::ListOptions {
                reload: false,
                signal: Some(interaction.signal()),
            })
            .await
            .map_err(auth_error)?;
        let mut env = IndexMap::new();
        env.insert("LLAMA_BASE_URL".to_string(), server_url);
        Ok(ApiKeyCredential {
            key: if api_key.is_empty() {
                None
            } else {
                Some(api_key)
            },
            env: Some(env),
            extra: IndexMap::new(),
        })
    }

    /// true：实现了 `check`（只解析服务器地址，不发网络请求）。
    fn has_check(&self) -> bool {
        true
    }

    /// `check({ ctx, credential })`：无副作用的可用性判断。
    ///
    /// 只调用 `resolve_server_url`（凭据 `env.LLAMA_BASE_URL` → `ctx.env("LLAMA_BASE_URL")`），
    /// 不访问网络。解析到 URL 就返回 `AuthCheck`：`source` 在存在存储凭据时是
    /// `"stored credential"`，否则 `"LLAMA_BASE_URL"`；`type` 恒为 `ApiKey`。返回
    /// `Ok(None)` 表示没有配置可用的服务器地址，`Models`/会话据此视为未鉴权（不会调用
    /// `resolve`）；只有实现未覆盖 `check`（`has_check` 为 `false`）时 `Models` 才改用
    /// `resolve` 判断可用性。URL 解析失败仍以 `Err` 返回。
    async fn check(&self, input: ApiKeyAuthInput<'_>) -> Result<Option<AuthCheck>, AuthError> {
        let server_url = resolve_server_url(input.ctx, input.credential)
            .await
            .map_err(auth_error)?;
        Ok(server_url.map(|_| AuthCheck {
            source: Some(
                if input.credential.is_some() {
                    "stored credential"
                } else {
                    "LLAMA_BASE_URL"
                }
                .to_string(),
            ),
            r#type: AuthType::ApiKey,
        }))
    }

    /// `resolve({ ctx, credential })`：解析一次模型请求要用的 auth。
    ///
    /// - 服务器 URL：`resolve_server_url`（凭据 env 优先，其次 `ctx.env`）；没有则返回
    ///   `Ok(None)`，`Models` 据此报 `Provider is not configured`。
    /// - API key：凭据 `key` → `ctx.env("LLAMA_API_KEY")` → 字面量 `"local"`。
    /// - `auth.base_url` 恒为 `<server>/v1`（OpenAI 兼容端点）；`auth.headers` 为 `None`。
    /// - 返回的 `env` 保留凭据里的其它条目，并把 `LLAMA_BASE_URL` 覆盖为规范化后的 URL。
    ///
    /// URL 规范化失败以 `AuthError` 返回，不会被当成“未配置”。
    async fn resolve(&self, input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        let server_url = resolve_server_url(input.ctx, input.credential)
            .await
            .map_err(auth_error)?;
        let Some(server_url) = server_url else {
            return Ok(None);
        };
        let api_key = match input
            .credential
            .and_then(|credential| credential.key.clone())
        {
            Some(key) => key,
            None => input
                .ctx
                .env("LLAMA_API_KEY")
                .await
                .unwrap_or_else(|| "local".to_string()),
        };
        let mut env = input
            .credential
            .and_then(|credential| credential.env.clone())
            .unwrap_or_default();
        env.insert("LLAMA_BASE_URL".to_string(), server_url.clone());
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some(api_key),
                headers: None,
                base_url: Some(llama_inference_url(&server_url).map_err(auth_error)?),
            },
            env: Some(env),
            source: Some(
                if input.credential.is_some() {
                    "stored credential"
                } else {
                    "LLAMA_BASE_URL"
                }
                .to_string(),
            ),
        }))
    }
}

/// `createLlamaProvider()`。
///
/// 使用 crate 内共享的默认 `HttpClient`（`client::default_http_client`）；目录初始为空，等待
/// `refresh_models` 或 `set_catalog` 填充。auth 已配置为只有 api-key 的
/// `LlamaApiKeyAuth`；凭据 env 与 `LLAMA_BASE_URL` 环境变量都没有给出服务器地址时，
/// `resolve` 返回 `Ok(None)`。
pub fn create_llama_provider() -> LlamaProviderController {
    create_llama_provider_with_http(default_http_client())
}

/// 注入 `HttpClient` 的 `createLlamaProvider()`（测试/宿主自定义传输）。
///
/// 同一个客户端同时用于目录刷新（`LlamaProvider`）与 auth 的连通性验证
/// （`LlamaApiKeyAuth`），因此自定义传输会覆盖两条路径。返回的 controller 与 provider
/// 共享同一份目录状态（见 [`LlamaProviderController`]）。
pub fn create_llama_provider_with_http(
    http: kiana_ai::http::SharedHttpClient,
) -> LlamaProviderController {
    let state = Arc::new(Mutex::new(LlamaCatalogState::default()));
    let (fallback_classifier, decision_classifier) = classifier_implementations();
    let provider: Arc<dyn Provider> = Arc::new(LlamaProvider {
        state: Arc::clone(&state),
        auth: ProviderAuth {
            api_key: Some(Arc::new(LlamaApiKeyAuth {
                http: Arc::clone(&http),
            })),
            oauth: None,
        },
        http: Arc::clone(&http),
        fallback_classifier,
        decision_classifier,
    });
    LlamaProviderController { provider, state }
}

#[cfg(test)]
mod tests {
    //! provider 纯函数的回归测试，锁定 JS 语义与 `/props` 并发失败行为。
    //!
    //! - `context_window_priority`：上下文窗口优先级与 JS 语义的 `args` 读取；
    //! - `selectable_uses_js_truthiness_for_failed`：`status.failed` 的 JS 真值；
    //! - `chat_and_decision_model_classification`：decision/chat 判定；
    //! - `refresh_models_propagates_the_first_props_failure_like_promise_all`：`/props`
    //!   探测的首个失败必须立即传播，不能被挂起的探测拖住；
    //! - `reasoning_only_when_template_mentions_enable_thinking`：思考支持与 `compat` 映射。

    use super::*;

    fn info(id: &str, status: &str) -> LlamaModelInfo {
        LlamaModelInfo {
            id: id.to_string(),
            status: super::super::client::LlamaModelStatusInfo {
                value: status.to_string(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn context_window_priority() {
        let mut model = info("m", "loaded");
        model.meta = Some(super::super::client::LlamaModelMeta {
            n_ctx: Some(65536.0),
            n_ctx_train: Some(131072.0),
            ..Default::default()
        });
        assert_eq!(context_window_of(&model, Some(1)), 65536);
        model.meta.as_mut().expect("meta").n_ctx = None;
        model.status.args = serde_json::json!(["--ctx-size", "32768"]);
        assert_eq!(context_window_of(&model, Some(1)), 32768);
        // TS 按 `.length`/下标读取原始 JSON：非数组对象也参与。
        model.status.args = serde_json::json!({
            "length": 3,
            "0": "llama-server",
            "1": "--ctx-size",
            "2": "16384"
        });
        assert_eq!(context_window_of(&model, Some(1)), 16384);
        model.status.args = Value::Null;
        assert_eq!(context_window_of(&model, Some(4096)), 4096);
        assert_eq!(context_window_of(&model, None), 131072);
        model.meta = None;
        assert_eq!(context_window_of(&model, None), 128_000);
    }

    #[test]
    fn selectable_uses_js_truthiness_for_failed() {
        let mut preset = info("preset", "unloaded");
        preset.source = Some("preset".to_string());
        preset.status.failed = Some(serde_json::json!(1));
        assert!(!model_is_selectable(&preset, true));
        preset.status.failed = Some(serde_json::json!(0));
        assert!(model_is_selectable(&preset, true));
        preset.status.failed = Some(serde_json::json!([]));
        assert!(!model_is_selectable(&preset, true));
        preset.status.failed = None;
        assert!(model_is_selectable(&preset, true));
    }

    #[test]
    fn chat_and_decision_model_classification() {
        let mut decide = info("decide", "sleeping");
        decide.architecture = Some(super::super::client::LlamaModelArchitecture {
            output_modalities: vec!["decisions".to_string()],
            ..Default::default()
        });
        assert!(is_decision_model(&decide));
        assert!(!is_chat_model(&decide));
        decide
            .architecture
            .as_mut()
            .expect("arch")
            .output_modalities = vec!["decisions".to_string(), "text".to_string()];
        assert!(is_chat_model(&decide));
        let chat = info("chat", "loaded");
        assert!(!is_decision_model(&chat));
        assert!(is_chat_model(&chat));
    }

    /// `/props` 探测的拒绝必须像 `Promise.all` 一样**按完成顺序立即**传播：一个挂起的
    /// 探测不能把另一个探测的错误拖到 15 s 超时之后（旧实现用 `join_all`）。
    struct ProbeHttpClient;

    impl kiana_ai::http::HttpClient for ProbeHttpClient {
        fn execute<'a>(
            &'a self,
            request: kiana_ai::http::HttpRequest,
            _signal: Option<&tokio_util::sync::CancellationToken>,
        ) -> kiana_ai::http::BoxFuture<
            'a,
            Result<kiana_ai::http::HttpResponse, kiana_ai::http::HttpError>,
        > {
            Box::pin(async move {
                if request.url.ends_with("/models") {
                    let body = serde_json::json!({
                        "data": [
                            { "id": "fast", "status": { "value": "loaded" } },
                            { "id": "slow", "status": { "value": "loaded" } }
                        ]
                    });
                    let stream: kiana_ai::http::ByteStream =
                        Box::pin(futures::stream::iter(vec![Ok(
                            kiana_json::js_object::stringify(&body).into_bytes(),
                        )]));
                    return Ok(kiana_ai::http::HttpResponse {
                        response: kiana_ai::types::ProviderResponse {
                            status: 200,
                            headers: indexmap::IndexMap::new(),
                        },
                        body: stream,
                    });
                }
                if request.url.contains("model=fast") {
                    return Err(kiana_ai::http::HttpError::new("boom"));
                }
                if request.url.contains("model=slow") {
                    // `Promise.all` 在 fast 拒绝时立即结束；`join_all` 会一直等这里。
                    std::future::pending::<()>().await;
                }
                Err(kiana_ai::http::HttpError::new("unexpected request"))
            })
        }
    }

    #[tokio::test]
    async fn refresh_models_propagates_the_first_props_failure_like_promise_all() {
        use kiana_ai::auth::types::{ApiKeyCredential, Credential};
        use kiana_ai::models::{ModelsPublication, PublishFn, RefreshModelsContext};

        let controller = create_llama_provider_with_http(Arc::new(ProbeHttpClient));
        let publish: PublishFn = Arc::new(|publication: ModelsPublication| {
            Box::pin(async move {
                if let Some(update) = publication.update {
                    update();
                }
                Ok(true)
            })
        });
        let mut env = IndexMap::new();
        env.insert(
            "LLAMA_BASE_URL".to_string(),
            "http://127.0.0.1:9".to_string(),
        );
        let context = RefreshModelsContext {
            credential: Some(Credential::ApiKey(ApiKeyCredential {
                key: Some("local".to_string()),
                env: Some(env),
                extra: IndexMap::new(),
            })),
            stored: None,
            publish,
            allow_network: true,
            force: None,
            signal: AbortScope::new(),
            base_url: None,
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            controller.provider.refresh_models(context),
        )
        .await
        .expect("the hung probe must not delay the first rejection");
        assert_eq!(result.expect_err("boom").message, "boom");
    }

    #[test]
    fn reasoning_only_when_template_mentions_enable_thinking() {
        let model = info("qwen", "loaded");
        let props = LlamaServerProps {
            models_autoload: None,
            chat_template: Some("{% if enable_thinking %}think{% endif %}".to_string()),
        };
        let mapped = to_kiana_model(&model, "http://h:8080", Some(&props), None);
        assert_eq!(mapped.reasoning, Some(true));
        let compat = mapped.compat.expect("compat");
        assert_eq!(
            compat.get("thinkingFormat").and_then(Value::as_str),
            Some("qwen-chat-template")
        );
        let mapped = to_kiana_model(&model, "http://h:8080", None, None);
        assert_eq!(mapped.reasoning, Some(false));
        assert!(mapped.thinking_level_map.is_none());
    }
}
