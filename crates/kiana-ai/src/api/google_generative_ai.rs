#![allow(clippy::result_large_err)]
//! api/google-generative-ai.ts——Gemini（Google AI Studio / Generative Language API）。
//!
//! Rust 侧不用 `@google/genai` SDK：客户端构造参数由
//! [`crate::api::google_shared::GoogleClientConfig`] 承载，请求走
//! `POST {base}/{version}/models/{model}:streamGenerateContent?alt=sse`。
//!
//! # 这个文件负责什么
//!
//! 把 `Model` + `TranscriptContext` + [`GoogleOptions`] 编排成一次 Gemini 流式请求：
//! 预检、构造请求体、发请求、消费 SSE、判定终态。消息/工具转换、思考配置、URL 拼接、
//! 鉴权头与 SSE 解码都在 [`crate::api::google_shared`]；本文件不重复这些逻辑。
//!
//! # 调用方
//!
//! `providers::google::google_provider`、`providers::opencode` / `opencode_go` 使用
//! [`google_generative_ai_api`]；`compat` 的旧全局入口也按注册表分派到这里。
//!
//! # 一次 `stream` 的执行顺序
//!
//! 1. [`stream`] 同步返回 [`AssistantMessageEventStream`]；真正的请求在 `spawn_forward`
//!    里执行（有 tokio runtime 时 `spawn`，否则专用线程 + current-thread runtime）。
//! 2. 预检：拒绝自定义 `fetch`，要求非空 `api_key`，按 model 构造客户端配置。
//! 3. `collapse_system_messages` 归一化上下文 → `build_params` 生成请求体；
//!    `on_payload` 返回 `Some` 时整体替换请求体。
//! 4. `request_stream` 发请求（重试只覆盖“等到响应头”阶段）；成功后才推 `start`。
//! 5. `read_google_events` 读 SSE：每个 chunk 先回调 `on_provider_stream_event`，再交给
//!    `handle_google_chunk` 更新 output 并推 text/thinking/toolCall 增量事件。
//! 6. 结束后补发未关闭块的 end 事件，检查取消 / 缺 finishReason / aborted-error，
//!    推 `done` 并结束流。
//!
//! 任一步失败都落在 [`stream`] 的错误分支：按取消令牌是否已触发选择 `Aborted` 或
//! `Error`，把当前 output 包进 `error` 事件后结束流。
//!
//! # 与 TS 的差异
//!
//! - TS 用 `try/catch` 包住整个异步体；Rust 用 `run_stream` 的 `Result` + 外层统一
//!   生成错误事件，错误文案与 TS 相同。
//! - TS 允许显式传 `globalThis.fetch`（与默认实现同一对象时放行）；Rust 的 `fetch`
//!   是 `Option<Arc<dyn HttpClient>>`，无法做身份比较，任何 `Some` 都判为自定义实现。
//! - `stream_simple` 的“缺 API key 同步抛错”在 Rust 用 `Result<_, AiError>` 表达；
//!   [`google_generative_ai_api`] 再把它映射成流上的 error 事件。
//!
//! # 失败模式
//!
//! - 缺 key：`stream` 产生 error 事件；`stream_simple` 直接返回 `Err`。
//! - 非 2xx：`request_stream` 把响应按 SDK 的 `ApiError` 形状转成 `GoogleFailure`。
//!   只有 4xx/5xx（400–599）的响应才带 `status`（`google_shared::throw_error_if_not_ok`
//!   只在 400 <= status < 600 时调用 `with_status`）；其余非 2xx（如 1xx/3xx 中的 304）
//!   与 JSON 解析失败不带 `status`，因此不会重试。带 status 且属于 408/409/429/5xx 的
//!   错误才进入重试判断，而 `max_retries` 默认是 0（实际仍不重试）。
//! - 流内 `error.code`、非法 JSON、残缺 SSE：`read_google_events` 返回 `GoogleFailure`。
//! - 流读完仍没有 finishReason：`"Google stream ended without a finish reason"`。

use std::sync::Arc;

use serde_json::Value;

use crate::api::google_shared::{
    GoogleBudgetFlavor, GoogleFailure, GoogleParamsOptions, GoogleStreamState,
    GoogleThinkingOption, build_params, create_gemini_client, finish_google_block,
    get_google_budget, handle_google_chunk, read_google_events, request_stream,
    resolve_google_thinking_level, to_google_thinking_level, uses_google_thinking_level,
};
use crate::api::lazy::{lazy_stream_error, spawn_forward};
use crate::api::simple_options::build_base_options;
use crate::error::{AiError, AiErrorKind};
use crate::event_stream::AssistantMessageEventStream;
use crate::models::{ProviderStreams, clamp_thinking_level};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, AssistantRole, ErrorReason, Model, ModelThinkingLevel,
    SimpleStreamOptions, StopReason, StreamOptions, ThinkingLevel, TranscriptContext, Usage,
};
use crate::utils::transcript::collapse_system_messages;

/// `GoogleOptions`。
#[derive(Default)]
pub struct GoogleOptions {
    /// 通用的流选项：鉴权、provider env、头、取消、回调、重试与超时。
    pub base: StreamOptions,
    /// `"auto" | "none" | "any"`。
    ///
    /// 原样交给 `google_shared::resolve_google_function_calling_mode`：`none`/`any`
    /// 优先于 strict 模式；没有 strict 工具时，其它非空字符串由 `map_tool_choice`
    /// 回退成 `AUTO`。
    pub tool_choice: Option<String>,
    /// 思考配置；`None` 表示完全不发 `thinkingConfig`（与 `enabled: false` 不同，
    /// 后者在支持思考的模型上会发“关闭思考”的配置）。
    pub thinking: Option<GoogleThinkingOption>,
}

/// `stream(model, context, options)`。
///
/// 同步返回事件流：请求、SSE 解析与事件推送都在后台任务里执行。
///
/// # 输入
///
/// - `model`、`context`：函数先克隆两者，调用方可在返回后继续复用；
/// - `options`：通用 [`StreamOptions`] 加上 Gemini 专属的 `tool_choice`/`thinking`。
///   `options.base.fetch` 必须为 `None`（否则 error 事件），`options.base.api_key`
///   必须非空（否则 error 事件）。
///
/// # 输出与副作用
///
/// - 请求成功后先收到 `start`（partial 是初始 output），随后是增量事件；正常结束推
///   `done`，失败推 `error` 并结束流；
/// - `error` 的 `ErrorReason` 在取消令牌已触发时为 `Aborted`，否则为 `Error`；
///   `error_message` 是 `GoogleFailure` 的显示文本。
///
/// 缺少 API key 在这里**不会**同步失败（与 [`stream_simple`] 不同），而是在流上产生
/// error 事件。
pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: GoogleOptions,
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

// 初始消息只用于 `start` 事件的 partial 和后续增量累积：`stop_reason` 先置 Pending，
// 收到 finishReason 后由 `handle_google_chunk` 改写；`api` 是固定判别串（不是 model.api），
// `timestamp` 是请求开始时刻。最终消息的 `duration_ms` 由事件流按自己的开始时间补写，
// 前提是 `timestamp` 不早于流的开始时间。
fn create_output(model: &Model) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: "google-generative-ai".into(),
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
    options: GoogleOptions,
    event_stream: &AssistantMessageEventStream,
    output: &mut AssistantMessage,
) -> Result<(), GoogleFailure> {
    // 阶段 1：预检。Rust 的 `fetch` 没有 `globalThis.fetch` 可供身份比较，任何 Some 都
    // 按自定义实现拒绝；TS 只在 fetch 不是全局实现时抛错。
    if options.base.fetch.is_some() {
        return Err(GoogleFailure::new(
            "Custom fetch is not supported by the Google Generative AI adapter",
        ));
    }
    // Gemini 必须显式提供 key（Vertex 版没有这一步）；空串按缺省处理。
    let api_key = options
        .base
        .api_key
        .clone()
        .filter(|key| !key.is_empty())
        .ok_or_else(|| {
            GoogleFailure::new(format!("No API key for provider: {}", model.provider))
        })?;
    // 客户端只是构造参数（模式、key、baseUrl、头），不建立连接；URL 在 request_stream
    // 里按 model / 请求级 httpOptions 重新计算。
    let client = create_gemini_client(
        model,
        Some(&api_key),
        options.base.headers.as_ref(),
        options.base.identity.as_ref(),
    );
    // 阶段 2：构造请求体。system 消息先收敛成单条 systemInstruction；build_params 负责
    // 消息/工具/思考配置转换，on_payload 可以整体替换结果（返回 None 表示不改）。
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

    // 阶段 3：发请求并拿到响应体流。重试只覆盖“等到响应头”这一段（见 request_stream）；
    // 读 body 阶段的错误由 read_google_events 处理，不会重试。start 事件在拿到流之后
    // 才推，与 TS 的顺序一致。
    let body = request_stream(&client, &params, &options.base).await?;
    let _ = event_stream.push(AssistantMessageEvent::Start {
        partial: Box::new(output.clone()),
    });
    // 阶段 4：逐 chunk 读 SSE。回调返回 Ok(true) 会立即停止读取；chunk 处理错误先暂存成
    // stream_error（闭包的错误类型固定是 GoogleFailure），等读取返回后再统一返回。
    let mut state = GoogleStreamState::default();
    let on_provider_event = options.base.on_provider_stream_event.clone();
    let model_for_events = model.clone();
    let mut stream_error: Option<GoogleFailure> = None;
    let result = read_google_events(body, options.base.signal.clone(), |chunk| {
        // TS 顺序：先回调 onProviderStreamEvent（拿到原始 chunk），再进入状态机。
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

    // 阶段 5：终态。流可能停在块中间，先补发对应的 end 事件；随后按 TS 的顺序检查取消、
    // 是否缺少 finishReason、以及 aborted/error 停止原因。
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
    // 一个 chunk 都没带 finishReason：TS 视为错误，避免把“空回复”当成正常结束。
    if output.stop_reason == StopReason::Pending {
        return Err(GoogleFailure::new(
            "Google stream ended without a finish reason",
        ));
    }
    // 模型侧给出的 aborted/error 停止原因没有独立的错误来源，改用 raw_stop_reason 拼出
    // 用户可见文案；没有 raw 值时用固定文案。
    if matches!(output.stop_reason, StopReason::Aborted | StopReason::Error) {
        let error_message = match &output.raw_stop_reason {
            Some(raw) => format!("Provider stopped with: {raw}"),
            None => "An unknown error occurred".to_string(),
        };
        return Err(GoogleFailure::new(error_message));
    }
    // 到这里 stop_reason 必为 Stop/Length/ToolUse/Deferred 之一，可以映射成 done 事件。
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

// ModelThinkingLevel → ThinkingLevel。`Off` 映射成 `Minimal` 只是让函数保持全量：
// 调用点已先对 `clamped == Off` 提前返回，不会用这个分支表达“关闭思考”。
fn model_to_thinking_level(level: ModelThinkingLevel) -> ThinkingLevel {
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

/// `streamSimple(model, context, options)`。
///
/// 缺少 API key 时与 TS 一样**同步**失败。
///
/// # 与 `stream` 的关系
///
/// 先把 [`SimpleStreamOptions`] 展开成 [`GoogleOptions`]（`build_base_options` 负责
/// 采样参数与上下文上限钳制，并写入这里已校验的 API key），再按下面的顺序决定 thinking：
///
/// 1. 没有 `reasoning` → `thinking.enabled = false`；
/// 2. `clamp_thinking_level` 得到 `ModelThinkingLevel::Off`（模型的可用等级里没有更高档，
///    通常是 `reasoning != Some(true)`）→ 同样禁用；
/// 3. 否则解析等级：`uses_google_thinking_level` 为真时发离散 `level`，否则按
///    `get_google_budget` 发 token 预算（`GoogleBudgetFlavor::GenerativeAi`，比 Vertex
///    多一个 `2.5-flash-lite` 分支）。
///
/// 即使 `reasoning` 缺省，也会显式传 `thinking: Some { enabled: false }`——对支持思考
/// 的模型这会生成“关闭思考”的配置，而不是完全不发 `thinkingConfig`。
///
/// `options.tool_choice` 的类型只有 `Auto`/`None` 两个变体（Rust 的 `ToolChoice`），
/// 因此这条路径无法表达 `"any"`；需要 `"any"` 时直接调用 [`stream`] 并自己构造
/// [`GoogleOptions`]。
///
/// # 错误
///
/// - 缺少非空 API key：返回 [`AiErrorKind::Auth`]，消息
///   `No API key for provider: <provider>`（TS 在这里同步 throw）；
/// - [`resolve_google_thinking_level`] 失败（如 `xhigh`/`max` 没有模型级映射）：
///   返回 [`AiErrorKind::Request`]。
///
/// 其余失败都发生在返回的流上，以 error 事件出现。
pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
) -> Result<AssistantMessageEventStream, AiError> {
    let api_key = options
        .base
        .api_key
        .clone()
        .filter(|key| !key.is_empty())
        .ok_or_else(|| {
            AiError::new(
                AiErrorKind::Auth,
                format!("No API key for provider: {}", model.provider),
            )
        })?;
    let base = build_base_options(model, context, Some(&options), Some(&api_key));
    let tool_choice = options.tool_choice.map(|choice| match choice {
        crate::types::ToolChoice::Auto => "auto".to_string(),
        crate::types::ToolChoice::None => "none".to_string(),
    });

    let Some(reasoning) = options.reasoning else {
        return Ok(stream(
            model,
            context,
            GoogleOptions {
                base,
                tool_choice,
                thinking: Some(GoogleThinkingOption {
                    enabled: false,
                    ..Default::default()
                }),
            },
        ));
    };
    let clamped = clamp_thinking_level(model, model_to_thinking_level_to_model(reasoning));
    if clamped == ModelThinkingLevel::Off {
        return Ok(stream(
            model,
            context,
            GoogleOptions {
                base,
                tool_choice,
                thinking: Some(GoogleThinkingOption {
                    enabled: false,
                    ..Default::default()
                }),
            },
        ));
    }
    let resolved = resolve_google_thinking_level(model, model_to_thinking_level(clamped))?;
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
                GoogleBudgetFlavor::GenerativeAi,
            )),
            level: None,
        }
    };
    Ok(stream(
        model,
        context,
        GoogleOptions {
            base,
            tool_choice,
            thinking: Some(thinking),
        },
    ))
}

// ThinkingLevel → ModelThinkingLevel：一对一映射，`ModelThinkingLevel::Off` 没有对应输入。
fn model_to_thinking_level_to_model(level: ThinkingLevel) -> ModelThinkingLevel {
    match level {
        ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
        ThinkingLevel::Low => ModelThinkingLevel::Low,
        ThinkingLevel::Medium => ModelThinkingLevel::Medium,
        ThinkingLevel::High => ModelThinkingLevel::High,
        ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
        ThinkingLevel::Max => ModelThinkingLevel::Max,
    }
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// `google-generative-ai.lazy.ts`：`lazyApi(() => import("./google-generative-ai.ts"))`。
///
/// # 两个入口的差异
///
/// - `stream`：把 API 专属字段从 [`StreamOptions::extra`] 解析成 [`GoogleOptions`]
///   （见 [`google_options_from_stream_options`]），其余字段原样传递；
/// - `stream_simple`：同步 `Err`（缺 key、思考等级映射失败）没有 `Result` 通道可返回，
///   用 [`lazy_stream_error`] 映射成流上的 `error` 事件。
pub fn google_generative_ai_api() -> Arc<dyn ProviderStreams> {
    Arc::new(GoogleGenerativeAiProvider)
}

struct GoogleGenerativeAiProvider;

impl ProviderStreams for GoogleGenerativeAiProvider {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        stream(model, context, google_options_from_stream_options(options))
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

/// [`StreamOptions`] → [`GoogleOptions`]：API 专属字段从 `extra` 里按 TS 字段名取。
///
/// - `"toolChoice"`：字符串，原样写入 `tool_choice`（合法值 `"auto"`/`"none"`/`"any"`；
///   没有 strict 工具时，未知的非空值由 `map_tool_choice` 回退成 `AUTO`）；
/// - `"thinking"`：交给 [`parse_google_thinking`]。
///
/// `extra` 被整体取出（`std::mem::take`）：未识别的键不会进入请求，也不会留在
/// `base.extra` 里。
pub fn google_options_from_stream_options(mut options: StreamOptions) -> GoogleOptions {
    let extra = std::mem::take(&mut options.extra);
    let tool_choice = extra
        .get("toolChoice")
        .and_then(Value::as_str)
        .map(str::to_string);
    let thinking = parse_google_thinking(extra.get("thinking"));
    GoogleOptions {
        base: options,
        tool_choice,
        thinking,
    }
}

/// 解析 `thinking: { enabled, budgetTokens?, level? }`。
///
/// 接受的形状：
///
/// ```text
/// { "enabled": bool, "budgetTokens": number, "level": "MINIMAL" | "LOW" | "MEDIUM" | "HIGH" }
/// ```
///
/// - 非对象（含 `null`）→ `None`；
/// - `enabled` 缺失按 `false`，因此 `{}` 与 `{ "enabled": false }` 等价；
/// - `budgetTokens` 用 `f64` 读取，非数字 → `None`；`-1` 是动态预算、`0` 表示关闭，
///   这两个值的语义由 Gemini API 协议解释；`google_shared::build_params` 只决定是否发送该
///   字段（`level` 优先于 `budgetTokens`，且仅当 `enabled` 与模型支持 reasoning 时才写入
///   `thinkingConfig.thinkingBudget`）；
/// - `level` 区分大小写，未知字符串解析成 `Unspecified` 而不报错。
pub fn parse_google_thinking(value: Option<&Value>) -> Option<GoogleThinkingOption> {
    let value = value?;
    let object = value.as_object()?;
    Some(GoogleThinkingOption {
        enabled: object
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        budget_tokens: object.get("budgetTokens").and_then(Value::as_f64),
        level: object
            .get("level")
            .and_then(Value::as_str)
            .map(crate::api::google_shared::GoogleApiThinkingLevel::parse),
    })
}
