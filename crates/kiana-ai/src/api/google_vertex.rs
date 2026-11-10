#![allow(clippy::result_large_err)]
//! api/google-vertex.ts——Vertex AI（ADC 或 API key）。
//!
//! 与 `google-generative-ai` 逐行同构（TS 侧 diff 只有 5 处差异），差异全部保留：
//! 自定义 fetch 错误串、`resolveApiKey` 而不是强制 API key、客户端选择、
//! `"Google Vertex stream ended without a finish reason"`、以及 `streamSimple` 不检查 API key。
//!
//! # 这个文件负责什么
//!
//! [`crate::api::google_shared`] 提供消息/工具转换、URL 拼接、鉴权头、SSE 解码与思考配置
//! 构造；本文件保留 Vertex 特有的客户端选择（API key 或 ADC）、项目/位置解析与专属错误
//! 文案，并复制了与 Gemini 版同构的流编排（`stream` / `run_stream` / `create_output` /
//! `done_reason`，以及 `stream_simple` 的思考选择与 `thinking_to_model_level` /
//! `model_level_to_thinking` 两个映射；这些都没有抽到共享层）。
//!
//! # 调用方
//!
//! `providers::google_vertex::google_vertex_provider` 使用 [`google_vertex_api`]；
//! `compat` 的旧全局入口也按注册表分派到这里。
//!
//! # 一次 `stream` 的执行顺序
//!
//! 1. [`stream`] 同步返回 [`AssistantMessageEventStream`]，`run_stream` 在后台任务里执行；
//! 2. 预检：拒绝自定义 `fetch`，用 [`resolve_api_key`] 过滤空串/占位符/marker；
//! 3. 有 API key → [`create_client_with_api_key`]；没有 → [`resolve_project`] 与
//!    [`resolve_location`] 后构造 ADC 客户端 [`create_client`]；
//! 4. 其余与 Gemini 版相同：`build_params` → `on_payload` → `request_stream` → `start`
//!    → SSE chunk 循环 → 终态检查 → `done` / `error`。
//!
//! # 重要约束
//!
//! - 模型目录里的 Vertex `baseUrl` 是 `https://{location}-aiplatform.googleapis.com`
//!   模板；[`create_client`] 通过 `build_vertex_http_options` 丢弃含 `{location}` 的
//!   baseUrl，改由客户端按 `location` 选主机。原样拼进 URL 会因主机名含 `{`/`}` 而
//!   无法解析。
//! - ADC 路径需要 project 与 location；缺任何一个都会在后台阶段失败并变成流上的 error
//!   事件（不是同步 `Err`）。
//! - [`stream_simple`] 不检查 API key，也不填 `project`/`location` 字段
//!   （`SimpleStreamOptions` 没有这两个字段），ADC 只能读 `options.base.env` 或进程环境
//!   变量。
//!
//! # 失败模式
//!
//! - 缺 API key 不失败；ADC 路径在以下情况失败（都在后台阶段变成流上的 error 事件）：
//!   缺 project/location（文案见 [`resolve_project`] / [`resolve_location`]），或凭据链
//!   无法解析——没有可用的默认 ADC 文件且 metadata server 不可用（`NO_ADC_FOUND`）、
//!   凭据文件读取失败、token 换取请求失败（`google_shared::vertex_access_token`）；
//! - 非 2xx、流内错误、残缺 SSE、未知 finishReason 的处理与 Gemini 版相同，见
//!   [`crate::api::google_shared`]；
//! - 读完流仍未收到 finishReason 时的文案是
//!   `"Google Vertex stream ended without a finish reason"`（Gemini 版没有 `Vertex`）。

use std::sync::Arc;

use serde_json::Value;

use crate::api::google_generative_ai::parse_google_thinking;
use crate::api::google_shared::{
    GoogleBudgetFlavor, GoogleClientConfig, GoogleFailure, GoogleParamsOptions, GoogleStreamState,
    GoogleThinkingOption, build_params, build_vertex_http_options, finish_google_block,
    get_google_budget, handle_google_chunk, read_google_events, request_stream,
    resolve_google_thinking_level, to_google_thinking_level, uses_google_thinking_level,
};
use crate::api::lazy::{lazy_stream_error, spawn_forward};
use crate::api::simple_options::build_base_options;
use crate::error::AiError;
use crate::event_stream::AssistantMessageEventStream;
use crate::models::{ProviderStreams, clamp_thinking_level};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, ErrorReason, Model, ModelThinkingLevel,
    SimpleStreamOptions, StopReason, StreamOptions, ThinkingLevel, TranscriptContext, Usage,
};
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::transcript::collapse_system_messages;

/// `API_VERSION`。
const API_VERSION: &str = "v1";
/// `GCP_VERTEX_CREDENTIALS_MARKER`。
const GCP_VERTEX_CREDENTIALS_MARKER: &str = "gcp-vertex-credentials";

/// `GoogleVertexOptions`。
#[derive(Default)]
pub struct GoogleVertexOptions {
    /// 通用的流选项：鉴权、provider env、头、取消、回调、重试与超时。
    pub base: StreamOptions,
    /// `"auto" | "none" | "any"`；进入 `toolConfig.functionCallingConfig.mode` 的解析。
    pub tool_choice: Option<String>,
    /// 思考配置；`None` 表示完全不发 `thinkingConfig`。
    pub thinking: Option<GoogleThinkingOption>,
    /// ADC 项目 ID；`None` 或空串时回退 `GOOGLE_CLOUD_PROJECT` / `GCLOUD_PROJECT`。
    pub project: Option<String>,
    /// ADC 区域；`None` 或空串时回退 `GOOGLE_CLOUD_LOCATION`。
    ///
    /// 只在 ADC 路径使用：有 API key 时 [`create_client_with_api_key`] 不设置 location，
    /// 主机固定为 `aiplatform.googleapis.com`。
    pub location: Option<String>,
}

/// `stream(model, context, options)`。
///
/// 同步返回事件流：请求、SSE 解析与事件推送都在后台任务里执行。事件序列与
/// [`crate::api::google_generative_ai::stream`] 相同（`start` → 增量 → `done`/`error`）。
///
/// # 与 Gemini 版的关键差异
///
/// - **不**要求 API key：[`resolve_api_key`] 返回 `None` 时走 ADC；此时 project/location
///   缺失，或凭据链无法解析（无可用默认 ADC 文件且 metadata server 不可用 → `NO_ADC_FOUND`、
///   凭据文件读取失败、token 请求失败）时才失败；
/// - 自定义 `fetch` 的错误文案是 `"Custom fetch is not supported by the Google Vertex
///   adapter"`；
/// - 失败同样在流上产生 `error` 事件，`ErrorReason` 按取消令牌是否触发放置
///   `Aborted`/`Error`。
pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: GoogleVertexOptions,
) -> AssistantMessageEventStream {
    let event_stream = AssistantMessageEventStream::new();
    let model = model.clone();
    let context = context.clone();
    let stream_for_task = event_stream.clone();
    spawn_forward(async move {
        let signal = options.base.signal.clone();
        let mut output = create_output(&model);
        match run_stream(&model, &context, options, &stream_for_task, &mut output).await {
            Ok(()) => {}
            Err(error) => {
                output.stop_reason = if signal.as_ref().is_some_and(|s| s.is_cancelled()) {
                    StopReason::Aborted
                } else {
                    StopReason::Error
                };
                output.error_message = Some(error.display());
                let reason = if output.stop_reason == StopReason::Aborted {
                    ErrorReason::Aborted
                } else {
                    ErrorReason::Error
                };
                let _ = stream_for_task.push(AssistantMessageEvent::Error {
                    reason,
                    error: Box::new(output),
                });
                stream_for_task.end_without_result();
            }
        }
    });
    event_stream
}

// 初始消息只用于 `start` 事件的 partial 与后续增量累积；`api` 是固定判别串
// `"google-vertex"`（不是 model.api），`stop_reason` 先置 Pending，解析 chunk 时改写。
fn create_output(model: &Model) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: "google-vertex".into(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage: Usage::default(),
        stop_reason: StopReason::Pending,
        timestamp: now_unix_ms(),
        ..AssistantMessage::default()
    }
}

async fn run_stream(
    model: &Model,
    context: &TranscriptContext,
    options: GoogleVertexOptions,
    event_stream: &AssistantMessageEventStream,
    output: &mut AssistantMessage,
) -> Result<(), GoogleFailure> {
    // 阶段 1：预检。与 Gemini 版一样，任何 `Some(fetch)` 都按自定义实现拒绝。
    if options.base.fetch.is_some() {
        return Err(GoogleFailure::new(
            "Custom fetch is not supported by the Google Vertex adapter",
        ));
    }
    // 注意：Vertex 版本**没有** `No API key for provider` 检查；API key 只用于选客户端。
    // `resolve_api_key` 会把空串/占位符/marker 归成 None，此时走 ADC：先解析项目与
    // 位置，二者的失败同样变成流上的 error 事件（缺 key 本身不是错误）。
    let api_key = resolve_api_key(&options);
    let client = match &api_key {
        Some(api_key) => create_client_with_api_key(
            model,
            api_key,
            options.base.headers.as_ref(),
            options.base.identity.as_ref(),
        ),
        None => {
            let project = resolve_project(&options)?;
            let location = resolve_location(&options)?;
            create_client(
                model,
                &project,
                &location,
                options.base.headers.as_ref(),
                options.base.env.as_ref(),
                options.base.identity.as_ref(),
            )
        }
    };

    // 阶段 2：与 Gemini 版相同的请求体构造（共享 build_params + on_payload 整体替换）。
    let normalized_context = collapse_system_messages(context);
    let params_options = GoogleParamsOptions {
        temperature: options.base.temperature,
        max_tokens: options.base.max_tokens,
        tool_choice: options.tool_choice.clone(),
        thinking: options.thinking.clone(),
        signal_aborted: options
            .base
            .signal
            .as_ref()
            .is_some_and(|signal| signal.is_cancelled()),
    };
    let mut params = build_params(model, &normalized_context, &params_options)
        .map_err(|error| GoogleFailure::new(error.message))?;
    if let Some(next) = options.base.apply_on_payload(params.clone(), model).await {
        params = next;
    }

    // 阶段 3：发请求；重试语义与 Gemini 版一致（只覆盖等响应头，读 body 不重试）。
    let body = request_stream(&client, &params, &options.base).await?;
    let _ = event_stream.push(AssistantMessageEvent::Start {
        partial: Box::new(output.clone()),
    });
    // 阶段 4：SSE chunk 循环。回调顺序与错误暂存方式同 Gemini 版：先
    // onProviderStreamEvent，再 handle_google_chunk；chunk 级错误存进 stream_error
    // 并让读取提前停止。
    let mut state = GoogleStreamState::default();
    let on_provider_event = options.base.on_provider_stream_event.clone();
    let model_for_events = model.clone();
    let mut stream_error: Option<GoogleFailure> = None;
    let result = read_google_events(body, options.base.signal.clone(), |chunk| {
        if let Some(callback) = &on_provider_event {
            callback(chunk.clone(), &model_for_events);
        }
        if let Err(error) = handle_google_chunk(model, output, event_stream, &mut state, &chunk) {
            stream_error = Some(GoogleFailure::new(error.message));
            return Ok(true);
        }
        Ok(false)
    })
    .await;
    if let Some(error) = stream_error {
        return Err(error);
    }
    result?;

    // 阶段 5：终态。先补发未关闭块的 end 事件，再依次检查取消、缺 finishReason 与
    // aborted/error；只有缺 finishReason 的文案带 `Vertex`，其余与 Gemini 版相同
    // （当前稳定契约，改动需同步测试）。
    if let Some(kind) = state.current_block {
        finish_google_block(kind, output, event_stream);
    }

    // 取消令牌可能在读完流之后才被观察到；TS 在这里检查 signal.aborted。
    if options
        .base
        .signal
        .as_ref()
        .is_some_and(|signal| signal.is_cancelled())
    {
        return Err(GoogleFailure::new("Request was aborted"));
    }
    if output.stop_reason == StopReason::Pending {
        return Err(GoogleFailure::new(
            "Google Vertex stream ended without a finish reason",
        ));
    }
    if matches!(output.stop_reason, StopReason::Aborted | StopReason::Error) {
        let error_message = match &output.raw_stop_reason {
            Some(raw) => format!("Provider stopped with: {raw}"),
            None => "An unknown error occurred".to_string(),
        };
        return Err(GoogleFailure::new(error_message));
    }
    let _ = event_stream.push(AssistantMessageEvent::Done {
        reason: done_reason(output.stop_reason),
        message: Box::new(output.clone()),
    });
    event_stream.end_without_result();
    Ok(())
}

// 把内部 StopReason 投射成 done 事件允许的子集。Pending/Aborted/Error 已在调用前被拒绝，
// 因此落到默认分支的只可能是 Stop。
fn done_reason(stop_reason: StopReason) -> crate::types::DoneReason {
    match stop_reason {
        StopReason::Length => crate::types::DoneReason::Length,
        StopReason::ToolUse => crate::types::DoneReason::ToolUse,
        StopReason::Deferred => crate::types::DoneReason::Deferred,
        _ => crate::types::DoneReason::Stop,
    }
}

/// `streamSimple(model, context, options)`。
///
/// 与 Gemini 版不同：**不**检查 API key，且 `buildBaseOptions(..., undefined)`。
///
/// # 行为
///
/// - `buildBaseOptions(..., None)` 表示不使用额外 API key，`base.api_key` 只保留调用方
///   原有的值；缺 key 时继续走 ADC，由 `resolve_project`/`resolve_location` 或服务端
///   决定成败；
/// - 构造的 [`GoogleVertexOptions`] 把 `project`/`location` 置为 `None`
///   （`SimpleStreamOptions` 没有这两个字段），ADC 目标只能来自 `base.env` 或进程环境
///   变量；
/// - 预算表用 [`GoogleBudgetFlavor::Vertex`]（没有 `2.5-flash-lite` 分支）；
/// - 其余（`reasoning` 缺省或 `clamp_thinking_level` 得到 `Off` → 禁用思考、离散
///   `level` 与 token 预算二选一）与 Gemini 版一致。
///
/// `options.tool_choice` 只有 `Auto`/`None` 两个变体（Rust 的 `ToolChoice`），这条路径
/// 无法表达 `"any"`；需要 `"any"` 时直接调用 [`stream`] 并自己构造
/// [`GoogleVertexOptions`]。
///
/// # 错误
///
/// 唯一的同步错误来自 [`resolve_google_thinking_level`]（如 `xhigh`/`max` 没有模型级
/// 映射），返回 `crate::error::AiErrorKind::Request`；缺 key/项目/位置都不会在这里同步
/// 失败。
pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
) -> Result<AssistantMessageEventStream, AiError> {
    let base = build_base_options(model, context, Some(&options), None);
    let tool_choice = options.tool_choice.map(|choice| match choice {
        crate::types::ToolChoice::Auto => "auto".to_string(),
        crate::types::ToolChoice::None => "none".to_string(),
    });

    let Some(reasoning) = options.reasoning else {
        return Ok(stream(
            model,
            context,
            GoogleVertexOptions {
                base,
                tool_choice,
                thinking: Some(GoogleThinkingOption {
                    enabled: false,
                    ..Default::default()
                }),
                project: None,
                location: None,
            },
        ));
    };
    let clamped = clamp_thinking_level(model, thinking_to_model_level(reasoning));
    if clamped == ModelThinkingLevel::Off {
        return Ok(stream(
            model,
            context,
            GoogleVertexOptions {
                base,
                tool_choice,
                thinking: Some(GoogleThinkingOption {
                    enabled: false,
                    ..Default::default()
                }),
                project: None,
                location: None,
            },
        ));
    }
    let resolved = resolve_google_thinking_level(model, model_level_to_thinking(clamped))?;
    let thinking = if uses_google_thinking_level(model) {
        GoogleThinkingOption {
            enabled: true,
            budget_tokens: None,
            level: Some(to_google_thinking_level(resolved)),
        }
    } else {
        GoogleThinkingOption {
            enabled: true,
            budget_tokens: Some(get_google_budget(
                model,
                resolved,
                options.thinking_budgets.as_ref(),
                GoogleBudgetFlavor::Vertex,
            )),
            level: None,
        }
    };
    Ok(stream(
        model,
        context,
        GoogleVertexOptions {
            base,
            tool_choice,
            thinking: Some(thinking),
            project: None,
            location: None,
        },
    ))
}

// ThinkingLevel → ModelThinkingLevel：一对一映射，`ModelThinkingLevel::Off` 没有对应输入。
fn thinking_to_model_level(level: ThinkingLevel) -> ModelThinkingLevel {
    match level {
        ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
        ThinkingLevel::Low => ModelThinkingLevel::Low,
        ThinkingLevel::Medium => ModelThinkingLevel::Medium,
        ThinkingLevel::High => ModelThinkingLevel::High,
        ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
        ThinkingLevel::Max => ModelThinkingLevel::Max,
    }
}

// ModelThinkingLevel → ThinkingLevel。`Off` 映射成 `Minimal` 只是让函数保持全量：
// 调用点已先对 `clamped == Off` 提前返回，不会用这个分支表达“关闭思考”。
fn model_level_to_thinking(level: ModelThinkingLevel) -> ThinkingLevel {
    match level {
        ModelThinkingLevel::Off => ThinkingLevel::Minimal,
        ModelThinkingLevel::Minimal => ThinkingLevel::Minimal,
        ModelThinkingLevel::Low => ThinkingLevel::Low,
        ModelThinkingLevel::Medium => ThinkingLevel::Medium,
        ModelThinkingLevel::High => ThinkingLevel::High,
        ModelThinkingLevel::Xhigh => ThinkingLevel::Xhigh,
        ModelThinkingLevel::Max => ThinkingLevel::Max,
    }
}

// ---------------------------------------------------------------------------
// 客户端
// ---------------------------------------------------------------------------

/// `createClient(model, project, location, optionsHeaders?, env?, identity?)`。
///
/// 构造 ADC 客户端配置（不建立连接、不缓存 token；每次请求都会重新走凭据链）：
///
/// - `vertexai = true`：URL 按 Vertex 规则拼 `publishers/...`，并可能加
///   `projects/<project>/locations/<location>` 前缀；
/// - `api_key = None`：鉴权走 ADC，`request_stream` 会取 bearer token；
/// - `project`/`location` 决定主机名（`us`/`eu` 用 `rep.googleapis.com`）与资源路径；
/// - `api_version = "v1"`；
/// - `google_auth_key_filename` 来自 [`build_google_auth_options`]；
/// - `http_options` 由 `build_vertex_http_options` 合并 identity 头、model 头与
///   `options_headers`，并处理自定义 baseUrl。
pub fn create_client(
    model: &Model,
    project: &str,
    location: &str,
    options_headers: Option<&crate::types::ProviderHeaders>,
    env: Option<&crate::types::ProviderEnv>,
    identity: Option<&crate::types::ClientIdentity>,
) -> GoogleClientConfig {
    GoogleClientConfig {
        vertexai: true,
        api_key: None,
        project: Some(project.to_string()),
        location: Some(location.to_string()),
        api_version: Some(API_VERSION.to_string()),
        google_auth_key_filename: build_google_auth_options(env),
        http_options: build_vertex_http_options(model, options_headers, identity),
    }
}

/// `createClientWithApiKey(model, apiKey, optionsHeaders?, identity?)`。
///
/// 与 [`create_client`] 相对：API key 优先于 ADC，`project`/`location` 留空。因此 URL
/// 不带 `projects/<p>/locations/<l>` 前缀，主机固定为 `aiplatform.googleapis.com`
/// （见 `GoogleClientConfig::request_base_with` 的 API key 分支）。
pub fn create_client_with_api_key(
    model: &Model,
    api_key: &str,
    options_headers: Option<&crate::types::ProviderHeaders>,
    identity: Option<&crate::types::ClientIdentity>,
) -> GoogleClientConfig {
    GoogleClientConfig {
        vertexai: true,
        api_key: Some(api_key.to_string()),
        project: None,
        location: None,
        api_version: Some(API_VERSION.to_string()),
        google_auth_key_filename: None,
        http_options: build_vertex_http_options(model, options_headers, identity),
    }
}

/// `buildGoogleAuthOptions(env)`：`GOOGLE_APPLICATION_CREDENTIALS` → `keyFilename`。
///
/// 查询顺序（见 `get_provider_env_value`）：非空的 provider env → 非空的进程 env →
/// Bun 沙箱回退；空串视为缺失。有值时作为 `GoogleAuthOptions.keyFilename`；没有时
/// 走 ADC 默认链：gcloud 的 `application_default_credentials.json` → GCE metadata
/// server（见 `google_shared` 的 `vertex_access_token`）。
pub fn build_google_auth_options(env: Option<&crate::types::ProviderEnv>) -> Option<String> {
    get_provider_env_value("GOOGLE_APPLICATION_CREDENTIALS", env)
}

/// `resolveApiKey(options)`。
///
/// # 判定
///
/// 先用 JS `trim` 去掉首尾空白；结果满足任一条件时返回 `None`，表示走 ADC：
///
/// - 空串；
/// - 等于 `"gcp-vertex-credentials"`（凭证链使用的 marker）；
/// - 匹配 `^<[^>]+>$`（任何尖括号包裹的非空占位符，如 `<authenticated>`）。
///
/// 否则返回**去空白后**的 key。`tests/google_vertex_api_key_resolution.rs` 锁定了
/// marker 与占位符两类输入都会回退 ADC。
pub fn resolve_api_key(options: &GoogleVertexOptions) -> Option<String> {
    let api_key = options
        .base
        .api_key
        .as_deref()
        .map(kiana_json::js_string::js_trim)
        .unwrap_or("");
    if api_key.is_empty()
        || api_key == GCP_VERTEX_CREDENTIALS_MARKER
        || is_placeholder_api_key(api_key)
    {
        return None;
    }
    Some(api_key.to_string())
}

/// `isPlaceholderApiKey(apiKey)`：`/^<[^>]+>$/`。
///
/// 只匹配整串由尖括号包住、中间非空的占位符；`<` 出现在其它位置的真实 key 不会被误判。
/// 正则编译失败会 panic（`expect("valid regex")`），但模式是常量，运行期不会失败。
fn is_placeholder_api_key(api_key: &str) -> bool {
    static PLACEHOLDER: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"^<[^>]+>$").expect("valid regex"));
    PLACEHOLDER.is_match(api_key)
}

/// `resolveProject(options)`。
///
/// 优先级：`options.project`（空串按缺失）→ `GOOGLE_CLOUD_PROJECT` →
/// `GCLOUD_PROJECT`。环境查询先看 `options.base.env`，再看进程 env（空串视为缺失）。
/// 都没有时返回 [`GoogleFailure`]，消息即用户可见文案
/// （`Vertex AI requires a project ID. ...`）。
pub fn resolve_project(options: &GoogleVertexOptions) -> Result<String, GoogleFailure> {
    let project = options
        .project
        .clone()
        .filter(|value| !value.is_empty())
        .or_else(|| get_provider_env_value("GOOGLE_CLOUD_PROJECT", options.base.env.as_ref()))
        .or_else(|| get_provider_env_value("GCLOUD_PROJECT", options.base.env.as_ref()));
    project.ok_or_else(|| {
        GoogleFailure::new(
            "Vertex AI requires a project ID. Set GOOGLE_CLOUD_PROJECT/GCLOUD_PROJECT or pass project in options.",
        )
    })
}

/// `resolveLocation(options)`。
///
/// 优先级：`options.location`（空串按缺失）→ `GOOGLE_CLOUD_LOCATION`（先看
/// `options.base.env`，再看进程 env）。都没有时返回 [`GoogleFailure`]，消息即用户可见
/// 文案（`Vertex AI requires a location. ...`）。区域同时决定 ADC 主机名与资源路径。
pub fn resolve_location(options: &GoogleVertexOptions) -> Result<String, GoogleFailure> {
    let location = options
        .location
        .clone()
        .filter(|value| !value.is_empty())
        .or_else(|| get_provider_env_value("GOOGLE_CLOUD_LOCATION", options.base.env.as_ref()));
    location.ok_or_else(|| {
        GoogleFailure::new(
            "Vertex AI requires a location. Set GOOGLE_CLOUD_LOCATION or pass location in options.",
        )
    })
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// `google-vertex.lazy.ts`：`lazyApi(() => import("./google-vertex.ts"))`。
///
/// - `stream`：把 [`StreamOptions::extra`] 里的 Vertex 专属字段解析成
///   [`GoogleVertexOptions`]（见 [`google_vertex_options_from_stream_options`]），其余
///   字段原样传递；
/// - `stream_simple`：把同步 `Err`（只可能来自思考等级映射）用 `lazy_stream_error`
///   映射成流上的 `error` 事件，因为冻结的 trait 签名没有 `Result` 通道。
pub fn google_vertex_api() -> Arc<dyn ProviderStreams> {
    Arc::new(GoogleVertexProvider)
}

struct GoogleVertexProvider;

impl ProviderStreams for GoogleVertexProvider {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        stream(
            model,
            context,
            google_vertex_options_from_stream_options(options),
        )
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        match stream_simple(model, context, options) {
            Ok(stream) => stream,
            Err(error) => lazy_stream_error(model, error),
        }
    }
}

/// [`StreamOptions`] → [`GoogleVertexOptions`]：API 专属字段从 `extra` 里按 TS 字段名取。
///
/// - `"toolChoice"` → `tool_choice`（字符串；没有 strict 工具时，未知的非空值由
///   `map_tool_choice` 回退 `AUTO`）；
/// - `"thinking"` → [`parse_google_thinking`]（与 Gemini 版共用）；
/// - `"project"` / `"location"` → ADC 选项，非字符串值按缺省处理。
///
/// `extra` 被整体取出（`std::mem::take`）：未识别的键不会进入请求，也不会留在
/// `base.extra` 里。
pub fn google_vertex_options_from_stream_options(
    mut options: StreamOptions,
) -> GoogleVertexOptions {
    let extra = std::mem::take(&mut options.extra);
    let tool_choice = extra
        .get("toolChoice")
        .and_then(Value::as_str)
        .map(str::to_string);
    let thinking = parse_google_thinking(extra.get("thinking"));
    let project = extra
        .get("project")
        .and_then(Value::as_str)
        .map(str::to_string);
    let location = extra
        .get("location")
        .and_then(Value::as_str)
        .map(str::to_string);
    GoogleVertexOptions {
        base: options,
        tool_choice,
        thinking,
        project,
        location,
    }
}
