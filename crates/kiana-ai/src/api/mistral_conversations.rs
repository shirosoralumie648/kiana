#![allow(clippy::result_large_err)]
//! api/mistral-conversations.ts——Mistral 原生 Chat Completions SSE。
//!
//! Mistral 没有 SDK 依赖：请求体、SSE 解析与事件归一化全部手写（与 TS 相同）。
//! 共享的传输小工具（`next_chunk`/`read_body_text`/`resolve_client`/`Utf8StreamDecoder`）
//! 由同一单元的 `kiana_messages` 模块提供，避免第二份实现。
//!
//! # 在系统中的位置
//!
//! - 下游：`providers/mistral.rs` 用 [`mistral_conversations_api`] 造出
//!   `Arc<dyn ProviderStreams>`，注册为 `mistral` provider；`compat.rs` 的 api 注册表
//!   按 `"mistral-conversations"` 取同一个工厂，`legacy_api_aliases.rs` 的旧全局别名
//!   也引用它。
//! - 上游：`kiana_messages`（流式读取、错误体读取、客户端解析、UTF-8 增量解码、
//!   状态原因短语）、`transform_messages`（历史归一化与 tool call id 规范化）、
//!   `simple_options`（`SimpleStreamOptions` → `StreamOptions`）、
//!   `constrained_sampling`（strict JSON schema）、`kiana_json`（与 JS 逐字一致的
//!   JSON / 字符串 / 数字语义），以及 `utils/*`（哈希、身份头、文本渲染、转录工具、
//!   错误体截断）与 `models`（`ProviderStreams`、计费、思考档位钳制）。
//! - 对应 TS：`packages/ai/src/api/mistral-conversations.ts`；逐条语义见
//!   `spec/ai-api-other.md` §2.12。
//!
//! # 一次请求的执行流程
//!
//! 1. 入口 [`stream`]（完整选项）或 [`stream_simple`]（简单选项）。`stream` 立即返回事件流，
//!    真正的 I/O 在 `spawn_forward` 派生的任务里执行；`stream_simple` 先同步检查 API key。
//! 2. `run_stream`：`resolve_transcript` 处理中途 system 消息 → `transform_messages_with`
//!    丢弃跨模型 thinking 并规范化 tool call id → [`build_chat_payload`] 组请求体 →
//!    可选的 `on_payload` 回调覆写 → `request_mistral_stream` 发 HTTP。
//! 3. 收到响应头且状态码为 2xx 之后才推送 `start` 事件；网络错误或非 2xx 不会有 `start`。
//! 4. `consume_chat_stream`：UTF-8 增量解码、按空行切 SSE 事件、把每个 chunk 归一成
//!    `text_*` / `thinking_*` / `toolcall_*` 事件；遇到 `[DONE]` 或响应体结束即收尾。
//! 5. 收尾：关闭未结束的文本/思考块，再按打开顺序结束每个 tool call；随后检查取消与
//!    `stop_reason`，正常时推送 `done`，失败则改写成 `error` 事件。
//!
//! # 重要约束与失败模式
//!
//! - 本适配器只使用 API key（`Authorization: Bearer ...`），没有 OAuth 流程。
//! - [`stream`] 的任何失败（缺 key、URL 非法、网络/超时、非 2xx、SSE 解析错误、取消）
//!   都变成一次 `error` 事件并结束流：信号已取消时 `stop_reason` 改写为 `Aborted`，
//!   否则为 `Error`；文案由 [`format_mistral_error`] 生成。
//! - [`stream_simple`] 缺 key 时按 TS 契约**同步**返回 `Err`（`AiErrorKind::Auth`，消息为
//!   `No API key for provider: <provider>`）；`ProviderStreams::stream_simple` 再把它映射成
//!   error 事件流（见 `api/mod.rs` 的“适配器入口同步错误契约”）。
//! - 请求体先按 camelCase 构造，发送前由 [`to_mistral_wire_payload`] 整体改成 snake_case；
//!   键顺序依赖工作区 `serde_json` 的 `preserve_order` 特性（`Cargo.toml` 已开启）。
//! - 是否发送图片由 `model.input` 是否包含 `image` 决定；不支持的图片被丢弃并留下占位文本。
//! - 本文件不做重试：重试由上层 `utils::retry` 统一处理，一次调用只发一个请求。
//! - 代码里的 `let _ = stream.push(...)` 是 TS `stream.push(...)`（无返回值）的等价写法：
//!   当前 `AssistantMessageEventStream::push` 不会产生错误，忽略返回值即等价。
//!
//! # 建议阅读顺序
//!
//! [`MistralOptions`] → [`stream`] / [`stream_simple`] → `run_stream` →
//! `request_mistral_stream`（HTTP）→ `consume_chat_stream` / `handle_completion_event`
//! （流式状态机）→ [`to_chat_messages`] / [`to_mistral_wire_payload`]（请求序列化）。

use std::sync::Arc;

use indexmap::IndexMap;
use regex::Regex;
use serde_json::{Map, Value};

use crate::api::constrained_sampling::{
    get_json_schema_tool_parameters, resolve_json_schema_strict_sampling,
};
use crate::api::kiana_messages::{
    Utf8StreamDecoder, next_chunk, read_body_text, reason_phrase, resolve_client,
};
use crate::api::lazy::{lazy_stream_error, spawn_forward};
use crate::api::simple_options::build_base_options;
use crate::api::transform_messages::transform_messages_with;
use crate::error::{AiError, AiErrorKind};
use crate::event_stream::AssistantMessageEventStream;
use crate::http::{ByteStream, HttpRequest};
use crate::models::ProviderStreams;
use crate::models::calculate_cost;
use crate::models::clamp_thinking_level;
use crate::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole, CacheRetention,
    ErrorReason, JsonObject, Message, Model, ModelThinkingLevel, SimpleStreamOptions, StopReason,
    StreamOptions, ThinkingLevel, Tool, ToolCallKind, TranscriptContext, Usage, UserContent,
    UserContentBlock,
};
use crate::utils::error_body::truncate_error_text;
use crate::utils::hash::short_hash;
use crate::utils::identity::{ClientIdentityTarget, get_identity_headers};
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::streaming_json::ToolCallArgumentsStream;
use crate::utils::text::{get_system_message_text, render_system_message_update};
use crate::utils::transcript::{get_current_tools, resolve_transcript};

/// `MISTRAL_TOOL_CALL_ID_LENGTH`。
const MISTRAL_TOOL_CALL_ID_LENGTH: usize = 9;
/// `MAX_MISTRAL_ERROR_BODY_CHARS`。
const MAX_MISTRAL_ERROR_BODY_CHARS: usize = 4000;

/// `MistralOptions`。
///
/// Mistral 适配器的完整请求选项：`base` 承载与 provider 无关的共享字段，另外三个是
/// Mistral 原生 API 的专属开关（对应 TS 的 `MistralOptions extends StreamOptions`）。
/// [`stream`] 直接消费本结构；[`stream_simple`] 从 `SimpleStreamOptions` 推导出这三个字段。
///
/// 直接调用 [`stream`] 时 `base.extra` **不会**被读取：Mistral 专属项必须显式填到下面的
/// 字段里。通过 `ProviderStreams::stream` 进入时，适配器会先用
/// [`mistral_options_from_stream_options`] 从 `extra` 取出这些字段。
#[derive(Default)]
pub struct MistralOptions {
    /// 与 provider 无关的共享选项：API key、取消信号、超时、headers、身份信息，
    /// 以及 `on_payload` / `on_response` / `on_provider_stream_event` 回调等。
    ///
    /// `api_key` 缺失或为空串时：[`stream`] 产出一条 error 事件，
    /// [`stream_simple`] 则同步返回 `Err`。
    pub base: StreamOptions,
    /// `"auto" | "none" | "any" | "required" | { type: "function"; function: { name } }`。
    ///
    /// 保持 TS 的原始 JSON 形状，发送前由 [`map_tool_choice`] 过滤：无法映射的值
    /// （包括 `Value::Null`）会被省略，而不是原样发给服务端。
    pub tool_choice: Option<Value>,
    /// `"reasoning"`。
    ///
    /// TS 的类型只有这一个取值；本适配器不校验内容，其它字符串也会原样发出。
    /// `"reasoning"` 用于让没有 `thinking_level_map` 的推理模型打开 reasoning 模式，
    /// 线格式字段为 `prompt_mode`。
    pub prompt_mode: Option<String>,
    /// `"none" | "low" | "medium" | "high" | "max"`。
    ///
    /// 有 `thinking_level_map` 的模型用它表达思考档位，线格式字段为 `reasoning_effort`；
    /// 未知取值会原样发出。
    pub reasoning_effort: Option<String>,
}

/// `stream(model, context, options)`。
///
/// 立即返回 [`AssistantMessageEventStream`]，请求与解析都在后台任务里执行
/// （`spawn_forward`：有 tokio runtime 时 `spawn`，否则用专用线程 + current-thread
/// runtime），因此本函数不会阻塞调用方。
///
/// 失败不会同步暴露：缺 API key、URL/请求构造失败、网络/超时、非 2xx、SSE 解析失败、
/// 取消，全部转成一次 `error` 事件后结束流。事件顺序为：
/// `start`（仅 2xx 之后）→ 若干增量事件 → `done` 或 `error`；两种终态都只出现一次。
///
/// `output` 的初始字段由 `create_output` 填好（api/provider/model/timestamp，
/// `stop_reason = Pending`）；本函数在后台任务里把它一路改写到终态。
pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: MistralOptions,
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
                output.error_message = Some(format_mistral_error(&error));
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

fn create_output(model: &Model) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage: Usage::default(),
        stop_reason: StopReason::Pending,
        timestamp: now_unix_ms(),
        ..AssistantMessage::default()
    }
}

/// `streamSimple(model, context, options)`。
///
/// 缺少 API key 时与 TS 一样**同步**失败（Rust 侧用 `Err` 表达）。
///
/// 从 `SimpleStreamOptions` 到 Mistral 线格式的推导规则（与 TS `streamSimple` 一致）：
///
/// - `reasoning` 先经 `clamp_thinking_level` 收敛到模型支持的档位；结果是 `Off` 时
///   视为“未开启”：有 `thinking_level_map` 时改发 `Off` 档的映射值，没有 map 时
///   不发任何 reasoning 控制。
/// - 模型同时满足 `reasoning == Some(true)` 且有 `thinking_level_map` 时，用
///   `reasoning_effort`：开启时取该档的映射值、未配置则回落 `"high"`；未开启时取
///   `Off` 档的映射值（例如 `"none"`）。
/// - 是推理模型、没有 map、且已开启时，改用 `prompt_mode: "reasoning"`。
/// - 其它情况两个字段都不发。
///
/// 注意 `SimpleStreamOptions::tool_choice` 只能表达 `Auto`/`None` 两种取值
/// （见 `ToolChoice`），序列化后为 `"auto"` / `"none"`；`"any"`/`"required"` 或
/// `{type:"function", ...}` 需要直接构造 [`MistralOptions`] 并调用 [`stream`]。
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
    let clamped = options
        .reasoning
        .map(|reasoning| clamp_thinking_level(model, to_model_thinking_level(reasoning)));
    let reasoning = clamped.filter(|level| *level != ModelThinkingLevel::Off);
    // 有 thinkingLevelMap 的模型用 `reasoning_effort`；其它 reasoning 模型用 `prompt_mode`。
    let effort_map = model
        .thinking_level_map
        .as_ref()
        .filter(|_| model.reasoning == Some(true));
    let reasoning_effort = match effort_map {
        Some(map) => match reasoning {
            Some(level) => map
                .get(&level)
                .and_then(|value| value.clone())
                .or_else(|| Some("high".into())),
            None => map
                .get(&ModelThinkingLevel::Off)
                .and_then(|value| value.clone()),
        },
        None => None,
    };
    let prompt_mode =
        if model.reasoning == Some(true) && effort_map.is_none() && reasoning.is_some() {
            Some("reasoning".to_string())
        } else {
            None
        };

    Ok(stream(
        model,
        context,
        MistralOptions {
            base,
            tool_choice: options
                .tool_choice
                .map(|choice| serde_json::to_value(choice).unwrap_or(Value::Null)),
            prompt_mode,
            reasoning_effort,
        },
    ))
}

// `ThinkingLevel`（调用方语义档位）→ `ModelThinkingLevel`（模型目录里的档位）的机械映射；
// 除 `ModelThinkingLevel::Off` 外，六个档位一一对应；是否真正受模型支持由
// `clamp_thinking_level` 决定，它可能把结果收敛为 `Off`（例如模型不支持推理时）。
fn to_model_thinking_level(level: ThinkingLevel) -> ModelThinkingLevel {
    match level {
        ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
        ThinkingLevel::Low => ModelThinkingLevel::Low,
        ThinkingLevel::Medium => ModelThinkingLevel::Medium,
        ThinkingLevel::High => ModelThinkingLevel::High,
        ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
        ThinkingLevel::Max => ModelThinkingLevel::Max,
    }
}

// 一次请求的完整生命周期。所有失败都收敛成 `MistralFailure`，由 `stream` 统一改写成
// error 事件；本函数只负责成功路径的 `done`。
//
// 阶段：
// 1. 再取一次 api key：`stream` 是公开入口，可能绕过 `stream_simple` 的同步检查。
// 2. `resolve_transcript`：按模型 compat 决定中途 system 消息是保留还是折进首条。
// 3. `transform_messages_with`：丢弃跨模型 thinking；把跨模型的 tool call id 规范化成
//    9 位（同模型 id 原样保留），保证 assistant 的 tool_call 与后续 toolResult 成对。
// 4. `build_chat_payload` + `on_payload`：回调返回 `Some` 时整体替换 payload。
// 5. `request_mistral_stream`：拿响应体；成功后才 push `start`。
// 6. `consume_chat_stream`：解码并推送增量事件。
// 7. 终态判定：先看取消，再看 `stop_reason` 是否仍为 `Pending`（没有 finish_reason），
//    最后看是否已被流内事件标记为 aborted/error（错误文案已在 `output.error_message`）。
async fn run_stream(
    model: &Model,
    context: &TranscriptContext,
    options: MistralOptions,
    event_stream: &AssistantMessageEventStream,
    output: &mut AssistantMessage,
) -> Result<(), MistralFailure> {
    let MistralOptions {
        base,
        tool_choice,
        prompt_mode,
        reasoning_effort,
    } = options;
    let api_key = base
        .api_key
        .clone()
        .filter(|key| !key.is_empty())
        .ok_or_else(|| {
            MistralFailure::Other(format!("No API key for provider: {}", model.provider))
        })?;

    // compat 为 `Some(true)` 时保留中途 system 消息；缺省/false 时把后续 system 消息
    // 折叠进首条（不同模型对 system 消息位置的接受程度不同）。
    let normalized_context =
        resolve_transcript(context, compat_supports_mid_convo_system_messages(model));
    // 每次请求都新建一份映射表：归一化结果只在该次请求的 `transform_messages_with`
    // 调用内复用，不会跨请求泄漏。
    let normalizer = MistralToolCallIdNormalizer::new();
    let normalized_id = normalizer.normalizer();
    let normalized_id_ref: &crate::api::transform_messages::NormalizeToolCallIdFn = &*normalized_id;
    let transformed_messages = transform_messages_with(
        &normalized_context.0.messages,
        model,
        Some(normalized_id_ref),
    );

    let mut payload = build_chat_payload(
        model,
        &normalized_context,
        &transformed_messages,
        &base,
        &tool_choice,
        &prompt_mode,
        &reasoning_effort,
    )
    .map_err(|error| MistralFailure::Other(error.message))?;
    // TS 是 `const nextPayload = await options?.onPayload?.(payload, model)`：
    // 返回 `undefined` 表示不修改。Rust 的 `OnPayloadFn` 是同步 `Fn`，
    // 因此这里不会等待异步回调（TS 允许 async，属于签名层面的已知差异）。
    if let Some(next) = base.apply_on_payload(payload.clone(), model).await {
        payload = next;
    }

    let mistral_stream = request_mistral_stream(model, &payload, &api_key, &base).await?;
    // `start` 在响应头已到、状态码为 2xx 且拿到 body 之后才推送；此前的失败只有 error 事件。
    let _ = event_stream.push(AssistantMessageEvent::Start {
        partial: Box::new(output.clone()),
    });
    consume_chat_stream(
        model,
        output,
        event_stream,
        mistral_stream,
        base.on_provider_stream_event.as_ref(),
        base.signal.as_ref(),
    )
    .await?;

    // 终态判定顺序：取消优先；其次“流结束但没有 finish_reason”视为协议错误；
    // 最后把流内已记录的 aborted/error 转成 Err，交给 `stream` 统一发 error 事件。
    if base.signal.as_ref().is_some_and(|s| s.is_cancelled()) {
        return Err(MistralFailure::Other("Request was aborted".into()));
    }
    if output.stop_reason == StopReason::Pending {
        return Err(MistralFailure::Other(
            "Mistral stream ended without a finish reason".into(),
        ));
    }
    if matches!(output.stop_reason, StopReason::Aborted | StopReason::Error) {
        return Err(MistralFailure::Other(
            output
                .error_message
                .clone()
                .unwrap_or_else(|| "An unknown error occurred".into()),
        ));
    }
    let _ = event_stream.push(AssistantMessageEvent::Done {
        reason: done_reason(output.stop_reason),
        message: Box::new(output.clone()),
    });
    event_stream.end_without_result();
    Ok(())
}

/// TS 的 `model.compat?.supportsMidConvoSystemMessages`。
fn compat_supports_mid_convo_system_messages(model: &Model) -> Option<bool> {
    model
        .compat
        .as_ref()
        .and_then(|compat| compat.get("supportsMidConvoSystemMessages"))
        .and_then(Value::as_bool)
}

// 把 `StopReason` 收敛到 `done` 事件允许的 `DoneReason` 子集。走到这里时
// `Pending`/`Aborted`/`Error` 已在 `run_stream` 的终态检查里被排除，`_` 分支
// （`Stop`/`Aborted`/`Error`/`Pending`）实际只会收到 `Stop`。
fn done_reason(stop_reason: StopReason) -> crate::types::DoneReason {
    match stop_reason {
        StopReason::Length => crate::types::DoneReason::Length,
        StopReason::ToolUse => crate::types::DoneReason::ToolUse,
        StopReason::Deferred => crate::types::DoneReason::Deferred,
        _ => crate::types::DoneReason::Stop,
    }
}

// ---------------------------------------------------------------------------
// 工具调用 id 归一化
// ---------------------------------------------------------------------------

/// `createMistralToolCallIdNormalizer()`：稳定地把任意 id 映射到 9 位 alnum，
/// 冲突时追加 `:attempt` 重新散列。
///
/// - 原始 id 去掉非字母数字后恰好 9 位且 `attempt == 0` 时，直接返回清洗结果、不再散列；
///   只有原 id 本身就是 9 位 ASCII 字母数字（没有字符被删）时，线上文本才与输入相同。
/// - 同一原始 id 在一次请求内总是得到同一个结果（`id_map` 幂等）。
/// - 两个不同原始 id 可能散列到同一个候选值；`reverse_map` 记录候选值的拥有者，
///   撞车时 `attempt += 1` 重新散列，直到找到空闲候选。
/// - 状态用 `Arc<Mutex<...>>` 保存：`NormalizeToolCallIdFn` 要求 `Fn + Send + Sync`，
///   而闭包需要修改映射表，只能用内部可变性表达。本流程里 `transform_messages_with`
///   是串行调用该闭包的。
/// - 实例由 `run_stream` 每次请求新建，不跨请求复用。
pub struct MistralToolCallIdNormalizer {
    shared: Arc<std::sync::Mutex<MistralToolCallIdState>>,
}

/// 归一化器的两张表：`id_map` 是“原始 id → 线上 id”的缓存，`reverse_map` 是
/// “线上 id → 原始 id”，用于检测不同原始 id 撞到同一候选值。
#[derive(Default)]
struct MistralToolCallIdState {
    id_map: std::collections::HashMap<String, String>,
    reverse_map: std::collections::HashMap<String, String>,
}

impl MistralToolCallIdNormalizer {
    /// 创建空的归一化器（两张表都为空）。
    pub fn new() -> Self {
        MistralToolCallIdNormalizer {
            shared: Arc::new(std::sync::Mutex::new(MistralToolCallIdState::default())),
        }
    }

    /// 返回 `transform_messages_with` 需要的闭包。
    ///
    /// 每次调用都会新建一个 `Arc` 闭包，但它们共享同一份映射状态；`model` 与 `message`
    /// 参数本适配器不使用（签名由 `NormalizeToolCallIdFn` 固定）。锁中毒时用
    /// `into_inner()` 继续使用已有映射表，而不是让后续归一化全部失败。
    pub fn normalizer(&self) -> NormalizerHandle {
        let state = self.shared.clone();
        Arc::new(
            move |id: &str, _model: &Model, _message: &AssistantMessage| {
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                normalize_mistral_tool_call_id(&mut state, id)
            },
        )
    }
}

/// 等价于 [`MistralToolCallIdNormalizer::new`]。
impl Default for MistralToolCallIdNormalizer {
    fn default() -> Self {
        Self::new()
    }
}

fn normalize_mistral_tool_call_id(state: &mut MistralToolCallIdState, id: &str) -> String {
    if let Some(existing) = state.id_map.get(id) {
        return existing.clone();
    }
    let mut attempt = 0usize;
    // 候选值空闲、或已归同一个原始 id 所有（幂等）时接受；被别的 id 占用则换下一个
    // attempt 重新散列，因此不同原始 id 不会映射到同一个线上 id。
    loop {
        let candidate = derive_mistral_tool_call_id(id, attempt);
        let owner = state.reverse_map.get(&candidate);
        if owner.is_none() || owner == Some(&id.to_string()) {
            state.id_map.insert(id.to_string(), candidate.clone());
            state.reverse_map.insert(candidate.clone(), id.to_string());
            return candidate;
        }
        attempt += 1;
    }
}

/// `createMistralToolCallIdNormalizer()` 返回的归一化闭包句柄。
///
/// 参数是 `(原始 id, 当前 model, 产生该 id 的 assistant 消息)`，返回发给 Mistral 的 id；
/// 形状与 `transform_messages_with` 的 `normalize_tool_call_id` 参数一致，可以直接
/// 解引用成 `&NormalizeToolCallIdFn` 传下去。
pub type NormalizerHandle = Arc<dyn Fn(&str, &Model, &AssistantMessage) -> String + Send + Sync>;

/// `deriveMistralToolCallId(id, attempt)`。
///
/// 候选 id 的确定性生成规则（纯函数，不读写归一化状态）：
///
/// 1. 去掉所有非 ASCII 字母数字字符；
/// 2. `attempt == 0` 且结果恰好 `MISTRAL_TOOL_CALL_ID_LENGTH`（9）位时原样返回；
/// 3. 否则以“清洗结果；清洗后为空则用原文”为种子，`attempt > 0` 时再拼 `:<attempt>`，
///    对种子取 `short_hash`，过滤出字母数字并截前 9 位。
///
/// `short_hash` 的算法必须与 `spec/ai-api-other.md` §2.12.2 规定的 `shortHash` 逐位一致：
/// 它决定发往服务端的 id 文本，两份实现对同一原始 id 必须给出相同结果。
pub fn derive_mistral_tool_call_id(id: &str, attempt: usize) -> String {
    let normalized: String = id.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if attempt == 0 && normalized.chars().count() == MISTRAL_TOOL_CALL_ID_LENGTH {
        return normalized;
    }
    let seed_base = if normalized.is_empty() {
        id.to_string()
    } else {
        normalized
    };
    let seed = if attempt == 0 {
        seed_base
    } else {
        format!("{seed_base}:{attempt}")
    };
    short_hash(&seed)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(MISTRAL_TOOL_CALL_ID_LENGTH)
        .collect()
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// Mistral 的 HTTP 错误（`MistralHttpError`）。
///
/// 只在响应头已到、状态码非 2xx 且响应体读取成功时构造；`body` 保存**未截断**的原文，
/// 截断发生在 [`format_mistral_error`] 里。响应体读取失败时得到的是
/// [`MistralFailure::Other`]，不会构造本类型。
#[derive(Debug, Clone)]
pub struct MistralHttpError {
    /// HTTP 状态码，原样保留。
    pub status_code: u16,
    /// 响应体全文（UTF-8 解码，非法字节替换成 U+FFFD）。
    pub body: String,
    /// 状态原因短语（`reason_phrase`）；未知状态码为空串，此时 `message()` 回落到
    /// `Request failed with status <code>`。
    pub status_text: String,
}

impl MistralHttpError {
    fn message(&self) -> String {
        if self.status_text.is_empty() {
            format!("Request failed with status {}", self.status_code)
        } else {
            self.status_text.clone()
        }
    }
}

/// 流内错误形态。
///
/// 区分标准是 TS `formatMistralError` 的输入形状：只有“带 `statusCode` 与 `body` 的
/// HTTP 错误”会做特殊格式化，其余失败都只取 `error.message`。
pub enum MistralFailure {
    /// 非 2xx 的 HTTP 响应；格式化时带上状态码与响应体。
    Http(MistralHttpError),
    /// 其它失败：网络、超时、取消、非法 URL、客户端创建失败、payload 构造、SSE 解析等；
    /// 字符串已经是最终错误文案。
    Other(String),
}

/// `formatMistralError(error)`。
///
/// - `Http`：响应体按 JS `trim` 后非空时输出
///   `Mistral API error (<code>): <body>`，body 按 UTF-16 码元截到
///   `MAX_MISTRAL_ERROR_BODY_CHARS`（4000）；body 为空时用状态原因短语
///   （未知状态码回落到 `Request failed with status <code>`）。
/// - `Other`：原样返回消息（传输层/构造错误的文本），不再加工。
pub fn format_mistral_error(error: &MistralFailure) -> String {
    match error {
        MistralFailure::Http(http) => {
            let body_text = kiana_json::js_string::js_trim(&http.body).to_string();
            if !body_text.is_empty() {
                format!(
                    "Mistral API error ({}): {}",
                    http.status_code,
                    truncate_error_text(&body_text, MAX_MISTRAL_ERROR_BODY_CHARS)
                )
            } else {
                format!(
                    "Mistral API error ({}): {}",
                    http.status_code,
                    http.message()
                )
            }
        }
        MistralFailure::Other(message) => message.clone(),
    }
}

// 发送请求并返回响应体字节流，分三段：
//
// 1. URL 与 headers：把 `base_url` 的 path 去掉尾部 `/` 后补一个 `/`，再相对解析
//    `v1/chat/completions`。因此 `https://api.mistral.ai` 得到 `/v1/chat/completions`，
//    而 `https://api.mistral.ai/v1` 会得到 `/v1/v1/chat/completions`（与 TS 的
//    `new URL("v1/chat/completions", baseUrl)` 一致，不是“智能拼接”）。
// 2. 等响应头：`timeout_ms` 默认 60s，只包住 `execute`；超时通过 `select!` 的 sleep
//    分支表达，并 cancel 一个**子**取消令牌（只中止本次请求，不反向取消调用方的 signal）。
//    响应体读取不受这个 deadline 影响，长思考流不会被固定超时切断。
// 3. 头到达后：先调用 `on_response`（与 TS 一样在状态码检查之前），非 2xx 读完 body
//    构造 `MistralHttpError`，2xx 直接交出 body 流（Rust 的 `HttpResponse.body` 必然存在，
//    没有 TS 的 `!response.body` 分支）。
//
// 失败一律是 `MistralFailure`：非 2xx 是 `Http`，其余（非法 URL、客户端创建失败、
// 超时、body 读取失败）是 `Other`。
async fn request_mistral_stream(
    model: &Model,
    payload: &Value,
    api_key: &str,
    options: &StreamOptions,
) -> Result<ByteStream, MistralFailure> {
    let base_url = url::Url::parse(&model.base_url)
        .map_err(|_| MistralFailure::Other(format!("Invalid URL {}", model.base_url)))?;
    // 复制后再改 path：`Url::join` 对以 `/` 结尾的基地址做“追加”，与 TS 的相对 URL
    // 解析规则相同；query/fragment 会在 join 时按相对引用规则丢弃。
    let mut joined = base_url.clone();
    let path = format!("{}/", base_url.path().trim_end_matches('/'));
    joined.set_path(&path);
    let url = joined
        .join("v1/chat/completions")
        .map_err(|error| MistralFailure::Other(error.to_string()))?;

    let headers = build_mistral_headers(model, api_key, options);
    // 超时只覆盖等响应头；长时间流（例如扩展思考）不能被固定 deadline 切断。
    let timeout_ms = options.timeout_ms.unwrap_or(60_000);
    let header_token = match &options.signal {
        Some(signal) => signal.child_token(),
        None => tokio_util::sync::CancellationToken::new(),
    };
    let request = HttpRequest {
        method: "POST".into(),
        url: url.to_string(),
        headers,
        body: Some(
            kiana_json::js_object::stringify(&to_mistral_wire_payload(payload)).into_bytes(),
        ),
        // 总超时由上面的 select 控制（只覆盖等响应头）。默认客户端的 `timeout_ms`
        // 覆盖整条请求（含 body），这里必须留空，否则长流会被二次 deadline 截断。
        timeout_ms: None,
    };
    let client = resolve_client(options).map_err(|error| MistralFailure::Other(error.message))?;

    let execute = client.execute(request, Some(&header_token));
    let timed_out;
    let response = tokio::select! {
        result = execute => {
            timed_out = false;
            result
        }
        _ = tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)) => {
            header_token.cancel();
            timed_out = true;
            Err(crate::http::HttpError::new(
                format!("Mistral response headers timed out after {timeout_ms}ms"),
            ))
        }
    };
    match response {
        Ok(response) => {
            // `on_response` 在检查状态码之前调用，非 2xx 也会通知观察者。
            if let Some(on_response) = &options.on_response {
                on_response(response.response.clone(), model);
            }
            // 非 2xx：整段读完 body（读取同样受取消令牌约束）后作为 `Http` 错误抛出；
            // 读取失败则退化成 `Other`，此时没有响应体可展示。
            if !(200..300).contains(&response.response.status) {
                let status = response.response.status;
                let body = read_body_text(response.body, options.signal.as_ref())
                    .await
                    .map_err(|error| MistralFailure::Other(error.message))?;
                return Err(MistralFailure::Http(MistralHttpError {
                    status_code: status,
                    body,
                    status_text: reason_phrase(status),
                }));
            }
            Ok(response.body)
        }
        Err(error) => {
            // 超时分支已构造 “Mistral response headers timed out after ...ms”；`timed_out`
            // 为 true 时 `error` 必然是那条合成错误，因此这里仅在调用方未取消时重复构造
            // 同一文案，调用方已取消时直接透传 `error.message`（两条路径文本相同）。
            // `stream` 会依据 signal 把 stop_reason 改写成 Aborted。
            if timed_out && !options.signal.as_ref().is_some_and(|s| s.is_cancelled()) {
                return Err(MistralFailure::Other(format!(
                    "Mistral response headers timed out after {timeout_ms}ms"
                )));
            }
            Err(MistralFailure::Other(error.message))
        }
    }
}

/// `buildMistralHeaders(model, apiKey, options)`。
///
/// 覆盖顺序（后者覆盖前者）：身份头（`User-Agent` + attribution）→ 协议头
/// （`accept` / `authorization` / `content-type`）→ `model.headers` → `options.headers`。
/// `options.headers` 的值为 `None` 时**删除**同名头（对应 TS 的 `null`）。
/// 最后，在开启 prompt caching 且两个覆盖层都没有显式 `x-affinity` 时补
/// `x-affinity: <sessionId>`（会话亲和性提示，与 `prompt_cache_key` 配套）。
///
/// `x-affinity` 的“显式”判断只看键是否存在、大小写不敏感，**不看值**：显式传 `null`
/// 也会抑制自动补值（测试 `honors_case_insensitive_header_overrides_and_explicit_affinity_suppression`
/// 锁定该行为）。
fn build_mistral_headers(
    model: &Model,
    api_key: &str,
    options: &StreamOptions,
) -> Vec<(String, String)> {
    let identity = get_identity_headers(
        &ClientIdentityTarget::new(&model.provider, &model.base_url),
        options.identity.as_ref(),
    );
    let mut headers = HeaderMap::new();
    for (name, value) in &identity {
        if let Some(value) = value {
            headers.set(name, value);
        }
    }
    headers.set("accept", "text/event-stream");
    headers.set("authorization", &format!("Bearer {api_key}"));
    headers.set("content-type", "application/json");
    apply_mistral_model_header_overrides(&mut headers, model.headers.as_ref());
    apply_mistral_header_overrides(&mut headers, options.headers.as_ref());

    let has_explicit_affinity =
        has_mistral_model_header_override(model.headers.as_ref(), "x-affinity")
            || has_mistral_header_override(options.headers.as_ref(), "x-affinity");
    if should_use_prompt_caching(options)
        && !has_explicit_affinity
        && let Some(session_id) = &options.session_id
    {
        headers.set("x-affinity", session_id);
    }
    headers.into_vec()
}

/// 大小写不敏感的 header 集合（复刻 WHATWG `Headers.set` / `delete`）。
///
/// 键统一转小写作为身份；迭代顺序是**首次插入**的顺序，名字保留最近一次 `set` 的写法。
/// HTTP 头顺序没有语义，测试在比较前会自行统一大小写。
#[derive(Default)]
struct HeaderMap {
    entries: IndexMap<String, (String, String)>,
}

impl HeaderMap {
    fn new() -> Self {
        HeaderMap::default()
    }

    fn set(&mut self, name: &str, value: &str) {
        self.entries
            .insert(name.to_lowercase(), (name.to_string(), value.to_string()));
    }

    fn delete(&mut self, name: &str) {
        self.entries.shift_remove(&name.to_lowercase());
    }

    fn into_vec(self) -> Vec<(String, String)> {
        self.entries.into_values().collect()
    }
}

// `model.headers` 的类型是 `IndexMap<String, String>`：只能设置/覆盖，没有删除语义
// （与 `options.headers` 的 `Option<String>` 不同）。
fn apply_mistral_model_header_overrides(
    headers: &mut HeaderMap,
    overrides: Option<&IndexMap<String, String>>,
) {
    let Some(overrides) = overrides else {
        return;
    };
    for (name, value) in overrides {
        headers.set(name, value);
    }
}

// “是否显式覆盖”只看键是否存在（大小写不敏感）：`model.headers` 的值只能是字符串，
// 任何显式 `X-Affinity` 键都会抑制自动补值；`null`/`None` 的删除/抑制语义只在
// `options.headers` 层成立（见 `has_mistral_header_override`）。
fn has_mistral_model_header_override(
    overrides: Option<&IndexMap<String, String>>,
    target: &str,
) -> bool {
    overrides.is_some_and(|overrides| overrides.keys().any(|name| name.to_lowercase() == target))
}

// `options.headers` 的 `None` 值表示删除同名默认头（TS 的 `headers.delete(name)`），
// 因此这一层既能覆盖也能抑制。
fn apply_mistral_header_overrides(
    headers: &mut HeaderMap,
    overrides: Option<&crate::types::ProviderHeaders>,
) {
    let Some(overrides) = overrides else {
        return;
    };
    for (name, value) in overrides {
        match value {
            None => headers.delete(name),
            Some(value) => headers.set(name, value),
        }
    }
}

fn has_mistral_header_override(
    overrides: Option<&crate::types::ProviderHeaders>,
    target: &str,
) -> bool {
    overrides.is_some_and(|overrides| overrides.keys().any(|name| name.to_lowercase() == target))
}

/// `shouldUsePromptCaching(options)`。
///
/// 只有显式 `cacheRetention == Some(CacheRetention::None)` 才关闭；未设置视为开启。
/// `sessionId` 必须存在且非空，因为它同时是 `prompt_cache_key` 与 `x-affinity` 的值。
///
/// 与其它适配器不同，这里**不读** `KIANA_CACHE_RETENTION`：Mistral 没有
/// `resolveCacheRetention` 这一步，环境变量对本 API 无效
/// （`spec/ai-api-other.md` §2.12.4）。
fn should_use_prompt_caching(options: &StreamOptions) -> bool {
    options.cache_retention != Some(CacheRetention::None)
        && options
            .session_id
            .as_deref()
            .is_some_and(|id| !id.is_empty())
}

// ---------------------------------------------------------------------------
// 线上 payload
// ---------------------------------------------------------------------------

/// `toMistralWirePayload(payload)`：camelCase → snake_case（就地保序重命名）。
///
/// 只重命名**存在**的键，值（包括 `null`）原样搬运。`messages` 与 `response_format`
/// 是嵌套结构：先拷贝子对象再逐层重命名，不改动入参——TS 同样用 `{...payload}` 浅拷贝，
/// 因此调用方（如 `on_payload` 观察者）看到的 payload 不受序列化影响。
///
/// 键顺序与 TS 的快照逐项一致，依赖工作区 `serde_json` 的 `preserve_order`：
/// `Map::insert` 对已存在的键保留原位置、对新键追加到末尾，正好等于 JS 的
/// “赋值保留位置 / 新键追加”语义。
pub fn to_mistral_wire_payload(payload: &Value) -> Value {
    let mut wire = match payload {
        Value::Object(map) => map.clone(),
        other => return other.clone(),
    };
    for (source, target) in [
        ("topP", "top_p"),
        ("maxTokens", "max_tokens"),
        ("randomSeed", "random_seed"),
        ("responseFormat", "response_format"),
        ("toolChoice", "tool_choice"),
        ("presencePenalty", "presence_penalty"),
        ("frequencyPenalty", "frequency_penalty"),
        ("parallelToolCalls", "parallel_tool_calls"),
        ("reasoningEffort", "reasoning_effort"),
        ("promptMode", "prompt_mode"),
        ("promptCacheKey", "prompt_cache_key"),
        ("safePrompt", "safe_prompt"),
    ] {
        remap_property(&mut wire, source, target);
    }
    if let Some(Value::Array(messages)) = wire.get("messages").cloned() {
        let wire_messages: Vec<Value> = messages.iter().map(to_mistral_wire_message).collect();
        set_property(&mut wire, "messages", Value::Array(wire_messages));
    }

    if let Some(Value::Object(response_format)) = wire.get("response_format").cloned() {
        let mut wire_format = response_format;
        remap_property(&mut wire_format, "jsonSchema", "json_schema");
        if let Some(Value::Object(json_schema)) = wire_format.get("json_schema").cloned() {
            let mut wire_schema = json_schema;
            remap_property(&mut wire_schema, "schemaDefinition", "schema");
            set_property(&mut wire_format, "json_schema", Value::Object(wire_schema));
        }
        set_property(&mut wire, "response_format", Value::Object(wire_format));
    }
    Value::Object(wire)
}

// 单条消息的键改名（`toolCalls` → `tool_calls`、`toolCallId` → `tool_call_id`），
// 并对 `content` 数组逐块改名；非对象形状原样返回。
fn to_mistral_wire_message(message: &Value) -> Value {
    let mut wire = match message {
        Value::Object(map) => map.clone(),
        other => return other.clone(),
    };
    remap_property(&mut wire, "toolCalls", "tool_calls");
    remap_property(&mut wire, "toolCallId", "tool_call_id");
    if let Some(Value::Array(content)) = wire.get("content").cloned() {
        let chunks: Vec<Value> = content.iter().map(to_mistral_wire_content_chunk).collect();
        set_property(&mut wire, "content", Value::Array(chunks));
    }
    Value::Object(wire)
}

// content chunk 的键改名（`imageUrl` / `documentUrl` / `documentName` / `fileId` /
// `referenceIds` / `inputAudio`）；Mistral 的线格式对这些字段用 snake_case。
fn to_mistral_wire_content_chunk(chunk: &Value) -> Value {
    let mut wire = match chunk {
        Value::Object(map) => map.clone(),
        other => return other.clone(),
    };
    for (source, target) in [
        ("imageUrl", "image_url"),
        ("documentUrl", "document_url"),
        ("documentName", "document_name"),
        ("fileId", "file_id"),
        ("referenceIds", "reference_ids"),
        ("inputAudio", "input_audio"),
    ] {
        remap_property(&mut wire, source, target);
    }
    Value::Object(wire)
}

/// `remapMistralProperty(record, source, target)`：
/// 目标键已存在时保留其位置（JS 的赋值语义），否则追加到末尾，再删除源键。
fn remap_property(record: &mut Map<String, Value>, source: &str, target: &str) {
    let Some(value) = record.get(source).cloned() else {
        return;
    };
    record.insert(target.to_string(), value);
    record.remove(source);
}

/// 只赋值、不删除源键（`messages` / `response_format` 的替换）。
fn set_property(record: &mut Map<String, Value>, target: &str, value: Value) {
    record.insert(target.to_string(), value);
}

/// `buildChatPayload(model, context, messages, options)`。
///
/// 两个消息来源分工不同（不能合并）：
/// - `messages` 是 `transform_messages_with` 处理过的副本，用于 `messages` 字段
///   （thinking 已按模型过滤、跨模型的 tool call id 已规范化为 9 位，同模型 id 不动）；
/// - `context` 是 `resolve_transcript` 之后、**未经** `transform_messages` 的 transcript，
///   用于 `get_current_tools` 取**当前**工具集（回放 system 消息里的 toolsAdded/toolsRemoved）。
///
/// 只写入已提供的字段：`temperature`、`maxTokens`、`toolChoice`（经 [`map_tool_choice`]，
/// `null` 或无法映射时整项省略）、`promptMode`、`reasoningEffort`、`promptCacheKey`。
/// 工具集为空时 `tools` 整项省略，而不是发空数组。
#[allow(clippy::too_many_arguments)]
pub fn build_chat_payload(
    model: &Model,
    context: &TranscriptContext,
    messages: &[Message],
    options: &StreamOptions,
    tool_choice: &Option<Value>,
    prompt_mode: &Option<String>,
    reasoning_effort: &Option<String>,
) -> Result<Value, AiError> {
    let mut payload = Map::new();
    payload.insert("model".into(), Value::String(model.id.clone()));
    payload.insert("stream".into(), Value::Bool(true));
    payload.insert(
        "messages".into(),
        Value::Array(to_chat_messages(
            messages,
            model
                .input
                .contains(&crate::types::ModelInputModality::Image),
        )),
    );

    let current_tools = get_current_tools(&context.0.messages);
    if !current_tools.is_empty() {
        payload.insert(
            "tools".into(),
            Value::Array(to_function_tools(&current_tools)?),
        );
    }
    if let Some(temperature) = options.temperature {
        payload.insert("temperature".into(), number_value(temperature));
    }
    if let Some(max_tokens) = options.max_tokens {
        payload.insert("maxTokens".into(), Value::from(max_tokens));
    }
    if let Some(choice) = tool_choice
        && !matches!(choice, Value::Null)
        && map_tool_choice(choice).is_some()
    {
        payload.insert(
            "toolChoice".into(),
            map_tool_choice(choice).unwrap_or(Value::Null),
        );
    }
    if let Some(prompt_mode) = prompt_mode {
        payload.insert("promptMode".into(), Value::String(prompt_mode.clone()));
    }
    if let Some(reasoning_effort) = reasoning_effort {
        payload.insert(
            "reasoningEffort".into(),
            Value::String(reasoning_effort.clone()),
        );
    }
    if should_use_prompt_caching(options)
        && let Some(session_id) = &options.session_id
    {
        payload.insert("promptCacheKey".into(), Value::String(session_id.clone()));
    }
    Ok(Value::Object(payload))
}

/// `mapToolChoice(choice)`。
///
/// 字符串 `auto` / `none` / `any` / `required` 原样返回；对象形式只认
/// `{type: "function", function: {name}}`，并重建一个只含这两个字段的对象
/// （额外字段被丢弃）。其它形状返回 `None`，调用方据此省略 `tool_choice`。
///
/// 与 TS 的差异：TS 的 `mapToolChoice` 不检查 `type`，对任何带 `function.name` 的对象
/// 都返回 `type: "function"`；Rust 额外要求 `type == "function"`，因此缺少或写错
/// `type` 的对象会被静默丢弃，而不是被“改正”。
pub fn map_tool_choice(choice: &Value) -> Option<Value> {
    match choice {
        Value::String(text) if matches!(text.as_str(), "auto" | "none" | "any" | "required") => {
            Some(choice.clone())
        }
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("function") {
                let name = object
                    .get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)?;
                let mut function = Map::new();
                function.insert("name".into(), Value::String(name.to_string()));
                let mut result = Map::new();
                result.insert("type".into(), Value::String("function".into()));
                result.insert("function".into(), Value::Object(function));
                Some(Value::Object(result))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// `toFunctionTools(tools)`。
///
/// 按“Mistral 支持 strict JSON schema”处理每个工具：
/// `resolve_json_schema_strict_sampling(tool, true, None)` 决定是否把 schema 收紧。
/// `constrained_sampling: require` 且 schema 无法收紧时返回 `Err`（上层转成 payload
/// 构造失败）；`prefer` 失败则回落 `strict: false` 并原样发送 schema。
/// `strict` 字段始终写出（`None` → `false`）。
pub fn to_function_tools(tools: &[Tool]) -> Result<Vec<Value>, AiError> {
    let mut result = Vec::with_capacity(tools.len());
    for tool in tools {
        let strict = resolve_json_schema_strict_sampling(tool, true, None)?;
        let parameters = get_json_schema_tool_parameters(tool, strict)?;
        let mut function = Map::new();
        function.insert("name".into(), Value::String(tool.name.clone()));
        function.insert(
            "description".into(),
            Value::String(tool.description.clone()),
        );
        function.insert("parameters".into(), parameters);
        function.insert("strict".into(), Value::Bool(strict.unwrap_or(false)));
        let mut entry = Map::new();
        entry.insert("type".into(), Value::String("function".into()));
        entry.insert("function".into(), Value::Object(function));
        result.push(Value::Object(entry));
    }
    Ok(result)
}

/// `toChatMessages(messages, supportsImages)`。
///
/// 把内部消息逐条转成 Mistral Chat 消息，顺序保持不变。调用方
/// [`build_chat_payload`] 用 `model.input` 是否包含 `image` 得到 `supports_images`
/// （用户消息与 tool result 共用这一判定）。
///
/// - `system`：第 1 条（下标 0）用完整 prompt（`get_system_message_text`），后续 system
///   消息渲染成 `Updated/Removed system prompt section "..."` 文本；空文本整条丢弃。
/// - `user`：字符串直接发出；数组按 text/image 转 chunk。`!supports_images` 时图片被丢弃，
///   若原本有图且没有任何文本，则发一条 `(image omitted: model does not support images)`
///   占位；两者都没有时整条消息丢弃。
/// - `assistant`：非空 text/thinking 转 chunk（thinking 以
///   `{type:"thinking", thinking:[{type:"text", ...}]}` 回放），tool call 用
///   `JSON.stringify(arguments)` 转成字符串。`prefix: false` 恒定写出；content 与
///   toolCalls 都为空时不发这条消息。
/// - `toolResult`：文本块用 `\n` 连接后交给 [`build_tool_result_text`] 加错误前缀/图片
///   提示；支持图片时再追加 `image_url` chunk。tool 消息永远至少含一个 text chunk。
///
/// `sanitize_surrogates` 在 Rust 里对 `&str` 是恒等变换（`&str` 不允许孤立代理项）。
/// 这里保留的调用各有 TS 对应点（system/user/assistant 文本与 thinking）；但 TS 在
/// `toolResult` 的每个 text 块上也调用 `sanitizeSurrogates`（spec `ai-api-other.md`
/// §2.12.8），Rust 在拼接 tool 文本时直接 `text.text.clone()`，没有对应调用，因此不能
/// 把本函数的调用视为与 TS 一一覆盖。
pub fn to_chat_messages(messages: &[Message], supports_images: bool) -> Vec<Value> {
    let mut result = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        match message {
            Message::System(system) => {
                // 首条 system 是完整提示；之后的 system 消息只能以“更新/删除 section”
                // 的文本形式表达（Mistral 的消息模型没有独立的 system 更新语义）。
                let text = if index == 0 {
                    get_system_message_text(system)
                } else {
                    render_system_message_update(system)
                };
                if !text.is_empty() {
                    let mut entry = Map::new();
                    entry.insert("role".into(), Value::String("system".into()));
                    entry.insert("content".into(), Value::String(sanitize_surrogates(&text)));
                    result.push(Value::Object(entry));
                }
            }
            Message::User(user) => match &user.content {
                UserContent::Text(text) => {
                    let mut entry = Map::new();
                    entry.insert("role".into(), Value::String("user".into()));
                    entry.insert("content".into(), Value::String(sanitize_surrogates(text)));
                    result.push(Value::Object(entry));
                }
                UserContent::Blocks(blocks) => {
                    let had_images = blocks
                        .iter()
                        .any(|item| matches!(item, UserContentBlock::Image(_)));
                    let mut content = Vec::new();
                    for item in blocks {
                        match item {
                            UserContentBlock::Text(text) => {
                                let mut chunk = Map::new();
                                chunk.insert("type".into(), Value::String("text".into()));
                                chunk.insert(
                                    "text".into(),
                                    Value::String(sanitize_surrogates(&text.text)),
                                );
                                content.push(Value::Object(chunk));
                            }
                            UserContentBlock::Image(image) => {
                                if !supports_images {
                                    continue;
                                }
                                let mut chunk = Map::new();
                                chunk.insert("type".into(), Value::String("image_url".into()));
                                chunk.insert(
                                    "imageUrl".into(),
                                    Value::String(format!(
                                        "data:{};base64,{}",
                                        image.mime_type, image.data
                                    )),
                                );
                                content.push(Value::Object(chunk));
                            }
                        }
                    }
                    if !content.is_empty() {
                        let mut entry = Map::new();
                        entry.insert("role".into(), Value::String("user".into()));
                        entry.insert("content".into(), Value::Array(content));
                        result.push(Value::Object(entry));
                        continue;
                    }
                    // 有图片、但模型不支持图片时留一条占位文本，而不是让用户消息凭空消失；
                    // 只有空文本而没有图片时则整条消息被丢弃（走下面的 no-op 路径）。
                    if had_images && !supports_images {
                        let mut entry = Map::new();
                        entry.insert("role".into(), Value::String("user".into()));
                        entry.insert(
                            "content".into(),
                            Value::String("(image omitted: model does not support images)".into()),
                        );
                        result.push(Value::Object(entry));
                    }
                }
            },
            Message::Assistant(assistant) => {
                let mut content_parts = Vec::new();
                let mut tool_calls = Vec::new();
                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Text(text) => {
                            if !kiana_json::js_string::js_trim(&text.text).is_empty() {
                                let mut chunk = Map::new();
                                chunk.insert("type".into(), Value::String("text".into()));
                                chunk.insert(
                                    "text".into(),
                                    Value::String(sanitize_surrogates(&text.text)),
                                );
                                content_parts.push(Value::Object(chunk));
                            }
                        }
                        AssistantContentBlock::Thinking(thinking) => {
                            if !kiana_json::js_string::js_trim(&thinking.thinking).is_empty() {
                                let mut part = Map::new();
                                part.insert("type".into(), Value::String("text".into()));
                                part.insert(
                                    "text".into(),
                                    Value::String(sanitize_surrogates(&thinking.thinking)),
                                );
                                let mut chunk = Map::new();
                                chunk.insert("type".into(), Value::String("thinking".into()));
                                chunk.insert(
                                    "thinking".into(),
                                    Value::Array(vec![Value::Object(part)]),
                                );
                                content_parts.push(Value::Object(chunk));
                            }
                        }
                        AssistantContentBlock::ToolCall(call) => {
                            let mut function = Map::new();
                            function.insert("name".into(), Value::String(call.name.clone()));
                            let arguments: Map<String, Value> = call
                                .arguments
                                .iter()
                                .map(|(key, value)| (key.clone(), value.clone()))
                                .collect();
                            function.insert(
                                "arguments".into(),
                                Value::String(kiana_json::js_object::stringify(&Value::Object(
                                    arguments,
                                ))),
                            );
                            let mut entry = Map::new();
                            entry.insert("id".into(), Value::String(call.id.clone()));
                            entry.insert("type".into(), Value::String("function".into()));
                            entry.insert("function".into(), Value::Object(function));
                            entry.insert("index".into(), Value::from(0));
                            tool_calls.push(Value::Object(entry));
                        }
                    }
                }
                // `prefix` 是 Mistral 的 assistant 消息字段，会恒定写出（TS 的
                // `{ role: "assistant", prefix: false }`）。
                let mut entry = Map::new();
                entry.insert("role".into(), Value::String("assistant".into()));
                entry.insert("prefix".into(), Value::Bool(false));
                if !content_parts.is_empty() {
                    entry.insert("content".into(), Value::Array(content_parts));
                }
                if !tool_calls.is_empty() {
                    entry.insert("toolCalls".into(), Value::Array(tool_calls));
                }
                // role 与 prefix 恒占 2 个键；长度超过 2 说明至少写入了 content 或
                // toolCalls，否则这条 assistant 消息没有任何内容，直接丢弃。
                if entry.len() > 2 {
                    result.push(Value::Object(entry));
                }
            }
            Message::ToolResult(tool_result) => {
                // 图片不参与文本拼接：它们要么以 image_url chunk 追加，要么由
                // `build_tool_result_text` 生成“图片被省略”的提示。
                let text_result = tool_result
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        UserContentBlock::Text(text) => Some(text.text.clone()),
                        UserContentBlock::Image(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let has_images = tool_result
                    .content
                    .iter()
                    .any(|part| matches!(part, UserContentBlock::Image(_)));
                let tool_text = build_tool_result_text(
                    &text_result,
                    has_images,
                    supports_images,
                    tool_result.is_error,
                );
                // 第一个 chunk 始终是 text：`build_tool_result_text` 保证即使没有文本
                // 也有占位文案（如 `(no tool output)`），所以 tool 消息不会没有 content。
                let mut tool_content = vec![{
                    let mut chunk = Map::new();
                    chunk.insert("type".into(), Value::String("text".into()));
                    chunk.insert("text".into(), Value::String(tool_text));
                    Value::Object(chunk)
                }];
                if supports_images {
                    for part in &tool_result.content {
                        if let UserContentBlock::Image(image) = part {
                            let mut chunk = Map::new();
                            chunk.insert("type".into(), Value::String("image_url".into()));
                            chunk.insert(
                                "imageUrl".into(),
                                Value::String(format!(
                                    "data:{};base64,{}",
                                    image.mime_type, image.data
                                )),
                            );
                            tool_content.push(Value::Object(chunk));
                        }
                    }
                }
                let mut entry = Map::new();
                entry.insert("role".into(), Value::String("tool".into()));
                entry.insert(
                    "toolCallId".into(),
                    Value::String(tool_result.tool_call_id.clone()),
                );
                entry.insert("name".into(), Value::String(tool_result.tool_name.clone()));
                entry.insert("content".into(), Value::Array(tool_content));
                result.push(Value::Object(entry));
            }
        }
    }
    result
}

/// `buildToolResultText(text, hasImages, supportsImages, isError)`。
///
/// 生成 tool 消息里那段文本，按优先级：
/// 1. `js_trim` 后非空 → `[tool error] ` 前缀 + trim 后的文本；若模型不支持图片且
///    带图，追加 `\n[tool image omitted: model does not support images]`。
/// 2. 文本为空但有图：支持图片 → `(see attached image)`；不支持 → 提示图片被省略。
/// 3. 文本与图片都为空 → `(no tool output)`。
///
/// trim 用 JS 的空白集合（含 U+FEFF 等），与 TS 的 `text.trim()` 一致；只含空白的
/// 工具输出会落到第 2/3 条，即被当作“没有输出”。`is_error` 只影响前缀与占位文案。
pub fn build_tool_result_text(
    text: &str,
    has_images: bool,
    supports_images: bool,
    is_error: bool,
) -> String {
    let trimmed = kiana_json::js_string::js_trim(text);
    let error_prefix = if is_error { "[tool error] " } else { "" };

    if !trimmed.is_empty() {
        let image_suffix = if has_images && !supports_images {
            "\n[tool image omitted: model does not support images]"
        } else {
            ""
        };
        return format!("{error_prefix}{trimmed}{image_suffix}");
    }

    if has_images {
        if supports_images {
            return if is_error {
                "[tool error] (see attached image)".into()
            } else {
                "(see attached image)".into()
            };
        }
        return if is_error {
            "[tool error] (image omitted: model does not support images)".into()
        } else {
            "(image omitted: model does not support images)".into()
        };
    }

    if is_error {
        "[tool error] (no tool output)".into()
    } else {
        "(no tool output)".into()
    }
}

// ---------------------------------------------------------------------------
// SSE 解析
// ---------------------------------------------------------------------------

/// `findMistralEventBoundary(buffer)`：`\r\n\r\n|\r\n\r|\r\n\n|\r\r\n|\n\r\n|\r\r|\n\r|\n\n`。
///
/// 返回 `(起始字节下标, 分隔符字节长度)`；没有完整分隔符时返回 `None`，调用方继续读块。
/// 交替顺序不能改：正则在同一位置按分支书写顺序取第一个匹配，`\r\n\r\n` 必须排在
/// `\r\n\r` 之前，否则 4 字节分隔符会被拆成 3+1 字节，事件边界与 TS 不一致。
fn find_mistral_event_boundary(buffer: &str) -> Option<(usize, usize)> {
    static BOUNDARY: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"\r\n\r\n|\r\n\r|\r\n\n|\r\r\n|\n\r\n|\r\r|\n\r|\n\n").expect("valid regex")
    });
    BOUNDARY
        .find(buffer)
        .map(|found| (found.start(), found.end() - found.start()))
}

/// `parseMistralEvent(raw)`：返回 `None`（无数据）、`"[DONE]"` 或解析后的事件对象。
///
/// 输入是**一个完整事件**（已去掉空行分隔符），步骤：
///
/// - 只保留以 `data:` 开头的行；去掉 5 字节前缀后做 `trimStart`，再用 `\n` 连接
///   （SSE 规范的多行 data 合并方式）；
/// - 整体 `js_trim` 后为空 → `Ok(None)`；精确等于 `[DONE]` →
///   `Ok(Some(Value::String("[DONE]")))`（用 JSON 字符串当哨兵，调用方解构）；
/// - 否则做与 `JSON.parse` 等价的解析，并要求顶层是对象、`choices` 是数组；
///   形状不符返回固定文案 `Invalid Mistral streaming event`。
///
/// 两种错误文本的来源不同：JSON 语法错误直接用 V8 `SyntaxError` 的 message（无前缀），
/// 形状错误用固定字符串（见下方行内注释）。
pub fn parse_mistral_event(raw: &str) -> Result<Option<Value>, AiError> {
    static LINE_SPLIT: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"\r\n|\r|\n").expect("valid regex"));
    let joined = LINE_SPLIT
        .split(raw)
        .filter(|line| line.starts_with("data:"))
        .map(|line| js_trim_start(&line[5..]))
        .collect::<Vec<_>>()
        .join("\n");
    let data = kiana_json::js_string::js_trim(&joined).to_string();
    if data.is_empty() {
        return Ok(None);
    }
    if data == "[DONE]" {
        return Ok(Some(Value::String("[DONE]".into())));
    }
    // TS 直接 `JSON.parse(data)`：解析失败抛 V8 的 `SyntaxError`，其 `message` 原样成为
    // 流错误文本（没有 "Invalid Mistral streaming event: " 前缀——那个文案只用于
    // JSON 合法但形状不对的情况）。`js_parse` 接受 JSON.parse 接受的一切输入
    // （孤立代理项转义、溢出数字等）。
    let parsed: Value = kiana_json::js_parse::parse(&data)
        .map_err(|error| AiError::new(AiErrorKind::Parse, error.message))?;
    if !parsed.is_object() || !parsed.get("choices").is_some_and(Value::is_array) {
        return Err(AiError::new(
            AiErrorKind::Parse,
            "Invalid Mistral streaming event",
        ));
    }
    Ok(Some(parsed))
}

/// `String.prototype.trimStart()` 的 JS 语义。
///
/// 字符集与 `kiana_json::js_string::js_trim` 一致（U+FEFF、U+00A0、U+3000 等）；
/// kiana-json 侧的判定函数是 `pub(crate)`，这里本地复刻一份，改字符集时必须两边同步。
fn js_trim_start(text: &str) -> &str {
    text.trim_start_matches(is_js_whitespace)
}

/// JS 的 `WhiteSpace` + `LineTerminator` 集合（不包含 U+0085、U+200B 等）。
fn is_js_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\t' | '\n' | '\u{000b}' | '\u{000c}' | '\r' | ' ' | '\u{00a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

// ---------------------------------------------------------------------------
// 事件消费
// ---------------------------------------------------------------------------

/// 当前已打开、尚未推送 end 事件的内容块类型。
#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    Thinking,
}

/// tool call 的归并键：优先用服务端给的 `index`，缺失时退回 id。
///
/// 同一 tool call 的后续增量片段可能不带 id/name、只带 `index`；用 index 归并才能把
/// 分片的 `arguments` 拼到同一个块上（TS 的 `key = toolCall.index ?? callId`）。
#[derive(Clone, PartialEq, Eq, Hash)]
enum ToolCallKey {
    Index(i64),
    Id(String),
}

/// 一个已打开但尚未结束的 tool call：内容块下标 + 增量 JSON 解析器。
/// 解析器独立于 `AssistantContentBlock` 保存，避免把中间解析状态写进 partial 快照。
struct OpenToolCall {
    content_index: usize,
    arguments: ToolCallArgumentsStream,
}

// 当前打开块的下标。调用点都保证 `blocks` 非空（`current_block` 为 `Some` 时最后一块
// 就是它）；`saturating_sub` 只是防御。
fn block_index(blocks: &[AssistantContentBlock]) -> usize {
    blocks.len().saturating_sub(1)
}

// 结束当前打开的块：按类型推送 `text_end` / `thinking_end`，内容取 `output.content`
// 最后一块的完整文本。本函数不修改 `output`、也不清空 `current_block`（由调用方
// take/替换），因此既能用于块切换，也能用于流末尾收尾。
fn finish_current_block(
    kind: BlockKind,
    output: &AssistantMessage,
    stream: &AssistantMessageEventStream,
) {
    let index = block_index(&output.content);
    match kind {
        BlockKind::Text => {
            let content = match output.content.get(index) {
                Some(AssistantContentBlock::Text(text)) => text.text.clone(),
                _ => String::new(),
            };
            let _ = stream.push(AssistantMessageEvent::TextEnd {
                content_index: index,
                content,
                partial: Box::new(output.clone()),
            });
        }
        BlockKind::Thinking => {
            let content = match output.content.get(index) {
                Some(AssistantContentBlock::Thinking(thinking)) => thinking.thinking.clone(),
                _ => String::new(),
            };
            let _ = stream.push(AssistantMessageEvent::ThinkingEnd {
                content_index: index,
                content,
                partial: Box::new(output.clone()),
            });
        }
    }
}

// SSE 消费状态机：字节流进，事件流 + `output` 出。
//
// TS 用 async generator（`readMistralEvents`）加 for-await（`consumeChatStream`）；
// Rust 没有 async generator，这里把两层合并成显式状态变量：
// - `decoder`：缓冲跨块的不完整 UTF-8 序列；最后一块之后（`stream_done`）冲掉残余。
// - `buffer` + `find_mistral_event_boundary`：只在完整分隔符处切事件，残缺尾块留到下次。
// - `current_block`：已打开但未结束的 text/thinking 块（不变量见 `ensure_*_block`）。
// - `open_tool_calls`：按首次出现顺序保存每个 tool call 的块下标与增量解析器。
// - `saw_done_marker`：收到过 `[DONE]`；`saw_stream_done`：外层循环终止条件。
//
// 对单个块的事件顺序恒为 start → delta* → end；tool call 在流末尾按打开顺序收尾。
// 取消在 `next_chunk` 内部（select 取消令牌）与它返回之后各检查一次。
async fn consume_chat_stream(
    model: &Model,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    mut body: ByteStream,
    on_provider_event: Option<&crate::types::OnProviderStreamEventFn>,
    signal: Option<&tokio_util::sync::CancellationToken>,
) -> Result<(), MistralFailure> {
    let mut current_block: Option<BlockKind> = None;
    let mut open_tool_calls: IndexMap<ToolCallKey, OpenToolCall> = IndexMap::new();
    let mut decoder = Utf8StreamDecoder::new();
    let mut buffer = String::new();
    let mut saw_stream_done = false;
    let mut saw_done_marker = false;

    while !saw_stream_done {
        let (stream_done, bytes) = match next_chunk(&mut body, signal).await {
            Ok(Some(bytes)) => (false, bytes),
            Ok(None) => (true, Vec::new()),
            Err(error) => {
                // 读取失败：若同时已取消，统一成取消文案；否则保留底层 HTTP 错误。
                if signal.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
                    return Err(MistralFailure::Other(request_aborted_message(signal)));
                }
                return Err(MistralFailure::Other(error.message));
            }
        };
        // 再检查一次取消：数据到达与取消可能同时就绪，这里让取消优先。
        if signal.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
            return Err(MistralFailure::Other(request_aborted_message(signal)));
        }
        // `stream = !stream_done`：还有后续字节时，保留结尾不完整的 UTF-8 序列。
        buffer.push_str(&decoder.decode(&bytes, !stream_done));

        // 一个字节块可能含多个事件；切到没有完整分隔符为止，残缺尾块留在 `buffer`。
        while let Some((index, length)) = find_mistral_event_boundary(&buffer) {
            let raw = buffer[..index].to_string();
            buffer.drain(..index + length);
            match parse_mistral_event(&raw).map_err(|error| MistralFailure::Other(error.message))? {
                Some(Value::String(done_marker)) if done_marker == "[DONE]" => {
                    // TS 的 `MISTRAL_STREAM_DONE` 哨兵：立即停止，缓冲区里 `[DONE]` 之后
                    // 的残余内容按 TS 语义直接丢弃（不再解析）。
                    saw_done_marker = true;
                    break;
                }
                Some(event) => {
                    // 回调先看到原始 chunk（未归一化），与 TS 的
                    // `await onProviderStreamEvent(chunk, model)` 顺序一致。
                    if let Some(callback) = on_provider_event {
                        callback(event.clone(), model);
                    }
                    handle_completion_event(
                        model,
                        output,
                        stream,
                        &event,
                        &mut current_block,
                        &mut open_tool_calls,
                    );
                }
                None => {}
            }
        }
        saw_stream_done = stream_done || saw_done_marker;
    }

    // 没有收到 `[DONE]` 的服务器：把最后一个没有空行结尾的事件也处理掉
    // （TS 的 `if (buffer.trim()) { ... }`）；已见 `[DONE]` 时残余按语义丢弃。
    if !saw_done_marker
        && !kiana_json::js_string::js_trim(&buffer).is_empty()
        && let Some(event) =
            parse_mistral_event(&buffer).map_err(|error| MistralFailure::Other(error.message))?
        && !matches!(&event, Value::String(marker) if marker == "[DONE]")
    {
        if let Some(callback) = on_provider_event {
            callback(event.clone(), model);
        }
        handle_completion_event(
            model,
            output,
            stream,
            &event,
            &mut current_block,
            &mut open_tool_calls,
        );
    }

    // 收尾顺序：先结束仍在打开的文本/思考块，再按打开顺序结束各个 tool call。
    if let Some(kind) = current_block {
        finish_current_block(kind, output, stream);
    }
    for open in open_tool_calls.values() {
        // `finish()` 用完整 arguments 文本重新解析，得到最终 arguments 与
        // arguments_error；增量解析器的中间状态不会进入 partial 快照。
        let block = output.content.get_mut(open.content_index);
        let finished = open.arguments.finish();
        if let Some(AssistantContentBlock::ToolCall(call)) = block {
            call.arguments = finished.arguments.clone();
            call.arguments_error = finished.arguments_error.clone();
        }
        let tool_call = match output.content.get(open.content_index) {
            Some(AssistantContentBlock::ToolCall(call)) => call.clone(),
            _ => continue,
        };
        let _ = stream.push(AssistantMessageEvent::ToolCallEnd {
            content_index: open.content_index,
            tool_call,
            partial: Box::new(output.clone()),
        });
    }
    Ok(())
}

// 取消文案统一取 `reqwest_client::ABORTED_MESSAGE`（`next_chunk` 取消时也返回同一文本，
// 因此“读取出错”和“块间检查到取消”两条路径的用户可见文案一致）。
// 参数 `_signal` 当前未使用：调用点已经确认过取消，不再需要区分原因。
fn request_aborted_message(_signal: Option<&tokio_util::sync::CancellationToken>) -> String {
    crate::http::reqwest_client::ABORTED_MESSAGE.to_string()
}

// 处理单个 SSE chunk（TS `consumeChatStream` 的循环体）。顺序固定：
// 1. `responseId ||= chunk.id`（只保留第一个非空 id）；
// 2. `usage` 存在时整块覆盖 usage 并重算 cost（最后一个带 usage 的 chunk 生效）；
// 3. 取 `choices[0]`；没有 choice 的 chunk（TS 用 `continue` 容忍）只结束本 chunk 的
//    后续处理，不撤销前两步；
// 4. `finish_reason` 非空 → 记录 raw_stop_reason 并映射 stop_reason/error_message；
// 5. `delta.content`：字符串或 chunk 数组，逐项产生 text/thinking 增量；
// 6. `delta.tool_calls`：先结束当前文本/思考块，再按 index（或 id）归并增量。
fn handle_completion_event(
    model: &Model,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    event: &Value,
    current_block: &mut Option<BlockKind>,
    open_tool_calls: &mut IndexMap<ToolCallKey, OpenToolCall>,
) {
    let chunk = event;
    // `output.responseId ||= chunk.id`：空串也视为“未设置”；chunk 没有 id 时保持原值。
    if output.response_id.as_deref().is_none_or(str::is_empty)
        && let Some(id) = chunk.get("id").and_then(Value::as_str)
    {
        output.response_id = Some(id.to_string());
    }

    if let Some(usage) = chunk.get("usage").filter(|value| value.is_object()) {
        // `prompt_tokens` 是含缓存命中的总输入，`input` 要扣掉 cache_read；
        // `cache_write` Mistral 没有该概念，恒为 0。
        let prompt_tokens = json_number_or_zero(usage.get("prompt_tokens"));
        let cached = get_mistral_cached_prompt_tokens(usage, prompt_tokens);
        output.usage.input = (prompt_tokens - cached).max(0.0) as u64;
        output.usage.output = json_number_or_zero(usage.get("completion_tokens")) as u64;
        output.usage.cache_read = cached as u64;
        output.usage.cache_write = 0;
        // `total_tokens || 四项之和`：缺失或为 0 时自己算（TS 的 `||` 回退）。
        let total = json_number_or_zero(usage.get("total_tokens"));
        output.usage.total_tokens = if total != 0.0 {
            total as u64
        } else {
            output.usage.input
                + output.usage.output
                + output.usage.cache_read
                + output.usage.cache_write
        };
        calculate_cost(model, &mut output.usage);
    }

    // TS 用 `if (!choice) continue` 容忍没有 choices 的 chunk；此时前面的 id/usage
    // 已经生效，这里只结束本 chunk 的后续处理。
    let Some(choice) = chunk
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    else {
        return;
    };

    // 空串视为“没有 finish_reason”（TS 的 `if (choice.finish_reason)`）。
    if let Some(finish_reason) = choice.get("finish_reason").and_then(Value::as_str)
        && !finish_reason.is_empty()
    {
        output.raw_stop_reason = Some(finish_reason.to_string());
        let mapped = map_chat_stop_reason(finish_reason);
        output.stop_reason = mapped.0;
        if let Some(error_message) = mapped.1 {
            output.error_message = Some(error_message);
        }
    }

    if let Some(delta) = choice.get("delta")
        && let Some(content) = delta.get("content")
        && !content.is_null()
    {
        // `content` 既可能是普通文本字符串，也可能是原生 thinking 的 chunk 数组；
        // `null`/缺失都表示本 chunk 没有内容（TS 的 `!== null && !== undefined`）。
        let items: Vec<Value> = match content {
            Value::String(text) => vec![Value::String(text.clone())],
            Value::Array(items) => items.clone(),
            _ => Vec::new(),
        };
        for item in items {
            match item {
                Value::String(text) => {
                    let text_delta = sanitize_surrogates(&text);
                    // 空 delta 直接跳过：GLM 在 Mistral 上会在 thinking 与 tool call
                    // 之间发空 content，若为空串开块，会把 thinking 拆成多块，
                    // Mistral 回放时会拒绝（TS 源码注释记录的回归）。
                    if text_delta.is_empty() {
                        continue;
                    }
                    ensure_text_block(output, stream, current_block);
                    if let Some(AssistantContentBlock::Text(block)) = output.content.last_mut() {
                        block.text.push_str(&text_delta);
                    }
                    let _ = stream.push(AssistantMessageEvent::TextDelta {
                        content_index: block_index(&output.content),
                        delta: text_delta,
                        partial: Box::new(output.clone()),
                    });
                }
                Value::Object(chunk_item) => {
                    let kind = chunk_item.get("type").and_then(Value::as_str).unwrap_or("");
                    // 原生 thinking 形如 `{type:"thinking", thinking:[{text}, ...]}`；
                    // 多个 part 按顺序拼接后再当作一个增量。
                    if kind == "thinking" {
                        let delta_text = chunk_item
                            .get("thinking")
                            .and_then(Value::as_array)
                            .map(|parts| {
                                parts
                                    .iter()
                                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                                    .filter(|text| !text.is_empty())
                                    .collect::<Vec<_>>()
                                    .join("")
                            })
                            .unwrap_or_default();
                        let thinking_delta = sanitize_surrogates(&delta_text);
                        if thinking_delta.is_empty() {
                            continue;
                        }
                        ensure_thinking_block(output, stream, current_block);
                        if let Some(AssistantContentBlock::Thinking(block)) =
                            output.content.last_mut()
                        {
                            block.thinking.push_str(&thinking_delta);
                        }
                        let _ = stream.push(AssistantMessageEvent::ThinkingDelta {
                            content_index: block_index(&output.content),
                            delta: thinking_delta,
                            partial: Box::new(output.clone()),
                        });
                    } else if kind == "text" {
                        let text_delta = sanitize_surrogates(
                            chunk_item.get("text").and_then(Value::as_str).unwrap_or(""),
                        );
                        if text_delta.is_empty() {
                            continue;
                        }
                        ensure_text_block(output, stream, current_block);
                        if let Some(AssistantContentBlock::Text(block)) = output.content.last_mut()
                        {
                            block.text.push_str(&text_delta);
                        }
                        let _ = stream.push(AssistantMessageEvent::TextDelta {
                            content_index: block_index(&output.content),
                            delta: text_delta,
                            partial: Box::new(output.clone()),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    let tool_calls: Vec<Value> = choice
        .get("delta")
        .and_then(|delta| delta.get("tool_calls"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for tool_call in tool_calls {
        // 工具调用出现即终结当前文本/思考块（TS 在每个 tool_call 前调用
        // `finishCurrentBlock`）；同一 delta 里的后续 tool call 不会再重复结束。
        if let Some(kind) = current_block.take() {
            finish_current_block(kind, output, stream);
        }
        // id 通常只在首个片段出现，后续片段靠 `index` 归并；字符串 `"null"` 与空串
        // 都按“没有 id”处理，用派生 id 兜底。
        let id = tool_call.get("id").and_then(Value::as_str);
        let index = tool_call.get("index").and_then(Value::as_f64);
        let call_id = match id {
            Some(id) if id != "null" && !id.is_empty() => id.to_string(),
            _ => derive_mistral_tool_call_id(
                &format!(
                    "toolcall:{}",
                    index.map(format_js_index).unwrap_or_else(|| "0".into())
                ),
                0,
            ),
        };
        // 有 index 用 index 归并（后续片段通常不带 id）；没有 index 才退回用 id
        // （TS 的 `key = toolCall.index ?? callId`）。
        let key = match index {
            Some(index) => ToolCallKey::Index(index as i64),
            None => ToolCallKey::Id(call_id.clone()),
        };
        let function = tool_call.get("function");
        let name = function
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        // 首次出现：追加 content 块、登记增量解析器并推送 `toolcall_start`；
        // 后续片段只把新的 arguments 文本喂给已有解析器。
        if !open_tool_calls.contains_key(&key) {
            output
                .content
                .push(AssistantContentBlock::ToolCall(crate::types::ToolCall {
                    kind_tool_call: ToolCallKind::ToolCall,
                    id: call_id,
                    name,
                    arguments: JsonObject::new(),
                    ..Default::default()
                }));
            let content_index = output.content.len() - 1;
            open_tool_calls.insert(
                key.clone(),
                OpenToolCall {
                    content_index,
                    arguments: ToolCallArgumentsStream::new(),
                },
            );
            let _ = stream.push(AssistantMessageEvent::ToolCallStart {
                content_index,
                partial: Box::new(output.clone()),
            });
        }

        // `arguments` 正常是字符串增量；TS 的类型也允许直接给对象
        // （`string | Record<string, unknown>`），此时先 stringify 再喂给增量解析器。
        // `null`/缺失按 `{}`；数字/布尔等越界形状 Rust 也统一按 `{}` 处理，与 TS 的
        // `JSON.stringify(args || {})` 在 truthy 标量上不同（TS 对 `5` 得 `"5"`、
        // 对 `true` 得 `"true"`）。
        let args_delta = match function.and_then(|function| function.get("arguments")) {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Object(_)) | Some(Value::Array(_)) => kiana_json::js_object::stringify(
                function
                    .and_then(|function| function.get("arguments"))
                    .unwrap_or(&Value::Null),
            ),
            _ => kiana_json::js_object::stringify(&Value::Object(Map::new())),
        };
        // `append` 内部做流式 JSON 解析，返回当前可用的 arguments 快照
        // （快照不是对象时给空对象）；`arguments_error` 只在收尾 `finish()` 时确定并写回。
        let open = open_tool_calls.get_mut(&key).expect("inserted above");
        let arguments = open.arguments.append(&args_delta);
        if let Some(AssistantContentBlock::ToolCall(call)) =
            output.content.get_mut(open.content_index)
        {
            call.arguments = arguments;
        }
        let content_index = open.content_index;
        let _ = stream.push(AssistantMessageEvent::ToolCallDelta {
            content_index,
            delta: args_delta,
            partial: Box::new(output.clone()),
        });
    }
}

// 确保当前打开的是 text 块：若已有别的块先结束它，再追加一个空 text 块并推送
// `text_start`；调用方随后把增量写进 `output.content` 的最后一块。
//
// 不变量：返回后 `*current_block == Some(BlockKind::Text)`，且最后一块就是该 text 块，
// 因此 `block_index(&output.content)` 永远指向当前块。
fn ensure_text_block(
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    current_block: &mut Option<BlockKind>,
) {
    if *current_block == Some(BlockKind::Text) {
        return;
    }
    if let Some(kind) = current_block.take() {
        finish_current_block(kind, output, stream);
    }
    output
        .content
        .push(AssistantContentBlock::Text(crate::types::TextContent {
            kind_text: crate::types::TextKind::Text,
            text: String::new(),
            text_signature: None,
        }));
    *current_block = Some(BlockKind::Text);
    let _ = stream.push(AssistantMessageEvent::TextStart {
        content_index: output.content.len() - 1,
        partial: Box::new(output.clone()),
    });
}

// 与 `ensure_text_block` 同构，只是创建 thinking 块并推送 `thinking_start`。
// `thinking_signature`/`redacted` 在 Mistral 流里没有对应字段，保持 `None`。
fn ensure_thinking_block(
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    current_block: &mut Option<BlockKind>,
) {
    if *current_block == Some(BlockKind::Thinking) {
        return;
    }
    if let Some(kind) = current_block.take() {
        finish_current_block(kind, output, stream);
    }
    output.content.push(AssistantContentBlock::Thinking(
        crate::types::ThinkingContent {
            kind_thinking: crate::types::ThinkingKind::Thinking,
            thinking: String::new(),
            thinking_signature: None,
            redacted: None,
        },
    ));
    *current_block = Some(BlockKind::Thinking);
    let _ = stream.push(AssistantMessageEvent::ThinkingStart {
        content_index: output.content.len() - 1,
        partial: Box::new(output.clone()),
    });
}

// TS 模板字符串 `${toolCall.index ?? 0}` 的等价文本：用 JS 的数字排版而不是 Rust 的
// `Display`，因为派生 tool call id 的种子要求逐字一致。
fn format_js_index(value: f64) -> String {
    kiana_json::js_num::format_js_number(value)
}

/// `getMistralCachedPromptTokens(usage, promptTokens)`。
///
/// 依次尝试 6 个字段名（camelCase 与 snake_case 各三种写法），取第一个存在且非 `null`
/// 的值，非数字按 0 处理；返回值是 `min(promptTokens, max(0, cached))`——正常 usage 下
/// 即夹在 `[0, promptTokens]`。这是 TS `??` 链的直译，逐项语义见函数内的行内注释。
pub fn get_mistral_cached_prompt_tokens(usage: &Value, prompt_tokens: f64) -> f64 {
    // `??` 跳过 null 与 undefined（继续看下一个候选）；选中的非数字值按 0 处理
    // （TS 的 `typeof === "number"` 检查）。
    let pick = |pointer: &[&str]| -> Option<&Value> {
        let mut current = usage;
        for segment in pointer {
            current = current.get(*segment)?;
        }
        Some(current).filter(|value| !value.is_null())
    };
    let raw = pick(&["promptTokensDetails", "cachedTokens"])
        .or_else(|| pick(&["prompt_tokens_details", "cached_tokens"]))
        .or_else(|| pick(&["promptTokenDetails", "cachedTokens"]))
        .or_else(|| pick(&["prompt_token_details", "cached_tokens"]))
        .or_else(|| pick(&["numCachedTokens"]))
        .or_else(|| pick(&["num_cached_tokens"]));
    let cached = match raw.and_then(Value::as_f64) {
        Some(number) if number.is_finite() => number,
        _ => 0.0,
    };
    prompt_tokens.min(cached.max(0.0))
}

// 与 TS 的 `x || 0` 对应：缺失、非数字、非有限值都归 0。`Value::as_f64` 对 serde_json
// 的 JSON 数字不会返回 NaN/±Infinity（`1e400` 在 `js_parse` 里解析为 null），
// `is_finite` 只是防御性检查。
fn json_number_or_zero(value: Option<&Value>) -> f64 {
    match value.and_then(Value::as_f64) {
        Some(number) if number.is_finite() => number,
        _ => 0.0,
    }
}

/// `mapChatStopReason(reason)`。
///
/// 返回 `(stop_reason, error_message)`；第二个值由调用方写入 `output.error_message`。
///
/// `"error"` 分支特意产出 `"... (server error)"`：`utils::retry` 的可重试模式包含
/// `server.?error`，从而把 Mistral 的瞬时服务端失败纳入重试。未知 reason 只带原文，
/// 是否可重试取决于原文是否命中 `utils::retry` 的模式（测试里的 `unmapped_error`
/// 不可重试）。
/// TS 里 `null` 也映射为 `stop`；Rust 的签名接收 `&str`，由调用方在 `finish_reason`
/// 为空时跳过调用。
pub fn map_chat_stop_reason(reason: &str) -> (StopReason, Option<String>) {
    match reason {
        "stop" => (StopReason::Stop, None),
        "length" | "model_length" => (StopReason::Length, None),
        "tool_calls" => (StopReason::ToolUse, None),
        "error" => (
            StopReason::Error,
            Some("Provider stopped with: error (server error)".into()),
        ),
        other => (
            StopReason::Error,
            Some(format!("Provider stopped with: {other}")),
        ),
    }
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

// `serde_json::Number` 不能表示 NaN/±Infinity：这类温度值会写成 `null`，
// 与 `JSON.stringify(NaN)` 的结果一致。
fn number_value(value: f64) -> Value {
    serde_json::Number::from_f64(value)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

/// `mistral-conversations.lazy.ts`：`lazyApi(() => import("./mistral-conversations.ts"))`。
///
/// 对应 TS 的动态 import 工厂：provider 注册表按 `api == "mistral-conversations"` 取到
/// 这个 trait object。`MistralProvider` 是零大小、无状态的结构，这里每次调用都会新建
/// 一个 `Arc` 包装。
pub fn mistral_conversations_api() -> Arc<dyn ProviderStreams> {
    Arc::new(MistralProvider)
}

/// `ProviderStreams` 的 Mistral 实现：把共享选项转成 [`MistralOptions`] 后转调模块级
/// [`stream`] / [`stream_simple`]。
struct MistralProvider;

impl ProviderStreams for MistralProvider {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        // API 专属选项由 `mistral_options_from_stream_options` 从 `options.extra` 按 TS
        // 字段名取出，其余字段留在 `base` 里。
        stream(model, context, mistral_options_from_stream_options(options))
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        // `ProviderStreams` 的签名不能返回 `Result`：把同步 `Err` 映射成 error 事件流，
        // 即 api/mod.rs 错误契约里的 setup rejection。
        match stream_simple(model, context, options) {
            Ok(stream) => stream,
            Err(error) => lazy_stream_error(model, error),
        }
    }
}

/// [`StreamOptions`] → [`MistralOptions`]：API 专属字段从 `extra` 里按 TS 字段名取。
///
/// `extra` 会被整体移出并丢弃（`std::mem::take`）：其中只有 `toolChoice`、
/// `promptMode`、`reasoningEffort` 三个键是本适配器认识的，其余键在 Mistral 线格式里
/// 没有对应位置。`promptMode`/`reasoningEffort` 只接受字符串（其它类型忽略）；
/// `toolChoice` 保持原始 JSON 形状，交给 [`map_tool_choice`] 过滤。
///
/// 这是 `Models` 把 API 专属选项透传给适配器的约定：键名保持 TS 字段名，值不做解释。
pub fn mistral_options_from_stream_options(mut options: StreamOptions) -> MistralOptions {
    let extra = std::mem::take(&mut options.extra);
    let tool_choice = extra.get("toolChoice").cloned();
    let prompt_mode = extra
        .get("promptMode")
        .and_then(Value::as_str)
        .map(str::to_string);
    let reasoning_effort = extra
        .get("reasoningEffort")
        .and_then(Value::as_str)
        .map(str::to_string);
    MistralOptions {
        base: options,
        tool_choice,
        prompt_mode,
        reasoning_effort,
    }
}
