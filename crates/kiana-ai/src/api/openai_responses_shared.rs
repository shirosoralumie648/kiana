//! api/openai-responses-shared.ts。
//!
//! Responses API 的消息/工具转换与流事件处理，供 `openai-responses.ts` 与
//! `azure-openai-responses.ts` 共用（TS 侧同源）。
//!
//! 本文件另外承载三个 OpenAI 兼容适配器（completions / responses / azure）共用的
//! **传输层**：SSE 解析、HTTP 请求执行与 OpenAI SDK 风格的错误对象构造。TS 的这部分逻辑在
//! `openai` npm SDK 内部（`Stream.fromSSEResponse`、`APIClient.makeStatusError`），Rust 侧没有
//! SDK，因此在这里实现一次并逐条对齐 SDK 的可观察行为（错误文本、`[DONE]`、`data.error`、
//! 状态行格式），避免三个适配器各写一份。
//!
//! # 这个文件解决什么问题
//!
//! TS 侧把 Responses API 的三类知识集中在 `openai-responses-shared.ts`：会话记录如何变成
//! Responses 的输入项、工具声明如何变成 Responses 的工具、SSE 事件如何变成统一的
//! assistant 消息。OpenAI 与 Azure 两个适配器共用它，避免两家行为漂移；Rust 侧的 codex
//! 适配器也复用其中的转换与流处理。本文件是它的 Rust 对等物。
//!
//! Rust 侧没有 `openai` npm SDK，因此 TS 由 SDK 承担的以下行为也被吸收进来（三个适配器
//! 共用，而不是各写一份）：
//! - SSE 分帧与解码（SDK 的 `LineDecoder` + `SSEDecoder`）；
//! - 一次请求的执行、超时与重试，以及 SDK 风格的 `APIError` / `APIConnectionError` 文本；
//! - `buildURL` 的 query 合并与 RFC3986 序列化。
//!
//! # 三个清晰的边界
//!
//! 1. **纯转换**（无 IO）：[`convert_responses_messages`] 接收 [`TranscriptContext`] 与
//!    [`Model`] 并返回可直接写进请求体的 input 项数组；[`convert_responses_tools`] 负责
//!    工具声明的转换；[`get_responses_tool_support`] 只由选项推导工具支持级别。三者都不
//!    读网络、不读全局状态。
//! 2. **流处理**（消费已解码事件）：[`process_responses_stream`] 按事件的 `output_index`
//!    把增量聚合成 [`AssistantMessage`] 的内容块，并向 `AssistantMessageEventStream`
//!    推送 start/delta/end 事件。
//! 3. **传输层**（有 IO）：[`provider_event_stream`] 解码 SSE；`execute_request` 与
//!    `request_with_retry` 执行 HTTP、超时与重试；`status_failure` 等构造错误对象。
//!    这一层被 completions / responses / azure 三个适配器共用（codex 适配器自己管理
//!    SSE/WebSocket，只复用转换与流处理）。
//!
//! # 一次 Responses 请求的流程
//!
//! 1. 适配器（如 `api::openai_responses`）构造 `HttpRequest` 并调用 `request_with_retry`。
//! 2. 非 2xx 时读出响应体并构造 SDK 风格错误；2xx 时返回响应对象（body 仍未读取）。
//! 3. 适配器推送 `Start`、检查 204/205（`ensure_response_body`），把 body 交给
//!    [`provider_event_stream`]。
//! 4. [`process_responses_stream`] 消费事件直到 `response.completed` / `response.incomplete`；
//!    若流在终态事件前结束则报错。
//! 5. 失败时适配器用 `format_thrown_error` 把 `AiError` 还原成 TS 侧可见的错误文本。
//!
//! # 重要约束
//!
//! - **字节级对齐**：所有 JSON 都用 `kiana_json` 的 JS 语义输出（数字格式、键序）；文本
//!   字段写入前经 `sanitize_surrogates`。`tests/fixtures/api-openai/conversions.json`
//!   的黄金测试逐字节对照 TS 参考实现，改动字段名、顺序或默认值都会让它失败。
//! - **`output_index` 可以是缺失值**：流处理把它作为槽位键（`Option<i64>`），与 TS 的
//!   `Map<number, ...>` 在事件缺字段时的行为一致，不要跳过或补默认值。
//! - **超时只覆盖响应头之前**：`HttpRequest.timeout_ms` 被执行层取出后不再下传，详见
//!   `execute_request` 的注释。
//! - 鉴权、base URL、请求字段（reasoning / prompt cache / service tier）不在这里，属于
//!   各适配器文件。
//!
//! # 失败模式
//!
//! - SSE 的 JSON 非法 → `AiErrorKind::Parse`（SDK 文案）；`event: error` 或 `data.error`
//!   → 带 `detail` 的错误（`name = "APIError"`）。
//! - SSE body 读取失败 → `AiErrorKind::Transport` + `APIConnectionError`；请求期的连接/
//!   超时失败由重试层转换为 `AiErrorKind::Other`，只有文案保留 SDK 的
//!   "Connection error." / "Request timed out."。用户取消由重试层统一改写为
//!   "Request aborted"。
//! - 流在终态事件前结束，或 `stop_reason == ToolUse` 时仍有未完成的 tool call → `Other`。
//! - 转换期错误（非法 thinking signature、grammar 工具输入不是字符串等）直接返回 `Err`，
//!   由适配器转成流上的 error 事件，不会 panic。
//!
//! # 阅读顺序
//!
//! 先读「选项」与 [`convert_responses_messages`] 了解请求形状，再读
//! [`provider_event_stream`] 与 [`process_responses_stream`] 了解响应如何变成消息；
//! 传输层（HTTP / 错误 / header / URL）可以最后看，它只服务于适配器。

// `AiError` 的尺寸由 `ARCHITECTURE.md` §4.1 的公开错误契约固定（`Display` 必须等于 TS 的
// `error.message`），因此结果类型只按文档要求携带它。
#![allow(clippy::result_large_err)]
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use futures_core::Stream;
use indexmap::{IndexMap, IndexSet};
use serde_json::{Map, Value};

use crate::api::constrained_sampling::{
    GrammarToolInputJsonBuffer, append_grammar_tool_input_json_delta,
    close_grammar_tool_input_json, get_grammar_tool_input, get_json_schema_tool_parameters,
    resolve_grammar_constrained_sampling, resolve_json_schema_strict_sampling,
};
use crate::api::transform_messages::{NormalizeToolCallIdFn, transform_messages_with};
use crate::error::{AiError, AiErrorKind};
use crate::event_stream::AssistantMessageEventStream;
use crate::http::{ByteStream, HttpClient, HttpError, HttpRequest, HttpResponse, SharedHttpClient};
use crate::models::calculate_cost;
use crate::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, JsonObject, Model,
    ModelInputModality, StopReason, SystemMessage, TextContent, TextSignaturePhase,
    ThinkingContent, Tool, ToolCall, TranscriptContext, Usage, UsageCost,
};
use crate::utils::error_body::{ProviderErrorFields, ProviderErrorValue};
use crate::utils::hash::short_hash;
use crate::utils::provider_retry::{ProviderError, ProviderRetryOptions, retry_provider_request};
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::streaming_json::ToolCallArgumentsStream;
use crate::utils::text::{get_system_message_text, render_system_message_update};
use crate::utils::transcript::{
    TranscriptToolSupport, resolve_transcript, resolve_transcript_tools,
};

// =============================================================================
// JS 语义小工具
// =============================================================================

// 这些函数是 TS 表达式的最小等价层，不是通用工具：`truthy`、`String(value)`、
// `value[key]`、非空字符串判断都参与协议判定（例如 `data.error` 只有真值才算错误），
// 换成 Rust 的直觉写法（`as_str`、`is_some`、`!is_empty` 混用）会改变 null / 缺失 /
// 空串 / 0 的处理。

/// `JSON.stringify(value)`（无缩进，JS 数字格式与键序）。
pub(crate) fn js_stringify(value: &Value) -> String {
    kiana_json::js_object::stringify(value)
}

/// JS 的真值判断。
pub(crate) fn truthy(value: Option<&Value>) -> bool {
    kiana_json::js_string::is_truthy(value)
}

/// `value[key]`（仅对象）。
pub(crate) fn field<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_object().and_then(|object| object.get(key))
}

/// `typeof value === "string" && value.length > 0`。
pub(crate) fn non_empty_str(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

/// `String(value)`（`undefined` → `"undefined"`）。
pub(crate) fn js_string(value: Option<&Value>) -> String {
    kiana_json::js_string::js_string(value)
}

/// JS 的 `value || 0` 后的非负整数（Usage 字段）。
pub(crate) fn usage_count(value: Option<&Value>) -> u64 {
    let Some(number) = value.and_then(Value::as_f64) else {
        return 0;
    };
    if !number.is_finite() || number <= 0.0 {
        return 0;
    }
    number as u64
}

/// 把 JSON 对象构造为 `Value`（保持插入顺序）。
pub(crate) fn json_object(pairs: Vec<(&str, Value)>) -> Value {
    let mut map = Map::new();
    for (key, value) in pairs {
        map.insert(key.to_string(), value);
    }
    Value::Object(map)
}

// =============================================================================
// 传输层：SSE 解析（OpenAI SDK 的 LineDecoder + SSEDecoder）
// =============================================================================

/// 解析后的事件流：每个元素是 SSE `data` 行的 `JSON.parse` 结果。
///
/// `Err` 是终止性的（解码失败或服务端错误事件），消费方应停止迭代。本模块的
/// [`provider_event_stream`] 返回的流由后台任务填充，只支持单消费者顺序读取；该别名
/// 本身只要求 `Stream + Send`，codex 适配器就用自建的 `unfold` 流包装自己的
/// SSE/WebSocket，不经过后台任务。
pub type ProviderEventStream = Pin<Box<dyn Stream<Item = Result<Value, AiError>> + Send>>;

/// 错误消息（OpenAI SDK `Stream.fromSSEResponse`）。
pub(crate) const MALFORMED_SSE_JSON: &str =
    "Error reading response: malformed server-sent event JSON.";

/// SDK 请求超时的文案（`APIConnectionTimeoutError` 的默认 `message`）。
pub(crate) const REQUEST_TIMED_OUT_MESSAGE: &str = "Request timed out.";

/// SDK 的默认请求超时（`OpenAI.DEFAULT_TIMEOUT = 600000`，10 分钟）。
///
/// 适配器只在 `options.timeoutMs !== undefined` 时才把 `timeout` 传给 SDK，否则 SDK 用这个
/// 默认值；因此未显式给出 `timeoutMs` 时，等响应头同样受 10 分钟约束。
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 600_000;

/// SDK 把 fetch 的 rejection 改写成 `APIConnectionError` 后的默认文案。
pub(crate) const CONNECTION_ERROR_MESSAGE: &str = "Connection error.";

/// 响应没有 body 时迭代 SDK 流的错误（`Stream._iterSSEMessages`）。
pub(crate) const NO_RESPONSE_BODY_MESSAGE: &str =
    "Attempted to iterate over a response with no body";

/// `new URL(...)` 失败的 `TypeError.message`（Node/V8）。
pub(crate) const INVALID_URL_MESSAGE: &str = "Invalid URL";

/// `fetch` 的传输层失败文案（Node undici 对所有请求阶段失败都给这一句）。
const FETCH_FAILED_MESSAGE: &str = "fetch failed";

/// 响应体为 null 的状态码（WHATWG fetch 的 null body statuses 里能作为最终响应的两个 2xx）。
///
/// 101 不能被 fetch 当作最终响应、103 不是最终响应、304 不是 2xx（走状态错误路径），
/// 因此适配器只需要处理 204/205。
pub(crate) fn response_has_no_body(status: u16) -> bool {
    matches!(status, 204 | 205)
}

/// 流式响应在开始迭代前检查「SDK 的响应对象没有 body」。
///
/// TS 里 `Stream.fromSSEResponse` 返回的对象在被迭代时抛
/// [`NO_RESPONSE_BODY_MESSAGE`]；Rust 的 `HttpResponse.body` 是 `ByteStream`，无法区分
/// `null` 与空 body，因此按状态码判断（真实服务器上只有 204/205 会给出 null body；
/// 自定义 `HttpClient` 用空流模拟 `new Response(null)` 时无法区分）。
pub(crate) fn ensure_response_body(status: u16) -> Result<(), AiError> {
    if response_has_no_body(status) {
        return Err(AiError::new(AiErrorKind::Other, NO_RESPONSE_BODY_MESSAGE));
    }
    Ok(())
}

/// SSE 消息累加器（SDK 的 `SSEDecoder`）：空行触发一次「事件名 + data 文本」输出。
///
/// 状态：`event` 是当前 `event:` 字段（可缺省），`data` 收集所有 `data:` 行。
/// 与 WHATWG SSE 一致的部分：忽略 `:` 注释行、字段值去掉一个前导空格、多行 data 以
/// `\n` 连接；这里只关心 OpenAI 用到的 `event` / `data` 两种字段。空行且没有待发内容
/// 时返回 `None`，不产生空事件。
#[derive(Default)]
struct SseMessageDecoder {
    event: Option<String>,
    data: Vec<String>,
}

impl SseMessageDecoder {
    /// `SSEDecoder.decode(line)`。
    ///
    /// 返回 `Some((event, data))` 表示一条完整消息；`None` 表示该行只是累积或被忽略。
    fn decode(&mut self, line: &str) -> Option<(Option<String>, String)> {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            if self.event.is_none() && self.data.is_empty() {
                return None;
            }
            let message = (self.event.take(), self.data.join("\n"));
            self.data.clear();
            return Some(message);
        }
        if line.starts_with(':') {
            return None;
        }
        let (fieldname, value) = match line.find(':') {
            Some(index) => {
                let value = &line[index + 1..];
                (&line[..index], value.strip_prefix(' ').unwrap_or(value))
            }
            None => (line, ""),
        };
        if fieldname == "event" {
            self.event = Some(value.to_string());
        } else if fieldname == "data" {
            self.data.push(value.to_string());
        }
        None
    }

    /// 流在最后一个空行前结束时，补发进行中的事件。
    fn flush(&mut self) -> Option<(Option<String>, String)> {
        self.decode("")
    }
}

/// `LineDecoder`：按 `\n`、`\r`、`\r\n` 切行，跨 chunk 保留残行。
///
/// 不变量：
/// - `buffer` 只保存**未以换行结束**的尾部字节；最后一个换行之前的字节都已产出；
/// - `skip_leading_lf` 只在「块尾是孤立的 `\r`」时置位，表示下一个块开头的 `\n` 属于
///   同一个 `\r\n` 行尾，必须吞掉；否则会多出一条空行，进而提前分发 SSE 消息。
///
/// 输出行内的非 UTF-8 字节由 `from_utf8_lossy` 替换为 U+FFFD（`TextDecoder` 非 fatal
/// 模式的行为）。`flush` 为未以换行结束的最后一行补一个 `\n` 后再解码。
#[derive(Default)]
struct LineDecoder {
    buffer: Vec<u8>,
    skip_leading_lf: bool,
}

impl LineDecoder {
    fn decode(&mut self, mut chunk: &[u8]) -> Vec<String> {
        if chunk.is_empty() {
            return Vec::new();
        }
        if self.skip_leading_lf {
            self.skip_leading_lf = false;
            if chunk.first() == Some(&0x0a) {
                chunk = &chunk[1..];
            }
            if chunk.is_empty() {
                return Vec::new();
            }
        }
        self.buffer.extend_from_slice(chunk);
        let mut lines = Vec::new();
        let mut start = 0usize;
        let mut index = 0usize;
        while index < self.buffer.len() {
            let byte = self.buffer[index];
            if byte == 0x0a {
                lines.push(String::from_utf8_lossy(&self.buffer[start..index]).into_owned());
                index += 1;
                start = index;
            } else if byte == 0x0d {
                lines.push(String::from_utf8_lossy(&self.buffer[start..index]).into_owned());
                if index + 1 < self.buffer.len() {
                    index += if self.buffer[index + 1] == 0x0a { 2 } else { 1 };
                    start = index;
                } else {
                    // 行尾 CR 单独落在 chunk 末尾：下一 chunk 开头的 LF 属于同一个行尾。
                    index += 1;
                    start = index;
                    self.skip_leading_lf = true;
                }
            } else {
                index += 1;
            }
        }
        // 只保留未消费的尾部，避免长连接下缓冲区持续增长。
        if start > 0 {
            self.buffer.drain(..start);
        }
        lines
    }

    fn flush(&mut self) -> Vec<String> {
        self.skip_leading_lf = false;
        if self.buffer.is_empty() {
            return Vec::new();
        }
        let mut appended = std::mem::take(&mut self.buffer);
        appended.push(0x0a);
        self.decode(&appended)
    }
}

/// 把 SSE 字节流解析成 provider 事件流。
///
/// 语义对齐 `openai` SDK：
/// - `[DONE]` 结束（之后的字节不再读取）；
/// - `event: error` 或 `data` 上带真值 `error` 字段 → `APIError`（消息取自 error 对象，
///   原始 error 对象放进 [`AiError::detail`] 供 catch 复刻 `error.error`）；
/// - JSON 解析失败 → `SyntaxError("Error reading response: malformed server-sent event JSON.")`；
/// - `event` 名以 `thread.` 开头时产出 `{event, data}` 信封（其余事件直接产出 data）。
///
/// # 输入与生命周期
///
/// `body` 是尚未读取的响应体字节流。函数**立即返回**接收端，真正的解码在
/// `api::lazy::spawn_forward` 起的后台任务里进行（没有 Tokio runtime 时用专用线程 +
/// current-thread runtime 驱动）。因此调用方可以先拿到流，再开始消费。
///
/// # 解码顺序
///
/// 1. 首个块去掉 UTF-8 BOM（`EF BB BF`）；
/// 2. `LineDecoder` 按 `\n` / `\r` / `\r\n` 切行，跨块残行留在缓冲区；
/// 3. `SseMessageDecoder` 累加 `data:` 行，遇空行产出一条消息；
/// 4. 每条消息交给 `emit`：`[DONE]` 结束、错误事件结束、`thread.` 事件包信封；
/// 5. body 结束时先 flush 行缓冲与消息缓冲，补发最后一个没有空行结尾的事件。
///
/// # 取消
///
/// `signal` 只用于区分「用户取消」与「服务端/网络错误」：body 读取返回
/// `reqwest_client::ABORTED_MESSAGE` 且 signal 已取消时静默结束迭代（SDK 的
/// `_iterSSEMessages` 会吞掉 AbortError）；否则任何传输错误都以
/// `AiErrorKind::Transport` + `APIConnectionError` 暴露。取消本身的文案
/// "Request aborted" 由上游重试层生成，本函数不产生它。
///
/// # 注意
///
/// 解码任务对 `sender` 的发送错误一律忽略：接收端被提前丢弃后，它仍会继续把 `body`
/// 读到 `[DONE]` 或 EOF 才结束，而不会立即停止读取。
pub fn provider_event_stream(
    body: ByteStream,
    signal: Option<tokio_util::sync::CancellationToken>,
) -> ProviderEventStream {
    let (sender, receiver) = futures::channel::mpsc::unbounded::<Result<Value, AiError>>();
    crate::api::lazy::spawn_forward(async move {
        let mut body = body;
        let mut lines = LineDecoder::default();
        let mut decoder = SseMessageDecoder::default();
        let mut first_chunk = true;
        // `[DONE]` 或错误事件之后不再处理任何字节（SDK 的迭代器在 `[DONE]` 处 break）。
        let mut finished = false;

        let emit = |message: (Option<String>, String),
                    sender: &futures::channel::mpsc::UnboundedSender<Result<Value, AiError>>|
         -> bool {
            let (event, data) = message;
            if data == "[DONE]" {
                return false;
            }
            let parsed = match kiana_json::js_parse::parse(&data) {
                Ok(value) => value,
                Err(_) => {
                    let _ = sender
                        .unbounded_send(Err(AiError::new(AiErrorKind::Parse, MALFORMED_SSE_JSON)));
                    return false;
                }
            };
            let is_error_event = event.as_deref() == Some("error");
            let error_value = field(&parsed, "error")
                .filter(|value| truthy(Some(value)))
                .cloned();
            if is_error_event || error_value.is_some() {
                // `data?.error ?? data`
                let error = match field(&parsed, "error") {
                    Some(value) if !value.is_null() => value.clone(),
                    _ => parsed,
                };
                let message = api_error_message(None, Some(&error), None);
                let _ = sender.unbounded_send(Err(AiError::new(AiErrorKind::Other, message)
                    .with_name("APIError")
                    .with_detail(error)));
                return false;
            }
            if let Some(event) = event
                && event.starts_with("thread.")
            {
                let envelope = json_object(vec![("event", Value::String(event)), ("data", parsed)]);
                let _ = sender.unbounded_send(Ok(envelope));
                return true;
            }
            let _ = sender.unbounded_send(Ok(parsed));
            true
        };

        // `finished` 区分「正常 EOF」与「已产出终止事件」：后者不能再 flush，否则会把
        // `[DONE]` / 错误事件之后缓冲的字节补发成新事件。
        'outer: loop {
            let chunk = match body.next().await {
                Some(Ok(chunk)) => chunk,
                Some(Err(error)) => {
                    // SDK 的 `_iterSSEMessages` 在 signal 已中止时吞掉 AbortError 并结束迭代；
                    // 适配器随后把“流提前结束”/signal 检查映射成用户可见错误。
                    let aborted = signal.as_ref().is_some_and(|signal| signal.is_cancelled());
                    if aborted && error.message == crate::http::reqwest_client::ABORTED_MESSAGE {
                        return;
                    }
                    let _ = sender.unbounded_send(Err(AiError::new(
                        AiErrorKind::Transport,
                        error.message,
                    )
                    .with_name("APIConnectionError")));
                    return;
                }
                None => break,
            };
            let chunk = if first_chunk {
                first_chunk = false;
                // 只在首个块剥 BOM；后续块里的 EF BB BF 是普通内容。
                chunk
                    .strip_prefix(&[0xef, 0xbb, 0xbf][..])
                    .map(<[u8]>::to_vec)
                    .unwrap_or(chunk)
            } else {
                chunk
            };
            for line in lines.decode(&chunk) {
                if let Some(message) = decoder.decode(&line)
                    && !emit(message, &sender)
                {
                    finished = true;
                    break 'outer;
                }
            }
        }
        if finished {
            return;
        }
        // 正常 EOF：先补发未以换行结束的最后一行，再补发没有空行结尾的消息。
        for line in lines.flush() {
            if let Some(message) = decoder.decode(&line)
                && !emit(message, &sender)
            {
                return;
            }
        }
        if let Some(message) = decoder.flush() {
            let _ = emit(message, &sender);
        }
    });
    Box::pin(receiver)
}

// =============================================================================
// 传输层：HTTP 请求与 OpenAI SDK 风格错误
// =============================================================================

/// 一次失败的请求：SDK 错误字段（供 retry 判定）+ catch 归一化用的抛出值。
///
/// 两个字段是同一次失败的两个视角：`provider_error` 是重试层要的消息/状态码/响应头；
/// `value` 是 TS `catch` 拿到的 SDK 错误对象形状（status + 已解析 body），供
/// `formatProviderError` 生成文本。分开保存是因为 `retry_provider_request` 只返回
/// `AiError`，会丢掉后两者。
#[derive(Clone)]
pub(crate) struct RequestFailure {
    /// 重试判定用的 SDK 错误字段（消息、状态码、`Headers.get`）。
    pub provider_error: ProviderError,
    /// catch 归一化用的抛出值（`APIError` 形状）。
    pub value: ProviderErrorValue,
}

/// `APIError.makeMessage(status, error, message)`。
///
/// 优先级与 SDK 一致：
/// 1. `error` 为真值时取 `error.message`（字符串原样，否则 `JSON.stringify`）；
///    message 缺失或为假值则 `JSON.stringify(error)`；
/// 2. 否则用显式 `message` 参数（本文件里是原始 body 文本）；
/// 3. 都没有 → `"(no status code or body)"`。
///
/// 有 status 时加 `${status} ` 前缀；有 status 但无消息时是
/// `${status} status code (no body)`。
pub(crate) fn api_error_message(
    status: Option<u16>,
    error: Option<&Value>,
    message: Option<&str>,
) -> String {
    let msg = match error {
        Some(error) if truthy(Some(error)) => Some(match field(error, "message") {
            Some(inner) if truthy(Some(inner)) => {
                if let Some(text) = inner.as_str() {
                    text.to_string()
                } else {
                    js_stringify(inner)
                }
            }
            _ => js_stringify(error),
        }),
        _ => message.map(str::to_string),
    };
    let msg = msg.filter(|msg| !msg.is_empty());
    match (status, msg) {
        (Some(status), Some(msg)) => format!("{status} {msg}"),
        (Some(status), None) => format!("{status} status code (no body)"),
        (None, Some(msg)) => msg,
        (None, None) => "(no status code or body)".to_string(),
    }
}

/// `Headers.get` 的大小写不敏感查找。
fn header_lookup(headers: &IndexMap<String, String>) -> crate::utils::provider_retry::HeaderLookup {
    let mut normalized: HashMap<String, String> = HashMap::new();
    for (name, value) in headers {
        normalized.insert(name.to_ascii_lowercase(), value.clone());
    }
    Some(Arc::new(move |name: &str| {
        normalized.get(&name.to_ascii_lowercase()).cloned()
    }))
}

/// `APIClient.makeStatusError(status, errJSON, errMessage, headers)`。
///
/// 响应体先按 JS 的 `JSON.parse` 解析；解析失败或结果为假值时用原始文本作为错误消息的
/// 兜底。解析成功且结果为真值时按 SDK 的 `normalizedError` 归一化：普通对象缺少
/// `error` 字段或 `error` 为 `null`，或结果本身是数组时包成 `{ "error": <原值> }`，
/// 其余原样使用。
///
/// `provider_error` 供重试判定（状态码 + 响应头），`value` 供 catch 还原 TS 侧错误
/// 文本（status + 解析后的 error 对象）；两者必须来自同一份响应体。
pub(crate) fn status_failure(
    status: u16,
    headers: &IndexMap<String, String>,
    body_text: &str,
) -> RequestFailure {
    let parsed = kiana_json::js_parse::parse(body_text).ok();
    let err_json = parsed.filter(|value| truthy(Some(value)));
    // `normalizedError = error && typeof error === 'object' && error.error == null ? { error } : error`
    let normalized = match &err_json {
        Some(Value::Object(map)) if map.get("error").is_none_or(Value::is_null) => {
            json_object(vec![("error", err_json.clone().unwrap_or(Value::Null))])
        }
        Some(Value::Array(_)) => {
            json_object(vec![("error", err_json.clone().unwrap_or(Value::Null))])
        }
        Some(other) => other.clone(),
        None => Value::Null,
    };
    let inner = field(&normalized, "error").cloned();
    let fallback = if err_json.is_some() {
        None
    } else {
        Some(body_text)
    };
    let message = api_error_message(Some(status), inner.as_ref(), fallback);
    let fields = ProviderErrorFields {
        message: message.clone(),
        status: Some(f64::from(status)),
        error_body: inner,
        ..ProviderErrorFields::default()
    };
    RequestFailure {
        provider_error: ProviderError {
            message,
            status: Some(status),
            header: header_lookup(headers),
        },
        value: ProviderErrorValue::Error(fields),
    }
}

/// 读空响应体。
///
/// 把整个 body 收集为一个字节串；中途任一 chunk 读取失败会立即返回该 `HttpError`，
/// 已读字节丢弃。调用方在非 2xx 路径上用它拼错误消息。
pub(crate) async fn collect_body(mut body: ByteStream) -> Result<Vec<u8>, HttpError> {
    let mut collected = Vec::new();
    while let Some(chunk) = body.next().await {
        collected.extend_from_slice(&chunk?);
    }
    Ok(collected)
}

/// 执行一次 HTTP 请求；非 2xx 时读出响应体并构造 SDK 风格错误。
///
/// 成功时返回的 [`HttpResponse`] 里 body 尚未读取，交给调用方决定是解析 SSE 还是丢弃。
/// 失败路径：
/// - 请求期错误被包装成 [`ProviderError`]/[`ProviderErrorValue`]（`fetch failed` 改写为
///   `"Connection error."`，其余保持原文），由重试层决定是否重试；
/// - 非 2xx 会把 body 读完（UTF-8 解码，非法字节替换为 U+FFFD）后交给 `status_failure`。
///
/// `request.timeout_ms` 在这里被 `take()` 掉并只约束「拿到响应对象」之前；body 读取
/// 不受它限制（原因见函数内注释）。
pub(crate) async fn execute_request(
    client: &dyn HttpClient,
    mut request: HttpRequest,
    signal: Option<&tokio_util::sync::CancellationToken>,
) -> Result<HttpResponse, RequestFailure> {
    // TS 的 `timeoutMs` 是 OpenAI SDK 的 request timeout：`fetchWithTimeout` 用
    // `setTimeout(abort, ms)` 计时，并在 `finally` 里立刻 `clearTimeout`——它只覆盖「拿到响应
    // 对象（响应头）之前」；之后 body 只受 undici 的 idle bodyTimeout 约束。未给出
    // `timeoutMs` 时 SDK 用默认的 600000ms（`OpenAI.DEFAULT_TIMEOUT`）。冻结的
    // `HttpRequest.timeout_ms` 会被 `ReqwestHttpClient` 当成整条请求（含 body）的 deadline，
    // 因此这里把它取出来自己用，绝不向下传（否则超过 timeoutMs 的健康 SSE 会被截断）。
    let timeout_ms = request
        .timeout_ms
        .take()
        .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS);
    let request_result = {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        match tokio::time::timeout_at(deadline, client.execute(request, signal)).await {
            Ok(result) => result,
            Err(_) => Err(HttpError::new(REQUEST_TIMED_OUT_MESSAGE)),
        }
    };
    let response = match request_result {
        Ok(response) => response,
        Err(error) => {
            // SDK 捕获 fetch 的 rejection 并改抛 `APIConnectionError`（默认文案
            // "Connection error."，status/headers 都是 undefined）；Node fetch 的传输层失败
            // 一律是 `TypeError: fetch failed`。其它文案（非法 URL、非法头名等请求装配错误）
            // 保持原样：TS 里它们在 fetch 之前由 SDK 抛出，不会被改写。调用方取消由
            // `retryProviderRequest` 统一改写为 "Request aborted"（见 `provider_retry`），
            // 与 TS 的顺序一致。
            let (message, status) = if error.message == FETCH_FAILED_MESSAGE {
                (CONNECTION_ERROR_MESSAGE.to_string(), None)
            } else {
                (error.message, error.status)
            };
            let fields = ProviderErrorFields {
                message: message.clone(),
                status: status.map(f64::from),
                ..ProviderErrorFields::default()
            };
            return Err(RequestFailure {
                provider_error: ProviderError {
                    message,
                    status,
                    header: None,
                },
                value: ProviderErrorValue::Error(fields),
            });
        }
    };
    let status = response.response.status;
    if (200..300).contains(&status) {
        return Ok(response);
    }
    let headers = response.response.headers.clone();
    let body_text = match collect_body(response.body).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) => error.message,
    };
    Err(status_failure(status, &headers, &body_text))
}

/// `retryProviderRequest(() => client.create(...).withResponse(), ...)` 的等价物。
///
/// 返回最终结果，以及**最后一次尝试**的失败信息（TS 的 catch 拿到的是 SDK 错误对象，
/// 带 status 与解析后的 body；`retry_provider_request` 只把消息转成 [`AiError`]）。
///
/// 每次尝试都把失败写入 `last_failure`（后一次覆盖前一次），所以返回的是最末一次失败
/// 的快照。用 `Arc<Mutex<..>>` 是因为重试闭包按 `FnMut` 被反复调用，不能借用外部可变
/// 状态；锁内只做一次移动、不跨 `await` 持锁，因此没有死锁风险。
///
/// 取消：`signal` 同时传给 HTTP 执行层与重试层。重试层在两次尝试之间发现取消会返回
/// "Request aborted"，此时 `AiError` 与 `last_failure` 里上一次失败的消息不同——
/// `thrown_value` 依赖这一点避免把陈旧失败当成最终错误。
///
/// 这里传空的 `no_retry_statuses`：重试策略与 OpenAI SDK 默认一致（`x-should-retry`
/// 响应头优先；否则无状态码的传输错误、408/409/429/5xx 重试，其余立即失败）。
pub(crate) async fn request_with_retry(
    client: SharedHttpClient,
    request: HttpRequest,
    signal: Option<tokio_util::sync::CancellationToken>,
    max_retries: Option<u32>,
    max_retry_delay_ms: Option<u64>,
) -> (Result<HttpResponse, AiError>, Option<RequestFailure>) {
    let last_failure: Arc<Mutex<Option<RequestFailure>>> = Arc::new(Mutex::new(None));
    let retry_options = ProviderRetryOptions {
        max_retries: max_retries.map(u64::from),
        max_retry_delay_ms,
        signal: signal.clone().map(crate::utils::abort::AbortScope::from),
        no_retry_statuses: Vec::new(),
    };
    let result = retry_provider_request(
        || {
            let client = client.clone();
            let request = request.clone();
            let signal = signal.clone();
            let last_failure = last_failure.clone();
            async move {
                match execute_request(&*client, request, signal.as_ref()).await {
                    Ok(response) => Ok(response),
                    Err(failure) => {
                        *last_failure.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(failure.clone());
                        Err(failure.provider_error)
                    }
                }
            }
        },
        retry_options,
    )
    .await;
    let failure = last_failure
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    (result, failure)
}

/// catch 块里使用的抛出值：最后一次请求失败优先，其次 SSE/解析错误（`detail` 承载原始
/// `error.error` 对象）。
///
/// 只有 `AiError` 的消息与最后一次 HTTP 失败完全一致时才复用 `RequestFailure.value`：
/// 消息相同说明这个 `AiError` 就是该失败经 `provider_error_to_ai_error` 转来的，此时
/// status/body 才有意义。否则（典型是取消发生在重试之间，`AiError` 是
/// "Request aborted"，而 `last_failure` 还停在上一次 5xx）用 `AiError` 自身的
/// message/detail 重新构造，避免报出陈旧的失败。
pub(crate) fn thrown_value(
    error: AiError,
    last_failure: Option<RequestFailure>,
) -> ProviderErrorValue {
    match last_failure {
        Some(failure) if failure.provider_error.message == error.message => failure.value,
        _ => ProviderErrorValue::Error(ProviderErrorFields {
            message: error.message,
            error_body: error.detail,
            ..ProviderErrorFields::default()
        }),
    }
}

/// `formatProviderError(normalizeProviderError(error), prefix?)`。
pub(crate) fn format_thrown_error(
    error: AiError,
    last_failure: Option<RequestFailure>,
    prefix: Option<&str>,
) -> String {
    let value = thrown_value(error, last_failure);
    let normalized = crate::utils::error_body::normalize_provider_error(&value);
    crate::utils::error_body::format_provider_error(&normalized, prefix)
}

/// `formatProviderError(normalizeProviderError(error))` 同时返回抛出值（completions 需要
/// `error.error.metadata.raw`）。
pub(crate) fn format_thrown_error_with_value(
    error: AiError,
    last_failure: Option<RequestFailure>,
    prefix: Option<&str>,
) -> (String, ProviderErrorValue) {
    let value = thrown_value(error, last_failure);
    let normalized = crate::utils::error_body::normalize_provider_error(&value);
    (
        crate::utils::error_body::format_provider_error(&normalized, prefix),
        value,
    )
}

// =============================================================================
// 文本签名
// =============================================================================

/// `encodeTextSignatureV1(id, phase?)`。
///
/// 文本块的签名在本项目内部是「下一轮请求回放这条 assistant 消息所需的 message id +
/// phase」。Responses 协议没有单独字段承载它，所以序列化成 JSON 字符串存进
/// `text_signature`；`v: 1` 是格式版本，供 `parse_text_signature` 识别。键序
/// `v` → `id` → `phase` 与 `JSON.stringify` 一致，phase 缺省时整个字段缺失。
fn encode_text_signature_v1(id: Option<&str>, phase: Option<TextSignaturePhase>) -> String {
    let mut payload = Map::new();
    payload.insert("v".to_string(), Value::from(1));
    if let Some(id) = id {
        payload.insert("id".to_string(), Value::String(id.to_string()));
    }
    if let Some(phase) = phase {
        payload.insert(
            "phase".to_string(),
            Value::String(
                match phase {
                    TextSignaturePhase::Commentary => "commentary",
                    TextSignaturePhase::FinalAnswer => "final_answer",
                }
                .to_string(),
            ),
        );
    }
    js_stringify(&Value::Object(payload))
}

/// `parseTextSignature(signature)` 的结果。
struct ParsedTextSignature {
    id: String,
    phase: Option<TextSignaturePhase>,
}

/// `parseTextSignature(signature)`。
///
/// 仅当签名以 `{` 开头、能解析、`v == 1` 且 `id` 是字符串时按新格式解读；其余情况
/// （包括 JSON 解析失败）都把整串当作 legacy message id、`phase = None`，用于兼容
/// 旧会话里已经持久化的签名。空串或缺失返回 `None`。
fn parse_text_signature(signature: Option<&str>) -> Option<ParsedTextSignature> {
    let signature = signature.filter(|text| !text.is_empty())?;
    if signature.starts_with('{')
        && let Ok(parsed) = kiana_json::js_parse::parse(signature)
        && field(&parsed, "v").and_then(Value::as_f64) == Some(1.0)
        && let Some(id) = field(&parsed, "id").and_then(Value::as_str)
    {
        let phase = match field(&parsed, "phase").and_then(Value::as_str) {
            Some("commentary") => Some(TextSignaturePhase::Commentary),
            Some("final_answer") => Some(TextSignaturePhase::FinalAnswer),
            _ => None,
        };
        return Some(ParsedTextSignature {
            id: id.to_string(),
            phase,
        });
    }
    Some(ParsedTextSignature {
        id: signature.to_string(),
        phase: None,
    })
}

// =============================================================================
// 工具结果输出
// =============================================================================

/// `convertToolResultOutput(model, content)`。
///
/// 工具结果在 Responses 协议里可以是字符串（纯文本）或 `input_text` / `input_image`
/// 数组。规则：模型不支持图片输入、或内容里没有图片时永远产出**字符串**；有图片且
/// 模型支持图片时产出数组。空文本用固定的协议占位文案
/// `"(see attached image)"` / `"(no tool output)"`。所有文本都经
/// `sanitize_surrogates`，图片转成 `data:` URL。
fn convert_tool_result_output(model: &Model, content: &[crate::types::UserContentBlock]) -> Value {
    let mut text_parts: Vec<&str> = Vec::new();
    let mut images: Vec<&crate::types::ImageContent> = Vec::new();
    for block in content {
        match block {
            crate::types::UserContentBlock::Text(text) => text_parts.push(&text.text),
            crate::types::UserContentBlock::Image(image) => images.push(image),
        }
    }
    let text_result = text_parts.join("\n");
    let has_text = !kiana_json::js_string::js_trim(&text_result).is_empty();
    if images.is_empty() || !model.input.contains(&ModelInputModality::Image) {
        let text = if has_text {
            text_result
        } else if !images.is_empty() {
            "(see attached image)".to_string()
        } else {
            "(no tool output)".to_string()
        };
        return Value::String(sanitize_surrogates(&text));
    }
    let mut output: Vec<Value> = Vec::new();
    if has_text {
        output.push(json_object(vec![
            ("type", Value::String("input_text".into())),
            ("text", Value::String(sanitize_surrogates(&text_result))),
        ]));
    }
    for image in images {
        output.push(json_object(vec![
            ("type", Value::String("input_image".into())),
            ("detail", Value::String("auto".into())),
            (
                "image_url",
                Value::String(format!("data:{};base64,{}", image.mime_type, image.data)),
            ),
        ]));
    }
    Value::Array(output)
}

// =============================================================================
// 选项
// =============================================================================

/// `resolveServiceTier(responseServiceTier, requestServiceTier)`。
///
/// 返回本次响应实际生效的 service tier（可缺省）。响应可能回写 `service_tier`，请求侧
/// 也可能显式指定，具体取舍由适配器注入；本模块只在 `finalize_response` 里调用它，
/// 结果交给 [`ServiceTierPricingFn`] 做计价。
pub type ServiceTierResolver =
    Arc<dyn Fn(Option<&str>, Option<&str>) -> Option<String> + Send + Sync>;
/// `applyServiceTierPricing(usage, serviceTier)`。
///
/// 在 `calculate_cost` 之后调用，用于按服务层倍率改写 `Usage.cost`。本模块不实现倍率表
/// （各模型不同），只保证调用时机与参数。
pub type ServiceTierPricingFn = Arc<dyn Fn(&mut Usage, Option<&str>) + Send + Sync>;

/// `OpenAIResponsesStreamOptions`。
///
/// 由适配器构造后传给 [`process_responses_stream`]。它只影响**流处理与计价**，除了
/// `service_tier` 的镜像外不参与请求体构造。
#[derive(Clone, Default)]
pub struct OpenAIResponsesStreamOptions {
    /// `onProviderStreamEvent`。
    ///
    /// 每个 provider 事件被处理**之前**调用，参数是事件副本与 model；用于观测/调试，
    /// 不参与状态推导。
    pub on_provider_stream_event: Option<crate::types::OnProviderStreamEventFn>,
    /// 请求里的 `service_tier`。
    ///
    /// 与响应回写的 tier 一起交给 [`ServiceTierResolver`]（或直接作为兜底）决定计价层。
    pub service_tier: Option<String>,
    /// grammar 工具名 → 输入参数名。
    ///
    /// 消息转换时用它决定历史 tool call 回放成 `custom_tool_call` 还是 `function_call`；
    /// 流处理侧只用它取 `custom_tool_call` 的输入属性名（缺失时回退 `"input"`），
    /// 调用类型由事件里的 `item.type` 决定。
    pub grammar_tool_input_properties: Option<IndexMap<String, String>>,
    /// `resolveServiceTier(responseServiceTier, requestServiceTier)`。
    pub resolve_service_tier: Option<ServiceTierResolver>,
    /// `applyServiceTierPricing(usage, serviceTier)`。
    pub apply_service_tier_pricing: Option<ServiceTierPricingFn>,
}

/// `ConvertResponsesMessagesOptions`。
///
/// 这些开关不是任意组合：`supports_mid_convo_system_messages` +
/// `supports_additional_tools` 同时为真时走 `Changes`（可在对话中途新增/重定义工具）；
/// 否则 `supports_tool_search` 为真走 `Additions`（用 `tool_search_call` /
/// `tool_search_output` 表达新增）；都为假则 `None`（工具只在顶层声明）。
#[derive(Clone, Default)]
pub struct ConvertResponsesMessagesOptions {
    /// 默认 `true`。
    ///
    /// 为 `false` 时丢弃开头那条 system 消息本身（工具变更锚点仍会被处理）。
    pub include_system_prompt: Option<bool>,
    /// grammar 工具名 → 输入参数名。
    ///
    /// 决定历史 tool call 回放成 `custom_tool_call` 还是 `function_call`：查不到名字时
    /// 按普通 function 处理。流处理侧对服务端返回的 `custom_tool_call` 另有 `"input"`
    /// 回退，两者不是同一规则。
    pub grammar_tool_input_properties: Option<IndexMap<String, String>>,
    /// 默认 `false`。为 `true` 时保留对话中途的 system 消息，并按工具变更锚点处理；
    /// 为 `false` 时 `resolve_transcript` 会把它们折叠到开头。
    pub supports_mid_convo_system_messages: Option<bool>,
    /// 默认 `false`。仅在 `Changes` 模式下使用：工具变更编码成 `additional_tools` 项，
    /// 其 `role` 恒为 `developer`。
    pub supports_additional_tools: Option<bool>,
    /// 默认 `false`。`Additions` 模式：后续新增的工具用 `tool_search_call` +
    /// `tool_search_output` 表达，并给工具加 `defer_loading: true`。
    pub supports_tool_search: Option<bool>,
    /// `toolOptions`。
    ///
    /// 供工具变更编码时调用 [`convert_responses_tools`] 使用的默认选项。
    pub tool_options: Option<ConvertResponsesToolsOptions>,
    /// 当前回复的 effort；设置时思考级别用 `configuration_update` 项表达。
    pub configuration_update_effort: Option<String>,
}

/// `ConvertResponsesToolsOptions`。
///
/// `strict` 用 `Option<Option<bool>>` 保留 TS 的三态：`None`（缺省）、`Some(None)`（`null`）、
/// `Some(Some(value))`。
///
/// 默认值：`strict` 缺省视为 `false`；`supports_strict_mode` 默认 `true`；
/// `supports_openai_grammar_tools` 默认 `false`；`tool_search_result` 默认 `false`。
#[derive(Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ConvertResponsesToolsOptions {
    /// `strict` 的默认值。`None` → `false`；`Some(None)`（只能由 Rust 代码显式构造；JSON
    /// 里的 `null` 经 serde 会变成外层 `None`，等同缺省并按 `false` 处理）→ 发送
    /// `strict: null`，且未自行声明 constrained sampling 的工具不做 strict schema 改写。
    pub strict: Option<Option<bool>>,
    /// 为假时不发送 `strict` 字段；若工具声明要求 strict（`strict: "require"`），
    /// 转换会直接报错而不是降级。
    pub supports_strict_mode: Option<bool>,
    /// 为真时 grammar 约束的工具转成 `type: "custom"` + `format`；为假时退回普通
    /// function 工具。
    pub supports_openai_grammar_tools: Option<bool>,
    /// 为真时给每个工具加 `defer_loading: true`（作为 `tool_search_output` 的结果声明）。
    pub tool_search_result: bool,
}

/// `getResponsesToolSupport(options)`。
///
/// 把三个能力开关压成一个 [`TranscriptToolSupport`]：`mid-convo system` **且**
/// `additional tools` → `Changes`；否则 `tool search` → `Additions`；否则 `None`。
/// 三个开关缺省都是 `false`。结果决定 [`convert_responses_messages`] 如何把工具变更
/// 编码进输入项，以及 `resolve_transcript_tools` 折叠多少条 system 消息。
pub fn get_responses_tool_support(
    options: &ConvertResponsesMessagesOptions,
) -> TranscriptToolSupport {
    let mid_convo = options.supports_mid_convo_system_messages.unwrap_or(false);
    let additional = options.supports_additional_tools.unwrap_or(false);
    if mid_convo && additional {
        return TranscriptToolSupport::Changes;
    }
    if options.supports_tool_search.unwrap_or(false) {
        return TranscriptToolSupport::Additions;
    }
    TranscriptToolSupport::None
}

// =============================================================================
// 工具转换
// =============================================================================

fn grammar_format(grammar: &crate::api::constrained_sampling::GrammarConstrainedSampling) -> Value {
    json_object(vec![
        ("type", Value::String("grammar".into())),
        ("syntax", Value::String(grammar.format.as_str().into())),
        ("definition", Value::String(grammar.definition.clone())),
    ])
}

/// `convertResponsesTools(tools, options?)`。
///
/// 每个工具二选一：
/// - 有 grammar 约束且支持 OpenAI grammar → `{type: "custom", format: {type: "grammar",
///   syntax, definition}}`。这类工具没有 `strict` / `parameters`，流处理里对应的调用
///   会被识别为 `custom_tool_call`；
/// - 否则 → `{type: "function", parameters, strict?}`，`parameters` 在 strict 为真时经
///   `make_strict_json_schema` 改写。
///
/// `strict` 是 `Option<bool>`：`None` 表示把 `null` 发出去（保留 TS 的三态）；该字段
/// 只在 `supports_strict_mode` 为真时出现。
///
/// 错误来自约束采样解析：grammar 变体缺失/参数形状不合法、或工具要求 strict 但当前
/// 不支持；错误消息由 `constrained_sampling` 生成并逐字保留。
pub fn convert_responses_tools(
    tools: &[Tool],
    options: Option<&ConvertResponsesToolsOptions>,
) -> Result<Vec<Value>, AiError> {
    let default_strict = match options.and_then(|options| options.strict) {
        None => Some(false),
        Some(strict) => strict,
    };
    let supports_strict_mode = options
        .and_then(|options| options.supports_strict_mode)
        .unwrap_or(true);
    let supports_grammar = options
        .and_then(|options| options.supports_openai_grammar_tools)
        .unwrap_or(false);
    let tool_search_result = options.is_some_and(|options| options.tool_search_result);

    let mut converted = Vec::with_capacity(tools.len());
    for tool in tools {
        // grammar 工具优先：命中后不再走 function 分支（对应 TS 的 `if (...) { return; }`）。
        if let Some(grammar) = resolve_grammar_constrained_sampling(tool, supports_grammar)? {
            let mut item = Map::new();
            item.insert("type".into(), Value::String("custom".into()));
            item.insert("name".into(), Value::String(tool.name.clone()));
            item.insert(
                "description".into(),
                Value::String(tool.description.clone()),
            );
            item.insert("format".into(), grammar_format(&grammar));
            if tool_search_result {
                item.insert("defer_loading".into(), Value::Bool(true));
            }
            converted.push(Value::Object(item));
            continue;
        }
        // 工具显式声明的 strict 优先于 options.strict 的默认值；`Some(false)` 是显式
        // false，不是缺省。
        let constrained_strict =
            resolve_json_schema_strict_sampling(tool, supports_strict_mode, None)?;
        let strict = constrained_strict.or(default_strict);
        let mut item = Map::new();
        item.insert("type".into(), Value::String("function".into()));
        item.insert("name".into(), Value::String(tool.name.clone()));
        item.insert(
            "description".into(),
            Value::String(tool.description.clone()),
        );
        item.insert(
            "parameters".into(),
            get_json_schema_tool_parameters(tool, Some(strict == Some(true)))?,
        );
        if tool_search_result {
            item.insert("defer_loading".into(), Value::Bool(true));
        }
        if supports_strict_mode {
            item.insert(
                "strict".into(),
                match strict {
                    Some(value) => Value::Bool(value),
                    None => Value::Null,
                },
            );
        }
        converted.push(Value::Object(item));
    }
    Ok(converted)
}

// =============================================================================
// 消息转换
// =============================================================================

use crate::types::{Message, UserContent, UserContentBlock};

/// `hasToolRedefinitions` 式的 compat 读取：只有显式 `false` 才关闭 developer role。
///
/// 字段缺失、`null` 或非布尔值都按「支持」处理，与 TS 的 `!== false` 判定一致。
fn supports_developer_role(model: &Model) -> bool {
    !matches!(
        model
            .compat
            .as_ref()
            .and_then(|compat| compat.get("supportsDeveloperRole"))
            .and_then(Value::as_bool),
        Some(false)
    )
}

/// 按 UTF-16 码元计长（JS 的 `string.length`），不是字节数或 Unicode 标量数。
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// JS 的 `/[^a-zA-Z0-9_-]/g` → `"_"`（按 UTF-16 码元）。
///
/// 正则没有 `u` 标志，代理对会被当成两个码元、各自替换成 `_`；这里的
/// `encode_utf16` 逐码元映射复刻了这一点。
fn replace_disallowed(part: &str) -> String {
    part.encode_utf16()
        .map(|unit| {
            let ch = unit as u32;
            let allowed = (u32::from(b'a')..=u32::from(b'z')).contains(&ch)
                || (u32::from(b'A')..=u32::from(b'Z')).contains(&ch)
                || (u32::from(b'0')..=u32::from(b'9')).contains(&ch)
                || ch == u32::from(b'_')
                || ch == u32::from(b'-');
            if allowed {
                char::from_u32(ch).unwrap_or('_')
            } else {
                '_'
            }
        })
        .collect()
}

/// `normalizeIdPart(part)`。
///
/// 清洗 → 超长截断 → 去掉尾部 `_`。截断用 `chars()` 而不是字节，但清洗后只剩
/// ASCII，因此与 JS 的 `slice(0, 64)`（按 UTF-16 码元）等价。去尾下划线可能把
/// `fc_` 变成 `fc`，调用方随后会按需要再补前缀（`fc_fc` 是照抄 TS 的结果）。
fn normalize_id_part(part: &str) -> String {
    let sanitized = replace_disallowed(part);
    let normalized: String = if utf16_len(&sanitized) > 64 {
        sanitized.chars().take(64).collect()
    } else {
        sanitized
    };
    normalized.trim_end_matches('_').to_string()
}

/// `buildForeignResponsesItemId(itemId)`。
///
/// 跨 provider 历史里的 item id 属于别家的不透明字符串，不能原样回传给 Responses；
/// 用短哈希生成 `fc_<hash>`（固定前缀以满足 Responses 的 item id 形状要求），再按
/// 64 长度上限截断。
fn build_foreign_responses_item_id(item_id: &str) -> String {
    let normalized = format!("fc_{}", short_hash(item_id));
    if utf16_len(&normalized) > 64 {
        normalized.chars().take(64).collect()
    } else {
        normalized
    }
}

/// `appendSystemToolAdditions(message, seed)`。
///
/// 把一条「后续 system 消息里声明的工具变更」编码成 Responses 输入项。三种工具支持
/// 模式下的行为：
/// - `Changes`：输出 `additional_tools` 项（`role` 恒为 `developer`）；若之前已经声明
///   过 additional tools，先插一条 `instruction_role` 的增量提示，列出仍不可用的工具。
/// - `Additions`：输出一对 `tool_search_call` + `tool_search_output`，`call_id` 由
///   `seed`（调用方传 `system:<msgIndex>`）与工具名列表哈希得到，保证同一会话可复现。
/// - `None`：不输出任何东西（这些工具的变更只能靠顶层工具列表表达）。
///
/// `unavailable_tools` 是跨消息累积的「已移除但仍需提示」集合：每次先移除本次重新
/// 声明的工具，移除通知由调用方另行生成。错误来自 `convert_responses_tools`。
#[allow(clippy::too_many_arguments)]
fn append_system_tool_additions(
    messages: &mut Vec<Value>,
    message: &SystemMessage,
    seed: &str,
    tool_support: TranscriptToolSupport,
    instruction_role: &str,
    options: &ConvertResponsesMessagesOptions,
    unavailable_tools: &mut IndexSet<String>,
    declared_additional_tools: &mut bool,
) -> Result<(), AiError> {
    let tools = message.tools_added.as_deref().unwrap_or(&[]);
    for tool in tools {
        unavailable_tools.shift_remove(&tool.name);
    }
    if tools.is_empty() {
        return Ok(());
    }
    if tool_support == TranscriptToolSupport::Changes {
        if *declared_additional_tools {
            messages.push(json_object(vec![
                ("role", Value::String(instruction_role.to_string())),
                (
                    "content",
                    Value::String(render_incremental_tools_notice(unavailable_tools)),
                ),
            ]));
        }
        *declared_additional_tools = true;
        let mut item = Map::new();
        item.insert("type".into(), Value::String("additional_tools".into()));
        item.insert("role".into(), Value::String("developer".into()));
        item.insert(
            "tools".into(),
            Value::Array(convert_responses_tools(
                tools,
                options.tool_options.as_ref(),
            )?),
        );
        messages.push(Value::Object(item));
        return Ok(());
    }
    if tool_support != TranscriptToolSupport::Additions {
        return Ok(());
    }
    let names: Vec<String> = tools.iter().map(|tool| tool.name.clone()).collect();
    // 稳定的合成 call_id：同一 seed + 同一组工具名总是得到同一 id。
    let call_id = format!(
        "kiana_tool_load_{}",
        short_hash(&format!("{seed}:{}", names.join(",")))
    );
    messages.push(json_object(vec![
        ("type", Value::String("tool_search_call".into())),
        ("call_id", Value::String(call_id.clone())),
        ("execution", Value::String("client".into())),
        ("status", Value::String("completed".into())),
        (
            "arguments",
            json_object(vec![
                ("query", Value::String(names.join(" "))),
                (
                    "limit",
                    Value::from(u32::try_from(names.len()).unwrap_or(u32::MAX)),
                ),
            ]),
        ),
    ]));
    let mut tool_options = options.tool_options.clone().unwrap_or_default();
    // 作为搜索结果声明的工具必须带 `defer_loading: true`，与上面的查询项配对。
    tool_options.tool_search_result = true;
    messages.push(json_object(vec![
        ("type", Value::String("tool_search_output".into())),
        ("call_id", Value::String(call_id)),
        ("execution", Value::String("client".into())),
        ("status", Value::String("completed".into())),
        (
            "tools",
            Value::Array(convert_responses_tools(tools, Some(&tool_options))?),
        ),
    ]));
    Ok(())
}

/// `renderIncrementalToolsNotice(unavailable)`。
///
/// 这段提示文本是协议的一部分（模型据此理解「增量工具列表」的语义），是当前稳定契约，
/// 改动需同步测试；有不可用工具时追加 `Still unavailable: ...` 一行。
fn render_incremental_tools_notice(unavailable: &IndexSet<String>) -> String {
    const NOTICE: &str = "This is an incremental tools update. Previously declared tools remain available unless explicitly marked unavailable. A tool declared again here is available again, and its latest definition replaces the earlier one.";
    if unavailable.is_empty() {
        NOTICE.to_string()
    } else {
        format!(
            "{NOTICE}\nStill unavailable: {}.",
            unavailable.iter().cloned().collect::<Vec<_>>().join(", ")
        )
    }
}

/// `renderToolRemovalNotice(names)`。
///
/// 移除无法在 Responses 协议里用结构化字段表达，只能作为一条 system/developer 文本
/// 告知模型「不要再调用这些工具」。逐字文案同样是协议的一部分。
fn render_tool_removal_notice(names: &[&str]) -> String {
    format!(
        "The following tools are no longer available. Do not call them:\n{}",
        names
            .iter()
            .map(|name| format!("- {name}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// `pushConfigurationUpdate(effort)`。
///
/// 在输入流里插入一个 `configuration_update` 项，告诉服务端这一轮使用的 reasoning
/// effort。它出现在 assistant 消息之前（同模型且记录了 provider effort 时）或整个输入
/// 的末尾（适配器为下一轮请求显式指定 effort 时），两种位置都不能随意挪动。
fn push_configuration_update(messages: &mut Vec<Value>, effort: &str) {
    messages.push(json_object(vec![
        ("type", Value::String("configuration_update".into())),
        (
            "reasoning",
            json_object(vec![("effort", Value::String(effort.to_string()))]),
        ),
    ]));
}

/// `JsonObject`（IndexMap）→ `Value::Object`。
///
/// 入参是借用，所以逐项克隆。`serde_json` 在本工作区启用了 `preserve_order`，因此
/// `Value::Object` 的键序与源 `IndexMap` 一致（后续 `stringify` 的输出逐字节稳定）。
pub(crate) fn json_from_object(object: &JsonObject) -> Value {
    Value::Object(
        object
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    )
}

/// `convertResponsesMessages(model, context, allowedToolCallProviders, options?)`。
///
/// 把统一消息列表转换成 Responses API 的 `input` 数组。这是纯函数：不发请求、不读全局
/// 状态，结果可直接 `JSON.stringify` 进请求体。
///
/// # 参数
///
/// - `model`：目标模型。决定 provider/api 判定（是否原样复用历史 item id）、图片输入
///   能力（tool result 的形状）、`supportsDeveloperRole`，以及 system 消息的角色
///   （`developer` 还是 `system`）。
/// - `context`：统一会话记录；先经 `resolve_transcript`（是否保留中途 system 消息）。
/// - `allowed_tool_call_providers`：目标模型的 `model.provider` 在这个集合内时，历史
///   工具调用 id 才按 Responses 的 `call_id|item_id` 格式解析；不在集合里的历史调用
///   只做字符净化。
/// - `options`：工具支持开关与 grammar 映射，见 [`ConvertResponsesMessagesOptions`]。
///
/// # 返回与错误
///
/// 返回 `input` 项数组。错误来自：thinking 签名不是合法 JSON（`Parse`）、grammar 工具
/// 参数不是字符串、工具转换失败（约束采样不满足）。调用方应把错误转成流上的 error 事件。
///
/// # 关键不变量
///
/// - 工具调用的内部 id 是 `call_id|item_id` 两段：Responses 需要 `call_id` 关联结果，
///   需要 `item_id`（`fc_` / `ctc_` 前缀）回放历史项。跨 provider 的 item id 会被替换
///   成 `fc_<hash>`，避免把别家的不透明 id 传回去。
/// - `msgIndex` 只在非「首条 system 消息」时递增，因此文本块的 fallback id
///   `msg_kiana_<n>` 由消息位置唯一决定，同一会话重复转换会得到相同 id。
/// - assistant 的 thinking 块签名按原样 `JSON.parse` 后回放；解析失败即报错，不静默丢弃。
/// - system 消息的 `tools_added` / `tools_removed` 只在 `anchors_tool_changes`（该条
///   system 未被顶层工具列表折叠）时生成协议项与移除提示，避免重复声明。
pub fn convert_responses_messages(
    model: &Model,
    context: &TranscriptContext,
    allowed_tool_call_providers: &[&str],
    options: Option<&ConvertResponsesMessagesOptions>,
) -> Result<Vec<Value>, AiError> {
    let default_options = ConvertResponsesMessagesOptions::default();
    let options = options.unwrap_or(&default_options);
    // 先归一化会话：不支持中途 system 消息时，它们会被折叠到开头（规则见
    // `utils::transcript`），后续的下标都基于归一化后的消息列表。
    let normalized = resolve_transcript(context, options.supports_mid_convo_system_messages);
    let source_messages = &normalized.as_context().messages;
    let mut messages: Vec<Value> = Vec::new();

    let target_provider = model.provider.clone();
    let target_api = model.api.clone();
    let allowed_providers: Vec<String> = allowed_tool_call_providers
        .iter()
        .map(|provider| provider.to_string())
        .collect();
    // 工具调用 id 的归一化回调：只有当目标 provider 在允许集合里、且 id 形如
    // `call_id|item_id` 时才做两段式处理；否则整串按普通 id 净化。
    let normalize_tool_call_id =
        move |id: &str, _target: &Model, source: &AssistantMessage| -> String {
            if !allowed_providers
                .iter()
                .any(|provider| provider == &target_provider)
            {
                return normalize_id_part(id);
            }
            if !id.contains('|') {
                return normalize_id_part(id);
            }
            let mut parts = id.split('|');
            let call_id = parts.next().unwrap_or("");
            let item_id = parts.next().unwrap_or("");
            let normalized_call_id = normalize_id_part(call_id);
            // 来源 provider/api 与目标不同即视为外部调用：item id 不能复用，改为哈希。
            let is_foreign_tool_call =
                source.provider != target_provider || source.api != target_api;
            let mut normalized_item_id = if is_foreign_tool_call {
                build_foreign_responses_item_id(item_id)
            } else {
                normalize_id_part(item_id)
            };
            if !normalized_item_id.starts_with("fc_") {
                normalized_item_id = normalize_id_part(&format!("fc_{normalized_item_id}"));
            }
            format!("{normalized_call_id}|{normalized_item_id}")
        };
    let callback: &NormalizeToolCallIdFn = &normalize_tool_call_id;
    let transformed = transform_messages_with(source_messages, model, Some(callback));

    // 工具支持等级决定后续如何表达工具变更；`folded_system_messages` 是开头已经被
    // 顶层工具列表覆盖的 system 消息条数，之后的 system 消息才需要生成变更项。
    let tool_support = get_responses_tool_support(options);
    let transcript_tools = resolve_transcript_tools(source_messages, tool_support);
    let mut declared_additional_tools = false;
    let mut unavailable_tools: IndexSet<String> = IndexSet::new();
    let include_initial_system_message = options.include_system_prompt.unwrap_or(true);
    let instruction_role = if model.reasoning == Some(true) && supports_developer_role(model) {
        "developer"
    } else {
        "system"
    };
    let grammar_properties = options.grammar_tool_input_properties.as_ref();

    // 两个计数器用途不同：`msg_index` 只数「非首条 system」消息（用作稳定 id），
    // `system_index` 数所有 system 消息（用于判断是否已越过 folded 前缀）。
    let mut msg_index: i64 = 0;
    let mut system_index: usize = 0;
    for (source_index, message) in transformed.iter().enumerate() {
        // 首条 system 消息是「基础 prompt」；其余 system 消息是对话中途的指令更新，
        // 渲染成 update 文本并可能携带工具变更。
        let is_leading_system_message = source_index == 0 && matches!(message, Message::System(_));
        match message {
            Message::System(system) => {
                let anchors_tool_changes = system_index >= transcript_tools.folded_system_messages;
                system_index += 1;
                if anchors_tool_changes {
                    append_system_tool_additions(
                        &mut messages,
                        system,
                        &format!("system:{msg_index}"),
                        tool_support,
                        instruction_role,
                        options,
                        &mut unavailable_tools,
                        &mut declared_additional_tools,
                    )?;
                }
                // includeSystemPrompt=false 时只丢弃首条 system 消息本身；工具变更
                // 锚点在上面已经处理过，不受影响。
                if !is_leading_system_message || include_initial_system_message {
                    let mut parts = vec![if is_leading_system_message {
                        get_system_message_text(system)
                    } else {
                        render_system_message_update(system)
                    }];
                    if anchors_tool_changes {
                        // 先去掉本次又声明过的工具；剩下的才是真正被移除的。
                        // 这些名字同时累积进 unavailable_tools，供后续增量提示使用。
                        let added: std::collections::HashSet<&str> = system
                            .tools_added
                            .iter()
                            .flatten()
                            .map(|tool| tool.name.as_str())
                            .collect();
                        let removed: Vec<&str> = system
                            .tools_removed
                            .iter()
                            .flatten()
                            .map(|tool| tool.name.as_str())
                            .filter(|name| !added.contains(name))
                            .collect();
                        if !removed.is_empty() {
                            parts.push(render_tool_removal_notice(&removed));
                        }
                        for name in &removed {
                            unavailable_tools.insert((*name).to_string());
                        }
                    }
                    let text = parts
                        .into_iter()
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    if !text.is_empty() {
                        messages.push(json_object(vec![
                            ("role", Value::String(instruction_role.to_string())),
                            ("content", Value::String(sanitize_surrogates(&text))),
                        ]));
                    }
                }
            }
            Message::User(user) => match &user.content {
                UserContent::Text(text) => {
                    messages.push(json_object(vec![
                        ("role", Value::String("user".into())),
                        (
                            "content",
                            Value::Array(vec![json_object(vec![
                                ("type", Value::String("input_text".into())),
                                ("text", Value::String(sanitize_surrogates(text))),
                            ])]),
                        ),
                    ]));
                }
                UserContent::Blocks(blocks) => {
                    let mut content: Vec<Value> = Vec::with_capacity(blocks.len());
                    for block in blocks {
                        match block {
                            UserContentBlock::Text(text) => content.push(json_object(vec![
                                ("type", Value::String("input_text".into())),
                                ("text", Value::String(sanitize_surrogates(&text.text))),
                            ])),
                            UserContentBlock::Image(image) => content.push(json_object(vec![
                                ("type", Value::String("input_image".into())),
                                ("detail", Value::String("auto".into())),
                                (
                                    "image_url",
                                    Value::String(format!(
                                        "data:{};base64,{}",
                                        image.mime_type, image.data
                                    )),
                                ),
                            ])),
                        }
                    }
                    if content.is_empty() {
                        continue;
                    }
                    messages.push(json_object(vec![
                        ("role", Value::String("user".into())),
                        ("content", Value::Array(content)),
                    ]));
                }
            },
            Message::Assistant(assistant) => {
                // assistant 内容逐块回放：thinking 原样、text 变成 `message` 项、
                // tool call 变成 `function_call` / `custom_tool_call`。
                let mut output: Vec<Value> = Vec::new();
                let is_same_provider_and_api =
                    assistant.provider == model.provider && assistant.api == model.api;
                let is_same_model = is_same_provider_and_api && assistant.model == model.id;
                let is_different_model = is_same_provider_and_api && assistant.model != model.id;
                let mut text_block_index: usize = 0;
                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Thinking(thinking) => {
                            if let Some(signature) = &thinking.thinking_signature
                                && !signature.is_empty()
                            {
                                let item =
                                    kiana_json::js_parse::parse(signature).map_err(|error| {
                                        AiError::new(AiErrorKind::Parse, error.message)
                                    })?;
                                output.push(item);
                            }
                        }
                        AssistantContentBlock::Text(text) => {
                            // 签名里存着上一轮服务端给的 message id 与 phase；没有签名时
                            // 用稳定 fallback id，超长 id 用短哈希压到 64 字符以内。
                            let parsed_signature =
                                parse_text_signature(text.text_signature.as_deref());
                            let fallback_message_id = if text_block_index == 0 {
                                format!("msg_kiana_{msg_index}")
                            } else {
                                format!("msg_kiana_{msg_index}_{text_block_index}")
                            };
                            text_block_index += 1;
                            let mut message_id = parsed_signature
                                .as_ref()
                                .map(|signature| signature.id.clone())
                                .unwrap_or_default();
                            if message_id.is_empty() {
                                message_id = fallback_message_id;
                            } else if utf16_len(&message_id) > 64 {
                                message_id = format!("msg_{}", short_hash(&message_id));
                            }
                            let mut item = Map::new();
                            item.insert("type".into(), Value::String("message".into()));
                            item.insert("role".into(), Value::String("assistant".into()));
                            item.insert(
                                "content".into(),
                                Value::Array(vec![json_object(vec![
                                    ("type", Value::String("output_text".into())),
                                    ("text", Value::String(sanitize_surrogates(&text.text))),
                                    ("annotations", Value::Array(Vec::new())),
                                ])]),
                            );
                            item.insert("status".into(), Value::String("completed".into()));
                            item.insert("id".into(), Value::String(message_id));
                            if let Some(phase) = parsed_signature
                                .as_ref()
                                .and_then(|signature| signature.phase)
                            {
                                item.insert(
                                    "phase".into(),
                                    Value::String(
                                        match phase {
                                            TextSignaturePhase::Commentary => "commentary",
                                            TextSignaturePhase::FinalAnswer => "final_answer",
                                        }
                                        .to_string(),
                                    ),
                                );
                            }
                            output.push(Value::Object(item));
                        }
                        AssistantContentBlock::ToolCall(tool_call) => {
                            // id 拆成 call_id 与 item_id 两段。item_id 的保留条件是
                            // `!is_different_model` 且前缀匹配（`fc_`/`ctc_`）；
                            // `is_different_model` 仅指同 provider/api 但模型 id 不同，
                            // 此时丢弃，因为服务端不认旧模型的 item。跨 provider/api 的 id
                            // 已在上一步的 id 归一化回调里改写成 `fc_<hash>`（或被整体
                            // 清洗掉），这里拿到的是改写后的值。
                            let mut id_parts = tool_call.id.split('|');
                            let call_id = id_parts.next().unwrap_or("").to_string();
                            let item_id_raw = id_parts.next().map(str::to_string);
                            let custom_input_property = grammar_properties
                                .and_then(|properties| properties.get(&tool_call.name))
                                .cloned();
                            let item_id_prefix = if custom_input_property.is_some() {
                                "ctc_"
                            } else {
                                "fc_"
                            };
                            let item_id = match item_id_raw {
                                Some(item_id)
                                    if !is_different_model
                                        && item_id.starts_with(item_id_prefix) =>
                                {
                                    Some(item_id)
                                }
                                _ => None,
                            };
                            match custom_input_property {
                                Some(property) => {
                                    let arguments: Map<String, Value> = tool_call
                                        .arguments
                                        .iter()
                                        .map(|(key, value)| (key.clone(), value.clone()))
                                        .collect();
                                    let input = get_grammar_tool_input(
                                        &tool_call.name,
                                        &arguments,
                                        &property,
                                    )?;
                                    let mut item = Map::new();
                                    item.insert(
                                        "type".into(),
                                        Value::String("custom_tool_call".into()),
                                    );
                                    if let Some(item_id) = &item_id {
                                        item.insert("id".into(), Value::String(item_id.clone()));
                                    }
                                    item.insert("call_id".into(), Value::String(call_id));
                                    item.insert(
                                        "name".into(),
                                        Value::String(tool_call.name.clone()),
                                    );
                                    item.insert(
                                        "input".into(),
                                        Value::String(sanitize_surrogates(&input)),
                                    );
                                    if is_same_model && let Some(namespace) = &tool_call.namespace {
                                        item.insert(
                                            "namespace".into(),
                                            Value::String(namespace.clone()),
                                        );
                                    }
                                    output.push(Value::Object(item));
                                }
                                None => {
                                    let mut item = Map::new();
                                    item.insert(
                                        "type".into(),
                                        Value::String("function_call".into()),
                                    );
                                    if let Some(item_id) = &item_id {
                                        item.insert("id".into(), Value::String(item_id.clone()));
                                    }
                                    item.insert("call_id".into(), Value::String(call_id));
                                    item.insert(
                                        "name".into(),
                                        Value::String(tool_call.name.clone()),
                                    );
                                    item.insert(
                                        "arguments".into(),
                                        Value::String(js_stringify(&json_from_object(
                                            &tool_call.arguments,
                                        ))),
                                    );
                                    if is_same_model && let Some(namespace) = &tool_call.namespace {
                                        item.insert(
                                            "namespace".into(),
                                            Value::String(namespace.clone()),
                                        );
                                    }
                                    output.push(Value::Object(item));
                                }
                            }
                        }
                    }
                }
                if output.is_empty() {
                    continue;
                }
                // 配置更新插在 assistant 输出之前：告知服务端这条回复使用的 effort；
                // 只有同模型回放才带（换模型后旧 effort 不适用）。
                if options.configuration_update_effort.is_some()
                    && is_same_model
                    && let Some(level) = &assistant.provider_thinking_level
                {
                    push_configuration_update(&mut messages, level);
                }
                messages.extend(output);
            }
            Message::ToolResult(result) => {
                // tool result 只回 `call_id`（`|` 之前那一段）；输出形状由
                // convert_tool_result_output 按模型的图片能力决定。
                let call_id = result
                    .tool_call_id
                    .split('|')
                    .next()
                    .unwrap_or("")
                    .to_string();
                let output = convert_tool_result_output(model, &result.content);
                let is_custom = grammar_properties
                    .is_some_and(|properties| properties.contains_key(&result.tool_name));
                messages.push(json_object(vec![
                    (
                        "type",
                        Value::String(
                            if is_custom {
                                "custom_tool_call_output"
                            } else {
                                "function_call_output"
                            }
                            .to_string(),
                        ),
                    ),
                    ("call_id", Value::String(call_id)),
                    ("output", output),
                ]));
            }
        }
        if !is_leading_system_message {
            msg_index += 1;
        }
    }
    if let Some(effort) = &options.configuration_update_effort {
        // 整段输入末尾再补一次 effort 更新（选项显式指定时），供本轮请求生效。
        push_configuration_update(&mut messages, effort);
    }
    Ok(messages)
}

// =============================================================================
// 流处理
// =============================================================================

// 流处理的状态模型（对应 TS `processResponsesStream` 的闭包状态）：
//
// - `output_slots`：`output_index` → 槽位。Responses 的增量事件只带 `output_index`，
//   必须先建槽才知道写哪个 content block；键用 `Option<i64>` 保留「事件缺字段」。
// - `SlotKind`：槽位类型。类型不匹配的 delta 一律忽略（对应 TS 的
//   `getSlot(index, type)` 返回 undefined）。
// - `unfinished_tool_calls`：还没收到 `output_item.done` 的 tool call 内容块下标；
//   流正常结束且 `stop_reason == ToolUse` 时若仍有残留，说明协议被截断，需要报错。
// - `reasoning_blocks_by_id`：reasoning item id → content 下标；终态响应里的
//   `encrypted_content` 只能通过 id 找回来补进签名。
//
// 流式解析器/缓冲区放在 `SlotInput` 而不是 content block 里：每条 delta 事件都会
// 克隆一份完整消息作为 partial 快照，解析器状态不属于公开消息形状，不应混进去。

/// 工具调用的流式状态（不挂在 content block 上，因此不会出现在 partial 快照里）。
#[allow(clippy::large_enum_variant)] // 与 TS 状态对象一一对应；大变体是流式 JSON 解析器。
enum SlotInput {
    /// 普通 function tool：累积 JSON 参数文本的增量解析器。
    Json(ToolCallArgumentsStream),
    /// grammar tool：服务端给的是单个字符串，需要经过增量 JSON 编码器转成
    /// `{ "<property>": "<input>" }` 形状的 JSON 增量。
    Custom {
        property: String,
        buffer: GrammarToolInputJsonBuffer,
        /// 首块 delta 里尚未发送的 JSON 文本前缀。
        unsent: String,
    },
}

/// 槽位对应的内容块类型；`Slot::get` 用它做类型校验。
#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotKind {
    Thinking,
    Text,
    ToolCall,
}

/// 一个 `output_index` 对应的流式状态：内容块下标 + 工具调用的解析器状态。
struct Slot {
    kind: SlotKind,
    content_index: usize,
    input: Option<SlotInput>,
}

/// 克隆当前消息作为 partial 快照；事件流消费者只能看到克隆时刻的状态。
fn partial(output: &AssistantMessage) -> Box<AssistantMessage> {
    Box::new(output.clone())
}

fn output_index(event: &Value) -> Option<i64> {
    field(event, "output_index").and_then(Value::as_i64)
}

fn event_delta(event: &Value) -> Option<String> {
    field(event, "delta")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// `customInput(property, initialInput)`。
///
/// 初始化 grammar 工具的输入缓冲：把 `initialInput` 喂进增量 JSON 编码器，返回的首块
/// JSON 前缀暂存到 `unsent`（等第一个 delta 事件时一起推送）。
fn custom_input(property: &str, initial_input: &str) -> Result<SlotInput, AiError> {
    let mut buffer = GrammarToolInputJsonBuffer::default();
    let unsent = append_grammar_tool_input_json_delta(&mut buffer, property, initial_input)?
        .unwrap_or_default();
    Ok(SlotInput::Custom {
        property: property.to_string(),
        buffer,
        unsent,
    })
}

/// `sendCustomInput(block, input, jsonDelta)` 里更新 block.arguments 的部分。
///
/// custom tool 只有一个字符串参数：用缓冲区的最新 input 整体覆盖 `block.arguments`
/// （不是追加）。返回值是要推送的增量 JSON 文本：若还有 `unsent` 前缀，就与本次 delta
/// 合并后一次性发出并清空；没有可发内容时返回 `None`。
fn send_custom_input(
    block_arguments: &mut JsonObject,
    property: &str,
    buffer: &GrammarToolInputJsonBuffer,
    unsent: &mut String,
    json_delta: Option<String>,
) -> Option<String> {
    let mut arguments = JsonObject::new();
    arguments.insert(property.to_string(), Value::String(buffer.input.clone()));
    *block_arguments = arguments;
    if unsent.is_empty() {
        return json_delta;
    }
    let sent = format!("{}{}", unsent, json_delta.unwrap_or_default());
    unsent.clear();
    Some(sent)
}

/// `createSlot(outputIndex, item)`。
///
/// 为 `output_index` 建槽并推送对应的 start 事件。返回新内容块的下标；`item.type`
/// 未知时返回 `None` 且不建槽。`function_call` / `custom_tool_call` 会同时登记到
/// `unfinished_tool_calls`，等 `output_item.done` 时移除。
///
/// `custom_tool_call` 的输入属性名从 `grammar_tool_input_properties` 查，缺失时回退为
/// `"input"`；这与消息转换侧不同：那里查不到工具名时按普通 function 处理，没有
/// `"input"` 回退。
fn create_slot(
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    slots: &mut HashMap<Option<i64>, Slot>,
    unfinished_tool_calls: &mut IndexSet<usize>,
    grammar_tool_input_properties: Option<&IndexMap<String, String>>,
    key: Option<i64>,
    item: &Value,
) -> Result<Option<usize>, AiError> {
    match field(item, "type").and_then(Value::as_str) {
        Some("reasoning") => {
            output
                .content
                .push(AssistantContentBlock::Thinking(ThinkingContent {
                    thinking: String::new(),
                    ..ThinkingContent::default()
                }));
            let content_index = output.content.len() - 1;
            slots.insert(
                key,
                Slot {
                    kind: SlotKind::Thinking,
                    content_index,
                    input: None,
                },
            );
            let _ = stream.push(AssistantMessageEvent::ThinkingStart {
                content_index,
                partial: partial(output),
            });
            Ok(Some(content_index))
        }
        Some("message") => {
            if field(item, "phase").and_then(Value::as_str) == Some("final_answer") {
                output.stop_reason = StopReason::Stop;
            }
            output.content.push(empty_text_block());
            let content_index = output.content.len() - 1;
            slots.insert(
                key,
                Slot {
                    kind: SlotKind::Text,
                    content_index,
                    input: None,
                },
            );
            let _ = stream.push(AssistantMessageEvent::TextStart {
                content_index,
                partial: partial(output),
            });
            Ok(Some(content_index))
        }
        Some("function_call") => {
            let mut arguments = ToolCallArgumentsStream::new();
            let initial = match non_empty_str(field(item, "arguments")) {
                Some(text) => arguments.append(text),
                None => JsonObject::new(),
            };
            let block = ToolCall {
                id: format!(
                    "{}|{}",
                    js_string(field(item, "call_id")),
                    js_string(field(item, "id"))
                ),
                name: field(item, "name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                arguments: initial,
                namespace: field(item, "namespace")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                ..ToolCall::default()
            };
            output.content.push(AssistantContentBlock::ToolCall(block));
            let content_index = output.content.len() - 1;
            unfinished_tool_calls.insert(content_index);
            slots.insert(
                key,
                Slot {
                    kind: SlotKind::ToolCall,
                    content_index,
                    input: Some(SlotInput::Json(arguments)),
                },
            );
            let _ = stream.push(AssistantMessageEvent::ToolCallStart {
                content_index,
                partial: partial(output),
            });
            Ok(Some(content_index))
        }
        Some("custom_tool_call") => {
            let name = field(item, "name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let property = grammar_tool_input_properties
                .and_then(|properties| properties.get(&name))
                .cloned()
                .unwrap_or_else(|| "input".to_string());
            let input = field(item, "input").and_then(Value::as_str).unwrap_or("");
            let mut arguments = JsonObject::new();
            arguments.insert(property.clone(), Value::String(input.to_string()));
            let block = ToolCall {
                id: format!(
                    "{}|{}",
                    js_string(field(item, "call_id")),
                    js_string(field(item, "id"))
                ),
                name,
                arguments,
                namespace: field(item, "namespace")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                ..ToolCall::default()
            };
            output.content.push(AssistantContentBlock::ToolCall(block));
            let content_index = output.content.len() - 1;
            unfinished_tool_calls.insert(content_index);
            let slot_input = custom_input(&property, input)?;
            slots.insert(
                key,
                Slot {
                    kind: SlotKind::ToolCall,
                    content_index,
                    input: Some(slot_input),
                },
            );
            let _ = stream.push(AssistantMessageEvent::ToolCallStart {
                content_index,
                partial: partial(output),
            });
            Ok(Some(content_index))
        }
        _ => Ok(None),
    }
}

/// `TextContent { type: "text", text: "" }`。
fn empty_text_block() -> AssistantContentBlock {
    AssistantContentBlock::Text(TextContent {
        text: String::new(),
        ..TextContent::default()
    })
}

impl Slot {
    fn get(&self, kind: SlotKind) -> Option<usize> {
        (self.kind == kind).then_some(self.content_index)
    }
}

/// `backfillReasoningSignatures(responseOutput)`。
///
/// 流式阶段 `output_item.done` 存下的 reasoning 签名是当时的 item JSON，可能不含
/// `encrypted_content`；终态响应的 `output` 数组里才带上它。本函数按 item id 找到对应
/// 的 thinking 块，把 `encrypted_content` 合入已存储的 JSON（已有则跳过），供下一轮
/// 原样回放。签名不是合法 JSON、签名为空、块不存在、id 缺失等情况一律跳过，不报错。
fn backfill_reasoning_signatures(
    response_output: &[Value],
    output: &mut AssistantMessage,
    reasoning_blocks_by_id: &HashMap<String, usize>,
) {
    for item in response_output {
        if field(item, "type").and_then(Value::as_str) != Some("reasoning") {
            continue;
        }
        let Some(encrypted) = field(item, "encrypted_content").filter(|value| truthy(Some(value)))
        else {
            continue;
        };
        let Some(id) = field(item, "id").and_then(Value::as_str) else {
            continue;
        };
        let Some(&index) = reasoning_blocks_by_id.get(id) else {
            continue;
        };
        let Some(AssistantContentBlock::Thinking(block)) = output.content.get_mut(index) else {
            continue;
        };
        let Some(signature) = block
            .thinking_signature
            .as_deref()
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Ok(mut stored) = kiana_json::js_parse::parse(signature) else {
            continue;
        };
        if truthy(field(&stored, "encrypted_content")) {
            continue;
        }
        match &mut stored {
            Value::Object(map) => {
                map.insert("encrypted_content".into(), encrypted.clone());
            }
            _ => {
                stored = json_object(vec![("encrypted_content", encrypted.clone())]);
            }
        }
        block.thinking_signature = Some(js_stringify(&stored));
    }
}

/// `mapStopReason(status, incompleteReason?)`。
///
/// | status | 结果 |
/// |---|---|
/// | 缺失 / 空串 / `completed` / `in_progress` / `queued` | `Stop` |
/// | `incomplete` + `max_output_tokens` | `Length` |
/// | `incomplete` + 其他 reason | `Error` + `"Response incomplete: {reason}"` |
/// | `incomplete` + 无 reason | `Error` + `"Response incomplete without a provider reason"` |
/// | `failed` / `cancelled` | `Error`（无 error_message） |
/// | 其他 | `Err`：`"Unhandled stop reason: {status}"` |
///
/// `in_progress` / `queued` 映射成 `Stop` 看起来反直觉，但这是 TS 的原样行为；本文件只
/// 在 `response.completed` / `response.incomplete` 的 `response.status` 上调用它。
fn map_stop_reason(
    status: Option<&str>,
    incomplete_reason: Option<&str>,
) -> Result<(StopReason, Option<String>), AiError> {
    let Some(status) = status.filter(|status| !status.is_empty()) else {
        return Ok((StopReason::Stop, None));
    };
    match status {
        "completed" => Ok((StopReason::Stop, None)),
        "incomplete" => {
            if incomplete_reason == Some("max_output_tokens") {
                return Ok((StopReason::Length, None));
            }
            match incomplete_reason.filter(|reason| !reason.is_empty()) {
                Some(reason) => Ok((
                    StopReason::Error,
                    Some(format!("Response incomplete: {reason}")),
                )),
                None => Ok((
                    StopReason::Error,
                    Some("Response incomplete without a provider reason".to_string()),
                )),
            }
        }
        "failed" | "cancelled" => Ok((StopReason::Error, None)),
        "in_progress" | "queued" => Ok((StopReason::Stop, None)),
        other => Err(AiError::new(
            AiErrorKind::Other,
            format!("Unhandled stop reason: {other}"),
        )),
    }
}

/// `finalizeResponse(response)`。
///
/// 终态事件（`response.completed` / `response.incomplete`）到达时结算消息：
///
/// 1. 用终态 `output` 补全 reasoning 签名里的 `encrypted_content`；
/// 2. 记录 `response_id`；
/// 3. 换算 usage：`input` 要**减去** cached 与 cache-write（两者单列），减法按 i128
///    做并夹到 0（provider 报的 `input_tokens` 小于缓存计数时也不会下溢）；然后按模型
///    价格 `calculate_cost`；若配置了 service tier 定价，再应用倍率；
/// 4. `raw_stop_reason` 保留 provider 原始状态（`incomplete` 时是
///    `"{status}.{reason}"`），`stop_reason` 经 `map_stop_reason` 映射；
/// 5. 内容里有 tool call 且映射结果为 `Stop` 时改判 `ToolUse`（TS 同样覆盖）。
///
/// 状态码未知会在第 4 步返回 `Err`，其余情况只写 `output`。
fn finalize_response(
    response: &Value,
    output: &mut AssistantMessage,
    model: &Model,
    options: Option<&OpenAIResponsesStreamOptions>,
    reasoning_blocks_by_id: &HashMap<String, usize>,
) -> Result<(), AiError> {
    let response_output: Vec<Value> = field(response, "output")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    backfill_reasoning_signatures(&response_output, output, reasoning_blocks_by_id);
    if let Some(id) = field(response, "id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        output.response_id = Some(id.to_string());
    }
    if let Some(usage) = field(response, "usage").filter(|value| truthy(Some(value))) {
        let input_details = field(usage, "input_tokens_details");
        let cached_tokens =
            usage_count(input_details.and_then(|details| field(details, "cached_tokens")));
        let cache_write_tokens =
            usage_count(input_details.and_then(|details| field(details, "cache_write_tokens")));
        let input_tokens = usage_count(field(usage, "input_tokens"));
        // input 与 total_tokens 口径不同：input 扣除缓存命中与缓存写入（两者单列），
        // total_tokens 直接采用 provider 报的值。
        let input =
            (i128::from(input_tokens) - i128::from(cached_tokens) - i128::from(cache_write_tokens))
                .max(0) as u64;
        output.usage = Usage {
            input,
            output: usage_count(field(usage, "output_tokens")),
            cache_read: cached_tokens,
            cache_write: cache_write_tokens,
            cache_write1h: None,
            reasoning: Some(usage_count(
                field(usage, "output_tokens_details")
                    .and_then(|details| field(details, "reasoning_tokens")),
            )),
            total_tokens: usage_count(field(usage, "total_tokens")),
            cost: UsageCost::default(),
        };
    }
    calculate_cost(model, &mut output.usage);
    if let Some(options) = options
        && let Some(apply) = &options.apply_service_tier_pricing
    {
        let response_tier = field(response, "service_tier").and_then(Value::as_str);
        let service_tier = match &options.resolve_service_tier {
            Some(resolve) => resolve(response_tier, options.service_tier.as_deref()),
            None => response_tier
                .map(str::to_string)
                .or_else(|| options.service_tier.clone()),
        };
        apply(&mut output.usage, service_tier.as_deref());
    }
    let status = field(response, "status").and_then(Value::as_str);
    let incomplete_reason = field(response, "incomplete_details")
        .and_then(|details| field(details, "reason"))
        .and_then(Value::as_str);
    // raw_stop_reason 保留 provider 的原始表达；incomplete 时拼成 `status.reason`，
    // 便于在诊断里区分「没输出完」的具体原因。
    output.raw_stop_reason = match incomplete_reason.filter(|reason| !reason.is_empty()) {
        Some(reason) => Some(format!(
            "{}.{}",
            js_string(field(response, "status")),
            reason
        )),
        None => status.map(str::to_string),
    };
    let (stop_reason, error_message) = map_stop_reason(status, incomplete_reason)?;
    output.stop_reason = stop_reason;
    output.error_message = error_message;
    if output
        .content
        .iter()
        .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)))
        && output.stop_reason == StopReason::Stop
    {
        // 有工具调用时把 completed 的 Stop 改判成 ToolUse（TS 同样覆盖）。
        output.stop_reason = StopReason::ToolUse;
    }
    Ok(())
}

/// `replace(event.arguments)` 中 `text.startsWith(...)` 对非字符串值抛出的 V8 TypeError 文案。
///
/// 三种文案对应 `arguments` 缺失（`undefined`）、`null`、以及其他非字符串值；它们会
/// 直接成为用户可见错误，必须逐字保留。
fn non_string_arguments_type_error(value: Option<&Value>) -> String {
    match value {
        Some(Value::Null) => "Cannot read properties of null (reading 'startsWith')".to_string(),
        None => "Cannot read properties of undefined (reading 'startsWith')".to_string(),
        Some(_) => "text.startsWith is not a function".to_string(),
    }
}

/// `processResponsesStream(openaiStream, output, stream, model, options?)`。
///
/// 消费 Responses 事件流，把增量写进 `output`，同时向 `stream` 推送
/// Thinking/Text/ToolCall 的 start/delta/end 事件。成功返回表示已收到终态事件并完成
/// 结算；任何 `Err` 都意味着流不可用（调用方据此生成 error 事件）。
///
/// # 状态
///
/// 见本节开头的状态模型：`output_slots`、`unfinished_tool_calls`、
/// `reasoning_blocks_by_id`，以及 `saw_terminal_response_event`。槽位以事件的
/// `output_index` 为键；同一 index 的 `output_item.done` 会复用已有槽位，缺失则按
/// item 补建（TS 的 `getOrCreateSlot`）。
///
/// # 事件处理（按前缀分组）
///
/// - `response.created`：记录 response id；
/// - `response.output_item.added`：建槽并推 start；
/// - `*.delta`：找到类型匹配的槽位后追加；找不到槽位就**静默忽略**（TS 的
///   `getSlot(...)` 返回 undefined 时同样不报错）；
/// - `response.function_call_arguments.done`：用完整 arguments 替换累积值；`arguments`
///   不是字符串时按 V8 的 TypeError 文案报错；
/// - `response.custom_tool_call_input.delta/done`：维护 grammar 工具的 JSON 缓冲；
/// - `response.output_item.done`：用完整 item 覆盖块内容、写签名/namespace、推 end 并
///   删除槽位；
/// - `response.completed` / `response.incomplete`：标记终态并 `finalize_response`；
/// - `error` / `response.failed`：直接返回 `Err`（消息格式见分支）。
///
/// # 终止条件与失败路径
///
/// 循环自然结束后：
/// 1. 没收到终态响应事件 → `"OpenAI Responses stream ended before a terminal response event"`；
/// 2. `stop_reason == ToolUse` 但 `unfinished_tool_calls` 非空 → 报出第一个未完成调用的
///    名称与 id。
///
/// 事件本身是 `Err`（SSE 解码/传输错误）时直接 `?` 透传，不做包装。
pub async fn process_responses_stream(
    mut events: ProviderEventStream,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    model: &Model,
    options: Option<&OpenAIResponsesStreamOptions>,
) -> Result<(), AiError> {
    let mut saw_terminal_response_event = false;
    let mut output_slots: HashMap<Option<i64>, Slot> = HashMap::new();
    let mut unfinished_tool_calls: IndexSet<usize> = IndexSet::new();
    let mut reasoning_blocks_by_id: HashMap<String, usize> = HashMap::new();
    let grammar_tool_input_properties =
        options.and_then(|options| options.grammar_tool_input_properties.as_ref());

    // 事件按 `type` 分派；未匹配的类型落到 `_ => {}`，不会报错。
    while let Some(event) = events.next().await {
        let event = event?;
        // 观测回调在状态更新之前执行，拿到的是事件副本，返回值被忽略。
        if let Some(on_event) =
            options.and_then(|options| options.on_provider_stream_event.as_ref())
        {
            on_event(event.clone(), model);
        }
        let event_type = field(&event, "type").and_then(Value::as_str).unwrap_or("");
        match event_type {
            "response.created" => {
                if let Some(id) = field(&event, "response")
                    .and_then(|response| field(response, "id"))
                    .and_then(Value::as_str)
                {
                    output.response_id = Some(id.to_string());
                }
            }
            "response.output_item.added" => {
                let item = field(&event, "item").cloned().unwrap_or(Value::Null);
                create_slot(
                    output,
                    stream,
                    &mut output_slots,
                    &mut unfinished_tool_calls,
                    grammar_tool_input_properties,
                    output_index(&event),
                    &item,
                )?;
            }
            // delta 类事件要求槽位已存在且类型匹配；不匹配时 continue（静默忽略）。
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let Some(index) = output_slots
                    .get(&output_index(&event))
                    .and_then(|slot| slot.get(SlotKind::Thinking))
                else {
                    continue;
                };
                let delta = event_delta(&event).unwrap_or_default();
                if let Some(AssistantContentBlock::Thinking(block)) = output.content.get_mut(index)
                {
                    block.thinking.push_str(&delta);
                }
                let _ = stream.push(AssistantMessageEvent::ThinkingDelta {
                    content_index: index,
                    delta,
                    partial: partial(output),
                });
            }
            "response.reasoning_summary_part.done" => {
                let Some(index) = output_slots
                    .get(&output_index(&event))
                    .and_then(|slot| slot.get(SlotKind::Thinking))
                else {
                    continue;
                };
                if let Some(AssistantContentBlock::Thinking(block)) = output.content.get_mut(index)
                {
                    block.thinking.push_str("\n\n");
                }
                let _ = stream.push(AssistantMessageEvent::ThinkingDelta {
                    content_index: index,
                    delta: "\n\n".to_string(),
                    partial: partial(output),
                });
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                let Some(index) = output_slots
                    .get(&output_index(&event))
                    .and_then(|slot| slot.get(SlotKind::Text))
                else {
                    continue;
                };
                let delta = event_delta(&event).unwrap_or_default();
                if let Some(AssistantContentBlock::Text(block)) = output.content.get_mut(index) {
                    block.text.push_str(&delta);
                }
                let _ = stream.push(AssistantMessageEvent::TextDelta {
                    content_index: index,
                    delta,
                    partial: partial(output),
                });
            }
            "response.function_call_arguments.delta" => {
                let key = output_index(&event);
                let Some(index) = output_slots
                    .get(&key)
                    .and_then(|slot| slot.get(SlotKind::ToolCall))
                else {
                    continue;
                };
                let delta = event_delta(&event).unwrap_or_default();
                let Some(slot) = output_slots.get_mut(&key) else {
                    continue;
                };
                let Some(SlotInput::Json(arguments)) = slot.input.as_mut() else {
                    continue;
                };
                let appended = arguments.append(&delta);
                if let Some(AssistantContentBlock::ToolCall(block)) = output.content.get_mut(index)
                {
                    block.arguments = appended;
                }
                let _ = stream.push(AssistantMessageEvent::ToolCallDelta {
                    content_index: index,
                    delta,
                    partial: partial(output),
                });
            }
            "response.function_call_arguments.done" => {
                let key = output_index(&event);
                let Some(index) = output_slots
                    .get(&key)
                    .and_then(|slot| slot.get(SlotKind::ToolCall))
                else {
                    continue;
                };
                let Some(slot) = output_slots.get_mut(&key) else {
                    continue;
                };
                let Some(SlotInput::Json(arguments)) = slot.input.as_mut() else {
                    continue;
                };
                // TS 无条件执行 `slot.input.arguments.replace(event.arguments)`；缺失 / null /
                // 非字符串的 `arguments` 会在 `text.startsWith(...)` 处抛 TypeError。
                let arguments_value = field(&event, "arguments");
                let Some(text) = arguments_value.and_then(Value::as_str) else {
                    return Err(AiError::new(
                        AiErrorKind::Other,
                        non_string_arguments_type_error(arguments_value),
                    ));
                };
                let replaced = arguments.replace(text);
                if let Some(AssistantContentBlock::ToolCall(block)) = output.content.get_mut(index)
                {
                    block.arguments = replaced.arguments;
                }
                if let Some(delta) = replaced.delta.filter(|delta| !delta.is_empty()) {
                    let _ = stream.push(AssistantMessageEvent::ToolCallDelta {
                        content_index: index,
                        delta,
                        partial: partial(output),
                    });
                }
            }
            "response.custom_tool_call_input.delta" => {
                let key = output_index(&event);
                let Some(index) = output_slots
                    .get(&key)
                    .and_then(|slot| slot.get(SlotKind::ToolCall))
                else {
                    continue;
                };
                let delta = event_delta(&event).unwrap_or_default();
                let Some(slot) = output_slots.get_mut(&key) else {
                    continue;
                };
                let Some(SlotInput::Custom {
                    property,
                    buffer,
                    unsent,
                }) = slot.input.as_mut()
                else {
                    continue;
                };
                let json_delta = append_grammar_tool_input_json_delta(buffer, property, &delta)?;
                let Some(AssistantContentBlock::ToolCall(block)) = output.content.get_mut(index)
                else {
                    continue;
                };
                if let Some(sent) =
                    send_custom_input(&mut block.arguments, property, buffer, unsent, json_delta)
                {
                    let _ = stream.push(AssistantMessageEvent::ToolCallDelta {
                        content_index: index,
                        delta: sent,
                        partial: partial(output),
                    });
                }
            }
            "response.custom_tool_call_input.done" => {
                let key = output_index(&event);
                let Some(index) = output_slots
                    .get(&key)
                    .and_then(|slot| slot.get(SlotKind::ToolCall))
                else {
                    continue;
                };
                let final_input = field(&event, "input").and_then(Value::as_str);
                let Some(slot) = output_slots.get_mut(&key) else {
                    continue;
                };
                let Some(SlotInput::Custom {
                    property,
                    buffer,
                    unsent,
                }) = slot.input.as_mut()
                else {
                    continue;
                };
                let json_delta = close_grammar_tool_input_json(buffer, property, final_input)?;
                let Some(AssistantContentBlock::ToolCall(block)) = output.content.get_mut(index)
                else {
                    continue;
                };
                if let Some(sent) =
                    send_custom_input(&mut block.arguments, property, buffer, unsent, json_delta)
                {
                    let _ = stream.push(AssistantMessageEvent::ToolCallDelta {
                        content_index: index,
                        delta: sent,
                        partial: partial(output),
                    });
                }
            }
            "response.output_item.done" => {
                let item = field(&event, "item").cloned().unwrap_or(Value::Null);
                let item_type = field(&item, "type").and_then(Value::as_str).unwrap_or("");
                if item_type == "message"
                    && field(&item, "phase").and_then(Value::as_str) == Some("final_answer")
                {
                    output.stop_reason = StopReason::Stop;
                }
                let key = output_index(&event);
                // 先查槽；没有就按 item 补建（TS 的 getOrCreateSlot）。
                let existing = output_slots
                    .get(&key)
                    .map(|slot| (slot.kind, slot.content_index));
                let slot = match existing {
                    Some(slot) => Some(slot),
                    None => {
                        let created = create_slot(
                            output,
                            stream,
                            &mut output_slots,
                            &mut unfinished_tool_calls,
                            grammar_tool_input_properties,
                            key,
                            &item,
                        )?;
                        created.map(|index| {
                            let kind = output_slots
                                .get(&key)
                                .map(|slot| slot.kind)
                                .unwrap_or(SlotKind::Text);
                            (kind, index)
                        })
                    }
                };
                let Some((slot_kind, content_index)) = slot else {
                    continue;
                };
                match item_type {
                    "reasoning" if slot_kind == SlotKind::Thinking => {
                        // 最终文本优先取 summary，其次 content；两者都空时保留流式累积的
                        // thinking。签名存整条 item JSON，终态响应到达后再补 encrypted_content。
                        let summary_text = field(&item, "summary")
                            .and_then(Value::as_array)
                            .map(|items| {
                                items
                                    .iter()
                                    .map(|summary| {
                                        field(summary, "text")
                                            .and_then(Value::as_str)
                                            .unwrap_or("")
                                            .to_string()
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n\n")
                            })
                            .unwrap_or_default();
                        let content_text = field(&item, "content")
                            .and_then(Value::as_array)
                            .map(|items| {
                                items
                                    .iter()
                                    .map(|content| {
                                        field(content, "text")
                                            .and_then(Value::as_str)
                                            .unwrap_or("")
                                            .to_string()
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n\n")
                            })
                            .unwrap_or_default();
                        let signature = js_stringify(&item);
                        let item_id = field(&item, "id").and_then(Value::as_str);
                        if let Some(AssistantContentBlock::Thinking(block)) =
                            output.content.get_mut(content_index)
                        {
                            block.thinking = if !summary_text.is_empty() {
                                summary_text
                            } else if !content_text.is_empty() {
                                content_text
                            } else {
                                block.thinking.clone()
                            };
                            block.thinking_signature = Some(signature);
                        }
                        if let Some(item_id) = item_id {
                            reasoning_blocks_by_id.insert(item_id.to_string(), content_index);
                        }
                        let content = match output.content.get(content_index) {
                            Some(AssistantContentBlock::Thinking(block)) => block.thinking.clone(),
                            _ => String::new(),
                        };
                        let _ = stream.push(AssistantMessageEvent::ThinkingEnd {
                            content_index,
                            content,
                            partial: partial(output),
                        });
                        output_slots.remove(&key);
                    }
                    "message" if slot_kind == SlotKind::Text => {
                        // content 数组里混有 output_text 与 refusal 项：分别取 text /
                        // refusal 字段，用空串连接（不是 "\n"）。
                        let text = field(&item, "content")
                            .and_then(Value::as_array)
                            .map(|items| {
                                items
                                    .iter()
                                    .map(|content| {
                                        if field(content, "type").and_then(Value::as_str)
                                            == Some("output_text")
                                        {
                                            field(content, "text")
                                                .and_then(Value::as_str)
                                                .unwrap_or("")
                                                .to_string()
                                        } else {
                                            field(content, "refusal")
                                                .and_then(Value::as_str)
                                                .unwrap_or("")
                                                .to_string()
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join("")
                            })
                            .unwrap_or_default();
                        let phase = match field(&item, "phase").and_then(Value::as_str) {
                            Some("commentary") => Some(TextSignaturePhase::Commentary),
                            Some("final_answer") => Some(TextSignaturePhase::FinalAnswer),
                            _ => None,
                        };
                        let signature = encode_text_signature_v1(
                            field(&item, "id").and_then(Value::as_str),
                            phase,
                        );
                        if let Some(AssistantContentBlock::Text(block)) =
                            output.content.get_mut(content_index)
                        {
                            block.text = text;
                            block.text_signature = Some(signature);
                        }
                        let content = match output.content.get(content_index) {
                            Some(AssistantContentBlock::Text(block)) => block.text.clone(),
                            _ => String::new(),
                        };
                        let _ = stream.push(AssistantMessageEvent::TextEnd {
                            content_index,
                            content,
                            partial: partial(output),
                        });
                        output_slots.remove(&key);
                    }
                    "function_call"
                        if slot_kind == SlotKind::ToolCall
                            && matches!(
                                output_slots.get(&key).and_then(|slot| slot.input.as_ref()),
                                Some(SlotInput::Json(_))
                            ) =>
                    {
                        if let Some(text) = non_empty_str(field(&item, "arguments"))
                            && let Some(SlotInput::Json(arguments)) = output_slots
                                .get_mut(&key)
                                .and_then(|slot| slot.input.as_mut())
                        {
                            arguments.replace(text);
                        }
                        let finished = output_slots
                            .get(&key)
                            .and_then(|slot| slot.input.as_ref())
                            .and_then(|input| match input {
                                SlotInput::Json(arguments) => Some(arguments.finish()),
                                SlotInput::Custom { .. } => None,
                            });
                        if let Some(finished) = finished
                            && let Some(AssistantContentBlock::ToolCall(block)) =
                                output.content.get_mut(content_index)
                        {
                            block.arguments = finished.arguments;
                            block.arguments_error = finished.arguments_error;
                            if let Some(namespace) =
                                field(&item, "namespace").and_then(Value::as_str)
                            {
                                block.namespace = Some(namespace.to_string());
                            }
                        }
                        unfinished_tool_calls.shift_remove(&content_index);
                        let tool_call = match output.content.get(content_index) {
                            Some(AssistantContentBlock::ToolCall(block)) => block.clone(),
                            _ => ToolCall::default(),
                        };
                        let _ = stream.push(AssistantMessageEvent::ToolCallEnd {
                            content_index,
                            tool_call,
                            partial: partial(output),
                        });
                        output_slots.remove(&key);
                    }
                    "custom_tool_call"
                        if slot_kind == SlotKind::ToolCall
                            && matches!(
                                output_slots.get(&key).and_then(|slot| slot.input.as_ref()),
                                Some(SlotInput::Custom { .. })
                            ) =>
                    {
                        let final_input = field(&item, "input").and_then(Value::as_str);
                        let json_delta = {
                            let slot = output_slots.get_mut(&key);
                            match slot.and_then(|slot| slot.input.as_mut()) {
                                Some(SlotInput::Custom {
                                    property,
                                    buffer,
                                    unsent,
                                }) => {
                                    let json_delta = close_grammar_tool_input_json(
                                        buffer,
                                        property,
                                        final_input,
                                    )?;
                                    let block = output.content.get_mut(content_index);
                                    match block {
                                        Some(AssistantContentBlock::ToolCall(block)) => {
                                            send_custom_input(
                                                &mut block.arguments,
                                                property,
                                                buffer,
                                                unsent,
                                                json_delta,
                                            )
                                        }
                                        _ => json_delta,
                                    }
                                }
                                _ => None,
                            }
                        };
                        if let Some(sent) = json_delta {
                            let _ = stream.push(AssistantMessageEvent::ToolCallDelta {
                                content_index,
                                delta: sent,
                                partial: partial(output),
                            });
                        }
                        if let Some(AssistantContentBlock::ToolCall(block)) =
                            output.content.get_mut(content_index)
                            && let Some(namespace) =
                                field(&item, "namespace").and_then(Value::as_str)
                        {
                            block.namespace = Some(namespace.to_string());
                        }
                        unfinished_tool_calls.shift_remove(&content_index);
                        let tool_call = match output.content.get(content_index) {
                            Some(AssistantContentBlock::ToolCall(block)) => block.clone(),
                            _ => ToolCall::default(),
                        };
                        let _ = stream.push(AssistantMessageEvent::ToolCallEnd {
                            content_index,
                            tool_call,
                            partial: partial(output),
                        });
                        output_slots.remove(&key);
                    }
                    _ => {}
                }
            }
            // 终态事件：标记已收到，交给 finalize_response 结算；其错误继续向上传播。
            "response.completed" | "response.incomplete" => {
                saw_terminal_response_event = true;
                let response = field(&event, "response").cloned().unwrap_or(Value::Null);
                finalize_response(&response, output, model, options, &reasoning_blocks_by_id)?;
            }
            "error" => {
                // 恒为 "Error Code {code}: {message}"，缺失字段会渲染成 "undefined"。
                let message = format!(
                    "Error Code {}: {}",
                    js_string(field(&event, "code")),
                    js_string(field(&event, "message"))
                );
                return Err(AiError::new(AiErrorKind::Other, message));
            }
            "response.failed" => {
                // 不经过 finalize_response：先记录 provider 状态供诊断，再按
                // error / incomplete_details / 兜底三级文案构造错误。
                let response = field(&event, "response").cloned().unwrap_or(Value::Null);
                output.raw_stop_reason = field(&response, "status")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let error = field(&response, "error");
                let details = field(&response, "incomplete_details");
                let message = if let Some(error) = error.filter(|value| truthy(Some(value))) {
                    let code = match field(error, "code").filter(|value| truthy(Some(value))) {
                        Some(value) => js_string(Some(value)),
                        None => "unknown".to_string(),
                    };
                    let detail = match field(error, "message").filter(|value| truthy(Some(value))) {
                        Some(value) => js_string(Some(value)),
                        None => "no message".to_string(),
                    };
                    format!("{code}: {detail}")
                } else if let Some(reason) = details
                    .and_then(|details| field(details, "reason"))
                    .filter(|value| truthy(Some(value)))
                {
                    format!("incomplete: {}", js_string(Some(reason)))
                } else {
                    "Unknown error (no error details in response)".to_string()
                };
                return Err(AiError::new(AiErrorKind::Other, message));
            }
            _ => {}
        }
    }
    // 正常 EOF 的两项一致性检查：必须先见到终态事件；ToolUse 时不允许有未完成的
    // 工具调用（否则调用方会收到未结算的 tool call）。
    if !saw_terminal_response_event {
        return Err(AiError::new(
            AiErrorKind::Other,
            "OpenAI Responses stream ended before a terminal response event",
        ));
    }
    if output.stop_reason == StopReason::ToolUse {
        for index in &unfinished_tool_calls {
            if let Some(AssistantContentBlock::ToolCall(block)) = output.content.get(*index) {
                return Err(AiError::new(
                    AiErrorKind::Other,
                    format!(
                        "OpenAI Responses stream completed with an unfinished tool call: {} ({})",
                        block.name, block.id
                    ),
                ));
            }
        }
    }
    Ok(())
}

// =============================================================================
// 传输层：header 组装与 URL（responses / azure 共用）
// =============================================================================

/// 大小写不敏感的 header 合并表（`None` 表示删除，重设移动到末尾）。
///
/// 键是**小写**的 header 名（用于大小写不敏感查找），值是 `(原始写法, 值)`：回传给
/// `HttpClient` 时保留调用方给出的原始大小写。最后设置的 header 排在最后，因此
/// `header_list` 的顺序等于「最后一次设置的顺序」。
pub(crate) type HeaderTable = IndexMap<String, (String, String)>;

/// 设置一个 header（后写覆盖先写，并移到末尾）。
///
/// 先 `shift_remove` 再 `insert`：既保证同名不同大小写只留一份，也让该 header 移到
/// 末尾（保持「最后一次设置」的顺序）。
pub(crate) fn set_header(table: &mut HeaderTable, name: &str, value: &str) {
    let lower = name.to_ascii_lowercase();
    table.shift_remove(&lower);
    table.insert(lower, (name.to_string(), value.to_string()));
}

/// 合并 `IndexMap<String, String>` 形式的 header。
///
/// 按迭代顺序逐个 `set_header`：同名的后写覆盖先写（大小写不敏感）。
pub(crate) fn merge_plain_headers(table: &mut HeaderTable, headers: &IndexMap<String, String>) {
    for (name, value) in headers {
        set_header(table, name, value);
    }
}

/// 合并 `ProviderHeaders`（`None` 删除同名 header）。
///
/// `None` 是**显式删除**（用于清掉模型或上层注入的默认头），不是「跳过」；因此不能用
/// `flatten` 之类的写法丢掉这批条目。
pub(crate) fn merge_provider_headers(
    table: &mut HeaderTable,
    headers: &crate::types::ProviderHeaders,
) {
    for (name, value) in headers {
        match value {
            Some(value) => set_header(table, name, value),
            None => {
                table.shift_remove(&name.to_ascii_lowercase());
            }
        }
    }
}

/// 取出 header 列表（顺序为最后一次设置的顺序）。
pub(crate) fn header_list(table: HeaderTable) -> Vec<(String, String)> {
    table.into_values().collect()
}

/// 默认 `HttpClient`（`options.fetch` 缺省时）。
///
/// 注入点：`StreamOptions.fetch` 给了就直接用（适配器与测试的自定义传输）。
/// 否则进程内只构造一次 reqwest 客户端并缓存到 `OnceLock`，复用连接池；
/// `ReqwestHttpClient::new()` 失败（极少数 TLS 后端初始化失败）时退回 `Default`
/// （等价于 `reqwest::Client::new()`）；该签名没有 `Result`，没有别的失败通道。
pub(crate) fn default_http_client(options: &crate::types::StreamOptions) -> SharedHttpClient {
    if let Some(fetch) = &options.fetch {
        return fetch.clone();
    }
    static DEFAULT: std::sync::OnceLock<SharedHttpClient> = std::sync::OnceLock::new();
    DEFAULT
        .get_or_init(
            || match crate::http::reqwest_client::ReqwestHttpClient::new() {
                Ok(client) => client.shared(),
                Err(_) => Arc::new(crate::http::reqwest_client::ReqwestHttpClient::default()),
            },
        )
        .clone()
}

/// SDK 的 `buildURL(path, query, defaultBaseURL)`（`OpenAIClient.buildURL`）。
///
/// TS 的行为：先 `new URL(baseURL + path)`（两边都以 `/` 结尾/开头时去掉 path 开头的
/// `/`），再用 `Object.fromEntries(url.searchParams)` 取出已有 query，与 `defaultQuery`
/// （Azure 的 `api-version`）依次浅合并（同名后者覆盖、位置保持首次出现处），最后用
/// `qs.stringify(..., { arrayFormat: "brackets" })`（RFC3986，只保留 `A-Za-z0-9-._~`）
/// 重新序列化并赋给 `url.search`。URL 本身由 WHATWG 解析器归一化。
///
/// 与 TS 一样，`path` 只可能是相对路径（`/chat/completions`、`/responses`），因此没有
/// `isAbsoluteURL(path)` 分支。`new URL` 失败时 TS 抛 `TypeError: Invalid URL`。
pub(crate) fn build_url(
    base_url: &str,
    path: &str,
    default_query: &[(&str, &str)],
) -> Result<String, AiError> {
    let combined = if base_url.ends_with('/') && path.starts_with('/') {
        format!("{}{}", base_url, &path[1..])
    } else {
        format!("{base_url}{path}")
    };
    let mut url = url::Url::parse(&combined)
        .map_err(|_| AiError::new(AiErrorKind::Other, INVALID_URL_MESSAGE))?;
    // `Object.fromEntries(url.searchParams)`：重复键后者取值、顺序按首次出现。
    let mut query: IndexMap<String, String> = IndexMap::new();
    for (key, value) in url.query_pairs() {
        query.insert(key.into_owned(), value.into_owned());
    }
    // `{ ...pathQuery, ...defaultQuery, ...query }`（调用方 query 缺省）。
    for (key, value) in default_query {
        query.insert((*key).to_string(), (*value).to_string());
    }
    if !query.is_empty() {
        url.set_query(Some(&stringify_query(&query)));
    }
    Ok(url.to_string())
}

/// `qs.stringify(query, { arrayFormat: "brackets" })` 对「字符串 → 字符串」对象的结果。
fn stringify_query(query: &IndexMap<String, String>) -> String {
    let mut out = String::new();
    for (index, (key, value)) in query.iter().enumerate() {
        if index > 0 {
            out.push('&');
        }
        out.push_str(&encode_query_component(key));
        out.push('=');
        out.push_str(&encode_query_component(value));
    }
    out
}

/// qs 的 RFC3986 编码（`internal/qs/utils.js#encode`）：只保留 `A-Za-z0-9-._~`，
/// 其余字符按 UTF-8 百分号编码（大写十六进制）。
fn encode_query_component(text: &str) -> String {
    use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
    const RFC3986: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~');
    utf8_percent_encode(text, RFC3986).to_string()
}

/// `Date.now()` 的毫秒时间戳。
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

/// `{ status }` 的 `ProviderResponse`（`onResponse` 用）。
pub(crate) fn provider_response(
    status: u16,
    headers: &IndexMap<String, String>,
) -> crate::types::ProviderResponse {
    crate::types::ProviderResponse {
        status,
        headers: crate::utils::headers::headers_to_record(headers),
    }
}

#[cfg(test)]
mod golden {
    //! 与 TS 参考实现（`/home/ctyun/kiana-work/upstream-reference`）逐字节对照的转换黄金测试。
    //!
    //! fixture 是 `tests/fixtures/api-openai/conversions.json`（TS 侧的输入/输出快照）；
    //! 断言用 `js_stringify` 比较整个数组，因此字段顺序、默认值与数字格式都受锁定。

    use super::*;
    use crate::types::TranscriptContext;

    const FIXTURE: &str = include_str!("../../tests/fixtures/api-openai/conversions.json");

    #[test]
    fn convert_responses_messages_matches_typescript_reference() {
        let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture parses");
        let model: Model =
            serde_json::from_value(fixture["responsesModel"].clone()).expect("responses model");
        let context: TranscriptContext =
            serde_json::from_value(fixture["context"].clone()).expect("context");
        let responses = &fixture["responses"];
        let grammar: IndexMap<String, String> =
            serde_json::from_value(responses["grammarToolInputProperties"].clone())
                .expect("grammar map");
        let tool_options: ConvertResponsesToolsOptions =
            serde_json::from_value(responses["toolOptions"].clone()).unwrap_or_default();
        let allowed: Vec<String> =
            serde_json::from_value(responses["allowedToolCallProviders"].clone()).expect("allowed");
        let allowed: Vec<&str> = allowed.iter().map(String::as_str).collect();
        let options = ConvertResponsesMessagesOptions {
            include_system_prompt: None,
            grammar_tool_input_properties: Some(grammar),
            supports_mid_convo_system_messages: Some(
                responses["supportsMidConvoSystemMessages"]
                    .as_bool()
                    .unwrap_or(false),
            ),
            supports_additional_tools: Some(
                responses["supportsAdditionalTools"]
                    .as_bool()
                    .unwrap_or(false),
            ),
            supports_tool_search: Some(responses["supportsToolSearch"].as_bool().unwrap_or(false)),
            tool_options: Some(tool_options),
            configuration_update_effort: responses["configurationUpdateEffort"]
                .as_str()
                .map(str::to_string),
        };
        let actual = convert_responses_messages(&model, &context, &allowed, Some(&options))
            .expect("convert");
        assert_eq!(
            js_stringify(&Value::Array(actual)),
            js_stringify(&responses["input"])
        );
    }
}

#[cfg(test)]
mod sse_tests {
    //! OpenAI SDK 的 SSE 语义（`LineDecoder` + `SSEDecoder` + `_iterSSEMessages`）。
    //!
    //! 覆盖分帧（CRLF、多行 data、EOF flush）、`[DONE]` 停止、`data.error`、非法 JSON、
    //! `thread.` 信封，以及「signal 已取消时吞掉 AbortError、否则照常上报」。

    use futures::StreamExt;

    use super::*;

    fn body(chunks: Vec<Result<Vec<u8>, HttpError>>) -> ByteStream {
        Box::pin(futures::stream::iter(chunks))
    }

    async fn collect(chunks: Vec<Result<Vec<u8>, HttpError>>) -> Vec<Result<Value, AiError>> {
        let mut stream = provider_event_stream(body(chunks), None);
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event);
        }
        events
    }

    #[tokio::test]
    async fn decodes_crlf_comments_multiline_data_and_stops_at_done() {
        let events = collect(vec![
            Ok(b"data: {\"a\":1}\r\n\r\n: comment\r\ndata: {\"b\":".to_vec()),
            Ok(b"2}\r\n\r\ndata: [DONE]\r\n\r\ndata: {\"ignored\":true}\r\n\r\n".to_vec()),
        ])
        .await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].as_ref().unwrap()["a"], Value::from(1));
        assert_eq!(events[1].as_ref().unwrap()["b"], Value::from(2));

        // 多行 data 用 `\n` 连接；末尾没有空行时 flush 仍补发。
        let events = collect(vec![Ok(b"data: {\"c\":\ndata: 3}".to_vec())]).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].as_ref().unwrap()["c"], Value::from(3));
    }

    #[tokio::test]
    async fn treats_data_error_payloads_as_api_errors() {
        let events = collect(vec![Ok(
            b"data: {\"error\":{\"message\":\"boom\",\"code\":\"bad\"}}\n\n".to_vec(),
        )])
        .await;
        assert_eq!(events.len(), 1);
        let error = events[0].as_ref().unwrap_err();
        assert_eq!(error.message, "boom");
        assert_eq!(error.detail.as_ref().unwrap()["code"], Value::from("bad"));
    }

    #[tokio::test]
    async fn rejects_malformed_event_json_with_the_sdk_message() {
        let events = collect(vec![Ok(b"data: not-json\n\n".to_vec())]).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].as_ref().unwrap_err().message, MALFORMED_SSE_JSON);
    }

    #[tokio::test]
    async fn yields_thread_events_as_envelopes() {
        let events = collect(vec![Ok(
            b"event: thread.message\ndata: {\"x\":1}\n\n".to_vec()
        )])
        .await;
        let event = events[0].as_ref().unwrap();
        assert_eq!(event["event"], Value::from("thread.message"));
        assert_eq!(event["data"]["x"], Value::from(1));
    }

    #[tokio::test]
    async fn swallows_transport_abort_errors_only_when_the_signal_is_cancelled() {
        let signal = tokio_util::sync::CancellationToken::new();
        signal.cancel();
        let mut stream = provider_event_stream(
            body(vec![Err(HttpError::new(
                crate::http::reqwest_client::ABORTED_MESSAGE,
            ))]),
            Some(signal),
        );
        assert!(stream.next().await.is_none());

        let mut stream = provider_event_stream(
            body(vec![Err(HttpError::new(
                crate::http::reqwest_client::ABORTED_MESSAGE,
            ))]),
            None,
        );
        let event = stream.next().await.expect("error is surfaced");
        assert_eq!(
            event.unwrap_err().message,
            crate::http::reqwest_client::ABORTED_MESSAGE
        );
    }
}
