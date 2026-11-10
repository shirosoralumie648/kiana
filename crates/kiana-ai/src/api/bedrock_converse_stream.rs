#![allow(clippy::result_large_err)]
//! api/bedrock-converse-stream.ts——AWS Bedrock ConverseStream。
//!
//! 不引入 AWS SDK（`ARCHITECTURE.md` §8 第 6 条）：
//! - `sigv4` 手写 SigV4 签名（sha2 + hmac）；
//! - [`EventStreamDecoder`] 手写 `application/vnd.amazon.eventstream` 分帧；
//! - 凭据链、region 解析与错误映射逐条对照 TS 源。
//!
//! 所有 HTTP 都经过 [`crate::http::HttpClient`]，测试因此可以注入 wiremock。
//!
//! # 这个文件负责什么
//!
//! 这是 `bedrock-converse-stream` 这一个 API 的完整适配器：把 Kiana 内部的
//! [`Message`]/[`Tool`] 转录（一次对话）翻译成 AWS Bedrock ConverseStream 的
//! `POST /model/{modelId}/converse-stream` 请求，再把服务端返回的
//! `application/vnd.amazon.eventstream` 二进制帧还原成 [`AssistantMessageEvent`] 事件流。
//!
//! 上游调用方：
//!
//! - `providers::amazon_bedrock` 通过 [`bedrock_converse_stream_api`] 取得实现了
//!   [`crate::models::ProviderStreams`] 的适配器并挂到 `amazon-bedrock` provider 上；
//! - `compat` 的注册表在启动时调用同一个工厂；
//! - [`crate::bedrock_provider::bedrock_provider_module`] 是静态模块入口，可用
//!   [`set_bedrock_provider_module`] 覆盖工厂返回的实例；
//! - 测试直接调用 [`stream`] 与 [`stream_with_items`]（后者用伪造的流项驱动状态机，
//!   不经过网络）。
//!
//! 下游：本文件只产出事件流（[`crate::event_stream::AssistantMessageEventStream`]），
//! 不负责会话持久化、多轮上下文管理，也不负责跨请求的上层重试编排（`utils::retry` /
//! `utils::provider_retry` 那类，属于上层职责）；单次请求内的 SDK 级重试在本文件实现
//! （见下文“重试与取消”一节）。
//!
//! # 一次请求的完整流程
//!
//! 1. [`stream`] 同步返回一个事件流，并把真正的请求工作丢到后台任务（`spawn_forward`）。
//! 2. `run_stream` 折叠中途 system 消息，按 [`BedrockOptions`] 解析客户端配置，再调用
//!    [`build_command_input`] 生成 ConverseStream 的 JSON 命令输入。
//! 3. `build_http_request` 把它变成 HTTP 请求：URL 里的 `modelId` 做百分号编码，
//!    body 里删掉 `modelId`，然后用 bearer token 或 SigV4 签名。
//! 4. 请求与“首帧预读”一起构成一次尝试（`read_first_event`）；非 2xx、传输失败与
//!    首帧异常会按 SDK 的 StandardRetryStrategy 规则重试。
//! 5. 首帧之后，`run_stream` 循环读取字节块 → [`EventStreamDecoder`] 分帧 →
//!    [`eventstream_message_to_item`] 解码 → [`handle_stream_item`] 更新状态机并推送事件。
//! 6. 正常结束时推送 `done` 事件；任何失败都走 `finish_stream_error`：补齐未关闭的块、
//!    写 `stop_reason`/`error_message`、追加诊断，最后推送 `error` 事件并关闭流。
//!
//! # 核心数据结构
//!
//! - [`BedrockOptions`]：调用方可见的请求选项（region/profile/思考档位/bearer token 等）。
//! - [`BedrockClientConfig`]：从选项与 env 解析出的“客户端构造参数”，决定 URL、鉴权与
//!   凭据来源；解析过程是纯函数，便于单测。
//! - [`BedrockFailure`]：把 TS 运行时可能抛出的服务异常、普通 `Error`、流内裸对象与中止
//!   统一成可匹配的枚举，是错误文本与诊断字段的来源。
//! - [`EventStreamDecoder`]：增量式 AWS event-stream 分帧器（跨 chunk 缓冲、CRC 校验）。
//! - [`DecodedEventStreamItem`]：一条消息的解码结果（正常项 / 跳过 / 服务异常 / 普通错误）。
//! - [`OpenBlock`] / [`OpenBlocks`]：以 `contentBlockIndex` 为键的“正在流式写入的块”表，
//!   记录它在 [`AssistantMessage::content`] 里的下标，以及工具调用参数或重加密思考字节。
//!
//! # 流式状态机的不变量
//!
//! - `contentBlockStart` 只用于 `toolUse`；文本与思考块没有 start 事件，在第一个 delta
//!   到来时隐式创建。
//! - 同一个 `contentBlockIndex` 的 delta 必须落到同一个 content 下标；`contentBlockStop`
//!   用 `shift_remove` 消费，因此重复的 stop 是无操作。
//! - 工具调用参数只在 `contentBlockStop` 时 `finish()` 一次，得到最终参数与可能的
//!   `arguments_error`；流式期间只更新 partial。
//! - 被脱敏（redacted）的思考内容不会进入 `thinking` 正文：正文只放
//!   [`REDACTED_THINKING_PLACEHOLDER`]，原始字节按块累积，在块结束（或流异常结束）时
//!   base64 写入 `thinking_signature`；一旦进入 redacted 状态，后续 `signature` 被忽略。
//! - 每个 partial 事件都携带 output 的克隆。解析器状态（[`ToolCallArgumentsStream`]、
//!   脱敏思考的原始字节）只放在 [`OpenBlocks`] 里；已解析的文本、工具调用 partial 参数
//!   与思考签名仍写回 content 块，并随 partial 克隆出现在对外事件中。
//!
//! # 重试与取消
//!
//! 重试只发生在 `run_stream` 的“发送请求 + 首帧预读”阶段，最多 `AWS_MAX_ATTEMPTS`
//! 次尝试（缺省 3，含首次）。可重试的是 THROTTLING 与 TRANSIENT 两类（见
//! `is_retryable_failure`）；退避带抖动，可用 `retry-after` / `x-amz-retry-after` 提示。
//! 退避 sleep 不可被取消令牌中断，取消会在下一次网络调用或读块时生效。
//!
//! 取消令牌（[`StreamOptions::signal`]）触发时，读取返回中止错误，最终 `stop_reason`
//! 置为 `aborted` 并推送 `error` 事件（`reason` 同样是 aborted）。
//!
//! # 失败模式
//!
//! - 配置错误：找不到 region、凭据链无可用凭据 → 请求前失败。
//! - 传输错误：连接失败/超时/读流中断 → `BedrockFailure::Named("Error", ...)`。只有
//!   发送阶段（`client.execute`）的传输失败会进入重试循环；首帧预读阶段的传输错误会被
//!   补成 `status=200` 的 Named Error，因此不再重试；首帧之后的读流中断直接失败、不重试。
//! - 协议错误：event-stream 长度/CRC/头类型非法 → 立即失败（解码器不尝试重新同步）。
//! - 服务错误：非 2xx（REST JSON 错误体）或流内 modeled 异常 → 保留 status/requestId，
//!   文本按 TS 的 `formatBedrockError` 规则拼接。
//! - 流提前结束：没有 `messageStop`（`stop_reason` 仍是 `pending`）→ 以错误结束。
//!
//! # 阅读顺序
//!
//! 1. [`BedrockOptions`] 与 [`resolve_client_config`]：先看输入如何变成客户端配置。
//! 2. [`build_command_input`] → [`convert_messages`] / [`convert_tool_config`]：请求体。
//! 3. `build_http_request` → [`sign_sigv4`]：URL/头/签名。
//! 4. `run_stream` → `read_first_event` → [`handle_stream_item`]：流式主循环与状态机。
//! 5. [`format_bedrock_error`] / [`append_bedrock_failure_diagnostic`]：失败收尾。
//! 6. 凭据链（`resolve_bedrock_credentials` 及其下级函数）：仅在需要理解 AWS 认证时读。

use std::sync::Arc;

use base64::Engine as _;
use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::api::constrained_sampling::{
    get_json_schema_tool_parameters, resolve_json_schema_strict_sampling,
};
use crate::api::lazy::{lazy_stream_error, spawn_forward};
use crate::api::simple_options::{
    adjust_max_tokens_for_thinking, build_base_options, clamp_max_tokens_to_context,
    clamp_reasoning,
};
use crate::api::transform_messages::{
    NormalizeToolCallIdFn, split_error_tool_result_images, transform_messages_with,
};
use crate::error::{AiError, AiErrorKind};
use crate::event_stream::AssistantMessageEventStream;
use crate::http::{ByteStream, HttpClient, HttpError, HttpRequest};
use crate::models::{ProviderStreams, calculate_cost};
use crate::types::UserAgentMode;
use crate::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageDiagnostic, AssistantMessageEvent,
    AssistantRole, CacheRetention, ClientIdentity, ErrorReason, JsonObject, Message, Model,
    ProviderEnv, SimpleStreamOptions, StopReason, StreamOptions, TextContent, ThinkingBudgets,
    ThinkingLevel, Tool, ToolCall, ToolCallKind, TranscriptContext, Usage, UserContent,
    UserContentBlock,
};
use crate::utils::diagnostics::append_assistant_message_diagnostic;
use crate::utils::error_body::{
    ProviderErrorFields, ProviderErrorValue, ResponseBody, normalize_provider_error,
};
use crate::utils::headers::provider_headers_to_record;
use crate::utils::identity::get_client_version;
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::streaming_json::ToolCallArgumentsStream;
use crate::utils::text::get_system_message_text;
use crate::utils::transcript::{
    collapse_system_messages, get_current_tools, get_initial_system_message,
    without_initial_system_message,
};

/// `BedrockThinkingDisplay`。
///
/// TS 里是字符串字面量联合 `"summarized" | "omitted"`；Rust 用 `&'static str` 表达，
/// 只是类型别名，不会在运行时校验取值。
pub type BedrockThinkingDisplay = &'static str;
/// `"summarized"`：要求服务端在 `thinking.display` 里返回思考摘要（缺省行为）。
pub const THINKING_DISPLAY_SUMMARIZED: BedrockThinkingDisplay = "summarized";
/// `"omitted"`：要求服务端省略思考正文，只保留签名/加密内容。
pub const THINKING_DISPLAY_OMITTED: BedrockThinkingDisplay = "omitted";

/// `BedrockOptions`。
///
/// `BedrockOptions extends StreamOptions`：`base` 承载通用选项（鉴权、取消令牌、
/// 生命周期回调、`fetch` 注入、缓存策略等），其余字段是 Bedrock 专属项。
/// [`bedrock_options_from_stream_options`] 负责从 `StreamOptions::extra` 里按 TS 字段名
/// 还原这些专属项。
#[derive(Default)]
pub struct BedrockOptions {
    /// 通用流选项；Bedrock 专属字段之外的配置都在这里。
    pub base: StreamOptions,
    /// 显式 region；优先级见 [`resolve_client_config`]（ARN 内嵌 > 本字段/env > 端点 > 缺省）。
    pub region: Option<String>,
    /// AWS 共享配置的 profile 名。显式设置时会压制 env 里的 AK/SK，交给凭据链按 profile 取。
    pub profile: Option<String>,
    /// `"auto" | "any" | "none" | { type: "tool"; name }`。
    ///
    /// 这是 Kiana 侧的取值形状；[`convert_tool_config`] 把它翻译成 Bedrock 的
    /// `toolChoice`。`"none"` 会让整个 `toolConfig` 不发送。
    pub tool_choice: Option<Value>,
    /// 思考档位；`None` 表示完全不发 reasoning 相关字段。
    pub reasoning: Option<ThinkingLevel>,
    /// 非自适应 Claude 的 `thinking.budget_tokens` 覆盖值，按档位给出。
    pub thinking_budgets: Option<ThinkingBudgets>,
    /// 非自适应 Claude 是否附加 `interleaved-thinking-2025-05-14` beta；缺省视为 `true`。
    pub interleaved_thinking: Option<bool>,
    /// 写入 `thinking.display` 的值；缺省 `"summarized"`，GovCloud 目标上不发送。
    pub thinking_display: Option<String>,
    /// 原样放进 ConverseStream 的 `requestMetadata`（键和值都必须是字符串）。
    pub request_metadata: Option<IndexMap<String, String>>,
    /// Bedrock bearer token；缺省按 `base.api_key` → `AWS_BEARER_TOKEN_BEDROCK` 回落。
    pub bearer_token: Option<String>,
}

/// `EMPTY_TEXT_PLACEHOLDER`。
///
/// 当用户文本、工具结果等内容被过滤成空时，用这个占位符保证请求里的每个 content
/// 数组至少有一个合法块（空文本块不会发送）。
pub const EMPTY_TEXT_PLACEHOLDER: &str = "<empty>";
/// `THINKING_BINDING_CONTROLS_BETA`。
///
/// 自适应思考配合 `thinking.block_binding`（前缀不匹配时丢弃该块）所需的 beta 名；
/// 只有非 GovCloud 且模型支持时才写进 `anthropic_beta`。
pub const THINKING_BINDING_CONTROLS_BETA: &str = "thinking-binding-controls-2026-08-01";
/// `REDACTED_THINKING_PLACEHOLDER`。
///
/// 收到 `redactedContent` 时写进 `thinking` 正文的可读占位符；真正的密文按字节累积，
/// 最终 base64 编码进 `thinking_signature`（见 `handle_content_block_delta`）。
pub const REDACTED_THINKING_PLACEHOLDER: &str = "[Reasoning redacted]";
/// `MAX_BEDROCK_DIAGNOSTIC_VALUE_CHARS`。
///
/// 诊断里 requestId 这类值的长度上限，单位是 UTF-16 码元（与 JS `string.length` 一致），
/// 超过就当作缺失，避免把超长/异常值写进消息诊断。
const MAX_BEDROCK_DIAGNOSTIC_VALUE_CHARS: usize = 200;
/// `BEDROCK_DATA_RETENTION_DOCS_URL`。
///
/// 错误文本包含 “data retention mode” 时附带的官方文档链接。
const BEDROCK_DATA_RETENTION_DOCS_URL: &str =
    "https://docs.aws.amazon.com/bedrock/latest/userguide/data-retention.html";

/// `BEDROCK_ERROR_PREFIXES`。
///
/// 把 modeled 的异常 `name` 映射成面向用户的英文前缀（`ThrottlingException` →
/// “Throttling error”）。未知名字原样作为前缀；这个映射只影响展示文本，不影响诊断里的
/// `errorCode`。
fn bedrock_error_prefix(name: &str) -> Option<&'static str> {
    match name {
        "InternalServerException" => Some("Internal server error"),
        "ModelStreamErrorException" => Some("Model stream error"),
        "ValidationException" => Some("Validation error"),
        "ThrottlingException" => Some("Throttling error"),
        "ServiceUnavailableException" => Some("Service unavailable"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 请求构造
// ---------------------------------------------------------------------------

/// `createNonBlankTextBlock(text)`。
///
/// 先调用 [`sanitize_surrogates`]（对应 TS 的代理项清理步骤；对合法的 `&str` 是恒等
/// 变换，孤立代理项在解码阶段已按 U+FFFD 处理），再用 JS 语义的 `trim` 判断空白。
/// 返回 `None` 表示这个文本块应当整个丢弃，而不是发送空串。
fn create_non_blank_text_block(text: &str) -> Option<Value> {
    let sanitized = sanitize_surrogates(text);
    if kiana_json::js_string::js_trim(&sanitized).is_empty() {
        None
    } else {
        Some(crate::api::google_shared::object([(
            "text",
            Value::String(sanitized),
        )]))
    }
}

/// `createRequiredTextBlock(text)`。
///
/// 与 `create_non_blank_text_block` 相同，但空白文本不会被丢弃，而是替换成
/// [`EMPTY_TEXT_PLACEHOLDER`]。用于“必须有内容”的位置（如 user 消息的主体）。
fn create_required_text_block(text: &str) -> Value {
    create_non_blank_text_block(text).unwrap_or_else(|| {
        crate::api::google_shared::object([("text", Value::String(EMPTY_TEXT_PLACEHOLDER.into()))])
    })
}

/// `sanitizeBedrockDocument(value)`：递归过滤空属性名。
///
/// 回放历史工具调用时会得到不可控的 `input` 对象；空属性名会被过滤掉（测试锁定
/// “只对回放的 Bedrock 输入删除空属性名”这一行为），避免把空键发给服务端。
/// 返回的是新构造的 JSON 树，保留原有键序。
pub fn sanitize_bedrock_document(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(sanitize_bedrock_document).collect()),
        Value::Object(map) => {
            let mut result = Map::new();
            for (key, nested) in map {
                if key.is_empty() {
                    continue;
                }
                result.insert(key.clone(), sanitize_bedrock_document(nested));
            }
            Value::Object(result)
        }
        other => other.clone(),
    }
}

/// `createImageBlock(mimeType, data)`。
///
/// MIME 只接受四种 Bedrock ImageFormat（`image/jpg` 视作 jpeg）。`data` 是 base64：
/// 先解码做合法性校验，再按标准字母表重新编码，避免把非规范 base64 原样带给服务端。
///
/// 错误：未知 MIME 或非法 base64 都返回 [`AiErrorKind::Request`]。本函数只产生
/// `AiError`；[`build_command_input`] 也只向上传播它，真正转成
/// `BedrockFailure::Named("Error", ...)` 的是 `run_stream` 调用
/// [`build_command_input`] 处的 `map_err`。
fn create_image_block(mime_type: &str, data: &str) -> Result<Value, AiError> {
    let format = match mime_type {
        "image/jpeg" | "image/jpg" => "jpeg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        other => {
            return Err(AiError::new(
                AiErrorKind::Request,
                format!("Unknown image type: {other}"),
            ));
        }
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|error| AiError::new(AiErrorKind::Request, error.to_string()))?;
    Ok(crate::api::google_shared::object([(
        "image",
        crate::api::google_shared::object([
            (
                "source",
                crate::api::google_shared::object([(
                    "bytes",
                    Value::String(base64::engine::general_purpose::STANDARD.encode(bytes)),
                )]),
            ),
            ("format", Value::String(format.into())),
        ]),
    )]))
}

/// `convertToolResultContent(content)`。
///
/// 把工具结果的用户内容块转成 Bedrock `toolResult.content`：图片转 `image`，非空白文本
/// 转 `text`。结果为空时补一个 [`EMPTY_TEXT_PLACEHOLDER`] 文本块，保证数组非空
/// （Bedrock 要求 `toolResult.content` 非空）。
fn convert_tool_result_content(content: &[UserContentBlock]) -> Result<Vec<Value>, AiError> {
    let mut result = Vec::new();
    for block in content {
        match block {
            UserContentBlock::Image(image) => {
                result.push(create_image_block(&image.mime_type, &image.data)?);
            }
            UserContentBlock::Text(text) => {
                if let Some(text_block) = create_non_blank_text_block(&text.text) {
                    result.push(text_block);
                }
            }
        }
    }
    if result.is_empty() {
        result.push(crate::api::google_shared::object([(
            "text",
            Value::String(EMPTY_TEXT_PLACEHOLDER.into()),
        )]));
    }
    Ok(result)
}

/// `normalizeToolCallId(id)`。
///
/// 工具调用 id 的请求侧清洗：只保留 ASCII 字母数字与 `_`/`-`，其它字符逐个替换成
/// `_`，并截断到 64 个字符（TS 的同一规则）。只用于回放历史；流式响应的 id 原样保留。
fn normalize_tool_call_id(id: &str) -> String {
    let sanitized: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    sanitized.chars().take(64).collect()
}

/// `convertMessages(context, model, cacheRetention, env)`。
///
/// 把转录转成 ConverseStream 的 `messages` 数组。步骤：
///
/// 1. 去掉首条 system 消息（它由 [`build_command_input`] 单独放进 `system` 字段），
///    再用 `transform_messages_with` 做通用归一化；工具调用 id 用
///    `normalize_tool_call_id` 清洗。
/// 2. user 消息：文本/图片逐块转换，空白文本被丢弃，全空时补 [`EMPTY_TEXT_PLACEHOLDER`]。
/// 3. assistant 消息：文本、工具调用（`toolUse`）与思考内容；全空的消息被跳过。
///    Claude 的思考内容带签名时用 `reasoningContent.reasoningText`，否则降级为纯文本。
/// 4. toolResult：连续的多个工具结果合并成一条 user 消息；Claude 的
///    错误结果里的图片会被移到所有 `toolResult` 之后，因为 Claude 拒绝错误结果内的
///    非文本块。
/// 5. 末尾缓存点：满足条件时给最后一条 user 消息追加 `cachePoint`。
///
/// 不变量：返回的每个 `content` 数组都非空；工具调用的 `input` 已经过
/// [`sanitize_bedrock_document`] 过滤。
///
/// 错误：图片 MIME 或 base64 非法时返回 [`AiErrorKind::Request`]。
pub fn convert_messages(
    context: &TranscriptContext,
    model: &Model,
    cache_retention: CacheRetention,
    env: Option<&ProviderEnv>,
) -> Result<Vec<Value>, AiError> {
    let mut result: Vec<Value> = Vec::new();
    let normalize: &NormalizeToolCallIdFn =
        &|id: &str, _model: &Model, _message: &AssistantMessage| normalize_tool_call_id(id);
    // 首条 system 消息不进 messages（由 build_system_prompt 放进 system 字段）；
    // transform_messages_with 负责通用的工具调用配对归一化与 id 清洗。
    let transformed = transform_messages_with(
        &without_initial_system_message(&context.0.messages),
        model,
        Some(normalize),
    );

    let mut i = 0usize;
    while i < transformed.len() {
        let message = &transformed[i];
        match message {
            Message::User(user) => {
                let mut content: Vec<Value> = Vec::new();
                match &user.content {
                    UserContent::Text(text) => {
                        content.push(create_required_text_block(text));
                    }
                    UserContent::Blocks(blocks) => {
                        for block in blocks {
                            match block {
                                UserContentBlock::Text(text) => {
                                    if let Some(text_block) =
                                        create_non_blank_text_block(&text.text)
                                    {
                                        content.push(text_block);
                                    }
                                }
                                UserContentBlock::Image(image) => {
                                    content
                                        .push(create_image_block(&image.mime_type, &image.data)?);
                                }
                            }
                        }
                        if content.is_empty() {
                            content.push(crate::api::google_shared::object([(
                                "text",
                                Value::String(EMPTY_TEXT_PLACEHOLDER.into()),
                            )]));
                        }
                    }
                }
                result.push(crate::api::kiana_messages::json_object([
                    ("role", Value::String("user".into())),
                    ("content", Value::Array(content)),
                ]));
            }
            Message::Assistant(assistant) => {
                if assistant.content.is_empty() {
                    i += 1;
                    continue;
                }
                let mut content_blocks: Vec<Value> = Vec::new();
                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Text(text) => {
                            if let Some(text_block) = create_non_blank_text_block(&text.text) {
                                content_blocks.push(text_block);
                            }
                        }
                        AssistantContentBlock::ToolCall(call) => {
                            content_blocks.push(crate::api::google_shared::object([(
                                "toolUse",
                                crate::api::google_shared::object([
                                    ("toolUseId", Value::String(call.id.clone())),
                                    ("name", Value::String(call.name.clone())),
                                    (
                                        "input",
                                        sanitize_bedrock_document(&Value::Object(
                                            call.arguments
                                                .iter()
                                                .map(|(key, value)| (key.clone(), value.clone()))
                                                .collect(),
                                        )),
                                    ),
                                ]),
                            )]));
                        }
                        AssistantContentBlock::Thinking(thinking) => {
                            if thinking.redacted == Some(true) {
                                if let Some(bytes) =
                                    decode_redacted_content(thinking.thinking_signature.as_deref())
                                    && !bytes.is_empty()
                                {
                                    content_blocks.push(crate::api::google_shared::object([(
                                        "reasoningContent",
                                        crate::api::google_shared::object([(
                                            "redactedContent",
                                            Value::String(
                                                base64::engine::general_purpose::STANDARD
                                                    .encode(bytes),
                                            ),
                                        )]),
                                    )]));
                                }
                                continue;
                            }
                            let text = sanitize_surrogates(&thinking.thinking);
                            if kiana_json::js_string::js_trim(&text).is_empty() {
                                continue;
                            }
                            if supports_thinking_signature(model) {
                                match thinking.thinking_signature.as_deref().filter(|signature| {
                                    !kiana_json::js_string::js_trim(signature).is_empty()
                                }) {
                                    None => content_blocks.push(crate::api::google_shared::object(
                                        [("text", Value::String(text))],
                                    )),
                                    Some(signature) => {
                                        content_blocks.push(crate::api::google_shared::object([(
                                            "reasoningContent",
                                            crate::api::google_shared::object([(
                                                "reasoningText",
                                                crate::api::google_shared::object([
                                                    ("text", Value::String(text)),
                                                    (
                                                        "signature",
                                                        Value::String(signature.to_string()),
                                                    ),
                                                ]),
                                            )]),
                                        )]))
                                    }
                                }
                            } else {
                                content_blocks.push(crate::api::google_shared::object([(
                                    "reasoningContent",
                                    crate::api::google_shared::object([(
                                        "reasoningText",
                                        crate::api::google_shared::object([(
                                            "text",
                                            Value::String(text),
                                        )]),
                                    )]),
                                )]));
                            }
                        }
                    }
                }
                if content_blocks.is_empty() {
                    i += 1;
                    continue;
                }
                result.push(crate::api::kiana_messages::json_object([
                    ("role", Value::String("assistant".into())),
                    ("content", Value::Array(content_blocks)),
                ]));
            }
            // 连续的 toolResult 合并为一条 user 消息；j 停在第一个非 toolResult 上，
            // 循环末尾用 i = j - 1 跳过已消费的项（i 还会再 +1）。
            Message::ToolResult(_) => {
                let mut tool_results: Vec<Value> = Vec::new();
                let mut error_images: Vec<Value> = Vec::new();
                let mut j = i;
                while j < transformed.len() {
                    let Message::ToolResult(tool_msg) = &transformed[j] else {
                        break;
                    };
                    // Claude 拒绝错误工具结果里的图片：拆分出来，稍后放到 toolResult 之后。
                    let split = if is_anthropic_claude_model(model) {
                        split_error_tool_result_images(tool_msg)
                    } else {
                        None
                    };
                    let split_content: Vec<UserContentBlock> = match &split {
                        Some(split) => split
                            .content
                            .iter()
                            .map(|text| {
                                UserContentBlock::Text(TextContent {
                                    kind_text: crate::types::TextKind::Text,
                                    text: text.text.clone(),
                                    text_signature: None,
                                })
                            })
                            .collect(),
                        None => tool_msg.content.clone(),
                    };
                    tool_results.push(crate::api::google_shared::object([(
                        "toolResult",
                        crate::api::google_shared::object([
                            ("toolUseId", Value::String(tool_msg.tool_call_id.clone())),
                            (
                                "content",
                                Value::Array(convert_tool_result_content(&split_content)?),
                            ),
                            (
                                "status",
                                Value::String(if tool_msg.is_error {
                                    "error".into()
                                } else {
                                    "success".into()
                                }),
                            ),
                        ]),
                    )]));
                    if let Some(split) = split {
                        error_images.push(crate::api::google_shared::object([(
                            "text",
                            Value::String(split.caption.clone()),
                        )]));
                        for image in &split.images {
                            error_images.push(create_image_block(&image.mime_type, &image.data)?);
                        }
                    }
                    j += 1;
                }
                i = j.saturating_sub(1);
                let mut content = tool_results;
                content.extend(error_images);
                result.push(crate::api::kiana_messages::json_object([
                    ("role", Value::String("user".into())),
                    ("content", Value::Array(content)),
                ]));
            }
            Message::System(_) => {}
        }
        i += 1;
    }

    // 末尾缓存点。
    // 仅当显式要求缓存、模型支持、且最后一条是 user 消息时追加到其 content 末尾；
    // cachePoint 是 content 数组的成员，不是请求顶层字段。
    if cache_retention != CacheRetention::None
        && supports_prompt_caching(model, env)
        && !result.is_empty()
    {
        let last = result.len() - 1;
        if result[last].get("role").and_then(Value::as_str) == Some("user") {
            let cache_point = cache_point_value(cache_retention);
            if let Some(Value::Array(content)) = result[last].get_mut("content") {
                content.push(cache_point);
            }
        }
    }
    Ok(result)
}

/// 构造一个 `cachePoint` 内容块。
///
/// `type` 固定为 `"default"`；[`CacheRetention::Long`] 额外写 `"ttl": "1h"`，
/// Short 则省略 ttl，交给服务端默认值。
fn cache_point_value(cache_retention: CacheRetention) -> Value {
    let mut point = Map::new();
    point.insert("type".into(), Value::String("default".into()));
    if cache_retention == CacheRetention::Long {
        point.insert("ttl".into(), Value::String("1h".into()));
    }
    crate::api::google_shared::object([("cachePoint", Value::Object(point))])
}

/// `buildSystemPrompt(systemPrompt, model, cacheRetention, env)`。
///
/// 返回 `system` 字段的内容块数组：一个文本块，外加可选的 `cachePoint`。
/// `system_prompt` 为 `None` 或空串时返回 `None`（请求里不出现 `system` 字段）。
/// 文本先经 [`sanitize_surrogates`]（对应 TS 的清理步骤；对合法 `&str` 是恒等变换）。
pub fn build_system_prompt(
    system_prompt: Option<&str>,
    model: &Model,
    cache_retention: CacheRetention,
    env: Option<&ProviderEnv>,
) -> Option<Vec<Value>> {
    let system_prompt = system_prompt.filter(|text| !text.is_empty())?;
    let mut blocks = vec![crate::api::google_shared::object([(
        "text",
        Value::String(sanitize_surrogates(system_prompt)),
    )])];
    if cache_retention != CacheRetention::None && supports_prompt_caching(model, env) {
        blocks.push(cache_point_value(cache_retention));
    }
    Some(blocks)
}

/// `convertToolConfig(tools, toolChoice, supportsStrictMode)`。
///
/// 返回 `toolConfig`（`{ tools, toolChoice? }`），或 `None` 表示请求里不出现该字段。
/// 两种情况返回 `None`：工具列表为空，或 `toolChoice == "none"`（此时连工具定义也
/// 一起省掉，与 TS 语义一致）。
///
/// 每个工具都经 `constrained_sampling` 解析 strict 采样：`strict` 只有在解析结果
/// **恰好为** `Some(true)` 时才写进 `toolSpec`（`false`/`None` 都不写，避免改变服务端
/// 默认行为）。`toolChoice` 的对象形状是 Bedrock 的：`"auto"`/`"any"` 映射成
/// `{auto:{}}`/`{any:{}}`，`{type:"tool",name}` 映射成 `{tool:{name}}`。
///
/// 错误：JSON schema 无法生成时向上传播 [`AiError`]。
pub fn convert_tool_config(
    tools: Option<&[Tool]>,
    tool_choice: Option<&Value>,
    supports_strict_mode: bool,
) -> Result<Option<Value>, AiError> {
    let Some(tools) = tools.filter(|tools| !tools.is_empty()) else {
        return Ok(None);
    };
    if matches!(tool_choice, Some(Value::String(choice)) if choice == "none") {
        return Ok(None);
    }
    let mut bedrock_tools = Vec::with_capacity(tools.len());
    for tool in tools {
        let strict = resolve_json_schema_strict_sampling(tool, supports_strict_mode, None)?;
        let parameters = get_json_schema_tool_parameters(tool, strict)?;
        let mut tool_spec = Map::new();
        tool_spec.insert("name".into(), Value::String(tool.name.clone()));
        tool_spec.insert(
            "description".into(),
            Value::String(tool.description.clone()),
        );
        tool_spec.insert(
            "inputSchema".into(),
            crate::api::google_shared::object([("json", parameters)]),
        );
        // 只有明确 true 才写 strict；None/false 都省略，保持服务端默认。
        if strict == Some(true) {
            tool_spec.insert("strict".into(), Value::Bool(true));
        }
        bedrock_tools.push(crate::api::google_shared::object([(
            "toolSpec",
            Value::Object(tool_spec),
        )]));
    }

    let bedrock_tool_choice = match tool_choice {
        Some(Value::String(choice)) if choice == "auto" => Some(crate::api::google_shared::object(
            [("auto", crate::api::google_shared::object([]))],
        )),
        Some(Value::String(choice)) if choice == "any" => Some(crate::api::google_shared::object(
            [("any", crate::api::google_shared::object([]))],
        )),
        Some(Value::Object(object))
            if object.get("type").and_then(Value::as_str) == Some("tool") =>
        {
            let name = object.get("name").and_then(Value::as_str).unwrap_or("");
            Some(crate::api::google_shared::object([(
                "tool",
                crate::api::google_shared::object([("name", Value::String(name.into()))]),
            )]))
        }
        _ => None,
    };

    let mut config = Map::new();
    config.insert("tools".into(), Value::Array(bedrock_tools));
    if let Some(choice) = bedrock_tool_choice {
        config.insert("toolChoice".into(), choice);
    }
    Ok(Some(Value::Object(config)))
}

/// `mapStopReason(reason)`。
///
/// 返回 `(Kiana stop_reason, 可选错误文本)`。映射关系：
///
/// ```text
/// end_turn / stop_sequence                    -> stop
/// max_tokens / model_context_window_exceeded  -> length
/// tool_use                                    -> toolUse
/// 其它非空字符串                                -> error + "Provider stopped with: <原值>"
/// 空串或缺失                                    -> error（无额外文本）
/// ```
///
/// 第二项只有未知的停止原因才非空，最终写进 `AssistantMessage::error_message`。
pub fn map_stop_reason(reason: Option<&str>) -> (StopReason, Option<String>) {
    match reason {
        Some("end_turn") | Some("stop_sequence") => (StopReason::Stop, None),
        Some("max_tokens") | Some("model_context_window_exceeded") => (StopReason::Length, None),
        Some("tool_use") => (StopReason::ToolUse, None),
        Some(reason) if !reason.is_empty() => (
            StopReason::Error,
            Some(format!("Provider stopped with: {reason}")),
        ),
        _ => (StopReason::Error, None),
    }
}

// ---------------------------------------------------------------------------
// 模型能力判定
// ---------------------------------------------------------------------------

/// `getModelMatchCandidates(modelId, modelName?)`。
///
/// 返回用于“按子串识别模型家族”的候选字符串：`model_id`（以及可选的 `model_name`）
/// 各自转小写，并额外生成一个把空白/`_`/`.`/`:` 连续段替换成 `-` 的变体
/// （对应 TS 的 `/[\s_.:]+/g`）。例如 `us.anthropic.claude-opus-4-8` 与
/// `Claude Opus 4.8` 都能命中 `opus-4-8`。
///
/// 这是纯字符串匹配，不查模型目录；ARN 形式的 id 里没有模型名时靠 `model.name` 兜底，
/// 所以下面所有能力判定都同时接收 id 与 name。
pub fn get_model_match_candidates(model_id: &str, model_name: Option<&str>) -> Vec<String> {
    let values: Vec<&str> = match model_name {
        Some(name) => vec![model_id, name],
        None => vec![model_id],
    };
    let mut result = Vec::new();
    for value in values {
        let lower = value.to_lowercase();
        result.push(lower.clone());
        // /[\s_.:]+/g → "-"
        let mut replaced = String::new();
        let mut in_separator = false;
        for ch in lower.chars() {
            if ch.is_whitespace() || ch == '_' || ch == '.' || ch == ':' {
                if !in_separator {
                    replaced.push('-');
                    in_separator = true;
                }
            } else {
                replaced.push(ch);
                in_separator = false;
            }
        }
        result.push(replaced);
    }
    result
}

/// `supportsAdaptiveThinking(modelId, modelName)`。
///
/// 候选字符串里出现 `opus-4-6` / `opus-4-7` / `opus-4-8` / `opus-5` / `sonnet-4-6` /
/// `sonnet-5` / `haiku-5` / `fable-5` 任一子串即为真。判定结果决定
/// [`build_additional_model_request_fields`] 发自适应思考
/// （`thinking.type = "adaptive"`）还是固定 `budget_tokens`。
pub fn supports_adaptive_thinking(model_id: &str, model_name: Option<&str>) -> bool {
    let candidates = get_model_match_candidates(model_id, model_name);
    candidates.iter().any(|candidate| {
        [
            "opus-4-6",
            "opus-4-7",
            "opus-4-8",
            "opus-5",
            "sonnet-4-6",
            "sonnet-5",
            "haiku-5",
            "fable-5",
        ]
        .iter()
        .any(|needle| candidate.contains(needle))
    })
}

/// `supportsNativeXhighEffort(model)`。
///
/// 支持原生 `xhigh` 档位的家族：`opus-4-7` / `opus-4-8` / `opus-5` / `sonnet-5` /
/// `haiku-5` / `fable-5`。[`map_thinking_level_to_effort`] 只有在为真时才允许把
/// `ThinkingLevel::Xhigh` 原样发成 `"xhigh"`；否则会落回映射/`"high"`。
pub fn supports_native_xhigh_effort(model: &Model) -> bool {
    let candidates = get_model_match_candidates(&model.id, Some(&model.name));
    candidates.iter().any(|candidate| {
        [
            "opus-4-7", "opus-4-8", "opus-5", "sonnet-5", "haiku-5", "fable-5",
        ]
        .iter()
        .any(|needle| candidate.contains(needle))
    })
}

/// `supportsThinkingBlockBinding(model)`。
///
/// 是否支持 `thinking.block_binding`。当前实现直接复用
/// [`supports_native_xhigh_effort`]（两者家族名单一致）；仅在非 GovCloud 目标上生效。
pub fn supports_thinking_block_binding(model: &Model) -> bool {
    supports_native_xhigh_effort(model)
}

/// `mapThinkingLevelToEffort(model, level)`。
///
/// TS 对 `thinkingLevelMap` 里的字符串**原样返回**（只做类型断言，不做白名单收窄）。
///
/// 优先级：`Xhigh` 且模型支持原生 xhigh → `"xhigh"`；否则取
/// `model.thinking_level_map` 的映射（原样返回）；再否则 minimal/low → `"low"`，
/// medium → `"medium"`，其余（含 `None`）→ `"high"`。
pub fn map_thinking_level_to_effort(model: &Model, level: Option<ThinkingLevel>) -> String {
    if level == Some(ThinkingLevel::Xhigh) && supports_native_xhigh_effort(model) {
        return "xhigh".to_string();
    }
    let mapped = level.and_then(|level| {
        model
            .thinking_level_map
            .as_ref()
            .and_then(|map| map.get(&thinking_to_model_level(level)))
            .and_then(|value| value.as_deref())
    });
    if let Some(mapped) = mapped {
        return mapped.to_string();
    }
    match level {
        Some(ThinkingLevel::Minimal) | Some(ThinkingLevel::Low) => "low".to_string(),
        Some(ThinkingLevel::Medium) => "medium".to_string(),
        Some(ThinkingLevel::High) => "high".to_string(),
        _ => "high".to_string(),
    }
}

fn thinking_to_model_level(level: ThinkingLevel) -> crate::types::ModelThinkingLevel {
    match level {
        ThinkingLevel::Minimal => crate::types::ModelThinkingLevel::Minimal,
        ThinkingLevel::Low => crate::types::ModelThinkingLevel::Low,
        ThinkingLevel::Medium => crate::types::ModelThinkingLevel::Medium,
        ThinkingLevel::High => crate::types::ModelThinkingLevel::High,
        ThinkingLevel::Xhigh => crate::types::ModelThinkingLevel::Xhigh,
        ThinkingLevel::Max => crate::types::ModelThinkingLevel::Max,
    }
}

/// `resolveCacheRetention(cacheRetention?, env?)`：缺省 `"short"`。
///
/// 显式值优先；否则读 `KIANA_CACHE_RETENTION`（scoped env 覆盖进程环境），值为
/// `"long"` 时返回 Long，其余一律 Short。
pub fn resolve_cache_retention(
    cache_retention: Option<CacheRetention>,
    env: Option<&ProviderEnv>,
) -> CacheRetention {
    if let Some(retention) = cache_retention {
        return retention;
    }
    if get_provider_env_value("KIANA_CACHE_RETENTION", env).as_deref() == Some("long") {
        CacheRetention::Long
    } else {
        CacheRetention::Short
    }
}

/// `isAnthropicClaudeModel(model)`。
///
/// id/name（小写后）包含 `anthropic.claude` 或 `anthropic/claude`，或 name 包含
/// `claude` 即为真。name 的宽匹配用于 ARN 形式的 id（模型名不在 id 里）。
///
/// 该判定影响：思考签名回放（[`supports_thinking_signature`]）、错误工具结果里的图片
/// 拆分（[`convert_messages`]）、thinking 请求字段的构造
/// （[`build_additional_model_request_fields`]），以及 `maxTokens` 的模型上限回落与
/// [`stream_simple`] 的思考预算调整。cache point 白名单不经过本函数：
/// [`supports_prompt_caching`] 独立地用候选串 `contains("claude")` 判断。
pub fn is_anthropic_claude_model(model: &Model) -> bool {
    let id = model.id.to_lowercase();
    let name = model.name.to_lowercase();
    id.contains("anthropic.claude")
        || id.contains("anthropic/claude")
        || name.contains("anthropic.claude")
        || name.contains("anthropic/claude")
        || name.contains("claude")
}

/// `supportsPromptCaching(model, env)`。
///
/// 非 Claude 模型只有 `AWS_BEDROCK_FORCE_CACHE=1` 时才启用缓存点；Claude 模型按家族
/// 白名单判断：`fable-5`/`opus-5`/`sonnet-5`/`haiku-5`、任一含 `-4-` 的家族、
/// `claude-3-7-sonnet`、`claude-3-5-haiku`。全部是子串匹配，未命中返回 false。
pub fn supports_prompt_caching(model: &Model, env: Option<&ProviderEnv>) -> bool {
    let candidates = get_model_match_candidates(&model.id, Some(&model.name));
    let has_claude_ref = candidates
        .iter()
        .any(|candidate| candidate.contains("claude"));
    if !has_claude_ref {
        return get_provider_env_value("AWS_BEDROCK_FORCE_CACHE", env).as_deref() == Some("1");
    }
    let any = |needles: &[&str]| {
        candidates
            .iter()
            .any(|candidate| needles.iter().any(|needle| candidate.contains(needle)))
    };
    if any(&["fable-5", "opus-5", "sonnet-5", "haiku-5"]) {
        return true;
    }
    if any(&["-4-"]) {
        return true;
    }
    if any(&["claude-3-7-sonnet"]) {
        return true;
    }
    if any(&["claude-3-5-haiku"]) {
        return true;
    }
    false
}

/// `supportsThinkingSignature(model)`。
///
/// 复用 [`is_anthropic_claude_model`]：只有 Claude 的思考块携带可回放的签名。
/// [`convert_messages`] 用它决定回放思考内容时是否带 `signature`。只有本函数为真且
/// 签名缺失/空白时才降级成纯文本块，避免 Bedrock 拒绝没有签名的 `reasoningText`；
/// 非 Claude 分支（本函数为假）仍发送不带 `signature` 的
/// `reasoningContent.reasoningText`。
pub fn supports_thinking_signature(model: &Model) -> bool {
    is_anthropic_claude_model(model)
}

/// `isGovCloudBedrockTarget(model, options)`。
///
/// 配置的 region 以 `us-gov-` 开头，或 model id 以 `us-gov.` / `arn:aws-us-gov:` 开头，
/// 即视为 GovCloud 目标。GovCloud 上 [`build_additional_model_request_fields`] 不发
/// `thinking.display`，也不发 `thinking.block_binding`。
pub fn is_gov_cloud_bedrock_target(model: &Model, options: &BedrockOptions) -> bool {
    if get_configured_bedrock_region(options)
        .is_some_and(|region| region.to_lowercase().starts_with("us-gov-"))
    {
        return true;
    }
    let model_id = model.id.to_lowercase();
    model_id.starts_with("us-gov.") || model_id.starts_with("arn:aws-us-gov:")
}

/// `buildAdditionalModelRequestFields(model, options)`。
///
/// 构造 `additionalModelRequestFields`，返回 `None` 表示请求里不出现该字段。
/// 前置条件：`options.reasoning` 有值且 `model.reasoning == Some(true)`，否则直接
/// `None`。
///
/// Claude 分支：
///
/// - GovCloud 目标不发送 `display`，也不支持 block binding；
/// - 自适应思考：`thinking.type = "adaptive"`，`output_config.effort` 取
///   [`map_thinking_level_to_effort`] 的结果；支持 block binding 时再补
///   `thinking.block_binding.prefix_mismatch_behavior = "drop_block"` 与
///   `anthropic_beta: [THINKING_BINDING_CONTROLS_BETA]`；
/// - 非自适应：`thinking.type = "enabled"` + `budget_tokens`。查覆盖值/预算时把
///   xhigh 与 max 折叠成 high 档，但缺省预算仍按原始档位取（两者都是 16384）；
/// - 非自适应且 `interleaved_thinking` 未显式关闭时，`anthropic_beta` 设为
///   `["interleaved-thinking-2025-05-14"]`。这与 block binding 分支互斥，因为
///   `adaptive` 与 `!adaptive` 条件互补。
///
/// 非 Claude 分支：gpt-oss 发扁平 `reasoning_effort`（xhigh/max 钳到 high）；其它
/// `gpt-` 家族发 `reasoning.effort`，优先取 `model.thinking_level_map` 的映射。
/// 其余模型不发任何 reasoning 字段。
pub fn build_additional_model_request_fields(
    model: &Model,
    options: &BedrockOptions,
) -> Option<Value> {
    let reasoning = options.reasoning?;
    if model.reasoning != Some(true) {
        return None;
    }

    if is_anthropic_claude_model(model) {
        let is_gov_cloud = is_gov_cloud_bedrock_target(model, options);
        let display = if is_gov_cloud {
            None
        } else {
            Some(options.thinking_display.as_deref().unwrap_or("summarized"))
        };
        let use_block_binding = !is_gov_cloud && supports_thinking_block_binding(model);
        let adaptive = supports_adaptive_thinking(&model.id, Some(&model.name));

        let mut result = Map::new();
        // 自适应：思考长度由服务端决定，档位通过 output_config.effort 表达。
        if adaptive {
            let mut thinking = Map::new();
            thinking.insert("type".into(), Value::String("adaptive".into()));
            if let Some(display) = display {
                thinking.insert("display".into(), Value::String(display.into()));
            }
            if use_block_binding {
                thinking.insert(
                    "block_binding".into(),
                    crate::api::google_shared::object([(
                        "prefix_mismatch_behavior",
                        Value::String("drop_block".into()),
                    )]),
                );
            }
            result.insert("thinking".into(), Value::Object(thinking));
            result.insert(
                "output_config".into(),
                crate::api::google_shared::object([(
                    "effort",
                    Value::String(map_thinking_level_to_effort(model, Some(reasoning))),
                )]),
            );
            if use_block_binding {
                result.insert(
                    "anthropic_beta".into(),
                    Value::Array(vec![Value::String(THINKING_BINDING_CONTROLS_BETA.into())]),
                );
            }
        } else {
            // 非自适应：固定预算；xhigh/max 在查覆盖值与档位时折叠为 high。
            let default_budget = |level: ThinkingLevel| -> u64 {
                match level {
                    ThinkingLevel::Minimal => 1024,
                    ThinkingLevel::Low => 2048,
                    ThinkingLevel::Medium => 8192,
                    ThinkingLevel::High | ThinkingLevel::Xhigh | ThinkingLevel::Max => 16384,
                }
            };
            let level = match reasoning {
                ThinkingLevel::Xhigh | ThinkingLevel::Max => ThinkingLevel::High,
                other => other,
            };
            let budget = options
                .thinking_budgets
                .as_ref()
                .and_then(|budgets| match level {
                    ThinkingLevel::Minimal => budgets.minimal,
                    ThinkingLevel::Low => budgets.low,
                    ThinkingLevel::Medium => budgets.medium,
                    ThinkingLevel::High | ThinkingLevel::Xhigh | ThinkingLevel::Max => budgets.high,
                })
                .unwrap_or_else(|| default_budget(reasoning));
            let mut thinking = Map::new();
            thinking.insert("type".into(), Value::String("enabled".into()));
            thinking.insert("budget_tokens".into(), Value::from(budget));
            if let Some(display) = display {
                thinking.insert("display".into(), Value::String(display.into()));
            }
            result.insert("thinking".into(), Value::Object(thinking));
        }

        // 与上面的 block binding 分支互斥：自适应分支不会进入这里，后写会覆盖前者。
        if !adaptive && options.interleaved_thinking.unwrap_or(true) {
            result.insert(
                "anthropic_beta".into(),
                Value::Array(vec![Value::String(
                    "interleaved-thinking-2025-05-14".into(),
                )]),
            );
        }
        return Some(Value::Object(result));
    }

    let candidates = get_model_match_candidates(&model.id, Some(&model.name));
    // gpt-oss 用扁平字段 reasoning_effort；xhigh/max 会被钳到 high。
    if candidates
        .iter()
        .any(|candidate| candidate.contains("gpt-oss"))
    {
        let effort = match reasoning {
            ThinkingLevel::Minimal | ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            _ => "high",
        };
        return Some(crate::api::google_shared::object([(
            "reasoning_effort",
            Value::String(effort.into()),
        )]));
    }
    // 其它 GPT 家族用嵌套的 reasoning.effort，优先采用模型自己的档位映射。
    if candidates
        .iter()
        .any(|candidate| candidate.contains("gpt-"))
    {
        let mapped = model
            .thinking_level_map
            .as_ref()
            .and_then(|map| map.get(&thinking_to_model_level(reasoning)))
            .and_then(|value| value.as_deref());
        let effort = mapped.unwrap_or(match reasoning {
            ThinkingLevel::Minimal | ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
            ThinkingLevel::Xhigh => "xhigh",
            ThinkingLevel::Max => "max",
        });
        return Some(crate::api::google_shared::object([(
            "reasoning",
            crate::api::google_shared::object([("effort", Value::String(effort.into()))]),
        )]));
    }
    None
}

// ---------------------------------------------------------------------------
// 客户端配置
// ---------------------------------------------------------------------------

/// `BedrockCredentials`。
///
/// SigV4 所需的静态凭据。`session_token` 存在时，签名会额外带上
/// `x-amz-security-token` 头（临时凭据的形态）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BedrockCredentials {
    /// AWS access key id，进签名 `Credential=<id>/...`。
    pub access_key_id: String,
    /// 原始 secret，只用于派生签名密钥，不会出现在请求里。
    pub secret_access_key: String,
    /// 会话令牌；临时凭据必须随请求发送。
    pub session_token: Option<String>,
}

/// `bedrockUserAgent(identity)` 的可观察形态。
///
/// TS 返回 `[["kiana", version]]` 这样的数组；Rust 只保留三种最终结果，由
/// `build_http_request` 拼成 `user-agent` 头。
#[derive(Debug, Clone, Default, PartialEq)]
pub enum BedrockUserAgent {
    /// 详细模式：`[["kiana", version]]`（version 可能为空）。
    #[default]
    Detailed,
    /// `"short"`：`[["kiana"]]`。
    Short,
    /// `{ custom }`。
    Custom(String),
}

/// `BedrockRuntimeClient` 构造参数的等价物。
///
/// 由 [`resolve_client_config`] 纯解析得到，随后被 `build_http_request` 与凭据链消费。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BedrockClientConfig {
    /// 共享配置 profile 名；`None` 表示 default/环境凭据。
    pub profile: Option<String>,
    /// User-Agent 形态。
    pub custom_user_agent: BedrockUserAgent,
    /// 最终 region；`None` 表示留给 profile/隐式配置，`build_http_request` 仍解析不到
    /// 时会以 `"Region is missing"` 失败。
    pub region: Option<String>,
    /// 显式端点（只有端点策略要求时才设置）；`None` 表示按 region 拼标准端点。
    pub endpoint: Option<String>,
    /// 静态凭据；`None` 表示运行时走完整凭据链。
    pub credentials: Option<BedrockCredentials>,
    /// bearer token；`Some` 时不签名，直接发 `Authorization: Bearer <token>`。
    pub token: Option<String>,
    /// bearer 模式下固定为 `["httpBearerAuth"]`；保留该字段以对齐 SDK 配置形状。
    pub auth_scheme_preference: Option<Vec<String>>,
}

/// `bedrockUserAgent(identity)`。
///
/// 映射三种形态：`Custom` 原样使用，short 模式 → `"kiana"`，其余（含未设置）→
/// `"kiana/<version>"`。version 由 [`get_client_version`] 从 identity 取。
pub fn bedrock_user_agent(identity: Option<&ClientIdentity>) -> BedrockUserAgent {
    let mode = identity.and_then(|identity| identity.user_agent.as_ref());
    match mode {
        Some(UserAgentMode::Custom { custom }) => BedrockUserAgent::Custom(custom.clone()),
        Some(mode) if mode.is_short() => BedrockUserAgent::Short,
        _ => BedrockUserAgent::Detailed,
    }
}

/// `user agent` 的详细版本（`getClientVersion(identity)`）。
fn bedrock_user_agent_version(identity: Option<&ClientIdentity>) -> String {
    get_client_version(identity)
}

/// `resolveClientConfig(model, options)`：构造 `BedrockRuntimeClient` 的配置。
///
/// 纯解析函数（不做 IO），把选项、scoped env、进程/沙箱 env 与 model 元数据合并成
/// [`BedrockClientConfig`]。优先级：
///
/// 1. profile：`options.profile` → scoped env 的 `AWS_PROFILE` → 进程/沙箱 env 的
///    `AWS_PROFILE`。前两步同时记作 `options_profile`，用于压制 env 里的静态 AK/SK
///    （显式 profile 优先于环境密钥）。
/// 2. region：ARN 内嵌 > 显式配置（option / `AWS_REGION` / `AWS_DEFAULT_REGION`）>
///    标准端点的 region（仅当端点被显式钉住）> `us-east-1`。若存在 ambient
///    `AWS_PROFILE`（这里只查进程/沙箱 env，不查 scoped env），则留 `None`，交给
///    profile 或 SDK 默认链决定。
/// 3. endpoint：仅当 [`should_use_explicit_bedrock_endpoint`] 为真时才用
///    `model.base_url`；否则按 region 拼标准端点，避免模型目录里的 `us-east-1`
///    覆盖调用方的 `AWS_REGION`。
/// 4. 鉴权：`AWS_BEDROCK_SKIP_AUTH=1` 时用 dummy 静态凭据（请求仍会用假密钥签名，
///    供本地/测试端点使用，不走真实凭据链）；否则 bearer token（`options.bearer_token`
///    → `base.api_key` → `AWS_BEARER_TOKEN_BEDROCK`）存在时用 Bearer 并把
///    `auth_scheme_preference` 固定为 `["httpBearerAuth"]`；再否则，没有显式 profile
///    时把 env 的 AK/SK/会话令牌当静态凭据，有显式 profile 时交给运行时凭据链。
pub fn resolve_client_config(model: &Model, options: &BedrockOptions) -> BedrockClientConfig {
    // profile：显式 option > scoped env（这两步构成 options_profile，用于压制 env 的
    // AK/SK）> 进程/沙箱 env。
    let options_profile = options
        .profile
        .clone()
        .or_else(|| {
            options
                .base
                .env
                .as_ref()
                .and_then(|env| env.get("AWS_PROFILE").cloned())
        })
        .filter(|profile| !profile.is_empty());
    let profile = options_profile
        .clone()
        .or_else(|| get_provider_env_value("AWS_PROFILE", options.base.env.as_ref()));
    let configured_region = get_configured_bedrock_region(options);
    let has_ambient_configured_profile = get_provider_env_value("AWS_PROFILE", None).is_some();
    let endpoint_region = get_standard_bedrock_endpoint_region(&model.base_url);
    let use_explicit_endpoint = should_use_explicit_bedrock_endpoint(
        &model.base_url,
        configured_region.as_deref(),
        has_ambient_configured_profile,
    );

    let skip_auth = get_provider_env_value("AWS_BEDROCK_SKIP_AUTH", options.base.env.as_ref())
        .as_deref()
        == Some("1");
    // bearer token 优先级：显式 -> 通用 apiKey -> 环境变量；空串视为未设置。
    let non_empty = |value: Option<String>| value.filter(|token| !token.is_empty());
    let bearer_token = non_empty(options.bearer_token.clone())
        .or_else(|| non_empty(options.base.api_key.clone()))
        .or_else(|| {
            non_empty(get_provider_env_value(
                "AWS_BEARER_TOKEN_BEDROCK",
                options.base.env.as_ref(),
            ))
        });
    let use_bearer_token = bearer_token.is_some() && !skip_auth;

    // Region 解析：ARN 内嵌 > 显式 option > env > endpoint（仅显式端点）> us-east-1。
    // 存在 ambient AWS_PROFILE 时保持 None，让共享配置里的 region 生效。
    let region = match arn_region(&model.id) {
        Some(region) => Some(region),
        None => match configured_region {
            Some(region) => Some(region),
            None => match (endpoint_region.clone(), use_explicit_endpoint) {
                (Some(region), true) => Some(region),
                _ => {
                    if !has_ambient_configured_profile {
                        Some("us-east-1".to_string())
                    } else {
                        None
                    }
                }
            },
        },
    };

    // skipAuth：用 dummy 密钥满足签名代码路径，但不触碰真实凭据链。
    // 否则只有“没有显式 profile”时才把 env 的 AK/SK 当静态凭据（profile 优先）。
    let credentials = if skip_auth {
        Some(BedrockCredentials {
            access_key_id: "dummy-access-key".into(),
            secret_access_key: "dummy-secret-key".into(),
            session_token: None,
        })
    } else {
        let env_credentials = get_configured_bedrock_credentials(options.base.env.as_ref());
        if env_credentials.is_some() && options_profile.is_none() {
            env_credentials
        } else {
            None
        }
    };

    BedrockClientConfig {
        profile,
        custom_user_agent: bedrock_user_agent(options.base.identity.as_ref()),
        region,
        endpoint: if use_explicit_endpoint {
            Some(model.base_url.clone())
        } else {
            None
        },
        credentials,
        token: if use_bearer_token { bearer_token } else { None },
        auth_scheme_preference: if use_bearer_token {
            Some(vec!["httpBearerAuth".into()])
        } else {
            None
        },
    }
}

/// `getConfiguredBedrockRegion(options)`。
///
/// 只读显式配置，不解析共享配置文件：`options.region`（非空）→ `AWS_REGION` →
/// `AWS_DEFAULT_REGION`。空串按“未设置”处理（scoped env 会覆盖进程环境）。
pub fn get_configured_bedrock_region(options: &BedrockOptions) -> Option<String> {
    options
        .region
        .clone()
        .filter(|region| !region.is_empty())
        .or_else(|| get_provider_env_value("AWS_REGION", options.base.env.as_ref()))
        .or_else(|| get_provider_env_value("AWS_DEFAULT_REGION", options.base.env.as_ref()))
}

/// `getConfiguredBedrockCredentials(env)`。
///
/// 从 env 直接取静态凭据：`AWS_ACCESS_KEY_ID` 与 `AWS_SECRET_ACCESS_KEY` 必须**同时**
/// 存在，缺任一返回 `None`；`AWS_SESSION_TOKEN` 可选。不读共享配置文件。
pub fn get_configured_bedrock_credentials(env: Option<&ProviderEnv>) -> Option<BedrockCredentials> {
    let access_key_id = get_provider_env_value("AWS_ACCESS_KEY_ID", env)?;
    let secret_access_key = get_provider_env_value("AWS_SECRET_ACCESS_KEY", env)?;
    Some(BedrockCredentials {
        access_key_id,
        secret_access_key,
        session_token: get_provider_env_value("AWS_SESSION_TOKEN", env),
    })
}

/// `arn:aws(-[a-z0-9-]+)?:bedrock:([a-z0-9-]+):` 的区域。
///
/// 手写解析而非正则：跳过 `arn:aws` 后可选的分区后缀（如 `-us-gov`），要求紧跟
/// `:bedrock:`，再取下一个 `:` 前的段；只有非空且全为小写字母/数字/`-` 才接受。
/// 这是跨分区 ARN（`aws`、`aws-us-gov`、`aws-cn`）能正确取 region 的原因。
fn arn_region(model_id: &str) -> Option<String> {
    let rest = model_id.strip_prefix("arn:aws")?;
    let rest = if let Some(rest) = rest.strip_prefix('-') {
        let end = rest.find(':')?;
        &rest[end..]
    } else {
        rest
    };
    let rest = rest.strip_prefix(":bedrock:")?;
    let end = rest.find(':')?;
    let region = &rest[..end];
    if !region.is_empty()
        && region
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        Some(region.to_string())
    } else {
        None
    }
}

/// `getStandardBedrockEndpointRegion(baseUrl)`。
///
/// 只识别标准 AWS 端点的主机名：
/// `bedrock-runtime[-fips].<region>.amazonaws.com[.cn]`（大小写不敏感），返回其中的
/// region。自定义端点、缺主机名或非法 URL 都返回 `None`——调用方据此判断是否需要
/// 显式使用这个端点。
pub fn get_standard_bedrock_endpoint_region(base_url: &str) -> Option<String> {
    if base_url.is_empty() {
        return None;
    }
    let url = url::Url::parse(base_url).ok()?;
    let hostname = url.host_str()?.to_lowercase();
    static PATTERN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^bedrock-runtime(?:-fips)?\.([a-z0-9-]+)\.amazonaws\.com(?:\.cn)?$")
            .expect("valid regex")
    });
    PATTERN
        .captures(&hostname)
        .and_then(|captures| captures.get(1))
        .map(|region| region.as_str().to_string())
}

/// `shouldUseExplicitBedrockEndpoint(baseUrl, configuredRegion, hasAmbientConfiguredProfile)`。
///
/// 自定义端点（非标准 AWS 域名）必须显式使用；标准端点只有在“没有显式 region 且没有
/// ambient profile”时才钉住。这样调用方设置 `AWS_REGION` 或 `AWS_PROFILE` 后，端点
/// 由 region 重新拼出，而不是被模型目录里的 `us-east-1` 端点覆盖。
pub fn should_use_explicit_bedrock_endpoint(
    base_url: &str,
    configured_region: Option<&str>,
    has_ambient_configured_profile: bool,
) -> bool {
    if get_standard_bedrock_endpoint_region(base_url).is_none() {
        return true;
    }
    configured_region.is_none() && !has_ambient_configured_profile
}

/// `buildCommandInput(model, context, cacheRetention, options)`。
///
/// 组装 ConverseStream 的完整命令输入（此时还是 JSON，尚未变成 HTTP 请求）：
///
/// - `modelId`：原样用 `model.id`；真正发送前由 `build_http_request` 从 body 删除并
///   编进 URL（SDK 的 `httpLabel` 语义），但 `on_payload` 可以在那之前改写它。
/// - `messages` / `system`：分别来自 [`convert_messages`] 与 [`build_system_prompt`]；
///   system 只取转录里的**首条** system 消息，中途 system 消息已由
///   `collapse_system_messages` 折叠。
/// - `inferenceConfig`：总是存在（即使为空对象）。`maxTokens` 优先用调用方设置，
///   未设置时只有 Claude 回落到 `model.max_tokens`；`temperature` 若无法表示为 JSON
///   数字（NaN/Inf）则写成 `null`，与 JS 的 `JSON.stringify` 一致。
/// - `toolConfig`：工具取当前轮次的工具（`get_current_tools`），转换见
///   [`convert_tool_config`]。
/// - `additionalModelRequestFields` / `requestMetadata`：按需附加。
///
/// 错误：来自消息/工具转换（未知图片 MIME、非法 base64、JSON schema 生成失败）。
pub fn build_command_input(
    model: &Model,
    context: &TranscriptContext,
    cache_retention: CacheRetention,
    options: &BedrockOptions,
    supports_strict_mode: bool,
) -> Result<Value, AiError> {
    // maxTokens 回落到模型上限只适用于 Claude；其它模型缺省时交给服务端默认值。
    let inference_max_tokens = options.base.max_tokens.or_else(|| {
        if is_anthropic_claude_model(model) {
            model.max_tokens
        } else {
            None
        }
    });
    let initial_system_message = get_initial_system_message(&context.0.messages);
    let initial_system_prompt = initial_system_message.map(get_system_message_text);

    let mut root = Map::new();
    root.insert("modelId".into(), Value::String(model.id.clone()));
    root.insert(
        "messages".into(),
        Value::Array(convert_messages(
            context,
            model,
            cache_retention,
            options.base.env.as_ref(),
        )?),
    );
    if let Some(system) = build_system_prompt(
        initial_system_prompt.as_deref(),
        model,
        cache_retention,
        options.base.env.as_ref(),
    ) {
        root.insert("system".into(), Value::Array(system));
    }
    let mut inference_config = Map::new();
    if let Some(max_tokens) = inference_max_tokens {
        inference_config.insert("maxTokens".into(), Value::from(max_tokens));
    }
    if let Some(temperature) = options.base.temperature {
        // NaN/Infinity 无法表示成 JSON 数字，写成 null（JS JSON.stringify 的行为）。
        inference_config.insert(
            "temperature".into(),
            serde_json::Number::from_f64(temperature)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        );
    }
    root.insert("inferenceConfig".into(), Value::Object(inference_config));
    // 工具定义随当前轮次变化：历史消息里可能有已经不再提供的工具。
    let current_tools = get_current_tools(&context.0.messages);
    if let Some(tool_config) = convert_tool_config(
        Some(&current_tools),
        options.tool_choice.as_ref(),
        supports_strict_mode,
    )? {
        root.insert("toolConfig".into(), tool_config);
    }
    if let Some(fields) = build_additional_model_request_fields(model, options) {
        root.insert("additionalModelRequestFields".into(), fields);
    }
    if let Some(metadata) = &options.request_metadata {
        root.insert(
            "requestMetadata".into(),
            Value::Object(
                metadata
                    .iter()
                    .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                    .collect(),
            ),
        );
    }
    Ok(Value::Object(root))
}

// ---------------------------------------------------------------------------
// SigV4 / 凭据
// ---------------------------------------------------------------------------

/// SigV4 签名输入。
///
/// 包含 canonical request 需要的全部信息。`path` 与 `query` 必须已经按 SigV4 规则
/// 编码（本文件的调用方用 `AWS_URI_ENCODE_SET` 处理 modelId 与表单参数）。
pub struct SigV4Request<'a> {
    /// HTTP 方法（大写，如 `"POST"`）。
    pub method: &'a str,
    /// 已百分号编码的路径（以 `/` 开头）。
    pub path: &'a str,
    /// 已排序的 query（`a=b&c=d`），可为空。
    pub query: &'a str,
    /// 待签名的头；名字大小写不敏感（内部统一转小写），应包含真实 `host`。
    pub headers: &'a [(String, String)],
    /// 原始请求体字节，用于 payload 的 SHA-256。
    pub body: &'a [u8],
    /// 签名 scope 里的 region（如 `us-east-1`）。
    pub region: &'a str,
    /// 签名 scope 里的服务名：Bedrock 用 `"bedrock"`，STS 用 `"sts"`。
    pub service: &'a str,
    /// AWS access key id。
    pub access_key_id: &'a str,
    /// AWS secret access key（只参与派生签名密钥）。
    pub secret_access_key: &'a str,
    /// 临时凭据的会话令牌；有值时附加 `x-amz-security-token` 并纳入签名。
    pub session_token: Option<&'a str>,
    /// `YYYYMMDDTHHMMSSZ`。
    pub amz_date: &'a str,
}

fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::{KeyInit as _, Mac as _};
    let mut mac =
        hmac::Hmac::<sha2::Sha256>::new_from_slice(key).expect("hmac accepts any key size");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// 计算 SigV4 `Authorization` 头的值（同时返回需要加入请求的 `x-amz-date`/安全令牌头）。
///
/// 返回 `(authorization, headers_to_send)`。`headers_to_send` 是调用方应实际发送的
/// 完整头：名字已转小写、补上 `x-amz-date`（有会话令牌时还有
/// `x-amz-security-token`）、同名头保序去重（保留最后一次设置）。`authorization`
/// 不在其中，需要调用方自己追加到请求头。
///
/// 步骤（对应 AWS SigV4 规范）：
///
/// 1. 规范化头：按名字排序，排除不参与签名的头（`authorization`、`connection`、
///    `expect`、`user-agent`、`x-amzn-trace-id`、`transfer-encoding`、
///    `content-length`），头值去首尾空白并把连续空白折叠为单个空格。
/// 2. 拼 canonical request：method/path/query/canonical headers/signed headers/
///    payload 的 SHA-256。
/// 3. 拼 string-to-sign：算法名、`amz_date`、credential scope、canonical request 的
///    SHA-256。
/// 4. 以 `AWS4<secret>` 为初始 key，依次对 date/region/service/`aws4_request` 求
///    HMAC-SHA256，再对 string-to-sign 求最终签名。
///
/// 约束：`amz_date` 必须是 `YYYYMMDDTHHMMSSZ`（UTC），函数只取前 8 位作为日期戳；
/// 调用方应提供真实 `host` 头（含端口）。缺失时这里会补一个空 host 参与签名，服务端
/// 校验通常不会通过。
pub fn sign_sigv4(request: &SigV4Request<'_>) -> (String, Vec<(String, String)>) {
    let mut headers: Vec<(String, String)> = request
        .headers
        .iter()
        .map(|(name, value)| (name.to_lowercase(), value.clone()))
        .collect();
    headers.push(("x-amz-date".into(), request.amz_date.to_string()));
    if let Some(token) = request.session_token {
        headers.push(("x-amz-security-token".into(), token.to_string()));
    }
    // host 缺失时补空串占位：直接调用本函数的测试/嵌入方必须自己提供真实 host。
    if host_of_headers(&headers).is_none() {
        headers.push(("host".into(), String::new()));
    }

    // 签名头：按名字排序；排除不参与签名的头。
    let excluded = [
        "authorization",
        "connection",
        "expect",
        "user-agent",
        "x-amzn-trace-id",
        "transfer-encoding",
        "content-length",
    ];
    let mut signed: Vec<(String, String)> = headers
        .iter()
        .filter(|(name, _)| !excluded.contains(&name.as_str()))
        .cloned()
        .collect();
    signed.sort_by(|a, b| a.0.cmp(&b.0));
    signed.dedup_by(|a, b| a.0 == b.0);
    for (_, value) in signed.iter_mut() {
        *value = normalize_sigv4_header_value(value);
    }

    let canonical_headers: String = signed
        .iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect();
    let signed_headers: String = signed
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let payload_hash = hex_sha256(request.body);
    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        request.method,
        request.path,
        request.query,
        canonical_headers,
        signed_headers,
        payload_hash
    );
    let canonical_request_hash = hex_sha256(canonical_request.as_bytes());

    let date_stamp = &request.amz_date[..8.min(request.amz_date.len())];
    let credential_scope = format!(
        "{date_stamp}/{}/{}/aws4_request",
        request.region, request.service
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        request.amz_date, credential_scope, canonical_request_hash
    );

    // AWS4 派生密钥：secret -> date -> region -> service -> aws4_request，逐层 HMAC。
    let mut key = format!("AWS4{}", request.secret_access_key).into_bytes();
    key = hmac_sha256(&key, date_stamp.as_bytes());
    key = hmac_sha256(&key, request.region.as_bytes());
    key = hmac_sha256(&key, request.service.as_bytes());
    key = hmac_sha256(&key, b"aws4_request");
    let signature = hex::encode(hmac_sha256(&key, string_to_sign.as_bytes()));

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
        request.access_key_id, credential_scope, signed_headers, signature
    );

    // 发送头与签名头分开：host 可能由 HTTP 客户端改写，authorization 由调用方追加。
    let mut send_headers: Vec<(String, String)> = headers
        .into_iter()
        .filter(|(name, _)| {
            name != "host"
                || !request
                    .headers
                    .iter()
                    .any(|(n, _)| n.eq_ignore_ascii_case("host"))
        })
        .collect();
    // 保序去重（保留最后一次设置）。
    let mut deduped: Vec<(String, String)> = Vec::new();
    for (name, value) in send_headers.drain(..) {
        deduped.retain(|(existing, _)| existing != &name);
        deduped.push((name, value));
    }
    (authorization, deduped)
}

fn host_of_headers(headers: &[(String, String)]) -> Option<String> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("host"))
        .map(|(_, value)| value.clone())
}

/// SigV4 规范化头值：去首尾空白、连续空白折叠为单个空格。
fn normalize_sigv4_header_value(value: &str) -> String {
    let mut result = String::new();
    let mut in_space = false;
    for ch in value.trim().chars() {
        if ch == ' ' || ch == '\t' {
            if !in_space {
                result.push(' ');
                in_space = true;
            }
        } else {
            result.push(ch);
            in_space = false;
        }
    }
    result
}

/// `YYYYMMDDTHHMMSSZ`（UTC）。
///
/// 用系统时钟生成 SigV4 的 `x-amz-date`。每次重试都会重新生成（见
/// `build_http_request`），避免沿用已经过期的时间戳。
pub fn amz_date_now() -> String {
    use chrono::Utc;
    Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// Bedrock `$metadata` 的可观察字段。
///
/// 两个字段都对应 SDK 错误对象上的 `$metadata`；`http_status_code` 用 `f64` 是为了
/// 保留 JS number 的语义（诊断与比较都按 number 处理）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BedrockErrorMetadata {
    /// HTTP 状态码；流内解析出的服务异常没有真实响应，保持 `None`。
    pub http_status_code: Option<f64>,
    /// 服务端请求 id，来自 `x-amzn-requestid` 响应头（或非 2xx 的响应头）。
    pub request_id: Option<String>,
}

/// 流内/请求阶段的失败形态。
///
/// 对应 TS 里可能被 `throw` 的几类值：SDK 的 `BedrockRuntimeServiceException`、
/// 普通 `Error`（含 `SyntaxError`）、流内直接抛出的裸对象，以及中止。保留下这些
/// 形状才能复刻错误文本、诊断字段与可重试判定；转成统一的 `AiError` 无法直接保留
/// `$metadata` 与这些分类信息（重试依赖 name/status）。
#[derive(Debug, Clone, PartialEq)]
pub enum BedrockFailure {
    /// `BedrockRuntimeServiceException`（HTTP 非 2xx 或 modeled 服务错误）。
    Service {
        /// modeled 异常名（PascalCase，如 `ThrottlingException`）。
        name: String,
        /// 异常对象上的 message。
        message: String,
        /// HTTP 状态码与 requestId；流内异常没有真实响应时是默认值。
        metadata: BedrockErrorMetadata,
        /// `$response.body` 的字符串形态（SDK 未把它折进 message 时用于展示）。
        body: Option<String>,
    },
    /// 真实的 `Error`（非 ServiceException），`name` 可能是 `:error-code`。
    Named {
        /// 普通 Error 的 name；流内 `error` 帧取 `:error-code`，JSON 解析失败是
        /// `SyntaxError`。
        name: String,
        /// 错误文本。
        message: String,
        /// 普通 Error 通常没有；反序列化失败路径会补 `status = 200` 与 requestId。
        metadata: Option<BedrockErrorMetadata>,
        /// 响应正文（非 2xx 的 REST JSON 错误体里取出）。
        body: Option<String>,
    },
    /// 流内裸对象（modeled mid-stream exception）。
    ///
    /// TS 的 `throw item.throttlingException` 抛的是普通对象；`message` 是
    /// `JSON.stringify(value)` 的结果。没有 name/status，因此不参与重试分类，
    /// 也不会产生 `errorCode` 诊断。
    Value { message: String },
    /// 中止。
    Aborted(String),
}

impl BedrockFailure {
    /// 构造普通 `Error`（没有 `$metadata`，不会带 body/status）。
    pub fn named(name: impl Into<String>, message: impl Into<String>) -> Self {
        BedrockFailure::Named {
            name: name.into(),
            message: message.into(),
            metadata: None,
            body: None,
        }
    }

    /// 返回底层的 message 文本。
    ///
    /// `Value` 变体返回 stringify 后的 JSON。重试分类里的 `"Signature expired"`
    /// 判断与最终展示都依赖它。
    pub fn message(&self) -> String {
        match self {
            BedrockFailure::Service { message, .. }
            | BedrockFailure::Named { message, .. }
            | BedrockFailure::Value { message }
            | BedrockFailure::Aborted(message) => message.clone(),
        }
    }

    // 只有 Service/Named 可能带元数据；Value/Aborted 没有。
    fn metadata(&self) -> Option<&BedrockErrorMetadata> {
        match self {
            BedrockFailure::Service { metadata, .. } => Some(metadata),
            BedrockFailure::Named { metadata, .. } => metadata.as_ref(),
            _ => None,
        }
    }

    // 只有 Service/Named 有 error name；重试分类与 errorCode 诊断依赖它。
    fn name(&self) -> Option<&str> {
        match self {
            BedrockFailure::Service { name, .. } | BedrockFailure::Named { name, .. } => Some(name),
            _ => None,
        }
    }

    // 服务异常决定 formatBedrockError 是否追加 "<友好前缀>: "。
    fn is_service_exception(&self) -> bool {
        matches!(self, BedrockFailure::Service { .. })
    }

    // 中止不参与重试，也不追加诊断。
    fn is_aborted(&self) -> bool {
        matches!(self, BedrockFailure::Aborted(_))
    }
}

/// `normalizeDiagnosticValue(value)`。
///
/// 诊断字段的清洗：`None`、空串或纯空白 → `None`；按 JS `trim` 去空白；UTF-16 码元数
/// 超过 `MAX_BEDROCK_DIAGNOSTIC_VALUE_CHARS` → `None`。用码元而不是字符数，是为了与
/// JS `string.length` 一致（非 BMP 字符算 2）。
pub fn normalize_diagnostic_value(value: Option<&str>) -> Option<String> {
    let value = value?;
    let trimmed = kiana_json::js_string::js_trim(value);
    if trimmed.is_empty() || trimmed.encode_utf16().count() > MAX_BEDROCK_DIAGNOSTIC_VALUE_CHARS {
        return None;
    }
    Some(trimmed.to_string())
}

/// `extractBedrockErrorCode(error)`。
///
/// 只有 name 以 `Exception` 结尾的失败才算“错误码”（SDK 的 modeled 异常）；传输类
/// 名字（`TimeoutError`、`Error`、`SyntaxError`）不算，避免诊断里出现误导性的
/// errorCode。结果再经 [`normalize_diagnostic_value`] 清洗。
fn extract_bedrock_error_code(failure: &BedrockFailure) -> Option<String> {
    let name = failure.name()?;
    if !name.ends_with("Exception") {
        return None;
    }
    normalize_diagnostic_value(Some(name))
}

/// `formatBedrockError(error)`。
///
/// 生成写进 `AssistantMessage::error_message` 的展示文本：
///
/// 1. 把失败形态转成 `ProviderErrorFields`（Service 带 status/body，Named 带可能的
///    metadata/body），再交给 `normalize_provider_error` 得到统一的 message/status/body。
/// 2. 若 message 本身不含 body、而 status 与 body 都有值，则拼成
///    `"<status>: <body>"`（status 按 JS 数字转字符串，`400.0` 输出 `400`）。
/// 3. 文本含 “data retention mode”（大小写不敏感）时追加官方文档链接。
/// 4. Service 异常加友好前缀（见 `bedrock_error_prefix`）；普通 `Error`（包括
///    `SyntaxError`）不加前缀。
///
/// 这段文本同时参与上层的可重试判定，不要随意改动格式。
pub fn format_bedrock_error(failure: &BedrockFailure) -> String {
    let fields = match failure {
        BedrockFailure::Service {
            message,
            metadata,
            body,
            ..
        } => ProviderErrorFields {
            message: message.clone(),
            metadata_status_code: metadata.http_status_code,
            response_body: body
                .clone()
                .map(ResponseBody::Text)
                .or(Some(ResponseBody::Stream)),
            ..Default::default()
        },
        BedrockFailure::Named {
            message,
            metadata,
            body,
            ..
        } => ProviderErrorFields {
            message: message.clone(),
            metadata_status_code: metadata.as_ref().and_then(|m| m.http_status_code),
            response_body: body.clone().map(ResponseBody::Text),
            ..Default::default()
        },
        other => ProviderErrorFields {
            message: other.message(),
            ..Default::default()
        },
    };
    let norm = normalize_provider_error(&ProviderErrorValue::Error(fields));
    let core = match (norm.message_carries_body, norm.status, norm.body.as_deref()) {
        (false, Some(status), Some(body)) => {
            format!(
                "{}: {}",
                kiana_json::js_num::to_radix_string(status, 10),
                body
            )
        }
        _ => norm.message.clone(),
    };
    let data_retention_hint = if core.to_lowercase().contains("data retention mode") {
        format!(" See {BEDROCK_DATA_RETENTION_DOCS_URL} for supported data retention modes.")
    } else {
        String::new()
    };
    if failure.is_service_exception() {
        let name = failure.name().unwrap_or("Unknown");
        let prefix = bedrock_error_prefix(name).unwrap_or(name);
        format!("{prefix}: {core}{data_retention_hint}")
    } else {
        format!("{core}{data_retention_hint}")
    }
}

/// `appendBedrockFailureDiagnostic(output, error, fallbackRequestId)`。
///
/// 往消息诊断里追加一条 `kind = "bedrock_response_failure"` 的记录，details 只包含
/// 可得的字段：
///
/// - `status`：HTTP 状态码；
/// - `errorCode`：name 以 `Exception` 结尾时的 name；
/// - `requestId`：优先错误自身的 `$metadata.requestId`，其次调用方给的
///   `fallback_request_id`（响应头里的 `x-amzn-requestid`）。
///
/// 三个字段都拿不到时直接返回，不产生空诊断。只在最终 `stop_reason == error` 时调用
/// （aborted 不追加），见 `finish_stream_error`。
pub fn append_bedrock_failure_diagnostic(
    output: &mut AssistantMessage,
    failure: &BedrockFailure,
    fallback_request_id: Option<&str>,
) {
    let mut details = JsonObject::new();
    if let Some(metadata) = failure.metadata()
        && let Some(status) = metadata.http_status_code
    {
        details.insert("status".into(), status_json(status));
    }
    if let Some(error_code) = extract_bedrock_error_code(failure) {
        details.insert("errorCode".into(), Value::String(error_code));
    }
    let request_id = failure
        .metadata()
        .and_then(|metadata| normalize_diagnostic_value(metadata.request_id.as_deref()))
        .or_else(|| fallback_request_id.map(str::to_string));
    if let Some(request_id) = request_id {
        details.insert("requestId".into(), Value::String(request_id));
    }
    if details.is_empty() {
        return;
    }
    append_assistant_message_diagnostic(
        output,
        AssistantMessageDiagnostic {
            kind: "bedrock_response_failure".into(),
            timestamp: now_unix_ms(),
            error: None,
            details: Some(details),
        },
    );
}

// ---------------------------------------------------------------------------
// AWS event-stream 解码
// ---------------------------------------------------------------------------

/// 一条解码后的 event-stream 消息。
///
/// `headers` 保存协议头（名字如 `:message-type`、`:event-type`，值一律转成字符串）；
/// `payload` 是未解析的原始字节，正常事件里通常是 JSON 文本。
#[derive(Debug, Clone, PartialEq)]
pub struct EventStreamMessage {
    /// 协议头；同名头后出现者覆盖先出现者（`IndexMap::insert` 语义）。
    pub headers: IndexMap<String, String>,
    /// 原始 payload 字节。
    pub payload: Vec<u8>,
}

impl EventStreamMessage {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// `application/vnd.amazon.eventstream` 增量解码器。
///
/// 调用方可以按任意边界切分字节块反复 [`EventStreamDecoder::push`]；不足一条消息的
/// 尾部字节留在 `buffer` 里等下一次输入。任何协议错误（长度越界、CRC 不匹配、头类型
/// 未知）都会返回 `Err`，调用方应终止整个流——解码器没有重新同步机制。
#[derive(Default)]
pub struct EventStreamDecoder {
    buffer: Vec<u8>,
}

/// prelude 长度：total_length(4) + headers_length(4) + prelude_crc(4)。
const PRELUDE_LEN: usize = 12;
/// 尾部 CRC 长度。
const MESSAGE_CRC_LEN: usize = 4;
/// `MAX_BEDROCK_EVENTSTREAM_MESSAGE_LEN`：单条消息上限（16 MiB）。
const MAX_MESSAGE_LEN: usize = 16 * 1024 * 1024;

impl EventStreamDecoder {
    /// 新建空解码器（缓冲区为空）。
    pub fn new() -> Self {
        EventStreamDecoder::default()
    }

    /// 追加字节并返回所有完整消息；协议错误返回 `Err`。
    ///
    /// 线格式（所有整数为大端）：
    ///
    /// ```text
    /// [total_length u32][headers_length u32][prelude_crc u32]
    /// [headers ...][payload ...][message_crc u32]
    /// ```
    ///
    /// `total_length` 包含 12 字节 prelude 与尾部 4 字节 CRC。`prelude_crc` 覆盖前 8
    /// 字节；`message_crc` 覆盖除最后 4 字节外的整条消息。长度检查先于取字节，确保
    /// 下面的切片不会越界。
    ///
    /// 返回的消息按到达顺序排列；一次 `push` 可能产出 0 条或多条。错误只表示协议
    /// 损坏，不表示服务端业务错误。
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<EventStreamMessage>, String> {
        self.buffer.extend_from_slice(bytes);
        let mut messages = Vec::new();
        loop {
            if self.buffer.len() < PRELUDE_LEN {
                break;
            }
            let total_length =
                u32::from_be_bytes(self.buffer[0..4].try_into().expect("4 bytes")) as usize;
            let headers_length =
                u32::from_be_bytes(self.buffer[4..8].try_into().expect("4 bytes")) as usize;
            // 先校验长度再取整帧：total 至少是 prelude+CRC，headers 不能越过 payload 起点。
            if !(PRELUDE_LEN + MESSAGE_CRC_LEN..=MAX_MESSAGE_LEN).contains(&total_length)
                || headers_length > total_length - PRELUDE_LEN - MESSAGE_CRC_LEN
            {
                return Err("Invalid event stream message length".into());
            }
            if self.buffer.len() < total_length {
                break;
            }
            let message: Vec<u8> = self.buffer.drain(..total_length).collect();
            // prelude CRC 覆盖 total_length/headers_length 这 8 字节。
            let prelude_crc = u32::from_be_bytes(message[8..12].try_into().expect("4 bytes"));
            if crc32(&message[..8]) != prelude_crc {
                return Err("Invalid event stream prelude checksum".into());
            }
            // 消息 CRC 覆盖除末尾 4 字节外的全部内容（含 prelude 与 payload）。
            let message_crc = u32::from_be_bytes(
                message[total_length - MESSAGE_CRC_LEN..]
                    .try_into()
                    .expect("4 bytes"),
            );
            if crc32(&message[..total_length - MESSAGE_CRC_LEN]) != message_crc {
                return Err("Invalid event stream message checksum".into());
            }
            let headers_bytes = &message[PRELUDE_LEN..PRELUDE_LEN + headers_length];
            let headers = decode_eventstream_headers(headers_bytes)?;
            let payload =
                message[PRELUDE_LEN + headers_length..total_length - MESSAGE_CRC_LEN].to_vec();
            messages.push(EventStreamMessage { headers, payload });
        }
        Ok(messages)
    }
}

/// CRC32（IEEE，与 `crc32fast` 相同）。
fn crc32(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}

/// 解析 event-stream 的头部区。
///
/// 每个头是 `[name_len u8][name][value_type u8][value]`。值类型按 Smithy 规范：
///
/// ```text
/// 0/1  bool true/false   2  byte      3  i16      4  i32
/// 5    i64               6  byte[]    7  string   8  timestamp(i64)
/// 9    uuid(16 字节)
/// ```
///
/// 本模块把所有值统一成字符串（`headers` 只用于读 `:message-type` 等文本头）：
/// byte[] 转 base64，uuid 转 32 个十六进制字符，数值按各分支的 `to_string()` 输出。
/// Bedrock 的协议头实际只用到 string 类型。名字与 string 用 `from_utf8_lossy`，
/// 损坏的 UTF-8 不会让整个解码失败；但任何长度截断都会返回 `Err`。
fn decode_eventstream_headers(bytes: &[u8]) -> Result<IndexMap<String, String>, String> {
    let mut headers = IndexMap::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        let name_len = bytes[offset] as usize;
        offset += 1;
        if offset + name_len > bytes.len() {
            return Err("Invalid event stream header name".into());
        }
        let name = String::from_utf8_lossy(&bytes[offset..offset + name_len]).into_owned();
        offset += name_len;
        if offset >= bytes.len() {
            return Err("Invalid event stream header type".into());
        }
        // 逐类型检查剩余长度后再读，避免越界；类型编号见上方说明。
        let value_type = bytes[offset];
        offset += 1;
        let value = match value_type {
            0 => {
                // bool true
                "true".to_string()
            }
            1 => {
                // bool false
                "false".to_string()
            }
            2 => {
                if offset >= bytes.len() {
                    return Err("Invalid event stream header".into());
                }
                let byte = bytes[offset];
                offset += 1;
                byte.to_string()
            }
            3 => {
                if offset + 2 > bytes.len() {
                    return Err("Invalid event stream header".into());
                }
                let value = i16::from_be_bytes([bytes[offset], bytes[offset + 1]]);
                offset += 2;
                value.to_string()
            }
            4 => {
                if offset + 4 > bytes.len() {
                    return Err("Invalid event stream header".into());
                }
                let value =
                    i32::from_be_bytes(bytes[offset..offset + 4].try_into().expect("4 bytes"));
                offset += 4;
                value.to_string()
            }
            5 | 8 => {
                if offset + 8 > bytes.len() {
                    return Err("Invalid event stream header".into());
                }
                let raw =
                    u64::from_be_bytes(bytes[offset..offset + 8].try_into().expect("8 bytes"));
                offset += 8;
                if value_type == 5 {
                    raw.to_string()
                } else {
                    f64::from_bits(raw).to_string()
                }
            }
            6 | 7 => {
                if offset + 2 > bytes.len() {
                    return Err("Invalid event stream header".into());
                }
                let len = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]) as usize;
                offset += 2;
                if offset + len > bytes.len() {
                    return Err("Invalid event stream header".into());
                }
                let raw = &bytes[offset..offset + len];
                offset += len;
                if value_type == 7 {
                    String::from_utf8_lossy(raw).into_owned()
                } else {
                    base64::engine::general_purpose::STANDARD.encode(raw)
                }
            }
            9 => {
                if offset + 16 > bytes.len() {
                    return Err("Invalid event stream header".into());
                }
                let raw = &bytes[offset..offset + 16];
                offset += 16;
                let high = u64::from_be_bytes(raw[0..8].try_into().expect("8 bytes"));
                let low = u64::from_be_bytes(raw[8..16].try_into().expect("8 bytes"));
                format!("{high:016x}{low:016x}")
            }
            _ => return Err("Unsupported event stream header type".into()),
        };
        headers.insert(name, value);
    }
    Ok(headers)
}

/// 一条 event-stream 消息的解码结果。
///
/// 解码只做“协议 → 可匹配值”的翻译，不改状态；四种结果分别对应 SDK 反序列化后的
/// 正常 yield、`$unknown` 跳过、抛服务异常、抛普通 Error。
pub enum DecodedEventStreamItem {
    /// 正常事件（会先交给 `onProviderStreamEvent`，再进入状态机）。
    Item(Value),
    /// 未知事件类型：SDK 的 `$unknown` → `continue`（既不回调也不进入状态机）。
    Skip,
    /// modeled exception：SDK 抛出 `BedrockRuntimeServiceException` 实例
    /// （`.name` 是 schema 名，`message` 取 payload 的 `message`/`Message`）。
    Exception(BedrockFailure),
    /// unmodeled error：SDK 抛出普通 `Error`（`:error-code` 作为 `.name`）。
    Error(BedrockFailure),
}

/// `schemaDeserializationMiddleware` 追加到普通 `Error` 的提示。
///
/// 文本逐字保留（含前导换行、双空格缩进与 `{error}` 字面量），因为它会出现在
/// `error_message` 里，下游可能按文本匹配。
pub const DESERIALIZATION_ERROR_HINT: &str = "\n  Deserialization error: to see the raw response, inspect the hidden field {error}.$response on this object.";

/// ConverseStreamOutput union 里的 6 个普通事件。
fn is_known_event_type(event_type: &str) -> bool {
    matches!(
        event_type,
        "messageStart"
            | "contentBlockStart"
            | "contentBlockDelta"
            | "contentBlockStop"
            | "messageStop"
            | "metadata"
    )
}

/// `:message-type=exception` 的 modeled 异常（union 里只有这五个 exception member）。
fn modeled_exception_name(exception_type: &str) -> Option<&'static str> {
    match exception_type {
        "internalServerException" => Some("InternalServerException"),
        "modelStreamErrorException" => Some("ModelStreamErrorException"),
        "validationException" => Some("ValidationException"),
        "throttlingException" => Some("ThrottlingException"),
        "serviceUnavailableException" => Some("ServiceUnavailableException"),
        _ => None,
    }
}

/// `readEventMember` 的 modeled 异常消息。
///
/// `Object.assign(new ErrCtor({}), metadata, { message }, dataObject)`：
/// `message` 先取 `dataObject.message ?? dataObject.Message ?? "Unknown"`，
/// 然后 `dataObject` 里的同名键（含显式 `null`）会覆盖它。
fn modeled_exception_message(payload: &Value) -> String {
    let Some(object) = payload.as_object() else {
        return "Unknown".into();
    };
    if let Some(value) = object.get("message") {
        return crate::api::google_shared::js_string_of_value(Some(value));
    }
    match object.get("Message") {
        Some(Value::Null) | None => "Unknown".into(),
        Some(value) => crate::api::google_shared::js_string_of_value(Some(value)),
    }
}

/// JSON.parse 失败：SDK 抛 V8 的 `SyntaxError`（`name` 是 `SyntaxError`，不是服务异常）。
fn js_syntax_error_failure(message: String) -> BedrockFailure {
    BedrockFailure::Named {
        name: "SyntaxError".into(),
        message,
        metadata: None,
        body: None,
    }
}

/// 把一条 event-stream 消息转成 TS 的 `ConverseStreamOutput` 项或失败。
///
/// 与 TS 运行时（`@smithy/core` 的 `getMessageUnmarshaller`）一致：
///
/// - `event`：已知事件类型 yield 一项；未知类型是 `$unknown` → 跳过；
/// - `exception`：modeled 类型抛服务异常实例；未知类型抛 `Error`（message 是原始正文）；
/// - `error`：抛普通 `Error`（name = `:error-code`，message = `:error-message`）；
/// - 任何 JSON.parse 失败 → `SyntaxError`（`js_parse` 给出与 V8 逐字一致的文案）。
///
/// `:message-type` 缺失时按 `"event"` 处理。`event` 缺 `:event-type` 返回
/// `Err(String)`（协议级失败）；`:message-type` 不是三种已知值时返回
/// `Error(BedrockFailure::named("Error", "Unrecognizable event type: ..."))`。
pub fn eventstream_message_to_item(
    message: &EventStreamMessage,
) -> Result<DecodedEventStreamItem, String> {
    let message_type = message.header(":message-type").unwrap_or("event");
    let payload_text = String::from_utf8_lossy(&message.payload).into_owned();
    let parse_payload = || -> Result<Value, BedrockFailure> {
        if message.payload.is_empty() {
            return Ok(Value::Object(Map::new()));
        }
        kiana_json::js_parse::parse(&payload_text)
            .map_err(|error| js_syntax_error_failure(error.message))
    };
    match message_type {
        "event" => {
            let event_type = message
                .header(":event-type")
                .ok_or_else(|| "Missing event type".to_string())?;
            if !is_known_event_type(event_type) {
                return Ok(DecodedEventStreamItem::Skip);
            }
            match parse_payload() {
                Ok(payload) => Ok(DecodedEventStreamItem::Item(
                    crate::api::google_shared::object([(event_type, payload)]),
                )),
                Err(failure) => Ok(DecodedEventStreamItem::Error(failure)),
            }
        }
        "exception" => {
            let exception_type = message.header(":exception-type").unwrap_or("Unknown");
            match modeled_exception_name(exception_type) {
                Some(name) => match parse_payload() {
                    Ok(payload) => Ok(DecodedEventStreamItem::Exception(BedrockFailure::Service {
                        name: name.to_string(),
                        message: modeled_exception_message(&payload),
                        metadata: BedrockErrorMetadata::default(),
                        body: None,
                    })),
                    Err(failure) => Ok(DecodedEventStreamItem::Error(failure)),
                },
                // 未知异常类型：`new Error(toUtf8(message.body))` + `error.name = code`。
                None => Ok(DecodedEventStreamItem::Error(BedrockFailure::Named {
                    name: exception_type.to_string(),
                    message: payload_text,
                    metadata: None,
                    body: None,
                })),
            }
        }
        "error" => {
            // `message.headers[":error-message"].value || "UnknownError"`；payload 不参与。
            let error_code = message.header(":error-code").unwrap_or("Unknown");
            let message_text = message
                .header(":error-message")
                .filter(|text| !text.is_empty())
                .unwrap_or("UnknownError")
                .to_string();
            Ok(DecodedEventStreamItem::Error(BedrockFailure::Named {
                name: error_code.to_string(),
                message: message_text,
                metadata: None,
                body: None,
            }))
        }
        _ => Ok(DecodedEventStreamItem::Error(BedrockFailure::named(
            "Error",
            format!(
                "Unrecognizable event type: {}",
                message.header(":event-type").unwrap_or("undefined")
            ),
        ))),
    }
}

// ---------------------------------------------------------------------------
// 事件循环
// ---------------------------------------------------------------------------

/// 打开的内容块状态（`OpenBlock`）。
///
/// 只存在于流式过程中，不进入对外的 partial 消息：
///
/// - `content_index`：该块在 [`AssistantMessage::content`] 里的下标（协议给的
///   `contentBlockIndex` 只是 Map 的键，不保证与 content 下标相同）；
/// - `arguments`：工具调用的参数累积器，仅 ToolCall 块有；
/// - `redacted_chunks`：被脱敏思考内容的原始字节，仅在收到 `redactedContent` 后出现。
#[derive(Default)]
pub struct OpenBlock {
    content_index: usize,
    arguments: Option<ToolCallArgumentsStream>,
    redacted_chunks: Option<Vec<u8>>,
}

/// `OpenBlocks`：key = `contentBlockIndex`。
///
/// 用 [`IndexMap`] 保持插入顺序；`contentBlockStop` 用 `shift_remove` 消费条目，
/// 未关闭的块由 `flush_open_blocks` 在流结束时统一收尾。
pub type OpenBlocks = IndexMap<u64, OpenBlock>;

/// 处理一个（已解码的）ConverseStream 项。
///
/// 分派顺序严格按 TS 的 if/else 链：`messageStart` → `contentBlockStart` →
/// `contentBlockDelta` → `contentBlockStop` → `messageStop` → `metadata` → 五个
/// `*Exception` 键。其它形状被忽略。
///
/// 失败：`messageStart` 的 role 不是 `"assistant"` 时返回 Named Error；五个
/// `*Exception` 键返回 [`BedrockFailure::Value`]（JSON stringify 后的裸对象），由调用方
/// 终止流。返回 `Err` 表示整条流不可继续，不是“跳过这一项”。
pub fn handle_stream_item(
    model: &Model,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    open_blocks: &mut OpenBlocks,
    item: &Value,
) -> Result<(), BedrockFailure> {
    let object = item.as_object();
    let get = |key: &str| object.and_then(|object| object.get(key));

    if let Some(message_start) = get("messageStart") {
        let role = message_start.get("role").and_then(Value::as_str);
        // role 不对说明请求/响应配错了对端，必须终止流而不是忽略。
        if role != Some("assistant") {
            return Err(BedrockFailure::named(
                "Error",
                "Unexpected assistant message start but got user message start instead",
            ));
        }
        let _ = stream.push(AssistantMessageEvent::Start {
            partial: Box::new(output.clone()),
        });
    } else if let Some(event) = get("contentBlockStart") {
        handle_content_block_start(event, open_blocks, output, stream);
    } else if let Some(event) = get("contentBlockDelta") {
        handle_content_block_delta(event, open_blocks, output, stream);
    } else if let Some(event) = get("contentBlockStop") {
        handle_content_block_stop(event, open_blocks, output, stream);
    } else if let Some(message_stop) = get("messageStop") {
        let reason = message_stop.get("stopReason").and_then(Value::as_str);
        output.raw_stop_reason = reason.map(str::to_string);
        let (stop_reason, error_message) = map_stop_reason(reason);
        output.stop_reason = stop_reason;
        if let Some(error_message) = error_message {
            output.error_message = Some(error_message);
        }
    } else if let Some(metadata) = get("metadata") {
        handle_metadata(metadata, model, output);
    } else if let Some(exception) = get("internalServerException") {
        // 流内 modeled 异常以裸对象出现；TS 直接 throw 该对象，message 取 stringify。
        return Err(bare_exception_failure(exception));
    } else if let Some(exception) = get("modelStreamErrorException") {
        return Err(bare_exception_failure(exception));
    } else if let Some(exception) = get("validationException") {
        return Err(bare_exception_failure(exception));
    } else if let Some(exception) = get("throttlingException") {
        return Err(bare_exception_failure(exception));
    } else if let Some(exception) = get("serviceUnavailableException") {
        return Err(bare_exception_failure(exception));
    }
    Ok(())
}

fn bare_exception_failure(value: &Value) -> BedrockFailure {
    // TS 的 `throw item.throttlingException` 抛的是裸对象：`safeJsonStringify(value)`。
    BedrockFailure::Value {
        message: kiana_json::js_object::stringify(value),
    }
}

/// 处理 `contentBlockStart`：目前只有 `toolUse` 会建块。
///
/// 文本与思考块没有 start 事件，它们在第一个 delta 到来时才隐式创建。这里把新的
/// ToolCall 追加到 output，并在 `OpenBlocks` 里登记 content 下标与参数累积器；
/// `contentBlockIndex` 或 `start.toolUse` 缺失时直接返回（与 TS 的可选链容错一致）。
fn handle_content_block_start(
    event: &Value,
    open_blocks: &mut OpenBlocks,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
) {
    let Some(index) = event.get("contentBlockIndex").and_then(Value::as_f64) else {
        return;
    };
    let Some(start) = event.get("start") else {
        return;
    };
    let Some(tool_use) = start.get("toolUse") else {
        return;
    };
    let id = tool_use
        .get("toolUseId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let name = tool_use.get("name").and_then(Value::as_str).unwrap_or("");
    output
        .content
        .push(AssistantContentBlock::ToolCall(ToolCall {
            kind_tool_call: ToolCallKind::ToolCall,
            id: id.to_string(),
            name: name.to_string(),
            arguments: JsonObject::new(),
            ..ToolCall::default()
        }));
    let content_index = output.content.len() - 1;
    // 记录的是 output.content 的下标；协议里的 index 只是 OpenBlocks 的键。
    open_blocks.insert(
        index as u64,
        OpenBlock {
            content_index,
            arguments: Some(ToolCallArgumentsStream::new()),
            redacted_chunks: None,
        },
    );
    let _ = stream.push(AssistantMessageEvent::ToolCallStart {
        content_index,
        partial: Box::new(output.clone()),
    });
}

/// 处理 `contentBlockDelta`：按 `text` → `toolUse` → `reasoningContent` 的顺序分派。
///
/// 首个 delta 会隐式创建对应内容块（工具调用除外，它在 start 时已创建）：
///
/// - text：新建空 Text 块并推 `text_start`，随后累加并推 `text_delta`；
/// - toolUse：只有已存在 ToolCall 且带参数累积器时才处理。增量交给
///   `ToolCallArgumentsStream::append`，解析出的 partial 参数写回消息块，并推
///   `toolcall_delta`；
/// - reasoningContent：新建 Thinking 块（signature 初始为空串），累加 `text`、追加
///   `signature`；遇到 `redactedContent` 时置 `redacted = true`、清空签名、正文插入
///   [`REDACTED_THINKING_PLACEHOLDER`] 并推一次占位 delta，同时把字节累积到
///   `OpenBlock::redacted_chunks`（真正的 base64 在 stop/flush 时才写入签名）。
///
/// `open_exists` 在分派前计算：text/reasoning 分支用它决定是否新建块；toolUse 分支
/// 还额外检查 content 块的类型与累积器是否存在。
fn handle_content_block_delta(
    event: &Value,
    open_blocks: &mut OpenBlocks,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
) {
    let Some(index) = event.get("contentBlockIndex").and_then(Value::as_f64) else {
        return;
    };
    let index = index as u64;
    let Some(delta) = event.get("delta") else {
        return;
    };
    // 先记录是否已有 open 块：text/reasoning 分支需要据此决定是否新建。
    let open_exists = open_blocks.contains_key(&index);

    // 文本块没有 start 事件：第一个 delta 到来时创建。
    if let Some(text) = delta.get("text").and_then(Value::as_str) {
        if !open_exists {
            output
                .content
                .push(AssistantContentBlock::Text(TextContent {
                    kind_text: crate::types::TextKind::Text,
                    text: String::new(),
                    text_signature: None,
                }));
            let content_index = output.content.len() - 1;
            open_blocks.insert(
                index,
                OpenBlock {
                    content_index,
                    arguments: None,
                    redacted_chunks: None,
                },
            );
            let _ = stream.push(AssistantMessageEvent::TextStart {
                content_index,
                partial: Box::new(output.clone()),
            });
        }
        let open = open_blocks.get(&index).expect("inserted above");
        let content_index = open.content_index;
        if let Some(AssistantContentBlock::Text(block)) = output.content.get_mut(content_index) {
            block.text.push_str(text);
            let _ = stream.push(AssistantMessageEvent::TextDelta {
                content_index,
                delta: text.to_string(),
                partial: Box::new(output.clone()),
            });
        }
        return;
    }

    // 工具调用块在 contentBlockStart 已创建；这里只追加参数增量。
    if let Some(tool_use) = delta.get("toolUse") {
        let is_tool_call = open_blocks
            .get(&index)
            .map(|open| open.arguments.is_some())
            .unwrap_or(false)
            && matches!(
                open_blocks
                    .get(&index)
                    .and_then(|open| output.content.get(open.content_index)),
                Some(AssistantContentBlock::ToolCall(_))
            );
        if is_tool_call {
            let input = tool_use.get("input").and_then(Value::as_str).unwrap_or("");
            let open = open_blocks.get_mut(&index).expect("checked");
            let content_index = open.content_index;
            if let Some(arguments) = open.arguments.as_mut() {
                let parsed = arguments.append(input);
                if let Some(AssistantContentBlock::ToolCall(call)) =
                    output.content.get_mut(content_index)
                {
                    call.arguments = parsed;
                }
            }
            let _ = stream.push(AssistantMessageEvent::ToolCallDelta {
                content_index,
                delta: input.to_string(),
                partial: Box::new(output.clone()),
            });
        }
        return;
    }

    // 思考块同样隐式创建；signature 与 redactedContent 互斥（redacted 置位后忽略签名）。
    if let Some(reasoning) = delta.get("reasoningContent") {
        if !open_exists {
            output.content.push(AssistantContentBlock::Thinking(
                crate::types::ThinkingContent {
                    kind_thinking: crate::types::ThinkingKind::Thinking,
                    thinking: String::new(),
                    thinking_signature: Some(String::new()),
                    redacted: None,
                },
            ));
            let content_index = output.content.len() - 1;
            open_blocks.insert(
                index,
                OpenBlock {
                    content_index,
                    arguments: None,
                    redacted_chunks: None,
                },
            );
            let _ = stream.push(AssistantMessageEvent::ThinkingStart {
                content_index,
                partial: Box::new(output.clone()),
            });
        }
        let content_index = open_blocks
            .get(&index)
            .map(|open| open.content_index)
            .unwrap_or(0);
        let mut emitted_text: Option<String> = None;
        let mut emitted_placeholder = false;
        {
            let Some(AssistantContentBlock::Thinking(block)) =
                output.content.get_mut(content_index)
            else {
                return;
            };
            if let Some(text) = reasoning
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                block.thinking.push_str(text);
                emitted_text = Some(text.to_string());
            }
            if let Some(signature) = reasoning
                .get("signature")
                .and_then(Value::as_str)
                .filter(|signature| !signature.is_empty())
                && block.redacted != Some(true)
            {
                let existing = block.thinking_signature.clone().unwrap_or_default();
                block.thinking_signature = Some(format!("{existing}{signature}"));
            }
            if let Some(redacted) = reasoning.get("redactedContent")
                && let Some(bytes) = bytes_from_json(redacted)
                && !bytes.is_empty()
            {
                // 首次出现 redactedContent：正文只留占位符，密文按块累积。
                if block.redacted != Some(true) {
                    block.redacted = Some(true);
                    block.thinking_signature = Some(String::new());
                    block.thinking.push_str(REDACTED_THINKING_PLACEHOLDER);
                    emitted_placeholder = true;
                }
                let open = open_blocks.get_mut(&index).expect("open");
                open.redacted_chunks
                    .get_or_insert_with(Vec::new)
                    .extend(bytes);
            }
        }
        if let Some(text) = emitted_text {
            let _ = stream.push(AssistantMessageEvent::ThinkingDelta {
                content_index,
                delta: text,
                partial: Box::new(output.clone()),
            });
        }
        // 占位 delta 在真实文本之后单独推送，与 TS 的推送顺序一致。
        if emitted_placeholder {
            let _ = stream.push(AssistantMessageEvent::ThinkingDelta {
                content_index,
                delta: REDACTED_THINKING_PLACEHOLDER.to_string(),
                partial: Box::new(output.clone()),
            });
        }
    }
}

/// 处理 `contentBlockStop`：消费 open 块并推对应的 end 事件。
///
/// 用 `shift_remove` 取走条目，因此重复的 stop 或未知 index 都是无操作。按块类型：
///
/// - Text：推 `text_end`；
/// - Thinking：先把累积的密文编码进 `thinking_signature`，再推 `thinking_end`；
/// - ToolCall：`finish()` 一次参数累积器，写入最终 `arguments`/`arguments_error`，
///   再推 `toolcall_end`。
///
/// end 事件携带的 partial 是处理后的 output 克隆；`ToolCallEnd` 额外携带 tool_call。
fn handle_content_block_stop(
    event: &Value,
    open_blocks: &mut OpenBlocks,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
) {
    let Some(index) = event.get("contentBlockIndex").and_then(Value::as_f64) else {
        return;
    };
    let Some(open) = open_blocks.shift_remove(&(index as u64)) else {
        return;
    };
    let content_index = open.content_index;
    match output.content.get(content_index).cloned() {
        Some(AssistantContentBlock::Text(block)) => {
            let _ = stream.push(AssistantMessageEvent::TextEnd {
                content_index,
                content: block.text,
                partial: Box::new(output.clone()),
            });
        }
        Some(AssistantContentBlock::Thinking(block)) => {
            flush_redacted_content(&open, output);
            let block = match output.content.get(content_index) {
                Some(AssistantContentBlock::Thinking(block)) => block.clone(),
                _ => block,
            };
            let _ = stream.push(AssistantMessageEvent::ThinkingEnd {
                content_index,
                content: block.thinking,
                partial: Box::new(output.clone()),
            });
        }
        Some(AssistantContentBlock::ToolCall(_)) => {
            if let Some(arguments) = &open.arguments {
                let finished = arguments.finish();
                if let Some(AssistantContentBlock::ToolCall(call)) =
                    output.content.get_mut(content_index)
                {
                    call.arguments = finished.arguments;
                    call.arguments_error = finished.arguments_error;
                }
            }
            let tool_call = match output.content.get(content_index) {
                Some(AssistantContentBlock::ToolCall(call)) => call.clone(),
                _ => return,
            };
            let _ = stream.push(AssistantMessageEvent::ToolCallEnd {
                content_index,
                tool_call,
                partial: Box::new(output.clone()),
            });
        }
        None => {}
    }
}

/// 把 `OpenBlock::redacted_chunks` 里的字节 base64 编码进思考块的
/// `thinking_signature`；没有累积过密文时不做任何事。
///
/// 这里只改消息内容块，不推送事件；事件由 stop 或流结束路径负责。
fn flush_redacted_content(open: &OpenBlock, output: &mut AssistantMessage) {
    let Some(chunks) = &open.redacted_chunks else {
        return;
    };
    if let Some(AssistantContentBlock::Thinking(block)) = output.content.get_mut(open.content_index)
    {
        block.thinking_signature = Some(base64::engine::general_purpose::STANDARD.encode(chunks));
    }
}

/// 流结束或失败时收尾所有仍未关闭的块：先编码重加密思考内容，再清空表。
///
/// 服务端可能不发送 `contentBlockStop`（截断的流）就结束；这条路径保证已收到的
/// redacted 字节仍会写进 `thinking_signature`，不会因为块没关闭而丢失。
fn flush_open_blocks(open_blocks: &mut OpenBlocks, output: &mut AssistantMessage) {
    for open in open_blocks.values() {
        flush_redacted_content(open, output);
    }
    open_blocks.clear();
}

/// 处理 `metadata`：把 usage 写进消息并计算费用。
///
/// 缺失或非有限数的字段按 0 处理。`totalTokens` 缺失/为 0 时用 input+output 估算。
/// `cacheDetails` 里 `ttl == "1h"` 的条目累加成 `cache_write1h`（计费时按 2 倍 input，
/// 见 `calculate_cost`）。
///
/// 副作用：直接覆盖 `output.usage` 的相关字段并调用 `calculate_cost`。
fn handle_metadata(event: &Value, model: &Model, output: &mut AssistantMessage) {
    let Some(usage) = event.get("usage") else {
        return;
    };
    let number = |key: &str| {
        usage
            .get(key)
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite())
            .unwrap_or(0.0)
    };
    output.usage.input = number("inputTokens") as u64;
    output.usage.output = number("outputTokens") as u64;
    output.usage.cache_read = number("cacheReadInputTokens") as u64;
    output.usage.cache_write = number("cacheWriteInputTokens") as u64;
    // 1h 缓存写入量按明细里的 ttl 统计；没有 cacheDetails 时保持 None。
    output.usage.cache_write1h =
        usage
            .get("cacheDetails")
            .and_then(Value::as_array)
            .map(|details| {
                details
                    .iter()
                    .filter(|detail| detail.get("ttl").and_then(Value::as_str) == Some("1h"))
                    .map(|detail| {
                        detail
                            .get("inputTokens")
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0) as u64
                    })
                    .sum()
            });
    let total = number("totalTokens");
    // TS 的 `totalTokens || input + output`：0 与缺失都走求和。
    output.usage.total_tokens = if total != 0.0 {
        total as u64
    } else {
        output.usage.input + output.usage.output
    };
    // 费用依赖 cache_write1h，必须在 usage 全部写完后计算。
    calculate_cost(model, &mut output.usage);
}

/// 解析 `redactedContent` 的两种编码。
///
/// JSON 数组按 byte[] 处理：逐元素用 `as u8` 的饱和转换（NaN → 0，越界钳到 0/255）；
/// 字符串按 base64 解码。其它形状或非法 base64 返回 `None`。两种表示都接受，覆盖
/// JSON 数组与 base64 字符串两种输入。
fn bytes_from_json(value: &Value) -> Option<Vec<u8>> {
    match value {
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(Value::as_f64)
                .map(|value| value as u8)
                .collect(),
        ),
        Value::String(text) => base64::engine::general_purpose::STANDARD.decode(text).ok(),
        _ => None,
    }
}

/// `decodeRedactedContent(signature)`。
///
/// 回放历史时把 `thinking_signature` 里的 base64 解回字节；空串或非法 base64 返回
/// `None`（对应 TS 用 try/catch 吞掉 `atob` 的错误）。
fn decode_redacted_content(signature: Option<&str>) -> Option<Vec<u8>> {
    let signature = signature.filter(|signature| !signature.is_empty())?;
    base64::engine::general_purpose::STANDARD
        .decode(signature)
        .ok()
}

/// 整数值的 f64 用整数 JSON 表示（`serde_json` 的 `Number` 区分 400 与 400.0）。
///
/// 诊断 details 最终会被序列化成 JSON，TS 的 `JSON.stringify(400)` 输出 `400`；
/// 若这里保留 400.0，输出会变成 `400.0`，与 TS 的可观察结果不一致。
fn status_json(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < 9.007_199_254_740_992e15 {
        Value::from(value as i64)
    } else {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }
}

/// 当前 Unix 时间戳（毫秒）。时钟早于 epoch 或系统错误时返回 0。
fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

/// `stream(model, context, options)`。
///
/// 同步返回事件流；真正的请求在后台任务里执行（`spawn_forward`）。后台任务构造初始
/// [`AssistantMessage`]（`api` 固定为 `"bedrock-converse-stream"`），调用
/// `run_stream`；任何失败都转交 `finish_stream_error`，保证流一定以 `done` 或
/// `error` 事件结束。
///
/// `output`、`open_blocks` 与 request id 由后台任务独占，不需要加锁；调用方只消费
/// 事件流。
pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: BedrockOptions,
) -> AssistantMessageEventStream {
    let event_stream = AssistantMessageEventStream::new();
    let model = model.clone();
    let context = context.clone();
    let stream_for_task = event_stream.clone();
    spawn_forward(async move {
        let mut output = AssistantMessage {
            role: AssistantRole::Assistant,
            content: Vec::new(),
            api: "bedrock-converse-stream".into(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            usage: Usage::default(),
            stop_reason: StopReason::Pending,
            timestamp: now_unix_ms(),
            ..AssistantMessage::default()
        };
        let mut open_blocks: OpenBlocks = IndexMap::new();
        let mut response_request_id: Option<String> = None;
        let mut observed_raw_response = false;
        match run_stream(
            &model,
            &context,
            &options,
            &stream_for_task,
            &mut output,
            &mut open_blocks,
            &mut response_request_id,
            &mut observed_raw_response,
        )
        .await
        {
            Ok(()) => {}
            Err(failure) => {
                finish_stream_error(
                    &options,
                    &stream_for_task,
                    &mut output,
                    &mut open_blocks,
                    failure,
                    response_request_id.as_deref(),
                );
            }
        }
    });
    event_stream
}

/// 测试/嵌入用入口：用给定的流项驱动完整的事件循环（流项可视为 mock 的 SDK 流）。
///
/// `items` 的 `Ok(Value)` 是一条 `ConverseStreamOutput` 项（例如
/// `{"messageStart":{"role":"assistant"}}`）；`Err` 表示迭代器抛出（modeled mid-stream
/// exception / unmodeled error）。
///
/// 与 `run_stream` 的尾部逻辑一致：先逐项回调 `on_provider_stream_event` 再交给
/// [`handle_stream_item`]；结束后检查取消令牌、`stop_reason` 是否仍为 `pending`、
/// 是否落在 error/aborted；成功则 flush 并推 `done`，失败走
/// `finish_stream_error`。
///
/// `context` 当前不参与状态机，只为与 [`stream`] 保持相同签名而保留。
pub fn stream_with_items(
    model: &Model,
    context: &TranscriptContext,
    options: BedrockOptions,
    response_request_id: Option<String>,
    items: Vec<Result<Value, BedrockFailure>>,
) -> AssistantMessageEventStream {
    let event_stream = AssistantMessageEventStream::new();
    let model = model.clone();
    let _context = context.clone();
    let stream_for_task = event_stream.clone();
    spawn_forward(async move {
        let mut output = AssistantMessage {
            role: AssistantRole::Assistant,
            content: Vec::new(),
            api: "bedrock-converse-stream".into(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            usage: Usage::default(),
            stop_reason: StopReason::Pending,
            timestamp: now_unix_ms(),
            ..AssistantMessage::default()
        };
        let mut open_blocks: OpenBlocks = IndexMap::new();
        let request_id = response_request_id;
        let signal = options.base.signal.clone();

        let mut failure: Option<BedrockFailure> = None;
        for item in items {
            match item {
                Ok(item) => {
                    if let Some(callback) = &options.base.on_provider_stream_event {
                        callback(item.clone(), &model);
                    }
                    if let Err(error) = handle_stream_item(
                        &model,
                        &mut output,
                        &stream_for_task,
                        &mut open_blocks,
                        &item,
                    ) {
                        failure = Some(error);
                        break;
                    }
                }
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        if failure.is_none() {
            if signal
                .as_ref()
                .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
            {
                failure = Some(BedrockFailure::Aborted(
                    crate::http::reqwest_client::ABORTED_MESSAGE.into(),
                ));
            } else if output.stop_reason == StopReason::Pending {
                failure = Some(BedrockFailure::named(
                    "Error",
                    "Bedrock stream ended without a stop reason",
                ));
            } else if matches!(output.stop_reason, StopReason::Error | StopReason::Aborted) {
                failure = Some(BedrockFailure::named(
                    "Error",
                    output
                        .error_message
                        .clone()
                        .unwrap_or_else(|| "An unknown error occurred".into()),
                ));
            }
        }
        match failure {
            None => {
                flush_open_blocks(&mut open_blocks, &mut output);
                let _ = stream_for_task.push(AssistantMessageEvent::Done {
                    reason: done_reason(output.stop_reason),
                    message: Box::new(output),
                });
                stream_for_task.end_without_result();
            }
            Some(failure) => {
                finish_stream_error(
                    &options,
                    &stream_for_task,
                    &mut output,
                    &mut open_blocks,
                    failure,
                    request_id.as_deref(),
                );
            }
        }
    });
    event_stream
}

/// catch 分支：flush、改写 stopReason/errorMessage、追加诊断并 push error 事件。
///
/// 规则：取消令牌已触发或失败本身是 Aborted → `stop_reason = aborted`；否则
/// `stop_reason = error`。只有 error 分支追加 `bedrock_response_failure` 诊断
/// （aborted 不追加）。无论哪种情况都推送 `Error` 事件并关闭流，因此调用方一定能
/// 收到终止事件。
///
/// `fallback_request_id` 来自响应头；错误自身没有 requestId 时用它补诊断。
fn finish_stream_error(
    options: &BedrockOptions,
    event_stream: &AssistantMessageEventStream,
    output: &mut AssistantMessage,
    open_blocks: &mut OpenBlocks,
    failure: BedrockFailure,
    fallback_request_id: Option<&str>,
) {
    flush_open_blocks(open_blocks, output);
    output.stop_reason = if options
        .base
        .signal
        .as_ref()
        .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
        || failure.is_aborted()
    {
        StopReason::Aborted
    } else {
        StopReason::Error
    };
    output.error_message = Some(format_bedrock_error(&failure));
    if output.stop_reason == StopReason::Error {
        append_bedrock_failure_diagnostic(output, &failure, fallback_request_id);
    }
    let reason = if output.stop_reason == StopReason::Aborted {
        ErrorReason::Aborted
    } else {
        ErrorReason::Error
    };
    let _ = event_stream.push(AssistantMessageEvent::Error {
        reason,
        error: Box::new(output.clone()),
    });
    event_stream.end_without_result();
}

/// 执行一次 ConverseStream 请求并驱动事件循环（[`stream`] 的后台主体）。
///
/// 阶段：
///
/// 1. 预处理：折叠中途 system 消息、解析客户端配置、读取 strict 模式与缓存策略、
///    [`build_command_input`] 生成命令输入，再允许 `on_payload` 整体替换它。
/// 2. 发送与重试：在 `AWS_MAX_ATTEMPTS` 上限内循环“构造请求（每次重新签名）→ 发送 →
///    非 2xx 分类 → 首帧预读”。传输失败、可重试的非 2xx 与可重试的首帧错误会退避重试。
/// 3. 响应回调：首帧成功后才记录 request id 并调用 `on_response`（TS 的 deserialize
///    中间件对非 2xx 不会回调）。
/// 4. 事件循环：严格按到达顺序一次处理一项（首帧 → 消息队列 → 继续读 chunk），保证
///    同一 chunk 里的后续错误排在前面的正常事件之后；`Skip` 项被忽略。
/// 5. 收尾：检查取消/缺少 stop reason/错误停止原因，flush 未关闭的块并推 `done`。
///
/// `response_request_id` 与 `observed_raw_response` 是输出参数：前者供 catch 分支做
/// 诊断 fallback；后者记录 `on_response` 已回调（本文件内目前只写不读）。
///
/// 错误：失败路径以 [`BedrockFailure`] 返回，由调用方统一转成 error 事件，此时本函数
/// 不结束事件流；成功路径则自行推送 `done` 并调用 `end_without_result`。
#[allow(clippy::too_many_arguments)]
async fn run_stream(
    model: &Model,
    context: &TranscriptContext,
    options: &BedrockOptions,
    event_stream: &AssistantMessageEventStream,
    output: &mut AssistantMessage,
    open_blocks: &mut OpenBlocks,
    response_request_id: &mut Option<String>,
    observed_raw_response: &mut bool,
) -> Result<(), BedrockFailure> {
    // 中途 system 消息先折叠进首条，convert_messages 随后把首条剔除、单独发 system。
    let normalized_context = collapse_system_messages(context);
    let config = resolve_client_config(model, options);
    let supports_strict_mode = model
        .compat
        .as_ref()
        .and_then(|compat| compat.get("supportsStrictMode"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let cache_retention =
        resolve_cache_retention(options.base.cache_retention, options.base.env.as_ref());

    let mut command_input = build_command_input(
        model,
        &normalized_context,
        cache_retention,
        options,
        supports_strict_mode,
    )
    .map_err(|error| BedrockFailure::named("Error", error.message))?;
    if let Some(next) = options
        .base
        .apply_on_payload(command_input.clone(), model)
        .await
    {
        command_input = next;
    }

    let client = resolve_client(&options.base)?;
    let max_attempts = bedrock_max_attempts(options.base.env.as_ref())?;
    let mut attempt: u32 = 0;
    // `client.send()`：SDK 的 StandardRetryStrategy 包住“端点解析 + 发送 + 首帧预读”。
    // 首帧预读（`deserializeEventStream` 里的 `asyncIterator.next()`）也在 send 内：
    // 它抛错时 `responseRequestId` 还没赋值、`onResponse` 也还没回调。
    let (http_response, mut body, mut decoder, first_item, pending_messages) = loop {
        attempt += 1;
        // 每次尝试都重新构造请求：SigV4 的 amz_date 不能跨重试复用。
        let request = build_http_request(
            model,
            &config,
            &command_input,
            options,
            &mut *observed_raw_response,
        )
        .await?;
        let response = match client.execute(request, options.base.signal.as_ref()).await {
            Ok(response) => response,
            Err(error) => {
                // 传输失败在 SDK 里是带 `ECONNRESET`/`ETIMEDOUT` 等 code 的普通 Error（transient）。
                let cancelled = options
                    .base
                    .signal
                    .as_ref()
                    .is_some_and(tokio_util::sync::CancellationToken::is_cancelled);
                if attempt < max_attempts && !cancelled {
                    sleep_bedrock_retry(attempt - 1, false, None).await;
                    continue;
                }
                return Err(http_failure(error, options.base.signal.as_ref()));
            }
        };

        let status = response.response.status;
        // 非 2xx：先读完 body，再按错误码/状态码决定是否重试。
        if !(200..300).contains(&status) {
            let retry_after = parse_retry_after_hint(&response.response.headers);
            let text = crate::api::kiana_messages::read_body_text(
                response.body,
                options.base.signal.as_ref(),
            )
            .await
            .map_err(|error| BedrockFailure::named("Error", error.message))?;
            let failure = non_2xx_failure(status, &response.response.headers, &text);
            if attempt < max_attempts && is_retryable_failure(&failure) {
                sleep_bedrock_retry(attempt - 1, is_throttling_failure(&failure), retry_after)
                    .await;
                continue;
            }
            return Err(failure);
        }

        // 首帧预读属于 send：它失败时 request id 与 on_response 都还没有发生。
        match read_first_event(
            response.body,
            &response.response.headers,
            options.base.signal.as_ref(),
        )
        .await
        {
            Ok(first) => {
                break (
                    response.response,
                    first.body,
                    first.decoder,
                    first.item,
                    first.messages,
                );
            }
            Err(failure) => {
                if attempt < max_attempts && is_retryable_failure(&failure) {
                    sleep_bedrock_retry(attempt - 1, is_throttling_failure(&failure), None).await;
                    continue;
                }
                return Err(failure);
            }
        }
    };

    // 只有拿到首帧后才记录 request id；与 TS 的 deserialize 中间件顺序一致。
    *response_request_id = normalize_diagnostic_value(
        http_response
            .headers
            .get("x-amzn-requestid")
            .map(String::as_str),
    );
    // TS 的 onResponse 是 deserialize 中间件，`await next(args)` 对非 2xx 会先抛错，
    // 因此失败请求不会触发回调。
    if let Some(on_response) = &options.base.on_response {
        on_response(http_response.clone(), model);
        *observed_raw_response = true;
    }

    let mut pending_item = first_item;
    let mut pending_messages = pending_messages;
    let mut finished = false;
    loop {
        // 一次只处理一条消息：同一 chunk 里的后续错误必须排在前面的正常事件之后。
        let decoded = if let Some(item) = pending_item.take() {
            Some(Ok(DecodedEventStreamItem::Item(item)))
        } else if let Some(message) = pending_messages.pop_front() {
            Some(eventstream_message_to_item(&message))
        } else if finished {
            None
        } else {
            match next_chunk(&mut body, options.base.signal.as_ref()).await {
                Ok(Some(bytes)) => {
                    let messages = decoder
                        .push(&bytes)
                        .map_err(|message| BedrockFailure::named("Error", message))?;
                    pending_messages.extend(messages);
                    continue;
                }
                Ok(None) => {
                    finished = true;
                    continue;
                }
                Err(error) => return Err(http_failure(error, options.base.signal.as_ref())),
            }
        };
        let Some(decoded) = decoded else {
            break;
        };
        match decoded.map_err(|message| BedrockFailure::named("Error", message))? {
            DecodedEventStreamItem::Item(item) => {
                if let Some(callback) = &options.base.on_provider_stream_event {
                    callback(item.clone(), model);
                }
                handle_stream_item(model, output, event_stream, open_blocks, &item)?;
            }
            DecodedEventStreamItem::Skip => {}
            DecodedEventStreamItem::Exception(failure) | DecodedEventStreamItem::Error(failure) => {
                return Err(failure);
            }
        }
    }

    // 结束后的三条例外按 TS 顺序检查：取消 > 缺 stop reason > error/aborted 停止原因。
    if options
        .base
        .signal
        .as_ref()
        .is_some_and(|signal| signal.is_cancelled())
    {
        return Err(BedrockFailure::Aborted(
            crate::http::reqwest_client::ABORTED_MESSAGE.into(),
        ));
    }
    if output.stop_reason == StopReason::Pending {
        return Err(BedrockFailure::named(
            "Error",
            "Bedrock stream ended without a stop reason",
        ));
    }
    if matches!(output.stop_reason, StopReason::Error | StopReason::Aborted) {
        return Err(BedrockFailure::named(
            "Error",
            output
                .error_message
                .clone()
                .unwrap_or_else(|| "An unknown error occurred".into()),
        ));
    }

    // 兜底 flush：正常流所有块都已 stop；截断流里未关闭的 redacted 签名靠这里保留。
    flush_open_blocks(open_blocks, output);
    let _ = event_stream.push(AssistantMessageEvent::Done {
        reason: done_reason(output.stop_reason),
        message: Box::new(output.clone()),
    });
    event_stream.end_without_result();
    Ok(())
}

/// `AWS_MAX_ATTEMPTS` → 重试总尝试次数（SDK 默认 3）。
///
/// 文案逐字沿用 SDK（含 `mast` 拼写错误）：非法值在客户端构造阶段抛出。
/// 返回值是“总尝试次数”（含首次请求），不是额外的重试次数；负数被钳到 0。
fn bedrock_max_attempts(env: Option<&ProviderEnv>) -> Result<u32, BedrockFailure> {
    let Some(value) = get_provider_env_value("AWS_MAX_ATTEMPTS", env) else {
        return Ok(3);
    };
    match js_parse_int(&value) {
        Some(parsed) => Ok(parsed.max(0.0) as u32),
        None => Err(BedrockFailure::named(
            "Error",
            format!("Environment variable AWS_MAX_ATTEMPTS mast be a number, got \"{value}\""),
        )),
    }
}

/// `parseInt(value)`（无 radix 参数）：修剪前导空白、可选符号、按前缀识别 0x 十六进制。
fn js_parse_int(text: &str) -> Option<f64> {
    let trimmed = text.trim_start_matches(|ch: char| ch.is_whitespace() || ch == '\u{feff}');
    let mut chars = trimmed.chars().peekable();
    let mut sign = 1.0;
    match chars.peek() {
        Some('-') => {
            sign = -1.0;
            chars.next();
        }
        Some('+') => {
            chars.next();
        }
        _ => {}
    }
    let rest: String = chars.collect();
    if let Some(hex) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        let digits: String = hex.chars().take_while(char::is_ascii_hexdigit).collect();
        if digits.is_empty() {
            return None;
        }
        return u128::from_str_radix(&digits, 16)
            .ok()
            .map(|value| sign * value as f64);
    }
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<f64>().ok().map(|value| sign * value)
}

/// `Math.random()`（`DefaultRetryBackoffStrategy` 的抖动）。
///
/// 用 53 位精度的均匀分布；系统随机源失败时回退 0.5，退避仍然可用（不是错误路径）。
fn math_random() -> f64 {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        return 0.5;
    }
    (u64::from_le_bytes(bytes) >> 11) as f64 / (1u64 << 53) as f64
}

/// `DefaultRetryBackoffStrategy#computeNextBackoffDelay` + `refreshRetryTokenForRetry`。
///
/// `delay = floor(random * min(base * 2^retry, 20000))`；带 `retry-after` 时
/// `max(delay, min(hint - now, delay + 5000))`。
///
/// `retry_count` 从 0 开始（首次重试），上限 20 秒；返回值单位毫秒。
fn sdk_retry_delay_ms(retry_count: u32, base: f64, retry_after_ms: Option<f64>) -> f64 {
    let ceiling = (base * 2f64.powi(retry_count as i32)).min(20_000.0);
    let backoff = (math_random() * ceiling).floor();
    match retry_after_ms {
        Some(hint) => backoff.max(hint.min(backoff + 5_000.0)),
        None => backoff,
    }
}

/// SDK 的重试 sleep 不可中止（`setTimeout`），与 kiana 自己的 abortable sleep 不同。
async fn sleep_bedrock_retry(retry_count: u32, throttling: bool, retry_after_ms: Option<f64>) {
    let base = if throttling { 500.0 } else { 100.0 };
    let delay = sdk_retry_delay_ms(retry_count, base, retry_after_ms);
    if delay > 0.0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay as u64)).await;
    }
}

const THROTTLING_ERROR_CODES: &[&str] = &[
    "BandwidthLimitExceeded",
    "EC2ThrottledException",
    "LimitExceededException",
    "PriorRequestNotComplete",
    "ProvisionedThroughputExceededException",
    "RequestLimitExceeded",
    "RequestThrottled",
    "RequestThrottledException",
    "SlowDown",
    "ThrottledException",
    "Throttling",
    "ThrottlingException",
    "TooManyRequestsException",
    "TransactionInProgressException",
];
const TRANSIENT_ERROR_CODES: &[&str] =
    &["TimeoutError", "RequestTimeout", "RequestTimeoutException"];
const TRANSIENT_ERROR_STATUS_CODES: &[u16] = &[500, 502, 503, 504];

fn failure_status(failure: &BedrockFailure) -> Option<f64> {
    failure
        .metadata()
        .and_then(|metadata| metadata.http_status_code)
}

/// `isThrottlingError`。
fn is_throttling_failure(failure: &BedrockFailure) -> bool {
    let name = failure.name().unwrap_or("");
    let status = failure_status(failure);
    status == Some(429.0) || THROTTLING_ERROR_CODES.contains(&name)
}

/// `getRetryErrorType(error)` ∈ {THROTTLING, TRANSIENT, SERVER_ERROR, CLIENT_ERROR}，
/// 以及 `StandardRetryStrategy#isRetryableError`：只有 THROTTLING/TRANSIENT 会重试。
fn is_retryable_failure(failure: &BedrockFailure) -> bool {
    // 分类顺序：中止（永不重试）→ THROTTLING → TRANSIENT。
    if failure.is_aborted() {
        return false;
    }
    if is_throttling_failure(failure) {
        return true;
    }
    let name = failure.name().unwrap_or("");
    let status = failure_status(failure);
    // `isTransientError`（Node 侧；浏览器 TypeError 分支不适用的代码路径一并省略）。
    if TRANSIENT_ERROR_CODES.contains(&name) {
        return true;
    }
    if name == "InvalidSignatureException" && failure.message().contains("Signature expired") {
        return true;
    }
    if status.is_some_and(|status| {
        u16::try_from(status as i64)
            .ok()
            .is_some_and(|status| TRANSIENT_ERROR_STATUS_CODES.contains(&status))
    }) {
        return true;
    }
    false
}

/// `parseRetryAfterHeader`：返回可用的提示延迟（毫秒）。
///
/// 逐个响应头（名字大小写不敏感）识别：
///
/// - `retry-after`：以 `GMT` 结尾时只按 RFC2822 日期解析，失败即视为无效；否则若是
///   无符号十进制数按秒解析；其它情况再尝试 RFC2822（只有未来时间才算有效）；
/// - `x-amz-retry-after`：毫秒数，空串按 0 处理。
///
/// 只返回第一个识别到的头；单位统一换算成毫秒。任一候选值无效时立即返回 `None`
/// （不会继续尝试后面的头）。
fn parse_retry_after_hint(headers: &IndexMap<String, String>) -> Option<f64> {
    for (name, value) in headers {
        match name.to_lowercase().as_str() {
            "retry-after" => {
                let mut seconds = f64::NAN;
                if value.ends_with("GMT") {
                    if let Ok(date) = chrono::DateTime::parse_from_rfc2822(value.trim()) {
                        seconds = (date.timestamp_millis() as f64 - js_now_ms()) / 1000.0;
                    }
                } else if is_js_unsigned_decimal(value) {
                    seconds = value.parse::<f64>().unwrap_or(f64::NAN);
                } else if let Ok(date) = chrono::DateTime::parse_from_rfc2822(value) {
                    let timestamp = date.timestamp_millis() as f64;
                    if timestamp >= js_now_ms() {
                        seconds = (timestamp - js_now_ms()) / 1000.0;
                    }
                }
                if seconds.is_nan() {
                    return None;
                }
                return Some(seconds * 1000.0);
            }
            "x-amz-retry-after" => {
                let millis = if value.trim().is_empty() {
                    0.0
                } else {
                    value.trim().parse::<f64>().unwrap_or(f64::NAN)
                };
                if millis.is_nan() {
                    return None;
                }
                return Some(millis);
            }
            _ => {}
        }
    }
    None
}

/// JS `/^((\d+)|(\d+\.\d+))$/`。
///
/// 只接受非负整数或带小数部分的十进制（`"1"`、`"1.5"`）；拒绝空串、前导小数点、
/// 尾随小数点、指数、符号与多小数点。
fn is_js_unsigned_decimal(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let mut dots = 0;
    let mut digits_after_dot = 0;
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'0'..=b'9' => {
                if dots == 1 {
                    digits_after_dot += 1;
                }
            }
            b'.' if dots == 0 && index > 0 => dots += 1,
            _ => return false,
        }
    }
    dots == 0 || digits_after_dot > 0
}

/// 当前 Unix 时间戳（毫秒，f64）。用于 `retry-after` 日期比较；时钟异常时返回 0。
fn js_now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as f64)
        .unwrap_or(0.0)
}

/// 首帧预读的结果。
struct FirstEvent {
    /// 第一个真实事件；流在读首帧前就结束时为 `None`。
    pub item: Option<Value>,
    /// 与首帧同一 chunk 里、尚未处理的消息（保持到达顺序）。
    pub messages: std::collections::VecDeque<EventStreamMessage>,
    /// 继续读取剩余字节的流。
    pub body: ByteStream,
    /// 已消费到当前 chunk 末尾的解码器。
    pub decoder: EventStreamDecoder,
}

/// 首帧预读：对应 `deserializeEventStream` 里 `asyncIterator.next()` 的第一跳。
///
/// 跳过 `$unknown` 事件直到拿到第一个真实事件（或流结束）；同一 chunk 里后续的消息
/// 原样留在队列里，由调用方按“流中途”语义处理。若第一个可解码结果是异常或协议错误，
/// 会经 `first_frame_failure` 处理：普通 `Error` 补反序列化提示与 `status = 200`
/// 元数据，Service/Aborted/Value 原样返回。
async fn read_first_event(
    mut body: ByteStream,
    headers: &IndexMap<String, String>,
    signal: Option<&tokio_util::sync::CancellationToken>,
) -> Result<FirstEvent, BedrockFailure> {
    let request_id = find_header(headers, "x-amzn-requestid");
    let mut decoder = EventStreamDecoder::new();
    loop {
        let (done, bytes) = match next_chunk(&mut body, signal).await {
            Ok(Some(bytes)) => (false, bytes),
            Ok(None) => (true, Vec::new()),
            Err(error) => {
                return Err(first_frame_failure(http_failure(error, signal), request_id));
            }
        };
        let messages = match decoder.push(&bytes) {
            Ok(messages) => messages,
            Err(message) => {
                return Err(first_frame_failure(
                    BedrockFailure::named("Error", message),
                    request_id,
                ));
            }
        };
        let mut messages = messages.into_iter();
        // 跳过 $unknown，直到第一条真实事件；同 chunk 的后续消息留给调用方。
        while let Some(message) = messages.next() {
            match eventstream_message_to_item(&message) {
                Ok(DecodedEventStreamItem::Item(item)) => {
                    return Ok(FirstEvent {
                        item: Some(item),
                        messages: messages.collect(),
                        body,
                        decoder,
                    });
                }
                Ok(DecodedEventStreamItem::Skip) => {}
                Ok(DecodedEventStreamItem::Exception(failure))
                | Ok(DecodedEventStreamItem::Error(failure)) => {
                    return Err(first_frame_failure(failure, request_id));
                }
                Err(message) => {
                    return Err(first_frame_failure(
                        BedrockFailure::named("Error", message),
                        request_id,
                    ));
                }
            }
        }
        if done {
            return Ok(FirstEvent {
                item: None,
                messages: std::collections::VecDeque::new(),
                body,
                decoder,
            });
        }
    }
}

/// `schemaDeserializationMiddleware` 的 catch：普通 Error 补 hint 与 `$metadata`；
/// modeled 服务异常自带 `$metadata` 属性（值 undefined），不受影响。
///
/// 只改写 [`BedrockFailure::Named`]；Aborted、Service 与 Value 原样返回（中止不是
/// 反序列化错误，服务异常已经带了自己的语义）。
fn first_frame_failure(failure: BedrockFailure, request_id: Option<String>) -> BedrockFailure {
    match failure {
        BedrockFailure::Named { name, message, .. } => BedrockFailure::Named {
            name,
            message: format!("{message}{DESERIALIZATION_ERROR_HINT}"),
            metadata: Some(BedrockErrorMetadata {
                http_status_code: Some(200.0),
                request_id,
            }),
            body: None,
        },
        // 中止不是反序列化错误。
        BedrockFailure::Aborted(_) => failure,
        // modeled 异常：`new ErrCtor({})` 在 ServiceException 构造器里设了 `$metadata`（undefined）。
        BedrockFailure::Service { .. } => failure,
        BedrockFailure::Value { .. } => failure,
    }
}

/// 非 2xx：`AwsRestJsonProtocol#deserializeResponse` + `handleError` + `decorateServiceException`。
///
/// 步骤：空 body 不解析；否则按 JSON 解析（失败则返回带反序列化提示的 `SyntaxError`，
/// 不是服务异常）。错误码依次从 `x-amzn-errortype`、body 的 `code`、body 的 `__type`
/// 提取；message 按 `message || Message || "UnknownError"` 取。响应 JSON 的 `body`/
/// `error` 字段会被复制到失败对象上，供 [`format_bedrock_error`] 展示。
fn non_2xx_failure(
    status: u16,
    headers: &IndexMap<String, String>,
    body_text: &str,
) -> BedrockFailure {
    let metadata = BedrockErrorMetadata {
        http_status_code: Some(status as f64),
        request_id: find_header(headers, "x-amzn-requestid"),
    };
    // 空 body 不解析（`deserializer.read(15, bytes)` 仅在 `bytes.byteLength > 0` 时执行）。
    let parsed = if body_text.is_empty() {
        Value::Object(Map::new())
    } else {
        match kiana_json::js_parse::parse(body_text) {
            Ok(parsed) => parsed,
            Err(error) => {
                // JSON.parse 的 SyntaxError 不是 ErrorCtor 实例：`formatBedrockError` 不加前缀。
                return BedrockFailure::Named {
                    name: "SyntaxError".into(),
                    message: format!("{}{DESERIALIZATION_ERROR_HINT}", error.message),
                    metadata: Some(metadata),
                    body: None,
                };
            }
        }
    };
    let object = parsed.as_object();
    let name = rest_json_error_code(headers, object).unwrap_or_else(|| "Unknown".into());
    // `decorateServiceException(exception, dataObject)` 把响应 JSON 的 `body`/`error`
    // 字段复制到异常对象上；normalizeProviderError 从那里取 body（`$response.body` 是流）。
    // decorateServiceException 会把响应 JSON 的 body/error 字段挂到异常上。
    let body = object
        .and_then(|object| object.get("body"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            object
                .and_then(|object| object.get("error"))
                .filter(|value| is_plain_non_empty_object(value))
                .map(kiana_json::js_object::stringify)
        });
    BedrockFailure::Service {
        name,
        message: response_error_message(object),
        metadata,
        body,
    }
}

/// `loadRestJsonErrorCode`：header → body `code` → body `__type`。
fn rest_json_error_code(
    headers: &IndexMap<String, String>,
    object: Option<&Map<String, Value>>,
) -> Option<String> {
    if let Some(value) = find_header(headers, "x-amzn-errortype") {
        return Some(sanitize_error_code(&value));
    }
    let object = object?;
    if let Some((_, value)) = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("code"))
        && !value.is_null()
    {
        return Some(sanitize_error_code(
            &crate::api::google_shared::js_string_of_value(Some(value)),
        ));
    }
    let value = object.get("__type").filter(|value| !value.is_null())?;
    Some(sanitize_error_code(
        &crate::api::google_shared::js_string_of_value(Some(value)),
    ))
}

/// `sanitizeErrorCode`：先按 `,`，再按 `:`，最后按 `#` 截断。
///
/// 前两个分隔符取**之前**的部分（去掉 `Error,foo` / `ns:Error` 的后缀）；`#` 取
/// **之后**的部分（`ns#Error` → `Error`），与 TS 的 `split("#")[1]` 一致。
fn sanitize_error_code(raw: &str) -> String {
    let mut clean = raw;
    if let Some((head, _)) = clean.split_once(',') {
        clean = head;
    }
    if let Some((head, _)) = clean.split_once(':') {
        clean = head;
    }
    if let Some(tail) = clean.split('#').nth(1) {
        clean = tail;
    }
    clean.to_string()
}

/// `findKey(headers, name)`（大小写不敏感）。
fn find_header(headers: &IndexMap<String, String>, name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

/// `decorateServiceException` 的最终 message：`message || Message || "UnknownError"`。
///
/// “或”是 JS 的 truthiness：`null`、空串、`0`、`false` 都算缺失，继续回落到下一个
/// 候选；只有两个候选都不可用时才用 `"UnknownError"`。
fn response_error_message(object: Option<&Map<String, Value>>) -> String {
    let message = object.and_then(|object| object.get("message"));
    let big_message = object.and_then(|object| object.get("Message"));
    let chosen = message
        .filter(|value| !value.is_null())
        .or_else(|| big_message.filter(|value| !value.is_null()));
    if let Some(value) = chosen.filter(|value| crate::api::google_shared::js_truthy(value)) {
        return crate::api::google_shared::js_string_of_value(Some(value));
    }
    if let Some(value) = big_message.filter(|value| crate::api::google_shared::js_truthy(value)) {
        return crate::api::google_shared::js_string_of_value(Some(value));
    }
    "UnknownError".into()
}

/// `isPlainNonEmptyObject`。
fn is_plain_non_empty_object(value: &Value) -> bool {
    value.as_object().is_some_and(|object| !object.is_empty())
}

/// 把内部 [`StopReason`] 映射成流 `done` 事件里的 [`crate::types::DoneReason`]。
///
/// 只有 length/toolUse/deferred 单独成类，其余（stop/error/aborted/pending）都归为
/// Stop——错误路径实际上不会走到这里（会先转成 error 事件）。
fn done_reason(stop_reason: StopReason) -> crate::types::DoneReason {
    match stop_reason {
        StopReason::Length => crate::types::DoneReason::Length,
        StopReason::ToolUse => crate::types::DoneReason::ToolUse,
        StopReason::Deferred => crate::types::DoneReason::Deferred,
        _ => crate::types::DoneReason::Stop,
    }
}

// ---------------------------------------------------------------------------
// AWS 默认凭据链（`@aws-sdk/credential-provider-node` 的等价物）
// ---------------------------------------------------------------------------

/// 读取 AWS 共享配置文件（INI 风格的极简解析）：返回指定 section 的键值。
///
/// 与 `@smithy/core` 的 `parseIni` 一致：`#`/`;` 注释、`key = value`，小节名区分大小写。
/// 键名统一转小写（调用方按 `aws_access_key_id` 这类小写名查找）。结果分两种：文件
/// 读不到（不存在、无权限、非 UTF-8 等）时返回 `None`；文件可读但没有该小节时返回
/// `Some(empty)`（空表），后者不是错误。
fn read_ini_section(path: &std::path::Path, section: &str) -> Option<IndexMap<String, String>> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut current: Option<String> = None;
    let mut values = IndexMap::new();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            current = Some(name.trim().to_string());
            continue;
        }
        if current.as_deref() != Some(section) {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        values.insert(key.trim().to_lowercase(), value.trim().to_string());
    }
    Some(values)
}

/// 从 env 取家目录（`HOME`）；空串视为缺失，返回 `None`。
fn home_dir(env: Option<&ProviderEnv>) -> Option<std::path::PathBuf> {
    get_provider_env_value("HOME", env)
        .filter(|home| !home.is_empty())
        .map(std::path::PathBuf::from)
}

/// `AWS_SHARED_CREDENTIALS_FILE` 或 `~/.aws/credentials`。
fn credentials_file_path(env: Option<&ProviderEnv>) -> Option<std::path::PathBuf> {
    if let Some(path) = get_provider_env_value("AWS_SHARED_CREDENTIALS_FILE", env) {
        return Some(std::path::PathBuf::from(path));
    }
    home_dir(env).map(|home| home.join(".aws").join("credentials"))
}

/// `AWS_CONFIG_FILE` 或 `~/.aws/config`。
fn config_file_path(env: Option<&ProviderEnv>) -> Option<std::path::PathBuf> {
    if let Some(path) = get_provider_env_value("AWS_CONFIG_FILE", env) {
        return Some(std::path::PathBuf::from(path));
    }
    home_dir(env).map(|home| home.join(".aws").join("config"))
}

/// `parseKnownFiles`：config 文件（`[profile x]` / `[default]`）与 credentials 文件合并，
/// credentials 文件里的同名键优先。
///
/// profile 为 `"default"` 时 config 小节名是 `[default]`，否则是 `[profile <name>]`；
/// credentials 文件统一用 `[<name>]`。两个文件都缺失时返回 `None`。
fn aws_profile_values(
    profile: &str,
    env: Option<&ProviderEnv>,
) -> Option<IndexMap<String, String>> {
    let mut merged: IndexMap<String, String> = IndexMap::new();
    if let Some(path) = config_file_path(env) {
        let section = if profile == "default" {
            "default".to_string()
        } else {
            format!("profile {profile}")
        };
        if let Some(values) = read_ini_section(&path, &section) {
            merged.extend(values);
        }
    }
    if let Some(path) = credentials_file_path(env)
        && let Some(values) = read_ini_section(&path, profile)
    {
        merged.extend(values);
    }
    if merged.is_empty() {
        None
    } else {
        Some(merged)
    }
}

/// 从共享配置里读 region（含 `source_profile` 的回退）。
///
/// 当前 profile 没有 region 时沿 `source_profile` 链向上找；最多 8 层，防止循环
/// 或病态配置导致无限递归。找不到返回 `None`。
fn profile_region(profile: Option<&str>, env: Option<&ProviderEnv>) -> Option<String> {
    let mut name = profile.unwrap_or("default").to_string();
    for _ in 0..8 {
        let values = aws_profile_values(&name, env);
        let region = values
            .as_ref()
            .and_then(|values| values.get("region"))
            .cloned()
            .filter(|region| !region.is_empty());
        if region.is_some() {
            return region;
        }
        let source = values
            .as_ref()
            .and_then(|values| values.get("source_profile"))
            .cloned()
            .filter(|source| !source.is_empty() && *source != name);
        name = source?;
    }
    None
}

/// 从 profile 的键值里取静态凭据：AK/SK 必须同时存在且非空，session token 可选。
fn static_profile_credentials(values: &IndexMap<String, String>) -> Option<BedrockCredentials> {
    let access_key_id = values.get("aws_access_key_id")?.clone();
    let secret_access_key = values.get("aws_secret_access_key")?.clone();
    if access_key_id.is_empty() || secret_access_key.is_empty() {
        return None;
    }
    Some(BedrockCredentials {
        access_key_id,
        secret_access_key,
        session_token: values
            .get("aws_session_token")
            .cloned()
            .filter(|token| !token.is_empty()),
    })
}

/// 默认凭据链：`fromIni` → `fromTokenFile` → ECS/IMDS → `Could not load credentials...`。
///
/// 顺序与 AWS SDK 的 Node 默认链一致，第一个成功的来源胜出：先看显式配置的静态
/// 凭据，再按 profile 解析（可能触发 AssumeRole/credential_process），然后环境里的
/// web identity，最后容器/EC2 元数据。全部失败才返回固定文案的错误。
async fn resolve_bedrock_credentials(
    config: &BedrockClientConfig,
    options: &BedrockOptions,
) -> Result<BedrockCredentials, BedrockFailure> {
    if let Some(credentials) = &config.credentials {
        return Ok(credentials.clone());
    }
    let profile = config.profile.clone().unwrap_or_else(|| "default".into());
    if let Some(credentials) =
        resolve_profile_credentials(&profile, config, options, &mut Vec::new()).await?
    {
        return Ok(credentials);
    }
    if let Some(credentials) = web_identity_env_credentials(config, options).await? {
        return Ok(credentials);
    }
    if let Some(credentials) = remote_provider_credentials(config, options).await? {
        return Ok(credentials);
    }
    Err(BedrockFailure::named(
        "Error",
        "Could not load credentials from any providers",
    ))
}

/// `fromIni` 的 `resolveProfileData`（role → static → web identity → process → SSO/未知）。
///
/// 分支顺序：
///
/// 1. `role_arn` 且 `source_profile`/`credential_source` **恰好给了一个**：先解析源
///    凭据，再 `AssumeRole`；`source_profile` 递归时用 `visited` 检测环并报错。
///    两者都给或都不给时不进入该分支，继续后面的静态凭据判断。
/// 2. 静态 AK/SK。
/// 3. `web_identity_token_file` + `role_arn`：读 token 后 `AssumeRoleWithWebIdentity`。
/// 4. `credential_process`：执行命令；失败是 `tryNextLink`，返回 `Ok(None)` 继续找。
/// 5. SSO/login profile 尚未移植，返回 `Ok(None)`。
///
/// ERROR：检测到 profile 环、token 文件读失败、STS 失败都会向上传播；普通“没有该
/// profile/该分支”返回 `Ok(None)`。
async fn resolve_profile_credentials(
    profile: &str,
    config: &BedrockClientConfig,
    options: &BedrockOptions,
    visited: &mut Vec<String>,
) -> Result<Option<BedrockCredentials>, BedrockFailure> {
    let env = options.base.env.as_ref();
    let Some(values) = aws_profile_values(profile, env) else {
        return Ok(None);
    };
    let role_arn = values
        .get("role_arn")
        .cloned()
        .filter(|value| !value.is_empty());
    let source_profile = values
        .get("source_profile")
        .cloned()
        .filter(|value| !value.is_empty());
    let credential_source = values
        .get("credential_source")
        .cloned()
        .filter(|value| !value.is_empty());
    if let Some(role_arn) = role_arn
        && (source_profile.is_some() != credential_source.is_some())
    {
        let source = match (&source_profile, &credential_source) {
            (Some(source_profile), _) => {
                if visited.iter().any(|name| name == source_profile) {
                    return Err(BedrockFailure::named(
                        "Error",
                        format!(
                            "Detected a cycle attempting to resolve credentials for profile {profile}. Profiles visited: {}",
                            visited.join(", ")
                        ),
                    ));
                }
                visited.push(source_profile.clone());
                let source = Box::pin(resolve_profile_credentials(
                    source_profile,
                    config,
                    options,
                    visited,
                ))
                .await?;
                visited.pop();
                let Some(source) = source else {
                    return Ok(None);
                };
                source
            }
            (_, Some(credential_source)) => {
                // credential_source 只支持三种来源；未知值直接报错（与 SDK 一致）。
                let source = match credential_source.as_str() {
                    "Environment" => get_configured_bedrock_credentials(env),
                    "Ec2InstanceMetadata" => Some(imds_credentials(options).await?),
                    "EcsContainer" => Some(ecs_container_credentials(options).await?),
                    other => {
                        return Err(BedrockFailure::named(
                            "Error",
                            format!("Unsupported credential_source {other} in profile {profile}"),
                        ));
                    }
                };
                let Some(source) = source else {
                    return Ok(None);
                };
                source
            }
            _ => return Ok(None),
        };
        let region = profile_region(Some(profile), env)
            .or_else(|| config.region.clone())
            .or_else(|| get_configured_bedrock_region(options))
            .unwrap_or_else(|| "us-east-1".to_string());
        let credentials = assume_role(
            &role_arn,
            values.get("role_session_name").map(String::as_str),
            values.get("external_id").map(String::as_str),
            values.get("duration_seconds").map(String::as_str),
            &region,
            &source,
            options,
        )
        .await?;
        return Ok(Some(credentials));
    }
    if let Some(credentials) = static_profile_credentials(&values) {
        return Ok(Some(credentials));
    }
    if let (Some(token_file), Some(role_arn)) = (
        values
            .get("web_identity_token_file")
            .filter(|value| !value.is_empty()),
        values.get("role_arn").filter(|value| !value.is_empty()),
    ) {
        let token = std::fs::read_to_string(token_file).map_err(|error| {
            BedrockFailure::named(
                "Error",
                format!("Unable to read the web identity token file {token_file}: {error}"),
            )
        })?;
        let region = profile_region(Some(profile), env)
            .or_else(|| config.region.clone())
            .or_else(|| get_configured_bedrock_region(options))
            .unwrap_or_else(|| "us-east-1".to_string());
        let credentials = assume_role_with_web_identity(
            role_arn,
            values.get("role_session_name").map(String::as_str),
            token.trim(),
            &region,
            options,
        )
        .await?;
        return Ok(Some(credentials));
    }
    if let Some(command) = values
        .get("credential_process")
        .filter(|value| !value.is_empty())
    {
        // `fromProcess` 的失败是 `tryNextLink: true`：继续走后面的 provider。
        return Ok(process_credentials(profile, command).await.ok());
    }
    // SSO / login profile：凭据链的 SSO 分支未移植（见 remainingGaps）。
    Ok(None)
}

/// `AssumeRole`（STS query 协议 + SigV4）。
///
/// 用源凭据给表单请求签名；`RoleSessionName` 缺省是 `aws-sdk-js-<毫秒时间戳>`，
/// `DurationSeconds` 缺省 3600（无法解析时也用默认值）。成功/失败都交给
/// `parse_sts_credentials` 解释 XML 响应。
#[allow(clippy::too_many_arguments)]
async fn assume_role(
    role_arn: &str,
    role_session_name: Option<&str>,
    external_id: Option<&str>,
    duration_seconds: Option<&str>,
    region: &str,
    source: &BedrockCredentials,
    options: &BedrockOptions,
) -> Result<BedrockCredentials, BedrockFailure> {
    let session_name = match role_session_name.filter(|value| !value.is_empty()) {
        Some(name) => name.to_string(),
        None => format!("aws-sdk-js-{}", now_unix_ms()),
    };
    // `parseInt(profile.duration_seconds || "3600", 10)`。
    let duration = duration_seconds
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(3600);
    let mut pairs: Vec<(&str, String)> = vec![
        ("Action", "AssumeRole".to_string()),
        ("Version", "2011-06-15".to_string()),
        ("RoleArn", role_arn.to_string()),
        ("RoleSessionName", session_name),
    ];
    if let Some(external_id) = external_id.filter(|value| !value.is_empty()) {
        pairs.push(("ExternalId", external_id.to_string()));
    }
    pairs.push(("DurationSeconds", duration.to_string()));
    let form = aws_query_encode(&pairs);
    let url = sts_endpoint(region, options);
    let host = url_host(&url);
    let headers = vec![
        (
            "content-type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        ),
        ("host".to_string(), host),
    ];
    let amz_date = amz_date_now();
    let (authorization, signed_headers) = sign_sigv4(&SigV4Request {
        method: "POST",
        path: "/",
        query: "",
        headers: &headers,
        body: form.as_bytes(),
        region,
        service: "sts",
        access_key_id: &source.access_key_id,
        secret_access_key: &source.secret_access_key,
        session_token: source.session_token.as_deref(),
        amz_date: &amz_date,
    });
    let mut headers = signed_headers;
    headers.push(("authorization".to_string(), authorization));
    let (status, body) = execute_aws_request(
        HttpRequest {
            method: "POST".into(),
            url,
            headers,
            body: Some(form.into_bytes()),
            timeout_ms: None,
        },
        options,
    )
    .await?;
    parse_sts_credentials(status, &body)
}

/// `AssumeRoleWithWebIdentity`（不签名）。
///
/// 该 STS 操作靠 `WebIdentityToken` 本身鉴权，因此请求不带 SigV4；响应同样交给
/// `parse_sts_credentials`。
async fn assume_role_with_web_identity(
    role_arn: &str,
    role_session_name: Option<&str>,
    token: &str,
    region: &str,
    options: &BedrockOptions,
) -> Result<BedrockCredentials, BedrockFailure> {
    let session_name = match role_session_name.filter(|value| !value.is_empty()) {
        Some(name) => name.to_string(),
        None => format!("aws-sdk-js-{}", now_unix_ms()),
    };
    let pairs: Vec<(&str, String)> = vec![
        ("Action", "AssumeRoleWithWebIdentity".to_string()),
        ("Version", "2011-06-15".to_string()),
        ("RoleArn", role_arn.to_string()),
        ("RoleSessionName", session_name),
        ("WebIdentityToken", token.to_string()),
    ];
    let form = aws_query_encode(&pairs);
    let (status, body) = execute_aws_request(
        HttpRequest {
            method: "POST".into(),
            url: sts_endpoint(region, options),
            headers: vec![(
                "content-type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            )],
            body: Some(form.into_bytes()),
            timeout_ms: None,
        },
        options,
    )
    .await?;
    parse_sts_credentials(status, &body)
}

/// `AWS_ROLE_ARN` + `AWS_WEB_IDENTITY_TOKEN_FILE`（`fromTokenFile`）。
///
/// 两个变量缺一就返回 `Ok(None)`；token 文件读失败是硬错误。region 依次取
/// config.region → profile → 显式配置 → `us-east-1`。
async fn web_identity_env_credentials(
    config: &BedrockClientConfig,
    options: &BedrockOptions,
) -> Result<Option<BedrockCredentials>, BedrockFailure> {
    let env = options.base.env.as_ref();
    let Some(role_arn) = get_provider_env_value("AWS_ROLE_ARN", env) else {
        return Ok(None);
    };
    let Some(token_file) = get_provider_env_value("AWS_WEB_IDENTITY_TOKEN_FILE", env) else {
        return Ok(None);
    };
    let token = std::fs::read_to_string(&token_file).map_err(|error| {
        BedrockFailure::named(
            "Error",
            format!("Unable to read the web identity token file {token_file}: {error}"),
        )
    })?;
    let region = config
        .region
        .clone()
        .or_else(|| profile_region(config.profile.as_deref(), env))
        .or_else(|| get_configured_bedrock_region(options))
        .unwrap_or_else(|| "us-east-1".to_string());
    let session_name = get_provider_env_value("AWS_ROLE_SESSION_NAME", env);
    let credentials = assume_role_with_web_identity(
        &role_arn,
        session_name.as_deref(),
        token.trim(),
        &region,
        options,
    )
    .await?;
    Ok(Some(credentials))
}

/// 容器（ECS/`AWS_CONTAINER_CREDENTIALS_*`）与 EC2 IMDS。
///
/// 设了任一个容器凭据 URI 就走 ECS；`AWS_EC2_METADATA_DISABLED` 只要不是字面量
/// `"false"` 就完全跳过 IMDS（返回 `Ok(None)`，而不是错误）；否则尝试 IMDS。
async fn remote_provider_credentials(
    _config: &BedrockClientConfig,
    options: &BedrockOptions,
) -> Result<Option<BedrockCredentials>, BedrockFailure> {
    let env = options.base.env.as_ref();
    let relative = get_provider_env_value("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI", env);
    let full = get_provider_env_value("AWS_CONTAINER_CREDENTIALS_FULL_URI", env);
    if relative.is_some() || full.is_some() {
        return Ok(Some(ecs_container_credentials(options).await?));
    }
    if get_provider_env_value("AWS_EC2_METADATA_DISABLED", env)
        .is_some_and(|value| value != "false")
    {
        return Ok(None);
    }
    Ok(Some(imds_credentials(options).await?))
}

/// `fromContainerMetadata`：relative URI 固定指向 `169.254.170.2`，full URI 只允许 loopback。
///
/// full URI 的主机被限制为 `localhost`/`127.0.0.1`、协议限制为 http/https，不满足
/// 直接报错；relative URI 则固定拼到 `169.254.170.2`。认证 token 支持直接值或 token
/// 文件（二选一，直接值优先）。请求超时固定 1000ms。
async fn ecs_container_credentials(
    options: &BedrockOptions,
) -> Result<BedrockCredentials, BedrockFailure> {
    let env = options.base.env.as_ref();
    let url = if let Some(relative) =
        get_provider_env_value("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI", env)
    {
        format!("http://169.254.170.2{relative}")
    } else if let Some(full) = get_provider_env_value("AWS_CONTAINER_CREDENTIALS_FULL_URI", env) {
        let parsed = url::Url::parse(&full).map_err(|_| {
            BedrockFailure::named(
                "Error",
                format!("{full} is not a valid container metadata service URL"),
            )
        })?;
        let host = parsed.host_str().unwrap_or_default();
        if host != "localhost" && host != "127.0.0.1" {
            return Err(BedrockFailure::named(
                "Error",
                format!("{host} is not a valid container metadata service hostname"),
            ));
        }
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return Err(BedrockFailure::named(
                "Error",
                format!(
                    "{} is not a valid container metadata service protocol",
                    parsed.scheme()
                ),
            ));
        }
        full
    } else {
        return Err(BedrockFailure::named(
            "Error",
            "The container metadata credential provider cannot be used unless the AWS_CONTAINER_CREDENTIALS_RELATIVE_URI or AWS_CONTAINER_CREDENTIALS_FULL_URI environment variable is set",
        ));
    };
    let mut headers = Vec::new();
    if let Some(token) = get_provider_env_value("AWS_CONTAINER_AUTHORIZATION_TOKEN", env) {
        headers.push(("authorization".to_string(), token));
    } else if let Some(path) = get_provider_env_value("AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE", env)
    {
        let token = std::fs::read_to_string(&path).map_err(|error| {
            BedrockFailure::named(
                "Error",
                format!("Unable to read the container authorization token file {path}: {error}"),
            )
        })?;
        headers.push(("authorization".to_string(), token.trim().to_string()));
    }
    let (status, body) = execute_aws_request(
        HttpRequest {
            method: "GET".into(),
            url,
            headers,
            body: None,
            timeout_ms: Some(1000),
        },
        options,
    )
    .await?;
    parse_imds_credentials(status, &body)
}

/// `fromInstanceMetadata`（IMDSv2 token，失败时退回 IMDSv1）。
///
/// 步骤：PUT `/latest/api/token` 取 token（失败或非 2xx 就退回 v1，不报错）；GET
/// 角色名列表，取第一行；再 GET 该角色的凭据 JSON，交给
/// `parse_imds_credentials`。所有请求超时 1000ms，端点可用
/// `AWS_EC2_METADATA_SERVICE_ENDPOINT` 覆盖。
async fn imds_credentials(options: &BedrockOptions) -> Result<BedrockCredentials, BedrockFailure> {
    let env = options.base.env.as_ref();
    let endpoint = get_provider_env_value("AWS_EC2_METADATA_SERVICE_ENDPOINT", env)
        .unwrap_or_else(|| "http://169.254.169.254".to_string());
    let base = endpoint.trim_end_matches('/').to_string();
    // IMDSv2：先取 token；任何失败都退回 IMDSv1（不阻断）。
    let token = match execute_aws_request(
        HttpRequest {
            method: "PUT".into(),
            url: format!("{base}/latest/api/token"),
            headers: vec![(
                "x-aws-ec2-metadata-token-ttl-seconds".to_string(),
                "21600".to_string(),
            )],
            body: None,
            timeout_ms: Some(1000),
        },
        options,
    )
    .await
    {
        Ok((status, body)) if (200..300).contains(&status) => Some(body.trim().to_string()),
        _ => None,
    };
    let mut headers: Vec<(String, String)> = Vec::new();
    if let Some(token) = token.filter(|token| !token.is_empty()) {
        headers.push(("x-aws-ec2-metadata-token".to_string(), token));
    }
    let (status, body) = execute_aws_request(
        HttpRequest {
            method: "GET".into(),
            url: format!("{base}/latest/meta-data/iam/security-credentials/"),
            headers: headers.clone(),
            body: None,
            timeout_ms: Some(1000),
        },
        options,
    )
    .await?;
    if !(200..300).contains(&status) {
        return Err(BedrockFailure::named(
            "Error",
            "Error response received from instance metadata service",
        ));
    }
    let role = body.trim().lines().next().unwrap_or("").trim().to_string();
    let (status, body) = execute_aws_request(
        HttpRequest {
            method: "GET".into(),
            url: format!("{base}/latest/meta-data/iam/security-credentials/{role}"),
            headers,
            body: None,
            timeout_ms: Some(1000),
        },
        options,
    )
    .await?;
    parse_imds_credentials(status, &body)
}

/// 解析 IMDS/ECS 凭据 JSON。
///
/// 非 2xx → 固定错误文案。JSON 解析失败或 AK/SK 任一为空 → 错误；`Token` 作为
/// session token（空串视为缺失）。
fn parse_imds_credentials(status: u16, body: &str) -> Result<BedrockCredentials, BedrockFailure> {
    if !(200..300).contains(&status) {
        return Err(BedrockFailure::named(
            "Error",
            "Error response received from instance metadata service",
        ));
    }
    let parsed: Value = serde_json::from_str(body).map_err(|error| {
        BedrockFailure::named(
            "Error",
            format!("Invalid response received from instance metadata service. {error}"),
        )
    })?;
    let access_key_id = parsed
        .get("AccessKeyId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let secret_access_key = parsed
        .get("SecretAccessKey")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if access_key_id.is_empty() || secret_access_key.is_empty() {
        return Err(BedrockFailure::named(
            "Error",
            "Invalid response received from instance metadata service.",
        ));
    }
    Ok(BedrockCredentials {
        access_key_id: access_key_id.to_string(),
        secret_access_key: secret_access_key.to_string(),
        session_token: parsed
            .get("Token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(str::to_string),
    })
}

/// 解析 STS 的 XML 响应。
///
/// 非 2xx：用 `<Code>`/`<Message>` 构造 [`BedrockFailure::Named`] 并附 status 与原始
/// body；2xx 时读 `<AccessKeyId>`/`<SecretAccessKey>`/`<SessionToken>`，缺 AK/SK 报错。
fn parse_sts_credentials(status: u16, body: &str) -> Result<BedrockCredentials, BedrockFailure> {
    if !(200..300).contains(&status) {
        let name = xml_tag(body, "Code").unwrap_or_else(|| "Error".to_string());
        let message = xml_tag(body, "Message").unwrap_or_else(|| body.to_string());
        return Err(BedrockFailure::Named {
            name,
            message,
            metadata: Some(BedrockErrorMetadata {
                http_status_code: Some(status as f64),
                request_id: None,
            }),
            body: Some(body.to_string()),
        });
    }
    let access_key_id = xml_tag(body, "AccessKeyId").unwrap_or_default();
    let secret_access_key = xml_tag(body, "SecretAccessKey").unwrap_or_default();
    if access_key_id.is_empty() || secret_access_key.is_empty() {
        return Err(BedrockFailure::named(
            "Error",
            "STS response did not contain credentials",
        ));
    }
    Ok(BedrockCredentials {
        access_key_id,
        secret_access_key,
        session_token: xml_tag(body, "SessionToken").filter(|token| !token.is_empty()),
    })
}

/// 取 XML 里第一个 `<tag>...</tag>` 的文本。
///
/// 只用于 STS 的固定形状响应，不处理命名空间、CDATA 或实体转义；标签不存在返回
/// `None`。
fn xml_tag(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// `credential_process`（`child_process.exec` 的等价物：`sh -c`）。
///
/// 标准输出必须是 JSON：`Version` 必须为 1，`AccessKeyId`/`SecretAccessKey` 非空。
/// 非零退出码、非 JSON、版本不符、凭据缺失都返回错误；注意调用方
/// `resolve_profile_credentials` 会吞掉这些错误并继续下一个 provider（`tryNextLink`）。
async fn process_credentials(
    profile: &str,
    command: &str,
) -> Result<BedrockCredentials, BedrockFailure> {
    let output = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .await
        .map_err(|error| BedrockFailure::named("Error", error.to_string()))?;
    if !output.status.success() {
        return Err(BedrockFailure::named(
            "Error",
            format!("Command failed: {command}"),
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim()).map_err(|_| {
        BedrockFailure::named(
            "Error",
            format!("Profile {profile} credential_process returned invalid JSON."),
        )
    })?;
    let version = parsed.get("Version").and_then(Value::as_i64);
    if version != Some(1) {
        return Err(BedrockFailure::named(
            "Error",
            format!("Profile {profile} credential_process did not return Version 1."),
        ));
    }
    let access_key_id = parsed
        .get("AccessKeyId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let secret_access_key = parsed
        .get("SecretAccessKey")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if access_key_id.is_empty() || secret_access_key.is_empty() {
        return Err(BedrockFailure::named(
            "Error",
            format!("Profile {profile} credential_process returned invalid credentials."),
        ));
    }
    Ok(BedrockCredentials {
        access_key_id: access_key_id.to_string(),
        secret_access_key: secret_access_key.to_string(),
        session_token: parsed
            .get("SessionToken")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(str::to_string),
    })
}

/// STS 端点：`AWS_ENDPOINT_URL_STS` 优先；`AWS_STS_REGIONAL_ENDPOINTS=legacy` 用全局
/// 端点；否则按 region 拼 `https://sts.<region>.amazonaws.com/`。
fn sts_endpoint(region: &str, options: &BedrockOptions) -> String {
    if let Some(endpoint) =
        get_provider_env_value("AWS_ENDPOINT_URL_STS", options.base.env.as_ref())
    {
        return endpoint;
    }
    if get_provider_env_value("AWS_STS_REGIONAL_ENDPOINTS", options.base.env.as_ref())
        .is_some_and(|value| value == "legacy")
    {
        return "https://sts.amazonaws.com/".to_string();
    }
    format!("https://sts.{region}.amazonaws.com/")
}

/// 从 URL 取 `host[:port]`，用于 STS 请求的 `host` 头；解析失败返回空串。
fn url_host(url: &str) -> String {
    let Ok(parsed) = url::Url::parse(url) else {
        return String::new();
    };
    match (parsed.host_str(), parsed.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_string(),
        (None, _) => String::new(),
    }
}

/// 按 AWS query 协议编码表单：键和值都用 `AWS_URI_ENCODE_SET`，用 `&` 连接。
/// 空格编码为 `%20`（不是 `+`）。
fn aws_query_encode(pairs: &[(&str, String)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                percent_encoding::utf8_percent_encode(key, AWS_URI_ENCODE_SET),
                percent_encoding::utf8_percent_encode(value, AWS_URI_ENCODE_SET)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// 用注入的 [`HttpClient`] 执行一个非流式 AWS 请求，返回 `(status, body_text)`。
///
/// 读取 body 走 `read_body_text`（UTF-8，非法字节替换）；传输/取消错误转成
/// [`BedrockFailure`]。超时由调用方填在 [`HttpRequest::timeout_ms`] 里。
async fn execute_aws_request(
    request: HttpRequest,
    options: &BedrockOptions,
) -> Result<(u16, String), BedrockFailure> {
    let client = resolve_client(&options.base)?;
    let response = client
        .execute(request, options.base.signal.as_ref())
        .await
        .map_err(|error| http_failure(error, options.base.signal.as_ref()))?;
    let status = response.response.status;
    let body =
        crate::api::kiana_messages::read_body_text(response.body, options.base.signal.as_ref())
            .await
            .map_err(|error| BedrockFailure::named("Error", error.message))?;
    Ok((status, body))
}

/// 组装 ConverseStream 的 HTTP 请求（含自定义头与鉴权）。
///
/// 步骤：
///
/// 1. 解析 region（`config.region` → profile 共享配置；都没有则报 `"Region is missing"`）；
///    `config.endpoint` 有值时用它，否则拼标准
///    `https://bedrock-runtime.<region>.amazonaws.com`。
/// 2. URL 用命令输入里的 `modelId`（`on_payload` 可能改过）拼
///    `/model/<百分号编码>/converse-stream`。
/// 3. 头：content-type/accept/host + 调用方自定义头（跳过保留头）+ user-agent。
/// 4. body：把 `modelId` 从命令输入里删掉后序列化（它是 httpLabel，只出现在 URL）。
/// 5. 鉴权：有 bearer token 就发 `Authorization: Bearer ...`；否则解析凭据链并用
///    SigV4 签名。签名覆盖最终的 URL path、头和 body。
///
/// 每次重试都会重新调用本函数，因此 `x-amz-date` 与签名都是新的。
async fn build_http_request(
    model: &Model,
    config: &BedrockClientConfig,
    command_input: &Value,
    options: &BedrockOptions,
    _observed: &mut bool,
) -> Result<HttpRequest, BedrockFailure> {
    // region 是 URL 与签名 scope 的必需项；config 没给时从 profile 共享配置补。
    let region = config
        .region
        .clone()
        .or_else(|| profile_region(config.profile.as_deref(), options.base.env.as_ref()))
        .ok_or_else(|| BedrockFailure::named("Error", "Region is missing"))?;
    let base = config
        .endpoint
        .clone()
        .unwrap_or_else(|| format!("https://bedrock-runtime.{region}.amazonaws.com"));
    // SDK 用命令输入里的 `modelId` 拼 URL（`onPayload` 可以改写它）。
    let request_model_id = command_input
        .get("modelId")
        .and_then(Value::as_str)
        .unwrap_or(&model.id);
    let encoded_model =
        percent_encoding::utf8_percent_encode(request_model_id, AWS_URI_ENCODE_SET).to_string();
    let url = format!(
        "{}/model/{encoded_model}/converse-stream",
        base.trim_end_matches('/')
    );
    let boundary =
        url::Url::parse(&url).map_err(|error| BedrockFailure::named("Error", error.to_string()))?;
    let path = boundary.path().to_string();
    let host = match (boundary.host_str(), boundary.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_string(),
        (None, _) => String::new(),
    };

    let mut headers: Vec<(String, String)> = vec![
        ("content-type".into(), "application/json".into()),
        ("accept".into(), "application/vnd.amazon.eventstream".into()),
        ("host".into(), host.clone()),
    ];
    // 基础头 + 调用方自定义头（保留头在 merge_custom_headers 里过滤）+ UA。
    if let Some(custom_headers) = provider_headers_to_record(&[options.base.headers.as_ref()]) {
        merge_custom_headers(&mut headers, &custom_headers);
    }
    // User-Agent（SDK 的 customUserAgent 会附加到自己的 UA 上；Rust 侧直接使用它）。
    match &config.custom_user_agent {
        BedrockUserAgent::Custom(custom) => {
            headers.push(("user-agent".into(), custom.clone()));
        }
        BedrockUserAgent::Short => headers.push(("user-agent".into(), "kiana".into())),
        BedrockUserAgent::Detailed => {
            let version = bedrock_user_agent_version(options.base.identity.as_ref());
            headers.push(("user-agent".into(), format!("kiana/{version}")));
        }
    }

    // `modelId` 是 httpLabel：SDK 只把它编进 URL，不写进 JSON body。
    let mut body_input = command_input.clone();
    if let Some(object) = body_input.as_object_mut() {
        object.shift_remove("modelId");
    }
    let body = kiana_json::js_object::stringify(&body_input).into_bytes();

    // bearer 模式不签名；否则解析凭据链并用 SigV4 覆盖最终请求。
    if let Some(token) = &config.token {
        headers.push(("authorization".into(), format!("Bearer {token}")));
    } else {
        let credentials = resolve_bedrock_credentials(config, options).await?;
        let service = "bedrock";
        let amz_date = amz_date_now();
        let signature_input = SigV4Request {
            method: "POST",
            path: &path,
            query: "",
            headers: &headers,
            body: &body,
            region: &region,
            service,
            access_key_id: &credentials.access_key_id,
            secret_access_key: &credentials.secret_access_key,
            session_token: credentials.session_token.as_deref(),
            amz_date: &amz_date,
        };
        let (authorization, signed_headers) = sign_sigv4(&signature_input);
        headers = signed_headers;
        headers.push(("authorization".into(), authorization));
    }

    Ok(HttpRequest {
        method: "POST".into(),
        url,
        headers,
        body: Some(body),
        timeout_ms: None,
    })
}

/// SigV4 的 URI 编码集合：保留 unreserved（`A-Za-z0-9-._~`）。
const AWS_URI_ENCODE_SET: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// 把调用方头写进请求（大小写不敏感地覆盖同名头；跳过 SigV4 保留头）。
///
/// 对应 TS 的 `kiana-ai-custom-headers` build-step 中间件。
pub fn merge_custom_headers(
    headers: &mut Vec<(String, String)>,
    custom_headers: &IndexMap<String, String>,
) {
    for (name, value) in custom_headers {
        if is_reserved_header(name) {
            continue;
        }
        headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
        headers.push((name.clone(), value.clone()));
    }
}

/// `isReservedHeader(key)`。
pub fn is_reserved_header(key: &str) -> bool {
    let lower = key.to_lowercase();
    lower.starts_with("x-amz-") || lower == "authorization" || lower == "host"
}

/// 把 HTTP 传输错误转成失败形态：取消令牌已触发时是 Aborted，否则是 Named
/// `"Error"`。
///
/// 注意 `"Error"` 本身不在 `is_retryable_failure` 的 TRANSIENT 名单里；发送阶段的
/// 传输失败是由 `run_stream` 的发送分支单独判断是否重试的，读流阶段失败不会重试。
fn http_failure(
    error: HttpError,
    signal: Option<&tokio_util::sync::CancellationToken>,
) -> BedrockFailure {
    if signal.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
        BedrockFailure::Aborted(error.message)
    } else {
        BedrockFailure::named("Error", error.message)
    }
}

/// `options.fetch` 或默认 reqwest 实现。
///
/// 默认实现构造失败时转成 [`BedrockFailure::Named`]，而不是 panic。
fn resolve_client(options: &StreamOptions) -> Result<Arc<dyn HttpClient>, BedrockFailure> {
    if let Some(fetch) = &options.fetch {
        return Ok(fetch.clone());
    }
    crate::http::reqwest_client::ReqwestHttpClient::new()
        .map(|client| client.shared())
        .map_err(|error| BedrockFailure::named("Error", error.message))
}

/// 读取下一个字节块（取消令牌触发时返回中止错误）。
async fn next_chunk(
    body: &mut ByteStream,
    signal: Option<&tokio_util::sync::CancellationToken>,
) -> Result<Option<Vec<u8>>, HttpError> {
    crate::api::kiana_messages::next_chunk(body, signal).await
}

/// `"streamSimple"`：按模型能力调整 thinking 预算。
///
/// 在 [`stream`] 之上做预处理：
///
/// - 无 `reasoning`：原样转发；
/// - Claude 且支持自适应：透传档位与预算；
/// - Claude 且非自适应：先用 `adjust_max_tokens_for_thinking` 按思考预算扩大
///   `maxTokens`，再按上下文裁剪；该档位的预算夹到 `max_tokens - 1024`（至少给答案留
///   1024 token）。xhigh/max 在这里经 `clamp_reasoning` 归一到 high；
/// - 其它模型：透传档位与预算。
///
/// 返回值目前总是 `Ok`（没有同步失败分支）；保留 `Result` 是为了对齐 `api/mod.rs`
/// 的适配器同步错误契约。Bedrock 的凭据缺失在请求阶段才暴露，因此会表现为流上的
/// `error` 事件，而不是这里的 `Err`。
pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
) -> Result<AssistantMessageEventStream, AiError> {
    let base = build_base_options(model, context, Some(&options), None);
    let tool_choice = options
        .tool_choice
        .map(|choice| serde_json::to_value(choice).unwrap_or(Value::Null));
    let Some(reasoning) = options.reasoning else {
        return Ok(stream(
            model,
            context,
            BedrockOptions {
                base,
                tool_choice,
                ..Default::default()
            },
        ));
    };

    if is_anthropic_claude_model(model) {
        // Claude 需要协调思考预算与 maxTokens；其它模型直接透传选项。
        if supports_adaptive_thinking(&model.id, Some(&model.name)) {
            return Ok(stream(
                model,
                context,
                BedrockOptions {
                    base,
                    tool_choice,
                    reasoning: Some(reasoning),
                    thinking_budgets: options.thinking_budgets.clone(),
                    ..Default::default()
                },
            ));
        }
        // 顺序：先按思考预算扩大上限，再按上下文窗口裁剪。
        let adjusted = adjust_max_tokens_for_thinking(
            base.max_tokens,
            model.max_tokens.unwrap_or(0),
            reasoning,
            options.thinking_budgets.as_ref(),
        );
        let max_tokens = clamp_max_tokens_to_context(model, context, adjusted.max_tokens);
        let mut budgets = options.thinking_budgets.clone().unwrap_or_default();
        let level = clamp_reasoning(Some(reasoning)).unwrap_or(ThinkingLevel::High);
        // 至少给答案留 1024 token，避免思考预算吃满整个上限。
        let budget = adjusted
            .thinking_budget
            .min(max_tokens.saturating_sub(1024));
        match level {
            ThinkingLevel::Minimal => budgets.minimal = Some(budget),
            ThinkingLevel::Low => budgets.low = Some(budget),
            ThinkingLevel::Medium => budgets.medium = Some(budget),
            ThinkingLevel::High | ThinkingLevel::Xhigh | ThinkingLevel::Max => {
                budgets.high = Some(budget)
            }
        }
        let mut adjusted_base = base;
        adjusted_base.max_tokens = Some(max_tokens);
        return Ok(stream(
            model,
            context,
            BedrockOptions {
                base: adjusted_base,
                tool_choice,
                reasoning: Some(reasoning),
                thinking_budgets: Some(budgets),
                ..Default::default()
            },
        ));
    }

    Ok(stream(
        model,
        context,
        BedrockOptions {
            base,
            tool_choice,
            reasoning: Some(reasoning),
            thinking_budgets: options.thinking_budgets.clone(),
            ..Default::default()
        },
    ))
}

/// `bedrockModuleOverride`：`setBedrockProviderModule` 注册的静态模块。
///
/// 用 `Mutex` 而不是 `OnceLock`：允许后设置覆盖先设置。锁中毒时取回内部值继续
/// （`unwrap_or_else(into_inner)`），不因别的线程 panic 而失效。
static BEDROCK_MODULE_OVERRIDE: std::sync::Mutex<Option<Arc<dyn ProviderStreams>>> =
    std::sync::Mutex::new(None);

/// `setBedrockProviderModule(module)`：后设置覆盖先设置，影响后续所有
/// [`bedrock_converse_stream_api`] 调用（TS `bedrock-converse-stream.lazy.ts`）。
///
/// 这是 Bun 单文件构建的打包器技巧在 Rust 里的等价钩子：静态链接本来可以直接用
/// [`bedrock_converse_stream_api`]，保留它只为对齐 TS 的公开语义。
pub fn set_bedrock_provider_module(module: Arc<dyn ProviderStreams>) {
    *BEDROCK_MODULE_OVERRIDE
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(module);
}

/// `bedrock-converse-stream.lazy.ts`。
///
/// 有注册的覆盖模块就返回它，否则返回一个新的 `BedrockProvider`。未注册覆盖时每次
/// 调用都会新建轻量适配器（无内部状态），因此返回的 `Arc` 不保证指针相等。
pub fn bedrock_converse_stream_api() -> Arc<dyn ProviderStreams> {
    if let Some(module) = BEDROCK_MODULE_OVERRIDE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
    {
        return module;
    }
    Arc::new(BedrockProvider)
}

/// 默认的 [`ProviderStreams`] 实现：把冻结签名的 trait 方法转发到本文件的公开入口。
struct BedrockProvider;

impl ProviderStreams for BedrockProvider {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        stream(model, context, bedrock_options_from_stream_options(options))
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        // trait 不能返回 Err：同步错误统一映射成流上的 error 事件。
        match stream_simple(model, context, options) {
            Ok(stream) => stream,
            Err(error) => lazy_stream_error(model, error),
        }
    }
}

/// [`StreamOptions`] → [`BedrockOptions`]：API 专属字段从 `extra` 里按 TS 字段名取。
///
/// 读取的键：`region`、`profile`、`toolChoice`、`reasoning`、`thinkingBudgets`、
/// `interleavedThinking`、`thinkingDisplay`、`requestMetadata`、`bearerToken`。
/// 类型不符的键会被忽略（`as_str`/反序列化失败即跳过）；`requestMetadata` 只保留
/// 字符串值。`extra` 被整体取走，`base` 里因此是空 `extra`；未知键不会报错。
pub fn bedrock_options_from_stream_options(mut options: StreamOptions) -> BedrockOptions {
    let extra = std::mem::take(&mut options.extra);
    let string = |key: &str| extra.get(key).and_then(Value::as_str).map(str::to_string);
    let reasoning = extra
        .get("reasoning")
        .and_then(|value| serde_json::from_value::<ThinkingLevel>(value.clone()).ok());
    let thinking_budgets = extra
        .get("thinkingBudgets")
        .and_then(|value| serde_json::from_value::<ThinkingBudgets>(value.clone()).ok());
    let request_metadata = extra.get("requestMetadata").and_then(|value| {
        value.as_object().map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string()))
                })
                .collect::<IndexMap<String, String>>()
        })
    });
    BedrockOptions {
        region: string("region"),
        profile: string("profile"),
        tool_choice: extra.get("toolChoice").cloned(),
        reasoning,
        thinking_budgets,
        interleaved_thinking: extra.get("interleavedThinking").and_then(Value::as_bool),
        thinking_display: string("thinkingDisplay"),
        request_metadata,
        bearer_token: string("bearerToken"),
        base: options,
    }
}

// 单元测试：用固定的 golden fixture 锁定 SigV4 签名与 Smithy 帧解码，并覆盖 CRC 拒绝、
// 增量分帧、异常/错误消息映射与保留头判定。fixture 来源见
// tests/fixtures/api-other/bedrock_golden.json。
#[cfg(test)]
mod tests {
    use super::*;

    fn encode_frame(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
        let mut header_bytes = Vec::new();
        for (name, value) in headers {
            header_bytes.push(name.len() as u8);
            header_bytes.extend_from_slice(name.as_bytes());
            header_bytes.push(7); // string
            header_bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
            header_bytes.extend_from_slice(value.as_bytes());
        }
        let total = 12 + header_bytes.len() + payload.len() + 4;
        let mut message = Vec::with_capacity(total);
        message.extend_from_slice(&(total as u32).to_be_bytes());
        message.extend_from_slice(&(header_bytes.len() as u32).to_be_bytes());
        let prelude_crc = crc32fast::hash(&message);
        message.extend_from_slice(&prelude_crc.to_be_bytes());
        message.extend_from_slice(&header_bytes);
        message.extend_from_slice(payload);
        let message_crc = crc32fast::hash(&message);
        message.extend_from_slice(&message_crc.to_be_bytes());
        message
    }

    #[test]
    fn sigv4_matches_the_node_reference() {
        let golden: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/api-other/bedrock_golden.json"
        ))
        .expect("fixture");
        let sigv4 = &golden["sigv4"];
        let string = |key: &str| sigv4[key].as_str().expect(key).to_string();
        let headers: Vec<(String, String)> = sigv4["headers"]
            .as_object()
            .expect("headers")
            .iter()
            .map(|(name, value)| (name.clone(), value.as_str().unwrap().to_string()))
            .collect();
        let body = string("body");
        let request = SigV4Request {
            method: "POST",
            path: &string("path"),
            query: &string("query"),
            headers: &headers,
            body: body.as_bytes(),
            region: &string("region"),
            service: &string("service"),
            access_key_id: &string("access_key_id"),
            secret_access_key: &string("secret_access_key"),
            session_token: Some(&string("session_token")),
            amz_date: &string("amz_date"),
        };
        let (authorization, sent) = sign_sigv4(&request);
        assert_eq!(authorization, string("authorization"));
        assert!(
            sent.iter()
                .any(|(name, value)| name == "x-amz-security-token" && value == "SESSIONTOKEN")
        );
        assert!(sent.iter().any(|(name, _)| name == "x-amz-date"));
    }

    #[test]
    fn decodes_a_smithy_generated_frame() {
        use base64::Engine as _;
        let golden: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/api-other/bedrock_golden.json"
        ))
        .expect("fixture");
        let stream = &golden["event_stream"];
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(stream["base64"].as_str().expect("base64"))
            .expect("decode");
        let mut decoder = EventStreamDecoder::new();
        let messages = decoder.push(&bytes).expect("frame");
        assert_eq!(messages.len(), 1);
        for (name, value) in stream["headers"].as_object().expect("headers") {
            assert_eq!(messages[0].header(name), value.as_str(), "header {name}");
        }
        assert_eq!(
            std::str::from_utf8(&messages[0].payload).expect("utf8"),
            stream["payload"].as_str().expect("payload")
        );
        let item = eventstream_message_to_item(&messages[0]).expect("item");
        let DecodedEventStreamItem::Item(item) = item else {
            panic!("expected event item");
        };
        assert_eq!(
            item,
            serde_json::json!({"messageStart": {"role": "assistant"}})
        );
    }

    #[test]
    fn decodes_event_stream_frames_incrementally() {
        let payload = br#"{"role":"assistant"}"#;
        let frame = encode_frame(
            &[(":message-type", "event"), (":event-type", "messageStart")],
            payload,
        );
        let mut decoder = EventStreamDecoder::new();
        // 逐字节喂入（验证跨块缓冲）。
        let mut messages = Vec::new();
        for byte in &frame {
            messages.extend(decoder.push(std::slice::from_ref(byte)).expect("decode"));
        }
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].header(":event-type"), Some("messageStart"));
        assert_eq!(messages[0].payload, payload);
        let item = eventstream_message_to_item(&messages[0]).expect("item");
        let DecodedEventStreamItem::Item(item) = item else {
            panic!("expected event item");
        };
        assert_eq!(
            item,
            serde_json::json!({"messageStart": {"role": "assistant"}})
        );
    }

    #[test]
    fn rejects_a_corrupted_message() {
        let mut frame = encode_frame(
            &[(":message-type", "event"), (":event-type", "chunk")],
            b"{}",
        );
        let last = frame.len() - 1;
        frame[last] ^= 0xff;
        let mut decoder = EventStreamDecoder::new();
        assert!(decoder.push(&frame).is_err());
    }

    #[test]
    fn maps_exception_and_error_frames() {
        let exception = encode_frame(
            &[
                (":message-type", "exception"),
                (":exception-type", "throttlingException"),
            ],
            br#"{"message":"Too many requests"}"#,
        );
        let mut decoder = EventStreamDecoder::new();
        let messages = decoder.push(&exception).expect("decode");
        let DecodedEventStreamItem::Exception(failure) =
            eventstream_message_to_item(&messages[0]).expect("item")
        else {
            panic!("expected exception");
        };
        // 真实 SDK 抛服务异常实例：PascalCase schema 名 + payload 里的 message。
        assert!(failure.is_service_exception());
        assert_eq!(failure.name(), Some("ThrottlingException"));
        assert_eq!(
            format_bedrock_error(&failure),
            "Throttling error: Too many requests"
        );

        let error = encode_frame(
            &[
                (":message-type", "error"),
                (":error-code", "ModelStreamErrorException"),
                (":error-message", "boom"),
            ],
            b"{}",
        );
        let messages = decoder.push(&error).expect("decode");
        let DecodedEventStreamItem::Error(failure) =
            eventstream_message_to_item(&messages[0]).expect("item")
        else {
            panic!("expected error");
        };
        assert_eq!(failure.name(), Some("ModelStreamErrorException"));
        assert_eq!(failure.message(), "boom");
        assert!(!failure.is_service_exception());
    }

    #[test]
    fn recognizes_x_amz_reserved_headers_case_insensitively() {
        assert!(is_reserved_header("X-Amz-Date"));
        assert!(is_reserved_header("authorization"));
        assert!(is_reserved_header("HOST"));
        assert!(!is_reserved_header("x-allowed"));
    }
}
