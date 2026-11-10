//! proxy.ts（`packages/agent/src/proxy.ts`）。
//!
//! 把 LLM 调用转给一个后端 proxy：`POST ${proxyUrl}/api/stream`，SSE 风格的逐行
//! `data: ` 帧。规范见 `spec/agent-codemode.md` §2.1.6（含 `processProxyEvent` 的 13 条
//! 事件映射与 `ToolCallArgumentsStream` 的线性时间要求）。
//!
//! ## 传输（决策：`ARCHITECTURE.md` §8 第 18 条 / GAPS G1.2 (a)）
//!
//! TS 用全局 `fetch`（**不**经过 `options.fetch` 的 provider 注入点）。Rust 侧保留同样的形状：
//! 本模块直接依赖 `reqwest`，而不是复用 `kiana_ai::http::HttpClient`。这样
//! `stream_proxy(model, context, options)` 的签名与 TS 一一对应，调用方（`kiana-core`/`kiana-session`）
//! 不需要额外装配 HTTP 客户端。
//!
//! ## 执行流程
//!
//! 1. [`stream_proxy`] 创建 [`AssistantMessageEventStream`]，把实际工作交给后台任务
//!    （`spawn_stream_task`：当前有 tokio runtime 就 `spawn` 上去，否则新开专用线程并在其中
//!    建立 current-thread runtime）。函数立即返回流；网络与协议错误都走流事件，不以 `Err`
//!    返回。
//! 2. `run_stream_proxy` 按模型信息构造初始 `AssistantMessage`（`stopReason = "pending"`、
//!    usage 全 0、`timestamp = Date.now()`），再调用 `request_and_consume`。
//! 3. `request_and_consume` 用 `serde_json` + `kiana_json` 写出与 `JSON.stringify` 一致的
//!    `{model, context, options}`，`POST` 到 `${proxy_url}/api/stream`。等待响应头与读取
//!    2xx 响应体都在 `tokio::select!` 中先检查取消令牌（`biased`）：取消时分别以
//!    `This operation was aborted` 与 `Request aborted by user` 结束。
//! 4. 非 2xx：先拼 `Proxy error: {status} {statusText}`，再尝试把错误 body 解析成
//!    `{error}`，真值则改用 `Proxy error: {error}`；读错误 body 期间取消会保留状态行文案
//!    并立即返回。204/205 走“响应体为 null”的 TypeError 文案（304 不是 success，先被
//!    非 2xx 分支拦截）。
//! 5. 2xx：按 chunk 读取响应体，经 `Utf8StreamDecoder` 增量解码后按 `\n` 切行；只处理
//!    `data: ` 开头的行，逐行 `JSON.parse` + `normalize_proxy_event_value` +
//!    [`process_proxy_event`]，把得到的事件 `push` 给消费者。EOF 时刷写解码器，并处理
//!    最后一段没有换行结尾的数据。
//! 6. 所有 `Err`（URL、连接、非 2xx、解析、类型不匹配、取消）由 `run_stream_proxy` 折叠成
//!    一条 `error` 事件，`reason` 取决于取消令牌；但仅当流尚未被更早的 `done`/`error`
//!    关闭时消费者才收得到：`AssistantMessageEventStream::push` 在 `done` 置真后直接返回
//!    `Ok(())` 并丢弃事件（kiana-ai/src/event_stream.rs 的 `push` 快路径）。因此 SSE 先发
//!    `done`、之后才出现畸形或失败时，`run_stream_proxy` 仍会构造并 `push` 一条 `error`，
//!    但它被静默忽略，结果保留终态值（见“有意的差异”）。“干净 EOF 但没有终态事件”则由
//!    `request_and_consume` 自行合成一条 `error` 事件。最后 `end_without_result()` 关闭流。
//!
//! ## 用户可见的稳定错误文本（改动需同步测试）
//!
//! - `Proxy error: ${status} ${statusText}` / `Proxy error: ${data.error}`
//! - `Request aborted by user`
//! - `Connection closed by proxy server before the response completed`
//! - `Received text_delta for non-text content`（以及 `text_end`/`thinking_delta`/
//!   `thinking_end`/`toolcall_delta` 的同构文案）
//! - `Unhandled proxy event type: ${type}`（`console.warn` → `tracing::warn!`，文本不带前缀）
//!
//! ## 与 TS 的对应关系
//!
//! - `TextDecoder` 的流式解码 → `Utf8StreamDecoder`（`decode(value, {stream:true})` 与
//!   EOF 时的 `decode()` 刷写；跨 chunk 的多字节序列被挂起，残缺序列在刷写时变成 U+FFFD；
//!   默认 `ignoreBOM = false`：流开头的 U+FEFF 被丢弃）。
//! - `JSON.parse` / `line.slice(6).trim()` → `kiana_json` 的 [`kiana_json::js_parse::parse`]、
//!   [`kiana_json::js_string::js_trim`]（解析错误文本与 V8 的 `SyntaxError.message` 保持一致，便于对拍）；模板字符串里的
//!   `${value}`（`String(v)`）走 [`kiana_json::js_string::js_string`]。
//! - `JSON.stringify({model, context, options})` → `serde_json::to_value` +
//!   [`kiana_json::js_object::stringify`]（键序/数字格式与 `JSON.stringify` 一致）。
//! - `fetch` 的失败文本：连接/发送失败 `fetch failed`，取消 `This operation was aborted`，
//!   响应体中途断开 `terminated`（本机 Node 26 实测；见各常量注释）。
//!
//! ## `response.statusText`
//!
//! Node 的 `fetch` 对 HTTP/1.x 返回状态行里的 reason phrase 原文（自定义文案、空串或标准
//! 短语），对 HTTP/2 返回空串（本机 Node 26 实测：fetch 的 ALPN 同时提供 `http/1.1`/`h2`，
//! h2 响应的 `statusText` 是空串）。Rust 侧从 hyper 写进响应扩展的
//! [`hyper::ext::ReasonPhrase`] 取回 HTTP/1.x 原文（hyper 只在短语与标准短语不同时才保存
//! 它），缺省时退回 `StatusCode::canonical_reason()`；HTTP/2+ 与 Node 一样是空串。非 UTF-8
//! 的短语字节按 `String::from_utf8_lossy` 解码成 U+FFFD，与 Node 一致（实测）。
//!
//! ## 有意的差异
//!
//! - JS 数组允许“空洞”（`partial.content[i] = ...` 跳过下标时），空洞是 `undefined` 并参与
//!   类型检查；`Vec` 没有 `undefined`：跳过下标处用空文本块占位，但下标记在
//!   [`ProxyEventState::holes`] 里，`*_delta`/`*_end` 落到空洞上仍按 TS 抛错。最终消息里
//!   空洞序列化成空文本块而不是 `null`（只有畸形服务端会制造空洞）。
//! - 下标超过 `MAX_PADDED_CONTENT_INDEX` 的块存进 [`ProxyEventState::far_content`] 字典
//!   （V8 稀疏数组的 dictionary mode），不物化到 `partial.content`：TS 的下标赋值是 O(1)，
//!   而 `Vec` 填充会被一帧 `contentIndex: 100000000` 放大成几十 GB 的分配。这类块的
//!   delta/end 事件照常推送，但事件与最终消息的 `partial.content` 里看不到它。
//! - HTTP(S)_PROXY 等环境变量被忽略（`Client::builder().no_proxy()`）：Node 的 `fetch` 默认
//!   不读这些变量（undici 的代理要显式装配），而 reqwest 的 `system-proxy` feature 默认会读。
//! - JS 的 `undefined` 与 `null` 可区分，且 `JSON.stringify` 保留 `null`、丢弃 `undefined`；
//!   Rust 的 `Option` 把两者合并成 `None`（键被丢弃）。因此服务端显式发送的
//!   `"namespace": null` 会保留客户端值（TS 写入 `null`），`"providerThinkingLevel": null`
//!   清空该字段（TS 写入 `null`），`"errorMessage": null` / `"contentSignature": null` 丢掉
//!   键（TS 写入 `null`）。
//! - 已知 `type` 但字段形态超出 JS 可转换范围的事件（`reason` 缺失或不在枚举里、
//!   `errorMessage`/`contentSignature` 不是字符串、`contentIndex` 不是规范整数、
//!   `toolCall.arguments` 不是对象等）：TS 的无校验转换把任意值带进状态机；Rust 的 serde
//!   反序列化失败，整个流以 serde 文案的 error 事件结束。可表示的宽容形态（缺失 `usage`、
//!   数字字符串 `contentIndex`、非字符串/缺失 `delta`、`toolCall` 缺失
//!   `type`/`id`/`name`/`arguments`）由 `normalize_proxy_event_value` 按 TS 语义补齐。
//! - `stream.result()` 的 JS 共享对象语义：TS 用**同一个** `partial` 对象 resolve 结果，终态事件
//!   之后继续处理流（畸形行、abort 与终态竞争）会改写已经 resolve 的结果；Rust 的终态事件
//!   携带 `AssistantMessage` 的所有权副本，`AssistantMessageEventStream::push` 在终态之后不再
//!   接收事件，因此结果保留终态时刻的值（只在非规范流或 abort 竞争时可见）。
//! - `toolcall_delta` 的复杂度：TS 的 `append()` 返回共享快照对象、事件里的 `partial` 是共享
//!   引用，每个 delta 基本 O(1)；Rust 的 `AssistantMessageEvent { partial: Box<AssistantMessage> }`
//!   与 kiana-ai 的 `append() -> JsonObject` 在冻结签名下每次都要深拷贝累积参数，回归 #9265 的
//!   520 KB / ~15k delta 用例约 4.2 s（断言上限 10 s）。多出来的克隆在 kiana-ai 的快照/事件
//!   所有权模型里，本单元内无法消除。

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};

use kiana_ai::AssistantMessageEventStream;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, CacheRetention, DoneReason,
    ErrorReason, JsonObject, Model, ProviderHeaders, SimpleStreamOptions, StopReason, TextContent,
    ThinkingBudgets, ThinkingContent, ThinkingLevel, ToolCall, TranscriptContext, Transport, Usage,
};
use kiana_ai::utils::streaming_json::ToolCallArgumentsStream;

/// `fetch` 因调用方取消（`AbortSignal`）而在拿到响应头之前失败时，Node 抛出的
/// `DOMException` 文本（本机 Node 26 实测）。
const FETCH_ABORTED_MESSAGE: &str = "This operation was aborted";
/// Node `fetch` 在连接/发送阶段失败时的 `TypeError` 文本（本机 Node 26 实测；细节在 `cause` 里）。
const FETCH_FAILED_MESSAGE: &str = "fetch failed";
/// Node `fetch` 的 body reader 在传输中途断开时的 `TypeError` 文本（本机 Node 26 实测）。
const BODY_TERMINATED_MESSAGE: &str = "terminated";
/// 循环出口发现 `signal.aborted` 时抛出的文本（TS 逐字；由 `reader.cancel` 的正常收尾路径产生）。
const REQUEST_ABORTED_BY_USER: &str = "Request aborted by user";
/// 没有 `done`/`error` 终态事件就 EOF 时合成的错误文本（TS 逐字）。
const CONNECTION_CLOSED_MESSAGE: &str =
    "Connection closed by proxy server before the response completed";

/// 服务端事件（partial 字段被剥离以省带宽）。
///
/// TS 是 12 个字符串字面量联合；Rust 用 `#[serde(tag = "type")]`，变体顺序与 TS 的声明顺序
/// 一致（`untagged` 不适用：判别字段总是存在）。未知类型由 `process_line` 过滤（对应
/// TS `switch` 的 `default`）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProxyAssistantMessageEvent {
    /// 流开始；事件本身不携带数据字段。
    Start,
    /// 在 `contentIndex` 处开始一个文本块（客户端先放入空文本块）。
    TextStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
    },
    /// 把 `delta` 追加到该下标的文本块。
    TextDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
    },
    /// 结束文本块；`contentSignature` 是 provider 私有的回放数据（可选）。
    TextEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        #[serde(
            rename = "contentSignature",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        content_signature: Option<String>,
    },
    /// 在 `contentIndex` 处开始一个思考块。
    ThinkingStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
    },
    /// 把 `delta` 追加到该下标的思考块。
    ThinkingDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
    },
    /// 结束思考块；`contentSignature` 是 provider 私有的回放数据（可选）。
    ThinkingEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        #[serde(
            rename = "contentSignature",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        content_signature: Option<String>,
    },
    /// 开始工具调用块；`id`/`toolName` 来自服务端，参数随后经 `toolcall_delta` 流式到达。
    ToolcallStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
    },
    /// 工具参数 JSON 文本的增量片段（未解析的原始文本）。
    ToolcallDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
    },
    /// 工具调用结束，携带服务端给出的最终 `toolCall` 对象。
    ToolcallEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        #[serde(rename = "toolCall")]
        tool_call: ToolCall,
    },
    /// 正常结束；`usage` 与可选的 `providerThinkingLevel` 回填到消息。
    Done {
        reason: ProxyDoneReason,
        usage: kiana_ai::types::Usage,
        #[serde(
            rename = "providerThinkingLevel",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        provider_thinking_level: Option<String>,
    },
    /// 失败结束；`reason` 区分取消与错误，`errorMessage` 是可选文案。
    Error {
        reason: ProxyErrorReason,
        #[serde(
            rename = "errorMessage",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        error_message: Option<String>,
        usage: kiana_ai::types::Usage,
        #[serde(
            rename = "providerThinkingLevel",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        provider_thinking_level: Option<String>,
    },
}

/// `ProxyAssistantMessageEvent` 的 12 个 `type` 字面量（顺序同 TS 的联合声明；
/// spec 里的「13 条事件映射」把 `switch` 的 `default` 也算一条）。
const PROXY_EVENT_TYPES: [&str; 12] = [
    "start",
    "text_start",
    "text_delta",
    "text_end",
    "thinking_start",
    "thinking_delta",
    "thinking_end",
    "toolcall_start",
    "toolcall_delta",
    "toolcall_end",
    "done",
    "error",
];

/// `done` 的 reason 子集：`"stop" | "length" | "toolUse"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProxyDoneReason {
    /// 模型自然结束本轮。
    Stop,
    /// 达到输出 token 上限。
    Length,
    /// 模型请求调用工具。
    ToolUse,
}

/// `error` 的 reason 子集：`"aborted" | "error"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProxyErrorReason {
    /// 调用方取消（取消令牌已触发）。
    Aborted,
    /// 传输、解析或服务端错误。
    Error,
}

impl From<ProxyDoneReason> for DoneReason {
    fn from(reason: ProxyDoneReason) -> Self {
        match reason {
            ProxyDoneReason::Stop => DoneReason::Stop,
            ProxyDoneReason::Length => DoneReason::Length,
            ProxyDoneReason::ToolUse => DoneReason::ToolUse,
        }
    }
}

impl From<ProxyErrorReason> for ErrorReason {
    fn from(reason: ProxyErrorReason) -> Self {
        match reason {
            ProxyErrorReason::Aborted => ErrorReason::Aborted,
            ProxyErrorReason::Error => ErrorReason::Error,
        }
    }
}

/// `processProxyEvent` 里 TS `throw new Error(...)` 的五种文案。
///
/// `Display` 逐字等于 TS 的 `error.message`（ARCHITECTURE.md §4.1）；`stream_proxy` 把它们
/// 转成 `{type:"error", reason:"error", errorMessage}` 事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProxyEventError {
    /// `text_delta` 的目标下标不是文本块：越界、数组空洞，或该位置是别的块类型。
    #[error("Received text_delta for non-text content")]
    TextDeltaForNonText,
    /// `text_end` 的目标下标不是文本块（判据同上）。
    #[error("Received text_end for non-text content")]
    TextEndForNonText,
    /// `thinking_delta` 的目标下标不是思考块（越界、空洞或其它类型）。
    #[error("Received thinking_delta for non-thinking content")]
    ThinkingDeltaForNonThinking,
    /// `thinking_end` 的目标下标不是思考块（判据同上）。
    #[error("Received thinking_end for non-thinking content")]
    ThinkingEndForNonThinking,
    /// 目标不是 toolCall 块，或该下标没有打开的参数流（尚未收到 `toolcall_start`，
    /// 或该流已被 `toolcall_end` 移除）。
    #[error("Received toolcall_delta for non-toolCall content")]
    ToolcallDeltaForNonToolCall,
}

/// `buildProxyRequestOptions(options)` 的全字段对象（缺失值为 `None`，序列化时丢弃）。
///
/// 字段集 = `Pick<SimpleStreamOptions, "temperature"|"samplingParams"|"maxTokens"|"reasoning"|
/// "cacheRetention"|"sessionId"|"headers"|"metadata"|"transport"|"thinkingBudgets"|
/// "maxRetryDelayMs">`，顺序即 TS 的 `Pick` 顺序。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyRequestOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<kiana_ai::types::SamplingParams>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ThinkingLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_retention: Option<CacheRetention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<ProviderHeaders>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_budgets: Option<ThinkingBudgets>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retry_delay_ms: Option<u64>,
}

/// `ProxyStreamOptions` = `buildProxyRequestOptions` 的 11 个字段 + `signal`/`authToken`/`proxyUrl`。
///
/// 字段直接展开而不是内嵌 `SimpleStreamOptions`：`SimpleStreamOptions` 不是 `Clone`
/// （`StreamOptions` 持有回调与取消令牌），而 TS 的 `ProxyStreamOptions` 也只是这 11 个字段的
/// `Pick` 加 3 个自有字段。构造用 [`ProxyStreamOptions::from_simple`]。
#[derive(Clone, Default)]
pub struct ProxyStreamOptions {
    // --- Pick<SimpleStreamOptions, ...>（顺序即 TS 的 Pick 顺序）----------------
    pub temperature: Option<f64>,
    pub sampling_params: Option<kiana_ai::types::SamplingParams>,
    pub max_tokens: Option<u64>,
    pub reasoning: Option<ThinkingLevel>,
    pub cache_retention: Option<CacheRetention>,
    pub session_id: Option<String>,
    pub headers: Option<ProviderHeaders>,
    pub metadata: Option<JsonObject>,
    pub transport: Option<Transport>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub max_retry_delay_ms: Option<u64>,

    // --- ProxyStreamOptions 自有字段 ------------------------------------------
    /// TS `signal?: AbortSignal` → `CancellationToken`（`ARCHITECTURE.md` §4.2）。
    ///
    /// 取消在三个等待点被 `select!` 观察：等响应头、读 2xx body、读非 2xx 错误 body；
    /// 读 2xx body 的每块之后还会再检查一次。产生的错误文案见模块文档“执行流程”。
    pub signal: Option<tokio_util::sync::CancellationToken>,
    /// 写进请求头 `Authorization: Bearer {auth_token}`，这里不做校验或转义。
    pub auth_token: String,
    /// 请求前缀；实际 URL 是 `{proxy_url}/api/stream`（纯字符串拼接，不归一化尾斜杠）。
    pub proxy_url: String,
}

impl ProxyStreamOptions {
    /// 从 `SimpleStreamOptions` + 认证信息构造（`signal` 由调用方另行给出，因为
    /// `SimpleStreamOptions.base.signal` 是 provider 层的取消令牌，语义不同）。
    pub fn from_simple(
        options: &SimpleStreamOptions,
        auth_token: impl Into<String>,
        proxy_url: impl Into<String>,
    ) -> Self {
        let base = &options.base;
        ProxyStreamOptions {
            temperature: base.temperature,
            sampling_params: base.sampling_params.clone(),
            max_tokens: base.max_tokens,
            reasoning: options.reasoning,
            cache_retention: base.cache_retention,
            session_id: base.session_id.clone(),
            headers: base.headers.clone(),
            metadata: base.metadata.clone(),
            transport: base.transport,
            thinking_budgets: options.thinking_budgets.clone(),
            max_retry_delay_ms: base.max_retry_delay_ms,
            signal: base.signal.clone(),
            auth_token: auth_token.into(),
            proxy_url: proxy_url.into(),
        }
    }

    /// `buildProxyRequestOptions(options)`：11 个字段的全字段对象。
    pub fn build_request_options(&self) -> ProxyRequestOptions {
        ProxyRequestOptions {
            temperature: self.temperature,
            sampling_params: self.sampling_params.clone(),
            max_tokens: self.max_tokens,
            reasoning: self.reasoning,
            cache_retention: self.cache_retention,
            session_id: self.session_id.clone(),
            headers: self.headers.clone(),
            metadata: self.metadata.clone(),
            transport: self.transport,
            thinking_budgets: self.thinking_budgets.clone(),
            max_retry_delay_ms: self.max_retry_delay_ms,
        }
    }
}

/// 请求体：`{ model, context, options: buildProxyRequestOptions(options) }`。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProxyRequestBody {
    pub model: Model,
    pub context: TranscriptContext,
    pub options: ProxyRequestOptions,
}

/// `streamProxy(model, context, options)`。
///
/// 返回的流由异步任务推送；调用方按 TS 的 `for await` 语义消费。错误一律走流上的
/// `error` 事件（`partial.stopReason` 为 `"error"`/`"aborted"`），**不从函数本身返回 `Err`**
/// ——函数本身永不抛错，异常一律转成流上的 error 事件。
pub fn stream_proxy(
    model: Model,
    context: TranscriptContext,
    options: ProxyStreamOptions,
) -> AssistantMessageEventStream {
    let stream = kiana_ai::create_assistant_message_event_stream();
    let task_stream = stream.clone();
    let failure_stream = stream.clone();
    let failure_model = model.clone();
    spawn_stream_task(
        async move {
            run_stream_proxy(model, context, options, task_stream).await;
        },
        move || {
            let message = AssistantMessage {
                stop_reason: StopReason::Error,
                error_message: Some("failed to start the proxy stream runtime".to_owned()),
                api: failure_model.api.clone(),
                provider: failure_model.provider.clone(),
                model: failure_model.id.clone(),
                timestamp: now_unix_ms(),
                ..AssistantMessage::default()
            };
            let _ = failure_stream.push(AssistantMessageEvent::Error {
                reason: ProxyErrorReason::Error.into(),
                error: Box::new(message),
            });
            failure_stream.end_without_result();
        },
    );
    stream
}

/// `streamProxy` 里的异步 IIFE：初始化 partial，跑请求，把任何失败折叠成 `error` 事件，
/// 最后 `stream.end()`（TS 的无参 `end()` → [`AssistantMessageEventStream::end_without_result`]；
/// 终态事件已由 `push` resolve `result()`）。
async fn run_stream_proxy(
    model: Model,
    context: TranscriptContext,
    options: ProxyStreamOptions,
    stream: AssistantMessageEventStream,
) {
    let mut partial = AssistantMessage {
        stop_reason: StopReason::Pending,
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage: Usage::default(),
        timestamp: now_unix_ms(),
        ..AssistantMessage::default()
    };

    if let Err(error_message) =
        request_and_consume(&model, &context, &options, &mut partial, &stream).await
    {
        // TS: `const reason = options.signal?.aborted ? "aborted" : "error";`
        let reason = if signal_aborted(&options.signal) {
            ProxyErrorReason::Aborted
        } else {
            ProxyErrorReason::Error
        };
        partial.stop_reason = match reason {
            ProxyErrorReason::Aborted => StopReason::Aborted,
            ProxyErrorReason::Error => StopReason::Error,
        };
        partial.error_message = Some(error_message);
        let _ = stream.push(AssistantMessageEvent::Error {
            reason: reason.into(),
            error: Box::new(partial),
        });
    }

    stream.end_without_result();
}

/// 发起 `POST ${proxyUrl}/api/stream` 并消费响应行流。`Err` 的字符串就是 TS 的
/// `error.message`（最终写进 `partial.errorMessage`）。
///
/// 执行分四段：
///
/// 1. 序列化请求体并发送，等待响应头（有取消令牌时可被取消打断）；
/// 2. 非 2xx：拼状态行文案，读错误 body 并尝试用其中的 `error` 字段覆盖；
/// 3. 2xx：按 chunk 读取并增量解码，按行应用事件，直到 EOF；
/// 4. EOF 后刷写解码器、处理没有换行结尾的尾行，并检查是否见过终态事件。
///
/// 错误路径（URL 解析、连接、非 2xx、行解析、事件类型不匹配、取消）一律返回 `Err(String)`，
/// 由 `run_stream_proxy` 尝试转成流上的 `error` 事件：仅当流尚未被更早的 `done`/`error`
/// 关闭时消费者才会收到；终态之后 `run_stream_proxy` 仍会构造并 `push`，但会被
/// `AssistantMessageEventStream::push` 的 `done` 快路径静默忽略（见模块文档“有意的差异”）。
/// 本函数自己唯一 `push` 的事件是第 4 段 “没有终态事件的干净 EOF”对应的 `error`；
/// 服务端的 `error` 事件则由 `process_line` 经 `process_proxy_event` 推送。
async fn request_and_consume(
    model: &Model,
    context: &TranscriptContext,
    options: &ProxyStreamOptions,
    partial: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
) -> Result<(), String> {
    let body = ProxyRequestBody {
        model: model.clone(),
        context: context.clone(),
        options: options.build_request_options(),
    };
    // `JSON.stringify`：键序（model, context, options）与数字格式都按 JS 语义写出。
    let body_value = serde_json::to_value(&body).map_err(|error| error.to_string())?;
    let body_text = kiana_json::js_object::stringify(&body_value);

    let url_text = format!("{}/api/stream", options.proxy_url);
    let url = reqwest::Url::parse(&url_text)
        .map_err(|_| format!("Failed to parse URL from {url_text}"))?;

    let request = http_client()
        .ok_or_else(|| FETCH_FAILED_MESSAGE.to_string())?
        .post(url)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", options.auth_token),
        )
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body_text);

    // 第一段：等响应头。取消时 Node 的 fetch 以 `AbortError` 拒绝（文本见常量）。
    let response = match &options.signal {
        Some(signal) => tokio::select! {
            biased;
            _ = signal.cancelled() => return Err(FETCH_ABORTED_MESSAGE.to_string()),
            result = request.send() => result.map_err(|_| FETCH_FAILED_MESSAGE.to_string())?,
        },
        None => request
            .send()
            .await
            .map_err(|_| FETCH_FAILED_MESSAGE.to_string())?,
    };

    let status = response.status();
    if !status.is_success() {
        let mut error_message = format!(
            "Proxy error: {} {}",
            status.as_u16(),
            response_status_text(&response)
        );
        // `await response.json()`：解析失败（含空 body、取消）时保留状态行文案。
        //
        // TS 的 `response.json()` 被 signal 取消时以 `AbortError` 拒绝，内层 `catch {}` 吞掉后
        // 仍然 `throw new Error(errorMessage)`；外层 catch 再按 `signal.aborted` 给出
        // `reason = "aborted"` 和同一条状态行文案。因此取消时返回的是**状态行文案**（而不是
        // `This operation was aborted`），并且必须立刻返回——否则会卡在错误 body 上等 EOF。
        let body = match &options.signal {
            Some(signal) => tokio::select! {
                biased;
                _ = signal.cancelled() => None,
                result = response.text() => Some(result),
            },
            None => Some(response.text().await),
        };
        if let Some(Ok(text)) = body
            && let Ok(data) = kiana_json::js_parse::parse(&text)
            && let Some(error) = data.get("error")
            && kiana_json::js_string::is_truthy(Some(error))
        {
            error_message = format!(
                "Proxy error: {}",
                kiana_json::js_string::js_string(Some(error))
            );
        }
        return Err(error_message);
    }

    // `fetch` 对 null body 状态（204/205）返回 `body === null`，`response.body!.getReader()`
    // 于是抛 TypeError；这里保留同一条文案。304 虽然也在下面的匹配里，但它不是 2xx，
    // 会先被上面的非 2xx 分支拦截（测试 `reports_the_null_body_type_error_for_no_content_responses`
    // 锁定 304 的文案是状态行）。
    if matches!(status.as_u16(), 204 | 205 | 304) {
        return Err("Cannot read properties of null (reading 'getReader')".to_string());
    }

    let mut response = response;
    let mut decoder = Utf8StreamDecoder::default();
    let mut buffer = String::new();
    let mut saw_terminal_event = false;
    // 打开中的工具调用参数流与稀疏 content 的簿记。
    let mut state = ProxyEventState::default();

    loop {
        let chunk = match &options.signal {
            Some(signal) => tokio::select! {
                biased;
                _ = signal.cancelled() => return Err(REQUEST_ABORTED_BY_USER.to_string()),
                result = response.chunk() => {
                    result.map_err(|_| BODY_TERMINATED_MESSAGE.to_string())?
                }
            },
            None => response
                .chunk()
                .await
                .map_err(|_| BODY_TERMINATED_MESSAGE.to_string())?,
        };
        let Some(chunk) = chunk else { break };
        // TS 在 `reader.read()` 返回之后才检查 aborted。
        if signal_aborted(&options.signal) {
            return Err(REQUEST_ABORTED_BY_USER.to_string());
        }
        buffer.push_str(&decoder.decode(&chunk, true));
        drain_complete_lines(
            &mut buffer,
            partial,
            &mut state,
            stream,
            &mut saw_terminal_event,
        )?;
    }

    if signal_aborted(&options.signal) {
        return Err(REQUEST_ABORTED_BY_USER.to_string());
    }

    // 最后一帧可能不以换行结尾：刷写解码器并处理残余 buffer（回归 #8996）。
    buffer.push_str(&decoder.decode(&[], false));
    if !buffer.is_empty() && process_line(&buffer, partial, &mut state, stream)? {
        saw_terminal_event = true;
    }

    if !saw_terminal_event {
        // 没有终态事件的干净 EOF：服务端中途断流。合成 error 事件而不是让消费者永远等下去。
        partial.stop_reason = StopReason::Error;
        partial.error_message = Some(CONNECTION_CLOSED_MESSAGE.to_string());
        let _ = stream.push(AssistantMessageEvent::Error {
            reason: ErrorReason::Error,
            error: Box::new(partial.clone()),
        });
    }

    Ok(())
}

/// `buffer.split("\n")` + `buffer = lines.pop() || ""`：处理所有完整行，保留最后一段。
fn drain_complete_lines(
    buffer: &mut String,
    partial: &mut AssistantMessage,
    state: &mut ProxyEventState,
    stream: &AssistantMessageEventStream,
    saw_terminal_event: &mut bool,
) -> Result<(), String> {
    let Some(last_newline) = buffer.rfind('\n') else {
        return Ok(());
    };
    let remainder = buffer[last_newline + 1..].to_string();
    {
        let complete = &buffer[..last_newline];
        for line in complete.split('\n') {
            if process_line(line, partial, state, stream)? {
                *saw_terminal_event = true;
            }
        }
    }
    *buffer = remainder;
    Ok(())
}

/// `processLine(line)`。返回 `true` 表示推入了终态事件（`done`/`error`）。
fn process_line(
    line: &str,
    partial: &mut AssistantMessage,
    state: &mut ProxyEventState,
    stream: &AssistantMessageEventStream,
) -> Result<bool, String> {
    let Some(payload) = line.strip_prefix("data: ") else {
        return Ok(false);
    };
    let data = kiana_json::js_string::js_trim(payload);
    if data.is_empty() {
        return Ok(false);
    }
    // `JSON.parse(data)`：失败时是 V8 的 `SyntaxError.message`（kiana_json 逐字复刻）。
    let mut value = kiana_json::js_parse::parse(data).map_err(|error| error.to_string())?;

    // `JSON.parse("null")` 之后 `proxyEvent.type` 在 JS 里先抛 TypeError（进入 switch 之前）。
    if value.is_null() {
        return Err("Cannot read properties of null (reading 'type')".to_string());
    }
    let Some(kind) = value.get("type") else {
        warn_unhandled_proxy_event(None);
        return Ok(false);
    };
    let Some(kind_name) = kind.as_str() else {
        warn_unhandled_proxy_event(Some(kind));
        return Ok(false);
    };
    if !PROXY_EVENT_TYPES.contains(&kind_name) {
        // TS 是 `as ProxyAssistantMessageEvent` 的无校验转换，未知类型落到 switch 的 default。
        warn_unhandled_proxy_event(Some(kind));
        return Ok(false);
    }

    // TS 是无校验转换：把 TS 会容忍、Rust 的冻结事件类型又能表示的字段形态补齐。
    if !normalize_proxy_event_value(&mut value, partial, state) {
        // `processProxyEvent` 在这里返回 `undefined`（不推事件、也不报错）。
        return Ok(false);
    }

    let proxy_event: ProxyAssistantMessageEvent =
        serde_json::from_value(value).map_err(|error| error.to_string())?;
    let terminal = matches!(
        proxy_event,
        ProxyAssistantMessageEvent::Done { .. } | ProxyAssistantMessageEvent::Error { .. }
    );
    let event =
        process_proxy_event(proxy_event, partial, state).map_err(|error| error.to_string())?;
    let Some(event) = event else {
        return Ok(false);
    };
    let _ = stream.push(event);
    Ok(terminal)
}

/// `console.warn(`Unhandled proxy event type: ${type}`)`。`type` 用 `String(v)` 语义插值
/// （缺失是 `undefined`，对象是 `[object Object]`，…）。
fn warn_unhandled_proxy_event(kind: Option<&serde_json::Value>) {
    tracing::warn!(
        "Unhandled proxy event type: {}",
        kiana_json::js_string::js_string(kind)
    );
}

/// `JSON.parse(data) as ProxyAssistantMessageEvent` 的无校验转换：把 TS 会接受、Rust 的冻结
/// 事件类型又能表示的字段形态补齐。返回 `false` 表示 TS 在 `processProxyEvent` 里直接返回
/// `undefined`（不推事件）。
///
/// 规范化只处理“Rust 类型仍然能忠实表达”的差异：
/// - `contentIndex`：JS 的数组下标访问把数字字符串当成下标（`content["0"]` 就是 `content[0]`），
///   规范的数字字符串转成数字；
/// - `delta`：`content.text += delta` / `args.append(delta)` 是 JS 字符串拼接，缺失按
///   `String(undefined) = "undefined"`、非字符串按 `String(v)` 处理；
/// - `done`/`error` 的 `usage`：缺失/`null`/缺字段时补 0。TS 里缺失的 usage 是 `undefined`，
///   而 Rust 的 `Usage` 是必填结构体，这里只能退化为全 0（见模块文档的偏差）；
/// - `done`/`error` 的 `providerThinkingLevel`：显式 `null` 在 TS 里是“赋值为 null”，会覆盖
///   之前的值；Rust 的 `Option<String>` 只能表达“清空”，事件里的 `None` 又表示“不改”，
///   因此在这里先把 `partial.provider_thinking_level` 清空；
/// - `toolcall_end` 的 `toolCall`：`Object.assign(content, toolCall)` 只复制源对象**存在**的
///   字段，缺失的 `type`/`id`/`name`/`arguments` 保留客户端流式累积值——这里用当前块补齐，
///   效果与“不覆盖”相同；目标下标不是 toolCall（含数组空洞）时 TS 直接返回 `undefined`，
///   这里同样跳过（也不去看 payload）。
fn normalize_proxy_event_value(
    value: &mut serde_json::Value,
    partial: &mut AssistantMessage,
    state: &ProxyEventState,
) -> bool {
    let Some(object) = value.as_object_mut() else {
        return true;
    };
    let Some(kind) = object
        .get("type")
        .and_then(|kind| kind.as_str())
        .map(str::to_string)
    else {
        return true;
    };
    let has_content_index = matches!(
        kind.as_str(),
        "text_start"
            | "text_delta"
            | "text_end"
            | "thinking_start"
            | "thinking_delta"
            | "thinking_end"
            | "toolcall_start"
            | "toolcall_delta"
            | "toolcall_end"
    );
    if has_content_index
        && let Some(index) = object.get_mut("contentIndex")
        && let Some(text) = index.as_str()
        && let Ok(parsed) = text.parse::<u64>()
        && parsed.to_string() == text
    {
        *index = serde_json::Value::from(parsed);
    }
    match kind.as_str() {
        "text_delta" | "thinking_delta" | "toolcall_delta" => {
            if !object
                .get("delta")
                .is_some_and(serde_json::Value::is_string)
            {
                let delta = kiana_json::js_string::js_string(object.get("delta"));
                object.insert("delta".to_string(), serde_json::Value::String(delta));
            }
        }
        "done" | "error" => {
            normalize_usage(object);
            // `providerThinkingLevel: null` 覆盖之前的值（见函数文档）。
            if object
                .get("providerThinkingLevel")
                .is_some_and(serde_json::Value::is_null)
            {
                partial.provider_thinking_level = None;
            }
        }
        "toolcall_end" => {
            let Some(index) = object
                .get("contentIndex")
                .and_then(serde_json::Value::as_u64)
            else {
                // `partial.content[<非数字>]` → `undefined` → TS 返回 `undefined`。
                return false;
            };
            let Some(AssistantContentBlock::ToolCall(client)) =
                content_block(partial, state, index as usize)
            else {
                // `content?.type === "toolCall"` 不成立 → TS 返回 `undefined`。
                return false;
            };
            let tool_call = object
                .entry("toolCall".to_string())
                .or_insert(serde_json::Value::Null);
            if !tool_call.is_object() {
                // `Object.assign(content, 原始值)` 不复制任何字段。
                *tool_call = serde_json::Value::Object(serde_json::Map::new());
            }
            let tool_call = tool_call.as_object_mut().expect("object");
            tool_call
                .entry("type".to_string())
                .or_insert_with(|| serde_json::Value::String("toolCall".to_string()));
            tool_call
                .entry("id".to_string())
                .or_insert_with(|| serde_json::Value::String(client.id.clone()));
            tool_call
                .entry("name".to_string())
                .or_insert_with(|| serde_json::Value::String(client.name.clone()));
            tool_call.entry("arguments".to_string()).or_insert_with(|| {
                serde_json::Value::Object(
                    client
                        .arguments
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                )
            });
        }
        _ => {}
    }
    true
}

/// `partial.usage = proxyEvent.usage`：TS 里任意值都能赋进去（缺失是 `undefined`），Rust 的
/// `Usage` 是必填结构体，缺失/`null`/缺字段时补 0。
fn normalize_usage(object: &mut serde_json::Map<String, serde_json::Value>) {
    if !object
        .get("usage")
        .is_some_and(serde_json::Value::is_object)
    {
        object.insert(
            "usage".to_string(),
            serde_json::Value::Object(serde_json::Map::new()),
        );
    }
    let usage = object
        .get_mut("usage")
        .and_then(serde_json::Value::as_object_mut)
        .expect("usage is an object");
    for key in ["input", "output", "cacheRead", "cacheWrite", "totalTokens"] {
        usage
            .entry(key.to_string())
            .or_insert_with(|| serde_json::Value::from(0));
    }
    if !usage.get("cost").is_some_and(serde_json::Value::is_object) {
        usage.insert(
            "cost".to_string(),
            serde_json::Value::Object(serde_json::Map::new()),
        );
    }
    let cost = usage
        .get_mut("cost")
        .and_then(serde_json::Value::as_object_mut)
        .expect("cost is an object");
    for key in ["input", "output", "cacheRead", "cacheWrite", "total"] {
        cost.entry(key.to_string())
            .or_insert_with(|| serde_json::Value::from(0));
    }
}

/// 物化到 `partial.content` 的最高下标（含）。
///
/// JS 的稀疏数组对任意下标都是 O(1)，空洞不占内存；Rust 的 `Vec<AssistantContentBlock>`
/// （`ARCHITECTURE.md` §4.9 冻结）必须为空洞分配占位块。超过本阈值的下标改存
/// [`ProxyEventState::far_content`] 字典，避免一帧 `contentIndex: 100000000` 触发几十 GB 的
/// 分配（以及每次事件快照的深拷贝）。
const MAX_PADDED_CONTENT_INDEX: usize = 4096;

/// `processProxyEvent` 的第三参数：打开的 toolCall 参数流 + JS 稀疏 content 的簿记。
///
/// TS 只有一个 `toolArguments: Map`；Rust 侧额外记录 `partial.content` 中哪些下标仍是
/// “空洞”（JS 的 `undefined`，`Vec` 用占位块表示）以及超过 `MAX_PADDED_CONTENT_INDEX`
/// 的块，语义见模块文档。
#[derive(Debug, Default)]
pub struct ProxyEventState {
    /// `toolArguments`：打开中的工具调用参数流，按 contentIndex 索引（不挂在 content block 上）。
    pub tool_arguments: HashMap<usize, ToolCallArgumentsStream>,
    /// `partial.content` 中仍是空洞的下标；空洞对 delta/end 表现为 `undefined`。
    pub holes: HashSet<usize>,
    /// 下标超过 `MAX_PADDED_CONTENT_INDEX` 的块（V8 稀疏数组的 dictionary mode）。
    pub far_content: HashMap<usize, AssistantContentBlock>,
}

impl ProxyEventState {
    /// 空的簿记状态。
    pub fn new() -> Self {
        Self::default()
    }
}

/// `processProxyEvent(proxyEvent, partial, toolArguments)`：把服务端事件折叠成 assistant 事件。
///
/// 返回 `Ok(None)` 表示该事件不影响流（`toolcall_end` 遇到非 toolCall 内容）。未知事件类型在
/// `process_line` 里就被过滤（TS 的 `switch` default：`console.warn` + `undefined`），不会
/// 进入本函数。类型不匹配的 delta/end 返回 [`ProxyEventError`]（TS 的 `throw`）。
pub fn process_proxy_event(
    proxy_event: ProxyAssistantMessageEvent,
    partial: &mut AssistantMessage,
    state: &mut ProxyEventState,
) -> Result<Option<AssistantMessageEvent>, ProxyEventError> {
    match proxy_event {
        ProxyAssistantMessageEvent::Start => Ok(Some(AssistantMessageEvent::Start {
            partial: Box::new(partial.clone()),
        })),

        ProxyAssistantMessageEvent::TextStart { content_index } => {
            assign_content_block(
                partial,
                state,
                content_index,
                AssistantContentBlock::Text(TextContent::default()),
            );
            Ok(Some(AssistantMessageEvent::TextStart {
                content_index,
                partial: Box::new(partial.clone()),
            }))
        }

        ProxyAssistantMessageEvent::TextDelta {
            content_index,
            delta,
        } => {
            {
                let Some(AssistantContentBlock::Text(content)) =
                    content_block_mut(partial, state, content_index)
                else {
                    return Err(ProxyEventError::TextDeltaForNonText);
                };
                content.text.push_str(&delta);
            }
            Ok(Some(AssistantMessageEvent::TextDelta {
                content_index,
                delta,
                partial: Box::new(partial.clone()),
            }))
        }

        ProxyAssistantMessageEvent::TextEnd {
            content_index,
            content_signature,
        } => {
            let text = {
                let Some(AssistantContentBlock::Text(content)) =
                    content_block_mut(partial, state, content_index)
                else {
                    return Err(ProxyEventError::TextEndForNonText);
                };
                // TS 直接赋值（`undefined` 也赋），JSON 序列化时 `undefined` 被丢弃。
                content.text_signature = content_signature;
                content.text.clone()
            };
            Ok(Some(AssistantMessageEvent::TextEnd {
                content_index,
                content: text,
                partial: Box::new(partial.clone()),
            }))
        }

        ProxyAssistantMessageEvent::ThinkingStart { content_index } => {
            assign_content_block(
                partial,
                state,
                content_index,
                AssistantContentBlock::Thinking(ThinkingContent::default()),
            );
            Ok(Some(AssistantMessageEvent::ThinkingStart {
                content_index,
                partial: Box::new(partial.clone()),
            }))
        }

        ProxyAssistantMessageEvent::ThinkingDelta {
            content_index,
            delta,
        } => {
            {
                let Some(AssistantContentBlock::Thinking(content)) =
                    content_block_mut(partial, state, content_index)
                else {
                    return Err(ProxyEventError::ThinkingDeltaForNonThinking);
                };
                content.thinking.push_str(&delta);
            }
            Ok(Some(AssistantMessageEvent::ThinkingDelta {
                content_index,
                delta,
                partial: Box::new(partial.clone()),
            }))
        }

        ProxyAssistantMessageEvent::ThinkingEnd {
            content_index,
            content_signature,
        } => {
            let thinking = {
                let Some(AssistantContentBlock::Thinking(content)) =
                    content_block_mut(partial, state, content_index)
                else {
                    return Err(ProxyEventError::ThinkingEndForNonThinking);
                };
                content.thinking_signature = content_signature;
                content.thinking.clone()
            };
            Ok(Some(AssistantMessageEvent::ThinkingEnd {
                content_index,
                content: thinking,
                partial: Box::new(partial.clone()),
            }))
        }

        ProxyAssistantMessageEvent::ToolcallStart {
            content_index,
            id,
            tool_name,
        } => {
            assign_content_block(
                partial,
                state,
                content_index,
                AssistantContentBlock::ToolCall(ToolCall {
                    id,
                    name: tool_name,
                    arguments: JsonObject::new(),
                    ..ToolCall::default()
                }),
            );
            state
                .tool_arguments
                .insert(content_index, ToolCallArgumentsStream::new());
            Ok(Some(AssistantMessageEvent::ToolCallStart {
                content_index,
                partial: Box::new(partial.clone()),
            }))
        }

        ProxyAssistantMessageEvent::ToolcallDelta {
            content_index,
            delta,
        } => {
            // TS：`content?.type === "toolCall" && args !== undefined`。
            let is_tool_call = matches!(
                content_block(partial, state, content_index),
                Some(AssistantContentBlock::ToolCall(_))
            );
            if !is_tool_call || !state.tool_arguments.contains_key(&content_index) {
                return Err(ProxyEventError::ToolcallDeltaForNonToolCall);
            }
            let arguments = state
                .tool_arguments
                .get_mut(&content_index)
                .expect("argument stream checked above")
                .append(&delta);
            let Some(AssistantContentBlock::ToolCall(content)) =
                content_block_mut(partial, state, content_index)
            else {
                unreachable!("toolCall content checked above");
            };
            // `{...content, arguments: args.append(delta)}`：块上永远只有最新 partial 参数，
            // 原始文本留在 `ToolCallArgumentsStream`（#9265）。
            content.arguments = arguments;
            Ok(Some(AssistantMessageEvent::ToolCallDelta {
                content_index,
                delta,
                partial: Box::new(partial.clone()),
            }))
        }

        ProxyAssistantMessageEvent::ToolcallEnd {
            content_index,
            tool_call,
        } => {
            let Some(target) = content_block(partial, state, content_index) else {
                return Ok(None);
            };
            if !matches!(target, AssistantContentBlock::ToolCall(_)) {
                return Ok(None);
            }
            let merged = {
                let Some(AssistantContentBlock::ToolCall(content)) =
                    content_block_mut(partial, state, content_index)
                else {
                    unreachable!("toolCall content checked above");
                };
                // `Object.assign(content, proxyEvent.toolCall)`：服务端字段覆盖，客户端独有的
                // 字段（可选字段在服务端缺省时）保留。
                merge_tool_call(content, tool_call);
                content.clone()
            };
            state.tool_arguments.remove(&content_index);
            Ok(Some(AssistantMessageEvent::ToolCallEnd {
                content_index,
                tool_call: merged,
                partial: Box::new(partial.clone()),
            }))
        }

        ProxyAssistantMessageEvent::Done {
            reason,
            usage,
            provider_thinking_level,
        } => {
            partial.stop_reason = match reason {
                ProxyDoneReason::Stop => StopReason::Stop,
                ProxyDoneReason::Length => StopReason::Length,
                ProxyDoneReason::ToolUse => StopReason::ToolUse,
            };
            partial.usage = usage;
            // `None` = 事件没带该字段，保留之前的值；显式 `null` 已在 normalize 阶段清空。
            if provider_thinking_level.is_some() {
                partial.provider_thinking_level = provider_thinking_level;
            }
            Ok(Some(AssistantMessageEvent::Done {
                reason: reason.into(),
                message: Box::new(partial.clone()),
            }))
        }

        ProxyAssistantMessageEvent::Error {
            reason,
            error_message,
            usage,
            provider_thinking_level,
        } => {
            partial.stop_reason = match reason {
                ProxyErrorReason::Aborted => StopReason::Aborted,
                ProxyErrorReason::Error => StopReason::Error,
            };
            partial.error_message = error_message;
            partial.usage = usage;
            // 同 `Done`：缺省不覆盖，显式 `null` 在 normalize 阶段清空。
            if provider_thinking_level.is_some() {
                partial.provider_thinking_level = provider_thinking_level;
            }
            Ok(Some(AssistantMessageEvent::Error {
                reason: reason.into(),
                error: Box::new(partial.clone()),
            }))
        }
    }
}

/// TS 的 `partial.content[index] = block`：下标赋值。跳过中间下标时 JS 留下空洞；`Vec` 用
/// 空文本块占位并记进 [`ProxyEventState::holes`]。超过 [`MAX_PADDED_CONTENT_INDEX`] 时存进
/// 字典，不做任何填充。
fn assign_content_block(
    partial: &mut AssistantMessage,
    state: &mut ProxyEventState,
    index: usize,
    block: AssistantContentBlock,
) {
    if index > MAX_PADDED_CONTENT_INDEX {
        state.far_content.insert(index, block);
        return;
    }
    while partial.content.len() < index {
        let hole = partial.content.len();
        partial
            .content
            .push(AssistantContentBlock::Text(TextContent::default()));
        state.holes.insert(hole);
    }
    if partial.content.len() == index {
        partial.content.push(block);
    } else {
        partial.content[index] = block;
    }
    state.holes.remove(&index);
}

/// TS 的 `partial.content[index]`：`None` 等价于 JS 的 `undefined`（越界、空洞或未物化的
/// 超远下标）。
fn content_block<'a>(
    partial: &'a AssistantMessage,
    state: &'a ProxyEventState,
    index: usize,
) -> Option<&'a AssistantContentBlock> {
    if index > MAX_PADDED_CONTENT_INDEX {
        return state.far_content.get(&index);
    }
    if state.holes.contains(&index) {
        return None;
    }
    partial.content.get(index)
}

/// [`content_block`] 的可变版本。
fn content_block_mut<'a>(
    partial: &'a mut AssistantMessage,
    state: &'a mut ProxyEventState,
    index: usize,
) -> Option<&'a mut AssistantContentBlock> {
    if index > MAX_PADDED_CONTENT_INDEX {
        return state.far_content.get_mut(&index);
    }
    if state.holes.contains(&index) {
        return None;
    }
    partial.content.get_mut(index)
}

/// `Object.assign(content, toolCall)`：服务端字段覆盖，客户端独有的字段（服务端缺省时会
/// `undefined`，不复制）保留。
fn merge_tool_call(content: &mut ToolCall, server: ToolCall) {
    content.kind_tool_call = server.kind_tool_call;
    content.id = server.id;
    content.name = server.name;
    content.arguments = server.arguments;
    if server.thought_signature.is_some() {
        content.thought_signature = server.thought_signature;
    }
    if server.namespace.is_some() {
        content.namespace = server.namespace;
    }
    if server.arguments_error.is_some() {
        content.arguments_error = server.arguments_error;
    }
}

/// `options.signal?.aborted`（`None` 视为未取消）。
fn signal_aborted(signal: &Option<tokio_util::sync::CancellationToken>) -> bool {
    signal.as_ref().is_some_and(|token| token.is_cancelled())
}

/// 共享 HTTP 客户端：对应 TS 全局 `fetch` 的连接池。
///
/// `Client::builder().build()` 只在 TLS 后端初始化失败这类极少数情况报错；此时按 Node `fetch`
/// 的发送失败文案（`fetch failed`）处理。
///
/// `.no_proxy()`：Node `fetch` 默认不读 `HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY`（代理要显式
/// 装配），而 reqwest 的 `system-proxy` feature 默认会读；这里关掉以保持同一条网络路径。
///
/// 缓存按 **tokio runtime 身份** 区分：`reqwest::Client` 的空闲连接由 hyper-util 在建立该连接
/// 的 runtime 上派发 dispatch task，跨 runtime 复用会以 `DispatchGone`（"runtime dropped the
/// dispatch task"）失败。生产进程只有一个长期存活的 runtime，行为不变；测试二进制里每个
/// `#[tokio::test]` 都有自己的 current-thread runtime，按 runtime 身份缓存可避免后续用例撞上
/// 已销毁 runtime 的空闲连接（proxy 套件偶发的 "fetch failed"）。
fn http_client() -> Option<reqwest::Client> {
    static CLIENT: OnceLock<Mutex<Option<(tokio::runtime::Id, reqwest::Client)>>> = OnceLock::new();
    let handle = tokio::runtime::Handle::try_current().ok()?;
    let mut slot = CLIENT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((id, client)) = slot.as_ref()
        && *id == handle.id()
    {
        return Some(client.clone());
    }
    let client = reqwest::Client::builder().no_proxy().build().ok()?;
    *slot = Some((handle.id(), client.clone()));
    Some(client)
}

/// `response.statusText`。
///
/// Node 的 `fetch` 对 HTTP/1.x 返回状态行里的 reason phrase 原文（自定义文案、空串或标准
/// 短语），对 HTTP/2 返回空串（本机 Node 26 实测：fetch 的 ALPN 同时提供 `http/1.1` 与 `h2`）。
/// hyper 只在短语与标准短语不同时才把它存进响应扩展的 [`hyper::ext::ReasonPhrase`]，其余
/// 情况等价于 `StatusCode::canonical_reason()`；两者合起来就是状态行原文。HTTP/2+ 没有
/// reason phrase，与 Node 一样是空串。
fn response_status_text(response: &reqwest::Response) -> String {
    match response.version() {
        reqwest::Version::HTTP_09 | reqwest::Version::HTTP_10 | reqwest::Version::HTTP_11 => {
            if let Some(reason) = response.extensions().get::<hyper::ext::ReasonPhrase>() {
                // 与 Node 一样按 UTF-8 解码，非法字节变成 U+FFFD（本机 Node 26 实测）。
                return String::from_utf8_lossy(reason.as_bytes()).into_owned();
            }
            response
                .status()
                .canonical_reason()
                .unwrap_or_default()
                .to_string()
        }
        _ => String::new(),
    }
}

/// `Date.now()`：Unix 毫秒。
fn now_unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// 与 `kiana-ai` 的 `spawn_forward` 相同：有 runtime 时交给它；没有时用专用线程 +
/// current-thread runtime 跑完（TS 的 `fetch` 也不要求调用方处于事件循环）。
///
/// 专用线程的 runtime 创建失败时，future 会被静默丢弃：调用方持有的流既收不到事件也不会
/// `end`，消费侧会一直等待。这是系统资源耗尽级别的失败路径，没有对应的 TS 行为可复刻。
fn spawn_stream_task<F, G>(future: F, on_spawn_failure: G)
where
    F: Future<Output = ()> + Send + 'static,
    G: FnOnce() + Send + 'static,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(future);
        return;
    }
    std::thread::spawn(move || {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime.block_on(future),
            // 专用 runtime 建不出来（资源耗尽）时不能静默丢弃 future：调用方会拿到一个
            // 永不产出、也永不结束的流。改为在流上补一个 error 事件并结束它。
            Err(_) => on_spawn_failure(),
        }
    });
}

/// 增量 UTF-8 解码（`new TextDecoder()` 的 `decode(value, {stream: true})` 语义）。
///
/// 解码循环与 `kiana-ai` 的 `openai_codex_responses.rs` 里的同名私有类型一致：跨 chunk 的
/// 多字节序列挂起到下一次解码；非法字节按最大子部分替换为 U+FFFD；`stream = false` 时把
/// 残余的残缺序列刷成一个 U+FFFD。在其之上额外实现了 `TextDecoder` 默认的 BOM 丢弃
/// （`kiana-ai::api::kiana_messages` 的版本显式不处理 BOM，不能与它对齐）。
#[derive(Debug, Default)]
struct Utf8StreamDecoder {
    pending: Vec<u8>,
    /// `new TextDecoder()` 的默认 `ignoreBOM = false`：流开头的 U+FEFF 被丢弃。
    bom_checked: bool,
}

impl Utf8StreamDecoder {
    fn decode(&mut self, chunk: &[u8], stream: bool) -> String {
        self.pending.extend_from_slice(chunk);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(text) => {
                    // 整个待处理缓冲区都是合法 UTF-8：一次交付并清空。
                    out.push_str(text);
                    self.pending.clear();
                    break;
                }
                Err(error) => {
                    // 先交付 `valid_up_to` 之前的合法前缀；这保证跨 chunk 的多字节序列
                    // 不会被提前拆开。
                    let valid = error.valid_up_to();
                    if valid > 0 {
                        out.push_str(
                            std::str::from_utf8(&self.pending[..valid]).expect("valid prefix"),
                        );
                        self.pending.drain(..valid);
                    }
                    match error.error_len() {
                        // `None`：末尾是尚未收全的多字节序列，挂起到下一次 `decode`。
                        // 调用方在 EOF 时以 `stream = false` 收尾（见下方刷写分支）。
                        None => break,
                        // `Some(len)`：`len` 个字节已经确定非法，按 WHATWG 的最大子部分规则
                        // 替换成一个 U+FFFD，然后继续处理剩余字节。
                        Some(len) => {
                            out.push('\u{FFFD}');
                            self.pending.drain(..len);
                        }
                    }
                }
            }
        }
        if !stream && !self.pending.is_empty() {
            // EOF 收尾：残余的残缺序列按替换字符刷出（`from_utf8_lossy` 对同一个残缺
            // 序列只产生一个 U+FFFD）。
            out.push_str(&String::from_utf8_lossy(&self.pending));
            self.pending.clear();
        }
        // `TextDecoder` 默认 `ignoreBOM = false`：只在流的开头丢弃一个 U+FEFF
        // （跨 chunk 拆开的 BOM 字节会先在 `pending` 里拼好）。
        if !self.bom_checked && !out.is_empty() {
            self.bom_checked = true;
            if out.starts_with('\u{FEFF}') {
                out.drain(..'\u{FEFF}'.len_utf8());
            }
        }
        out
    }
}

/// `streamProxy` 的便捷函数类型：收 `(model, context, options)`，直接返回
/// [`AssistantMessageEventStream`]（`Arc` 便于跨任务共享）。
///
/// 注意它与 [`crate::types::StreamFn`] **不是**同一个签名：后者收 `Option<SimpleStreamOptions>`
/// 并返回 `BoxFuture<Result<...>>`，因此本别名不能直接传给
/// [`crate::stream_fn::set_default_stream_fn`]，需要调用方先包一层（把
/// `SimpleStreamOptions` 转成 [`ProxyStreamOptions`]，并自行处理错误与 future 的包装）。
pub type ProxyStreamFn = Arc<
    dyn Fn(Model, TranscriptContext, ProxyStreamOptions) -> AssistantMessageEventStream
        + Send
        + Sync,
>;
