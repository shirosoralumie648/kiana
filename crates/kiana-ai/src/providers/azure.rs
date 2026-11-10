//! providers/azure.ts：`azure_provider` provider 工厂与 Azure 端点解析包装。
//!
//! TS: `packages/ai/src/providers/azure.ts`
//!
//! # 为什么需要这个文件
//!
//! Azure OpenAI 的端点与 deployment 名是**按用户/资源变化**的：同一个模型 id 在不同
//! Azure 资源下对应不同主机，请求体里的 `model` 也可能不是目录里的 id。两种协议的处理
//! 方式不同：
//!
//! - `azure-openai-responses`：适配器自己会在请求前调用
//!   `api::azure_openai_config` 的解析函数，因此直接注册即可；
//! - `openai-completions`：这是与其它厂商共享的通用适配器，不认识 Azure 规则，所以本文件
//!   用 [`AzureStreams`] 在调用底层实现前补做 base URL 与 deployment 的解析。
//!
//! # 主要流程（一次 [`AzureStreams::stream`]）
//!
//! 1. 从 [`StreamOptions::extra`] 读四个 Azure 选项（`azureApiVersion`/`azureResourceName`/
//!    `azureBaseUrl`/`azureDeploymentName`），并只把 `env` 带进 `AzureEndpointOptions`
//!    （解析函数只用到 env，见 `api::azure_openai_config` 的模块文档）；
//! 2. 用 `resolve_azure_base_url` 现算 base URL（选项 → 环境变量 → 资源名 → 模型自身的
//!    `baseUrl`，失败返回 `AiErrorKind::Request`）；
//! 3. 若 deployment 名与 `model.id` 不同，改写 `options.on_payload`，在请求体里把
//!    `model` 字段替换成 deployment 名（`model.id` 仍作为目录 id 记录在助手消息里）；
//! 4. 调用内层实现。
//!
//! 这些步骤放在 [`lazy_stream`] 的 setup 里：`stream()` 必须同步返回事件流，而
//! 「端点没配置」这类错误应作为流上的 `error` 事件出现，而不是从 `stream()` 抛出
//! （spec/ai-core.md §2.3.12 的注释）。
//!
//! # 约束
//!
//! - `with_deployment_name` 只在 `on_payload` 上叠加，不修改其它请求字段；它先写入
//!   deployment 再调用调用方原有的回调，原回调返回非 `null` 时以原回调的结果为准。
//! - 非对象 payload 会被替换成空对象后再写入 `model`（正常适配器总是传对象；该分支只
//!   防御异常输入）。
//! - deployment 与 base URL 的解析互相独立：deployment 只看 `model.id`、显式选项与
//!   `AZURE_OPENAI_DEPLOYMENT_NAME_MAP`。
//!
//! 调用方：[`crate::providers::all::builtin_providers`] 按固定顺序实例化本工厂。

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::Value;

use crate::api::azure_openai_config::{
    AzureEndpointOptions, resolve_azure_base_url, resolve_deployment_name,
};
use crate::api::azure_openai_responses::azure_openai_responses_api;
use crate::api::lazy::lazy_stream;
use crate::api::openai_completions::openai_completions_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::error::AiError;
use crate::event_stream::AssistantMessageEventStream;
use crate::models::provider::ProviderApiImpl;
use crate::models::{CreateProviderOptions, Provider, ProviderStreams, create_provider};
use crate::providers::catalog_chat_models;
use crate::types::{Model, SimpleStreamOptions, StreamOptions, TranscriptContext};

/// 从 [`StreamOptions`] 的 `extra` 读取 Azure 专属字段（`AzureOpenAIResponsesOptions::from_stream_options`
/// 的同构提取，仅保留解析函数用到的字段）。
///
/// 键名是 TS 的 camelCase 字段名，值必须是字符串；其它类型按未设置处理（`as_str` 返回
/// `None`）。返回的 `base` 只复制了 `env`——`resolve_azure_base_url` 与
/// `resolve_deployment_name` 只从 env 读取 `AZURE_OPENAI_*` 配置，其余字段（超时、回调、
/// 取消令牌）留在原 `StreamOptions` 里继续传给内层。
fn azure_endpoint_options(options: &StreamOptions) -> AzureEndpointOptions {
    let string = |key: &str| {
        options
            .extra
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    AzureEndpointOptions {
        base: StreamOptions {
            env: options.env.clone(),
            ..StreamOptions::default()
        },
        azure_api_version: string("azureApiVersion"),
        azure_resource_name: string("azureResourceName"),
        azure_base_url: string("azureBaseUrl"),
        azure_deployment_name: string("azureDeploymentName"),
    }
}

/// `resolveAzureModel(model, options)`。
///
/// 复制模型并替换 `base_url`；解析失败时原样返回 [`AiError`]（`AiErrorKind::Request`，
/// 消息来自 `resolve_azure_base_url`）。模型其它字段与调用方持有的引用都不变。
#[allow(clippy::result_large_err)]
fn resolve_azure_model(model: &Model, options: &AzureEndpointOptions) -> Result<Model, AiError> {
    let mut resolved = model.clone();
    resolved.base_url = resolve_azure_base_url(model, Some(options))?;
    Ok(resolved)
}

/// Send the deployment name as the request's model, keeping `model.id` as the catalog id.
///
/// 仅当解析出的 deployment 名与 `model.id` 不同时才安装回调；相同则直接返回原选项
/// （避免无意义的闭包与克隆）。安装时先调用调用方原有的 `on_payload`，因此：
///
/// - 原回调看到的是**已经写入 deployment** 的参数副本；
/// - 原回调返回 `Some(非 null)` 时采用它的结果（调用方可以完全改写 payload）；
/// - 原回调返回 `None` 或 JSON `null` 时使用写入 deployment 后的参数。
///
/// 非对象 payload 会先被替换成空对象；正常情况下适配器总是传 JSON 对象。
fn with_deployment_name(
    model: &Model,
    mut options: StreamOptions,
    endpoint_options: &AzureEndpointOptions,
) -> StreamOptions {
    let deployment_name = resolve_deployment_name(model, Some(endpoint_options));
    // 目录 id 已经是 deployment 名时无需改写请求体，也不安装额外回调。
    if deployment_name == model.id {
        return options;
    }
    // 接管调用方已有的 on_payload：闭包先写入 deployment，再决定是否采用原回调结果。
    let deployment_name_for_async = deployment_name.clone();
    let previous = options.on_payload.take();
    options.on_payload = Some(Arc::new(move |payload: Value, payload_model: &Model| {
        // 防御非对象 payload：正常路径下适配器传入的是 JSON object。
        let mut params = match payload {
            Value::Object(map) => Value::Object(map),
            _ => Value::Object(serde_json::Map::new()),
        };
        if let Value::Object(map) = &mut params {
            map.insert("model".into(), Value::String(deployment_name.clone()));
        }
        // 原回调拿到的是改写后的副本；返回 None 或 JSON null 都表示「不覆盖」。
        match previous
            .as_ref()
            .and_then(|on_payload| on_payload(params.clone(), payload_model))
        {
            Some(value) if !value.is_null() => Some(value),
            _ => Some(params),
        }
    }));
    // 异步版回调（扩展桥）：同样先写入 deployment，再 await 原回调。
    let previous_async = options.on_payload_async.take();
    if previous_async.is_some() {
        options.on_payload_async = Some(Arc::new(move |payload: Value, payload_model: &Model| {
            let previous = previous_async.clone();
            let deployment_name = deployment_name_for_async.clone();
            Box::pin(async move {
                let mut params = match payload {
                    Value::Object(map) => Value::Object(map),
                    _ => Value::Object(serde_json::Map::new()),
                };
                if let Value::Object(map) = &mut params {
                    map.insert("model".into(), Value::String(deployment_name.clone()));
                }
                let next = match previous.as_ref() {
                    Some(on_payload) => on_payload(params.clone(), payload_model).await,
                    None => None,
                };
                match next {
                    Some(value) if !value.is_null() => Some(value),
                    _ => Some(params),
                }
            })
        }));
    }
    options
}

/// `azureStreams(streams)`：在 `lazyStream` 内解析 Azure endpoint 与 deployment。
///
/// 包装器只实现 `stream`/`stream_simple`，不转发内层实现的 `fetch_deferred`/
/// `cancel_deferred`（与 TS 返回的对象形状一致）。但不要把「没有延迟响应能力」全归因于
/// 包装器：内层由 [`openai_completions_api`] 构造，而该工厂调用的是
/// `lazy_api(load, None)`，`LazyApiCapabilities` 的 `fetch_deferred`/`cancel_deferred`
/// 两项都是 `false`（`None` 走 `Default`），即内层本身就不支持 deferred。因此 Azure 上
/// `api` 为 `openai-completions` 的模型不具备延迟响应能力；即使换成不包装的实现也一样。
pub struct AzureStreams {
    inner: Arc<dyn ProviderStreams>,
}

impl AzureStreams {
    /// 包装一个实现。
    ///
    /// `inner` 通常是 `openai_completions_api()`；构造过程不调用它，真正的分派发生在
    /// 惰性流建立之后。
    pub fn new(inner: Arc<dyn ProviderStreams>) -> Self {
        AzureStreams { inner }
    }
}

#[async_trait]
impl ProviderStreams for AzureStreams {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        let inner = Arc::clone(&self.inner);
        // setup future 必须 `'static` 且拥有自己的输入：把 model/context 克隆进去，同时保留
        // 一份 `stream_model` 给 lazy_stream 填充失败消息的元数据（api/provider/model）。
        let model = model.clone();
        let stream_model = model.clone();
        let context = context.clone();
        lazy_stream(
            &stream_model,
            Box::pin(async move {
                // 阶段 1：提取 Azure 选项（只用到 extra 的四个键与 env）。
                let endpoint_options = azure_endpoint_options(&options);
                // 阶段 2：解析 base URL；失败会变成流上的 error 事件而不是同步 Err。
                let resolved_model = resolve_azure_model(&model, &endpoint_options)?;
                // 阶段 3：按需改写 on_payload，使用 deployment 名作为请求体 model。
                let options = with_deployment_name(&model, options, &endpoint_options);
                Ok(inner.stream(&resolved_model, &context, options))
            }),
            None,
        )
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        let inner = Arc::clone(&self.inner);
        let model = model.clone();
        let stream_model = model.clone();
        let context = context.clone();
        lazy_stream(
            &stream_model,
            Box::pin(async move {
                // `SimpleStreamOptions::base` 承载同一套通用选项；解析步骤与 `stream` 相同。
                let endpoint_options = azure_endpoint_options(&options.base);
                let resolved_model = resolve_azure_model(&model, &endpoint_options)?;
                // 只替换 base，`tool_choice`/`reasoning`/`deferred` 等字段原样保留。
                let options = SimpleStreamOptions {
                    base: with_deployment_name(&model, options.base, &endpoint_options),
                    ..options
                };
                Ok(inner.stream_simple(&resolved_model, &context, options))
            }),
            None,
        )
    }
}

/// `azureStreams(streams)`。
///
/// 把任意 [`ProviderStreams`] 包成 Azure 端点感知的版本；只影响传给内层的 `model` 与
/// `options`，事件流语义完全由内层决定。每次调用新建一个包装器。
pub fn azure_streams(streams: Arc<dyn ProviderStreams>) -> Arc<dyn ProviderStreams> {
    Arc::new(AzureStreams::new(streams))
}

/// 构造 `azure` provider（展示名 `Azure`）。
///
/// api map 固定两项，按 `model.api` 精确分派：
///
/// - `azure-openai-responses` → `azure_openai_responses_api()`（端点解析在适配器内部完成）；
/// - `openai-completions` → [`azure_streams`] 包装的通用适配器。
///
/// 鉴权是标准的 `AZURE_OPENAI_API_KEY`（存储 key 优先）；模型来自编译期生成的 `azure`
/// chat 表。provider 不设 `base_url`，每个请求按资源/选项现算。`expect` 分支不可达：
/// map 非空即视为声明了 chat 能力。
///
/// TS: `providers/azure.ts#azureProvider`
pub fn azure_provider() -> Arc<dyn Provider> {
    let mut api = IndexMap::new();
    api.insert(
        "azure-openai-responses".to_string(),
        azure_openai_responses_api(),
    );
    api.insert(
        "openai-completions".to_string(),
        azure_streams(openai_completions_api()),
    );
    create_provider(CreateProviderOptions {
        id: "azure".into(),
        name: Some("Azure".into()),
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth(
                "Azure OpenAI API key",
                &["AZURE_OPENAI_API_KEY"],
            )),
            oauth: None,
        },
        models: catalog_chat_models("azure"),
        api: Some(ProviderApiImpl::ByApi(api)),
        ..Default::default()
    })
    .expect("azure provider declares an api implementation")
}
