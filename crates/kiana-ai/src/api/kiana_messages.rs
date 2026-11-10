#![allow(clippy::result_large_err)]
//! api/kiana-messages.ts——kiana 自己的消息协议直连后端。
//!
//! 请求是 `{ model, context, options }` 的单次 POST 到 `<baseUrl>/messages`，响应是
//! 一串序列化的 assistant-message 事件（SSE）加一个终态 `done`/`error` 事件。
//! Radius 网关说的就是这套协议，任何实现它的后端都可以用（例如 models.json 里
//! `"api": "kiana-messages"` 的自定义 provider）。
//!
//! 与 TS 的差异只在错误类型上：TS 用 `Error` 子类，Rust 用 [`KianaMessagesResponseError`]；
//! `Display` 逐字等于 TS 的 `error.message`。
//!
//! # 为什么需要这个模块
//!
//! Kiana 有一个自有的、比各家服务商原生协议更窄的「消息协议」：客户端只发
//! `{ model, context, options }`，服务端把一条 assistant 消息拆成增量事件推回来。它不绑定
//! 某一家模型厂商，Radius 网关以及 `models.json` 里声明 `"api": "kiana-messages"` 的自定义
//! provider 都走它。本文件就是这个协议的客户端适配器。
//!
//! # 被谁调用
//!
//! - [`stream`] / [`stream_simple`] 是适配器入口。`providers/radius.rs` 通过
//!   [`kiana_messages_api`] 把本文件注册成 `"kiana-messages"` API，`compat.rs` 的内置 API
//!   注册表登记同一个工厂。
//! - 传输辅助函数的使用者并不相同：`Utf8StreamDecoder`、`read_body_text`、
//!   `resolve_client`、`reason_phrase` 由 `mistral_conversations` 与 `google_shared` 复用；
//!   `json_object` 只由 `bedrock_converse_stream` 复用；`next_chunk` 三者都用（bedrock 经
//!   自己的薄包装转发）。因此本文件也是这些适配器的共享底层。
//!
//! # 一次请求的执行流程
//!
//! 1. [`stream`] 同步返回一个 [`AssistantMessageEventStream`]，并用 `spawn_forward` 把
//!    `run_stream` 放到后台任务执行（当前没有 tokio runtime 时退化为专用线程 +
//!    current-thread runtime，见 `api::lazy::spawn_forward`）。
//! 2. `run_stream` 校验 API key、拼 `<baseUrl>/messages`（`debug` 时追加 `?debug=1`）、
//!    构造 payload、调用 `onPayload`、合并请求头，然后调用 `HttpClient::execute`。
//! 3. 非 2xx：读完整 body，构造 [`KianaMessagesResponseError`]，转成终态 error 事件。
//! 4. 2xx：`read_kiana_messages_events` 按 `"\n\n"` 切 SSE 帧，每帧解析第一行 `data:`；
//!    `onProviderStreamEvent` 先看到原始 JSON，再经 `EventConverter` 归一化成
//!    [`AssistantMessageEvent`] 推入事件流。
//! 5. `done` / `error` 是终态：读到后立即停止读取，`push` 同时提交 `result()`。
//! 6. 流正常结束却没有终态事件 → 生成 `"<provider> stream ended without a terminal event"`。
//!
//! 本适配器**不做重试**：单次请求，`StreamOptions::max_retries` 等字段被忽略；是否重试由上层调用方决定。
//!
//! # 协议约束
//!
//! - 帧分隔符只认 `"\n\n"`；每轮先把 `"\r\n"` 规范化成 `"\n"`。一帧只取第一行以
//!   `"data:"` 开头的行，多行 `data:` 不会拼接。`[DONE]` 与空 data 被忽略。
//! - 事件 JSON 走 `kiana_json::js_parse`，**接受范围**与 V8 `JSON.parse` 一致：孤立代理项、
//!   溢出数字等 serde_json 会拒绝的输入也能解析。但解析出的值有已知表示偏差（见
//!   `crates/kiana-json/src/js_parse.rs` 与 `ARCHITECTURE.md` §8 第 31/32 条）：孤立代理项
//!   被替换为 U+FFFD，非有限数保存为 `null`，因此并非所有观察都与 V8 等价。
//! - `contentIndex` 接受 JSON 数字，也接受规范数组索引字符串（`"0"`、`"12"`），对应
//!   JS 里 `arr["0"]` 与 `arr[0]` 是同一个属性。
//! - 请求体用 `kiana_json::js_object::stringify` 序列化，与 `JSON.stringify` 一样丢弃
//!   `undefined`（Rust 侧用 `Option` 缺失表达），非有限数字输出 `null`。
//!
//! # 与 TS 实现的关键取舍
//!
//! - TS 的 `partial.content` 是 JS 数组，可以有空洞（`arr[3] = x` 后 0..2 是
//!   `undefined`）。Rust 的 `Vec` 没有空洞，`EventConverter` 用占位 `Text` 块 + `holes`
//!   集合模拟；对空洞做 `text += ...` / `Object.assign` 时按 V8 的 TypeError 文案返回错误。
//! - [`AssistantMessageEvent`] 是封闭枚举，未知线上事件类型无法原样透传；它们不进入事件
//!   流，但 `onProviderStreamEvent` 仍能看到原始 JSON。
//! - 缺失/类型不符的字段按宽松规则取值（不丢事件），`done`/`error` 永远生效；未知 reason
//!   的映射见 `stop_reason_from_wire` / `done_reason_from_wire`。
//!
//! # 错误传播
//!
//! `run_stream` 内部用 `StreamFailure` 区分「HTTP 响应错误」（保留诊断字段）、「调用方
//! 取消」与「其它错误」。所有错误最终都由 [`stream`] 的后台任务转成一个终态 `error`
//! 事件；调用方从返回值上看不到 `Err`（[`stream_simple`] 的 `Result` 是按 crate 统一契约
//! 保留的签名，本实现永远返回 `Ok`）。取消令牌会中断仍在等待的 HTTP 请求与流读取
//! （`HttpClient::execute`、`read_body_text`、`next_chunk` 都观察同一个令牌），最终以
//! `Aborted` 结束；已经 push 的事件不会撤回，未处理的残余 buffer 随读取循环退出而丢弃。

use std::sync::Arc;

use futures::StreamExt;
use serde_json::{Map, Value};

use crate::api::lazy::{lazy_stream_error, spawn_forward};
use crate::error::{AiError, AiErrorKind};
use crate::event_stream::AssistantMessageEventStream;
use crate::http::{ByteStream, HttpClient, HttpError, HttpRequest};
use crate::models::ProviderStreams;
use crate::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageDiagnostic, AssistantMessageEvent,
    AssistantRole, CacheRetention, DiagnosticErrorInfo, DoneReason, ErrorReason, JsonObject, Model,
    SimpleStreamOptions, StopReason, StreamOptions, TextContent, TextKind, ThinkingContent,
    ThinkingKind, ThinkingLevel, ToolCall, ToolCallKind, TranscriptContext, Usage, UsageCost,
};
use crate::utils::diagnostics::{
    ThrownValue, append_assistant_message_diagnostic, create_assistant_message_diagnostic,
};
use crate::utils::headers::provider_headers_to_record;
use crate::utils::identity::{ClientIdentityTarget, get_identity_headers};
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::streaming_json::ToolCallArgumentsStream;
use crate::utils::text::owned_slice;

/// `KianaMessagesOptions`：`kiana-messages` 适配器的完整选项。
///
/// 通常由 [`kiana_messages_options_from_stream_options`] 从通用 [`StreamOptions`] 构造。
/// 这里未列出的 `StreamOptions` 字段（如 `max_retries`、`timeout_ms`）不会进入请求体，
/// 也不参与本适配器的重试/超时控制。
#[derive(Default)]
pub struct KianaMessagesOptions {
    /// 通用选项：API key、请求头、取消令牌，以及 `onPayload` / `onResponse` /
    /// `onProviderStreamEvent` 回调与 `fetch` 注入点都在这里。
    pub base: StreamOptions,
    /// 思考档位，序列化成 `options.reasoning`（`minimal`/`low`/`medium`/`high`/
    /// `xhigh`/`max`）。
    pub reasoning: Option<ThinkingLevel>,
    /// `"auto" | "none" | "required" | { type: "function"; function: { name } }`。
    ///
    /// 原样放进 `options.toolChoice`，本适配器不解释其形状（由网关或后端校验）。
    pub tool_choice: Option<Value>,
    /// 让后端返回调试元数据（例如路由响应头）。
    ///
    /// 实现方式是在 URL 上追加 `?debug=1`，而不是改请求体。
    pub debug: bool,
}

/// `KianaMessagesRewriteImpact`：服务端消息重写（例如网关策略）的影响摘要。
///
/// 对应线上 `done`/`error` 事件里可选的 `rewrite` 字段。适配器不会把它反序列化成这个
/// 结构体，而是把 `rewrite` 的原始 JSON 键值整体放进诊断 details（见
/// `append_rewrite_diagnostic`）；本类型只描述形状。字段按 camelCase 序列化（线上协议字段名）。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KianaMessagesRewriteImpact {
    /// 触发重写的策略标识。
    pub policy_id: String,
    /// 策略版本。协议用 JSON 数字表达，用 `f64` 保真（协议没有声明必须是整数）。
    pub policy_version: f64,
    /// 服务端是否实际改写了消息。
    pub changed: bool,
    /// token 数变化量（可以为负）。
    pub token_count_change: f64,
    /// 消息条数变化量（可以为负）。
    pub message_count_change: f64,
    /// 系统提示是否被改写。
    pub system_prompt_changed: bool,
}

/// `KianaMessagesResponseError`（`extends Error`）。
///
/// 只在收到非 2xx 响应时构造，随后由 `StreamFailure::Response` 带到 `create_error_event`，
/// 最终写进 assistant 消息的 `errorMessage` 与 `kiana_messages_response_failure` 诊断；
/// 不会作为 `Err` 返回给 [`stream`] 的调用方。
#[derive(Debug, Clone, PartialEq)]
pub struct KianaMessagesResponseError {
    /// 已拼好的错误文本；`Display` 逐字返回它（等于 TS 的 `error.message`）。
    pub message: String,
    /// 响应体 `error.code` 的字符串副本；非字符串或缺失为 `None`。
    pub code: Option<String>,
    /// 诊断 details（`version`/`provider`/`model`/`url`/`status`/`statusText` 及条件键），
    /// 键序与 TS 的对象字面量一致。
    pub diagnostic_details: JsonObject,
}

impl std::fmt::Display for KianaMessagesResponseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for KianaMessagesResponseError {}

/// `parseKianaMessagesErrorBody(body)`：`JSON.parse` 成功且 `parsed.error` 是**非 null 非数组对象**
/// 时返回解析结果，否则 `None`（catch 返回 `undefined`）。
///
/// `JSON.parse` 用 [`kiana_json::js_parse`]（V8 等价）：孤立代理项转义等输入 V8 接受，serde_json 会拒绝。
///
/// 返回的是**整个** parsed 对象而不是 `error` 字段，因为调用方还需要 `error.message` /
/// `error.code`。`Value::is_object` 同时排除 null、数组与原始值，与 TS 的
/// `typeof error === "object" && error !== null && !Array.isArray(error)` 等价。
fn parse_kiana_messages_error_body(body: &str) -> Option<Value> {
    let parsed: Value = kiana_json::js_parse::parse(body).ok()?;
    match parsed.get("error") {
        Some(error) if error.is_object() => Some(parsed),
        _ => None,
    }
}

/// `truncateDiagnosticString(value)`：长度（UTF-16 码元）> 8192 → 切片 + `…`。
///
/// 8192 与 TS 相同。长度按 UTF-16 码元计（对应 JS 的 `string.length`），切片用
/// `utils::text::owned_slice`：边界落在代理对中间时会多保留一个码元，不产生孤立代理项
/// （ARCHITECTURE §4.6）。未超长时原样返回，不做任何转义。
fn truncate_diagnostic_string(value: &str) -> String {
    const MAX_LENGTH: usize = 8192;
    if value.encode_utf16().count() > MAX_LENGTH {
        format!("{}…", owned_slice(value, 0, Some(MAX_LENGTH)))
    } else {
        value.to_string()
    }
}

/// `formatKianaMessagesResponseError(response, body, errorBody)`。
///
/// 文本形状固定为 `"<status> <statusText>: <message 或 body>[ (<code>)]"`。
/// `message` 只接受字符串（数字/对象等回落到整个 body）；空字符串仍是合法 message
/// （TS 的 `??` 只跳过 null/undefined）。空 code 不追加后缀（TS 的真值判断）。
fn format_kiana_messages_response_error(
    status: u16,
    status_text: &str,
    body: &str,
    error_body: Option<&Value>,
) -> String {
    let error = error_body.and_then(|parsed| parsed.get("error"));
    let message = error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str);
    let code = error
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str);
    let suffix = message.unwrap_or(body);
    let code_suffix = match code {
        Some(code) if !code.is_empty() => format!(" ({code})"),
        _ => String::new(),
    };
    format!("{status} {status_text}: {suffix}{code_suffix}")
}

/// `createKianaMessagesResponseError(model, url, response, body)`。
///
/// details 的键序与 spec/ai-api-other.md §2.10 的 TS 对象字面量一致（`JsonObject` =
/// `IndexMap` 保序）。`error` 与 `body` 是互斥的条件键：未得到对象形式的 `error`（JSON
/// 解析失败，或解析成功但 `error` 不是对象）时加 `body`；解析成功且 `error` 是对象时加
/// `error`；两者不会同时出现。`timestampMs` 用当前 Unix 毫秒（`Date.now()` 的等价物，
/// 见 `now_unix_ms`）。
fn create_kiana_messages_response_error(
    model: &Model,
    url: &str,
    status: u16,
    status_text: &str,
    body: &str,
    error_body: Option<&Value>,
) -> KianaMessagesResponseError {
    let error = error_body.and_then(|parsed| parsed.get("error"));
    let code = error
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let message = format_kiana_messages_response_error(status, status_text, body, error_body);

    // 键顺序与 TS 的对象字面量一致：version 在最前，随后是 provider/model/url/status/
    // statusText，然后是互斥的条件键（error 或 body，二者总有一个），最后 timestampMs。
    let mut details = JsonObject::new();
    details.insert("version".into(), Value::from(1));
    details.insert("provider".into(), Value::String(model.provider.clone()));
    details.insert("model".into(), Value::String(model.id.clone()));
    details.insert("url".into(), Value::String(url.to_string()));
    details.insert("status".into(), Value::from(status));
    details.insert("statusText".into(), Value::String(status_text.to_string()));
    if let Some(error) = error {
        details.insert("error".into(), error.clone());
    }
    if error_body.is_none() {
        details.insert(
            "body".into(),
            Value::String(truncate_diagnostic_string(body)),
        );
    }
    details.insert("timestampMs".into(), Value::from(now_unix_ms()));

    KianaMessagesResponseError {
        message,
        code,
        diagnostic_details: details,
    }
}

/// `appendRewriteDiagnostic(message, rewrite)`。
///
/// TS 是 `if (!rewrite) return;` + `details: { ...rewrite }`：任何真值都追加诊断，
/// details 就是对象自有的可枚举键（形状不对时可能是空对象），不做 schema 校验。
///
/// Rust 只对 `Value::Object` 逐键复制，其它真值统一给空对象；JS 的对象展开对字符串/数组
/// 还会产生 `0`、`1`… 下标键。线上协议里 `rewrite` 按 `KianaMessagesRewriteImpact` 是
/// 对象，正常路径不会出现这种输入。
fn append_rewrite_diagnostic(message: &mut AssistantMessage, rewrite: Option<&Value>) {
    let Some(rewrite) = rewrite.filter(|value| js_truthy(value)) else {
        return;
    };
    let details: JsonObject = match rewrite {
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        _ => JsonObject::new(),
    };
    append_assistant_message_diagnostic(
        message,
        AssistantMessageDiagnostic {
            kind: "kiana_messages_rewrite".into(),
            timestamp: now_unix_ms(),
            error: None,
            details: Some(details),
        },
    );
}

/// JS 的真值判断（`if (!rewrite)`）。
///
/// 只处理 `serde_json::Value` 能表达的值：`null`/`false`/`0`（含 `-0.0`，Rust 里
/// `-0.0 == 0.0`）/空串为假，数组与对象恒为真（JS 里空数组、空对象也是真值）。JSON 没有
/// `undefined` 与 `NaN`，因此不需要处理这两类。
fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// 单条消息 content 的最大块数。
///
/// JS 的稀疏数组可以在 2^32-2 处赋值而不分配内存；Rust 的 `Vec` 用占位块填洞，而每个
/// 事件都会克隆整份 `partial`。把上限压到 1024：真实响应远用不到这么多块，而恶意
/// `contentIndex: 65535` + 上万条 delta 的分配放大从「每次 4MB」降到「每次 64KB」。
const MAX_CONTENT_INDEX: usize = 1_024;

/// JS 的数组下标：`undefined` / 非整数 / 负数都按 `undefined` 处理。
///
/// 只接受有限、非负、整数值，并应用与 [`canonical_array_index`] 相同的上限。
/// 否则不可信的 `contentIndex`（如 `4000000000`）会让 `set_block` 填充海量占位块，
/// 耗尽内存。
fn as_index(value: f64) -> Option<usize> {
    if value.is_finite() && value >= 0.0 && value.fract() == 0.0 && value < MAX_CONTENT_INDEX as f64
    {
        Some(value as usize)
    } else {
        None
    }
}

/// 规范数组索引字符串（`"0"`、`"12"`；`"01"`、`"-1"`、`"1.5"` 都不是数组下标）。
///
/// JS 里 `arr["0"]` 与 `arr[0]` 是同一个属性；线上 JSON 的 `contentIndex` 若是字符串，
/// TS 仍按数组下标处理（只要字符串是规范的 uint32 索引）。
///
/// 10 字符上限保留了 JS 数组下标（uint32）的写法；在 `Vec` 里真正生效的是
/// [`MAX_CONTENT_INDEX`] 的内存上限。前导零、负号、小数点与非 ASCII 数字都会落选。
fn canonical_array_index(text: &str) -> Option<usize> {
    if text.is_empty() || text.len() > 10 {
        return None;
    }
    if text != "0" && text.starts_with('0') {
        return None;
    }
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value: u64 = text.parse().ok()?;
    if value >= MAX_CONTENT_INDEX as u64 {
        return None;
    }
    Some(value as usize)
}

/// `createEventConverter(model)` 的可调用状态。
///
/// 生命周期与一条响应流相同：[`stream`] 同步创建它，之后由 `run_stream` 在同一个后台任务
/// 里顺序调用 [`EventConverter::convert`]（没有并发访问，因此不需要锁）。每个出站事件都
/// 携带 `partial` 的**克隆快照**；消费者能看到进度，但不能影响转换器内部状态。
struct EventConverter {
    /// 正在累积的 assistant 消息；初始 `stop_reason = pending`、`content` 为空。
    partial: AssistantMessage,
    /// 每个 contentIndex 对应的流式参数解析器；`toolcall_end` 时移除。没有对应
    /// `toolcall_start` 的 delta 会被忽略（与 TS 的 `Map.get` 未命中一致）。
    tool_arguments: std::collections::HashMap<usize, ToolCallArgumentsStream>,
    /// `partial.content` 里仍是 JS 稀疏数组空洞的下标（值为 `undefined`）。
    holes: std::collections::HashSet<usize>,
}

impl EventConverter {
    /// `createEventConverter(model)`：记录 model 的 api/provider/id 与**调用时刻**的时间戳。
    /// 时间戳在 `stream` 的同步阶段取得，保证与 TS 一样以「开始请求」为 `timestamp`。
    fn new(model: &Model) -> Self {
        EventConverter {
            partial: AssistantMessage {
                role: AssistantRole::Assistant,
                content: Vec::new(),
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                usage: Usage::default(),
                stop_reason: StopReason::Pending,
                timestamp: now_unix_ms(),
                ..AssistantMessage::default()
            },
            tool_arguments: std::collections::HashMap::new(),
            holes: std::collections::HashSet::new(),
        }
    }

    /// `partial.content[index] = block`（稀疏数组语义：中间下标保持空洞）。
    ///
    /// JS 数组可以直接在任意下标赋值，中间位置自动成为空洞；Rust 的 `Vec` 没有空洞，这里
    /// 用默认 `Text` 块占位并把下标记入 `holes`。后续 `text_delta` / `thinking_delta` 命中
    /// 空洞时会复刻 V8 的 TypeError（见 [`EventConverter::convert`]），避免把从未 start 的
    /// 块伪装成正常文本。
    ///
    /// 注意占位块会出现在本事件的 `partial` 快照里（TS 的空洞在 JSON 序列化时是 `null`），
    /// 这是封闭的 Rust 类型无法完全复刻的部分。
    fn set_block(&mut self, index: usize, block: AssistantContentBlock) {
        while self.partial.content.len() < index {
            let hole = self.partial.content.len();
            self.partial
                .content
                .push(AssistantContentBlock::Text(TextContent::default()));
            self.holes.insert(hole);
        }
        if index < self.partial.content.len() {
            self.partial.content[index] = block;
            self.holes.remove(&index);
        } else {
            self.partial.content.push(block);
            self.holes.remove(&index);
        }
    }

    fn is_hole(&self, index: usize) -> bool {
        self.holes.contains(&index)
    }

    /// `event.contentIndex`：JS 里数组下标可以是数字或规范的索引字符串（`"2"`）。
    ///
    /// 缺失或形状不符时返回 `None`；调用方仍会发出事件（事件里的 `content_index` 回落 0），
    /// 只是不写入块。数字路径见 `as_index`，字符串路径见 `canonical_array_index`。
    fn content_index(event: &Map<String, Value>) -> Option<usize> {
        match event.get("contentIndex") {
            Some(Value::Number(number)) => as_index(number.as_f64()?),
            Some(Value::String(text)) => canonical_array_index(text),
            // `None` 与 `null` 都表示缺省。
            None | Some(Value::Null) => None,
            _ => None,
        }
    }

    /// 把一条线上事件转换成 [`AssistantMessageEvent`]。
    ///
    /// TS 的 `convertEvent` 对事件**不做任何形状校验**：按 `event.type` 分支，未知类型原样
    /// 透传 `{ ...event, partial }`，已知类型只在访问不存在的块时抛 TypeError。Rust 侧的
    /// [`AssistantMessageEvent`] 是封闭枚举（ARCHITECTURE §4.9），因此：
    /// - 未知类型无法进入事件流（`onProviderStreamEvent` 仍会先看到原始事件）；
    /// - 已知类型逐字段宽松取值（缺字段/类型不符不会丢弃事件），`done`/`error` 永远生效，
    ///   避免把成功的终态变成 "stream ended without a terminal event"。
    ///
    /// 转换顺序固定：先改 `partial`（start 类事件先建块），再把 `partial` 克隆进事件。
    /// 因此事件里的 `partial` 总是「包含本事件效果」的快照；`start` 不改内容。返回
    /// `Err(String)` 表示复刻 V8 的 TypeError（例如对稀疏数组空洞赋值），调用方会立即
    /// 终止读取并把该文本作为 error 事件的消息。
    fn convert(&mut self, raw: &Value) -> Result<Option<AssistantMessageEvent>, String> {
        let Some(event) = raw.as_object() else {
            return Ok(None);
        };
        // `contentIndex` 存在但无法解析（超大值/非整数）时忽略整条事件：
        // 块不会被创建，若仍按 `unwrap_or(0)` 发出，delta 会被错误归到第 0 块。
        let indexed_event = matches!(
            event.get("type").and_then(Value::as_str),
            Some(
                "text_start"
                    | "text_delta"
                    | "text_end"
                    | "thinking_start"
                    | "thinking_delta"
                    | "thinking_end"
                    | "toolcall_start"
                    | "toolcall_delta"
                    | "toolcall_end"
            )
        );
        if indexed_event
            // `null` 是 JS 生产者表达「没有值」的常见写法，按缺省处理（落到 0），
            // 不能当作非法索引把整条事件丢掉。
            && !matches!(event.get("contentIndex"), None | Some(Value::Null))
            && Self::content_index(event).is_none()
        {
            return Ok(None);
        }
        match event.get("type").and_then(Value::as_str) {
            Some("done") => {
                // 终态事件永远生效：缺字段/未知 reason 也产出 Done。注意两套 reason 映射：
                // stop_reason 上的未知值 → Error，而事件 reason 上的未知值 → Stop（见
                // stop_reason_from_wire / done_reason_from_wire）。
                let reason = event.get("reason").and_then(Value::as_str).unwrap_or("");
                self.partial.stop_reason = stop_reason_from_wire(reason);
                self.partial.usage = usage_from_json(event.get("usage"));
                self.partial.response_id = event
                    .get("responseId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if let Some(response_model) = event.get("responseModel").and_then(Value::as_str) {
                    self.partial.response_model = Some(response_model.to_string());
                }
                if let Some(level) = event.get("providerThinkingLevel").and_then(Value::as_str) {
                    self.partial.provider_thinking_level = Some(level.to_string());
                }
                append_rewrite_diagnostic(&mut self.partial, event.get("rewrite"));
                Ok(Some(AssistantMessageEvent::Done {
                    reason: done_reason_from_wire(reason),
                    message: Box::new(self.partial.clone()),
                }))
            }
            Some("error") => {
                // 与 done 一样始终产出 Error 事件；errorMessage 缺失时保持 None。
                let reason = event.get("reason").and_then(Value::as_str).unwrap_or("");
                self.partial.stop_reason = stop_reason_from_wire(reason);
                self.partial.usage = usage_from_json(event.get("usage"));
                self.partial.error_message = event
                    .get("errorMessage")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                self.partial.response_id = event
                    .get("responseId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if let Some(response_model) = event.get("responseModel").and_then(Value::as_str) {
                    self.partial.response_model = Some(response_model.to_string());
                }
                if let Some(level) = event.get("providerThinkingLevel").and_then(Value::as_str) {
                    self.partial.provider_thinking_level = Some(level.to_string());
                }
                append_rewrite_diagnostic(&mut self.partial, event.get("rewrite"));
                let reason = if self.partial.stop_reason == StopReason::Aborted {
                    ErrorReason::Aborted
                } else {
                    ErrorReason::Error
                };
                Ok(Some(AssistantMessageEvent::Error {
                    reason,
                    error: Box::new(self.partial.clone()),
                }))
            }
            Some("start") => Ok(Some(AssistantMessageEvent::Start {
                partial: Box::new(self.partial.clone()),
            })),
            Some("text_start") => {
                // 缺 contentIndex 时不写块（TS 会把它写成 `content[undefined]` 这个普通
                // 属性）；事件仍照常产出，content_index 回落 0。
                let index = Self::content_index(event);
                if let Some(index) = index {
                    self.set_block(
                        index,
                        AssistantContentBlock::Text(TextContent {
                            kind_text: TextKind::Text,
                            text: String::new(),
                            text_signature: None,
                        }),
                    );
                }
                Ok(Some(AssistantMessageEvent::TextStart {
                    content_index: index.unwrap_or(0),
                    partial: Box::new(self.partial.clone()),
                }))
            }
            Some("text_delta") => {
                let index = Self::content_index(event);
                let delta = crate::api::google_shared::js_string_of_value(event.get("delta"));
                if let Some(index) = index {
                    if self.is_hole(index) {
                        // V8: `(undefined).text += delta` → TypeError。
                        return Err("Cannot read properties of undefined (reading 'text')".into());
                    }
                    if let Some(AssistantContentBlock::Text(text)) =
                        self.partial.content.get_mut(index)
                    {
                        text.text.push_str(&delta);
                    }
                    // TS 对非文本块做 `obj.text += delta`（给对象加属性）；Rust 的类型化块
                    // 无法表达额外属性，这里保持块不变。
                }
                Ok(Some(AssistantMessageEvent::TextDelta {
                    content_index: index.unwrap_or(0),
                    delta,
                    partial: Box::new(self.partial.clone()),
                }))
            }
            Some("text_end") => {
                let index = Self::content_index(event);
                let content = crate::api::google_shared::js_string_of_value(event.get("content"));
                if let Some(index) = index {
                    if self.is_hole(index) {
                        // V8: `Object.assign(undefined, ...)` → TypeError。
                        return Err("Cannot convert undefined or null to object".into());
                    }
                    if let Some(AssistantContentBlock::Text(text)) =
                        self.partial.content.get_mut(index)
                    {
                        text.text = content.clone();
                        text.text_signature = event
                            .get("contentSignature")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                    }
                }
                Ok(Some(AssistantMessageEvent::TextEnd {
                    content_index: index.unwrap_or(0),
                    content,
                    partial: Box::new(self.partial.clone()),
                }))
            }
            Some("thinking_start") => {
                let index = Self::content_index(event);
                if let Some(index) = index {
                    self.set_block(
                        index,
                        AssistantContentBlock::Thinking(ThinkingContent {
                            kind_thinking: ThinkingKind::Thinking,
                            thinking: String::new(),
                            thinking_signature: None,
                            redacted: None,
                        }),
                    );
                }
                Ok(Some(AssistantMessageEvent::ThinkingStart {
                    content_index: index.unwrap_or(0),
                    partial: Box::new(self.partial.clone()),
                }))
            }
            Some("thinking_delta") => {
                let index = Self::content_index(event);
                let delta = crate::api::google_shared::js_string_of_value(event.get("delta"));
                if let Some(index) = index {
                    if self.is_hole(index) {
                        return Err(
                            "Cannot read properties of undefined (reading 'thinking')".into()
                        );
                    }
                    if let Some(AssistantContentBlock::Thinking(thinking)) =
                        self.partial.content.get_mut(index)
                    {
                        thinking.thinking.push_str(&delta);
                    }
                }
                Ok(Some(AssistantMessageEvent::ThinkingDelta {
                    content_index: index.unwrap_or(0),
                    delta,
                    partial: Box::new(self.partial.clone()),
                }))
            }
            Some("thinking_end") => {
                let index = Self::content_index(event);
                let content = crate::api::google_shared::js_string_of_value(event.get("content"));
                if let Some(index) = index {
                    if self.is_hole(index) {
                        return Err("Cannot convert undefined or null to object".into());
                    }
                    if let Some(AssistantContentBlock::Thinking(thinking)) =
                        self.partial.content.get_mut(index)
                    {
                        thinking.thinking = content.clone();
                        thinking.thinking_signature = event
                            .get("contentSignature")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        thinking.redacted = event.get("redacted").and_then(Value::as_bool);
                    }
                }
                Ok(Some(AssistantMessageEvent::ThinkingEnd {
                    content_index: index.unwrap_or(0),
                    content,
                    partial: Box::new(self.partial.clone()),
                }))
            }
            Some("toolcall_start") => {
                let index = Self::content_index(event);
                let id = crate::api::google_shared::js_string_of_value(event.get("id"));
                let name = crate::api::google_shared::js_string_of_value(event.get("toolName"));
                if let Some(index) = index {
                    self.set_block(
                        index,
                        AssistantContentBlock::ToolCall(ToolCall {
                            kind_tool_call: ToolCallKind::ToolCall,
                            id,
                            name,
                            arguments: JsonObject::new(),
                            ..ToolCall::default()
                        }),
                    );
                    self.tool_arguments
                        .insert(index, ToolCallArgumentsStream::new());
                }
                Ok(Some(AssistantMessageEvent::ToolCallStart {
                    content_index: index.unwrap_or(0),
                    partial: Box::new(self.partial.clone()),
                }))
            }
            Some("toolcall_delta") => {
                let index = Self::content_index(event);
                let delta = crate::api::google_shared::js_string_of_value(event.get("delta"));
                if let Some(index) = index
                    && let Some(args) = self.tool_arguments.get_mut(&index)
                {
                    let arguments = args.append(&delta);
                    if let Some(AssistantContentBlock::ToolCall(call)) =
                        self.partial.content.get_mut(index)
                    {
                        call.arguments = arguments;
                    }
                }
                Ok(Some(AssistantMessageEvent::ToolCallDelta {
                    content_index: index.unwrap_or(0),
                    delta,
                    partial: Box::new(self.partial.clone()),
                }))
            }
            Some("toolcall_end") => {
                let index = Self::content_index(event);
                if let Some(index) = index
                    && self.is_hole(index)
                {
                    // V8: `Object.assign(undefined, event.toolCall)` → TypeError。
                    return Err("Cannot convert undefined or null to object".into());
                }
                // 块存在则 `Object.assign` 式合并；块不存在（index 缺失/越界）时只把
                // event.toolCall 解析成事件载荷，不写入 content。块存在但不是 ToolCall 时
                // 同样只解析、不修改该块——TS 的 Object.assign 会把字段并进任意块，Rust 的
                // 类型化块无法表达这种“给对象加属性”的行为。
                let call = match index.and_then(|index| self.partial.content.get_mut(index)) {
                    Some(AssistantContentBlock::ToolCall(existing)) => {
                        if let Some(tool_call) = event.get("toolCall") {
                            merge_tool_call(existing, tool_call);
                        }
                        existing.clone()
                    }
                    _ => match event.get("toolCall") {
                        Some(tool_call) => tool_call_from_json(tool_call),
                        None => ToolCall::default(),
                    },
                };
                if let Some(index) = index {
                    self.tool_arguments.remove(&index);
                }
                Ok(Some(AssistantMessageEvent::ToolCallEnd {
                    content_index: index.unwrap_or(0),
                    tool_call: call,
                    partial: Box::new(self.partial.clone()),
                }))
            }
            // 未知类型：TS 会原样透传 `{ ...event, partial }`。封闭的事件枚举无法表达，
            // 因此不进入事件流（`onProviderStreamEvent` 已经拿到原始事件）。
            _ => Ok(None),
        }
    }
}

/// `mapStopReason`：kiana-messages 的 `done.reason` / `error.reason`。
///
/// 协议只定义 `stop`/`length`/`toolUse`/`aborted`/`error`；缺失或未知值一律映射成
/// `Error`（防御性处理，避免把坏数据当成正常结束）。
fn stop_reason_from_wire(reason: &str) -> StopReason {
    match reason {
        "stop" => StopReason::Stop,
        "length" => StopReason::Length,
        "toolUse" => StopReason::ToolUse,
        "aborted" => StopReason::Aborted,
        "error" => StopReason::Error,
        _ => StopReason::Error,
    }
}

/// `done` 事件向外暴露的 reason：只区分 `length`/`toolUse`/`deferred`，其余（含未知/缺失）
/// 回落到 `Stop`。它与 `StopReason` 的映射是两套独立规则：未知 reason 在消息上是 `error`，
/// 在事件上却是 `stop`。这是对缺失字段的防御性处理，正常线上事件不会触发。
fn done_reason_from_wire(reason: &str) -> DoneReason {
    match reason {
        "length" => DoneReason::Length,
        "toolUse" => DoneReason::ToolUse,
        "deferred" => DoneReason::Deferred,
        _ => DoneReason::Stop,
    }
}

/// `toolCall` JSON → [`ToolCall`]（宽松：缺 `arguments` 取 `{}`）。
///
/// 缺 `id`/`name` 时用空串（TS 里是 `undefined`，序列化时键会消失；Rust 的字段是
/// `String`，只能选一个可表示的值）。`arguments` 只接受对象，其它形状按空对象处理；
/// 三个可选字段（`thoughtSignature`/`namespace`/`argumentsError`）缺失或不是字符串时
/// 均为 `None`。
fn tool_call_from_json(value: &Value) -> ToolCall {
    let object = value.as_object();
    let get = |key: &str| object.and_then(|object| object.get(key));
    ToolCall {
        kind_tool_call: ToolCallKind::ToolCall,
        id: get("id").and_then(Value::as_str).unwrap_or("").to_string(),
        name: get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        arguments: match get("arguments") {
            Some(Value::Object(map)) => map
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            _ => JsonObject::new(),
        },
        thought_signature: get("thoughtSignature")
            .and_then(Value::as_str)
            .map(str::to_string),
        namespace: get("namespace").and_then(Value::as_str).map(str::to_string),
        arguments_error: get("argumentsError")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// `Object.assign(existing, event.toolCall)`：只覆盖事件里出现的字段。
///
/// 出现但类型不符的可选字段（例如 `thoughtSignature: null`）会显式清成 `None`，序列化时
/// 该键被省略；TS 的 `Object.assign` 则会把 `null` 原样写入（`JSON.stringify` 输出
/// `"thoughtSignature":null`）。这是 `Option<String>` 的类型化差异，不是对 `Object.assign`
/// 语义的复刻。`arguments` 不是对象时保持旧值（TS 会直接赋原始值，类型系统外无法表达）。
/// `value` 整体不是对象时退化为 `tool_call_from_json` 的全量替换。
fn merge_tool_call(existing: &mut ToolCall, value: &Value) {
    let Some(object) = value.as_object() else {
        *existing = tool_call_from_json(value);
        return;
    };
    if let Some(id) = object.get("id").and_then(Value::as_str) {
        existing.id = id.to_string();
    }
    if let Some(name) = object.get("name").and_then(Value::as_str) {
        existing.name = name.to_string();
    }
    if let Some(Value::Object(arguments)) = object.get("arguments") {
        existing.arguments = arguments
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
    }
    if let Some(signature) = object.get("thoughtSignature") {
        existing.thought_signature = signature.as_str().map(str::to_string);
    }
    if let Some(namespace) = object.get("namespace") {
        existing.namespace = namespace.as_str().map(str::to_string);
    }
    if let Some(arguments_error) = object.get("argumentsError") {
        existing.arguments_error = arguments_error.as_str().map(str::to_string);
    }
}

/// `usage` JSON → [`Usage`]（缺字段按 0）。
///
/// 计数类字段只接受有限非负数：缺失、负数、非数字都按 0（TS 类型上这些字段是必填
/// number，这里是防御性收敛）。`cacheWrite1h`/`reasoning` 缺省为 `None`；`cost` 里的数字
/// 不做过滤（与 TS 一样原样取 number）。协议允许可选的 `cost.totalFromProvider`
/// （`spec/ai-core.md` 的 `Usage` 定义），但本函数不读取它，恒为 `None`；TS 的 `done`
/// 处理器把 `event.usage` 整体赋给 partial，会原样保留该字段——这是与 TS 的差异。
fn usage_from_json(value: Option<&Value>) -> Usage {
    let object = value.and_then(Value::as_object);
    let number = |key: &str| {
        object
            .and_then(|object| object.get(key))
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map(|value| value as u64)
            .unwrap_or(0)
    };
    let optional_number = |key: &str| {
        object
            .and_then(|object| object.get(key))
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map(|value| value as u64)
    };
    let cost_object = object
        .and_then(|object| object.get("cost"))
        .and_then(Value::as_object);
    let cost_number = |key: &str| {
        cost_object
            .and_then(|cost| cost.get(key))
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    };
    Usage {
        input: number("input"),
        output: number("output"),
        cache_read: number("cacheRead"),
        cache_write: number("cacheWrite"),
        cache_write1h: optional_number("cacheWrite1h"),
        reasoning: optional_number("reasoning"),
        total_tokens: number("totalTokens"),
        cost: UsageCost {
            input: cost_number("input"),
            output: cost_number("output"),
            cache_read: cost_number("cacheRead"),
            cache_write: cost_number("cacheWrite"),
            total: cost_number("total"),
            total_from_provider: None,
        },
    }
}

/// 流式 UTF-8 解码：等价于 `new TextDecoder()` 的 `decode(value, { stream: true })`。
///
/// `pending` 保存上一个块末尾不完整的多字节序列（最多 3 字节），下一次 `decode` 时与新
/// 字节拼接后再解码。`stream = false` 表示流结束：剩余字节一次性解码，非法序列由
/// `String::from_utf8_lossy` 替换成 U+FFFD。解码按字节块进行，UTF-8 字符被切开也不会产生
/// 乱码。
///
/// 与 `TextDecoder` 的差异：本实现不剥离开头的 UTF-8 BOM（`TextDecoder` 默认会剥离），
/// 也不做编码嗅探。线上 SSE 若带 BOM，首帧第一行会因为不是以 `"data:"` 开头而被忽略。
pub(crate) struct Utf8StreamDecoder {
    /// 尚未凑成完整 UTF-8 序列的结尾字节（0..=3 个）。
    pending: Vec<u8>,
}

impl Utf8StreamDecoder {
    /// `new TextDecoder()`：无状态构造。
    pub(crate) fn new() -> Self {
        Utf8StreamDecoder {
            pending: Vec::new(),
        }
    }

    /// `stream = true` 表示还有后续字节（保留不完整的结尾）。
    ///
    /// - `stream = true`：拼上 `pending`，只解码到最后一个完整字符，剩余不完整后缀留在
    ///   `pending`；
    /// - `stream = false`：连 `pending` 一起全部解码并清空，用于流结束或显式 flush；
    /// - 空输入且 `stream = false`：等价于 flush，只吐出 `pending` 中残留的字节。
    ///
    /// 已解码的部分向前推进后立即从 `pending` 移除，内存占用与未完成后缀长度成正比。
    pub(crate) fn decode(&mut self, bytes: &[u8], stream: bool) -> String {
        if bytes.is_empty() && !stream {
            let leftovers = std::mem::take(&mut self.pending);
            return String::from_utf8_lossy(&leftovers).into_owned();
        }
        self.pending.extend_from_slice(bytes);
        if stream {
            let keep = incomplete_suffix_len(&self.pending);
            let split = self.pending.len() - keep;
            let head = String::from_utf8_lossy(&self.pending[..split]).into_owned();
            self.pending.drain(..split);
            head
        } else {
            let all = std::mem::take(&mut self.pending);
            String::from_utf8_lossy(&all).into_owned()
        }
    }
}

/// 结尾可能是一个不完整 UTF-8 序列的字节数（0..=3）。
///
/// 只把「合法起始字节 + 若干合法后续字节」的短尾判为不完整；非法起始字节（0xC0/0xC1、
/// 0xF5..0xFF）和后续字节不在 0x80..=0xBF 的情况返回 0，让 `from_utf8_lossy` 立即把它们
/// 替换成 U+FFFD。返回 1..=3 时调用方保留这段尾字节，等下一块再解码。
///
/// 从最短后缀（1 字节）开始向上试：完整字符的结尾不会命中，因此正常数据总是返回 0。
fn incomplete_suffix_len(bytes: &[u8]) -> usize {
    let len = bytes.len();
    for keep in 1..=3.min(len) {
        let start = len - keep;
        let first = bytes[start];
        let expected = match first {
            0x00..=0x7f => 1,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => continue,
        };
        if expected <= keep {
            continue;
        }
        let mut valid = true;
        for &byte in &bytes[start + 1..] {
            if byte & 0xc0 != 0x80 {
                valid = false;
                break;
            }
        }
        if valid {
            return keep;
        }
    }
    0
}

/// `parseKianaMessagesEvent(raw)`：取第一行以 `"data:"` 开头的行，去掉前 5 字符后 `trim()`。
///
/// 只取**第一行**（TS 的 `.find`）：多行 `data:` 不拼接，`event:`/`id:`/`retry:` 等 SSE
/// 字段被忽略。`trim()` 用 JS 语义（含 U+FEFF 等 Unicode 空白）。空 data 与 `[DONE]`
/// 返回 `Ok(None)`；整帧没有 data 行（注释/心跳）同样返回 `Ok(None)`。
///
/// 解析失败返回 `AiErrorKind::Parse`，message 用 `kiana_json::js_parse` 的 V8 原文；
/// 调用方会把整条流转成 error 事件（与 TS 里 `JSON.parse` 抛出被外层 catch 一致）。
fn parse_kiana_messages_event(raw: &str) -> Result<Option<Value>, AiError> {
    let data = raw
        .split('\n')
        .find(|line| line.starts_with("data:"))
        .map(|line| kiana_json::js_string::js_trim(&line[5..]).to_string());
    let Some(data) = data else {
        return Ok(None);
    };
    if data.is_empty() || data == "[DONE]" {
        return Ok(None);
    }
    // 解析失败时把 `js_parse` 的错误消息直接作为 errorMessage：该文案与 V8 一致，
    // 且接受 `JSON.parse` 接受的一切输入（孤立代理项、溢出数字）。
    kiana_json::js_parse::parse(&data)
        .map(Some)
        .map_err(|error| AiError::new(AiErrorKind::Parse, error.message))
}

/// `readKianaMessagesEvents(stream)` 的 Rust 形态：从字节流里逐条产出原始事件 JSON。
///
/// `on_event` 返回 `true` 表示已收到终态事件，读取立即停止（TS 在 `for await` 里 `return`，
/// 生成器的 `finally` 释放 reader）。语义与 TS 的异步生成器一致：每轮把新字节追加进 buffer
/// （流式 UTF-8 解码）、`\r\n` → `\n`、按 `"\n\n"` 切分；结束时残余 buffer 若 `trim()`
/// 非空也解析一次。
///
/// # 取消与终止
///
/// 有取消令牌时，每次等待下一块都由 `next_chunk` 用 `select!` 与令牌竞争，取消时返回
/// `AiErrorKind::Aborted`；没有令牌时 `next_chunk` 直接 `await`，不会产生 `Aborted`。
/// 未消费的 body 直接丢弃。同步处理（切帧/解析/回调）期间不检查取消，因此单个超大帧的
/// 处理不会被中途打断。
///
/// `on_event` 返回 `true` 时立即 `return Ok(())`，不再读取剩余 body（提前返回即丢弃
/// reader、释放连接）。解析错误或 `on_event` 返回 `Err` 也会立即结束，不会
/// 继续消费流。
///
/// # 末尾处理
///
/// 读到 EOF 后先 flush 解码器，再把 `trim()` 非空的残余 buffer 当作最后一帧解析一次
/// （服务端最后一帧可能没有 `"\n\n"` 结尾）。
async fn read_kiana_messages_events(
    mut body: ByteStream,
    signal: Option<tokio_util::sync::CancellationToken>,
    mut on_event: impl FnMut(&Value) -> Result<bool, AiError>,
) -> Result<(), AiError> {
    let mut decoder = Utf8StreamDecoder::new();
    let mut buffer = String::new();

    loop {
        let (done, bytes) = match next_chunk(&mut body, signal.as_ref()).await {
            Ok(Some(bytes)) => (false, bytes),
            Ok(None) => (true, Vec::new()),
            Err(error) => return Err(http_error_to_ai(error, signal.as_ref())),
        };
        buffer.push_str(&decoder.decode(&bytes, !done));
        buffer = buffer.replace("\r\n", "\n");

        while let Some(split) = buffer.find("\n\n") {
            let raw = buffer[..split].to_string();
            buffer.drain(..split + 2);
            if let Some(event) = parse_kiana_messages_event(&raw)?
                && on_event(&event)?
            {
                return Ok(());
            }
        }

        if done {
            break;
        }
    }
    buffer.push_str(&decoder.decode(&[], false));

    if !kiana_json::js_string::js_trim(&buffer).is_empty()
        && let Some(event) = parse_kiana_messages_event(&buffer)?
        && on_event(&event)?
    {
        return Ok(());
    }
    Ok(())
}

/// `createErrorEvent(model, error, aborted)`。
///
/// 构造终态 `error` 事件：消息内容为空，`stop_reason` 为 aborted/error，`error_message`
/// 取 `StreamFailure::message()`。只有当错误是 HTTP 响应错误且未被取消时，才追加
/// `kiana_messages_response_failure` 诊断（携带 `KianaMessagesResponseError` 的
/// name/message/code 与 diagnosticDetails）；取消与传输错误不带该诊断，避免把用户主动
/// 取消误报成服务端失败。
fn create_error_event(
    model: &Model,
    error: &StreamFailure,
    aborted: bool,
) -> AssistantMessageEvent {
    let reason = if aborted {
        StopReason::Aborted
    } else {
        StopReason::Error
    };
    let mut message = AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage: Usage::default(),
        stop_reason: reason,
        error_message: Some(error.message().to_string()),
        timestamp: now_unix_ms(),
        ..AssistantMessage::default()
    };
    if !aborted && let StreamFailure::Response(response) = error {
        let thrown = ThrownValue::Error(DiagnosticErrorInfo {
            name: Some("KianaMessagesResponseError".into()),
            message: response.message.clone(),
            stack: None,
            code: response.code.clone().map(Value::String),
        });
        append_assistant_message_diagnostic(
            &mut message,
            create_assistant_message_diagnostic(
                "kiana_messages_response_failure",
                &thrown,
                Some(response.diagnostic_details.clone()),
            ),
        );
    }
    let reason = if aborted {
        ErrorReason::Aborted
    } else {
        ErrorReason::Error
    };
    AssistantMessageEvent::Error {
        reason,
        error: Box::new(message),
    }
}

/// `resolveCacheRetention(cacheRetention?, env?)`：显式值优先，否则 `KIANA_CACHE_RETENTION === "long"`。
///
/// 缺省返回 `None`，即请求体里不出现 `cacheRetention`（由后端决定），与 Bedrock 的
/// `"short"` 默认不同。环境变量查询走 `get_provider_env_value`：`base.env` 里的同名值优先
/// 于进程环境，且空串视为缺失。
fn resolve_cache_retention(
    cache_retention: Option<CacheRetention>,
    env: Option<&crate::types::ProviderEnv>,
) -> Option<CacheRetention> {
    if let Some(retention) = cache_retention {
        return Some(retention);
    }
    if get_provider_env_value("KIANA_CACHE_RETENTION", env).as_deref() == Some("long") {
        Some(CacheRetention::Long)
    } else {
        None
    }
}

/// `stream(model, context, options)`。
///
/// 同步返回事件流；真正的网络请求在后台任务里执行。`model`/`context` 被克隆、`options`
/// 被移动进任务，调用方在返回后可以立即释放它们。转换器在调用线程同步创建，因此
/// `partial.timestamp` 记录的是「开始请求」的时间（与 TS 的 `createEventConverter` 同步
/// 执行一致）。
///
/// 错误处理：`run_stream` 的任何错误都不会以 panic 或 `Err` 冒泡，而是被转成一个终态
/// error 事件（`create_error_event`）。`aborted` 同时看错误类型与取消令牌当前状态：取消
/// 发生在网络错误之后时，仍按调用方取消呈现。
///
/// 并发：事件流的克隆被移交给后台任务，调用方持有原句柄；`push` 保证终态事件与
/// `result()` 提交在同一临界区完成（见 `AssistantMessageEventStream::push`）。没有 tokio
/// runtime 时 `spawn_forward` 会新建线程 + current-thread runtime，因此同步调用方不需要
/// 处于 async 上下文。
pub fn stream(
    model: &Model,
    context: &TranscriptContext,
    options: KianaMessagesOptions,
) -> AssistantMessageEventStream {
    let event_stream = AssistantMessageEventStream::new();
    // 转换器必须在调用线程同步创建：它是 timestamp（“开始请求”时刻）的来源。
    let mut converter = EventConverter::new(model);
    let model = model.clone();
    let context = context.clone();
    let stream_for_task = event_stream.clone();
    // 后台任务独占 converter 与事件流克隆；run_stream 返回的 Err 在这里统一转成终态事件。
    spawn_forward(async move {
        let aborted_flag = options.base.signal.clone();
        match run_stream(&model, &context, options, &stream_for_task, &mut converter).await {
            Ok(()) => {}
            Err(error) => {
                let aborted = error.is_aborted()
                    || aborted_flag
                        .as_ref()
                        .is_some_and(tokio_util::sync::CancellationToken::is_cancelled);
                let event = create_error_event(&model, &error, aborted);
                let _ = stream_for_task.push(event);
            }
        }
    });
    event_stream
}

/// 流内错误：`Response` 保留 `KianaMessagesResponseError` 的字段供诊断。
///
/// - `Response`：服务端返回非 2xx，携带完整诊断 details；
/// - `Aborted`：取消令牌触发（或竞态下被判定为取消）；
/// - `Other`：缺 API key、URL 非法、传输错误、解析错误、没有终态事件等。
///
/// `Aborted` 总是渲染为 `Aborted`；此外只要取消令牌已经触发，任何错误都会按取消呈现
/// （见 [`stream`] 里的 `aborted` 计算）。`create_error_event` 只为未被取消的 `Response`
/// 追加响应失败诊断。
enum StreamFailure {
    /// HTTP 响应错误（非 2xx）。
    Response(KianaMessagesResponseError),
    /// 调用方取消。
    Aborted(String),
    /// 其它错误（鉴权、URL、传输、解析、协议终止缺失）。
    Other(String),
}

impl StreamFailure {
    fn message(&self) -> &str {
        match self {
            StreamFailure::Response(error) => &error.message,
            StreamFailure::Aborted(message) | StreamFailure::Other(message) => message,
        }
    }

    fn is_aborted(&self) -> bool {
        matches!(self, StreamFailure::Aborted(_))
    }
}

// 让 `error.message` 的读取在两种形态下都可用。
impl std::ops::Deref for StreamFailure {
    type Target = str;
    fn deref(&self) -> &str {
        self.message()
    }
}

/// 执行一次请求并驱动事件流（[`stream`] 后台任务的主体）。
///
/// `event_stream` 是调用方事件流的克隆。本函数只在成功/终态事件上 `push`，**不**调用
/// `end()`（终态事件自己提交 `result()`）。任何错误都通过 `StreamFailure` 返回，由
/// [`stream`] 统一转成 error 事件；已经 push 的事件保留，不会被撤回。
///
/// 阶段与 TS 的 async IIFE 一一对应：
/// 1. 校验 API key（缺失文案是 `No API key provided for provider "<provider>"`，与其它
///    适配器的 `No API key for provider: X` 不同，见 spec/ai-api-other.md §2.10）；
/// 2. 拼 URL 与 payload，允许 `onPayload` 整体替换；
/// 3. 合并请求头并 `execute`（单次请求，不做重试；`max_retries` 等字段被忽略）；
/// 4. 非 2xx → 读完整 body → `StreamFailure::Response`；
/// 5. 2xx → 逐事件转换并 push，直到 `done`/`error` 或流结束。
async fn run_stream(
    model: &Model,
    context: &TranscriptContext,
    options: KianaMessagesOptions,
    event_stream: &AssistantMessageEventStream,
    converter: &mut EventConverter,
) -> Result<(), StreamFailure> {
    let KianaMessagesOptions {
        base,
        reasoning,
        tool_choice,
        debug,
    } = options;
    // 阶段 1：鉴权。空字符串视同缺失（TS 的 `if (!apiKey)`）；错误文案与其它适配器不同。
    let api_key = base
        .api_key
        .clone()
        .filter(|key| !key.is_empty())
        .ok_or_else(|| {
            StreamFailure::Other(format!(
                "No API key provided for provider \"{}\"",
                model.provider
            ))
        })?;

    // 阶段 2a：URL。TS 用 `new URL(...)` 抛 TypeError；这里返回带固定文案的 StreamFailure。
    // `trim_end_matches('/')` 对应 TS 的正则 `/\/+$/u`（去掉所有结尾斜杠）。
    let base_url = model.base_url.trim_end_matches('/').to_string();
    let mut url = url::Url::parse(&format!("{base_url}/messages"))
        .map_err(|_| StreamFailure::Other(format!("Invalid URL {base_url}/messages")))?;
    if debug {
        // `URLSearchParams.set('debug', '1')`：已有 debug 参数时替换，不能追加成
        // `?debug=0&debug=1`（服务端可能只读第一个值）。
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| key != "debug")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        url.set_query(None);
        {
            let mut query = url.query_pairs_mut();
            for (key, value) in &pairs {
                query.append_pair(key, value);
            }
            query.append_pair("debug", "1");
        }
    }

    // 阶段 2b：`options` 只包含有值的键（TS 里 JSON.stringify 会丢弃 undefined）。
    // `reasoning`/`cacheRetention` 用 serde 转成线格式；`toolChoice` 原样透传。
    let options_value = {
        let mut object = Map::new();
        if let Some(temperature) = base.temperature {
            object.insert("temperature".into(), number_value(temperature));
        }
        if let Some(max_tokens) = base.max_tokens {
            object.insert("maxTokens".into(), Value::from(max_tokens));
        }
        if let Some(reasoning) = reasoning {
            object.insert(
                "reasoning".into(),
                serde_json::to_value(reasoning).unwrap_or(Value::Null),
            );
        }
        if let Some(retention) = resolve_cache_retention(base.cache_retention, base.env.as_ref()) {
            object.insert(
                "cacheRetention".into(),
                serde_json::to_value(retention).unwrap_or(Value::Null),
            );
        }
        if let Some(session_id) = &base.session_id {
            object.insert("sessionId".into(), Value::String(session_id.clone()));
        }
        if let Some(tool_choice) = &tool_choice {
            object.insert("toolChoice".into(), tool_choice.clone());
        }
        Value::Object(object)
    };
    // 阶段 2c：payload 固定为 `{ model, context, options }`；`context` 是调用方传入的完整
    // `TranscriptContext`，本层不做裁剪或转换，与 TS 直接发 `context` 一致。
    let mut payload = {
        let mut root = Map::new();
        root.insert("model".into(), Value::String(model.id.clone()));
        root.insert(
            "context".into(),
            serde_json::to_value(context).unwrap_or(Value::Null),
        );
        root.insert("options".into(), options_value);
        Value::Object(root)
    };
    // `onPayload` 返回 `Some` 时整体替换 payload（`None` 表示不修改）。
    if let Some(next) = base.apply_on_payload(payload.clone(), model).await {
        payload = next;
    }

    // 阶段 3：请求头按 identity → 默认 → 用户 的顺序合并；同名键后者覆盖前者，值为 `None`
    // 会删除已有的同名头。注意**不读取 `model.headers`**（该字段只用于 Mistral 适配器）。
    let identity_headers = get_identity_headers(
        &ClientIdentityTarget::new(&model.provider, &model.base_url),
        base.identity.as_ref(),
    );
    let mut defaults = crate::types::ProviderHeaders::new();
    defaults.insert("authorization".into(), Some(format!("Bearer {api_key}")));
    defaults.insert("accept".into(), Some("text/event-stream".into()));
    defaults.insert("content-type".into(), Some("application/json".into()));
    let headers = provider_headers_to_record(&[
        Some(&identity_headers),
        Some(&defaults),
        base.headers.as_ref(),
    ])
    .unwrap_or_default();

    // body 用 JS `JSON.stringify` 语义（无多余空格、丢弃 undefined、非有限数字输出 null）。
    let request = HttpRequest {
        method: "POST".into(),
        url: url.to_string(),
        headers: headers.into_iter().collect(),
        body: Some(kiana_json::js_object::stringify(&payload).into_bytes()),
        timeout_ms: None,
    };
    // 单次请求；取消令牌直接交给 HttpClient，由底层负责中断（本层不设整体超时）。
    let client = resolve_client(&base).map_err(|error| StreamFailure::Other(error.message))?;
    let response = client
        .execute(request, base.signal.as_ref())
        .await
        .map_err(|error| http_error_to_ai_failure(error, base.signal.as_ref()))?;

    // onResponse 在判断状态码之前调用，因此 4xx/5xx 的响应头也可见。

    if let Some(on_response) = &base.on_response {
        on_response(response.response.clone(), model);
    }

    // 阶段 4：非 2xx。读完整 body（非法 UTF-8 替换为 U+FFFD）再构造诊断；statusText 按
    // 状态码查表重建（响应视图不带真实 reason phrase，见 `reason_phrase`）。
    if !(200..300).contains(&response.response.status) {
        let status = response.response.status;
        let body = read_body_text(response.body, base.signal.as_ref())
            .await
            .map_err(|error| StreamFailure::Other(error.message))?;
        let status_text = reason_phrase(status);
        let error_body = parse_kiana_messages_error_body(&body);
        let error = create_kiana_messages_response_error(
            model,
            url.as_str(),
            status,
            &status_text,
            &body,
            error_body.as_ref(),
        );
        return Err(StreamFailure::Response(error));
    }

    // 阶段 5：2xx。先给 onProviderStreamEvent 原始 JSON（TS 在转换之前回调），再转换；
    // 转换失败（复刻的 V8 TypeError）或解析失败会由 read_kiana_messages_events 原样冒泡，
    // 最终变成 error 事件。`push` 的返回值当前实现不会失败，忽略即可。
    let signal = base.signal.clone();
    let on_provider_event = base.on_provider_stream_event.clone();
    let model_for_events = model.clone();
    let mut terminal_seen = false;
    read_kiana_messages_events(response.body, signal.clone(), |raw| {
        if let Some(callback) = &on_provider_event {
            callback(raw.clone(), &model_for_events);
        }
        let converted = converter
            .convert(raw)
            .map_err(|message| AiError::new(AiErrorKind::Other, message))?;
        let Some(converted) = converted else {
            return Ok(false);
        };
        terminal_seen = matches!(
            converted,
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
        );
        let _ = event_stream.push(converted);
        Ok(terminal_seen)
    })
    .await
    .map_err(|error| {
        if error.kind == AiErrorKind::Aborted {
            StreamFailure::Aborted(error.message)
        } else {
            StreamFailure::Other(error.message)
        }
    })?;
    // 没有终态事件：取消优先报 aborted（读取可能因取消而提前结束），否则按协议错误处理。
    if terminal_seen {
        return Ok(());
    }
    if base.signal.as_ref().is_some_and(|s| s.is_cancelled()) {
        return Err(StreamFailure::Aborted(
            crate::http::reqwest_client::ABORTED_MESSAGE.into(),
        ));
    }
    Err(StreamFailure::Other(format!(
        "{} stream ended without a terminal event",
        model.provider
    )))
}

/// `streamSimple(model, context, options)`：透传（`SimpleStreamOptions` 已包含 reasoning/toolChoice）。
///
/// 与 TS 相同，本适配器把选项原样交给 [`stream`]，`debug` 固定为 false；
/// `tool_choice` 用 serde 序列化成 JSON（`ToolChoice` 的线格式是 `"auto"`/`"none"`，
/// 序列化失败的分支实际不可达，保留 `"auto"` 作为兜底）。
///
/// 返回 `Result` 是 crate 统一的同步入口契约：本实现永远返回 `Ok`——缺 API key 等错误在
/// [`stream`] 的后台任务里变成 error 事件，而不是 `Err`。`ProviderStreams::stream_simple`
/// 里的 `Err` 分支因此当前不可达（保留给契约与其它适配器）。
pub fn stream_simple(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
) -> Result<AssistantMessageEventStream, AiError> {
    let SimpleStreamOptions {
        base,
        tool_choice,
        reasoning,
        ..
    } = options;
    let tool_choice = tool_choice.map(|choice| {
        serde_json::to_value(choice).unwrap_or_else(|_| Value::String("auto".into()))
    });
    Ok(stream(
        model,
        context,
        KianaMessagesOptions {
            base,
            reasoning,
            tool_choice,
            debug: false,
        },
    ))
}

/// `kiana-messages.lazy.ts`：`lazyApi(() => import("./kiana-messages.ts"))` 的 Rust 等价物。
///
/// Rust 没有动态 import，直接返回无状态的 `KianaMessagesProvider`；每次调用都新建一个
/// `Arc`（实现无内部状态，多个 provider 与注册表可各持一份）。注册点：`providers/radius.rs`
/// 的 `streams` 字段，以及 `compat.rs` 的内置 API 注册表。
pub fn kiana_messages_api() -> Arc<dyn ProviderStreams> {
    Arc::new(KianaMessagesProvider)
}

/// [`ProviderStreams`] 的适配器实现；没有字段，因此无状态、可安全共享。
struct KianaMessagesProvider;

impl ProviderStreams for KianaMessagesProvider {
    /// 完整选项入口：先按 TS 字段名从 `base.extra` 提取 API 专属字段。
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        stream(
            model,
            context,
            kiana_messages_options_from_stream_options(options),
        )
    }

    /// 简单选项入口：把同步 `Err` 映射成 lazy stream error（当前实现不会走到该分支）。
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

/// [`StreamOptions`] → [`KianaMessagesOptions`]：API 专属字段从 `extra` 里按 TS 字段名取。
///
/// `extra` 被整体取走（`mem::take`），其中 `reasoning` 用 serde 反序列化成
/// [`ThinkingLevel`]（形状不符当作未设置），`toolChoice` 原样保留 JSON，`debug` 只认布尔
/// 真值；其余键被丢弃。`base` 里保留 `temperature`/`maxTokens`/`sessionId`/
/// `cacheRetention` 等由 `run_stream` 序列化的字段。
pub fn kiana_messages_options_from_stream_options(
    mut options: StreamOptions,
) -> KianaMessagesOptions {
    let extra = std::mem::take(&mut options.extra);
    let reasoning = extra
        .get("reasoning")
        .and_then(|value| serde_json::from_value::<ThinkingLevel>(value.clone()).ok());
    let tool_choice = extra.get("toolChoice").cloned();
    let debug = extra.get("debug").and_then(Value::as_bool).unwrap_or(false);
    KianaMessagesOptions {
        base: options,
        reasoning,
        tool_choice,
        debug,
    }
}

/// 按给定键序构造 JSON 对象（与 `google_shared::object` 相同；两个适配器共用）。
///
/// 依赖 workspace 开启的 `serde_json/preserve_order`：`Map` 保持插入顺序，因此线上 JSON
/// 的键序与调用方给出的数组顺序一致（测试或网关可能按字节/快照比较）。重复键后者覆盖
/// 前者，位置保持在第一次插入处（`IndexMap::insert` 语义）。
pub fn json_object<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    let mut map = Map::new();
    for (key, value) in pairs {
        map.insert(key.to_string(), value);
    }
    Value::Object(map)
}

/// `options.fetch` 或默认 reqwest 实现。
///
/// `options.fetch` 是 crate 的 fetch 注入点（`HttpClient` 抽象），测试用它替换真实网络。
/// 没有注入时每次调用都新建一个 `ReqwestHttpClient`（独立连接池）；需要复用连接或统一
/// 代理/超时的调用方应通过 `fetch` 传入共享客户端。TLS 初始化失败等构造错误映射为
/// `AiErrorKind::Transport`。
pub(crate) fn resolve_client(options: &StreamOptions) -> Result<Arc<dyn HttpClient>, AiError> {
    if let Some(fetch) = &options.fetch {
        return Ok(fetch.clone());
    }
    crate::http::reqwest_client::ReqwestHttpClient::new()
        .map(|client| client.shared())
        .map_err(|error| AiError::new(AiErrorKind::Transport, error.message))
}

/// 读完整 body 的文本（`response.text()`：UTF-8，非法字节替换成 U+FFFD）。
///
/// 取消令牌触发时返回 `AiErrorKind::Aborted`（`AiError::aborted`，name = `AbortError`），
/// 其它读取错误返回 `Transport`。与 `response.text()` 一样会把整个 body 收进内存：只用于
/// 错误响应或非流式响应，不用于正常 SSE 流。
pub(crate) async fn read_body_text(
    mut body: ByteStream,
    signal: Option<&tokio_util::sync::CancellationToken>,
) -> Result<String, AiError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = next_chunk(&mut body, signal).await.map_err(|error| {
        if signal.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
            AiError::aborted(error.message)
        } else {
            AiError::new(AiErrorKind::Transport, error.message)
        }
    })? {
        bytes.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// 读取下一个字节块；取消令牌触发时返回 `Err(ABORTED_MESSAGE)`。
///
/// 有取消令牌时用 `tokio::select!` 同时等待 `StreamExt::next` 与 `token.cancelled()`；
/// `next` 可以安全地被取消（未完成的 poll 不会从流里取走字节），因此不会丢数据。没有令牌
/// 时直接 await。`Ok(None)` 表示流正常结束。
///
/// 取消检查只发生在等待期间：已经返回给调用方的字节仍会照常处理。
pub(crate) async fn next_chunk(
    body: &mut ByteStream,
    signal: Option<&tokio_util::sync::CancellationToken>,
) -> Result<Option<Vec<u8>>, HttpError> {
    let next = body.next();
    match signal {
        Some(token) => {
            tokio::select! {
                item = next => match item {
                    Some(Ok(bytes)) => Ok(Some(bytes)),
                    Some(Err(error)) => Err(error),
                    None => Ok(None),
                },
                _ = token.cancelled() => Err(HttpError::new(
                    crate::http::reqwest_client::ABORTED_MESSAGE,
                )),
            }
        }
        None => match next.await {
            Some(Ok(bytes)) => Ok(Some(bytes)),
            Some(Err(error)) => Err(error),
            None => Ok(None),
        },
    }
}

/// 把 `HttpError` 映射成 `AiError`：取消令牌已触发时归为 `Aborted`（即使底层报的是传输
/// 错误），否则归为 `Transport`。`HttpError` 本身没有错误分类，只能靠信号状态判断这层
/// 竞态：用户先取消、底层随后返回错误时，应以取消呈现。
fn http_error_to_ai(
    error: HttpError,
    signal: Option<&tokio_util::sync::CancellationToken>,
) -> AiError {
    if signal.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
        AiError::aborted(error.message)
    } else {
        AiError::new(AiErrorKind::Transport, error.message)
    }
}

/// 同 `http_error_to_ai`，但输出 `StreamFailure`（`Aborted`/`Other`）。
fn http_error_to_ai_failure(
    error: HttpError,
    signal: Option<&tokio_util::sync::CancellationToken>,
) -> StreamFailure {
    if signal.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
        StreamFailure::Aborted(error.message)
    } else {
        StreamFailure::Other(error.message)
    }
}

/// f64 → JSON 数字；非有限值（NaN/±∞）输出 `null`，与 `JSON.stringify` 一致
/// （`serde_json::Number::from_f64` 拒绝它们，因此用 `null` 兜底而不是丢键）。
fn number_value(value: f64) -> Value {
    serde_json::Number::from_f64(value)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

/// `Date.now()` 的等价物：Unix 毫秒。系统时钟早于 1970 时返回 0（`duration_since` 的 Err
/// 分支），保证不 panic。
fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Node `fetch` 的 `Response.statusText`（标准原因短语）。
///
/// 响应视图 `ProviderResponse` 只有 status/headers，没有真实的原因短语，因此这里按状态码
/// 查表重建；服务端自定义的 statusText 不会保留。非标准状态码或自定义原因短语无法还原，
/// 返回空串；Node 的 `fetch` 会保留服务端返回的 statusText，这是本适配器与 TS 的已知差异。
/// 这张表是固定协议数据，不需要按代码逐项解释。
pub(crate) fn reason_phrase(status: u16) -> String {
    let phrase = match status {
        100 => "Continue",
        101 => "Switching Protocols",
        102 => "Processing",
        103 => "Early Hints",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        203 => "Non-Authoritative Information",
        204 => "No Content",
        205 => "Reset Content",
        206 => "Partial Content",
        207 => "Multi-Status",
        208 => "Already Reported",
        226 => "IM Used",
        300 => "Multiple Choices",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        305 => "Use Proxy",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        407 => "Proxy Authentication Required",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        411 => "Length Required",
        412 => "Precondition Failed",
        413 => "Payload Too Large",
        414 => "URI Too Long",
        415 => "Unsupported Media Type",
        416 => "Range Not Satisfiable",
        417 => "Expectation Failed",
        418 => "I'm a Teapot",
        421 => "Misdirected Request",
        422 => "Unprocessable Entity",
        423 => "Locked",
        424 => "Failed Dependency",
        425 => "Too Early",
        426 => "Upgrade Required",
        428 => "Precondition Required",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        451 => "Unavailable For Legal Reasons",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        505 => "HTTP Version Not Supported",
        506 => "Variant Also Negotiates",
        507 => "Insufficient Storage",
        508 => "Loop Detected",
        510 => "Not Extended",
        511 => "Network Authentication Required",
        _ => "",
    };
    phrase.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_content_index_events_are_dropped() {
        let mut converter = EventConverter::new(&Model::default());
        let invalid = serde_json::json!({"type": "text_start", "contentIndex": 70_000});
        assert!(converter.convert(&invalid).expect("convert").is_none());
        let missing = serde_json::json!({"type": "text_start"});
        assert!(converter.convert(&missing).expect("convert").is_some());
    }

    #[test]
    fn numeric_content_index_applies_the_js_array_limit() {
        assert_eq!(as_index(0.0), Some(0));
        assert_eq!(as_index(1_023.0), Some(1_023));
        assert_eq!(as_index(1_024.0), None);
        assert_eq!(as_index(4_000_000_000.0), None);
        assert_eq!(as_index(70_000.0), None);
        assert_eq!(as_index(-1.0), None);
        assert_eq!(as_index(1.5), None);
        // 字符串路径必须给出同样的上限。
        assert_eq!(canonical_array_index("4000000000"), None);
        assert_eq!(canonical_array_index("70000"), None);
    }
}
