//! `transports/streamable-http.ts`：SSE 解析器 + `StreamableHttpTransport`。
//!
//! 源：`packages/mcp/src/transports/streamable-http.ts`（`spec/telemetry-mcp.md` §17、§20.2、
//! §25 第 8–11 条、§27.3、§30.7/13/14/19/28/29）。
//!
//! # 结构
//!
//! - [`consume_sse_stream`]：WHATWG SSE 子集解析器，**手写**（不能用 `eventsource-stream` 之类）：
//!   字节上限的记账、逐 `id`/`retry` 回调、逐行 CR 剥离、EOF 冲刷都是契约的一部分
//!   （§17.1、§30.28）。
//! - [`StreamableHttpTransport`]：MCP Streamable HTTP 传输。POST 一个 JSON-RPC 消息，在
//!   `application/json` / `text/event-stream` / 202/204 / 其他之间分类；打开服务端→客户端的
//!   GET SSE 流并按指数退避重连（§17.4–§17.12）。
//!
//! # 与 TS 的映射（被迫偏差与已记录偏差）
//!
//! - TS 的三个错误类 `McpHttpError` / `McpAuthRequiredError` / `McpSessionExpiredError` 由统一
//!   错误信封 [`McpTransportError`] 表达（`name()` 逐字返回三个类名，`status()`/`body()`/
//!   `www_authenticate()` 返回对应字段；见 `auth_provider` 模块文档）。这里把它们定义成类型
//!   别名，保住 §19.2 的包根导出名；构造走 `McpTransportError::{http, auth_required,
//!   session_expired}`。
//! - `ReadableStream<Uint8Array>` → [`crate::auth_provider::FetchByteStream`]；`TextDecoder` 的流式 UTF-8 解码在这里
//!   手写（`Utf8Decoder`：复刻 Node 的 UTF-8 fast path，包括它那条"补齐跨 chunk 多字节序列
//!   的调用里连带丢掉紧随其后的 BOM"的规则——朴素实现会保留那个 BOM，进而改变
//!   `Buffer.byteLength(buffered)` 的超限判定；见 `Utf8Decoder` 的文档与
//!   `tests/fixtures/decoder_vectors.json`）。
//! - `contentType()` 的 `trim()`/`toLowerCase()` 用 **JS 的**集合（`content_type`）：JS 的
//!   `trim` 含 U+FEFF、不含 U+0085，而 `str::trim()` 相反；头值落在 latin1 域，`toLowerCase`
//!   会把 `À`–`Þ` 也小写。裸 0x85 字节（undici 按 latin1 解码）因此有一条可观测的分支。
//! - `send` 的 JSON 分支**不**看 `response.body`（TS 只写 `await response.json()`）：fetch 的
//!   无体状态（101/103/204/205/304）上 `text()` 是 `""`，`JSON.parse("")` 抛
//!   `SyntaxError: Unexpected end of JSON input`。205 + JSON 是唯一走得到的情形（202/204 先被
//!   自己的分支拦截），见 `response_has_body`。
//! - `buildHeaders` 的 `if (this.protocolVersion)`：空串在 JS 里是假值，`MCP-Protocol-Version`
//!   **不**发。
//! - `needsAuthorization` 的正则用 **JS 的 `\s`**（`insufficient_scope_challenge`）：JS 含
//!   U+FEFF、不含 U+0085；Rust `regex` 的 `\s` 正好相反，裸 0x85 字节会让后者多匹配一次。
//! - TS 的 `finally { reader.releaseLock() }` 在 Rust 里没有等价物：消费失败时**丢弃**流即取消
//!   底层交换（§30.29 已要求文档化）。TS 不 cancel 底层流，因此服务器可能看到连接提前断开。
//! - `AbortSignal` → [`CancellationToken`]（§4.2）：`send` 的调用方 signal 只决定「是否还派发」，
//!   出站 `fetch` 用 transport 自己的令牌（只有 `close()` 中止）。Node 里 abort 会取消响应体读取，
//!   因此 [`ConsumeSseOptions::cancel`] 把 transport 令牌交给解析器，`close()` 时读循环立即结束。
//! - `setTimeout` 的延迟钳位被复刻（`node_timeout_duration`）：Node 把不在 `[1, 2^31-1]` 内的
//!   延迟（含 `NaN`、`Infinity`、负数、0）钳到 1 ms（§30.28 的 `retry: <超长数字>` → `Infinity`）。
//! - `fetch` **不带接收者**：`Arc<dyn McpFetch>` 天然没有 `this`（§30.19 / 测试
//!   `calls fetch without a receiver`）。
//! - `options.authProvider?.onUnauthorized?.`：TS 的 `onUnauthorized` 是可选成员，缺省时 401 直接
//!   返回、**不**重试。Rust 的 `on_unauthorized` 有 no-op 缺省实现，光看行为分不出「没实现」与
//!   「实现了但什么都不做」，因此 [`AuthProvider`] 增加判别方法 `has_on_unauthorized()`（缺省
//!   `false`）：传输只在它为 `true` 时才把 401/403 交给 provider 并重试一次，与 TS 的
//!   `!onUnauthorized` 分支行为一致（`tests/streamable_http.rs` 的
//!   `does_not_retry_a_401_when_the_provider_has_no_on_unauthorized_handler` 覆盖）。
//!   实现 `on_unauthorized` 的类型必须同时覆盖 `has_on_unauthorized` 返回 `true`。
//! - **401/403 响应体的读取失败被吞掉**：TS 把活的 `Response` 交给 `onUnauthorized`
//!   （provider 读体时才会看到流错误；`discard` 只 `cancel()` 体，不读）。Rust 的
//!   [`UnauthorizedContext::body`] 是 §12 冻结的 `Vec<u8>`（已 drain），没有错误通道，因此
//!   传输在调用 provider **之前**读取体；读取失败（连接中断、超时、`close()` 取消）退化成空
//!   `Vec<u8>`，provider 看到的是空体而不是流错误。本仓库的 `onUnauthorized` 实现只读头，
//!   该差异不可观察（`tests/streamable_http.rs` 的
//!   `a_failed_401_body_read_becomes_an_empty_unauthorized_context_body` 钉住行为）。
//! - 两个 `setTimeout` 语义点：退避等待用 `tokio::select!` 与 transport 令牌（没有 `unref`）；
//!   `close()` 的 DELETE 用 `FetchInit::timeout`（`AbortSignal.timeout(1000)`）。
//!
//! # 公开面
//!
//! 包根导出（§19.2、`index.rs`）：[`StreamableHttpTransport`]、[`StreamableHttpTransportOptions`]、
//! [`McpHttpError`]、[`McpAuthRequiredError`]、[`McpSessionExpiredError`]。
//! 模块内 `pub` 但**不**进包根（§30.1）：[`consume_sse_stream`]、[`SseEvent`]、
//! [`ConsumeSseOptions`]、[`StreamableHttpReconnectOptions`] 与五个常量。
//!
//! # 使用位置
//!
//! 本类型主要通过 [`McpTransport`] trait 被上层使用：[`crate::client::McpClient`] 持有的是
//! `Arc<dyn McpTransport>`，因此 `StreamableHttpTransport` 与 stdio/in-memory 传输可以互换。
//! crate 外可以直接 `new` 后包成 `Arc<dyn McpTransport>`（`kiana-builtin-extensions` 的 MCP
//! runtime 就是这样按配置构造它的）。单元测试在 `tests/streamable_http.rs`，它用测试内的最小
//! 握手辅助替代 `McpClient` 的那一层。
//!
//! # 一次调用的生命周期
//!
//! 1. [`StreamableHttpTransport::new`]：只解析 URL 并选定 `fetch`（缺省是全局共享的
//!    [`crate::auth_provider::ReqwestFetch`]）；不做任何网络 IO。
//! 2. `start()`：只把 `started` 置位，同样不发请求。重复调用报
//!    `MCP Streamable HTTP transport already started`；**已经 start 过、随后 close** 的实例
//!    再调用也报这一条（实现先查 `started` 再查 `closed`）。从未 start 过就先 `close()` 的
//!    实例则返回 `McpConnectionClosedError`（消息 `MCP connection closed`），因为此时
//!    `started` 仍为 `false`（`tests/streamable_http.rs` 的
//!    `start_and_send_failures_use_the_typescript_messages` 只覆盖了前一种顺序）。
//! 3. `send()`：POST 一条消息。`authorized_fetch` 负责取 token、加 `Authorization`，并在
//!    401 / `insufficient_scope` 403 时交给提供方后重试一次；返回后由 `check_response`
//!    把非 2xx 分类成三种 HTTP 错误，再由 `capture_session` 记录 `Mcp-Session-Id`。
//! 4. 非请求消息（通知/响应）只等确认并丢弃体；其中 `notifications/initialized` 会触发
//!    服务端→客户端的 GET 流（每个 transport 至多一次）。
//! 5. 请求消息按 `Content-Type` 分派：`application/json` 解析后逐条派发；
//!    `text/event-stream` 交给后台任务读取（`send` 立即返回）；其余类型报
//!    `Unsupported MCP response content type`。
//! 6. `close()`：置 `closed`、取消 transport 令牌（中止在途 `fetch` 与 SSE 读循环）；已建立
//!    会话时用**最近一次请求记录的 token** 发一个 1 秒超时的 DELETE，最后 `emit_close`
//!    （每个实例至多一次）。DELETE 的失败被忽略——服务器上的会话本来也会过期。
//!
//! # 状态与并发
//!
//! - 全部运行期状态在 `Arc<TransportInner>` 里；GET 流与每个请求的响应流读取任务各自克隆
//!   这个 `Arc`。因此**丢弃 [`StreamableHttpTransport`] 不会让已启动的后台任务退出**。
//!   `close()` 能主动中止仍在读取的任务；任务本身也会自行退出：流正常结束且无法继续
//!   （响应流没有恢复点）、答复到达、遇到不可重试错误、重试上限用尽，或 GET 流收到 405。
//! - `started`/`closed`/`get_stream_started` 是 `AtomicBool`，本文件统一用 `SeqCst` 读写；
//!   `session_id`/`protocol_version`/`last_token` 是 `Mutex<Option<String>>`。所有取锁点都
//!   对中毒做了降级（读成 `None`、跳过更新或退出重连循环）：锁中毒不会在这里 panic。
//! - `authorized_fetch` 的每次尝试都用 transport 令牌作为 `FetchInit` 的取消信号；
//!   `send` 调用方传的 `signal` 只在每次派发**之前**检查，不会中止已经发出的交换。
//! - 并发 `send` 会各自读 token 并写 `last_token`（后写者生效）；`close()` 读到的就是最后
//!   写入的那个值，因此 DELETE 携带的未必是每个请求自己的 token。
//!
//! # 错误传播
//!
//! `start`/`send`/`close` 把错误返回给调用方；后台任务（GET 流、响应流）没有调用方，走两条
//! 路：可报告的错误（连接失败、SSE 解析失败等）经 `TransportEvents::emit_error` 交给监听器，
//! 请求级失败则合成一个 `-32603` 的 JSON-RPC 错误响应经 `emit_message` 发出（见
//! `consume_response_stream`）。是否重连由
//! [`McpTransportError::is_retryable`] 判定：瞬时 HTTP 状态（408/429/5xx）、`TypeError`
//! （网络失败）与 `E*`/`UND_ERR_*` 码可重试；其余错误不重连——GET 流报告一次后停止，
//! 响应流则合成错误响应、结束请求方的等待。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http::Method;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::auth_provider::{
    AuthProvider, FetchBody, FetchInit, FetchResponse, JsHeaders, McpFetch, McpTransportError,
    UnauthorizedContext,
};
use crate::jsonrpc::{
    JsonRpcErrorCodes, JsonRpcId, is_json_rpc_request, is_json_rpc_response,
    parse_json_rpc_message, value_to_json_rpc_id,
};
use crate::transports::{
    DEFAULT_MAX_MESSAGE_BYTES, McpTransport, McpTransportCloseListener, McpTransportErrorListener,
    McpTransportMessageListener, McpTransportSendOptions, Subscription, TransportEvents,
};

/// `MAX_ERROR_BODY_BYTES = 8 * 1024`：`checkResponse` 保留的错误体上限（UTF-16 代码单元）。
///
/// 截断发生在构造 [`McpTransportError`] **之前**，因此错误对象的 [`McpTransportError::body`]
/// 已经是至多 8192 个单元的文本（构造器本身不截断，见该方法的文档）。它只约束错误对象；
/// `describe_http_failure` 的消息摘要另有一个更短的上限 [`ERROR_MESSAGE_BODY_CHARS`]。
pub const MAX_ERROR_BODY_BYTES: usize = 8 * 1024;

/// `ERROR_MESSAGE_BODY_CHARS = 500`：`describeHttpFailure` 摘要的字符数上限。
///
/// 超限时保留前 497 个 UTF-16 代码单元再拼 `...`（`500 - 3`，按 JS `slice` 的语义；截断点
/// 落在代理对中间时补一个 U+FFFD，见 `utf16_prefix`）。它只影响错误消息里 `: ` 之后的
/// 摘要，不影响 [`McpTransportError::body`]（后者由 [`MAX_ERROR_BODY_BYTES`] 限制）。
pub const ERROR_MESSAGE_BODY_CHARS: usize = 500;

/// `DEFAULT_RECONNECT_INITIAL_DELAY_MS = 1_000`。
pub const DEFAULT_RECONNECT_INITIAL_DELAY_MS: u64 = 1_000;

/// `DEFAULT_RECONNECT_MAX_DELAY_MS = 30_000`。
pub const DEFAULT_RECONNECT_MAX_DELAY_MS: u64 = 30_000;

/// `DEFAULT_RECONNECT_MAX_RETRIES = 5`。
pub const DEFAULT_RECONNECT_MAX_RETRIES: u32 = 5;

/// `McpHttpError`：带 HTTP 状态与响应体摘录的错误（信封 [`McpTransportError`] 的别名）。
///
/// TS 的类在骨架里统一成 [`McpTransportError`]（`name() == "McpHttpError"`、`status()`/`body()`
/// 返回字段）。保留别名是为了让 `index.rs` 的再导出名与 §19.2 保持一致。
///
/// 三个别名指向同一个 Rust 类型，类型系统不区分它们；判别靠 `name()` 或
/// [`McpTransportError::is_auth_required`] / [`McpTransportError::is_session_expired`]。
pub type McpHttpError = McpTransportError;

/// `McpAuthRequiredError`：401（`name() == "McpAuthRequiredError"`、`status()` 为 401、
/// `www_authenticate()` 来自响应头）。
pub type McpAuthRequiredError = McpTransportError;

/// `McpSessionExpiredError`：已建立会话的 404（`name() == "McpSessionExpiredError"`、`status()` 为 404）。
pub type McpSessionExpiredError = McpTransportError;

/// `SseEvent`：一个已派发的事件，只含存在的字段（空串的 `event`/`id` 被省略）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseEvent {
    /// `event:` 字段（空串被省略）。
    pub event: Option<String>,
    /// `data:` 行以 `\n` 连接。
    pub data: String,
    /// `id:` 字段（空串被省略）。
    pub id: Option<String>,
}

/// `onEvent(event)`（返回 `Err` 时中断解析并向上传播，与 TS 的抛出等价）。
///
/// 回调在解析器内部**同步**调用（不 await）；`Err` 会让 [`consume_sse_stream`] 立即返回，
/// 同一 chunk 里尚未处理的行与剩余字节都不再解析。
pub type SseEventHandler = Arc<dyn Fn(SseEvent) -> Result<(), McpTransportError> + Send + Sync>;

/// `onId?(id)`。
///
/// 每个通过校验（值不含 NUL）的 `id:` 字段都会调用它，**早于**该事件派发；没有 `data`
/// 的事件也会调用（这正是恢复点的来源）。回调没有错误通道，无法中断解析。
pub type SseIdHandler = Arc<dyn Fn(&str) + Send + Sync>;

/// `onRetry?(delayMs)`（可能是 `Infinity`，见 `node_timeout_duration`）。
///
/// 只在值匹配 `/^\d+$/` 时调用，单位毫秒；超长数字串得到 `f64::INFINITY`。与 `id` 一样，
/// 回调在解析该行时立即触发，早于事件派发；它没有错误通道。
pub type SseRetryHandler = Arc<dyn Fn(f64) + Send + Sync>;

/// `consumeSse` 的 `onMessage?(message)`。
type SseMessageHandler = Arc<dyn Fn(&Value) + Send + Sync>;

/// `ConsumeSseOptions`。
///
/// [`consume_sse_stream`] 的全部配置。三个回调都是 `Arc<dyn Fn …>`：解析器在异步读循环里
/// 持有它们直到流结束（EOF、超限或取消），因此必须是 `Send + Sync`。`on_event` 可以失败并
/// 终止解析；`on_id`/`on_retry` 没有错误通道。未给出 `max_event_bytes` 时使用
/// [`DEFAULT_MAX_MESSAGE_BYTES`]（16 MiB）。
#[derive(Default)]
pub struct ConsumeSseOptions {
    /// `maxEventBytes ?? 16 MiB`。
    pub max_event_bytes: Option<usize>,
    /// 每个已派发事件调用一次。
    pub on_event: Option<SseEventHandler>,
    /// 每个 `id` 字段都会调用（包括没有 data 的事件，用于预备恢复）。
    pub on_id: Option<SseIdHandler>,
    /// 每个合法 `retry` 字段，单位毫秒。
    pub on_retry: Option<SseRetryHandler>,
    /// 取消：Node 里 `AbortSignal` 会中止响应体读取，因此在读循环里 select 它。
    ///
    /// 被取消时 [`consume_sse_stream`] 返回 `Ok(())` 且**不**派发 `on_event`（剩下的数据已不可读），
    /// 与 `fetch` 抛 `AbortError` 后被上层当作流结束的行为一致。
    pub cancel: Option<CancellationToken>,
}

impl ConsumeSseOptions {
    /// 只有 `on_event` 的最小配置。
    pub fn new<F>(on_event: F) -> Self
    where
        F: Fn(SseEvent) -> Result<(), McpTransportError> + Send + Sync + 'static,
    {
        ConsumeSseOptions {
            on_event: Some(Arc::new(on_event)),
            ..ConsumeSseOptions::default()
        }
    }
}

impl std::fmt::Debug for ConsumeSseOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsumeSseOptions")
            .field("max_event_bytes", &self.max_event_bytes)
            .field("on_event", &self.on_event.is_some())
            .field("on_id", &self.on_id.is_some())
            .field("on_retry", &self.on_retry.is_some())
            .field("cancel", &self.cancel.is_some())
            .finish()
    }
}

/// `StreamableHttpReconnectOptions`。
///
/// 只影响**断流之后**的重连，首个 POST/GET 请求不受影响。GET 流与带事件 id 的响应流共用
/// 这组参数；`max_retries` 是单条流在放弃前的连续失败上限。重置条件并不只是"收到事件"：
/// `run_get_stream` 只在流**正常结束**且收到过事件（或存活超过 `max_delay()` 毫秒）时把
/// `attempt` 归零，读错误结束不重置；`consume_response_stream` 在上一轮收到过事件时归零
/// （见两者的文档）。服务器在 SSE 流里给过 `retry:` 时，下一次延迟直接取该值，
/// `initial_delay_ms`/`max_delay_ms` 的指数退避不参与（见 `reconnect_delay`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamableHttpReconnectOptions {
    /// 首次重连前的延迟（服务器给过 `retry` 时用它）。默认 1000。
    pub initial_delay_ms: Option<u64>,
    /// 指数退避的上界。默认 30000。
    pub max_delay_ms: Option<u64>,
    /// 一条流在放弃前的连续失败次数。默认 5。
    pub max_retries: Option<u32>,
}

/// `StreamableHttpTransportOptions`。
///
/// 构造时整体移入 transport，之后可经 [`StreamableHttpTransport::options`] 读回。只有 `url`
/// 必填（`new` 用 `Url::parse` 解析它，失败报 `TypeError: Invalid URL`）。缺省语义：
/// `fetch` → 全局共享的 [`crate::auth_provider::ReqwestFetch`]；`open_get_stream` 只有显式
/// `Some(false)` 才关闭（`None` 等同开启）；`max_message_bytes` → 16 MiB（SSE 事件上限，
/// 不影响 JSON 响应体）；`auth_provider` → 不发 `Authorization`、401 不重试；`reconnect`
/// → 三个 `DEFAULT_RECONNECT_*` 常量。
#[derive(Clone, Default)]
pub struct StreamableHttpTransportOptions {
    /// `url: string | URL`（`new URL(options.url)` 的等价物由构造函数完成）。
    pub url: String,
    /// 每个请求都带的头（按 WHATWG `Headers` 语义规范化）。
    pub headers: Option<Vec<(String, String)>>,
    /// `fetch?: McpFetch`；`None` → [`crate::auth_provider::ReqwestFetch`]。
    pub fetch: Option<Arc<dyn McpFetch>>,
    /// 初始化后打开服务端→客户端的 GET 流。默认 `true`。
    pub open_get_stream: Option<bool>,
    /// SSE 事件的字节上限（默认 16 MiB）。
    pub max_message_bytes: Option<usize>,
    /// 提供 bearer token 并在 401 后刷新。
    pub auth_provider: Option<Arc<dyn AuthProvider>>,
    /// 断掉的 SSE 流（GET 流与带事件 id 的响应流）的重连参数。
    pub reconnect: Option<StreamableHttpReconnectOptions>,
}

impl std::fmt::Debug for StreamableHttpTransportOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamableHttpTransportOptions")
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("fetch", &self.fetch.is_some())
            .field("open_get_stream", &self.open_get_stream)
            .field("max_message_bytes", &self.max_message_bytes)
            .field("auth_provider", &self.auth_provider.is_some())
            .field("reconnect", &self.reconnect)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// 流式 UTF-8 解码（`new TextDecoder()`）
// ---------------------------------------------------------------------------

/// `TextDecoder` 的流式形态：跨 chunk 拆分的多字节序列要保留到下一个 chunk。
///
/// `new TextDecoder()` 的默认参数是 `{ fatal: false, ignoreBOM: false }`：非法字节替换为
/// U+FFFD，**流首**的 U+FEFF 被当作 BOM 丢掉。
///
/// 这里复刻的是 Node 的实际实现（`internal/encoding` 的 UTF-8 fast path +
/// `encoding_binding::DecodeUTF8`），不是"教科书式"的增量解码，因为两者在 BOM 上不同：
/// Node 在**每个已产出输出的 decode 调用之后**就置 `bomSeen`（`if (stream) this[kBOMSeen] = true`），
/// 而 `DecodeUTF8` 只在**它自己收到的字节**以 BOM 开头时才剥。于是有一个可观测的差异：
/// 当一个未完成的多字节序列在 chunk 边界处补齐、同一 chunk 的后续字节恰好以 BOM 开头时，
/// Node 把这个 BOM **丢掉**（`[[0xC3], [0xA9, BOM]]` → `"é"`），而"只在流首剥一个 BOM"的
/// 朴素实现会留下它（→ `"é\u{feff}"`）。BOM 会进入解析器的 `buffered`，因此会改变
/// `Buffer.byteLength(buffered) > maxEventBytes` 的判定，必须在字节数上完全一致。
///
/// 复刻的结构与 Node 一一对应：
/// - `pending` 字段 ↔ `kChunk`（上一调用留下的未完成字节）；
/// - [`Self::bom_seen`] ↔ `kBOMSeen`（流首已过）；`ignoreBOM` 恒为 `false`（默认参数）；
/// - 每次 `decode` 先用 [`merge_prefix_utf8`] 把 `chunk` 与本次输入的前几个字节拼成一个
///   完整前缀单独解码，再解码剩下的部分（第二部分总按 `ignoreBom = bom_seen || false` 解码。
///   **注意** Node 源码写的是 `decodeUTF8(u, ignoreBom || prefix, …)`，而 binding 用
///   `args[1]->IsTrue()` 取值——`prefix` 是 `Uint8Array`，`IsTrue()` 为 `false`，因此
///   BOM 处理只由 `ignoreBom` 决定；用 `--expose-internals` 直接调 binding 已核实）。
/// - `decode()`（EOF、无 `stream`）会先清 `bom_seen`，然后只解码残留字节。
///
/// 随机对拍（`tests/fixtures/utf8_vectors.json` 之外，另用 8,000 组随机 chunk 拆分 +
/// 全部 BOM/多字节组合在 node 26 上与 `new TextDecoder()` 逐字符比对）已通过。
struct Utf8Decoder {
    /// 上一 chunk 末尾未完成的多字节序列（Node 的 `kChunk`）。
    pending: Vec<u8>,
    /// 流首是否已经过去（Node 的 `kBOMSeen`）：一旦为真，后续 BOM 一律保留。
    bom_seen: bool,
}

impl Utf8Decoder {
    fn new() -> Self {
        Utf8Decoder {
            pending: Vec::new(),
            bom_seen: false,
        }
    }

    /// `decoder.decode(chunk, { stream: true })`。
    fn decode(&mut self, chunk: &[u8]) -> String {
        self.decode_impl(chunk, true)
    }

    /// `decoder.decode()`（EOF）。
    fn finish(&mut self) -> String {
        self.decode_impl(&[], false)
    }

    /// Node `TextDecoder#decode` 的 UTF-8 fast path，逐行对应（`kIgnoreBOM` 恒为 `false`）。
    fn decode_impl(&mut self, input: &[u8], stream: bool) -> String {
        // `const ignoreBom = this[kIgnoreBOM] || this[kBOMSeen];` —— 在重置**之前**取值。
        let ignore_bom = self.bom_seen;
        if !stream {
            // `this[kBOMSeen] = false; if (!chunk) return decodeUTF8(input, ignoreBom, …)`
            self.bom_seen = false;
            if self.pending.is_empty() {
                return maybe_strip_bom(utf8_lossy(input), ignore_bom);
            }
        }
        let mut u = input.to_vec();
        if u.is_empty() && stream {
            // `if (u.length === 0 && stream) return ''; // no state change`
            return String::new();
        }
        let mut prefix: Option<Vec<u8>> = None;
        if !self.pending.is_empty() {
            let merged = merge_prefix_utf8(&u, &self.pending);
            if u.len() < 3 {
                // `u = merged; // Might be unfinished, but fully consumed old u`
                u = merged;
            } else {
                let add = merged.len() - self.pending.len();
                prefix = Some(merged);
                if add > 0 {
                    u.drain(..add);
                }
            }
            self.pending.clear();
        }
        if stream {
            let trail = unfinished_bytes_utf8(&u);
            if trail > 0 {
                self.pending = u[u.len() - trail..].to_vec();
                if prefix.is_none() && trail == u.len() {
                    // `if (!prefix && trail === u.length) return ''; // No further state change`
                    return String::new();
                }
                u.truncate(u.len() - trail);
            }
        }
        let mut out = String::new();
        if let Some(prefix) = &prefix {
            out.push_str(&maybe_strip_bom(utf8_lossy(prefix), ignore_bom));
        }
        out.push_str(&maybe_strip_bom(utf8_lossy(&u), ignore_bom));
        if stream {
            self.bom_seen = true;
        }
        out
    }
}

/// `StringBytes::Encode(…, UTF8)`：非法序列按 WHATWG 的 replacement 规则替换。
///
/// 直接用 `String::from_utf8_lossy`——两者对非法序列的替换个数一致（每个"最大非法子部分"
/// 一个 U+FFFD；`[0xF0,0x80,0x80]` → 三个 U+FFFD，`[0xE0,0x80]` → 两个）。
fn utf8_lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `encoding_binding::DecodeUTF8(bytes, ignore_bom, fatal)` 的 BOM 部分。
fn maybe_strip_bom(text: String, ignore_bom: bool) -> String {
    if ignore_bom {
        return text;
    }
    match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_owned(),
        None => text,
    }
}

/// `internal/encoding/util` 的 `unfinishedBytesUtf8(data, len)`：末尾有几个字节可能是
/// 一个未完成的多字节序列（0–3）。
///
/// 逐字移植（`@exodus/bytes`）：先回退最多 2 个续字节找前导字节，再按前导字节的合法
/// 续字节区间判断。注意 `0xC0`/`0xC1`/`0xF5..=0xFF` 都被判为 0（不是前导字节）。
fn unfinished_bytes_utf8(data: &[u8]) -> usize {
    let len = data.len();
    let mut pos = 0usize;
    while pos < 2 && pos < len && (data[len - pos - 1] & 0xc0) == 0x80 {
        pos += 1;
    }
    if pos == len {
        return 0;
    }
    let lead = data[len - pos - 1];
    if !(0xc2..=0xf4).contains(&lead) {
        return 0;
    }
    if pos == 0 {
        return 1;
    }
    if lead < 0xe0 || (lead < 0xf0 && pos >= 2) {
        return 0;
    }
    let lower = match lead {
        0xf0 => 0x90,
        0xe0 => 0xa0,
        _ => 0x80,
    };
    let upper = match lead {
        0xf4 => 0x8f,
        0xed => 0x9f,
        _ => 0xbf,
    };
    let next = data[len - pos];
    usize::from(next >= lower && next <= upper) * (pos + 1)
}

/// `internal/encoding/util` 的 `mergePrefixUtf8(data, chunk)`。
///
/// 调用时 `data` 是本次输入、`chunk` 是上一 chunk 留下的未完成字节。`data` 不足 3 字节时
/// 直接返回 `chunk + data`；否则返回 `chunk` 加上 `data` 里**使合并序列可判定（落在码点
/// 边界）的最短前缀**。`data` 的剩余部分整体留给第二次解码，它不一定是"未完成序列"——可能是
/// 完整文本甚至一个完整 BOM；这些字节在第二次解码里的流首位置决定 BOM 是否被剥掉（见
/// `Utf8Decoder` 的文档）。
///
/// 循环变量 `i` 是试探加入的 `data` 字节数，`unfinished` 是拼出的前缀末尾仍未完成的字节数，
/// 返回长度是 `chunk.len() + (i - unfinished)`。`data.len() >= 3` 时 `i` 至多 3：UTF-8
/// 序列最长 4 字节，而 `chunk` 非空时至少已有 1 字节。
fn merge_prefix_utf8(data: &[u8], chunk: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return chunk.to_vec();
    }
    if data.len() < 3 {
        let mut merged = Vec::with_capacity(data.len() + chunk.len());
        merged.extend_from_slice(chunk);
        merged.extend_from_slice(data);
        return merged;
    }
    let mut temp = Vec::with_capacity(chunk.len() + 3);
    temp.extend_from_slice(chunk);
    temp.extend_from_slice(&data[..3]);
    for i in 1..=3usize {
        let unfinished = unfinished_bytes_utf8(&temp[..chunk.len() + i]);
        if unfinished <= i {
            let add = i - unfinished;
            return if add > 0 {
                temp[..chunk.len() + add].to_vec()
            } else {
                chunk.to_vec()
            };
        }
    }
    // 与 Node 一样不可达（i = 3 时 unfinished 至多 3）。
    temp
}

// ---------------------------------------------------------------------------
// consumeSseStream
// ---------------------------------------------------------------------------

/// `consumeSseStream(stream, options)`：解析 WHATWG SSE 子集（§17.1）。
///
/// 必须遵守的规则：
/// - 每行剥掉**一个**尾随 `\r`；空行派发；`:` 开头是注释；`field: value` 里冒号后的一个空格被剥掉；
///   没有冒号时整行是字段名、值为空串。
/// - `data` 行以 `\n` 连接；`dataBytes` 只累计 `data` 值（多行时每行再加 1 个 `\n`），
///   逐行检查超限，且每个 chunk 处理完完整行后再检查一次未换行的缓冲。
/// - `id` 含 NUL 时忽略；`retry` 只接受 `/^\d+$/`（`+5`、`5.0`、` 5`、`-5` 都不接受，`007` 接受）。
/// - `dispatch()` 清空 event/id；空串的 event/id 不出现在事件里。
/// - EOF 时先冲刷解码器、处理残留缓冲、最后再 `dispatch()`。
///
/// 错误：超限时 `MCP SSE event exceeds {max} bytes`；`on_event` 的错误原样向上传播。
///
/// 输入是字节块流；`Ok(())` 表示读到 EOF 或 `cancel` 被触发，`Err` 来自底层流、超限或
/// `on_event` 的返回值。解析状态（解码器、未成行的缓冲、当前事件字段）都封装在 `SseParser`
/// 里，本函数只负责"读一块 → 解码 → 切行 → 检查上限"的顺序。
pub async fn consume_sse_stream(
    mut stream: crate::auth_provider::FetchByteStream,
    options: ConsumeSseOptions,
) -> Result<(), McpTransportError> {
    use futures_core::Stream;

    let mut parser = SseParser::new(options);
    loop {
        let next = std::future::poll_fn(|cx| std::pin::Pin::new(&mut stream).poll_next(cx));
        let item = match &parser.cancel {
            Some(cancel) => {
                // 取消优先：`close()` 之后立即结束读循环，与 Node 里流被 abort 后读到 EOF 等价。
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Ok(()),
                    item = next => item,
                }
            }
            None => next.await,
        };
        let Some(chunk) = item else { break };
        // 读错误（连接在体中途断开）：TS 的 `reader.read()` reject，这里同样向上传播，
        // 由重连状态机的 `isRetryable` 分类（`reqwest` 的体错误被映射成 `TypeError`）。
        let chunk = chunk?;
        // 解码是流式的：跨 chunk 的多字节序列留在 parser 的解码器里，不在这里拼接。
        parser.push_decoded(&chunk);
        // 先处理所有完整行（`data` 行在这里逐行检查超限）。
        parser.drain_lines()?;
        // 再检查没有任何换行的残留缓冲：TS 在每次 read 之后做同样的 `byteLength` 检查。
        if parser.buffered_bytes() > parser.max_event_bytes {
            return Err(parser.exceeds());
        }
    }
    parser.finish()?;
    Ok(())
}

/// `consumeSseStream` 的状态机（把 TS 的闭包拆成方法，语义一一对应）。
///
/// 一段输入的固定处理顺序：解码（跨 chunk 的字节挂在 `decoder` 里）→ 追加到 `buffered`
/// → 按 `\n` 切行逐条 `process_line` → 每次 chunk 结束检查剩余缓冲的字节数。`dispatch`
/// 是唯一重置事件状态的地方：`event_name`/`event_id`/`data_lines`/`data_bytes` 在派发后
/// 或空行无 data 时清空；`buffered` 只在行被消费时前移（EOF 时整段作为最后一行处理）。
struct SseParser {
    /// 实际使用的上限（选项缺省时是 16 MiB）。
    max_event_bytes: usize,
    on_event: Option<SseEventHandler>,
    on_id: Option<SseIdHandler>,
    on_retry: Option<SseRetryHandler>,
    cancel: Option<CancellationToken>,
    /// 跨 chunk 的流式 UTF-8 解码器（见 `Utf8Decoder`）。
    decoder: Utf8Decoder,
    /// 已解码但尚未按行切分的文本；完整行处理后立即移除。
    buffered: String,
    /// 当前事件的 `event:` 值（`dispatch` 清空）。
    event_name: Option<String>,
    /// 当前事件的 `id:` 值（`dispatch` 清空；含 NUL 的 id 被忽略）。
    event_id: Option<String>,
    /// 当前事件的 `data:` 值集合，派发时以 `\n` 连接。
    data_lines: Vec<String>,
    /// 当前事件已累计的 data 字节数（含多行之间的 `\n`），用于逐行超限检查。
    data_bytes: usize,
}

impl SseParser {
    fn new(options: ConsumeSseOptions) -> Self {
        SseParser {
            max_event_bytes: options.max_event_bytes.unwrap_or(DEFAULT_MAX_MESSAGE_BYTES),
            on_event: options.on_event,
            on_id: options.on_id,
            on_retry: options.on_retry,
            cancel: options.cancel,
            decoder: Utf8Decoder::new(),
            buffered: String::new(),
            event_name: None,
            event_id: None,
            data_lines: Vec::new(),
            data_bytes: 0,
        }
    }

    /// 超限错误（`MCP SSE event exceeds {max} bytes`）。
    fn exceeds(&self) -> McpTransportError {
        McpTransportError::new(format!(
            "MCP SSE event exceeds {} bytes",
            self.max_event_bytes
        ))
    }

    /// `buffered += decoder.decode(value, { stream: true })`。
    fn push_decoded(&mut self, chunk: &[u8]) {
        let text = self.decoder.decode(chunk);
        self.buffered.push_str(&text);
    }

    /// `Buffer.byteLength(buffered)`（UTF-8 字节数）。
    fn buffered_bytes(&self) -> usize {
        self.buffered.len()
    }

    /// 处理所有完整行（`while (newline >= 0)`）。
    fn drain_lines(&mut self) -> Result<(), McpTransportError> {
        while let Some(newline) = self.buffered.find('\n') {
            let line: String = self.buffered[..newline].to_owned();
            self.buffered.drain(..newline + 1);
            self.process_line(&line)?;
        }
        Ok(())
    }

    /// EOF：`buffered += decoder.decode()`；若有残留则处理它，最后 `dispatch()`。
    fn finish(&mut self) -> Result<(), McpTransportError> {
        let tail = self.decoder.finish();
        self.buffered.push_str(&tail);
        if !self.buffered.is_empty() {
            let line = std::mem::take(&mut self.buffered);
            self.process_line(&line)?;
        }
        self.dispatch()
    }

    /// `processLine(rawLine)`。
    ///
    /// 字段名区分大小写：只有小写的 `data`/`event`/`id`/`retry` 生效，未知字段与 `:` 注释行
    /// 都被忽略（不报错）。
    fn process_line(&mut self, raw_line: &str) -> Result<(), McpTransportError> {
        // 每行最多剥掉**一个**尾随 `\r`。
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, mut value) = match line.find(':') {
            Some(colon) => (&line[..colon], &line[colon + 1..]),
            None => (line, ""),
        };
        if let Some(rest) = value.strip_prefix(' ') {
            value = rest;
        }
        match field {
            "data" => {
                // `Buffer.byteLength(value) + (dataLines.length > 0 ? 1 : 0)`
                self.data_bytes += value.len() + usize::from(!self.data_lines.is_empty());
                if self.data_bytes > self.max_event_bytes {
                    return Err(self.exceeds());
                }
                self.data_lines.push(value.to_owned());
            }
            "event" => self.event_name = Some(value.to_owned()),
            "id" => {
                if !value.contains('\0') {
                    self.event_id = Some(value.to_owned());
                    if let Some(on_id) = &self.on_id {
                        on_id(value);
                    }
                }
            }
            "retry" => {
                if let (Some(delay), Some(on_retry)) = (parse_sse_retry(value), &self.on_retry) {
                    on_retry(delay);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// `dispatch()`：没有 data 行时只清 event/id；否则派发并重置全部状态。
    fn dispatch(&mut self) -> Result<(), McpTransportError> {
        if self.data_lines.is_empty() {
            self.event_name = None;
            self.event_id = None;
            return Ok(());
        }
        let event = SseEvent {
            // 空字符串的 event/id 被省略（TS 的 `eventName ? {...} : {}`）。
            event: self.event_name.take().filter(|name| !name.is_empty()),
            data: self.data_lines.join("\n"),
            id: self.event_id.take().filter(|id| !id.is_empty()),
        };
        self.event_name = None;
        self.event_id = None;
        self.data_lines.clear();
        self.data_bytes = 0;
        match &self.on_event {
            Some(on_event) => on_event(event),
            None => Ok(()),
        }
    }
}

/// `/^\d+$/` + `Number(value)`。
///
/// 不能用 `str::parse::<u64>()`：它接受前导 `+`（`"+5"` → `Ok(5)`），而 JS 的正则不接受
/// （§30.28，最容易漏的一处）。超长数字串在 JS 里是 `Infinity`——这里同样返回 `f64::INFINITY`
/// （延迟会被 [`node_timeout_duration`] 钳到 1 ms）。
fn parse_sse_retry(value: &str) -> Option<f64> {
    if value.is_empty() {
        return None;
    }
    // 只接受 ASCII 数字；JS 的 `\d` 是 ASCII-only（不是 Unicode 数字）。
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    // `Number("0012")` = 12；溢出时是 `Infinity`（`f64` 的解析也给出 `inf`）。
    Some(value.parse::<f64>().unwrap_or(f64::INFINITY))
}

// ---------------------------------------------------------------------------
// 传输
// ---------------------------------------------------------------------------

/// `StreamableHttpTransport`：MCP Streamable HTTP 传输（§17.3–§17.12）。
///
/// 内部状态放在 [`Arc`] 里：SSE 消费与 GET 流重连要在后台任务里继续运行（TS 的
/// `void this.consumeResponseStream(...)` / `void this.runGetStream()`），因此后台任务持有
/// 同一个 `Arc`，`close()` 通过 transport 令牌让它们退出。
pub struct StreamableHttpTransport {
    inner: Arc<TransportInner>,
}

impl std::fmt::Debug for StreamableHttpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamableHttpTransport")
            .field("url", &self.inner.url.as_str())
            .field("session_id", &self.session_id())
            .field("started", &self.inner.started.load(Ordering::SeqCst))
            .field("closed", &self.inner.is_closed())
            .finish()
    }
}

struct TransportInner {
    /// 监听器表：`McpTransport` 的 `on_*` 委托给它，后台任务也通过它派发消息/错误。
    events: TransportEvents,
    /// `new()` 解析后的 URL；所有请求（含 `close()` 的 DELETE）都发往这里。
    url: Url,
    /// 构造时传入的选项。`options.url` 保留原字符串，运行期只用上面解析后的 `url`。
    options: StreamableHttpTransportOptions,
    /// 解析后的 `fetch`：注入的实现，或全局共享的缺省实现。
    fetch: Arc<dyn McpFetch>,
    /// `controller`：`close()` 时中止 `authorized_fetch` 在途的请求（TS 的 `AbortController`）。
    /// `close()` 自己的 DELETE 不用它——那个请求只带 1 秒超时。
    controller: CancellationToken,
    /// `start()` 是否已调用；`send()` 要求它为 `true` 且 `closed` 为 `false`。
    started: AtomicBool,
    /// `close()` 是否已调用：`send()` 拒绝、后台循环退出、`close()` 幂等。
    closed: AtomicBool,
    /// 最近一次响应里**非空**的 `Mcp-Session-Id`；请求头与 404 的过期判定都读它。
    session_id: Mutex<Option<String>>,
    /// `set_protocol_version` 写入的值；空串视作未设置（见 `build_headers`）。
    protocol_version: Mutex<Option<String>>,
    /// GET 流是否已启动过（`start_get_stream` 的 CAS 闸门）。
    get_stream_started: AtomicBool,
    /// 最近一次请求的 token：`close()` 复用它，而不是再问认证提供方（§17.5 / #10565）。
    last_token: Mutex<Option<String>>,
}

impl StreamableHttpTransport {
    /// 构造：`new URL(options.url)`；`fetch` 缺省用共享的 [`crate::auth_provider::ReqwestFetch`]。
    ///
    /// TS 的构造函数不校验 URL 之外的东西；这里同样只解析 URL。失败时 Node 抛
    /// `TypeError("Invalid URL")`（消息就是这两个词，不带底层细节，已在 Node 26 上实测），
    /// 这里用 `McpTransportError::network` 给出同样的 `name` 与消息。
    pub fn new(options: StreamableHttpTransportOptions) -> Result<Self, McpTransportError> {
        let url =
            Url::parse(&options.url).map_err(|_| McpTransportError::network("Invalid URL"))?;
        let fetch = match &options.fetch {
            Some(fetch) => Arc::clone(fetch),
            None => default_fetch(),
        };
        Ok(StreamableHttpTransport {
            inner: Arc::new(TransportInner {
                events: TransportEvents::new(),
                url,
                options,
                fetch,
                controller: CancellationToken::new(),
                started: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                session_id: Mutex::new(None),
                protocol_version: Mutex::new(None),
                get_stream_started: AtomicBool::new(false),
                last_token: Mutex::new(None),
            }),
        })
    }

    /// `get sessionId()`：最近一次响应捕获的 `Mcp-Session-Id`。
    pub fn session_id(&self) -> Option<String> {
        self.inner.session_id.lock().ok().and_then(|id| id.clone())
    }

    /// `this.url`。
    pub fn url(&self) -> &Url {
        &self.inner.url
    }

    /// `this.options`（冻结的浅拷贝的等价物）。
    pub fn options(&self) -> &StreamableHttpTransportOptions {
        &self.inner.options
    }

    /// 共享的 `TransportEvents`（注册监听器的入口；[`McpTransport`] 的同名方法委托给它）。
    pub fn events(&self) -> &TransportEvents {
        &self.inner.events
    }
}

/// 缺省 `fetch`：与 TS 的 `globalThis.fetch` 一样由所有 transport 共享。
fn default_fetch() -> Arc<dyn McpFetch> {
    static DEFAULT: std::sync::OnceLock<Arc<dyn McpFetch>> = std::sync::OnceLock::new();
    Arc::clone(DEFAULT.get_or_init(|| Arc::new(crate::auth_provider::ReqwestFetch::new())))
}

/// `StreamCursor`：一条 SSE 流的恢复状态。
///
/// 响应流按请求持有一个，GET 流按 transport 持有一个；`on_event`/`on_id`/`on_retry` 回调与
/// 重连循环都要读写它，因此外面套了 `Arc<Mutex<…>>`。锁只包住单次字段读写：回调是同步
/// 函数，循环在 await（`sleep`/`open_sse_stream`）之前就释放了锁。
#[derive(Default)]
struct StreamCursor {
    last_event_id: Option<String>,
    /// 服务器给过的 `retry`（毫秒；可能是 `Infinity`）。
    retry_ms: Option<f64>,
    /// 自（重新）打开以来是否投递过事件。
    received: bool,
}

/// `new Error("MCP Streamable HTTP transport already started")` 等的共用构造。
fn error(message: impl Into<String>) -> McpTransportError {
    McpTransportError::new(message)
}

/// `new McpConnectionClosedError()`。
fn connection_closed() -> McpTransportError {
    McpTransportError::from(crate::jsonrpc::McpError::connection_closed())
}

/// `new McpAbortError()`（`name === "AbortError"`，消息 `MCP request aborted`）。
fn aborted() -> McpTransportError {
    McpTransportError::from(crate::jsonrpc::McpError::aborted())
}

#[async_trait::async_trait]
impl McpTransport for StreamableHttpTransport {
    /// `start()`：只置 `started`，不发起网络请求。重复调用，以及**已经 start 过、随后
    /// close** 的实例上的调用，都返回 `MCP Streamable HTTP transport already started`
    /// （先查 `started` 再查 `closed`）。从未 start 过就先 `close()` 的实例则返回
    /// `McpConnectionClosedError`（消息 `MCP connection closed`），因为此时 `started`
    /// 仍为 `false`。
    async fn start(&self) -> Result<(), McpTransportError> {
        let inner = &self.inner;
        if inner.started.load(Ordering::SeqCst) {
            return Err(error("MCP Streamable HTTP transport already started"));
        }
        if inner.closed.load(Ordering::SeqCst) {
            return Err(connection_closed());
        }
        inner.started.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn send(
        &self,
        message: Value,
        options: McpTransportSendOptions,
    ) -> Result<(), McpTransportError> {
        self.inner.send(message, options.signal).await
    }

    async fn close(&self) -> Result<(), McpTransportError> {
        self.inner.close().await
    }

    fn on_message(&self, listener: McpTransportMessageListener) -> Subscription {
        self.inner
            .events
            .on_message(move |message| listener(message))
    }

    fn on_error(&self, listener: McpTransportErrorListener) -> Subscription {
        self.inner.events.on_error(move |error| listener(error))
    }

    fn on_close(&self, listener: McpTransportCloseListener) -> Subscription {
        self.inner.events.on_close(move || listener())
    }

    /// `setProtocolVersion(version)`：记录版本，后续请求带 `MCP-Protocol-Version`；空串等价于
    /// 未设置（`build_headers` 按 JS 的假值规则跳过）。
    fn set_protocol_version(&self, version: &str) {
        if let Ok(mut slot) = self.inner.protocol_version.lock() {
            *slot = Some(version.to_owned());
        }
    }
}

// ---------------------------------------------------------------------------
// 辅助函数（§17.2）
// ---------------------------------------------------------------------------

/// 401，或带 `insufficient_scope` bearer challenge 的 403（step-up 授权）。
///
/// 正则逐字：`/(?:^|[\s,])error="?insufficient_scope"?/i`（§17.2、§27 的实测向量）：
/// `xerror="insufficient_scope"` 不匹配；`Bearer,error="insufficient_scope"` 匹配；引号可选；
/// 无尾边界（`error=insufficient_scopeX` 匹配）；`=` 旁有空格不匹配。
///
/// `\s` 展开成 **JS 的**空白集合，而不是 Rust `regex` 的 `\s`（Unicode `White_Space`）：
/// 两者在两个方向上都不同——JS 含 U+FEFF、不含 U+0085（NEL）。头值经 latin1 解码，
/// 所以 U+0085 是可达的：服务器发一个裸 0x85 字节就得到 `"Bearer\u{85}error=…"`。
/// 用 Rust 的 `\s` 会在这里匹配（多一次授权往返），JS 不会（§17.2 的「实际头值不含 BOM」
/// 只覆盖了 U+FEFF 方向，反方向是可观测的）。
///
/// 展开后的字符类 = `\t \n \x0B \x0C \r 空格 U+00A0 U+1680 U+2000–U+200A U+2028 U+2029
/// U+202F U+205F U+3000 U+FEFF` 加逗号。
fn needs_authorization(response: &FetchResponse) -> bool {
    if response.status() == 401 {
        return true;
    }
    if response.status() != 403 {
        return false;
    }
    insufficient_scope_challenge(&response.header("www-authenticate").unwrap_or_default())
}

/// `needsAuthorization` 的正则部分（`www-authenticate` 的值；缺失视为 `""`）。
///
/// 单独拆出来是为了能用任意字符串测 `\s` 的集合——头值经 latin1 解码，`U+FEFF` 这种码位
/// 从网络字节走不到这里（`EF BB BF` 会变成三个 latin1 码位），但正则本身按 JS 的集合写。
fn insufficient_scope_challenge(challenge: &str) -> bool {
    static REGEX: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let regex = REGEX.get_or_init(|| {
        regex::Regex::new(
            r#"(?i)(?:^|[\x09\x0A\x0B\x0C\x0D \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF},])error="?insufficient_scope"?"#,
        )
        .expect("the pattern is a literal and compiles")
    });
    regex.is_match(challenge)
}

/// `response.body !== null`。
///
/// fetch 的规范把 101/103/204/205/304 定为「无体状态」，`Response.body` 对这些状态总是 `null`
/// （Node 26 实测：204/205/304 都是 `null`），与服务器是否真的发了字节无关；其余状态总有一个
/// （可能立刻结束的）流。TS 的两处判定都写作 `!response.body`（`send` 的 SSE 分支与
/// `openSseStream`），因此 [`FetchResponse::has_body`] 还要叠上这个状态集合。
fn response_has_body(response: &FetchResponse) -> bool {
    response.has_body() && !matches!(response.status(), 101 | 103 | 204 | 205 | 304)
}

/// `contentType(response)` = `headers.get("content-type")?.split(";", 1)[0]?.trim().toLowerCase()`。
///
/// 不能用 [`FetchResponse::content_type`]：那个辅助函数（在共享的 `auth_provider.rs` 里）用的是
/// Rust 的 `str::trim()` 与 `to_ascii_lowercase()`，而 JS 的 `trim()`/`toLowerCase()` 集合不同：
/// - `trim()`：JS 含 U+FEFF、**不含 U+0085（NEL）**；Rust `char::is_whitespace` 正好相反。
///   undici 把头字节按 latin1 解码，服务器发一个裸 0x85 字节，应用看到的是
///   `"text/event-stream\u{85}"`：JS 不 trim 它 → 与 `"text/event-stream"` 不等 → 抛
///   `Unsupported MCP response content type: text/event-stream`（Node 26 实测）；Rust 的
///   `str::trim()` 会 trim 掉 → 误判成 SSE。
/// - `toLowerCase()`：头值落在 latin1 域，JS 会把 `À`–`Þ`（`×` 除外）小写成 `à`–`þ`，
///   `to_ascii_lowercase()` 不会。JS 自身对这些字符的小写映射正好是 +0x20（已实测
///   `U+00C0..U+00DE` 全表），与 `A`–`Z` 的规则相同。
///
/// `split(";", 1)[0]` 的语义与 `str::split(';').next()` 一致（首个分号之前的部分，
/// 没有分号时整串）。
fn content_type(response: &FetchResponse) -> Option<String> {
    let raw = response.header("content-type")?;
    let first = raw.split(';').next().unwrap_or("");
    Some(js_latin1_lowercase(kiana_json::js_string::js_trim(first)))
}

/// `String.prototype.toLowerCase()` 在 **latin1 域**（U+0000..=U+00FF）的等价物。
///
/// 头值经 [`crate::auth_provider::header_joined`] 按 latin1 逐字节解码，只可能落在这一域内。
/// 该域内 JS 的小写映射只有两段：`A`–`Z` 与 `À`–`Þ`（`×` U+00D7 除外）；`µ`（U+00B5）、
/// `ß`（U+00DF）、`ÿ`（U+00FF）等都映射到自身（Node 26 实测）。
fn js_latin1_lowercase(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            'A'..='Z' => char::from(ch as u8 + 32),
            '\u{00C0}'..='\u{00DE}' if ch != '\u{00D7}' => char::from(ch as u32 as u8 + 32),
            other => other,
        })
        .collect()
}

/// `describeHttpFailure(status, body)`（摘要按 UTF-16 代码单元切成 497 + `...`）。
///
/// 先按 JS `String.prototype.trim()` 去空白；摘要为空时不加 `: ` 后缀。调用方传入的
/// `body` 已在 `check_response` 里被截到 [`MAX_ERROR_BODY_BYTES`] 个单元，这里的
/// [`ERROR_MESSAGE_BODY_CHARS`] 是第二级、更短的截断。
fn describe_http_failure(status: u16, body: &str) -> String {
    let text = kiana_json::js_string::js_trim(body);
    let snippet = if utf16_len(text) > ERROR_MESSAGE_BODY_CHARS {
        format!("{}...", utf16_prefix(text, ERROR_MESSAGE_BODY_CHARS - 3))
    } else {
        text.to_owned()
    };
    if snippet.is_empty() {
        format!("MCP HTTP request failed with status {status}")
    } else {
        format!("MCP HTTP request failed with status {status}: {snippet}")
    }
}

/// `text.length`（UTF-16 代码单元）。
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// JS `text.slice(0, max_units)`：按 UTF-16 代码单元截断。
///
/// 若截断点落在代理对中间，JS 会留下一个孤立代理项，编码成 UTF-8 时变成 U+FFFD，
/// 因此这里直接补 U+FFFD（`String` 装不下孤立代理项，见 §22 的说明）。
fn utf16_prefix(text: &str, max_units: usize) -> String {
    let mut used = 0usize;
    let mut out = String::new();
    for ch in text.chars() {
        let width = ch.len_utf16();
        if used + width > max_units {
            if used < max_units {
                out.push('\u{fffd}');
            }
            break;
        }
        used += width;
        out.push(ch);
    }
    out
}

/// Node `setTimeout(fn, delay)` 的延迟钳位：不在 `[1, 2^31-1]` 内（含 `NaN`/`Infinity`）时用 1 ms。
///
/// §30.28：`retry: <超长数字>` 在 JS 里是 `Infinity`，`setTimeout` 把它钳到 1 ms。
fn node_timeout_duration(delay_ms: f64) -> Duration {
    const TIMEOUT_MAX: f64 = 2_147_483_647.0;
    if !(1.0..=TIMEOUT_MAX).contains(&delay_ms) {
        return Duration::from_millis(1);
    }
    Duration::from_millis(delay_ms as u64)
}

// ---------------------------------------------------------------------------
// TransportInner：TS 类的主体
// ---------------------------------------------------------------------------

impl TransportInner {
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn session_id(&self) -> Option<String> {
        self.session_id.lock().ok().and_then(|id| id.clone())
    }

    /// `send(message, { signal })`（§17.4、§30.7）。
    ///
    /// 阶段：前置检查（`started` 且未 `closed`）→ 序列化并 POST → `check_response` 分类 →
    /// `capture_session` → 按消息种类与 `Content-Type` 分派。通知/响应分支丢弃体；请求分支在
    /// SSE 时 `tokio::spawn` 后台读取并立即返回，因此**答复不会出现在本次调用的返回值里**，
    /// 而是稍后经 `emit_message` 到达监听器。
    ///
    /// 返回 `Ok(())` 只表示消息已被接受（JSON 答复已派发完，或 SSE 已交给后台任务），**不**
    /// 表示调用方已经拿到 JSON-RPC 结果；网络/协议/分类错误都以 `Err` 返回。未 `start()` 或
    /// 已 `close()` 时返回 `McpConnectionClosedError`（消息 `MCP connection closed`）。
    async fn send(
        self: &Arc<Self>,
        message: Value,
        signal: Option<CancellationToken>,
    ) -> Result<(), McpTransportError> {
        let self_arc = Arc::clone(self);
        if !self.started.load(Ordering::SeqCst) || self.is_closed() {
            return Err(connection_closed());
        }
        let body = kiana_json::js_object::stringify(&message);
        let mut response = self
            .authorized_fetch(
                Method::POST,
                vec![
                    (
                        "accept".to_owned(),
                        "application/json, text/event-stream".to_owned(),
                    ),
                    ("content-type".to_owned(), "application/json".to_owned()),
                ],
                Some(body),
                signal.as_ref(),
            )
            .await?;
        self.check_response(&mut response).await?;
        // 必须在 check_response **之后**：404 的会话过期判定要用旧的 session id。
        self.capture_session(&response);

        if !is_json_rpc_request(&message) {
            // 通知与响应只被 202 确认，不带答复；忽略任何 body。
            response.discard().await;
            // 服务端→客户端的流只能在会话初始化之后打开。
            if message.get("method").and_then(Value::as_str) == Some("notifications/initialized") {
                self.start_get_stream();
            }
            return Ok(());
        }
        // 到这里响应一定是 2xx：`check_response` 已把其余状态变成错误。
        let status = response.status();
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if status == 202 || status == 204 {
            // 2xx 里的 202/204 按传输契约表示服务器不会给出答复：请求方若继续等待就永远不会
            // 返回，因此在这里失败。
            return Err(McpTransportError::http(
                status,
                format!("MCP server accepted request {method} without a response"),
                "",
            ));
        }
        let content_type = content_type(&response);
        if content_type.as_deref() == Some("application/json") {
            // TS 无条件 `await response.json()`：对 fetch 的**无体状态**（101/103/204/205/304）
            // `Response.text()` 是 `""`，于是 `JSON.parse("")` 抛
            // `SyntaxError: Unexpected end of JSON input`——205 + JSON 体在这里**不会**被解析，
            // 即使服务器真的发了字节（Node 26 实测：205 的 `Response.body` 是 `null`，
            // 服务端发来的 JSON 被丢弃）。200/204/304 的其它分支先被拦截，205 是唯一走得到的。
            let body = if response_has_body(&response) {
                response.json().await?
            } else {
                kiana_json::js_parse::parse("")
                    .map_err(|error| McpTransportError::syntax(error.message))?
            };
            let items = match body {
                Value::Array(items) => items,
                other => vec![other],
            };
            for item in &items {
                // TS 的 `parseJsonRpcMessage` 校验后返回**原对象**；`emitMessage` 把它原样交给监听器。
                parse_json_rpc_message_value(item)?;
                self.events.emit_message(item)?;
            }
            return Ok(());
        }
        // 只有类型与体同时满足才接管；任缺一项都落到函数末尾的统一错误
        // （例如 205 + text/event-stream：无体状态使 `response_has_body` 为 false）。
        if content_type.as_deref() == Some("text/event-stream")
            && response_has_body(&response)
            && let Some(stream) = response.take_stream()
        {
            // `is_json_rpc_request` 已保证 `id` 是合法 id，因此转换不会失败。
            let request_id = value_to_json_rpc_id(&message["id"])
                .expect("is_json_rpc_request guarantees a valid id");
            let inner = Arc::clone(&self_arc);
            // `void this.consumeResponseStream(...)`：立即返回，答复经 emitMessage 异步到达。
            tokio::spawn(async move {
                // TS 是未处理的 promise rejection（同样不会回到请求方）；这里丢弃监听器错误。
                let _ = inner.consume_response_stream(stream, request_id).await;
            });
            return Ok(());
        }
        response.discard().await;
        Err(McpTransportError::http(
            status,
            format!(
                "Unsupported MCP response content type: {}",
                content_type.as_deref().unwrap_or("missing")
            ),
            "",
        ))
    }

    /// `close()`（§17.5）：**不**问认证提供方（复用 `lastToken`），DELETE 1 秒超时且失败被忽略。
    ///
    /// 步骤：幂等检查（第二次调用直接返回，不重复 DELETE/`emit_close`）→ 取消令牌（在途
    /// fetch 与 SSE 读循环随之结束）→ 有会话时用旧 token 尽力发 DELETE → `emit_close`。
    /// DELETE 的任何失败都不会返回错误；但 `emit_close` 里监听器返回的错误会向上传播。
    async fn close(&self) -> Result<(), McpTransportError> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.controller.cancel();
        if self.started.load(Ordering::SeqCst) && self.session_id().is_some() {
            // 只读记录，不调用 provider.token()：刷新可能要走网络，close 不能等它。
            let token = self.last_token.lock().ok().and_then(|token| token.clone());
            if let Ok(headers) = self.build_headers(&[], token.as_deref()) {
                let init = FetchInit::with_method(Method::DELETE)
                    .with_headers(headers)
                    .with_timeout(Some(Duration::from_millis(1_000)));
                // 尽力而为：服务器上的会话本来也会过期，错误一律忽略。
                if let Ok(mut response) = self.fetch.fetch(&self.url, init).await {
                    response.discard().await;
                }
            }
        }
        // 无论 DELETE 是否发出、是否成功，都通知 close 监听器（emit_close 自身幂等）。
        self.events.emit_close()
    }

    /// `authorizedFetch(method, init, signal?)`（§17.6、§30.14）。
    ///
    /// `signal` 只阻止**派发**：拿 token 或恢复授权可能要等，期间被调用方放弃的请求不得再上线；
    /// 它不会中止共享的恢复。出站 fetch 用 transport 自己的令牌。
    ///
    /// 重试上限是一次：只有第一轮（`attempt == 0`）的 401 / `insufficient_scope` 403 会交给
    /// provider；provider 返回 `Err` 时直接抛给调用方。每轮都重新走 `headers()`，因为
    /// provider 可能在 `on_unauthorized` 里刷新了 token。
    ///
    /// 被拒响应的体在这里读入 `UnauthorizedContext`；读取失败退化为空 `Vec<u8>`
    /// （`FetchResponse::bytes` 的错误被 `unwrap_or_default` 吞掉），provider 因此看不到
    /// 流错误——这是与 TS 交付活 `Response` 的已登记偏差（见模块文档）。
    async fn authorized_fetch(
        &self,
        method: Method,
        extra_headers: Vec<(String, String)>,
        body: Option<String>,
        signal: Option<&CancellationToken>,
    ) -> Result<FetchResponse, McpTransportError> {
        // `const onUnauthorized = this.options.authProvider?.onUnauthorized?.bind(...)`：在循环**之前**
        // 决定一次。TS 用 `onUnauthorized === undefined` 区分「没实现」，Rust 的 `on_unauthorized`
        // 缺省是 no-op，因此用 `has_on_unauthorized()` 判别（不能只看 `is_some()`）。
        let provider = self
            .options
            .auth_provider
            .as_ref()
            .filter(|provider| provider.has_on_unauthorized());
        let mut attempt = 0usize;
        loop {
            let (headers, token) = self.headers(&extra_headers).await?;
            // 取 token 之后、发请求之前检查：已取消的请求不再上线；`signal` 不会中止已经发出
            // 的请求（那由 transport 令牌负责）。
            if signal.is_some_and(CancellationToken::is_cancelled) {
                return Err(aborted());
            }
            let mut init = FetchInit::with_method(method.clone())
                .with_headers(headers)
                .with_signal(Some(self.controller.clone()));
            if let Some(body) = &body {
                init = init.with_body(FetchBody::Text(body.clone()));
            }
            let mut response = self.fetch.fetch(&self.url, init).await?;
            // `!onUnauthorized` 的分支：没有认证提供方（或它没实现 `on_unauthorized`）时直接返回
            // （401 → checkResponse 分类），**不**重试。
            let Some(provider) = provider else {
                return Ok(response);
            };
            if attempt > 0 || !needs_authorization(&response) {
                return Ok(response);
            }
            // 把被拒响应**完整读入**后交给提供方（§12：Rust 侧的 `UnauthorizedContext` 带 body）。
            // 读失败不改变授权流程：provider 仍会被调用，只是看到空体。
            let body_bytes = response.bytes().await.unwrap_or_default();
            let context = UnauthorizedContext {
                status: response.status(),
                headers: response.headers().clone(),
                body: body_bytes,
                server_url: self.url.clone(),
                fetch: Arc::clone(&self.fetch),
                token: token.clone(),
            };
            response.discard().await;
            provider.on_unauthorized(context).await?;
            attempt += 1;
        }
    }

    /// `headers(extra)`：取 token（可能刷新）并记录 `lastToken`。
    ///
    /// 返回供本次请求使用的头与 token。副作用是更新 `last_token`（每次请求都写，含 `None`），
    /// 这是 `close()` 的 DELETE 唯一的 token 来源。并发请求各自写这个槽，最后写入者胜出。
    async fn headers(
        &self,
        extra: &[(String, String)],
    ) -> Result<(JsHeaders, Option<String>), McpTransportError> {
        // `...(token ? { token } : {})`：空串在 JS 里是假值，等同于没有 token（§30.14）。
        let token = match &self.options.auth_provider {
            Some(provider) => provider.token().await.filter(|token| !token.is_empty()),
            None => None,
        };
        if let Ok(mut slot) = self.last_token.lock() {
            *slot = token.clone();
        }
        let headers = self.build_headers(extra, token.as_deref())?;
        Ok((headers, token))
    }

    /// `buildHeaders(extra, token)`（§30.19 的顺序）：options.headers → extra → session →
    /// protocol version → Authorization。
    ///
    /// `options.headers` 先按 WHATWG `Headers` 的构造语义合并（同名 `append` 成 `", "`），其余
    /// 各层用 `set`：后写的同名头覆盖先写的（`JsHeaders` 保留该名字第一次出现的位置）。
    /// 任何非法头名/头值都会返回错误，从而让本次请求在发出去之前失败。
    fn build_headers(
        &self,
        extra: &[(String, String)],
        token: Option<&str>,
    ) -> Result<JsHeaders, McpTransportError> {
        let mut headers = match &self.options.headers {
            Some(pairs) => JsHeaders::from_pairs(
                pairs
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone())),
            )?,
            None => JsHeaders::new(),
        };
        for (name, value) in extra {
            headers.set(name.clone(), value.clone())?;
        }
        if let Some(session_id) = self.session_id() {
            headers.set("Mcp-Session-Id", session_id)?;
        }
        if let Some(version) = self
            .protocol_version
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
            // TS 是 `if (this.protocolVersion)`——空串在 JS 里是假值，因此不发这个头
            // （`setProtocolVersion("")` 等价于没设置过；`McpClient` 不会传空串，但传输层
            // 可以被直接驱动，头是否出现是可观察的）。
            .filter(|version| !version.is_empty())
        {
            headers.set("MCP-Protocol-Version", version)?;
        }
        if let Some(token) = token {
            headers.set("Authorization", format!("Bearer {token}"))?;
        }
        Ok(headers)
    }

    /// `captureSession(response)`：响应头里**非空**的 `mcp-session-id` 才更新会话。
    fn capture_session(&self, response: &FetchResponse) {
        if let Some(session_id) = response.header("mcp-session-id")
            && !session_id.is_empty()
            && let Ok(mut slot) = self.session_id.lock()
        {
            *slot = Some(session_id);
        }
    }

    /// `checkResponse(response)`（§17.7、§30.7、§30.13）。
    ///
    /// 调用点必须在本函数**之后**才 `capture_session`：404 的 `McpSessionExpiredError` 判定用的是
    /// **旧的** session id。
    ///
    /// 分类：401 → `McpAuthRequiredError`（没有 `WWW-Authenticate` 时该字段为 `None`）；
    /// 404 且已有会话 → `McpSessionExpiredError`；其余非 2xx → `McpHttpError`，消息里的摘要由
    /// `describe_http_failure` 生成。体读取失败被当作空串，不影响状态码分类。
    async fn check_response(&self, response: &mut FetchResponse) -> Result<(), McpTransportError> {
        if response.ok() {
            return Ok(());
        }
        // `await response.text()` 的失败（连接中断/超时/已关闭）在这里退化：错误对象仍会按
        // 状态码构造，只是 body 为空。
        let text = response.text().await.unwrap_or_default();
        // `${text.slice(0, 8192)} `.slice(0, -1)：等价于 8192 个 UTF-16 代码单元的副本。
        let body = utf16_prefix(&text, MAX_ERROR_BODY_BYTES);
        let status = response.status();
        if status == 401 {
            return Err(McpTransportError::auth_required(
                response.header("www-authenticate"),
                body,
            ));
        }
        if status == 404 && self.session_id().is_some() {
            return Err(McpTransportError::session_expired(body));
        }
        Err(McpTransportError::http(
            status,
            describe_http_failure(status, &body),
            body,
        ))
    }
}

impl TransportInner {
    /// `consumeSse(stream, cursor, onMessage?)`（§17.8）。
    ///
    /// 把 `TransportEvents`、重连游标与 SSE 解析器接起来的适配层。`on_event` 按顺序做：
    /// 1. 任何有 data 的已派发事件都置 `cursor.received = true`（重连退避的健康判定）；
    /// 2. `data` 去空白后为空、或 `event` 名字不是 `message` 的事件被忽略（但其中的 `id`
    ///    仍会先更新恢复点）；
    /// 3. `JSON.parse` 与 JSON-RPC 校验失败时 `emit_error` 后**继续**读流；
    /// 4. 合法消息先交给 `on_message`（响应流用它判断"已答复"），再 `emit_message`。
    ///    如果监听器自己返回 `Err`，它会从 `emit_*` 冒出来并终止解析（与 TS 的抛出等价）。
    ///
    /// `cancel` 用 transport 的令牌：`close()` 会结束读循环并返回 `Ok(())`。
    async fn consume_sse(
        self: &Arc<Self>,
        stream: crate::auth_provider::FetchByteStream,
        cursor: Arc<Mutex<StreamCursor>>,
        on_message: Option<SseMessageHandler>,
    ) -> Result<(), McpTransportError> {
        let id_cursor = Arc::clone(&cursor);
        let retry_cursor = Arc::clone(&cursor);
        let received_cursor = Arc::clone(&cursor);
        let inner = Arc::clone(self);
        let options = ConsumeSseOptions {
            max_event_bytes: self.options.max_message_bytes,
            on_event: Some(Arc::new(move |event: SseEvent| {
                // 先置 received：任何有 data 的事件都算流有进展，哪怕它不是 JSON-RPC。
                if let Ok(mut state) = received_cursor.lock() {
                    state.received = true;
                }
                // 没有 data 的事件用于预备恢复；其他事件类型不是 JSON-RPC。
                if kiana_json::js_string::js_trim(&event.data).is_empty()
                    || event.event.as_deref().is_some_and(|name| name != "message")
                {
                    return Ok(());
                }
                let value = match kiana_json::js_parse::parse(&event.data) {
                    Ok(value) => value,
                    Err(error) => {
                        // 解析失败只报告，不中断流：上层通过 on_error 监听器看到 SyntaxError。
                        inner
                            .events
                            .emit_error(&McpTransportError::syntax(error.message))?;
                        return Ok(());
                    }
                };
                if let Err(error) = parse_json_rpc_message_value(&value) {
                    // 校验失败同样只报告；这条事件被丢弃，流继续。
                    inner.events.emit_error(&McpTransportError::from(error))?;
                    return Ok(());
                }
                // 响应流用这个回调记录 answered；代码顺序是先记录、再 emit_message。
                if let Some(on_message) = &on_message {
                    on_message(&value);
                }
                inner.events.emit_message(&value)
            })),
            on_id: Some(Arc::new(move |id: &str| {
                if let Ok(mut state) = id_cursor.lock() {
                    state.last_event_id = Some(id.to_owned());
                }
            })),
            on_retry: Some(Arc::new(move |delay_ms: f64| {
                if let Ok(mut state) = retry_cursor.lock() {
                    state.retry_ms = Some(delay_ms);
                }
            })),
            cancel: Some(self.controller.clone()),
        };
        consume_sse_stream(stream, options).await
    }

    /// `consumeResponseStream(body, requestId)`（§17.9）：读一个请求的 SSE 答复，必要时用 GET
    /// 与 `Last-Event-ID` 恢复；最终仍无答复则合成一个 `-32603` 错误响应。
    ///
    /// 循环状态：
    /// - `answered`：`on_message` 看到 id 匹配的 JSON-RPC **响应**时置位（不区分 result/error）；
    /// - `cursor.last_event_id`：每个 `id:` 字段更新，没有它就无法续读；
    /// - `cursor.retry_ms`：服务器给的 `retry:`，优先于指数退避；
    /// - `attempt`：连续**无进展**的重试次数，收到过事件后归零；
    /// - `failure`：最近一次流错误；既用于 `is_retryable` 判定是否继续重连，也用于最终
    ///   合成错误响应时的摘要。
    ///
    /// 退出条件（任一）：已答复、已 `close()`、流错误不可重试、没有 `last_event_id`、
    /// 或 `attempt` 达到 `max_retries`。正常 EOF 但尚未答复时不算失败：只要有
    /// `last_event_id` 就继续续读。已关闭时收尾返回 `Ok(())`，不会合成错误响应。
    async fn consume_response_stream(
        self: &Arc<Self>,
        body: crate::auth_provider::FetchByteStream,
        request_id: JsonRpcId,
    ) -> Result<(), McpTransportError> {
        let cursor = Arc::new(Mutex::new(StreamCursor::default()));
        let answered = Arc::new(AtomicBool::new(false));
        let answered_flag = Arc::clone(&answered);
        let wanted = request_id.clone();
        let on_message: SseMessageHandler = Arc::new(move |message: &Value| {
            if is_json_rpc_response(message)
                && value_to_json_rpc_id(&message["id"]).is_some_and(|id| id == wanted)
            {
                answered_flag.store(true, Ordering::SeqCst);
            }
        });
        let mut stream = Some(body);
        let mut failure: Option<McpTransportError> = None;
        let mut attempt: u32 = 0;
        loop {
            if let Some(current) = stream.take() {
                match self
                    .consume_sse(current, Arc::clone(&cursor), Some(Arc::clone(&on_message)))
                    .await
                {
                    Ok(()) => failure = None,
                    Err(error) => failure = Some(error),
                }
            }
            // 最常见的退出点：答复已到，或 transport 已被关闭。
            if answered.load(Ordering::SeqCst) || self.is_closed() {
                return Ok(());
            }
            // 非重试错误（4xx 等）直接放弃；`failure == None`（正常 EOF）则可能续读。
            if failure.as_ref().is_some_and(|error| !is_retryable(error)) {
                break;
            }
            let last_event_id = cursor
                .lock()
                .ok()
                .and_then(|state| state.last_event_id.clone());
            // 没有恢复点：即使想续读也不知道从哪里开始。
            let Some(last_event_id) = last_event_id else {
                break;
            };
            if attempt >= self.max_retries() {
                break;
            }
            {
                let mut state = match cursor.lock() {
                    Ok(state) => state,
                    Err(_) => return Ok(()),
                };
                // 上一轮收到过事件 → 视为有进展，连续失败计数归零（对后续上限判定生效）。
                if state.received {
                    attempt = 0;
                }
                state.received = false;
            }
            let retry_ms = cursor.lock().ok().and_then(|state| state.retry_ms);
            let delay = self.reconnect_delay(attempt, retry_ms);
            attempt += 1;
            // 等待期间 close() 会取消令牌：`sleep` 返回 false，静默退出。
            if !self.sleep(delay).await {
                return Ok(());
            }
            match self.open_sse_stream(Some(&last_event_id)).await {
                Ok(reopened) => stream = reopened,
                Err(error) => {
                    let retryable = is_retryable(&error);
                    failure = Some(error);
                    if !retryable {
                        break;
                    }
                    stream = None;
                }
            }
        }
        // close() 之后不再合成错误：请求方已不再等待，报错只会制造噪声。
        if self.is_closed() {
            return Ok(());
        }
        let reason = match &failure {
            None => "stream ended without a response".to_owned(),
            Some(error) => error.message(),
        };
        // 走到这里说明流已无法恢复：让等待答复的调用方以一条 JSON-RPC 错误结束。
        let response = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {
                "code": JsonRpcErrorCodes::INTERNAL_ERROR,
                "message": format!("MCP response stream failed: {reason}"),
            },
        });
        self.events.emit_message(&response)
    }

    /// `startGetStream()`：初始化完成后的服务端→客户端流（只能启动一次）。
    ///
    /// `open_get_stream: Some(false)` 或已 `close()` 时不启动；`get_stream_started` 用
    /// `swap(true)` 做一次性闸门，因此重复（或并发）调用只 spawn 一个任务。任务返回的错误被
    /// 丢弃（与 TS 的 `void` promise 一致），需要上报的错误在 `run_get_stream` 里经
    /// `emit_error` 发出。
    fn start_get_stream(self: &Arc<Self>) {
        if self.options.open_get_stream == Some(false) || self.is_closed() {
            return;
        }
        if self.get_stream_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let inner = Arc::clone(self);
        tokio::spawn(async move {
            // TS 的 `void this.runGetStream()`：promise 的错误不会回来，这里同样丢弃。
            let _ = inner.run_get_stream().await;
        });
    }

    /// `runGetStream()`（§17.10）：保持服务端→客户端流，断开时按退避重连。
    ///
    /// 状态机：每轮先用 `cursor.last_event_id` 打开 GET 流；流结束（正常或错误）后清掉
    /// `received`，再决定是否重连。
    /// - `Ok(None)`（405）：服务器不提供 GET 流，静默返回；
    /// - 非重试错误：`emit_error` 后返回；
    /// - 健康判定（只在流**正常结束**时做）：收到过至少一个事件，或流存活超过
    ///   `max_delay()` 毫秒，则把 `attempt` 归零；错误结束不重置；
    /// - `attempt >= max_retries`：`emit_error` 一条 "dropped and could not be reopened"
    ///   后返回；
    /// - `sleep` 被 `close()` 取消：直接返回，不报错。
    ///
    /// `cursor` 跨重连保留（`last_event_id`/`retry_ms`）；`opened_at` 每轮重新取。
    async fn run_get_stream(self: &Arc<Self>) -> Result<(), McpTransportError> {
        let cursor = Arc::new(Mutex::new(StreamCursor::default()));
        let mut attempt: u32 = 0;
        while !self.is_closed() {
            let last_event_id = cursor
                .lock()
                .ok()
                .and_then(|state| state.last_event_id.clone());
            match self.open_sse_stream(last_event_id.as_deref()).await {
                Ok(None) => {
                    // 服务器不提供 GET 流。
                    return Ok(());
                }
                Ok(Some(stream)) => {
                    // `openedAt = Date.now()`：在流打开**之后**取，健康期从这一刻算起。
                    let opened_at = tokio::time::Instant::now();
                    match self.consume_sse(stream, Arc::clone(&cursor), None).await {
                        Ok(()) => {
                            // 存活时间超过 `maxDelay()` 的流即使没事件也算健康。
                            // （只在正常结束时重置：TS 的重置写在 try 里 `await consumeSse` 之后，
                            // 抛出时不会执行。）`received` 由 `consume_sse` 的 on_event 置位：
                            // 任何有 data 的事件都算，不要求是合法 JSON-RPC。
                            let healthy = cursor.lock().ok().is_some_and(|state| state.received)
                                || opened_at.elapsed().as_secs_f64() * 1000.0 > self.max_delay();
                            if healthy {
                                attempt = 0;
                            }
                        }
                        Err(error) => {
                            if self.is_closed() {
                                return Ok(());
                            }
                            if !is_retryable(&error) {
                                self.events.emit_error(&error)?;
                                return Ok(());
                            }
                        }
                    }
                }
                Err(error) => {
                    if self.is_closed() {
                        return Ok(());
                    }
                    if !is_retryable(&error) {
                        self.events.emit_error(&error)?;
                        return Ok(());
                    }
                }
            }
            // 每次重连前清 received；retry_ms 一直保留（服务器给过的值持续生效）。
            let retry_ms = match cursor.lock() {
                Ok(mut state) => {
                    let retry_ms = state.retry_ms;
                    state.received = false;
                    retry_ms
                }
                Err(_) => return Ok(()),
            };
            // TS 的 `for (let attempt = 0; !this.closed; )` 在**每轮体之前**重新检查 `closed`：
            // 一个正常结束（或被 `close()` 取消）的流到达这里时若已关闭，直接静默退出，**不**走
            // `maxRetries` 分支（否则 `maxRetries: 0` 时 close() 之后会冒出一条假错误）。
            if self.is_closed() {
                return Ok(());
            }
            if attempt >= self.max_retries() {
                self.events.emit_error(&error(
                    "MCP server-to-client stream dropped and could not be reopened",
                ))?;
                return Ok(());
            }
            let delay = self.reconnect_delay(attempt, retry_ms);
            attempt += 1;
            if !self.sleep(delay).await {
                return Ok(());
            }
        }
        Ok(())
    }

    /// `openSseStream(lastEventId)`（§17.11）：405 时返回 `None`（没有 GET 流）。
    ///
    /// 405 是"服务器不支持 GET 流"的正常信号：`run_get_stream` 据此停止。响应流的恢复路径
    /// （`consume_response_stream`）则把 `Ok(None)` 当作"没有可恢复的 GET 流"，置空当前流后
    /// 继续按 `max_retries` 重试，最终合成 `-32603` 错误响应，**不会**因 405 立即停止。其余
    /// 非 2xx 走 `check_response`（404 且已有会话时是 `McpSessionExpiredError`）。成功时先
    /// `capture_session`，再检查 `Content-Type` 与是否有体；任一不满足都报
    /// `Unsupported MCP GET response content type`。
    async fn open_sse_stream(
        &self,
        last_event_id: Option<&str>,
    ) -> Result<Option<crate::auth_provider::FetchByteStream>, McpTransportError> {
        let mut headers = vec![("accept".to_owned(), "text/event-stream".to_owned())];
        if let Some(last_event_id) = last_event_id {
            headers.push(("last-event-id".to_owned(), last_event_id.to_owned()));
        }
        let mut response = self
            .authorized_fetch(Method::GET, headers, None, None)
            .await?;
        if response.status() == 405 {
            response.discard().await;
            return Ok(None);
        }
        self.check_response(&mut response).await?;
        self.capture_session(&response);
        let content_type = content_type(&response);
        // TS：`if (type !== "text/event-stream" || !response.body) { discard; throw McpHttpError(
        // `Unsupported MCP GET response content type: ${type ?? "missing"}`) }`。消息里写的是**实际**
        // content type（`text/event-stream` 无体时也是它），而不是 "missing"。
        if content_type.as_deref() != Some("text/event-stream") || !response_has_body(&response) {
            response.discard().await;
            return Err(McpTransportError::http(
                response.status(),
                format!(
                    "Unsupported MCP GET response content type: {}",
                    content_type.as_deref().unwrap_or("missing")
                ),
                "",
            ));
        }
        // `response.body` 非空：`take_stream()` 必然成功（`consume == false` 时 `body` 就是 `Some`）。
        match response.take_stream() {
            Some(stream) => Ok(Some(stream)),
            None => {
                response.discard().await;
                Err(McpTransportError::http(
                    response.status(),
                    format!(
                        "Unsupported MCP GET response content type: {}",
                        content_type.as_deref().unwrap_or("missing")
                    ),
                    "",
                ))
            }
        }
    }

    /// `reconnectDelay(attempt, serverDelayMs)`（§17.12）。
    ///
    /// 服务器给过 `retry:` 时原样返回该值（不参与 `max_delay` 的上界；最终由 `sleep` 经
    /// `node_timeout_duration` 做 `setTimeout` 式钳位）。否则按
    /// `min(initial * 2^attempt, max_delay)` 计算。
    fn reconnect_delay(&self, attempt: u32, server_delay_ms: Option<f64>) -> f64 {
        if let Some(server_delay_ms) = server_delay_ms {
            return server_delay_ms;
        }
        let initial = self
            .options
            .reconnect
            .and_then(|reconnect| reconnect.initial_delay_ms)
            .unwrap_or(DEFAULT_RECONNECT_INITIAL_DELAY_MS);
        // `Math.min(initial * 2 ** attempt, maxDelay())`；`attempt` 很大时是 `Infinity`。
        let backoff = (initial as f64) * 2f64.powi(attempt.min(1_000) as i32);
        backoff.min(self.max_delay())
    }

    /// `maxDelay()`。
    fn max_delay(&self) -> f64 {
        self.options
            .reconnect
            .and_then(|reconnect| reconnect.max_delay_ms)
            .unwrap_or(DEFAULT_RECONNECT_MAX_DELAY_MS) as f64
    }

    /// `maxRetries()`。
    fn max_retries(&self) -> u32 {
        self.options
            .reconnect
            .and_then(|reconnect| reconnect.max_retries)
            .unwrap_or(DEFAULT_RECONNECT_MAX_RETRIES)
    }

    /// `sleep(ms)`：transport 在等待期间关闭时返回 `false`。
    async fn sleep(&self, ms: f64) -> bool {
        if self.is_closed() {
            return false;
        }
        tokio::select! {
            biased;
            _ = self.controller.cancelled() => false,
            _ = tokio::time::sleep(node_timeout_duration(ms)) => true,
        }
    }
}

/// `isRetryable(error)`（§17.12）：瞬时状态、`TypeError`（网络失败）、`E*`/`UND_ERR` 码可重试。
///
/// 分类实现见 [`McpTransportError::is_retryable`]：HTTP 错误只看 408/429/5xx；不是 HTTP、
/// 不是 `TypeError`、也没有 `code` 的普通 `Error` 不可重试（例如超限的 SSE 错误）。
fn is_retryable(error: &McpTransportError) -> bool {
    error.is_retryable()
}

/// `parseJsonRpcMessage(value)` 的判定（失败即 `McpError(invalidRequest)`）。
///
/// 调用点决定错误如何传播：`send` 的 JSON 分支用 `?` 中止整个响应（后面的数组项不再派发）；
/// `consume_sse` 则 `emit_error` 后丢弃这一条并继续读流。
fn parse_json_rpc_message_value(value: &Value) -> Result<(), crate::jsonrpc::McpError> {
    parse_json_rpc_message(value).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Node 26 实测：`new URL(x)` 对这些输入都抛 `TypeError("Invalid URL")`。
    #[test]
    fn invalid_urls_are_type_errors_with_the_node_message() {
        for bad in ["not a url", "", "http://", "://x"] {
            let error = StreamableHttpTransport::new(StreamableHttpTransportOptions {
                url: bad.to_owned(),
                ..StreamableHttpTransportOptions::default()
            })
            .expect_err("must fail");
            assert_eq!(error.name(), "TypeError", "{bad:?}");
            assert_eq!(error.to_string(), "Invalid URL", "{bad:?}");
        }
    }

    /// `/^\d+$/`：`+5`、`5.0`、` 5`、`-5`、空串都不接受；`007` 是 7；超长数字串是 `Infinity`
    /// （§30.28）。不能用 `str::parse::<u64>()`（它接受前导 `+`）。
    #[test]
    fn retry_field_accepts_only_ascii_digits() {
        assert_eq!(parse_sse_retry("5"), Some(5.0));
        assert_eq!(parse_sse_retry("007"), Some(7.0));
        assert_eq!(parse_sse_retry("0"), Some(0.0));
        assert_eq!(parse_sse_retry("+5"), None);
        assert_eq!(parse_sse_retry("5.0"), None);
        assert_eq!(parse_sse_retry(" 5"), None);
        assert_eq!(parse_sse_retry("-5"), None);
        assert_eq!(parse_sse_retry(""), None);
        assert_eq!(parse_sse_retry("1x"), None);
        // Unicode 数字（`\d` 在 JS 里是 ASCII-only）。
        assert_eq!(parse_sse_retry("٥"), None);
        assert!(parse_sse_retry(&"9".repeat(400)).is_some_and(f64::is_infinite));
    }

    /// `setTimeout` 把不在 `[1, 2^31-1]` 内的延迟钳到 1 ms。
    #[test]
    fn node_timeout_clamps_delays() {
        assert_eq!(node_timeout_duration(0.0), Duration::from_millis(1));
        assert_eq!(node_timeout_duration(-5.0), Duration::from_millis(1));
        assert_eq!(node_timeout_duration(f64::NAN), Duration::from_millis(1));
        assert_eq!(
            node_timeout_duration(f64::INFINITY),
            Duration::from_millis(1)
        );
        assert_eq!(node_timeout_duration(1.0), Duration::from_millis(1));
        assert_eq!(
            node_timeout_duration(2_147_483_647.0),
            Duration::from_millis(2_147_483_647)
        );
        assert_eq!(
            node_timeout_duration(2_147_483_648.0),
            Duration::from_millis(1)
        );
        // 小数按 `ToUint32` 之后……Node 把 5.7 当作 5 ms（setTimeout 内部截断）。
        assert_eq!(node_timeout_duration(5.7), Duration::from_millis(5));
    }

    /// `describeHttpFailure`：`text.trim()`，超过 500 个 UTF-16 单元时 497 + `...`；
    /// 空 body 时没有 `: ` 后缀（§17.2、§30.13）。
    #[test]
    fn describe_http_failure_truncates_at_utf16_boundaries() {
        assert_eq!(
            describe_http_failure(400, ""),
            "MCP HTTP request failed with status 400"
        );
        assert_eq!(
            describe_http_failure(400, "  \n "),
            "MCP HTTP request failed with status 400"
        );
        assert_eq!(
            describe_http_failure(500, "  boom  "),
            "MCP HTTP request failed with status 500: boom"
        );
        let long = "a".repeat(600);
        let message = describe_http_failure(500, &long);
        assert_eq!(
            message,
            format!(
                "MCP HTTP request failed with status 500: {}...",
                "a".repeat(497)
            )
        );
        // UTF-16 计数：一个 emoji 是 2 个代码单元；截断点落在代理对中间时补 U+FFFD。
        let emoji = "😀".repeat(300); // 600 个代码单元
        let message = describe_http_failure(500, &emoji);
        let snippet = message
            .strip_prefix("MCP HTTP request failed with status 500: ")
            .expect("prefix")
            .strip_suffix("...")
            .expect("suffix");
        assert_eq!(utf16_len(snippet), 497);
        // 497 是奇数 → 最后一个 emoji 被切开，JS 留下孤立代理项（编码成 U+FFFD）。
        assert!(snippet.ends_with('\u{fffd}'), "{snippet:?}");
    }

    /// `needsAuthorization` 的正则（§17.2 / §27 的实测向量）。
    #[test]
    fn needs_authorization_matches_the_insufficient_scope_challenge() {
        fn response(status: u16, challenge: Option<&str>) -> FetchResponse {
            let mut headers = http::HeaderMap::new();
            if let Some(challenge) = challenge {
                headers.insert(
                    "www-authenticate",
                    http::HeaderValue::from_str(challenge).expect("header"),
                );
            }
            FetchResponse::from_bytes(status, headers, Vec::new())
        }
        assert!(needs_authorization(&response(401, None)));
        assert!(!needs_authorization(&response(403, None)));
        assert!(!needs_authorization(&response(403, Some("Bearer"))));
        assert!(needs_authorization(&response(
            403,
            Some("Bearer error=\"insufficient_scope\", scope=\"admin\"")
        )));
        assert!(needs_authorization(&response(
            403,
            Some("Bearer,error=\"insufficient_scope\"")
        )));
        assert!(needs_authorization(&response(
            403,
            Some("Bearer error=insufficient_scope")
        )));
        // 无尾边界：`error=insufficient_scopeX` 也匹配。
        assert!(needs_authorization(&response(
            403,
            Some("Bearer error=insufficient_scopeX")
        )));
        // 前边界：`xerror=` 不匹配；`=` 旁有空格不匹配；大小写不敏感。
        assert!(!needs_authorization(&response(
            403,
            Some("Bearer xerror=\"insufficient_scope\"")
        )));
        assert!(!needs_authorization(&response(
            403,
            Some("Bearer error = \"insufficient_scope\"")
        )));
        assert!(needs_authorization(&response(
            403,
            Some("BEARER ERROR=\"INSUFFICIENT_SCOPE\"")
        )));
        assert!(!needs_authorization(&response(500, Some("Bearer"))));
    }

    /// 回归：评审 finding #5（minor）。JS 的 `\s` 不含 U+0085（NEL），Rust `regex` 的 `\s` 含它。
    ///
    /// header 值按 latin1 解码，所以服务器发裸 0x85 字节时应用看到的是 `"\u{85}"`——可观测：
    /// TS 不匹配（不重试），Rust 原来会匹配（多一次授权往返）。反方向（U+FEFF）按 §17.2 的
    /// 说明不可达（头字节 > 0x7F 逐个变成 latin1 码位，UTF-8 BOM 会变成三个码位），但仍按 JS
    /// 的集合处理。
    #[test]
    fn needs_authorization_uses_javascript_whitespace_for_the_challenge_boundary() {
        fn response(status: u16, challenge: Option<&str>) -> FetchResponse {
            let mut headers = http::HeaderMap::new();
            if let Some(challenge) = challenge {
                headers.insert(
                    "www-authenticate",
                    http::HeaderValue::from_bytes(challenge.as_bytes()).expect("header"),
                );
            }
            FetchResponse::from_bytes(status, headers, Vec::new())
        }
        // U+0085（NEL）：Rust 的 `\s` 会匹配，JS 不会（Node 26 实测
        // `/(?:^|[\s,])error="?insufficient_scope"?/i.test("Bearer\u0085error=…") === false`）。
        // 裸 0x85 字节经 latin1 解码就是 U+0085，所以这条走 `needs_authorization` 的完整路径。
        assert!(!needs_authorization(&response(
            403,
            Some("Bearer\u{85}error=\"insufficient_scope\"")
        )));
        // U+FEFF：JS 的 `\s` 含它，Rust 的 `\s` 不含。头字节走不到这个码位（`EF BB BF` 经
        // latin1 变成三个码位），因此直接测正则本身。
        assert!(insufficient_scope_challenge(
            "Bearer\u{feff}error=\"insufficient_scope\""
        ));
        assert!(!insufficient_scope_challenge(
            "Bearer\u{feff}xerror=\"insufficient_scope\""
        ));
        // JS `\s` 的完整集合（Node 26 实测）都要匹配。
        for whitespace in [
            '\t', '\n', '\u{0b}', '\u{0c}', '\r', ' ', '\u{a0}', '\u{1680}', '\u{2000}',
            '\u{200a}', '\u{2028}', '\u{2029}', '\u{202f}', '\u{205f}', '\u{3000}', '\u{feff}',
        ] {
            assert!(
                insufficient_scope_challenge(&format!(
                    "Bearer{whitespace}error=\"insufficient_scope\""
                )),
                "U+{:04X} must match",
                whitespace as u32
            );
        }
        // 不是 `\s` 的：Rust 的 `char::is_whitespace` 会把 U+0085 当空白，JS 不会；
        // U+180E/U+200B 两边都不算。
        for not_whitespace in ['\u{85}', '\u{180e}', '\u{200b}', '\u{1c}'] {
            assert!(
                !insufficient_scope_challenge(&format!(
                    "Bearer{not_whitespace}error=\"insufficient_scope\""
                )),
                "U+{:04X} must not match",
                not_whitespace as u32
            );
        }
    }

    /// 回归：评审 finding #2（major）。`contentType()` 的 `trim()`/`toLowerCase()` 必须是 JS 的。
    ///
    /// 向量来自 `tests/fixtures/decoder_vectors.json` 的 `ctVectors`（node 26 上用
    /// `raw.split(";", 1)[0].trim().toLowerCase()` 生成）。
    #[test]
    fn content_type_uses_javascript_trim_and_lowercase() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/decoder_vectors.json"))
                .expect("fixture");
        let cases = fixture["ctVectors"].as_array().expect("ctVectors");
        for case in cases {
            let bytes: Vec<u8> = case["bytes"]
                .as_array()
                .expect("bytes")
                .iter()
                .map(|byte| byte.as_u64().expect("byte") as u8)
                .collect();
            // fixture 里是头值的**线上字节**；`header_joined` 按 latin1 逐个解码。
            let raw: String = bytes.iter().map(|byte| char::from(*byte)).collect();
            let mut headers = http::HeaderMap::new();
            headers.insert(
                "content-type",
                http::HeaderValue::from_bytes(&bytes).expect("header"),
            );
            let response = FetchResponse::from_bytes(200, headers, Vec::new());
            assert_eq!(
                content_type(&response).as_deref(),
                case["type"].as_str(),
                "raw {raw:?}"
            );
        }
        // 裸 0x85（NEL）：JS 的 `trim()` 不剥它，因此不等于 `text/event-stream`。
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "content-type",
            http::HeaderValue::from_bytes(b"text/event-stream\x85").expect("header"),
        );
        assert_eq!(
            content_type(&FetchResponse::from_bytes(200, headers, Vec::new())).as_deref(),
            Some("text/event-stream\u{85}"),
            "NEL must survive the trim (JS: String.prototype.trim excludes U+0085)"
        );
        // U+FEFF：JS 的 `trim()` 剥它，但头值经 latin1 解码时**到不了**这个码位——UTF-8 的
        // BOM 字节 `EF BB BF` 会变成三个 latin1 字符 `ï»¿`，JS 也不会剥（Node 26 实测
        // `"text/event-streamï»¿".trim()` 不变）。这一方向不可达，只保留记录。
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "content-type",
            http::HeaderValue::from_bytes(b"text/event-stream\xEF\xBB\xBF").expect("header"),
        );
        assert_eq!(
            content_type(&FetchResponse::from_bytes(200, headers, Vec::new())).as_deref(),
            Some("text/event-stream\u{ef}\u{bb}\u{bf}")
        );
        // U+FEFF 本身（若直接给出该码位）会被剥掉：`js_trim` 的集合与 JS 一致。
        assert_eq!(
            kiana_json::js_string::js_trim("text/event-stream\u{feff}"),
            "text/event-stream"
        );
        // 没有 content-type → `undefined`。
        assert_eq!(
            content_type(&FetchResponse::from_bytes(
                200,
                http::HeaderMap::new(),
                Vec::new()
            )),
            None
        );
    }

    /// 回归：评审 finding #3（minor）。Node 的 `TextDecoder` 在一个**补齐跨 chunk 多字节序列**
    /// 的调用里会连带丢掉紧跟其后的 BOM，朴素实现会保留它。
    ///
    /// 向量来自 `tests/fixtures/decoder_vectors.json` 的 `decoderVectors`（node 26 上按调用序列
    /// 记录 `decode(chunk, {stream:true})` 的返回值，最后一格是 `decode()`）。
    #[test]
    fn utf8_decoder_matches_the_node_chunk_and_bom_rules() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/decoder_vectors.json"))
                .expect("fixture");
        let cases = fixture["decoderVectors"]
            .as_array()
            .expect("decoderVectors");
        assert!(cases.len() >= 30);
        for case in cases {
            let chunks: Vec<Vec<u8>> = case["chunks"]
                .as_array()
                .expect("chunks")
                .iter()
                .map(|chunk| {
                    chunk
                        .as_array()
                        .expect("chunk")
                        .iter()
                        .map(|byte| byte.as_u64().expect("byte") as u8)
                        .collect()
                })
                .collect();
            let expected: Vec<&str> = case["perCall"]
                .as_array()
                .expect("perCall")
                .iter()
                .map(|text| text.as_str().expect("text"))
                .collect();
            let mut decoder = Utf8Decoder::new();
            let mut actual: Vec<String> =
                chunks.iter().map(|chunk| decoder.decode(chunk)).collect();
            actual.push(decoder.finish());
            assert_eq!(actual, expected, "chunks {chunks:?}");
        }
    }

    /// 流式 UTF-8 解码：跨 chunk 的多字节序列、非法字节、流首 BOM（§21 的 TextDecoder 映射）。
    #[test]
    fn utf8_decoder_matches_text_decoder() {
        let mut decoder = Utf8Decoder::new();
        // "café" 的 é 是 2 字节，拆在中间。
        let bytes = "data: café\n\n".as_bytes();
        let split = bytes.iter().position(|byte| *byte == 0xc3).expect("é") + 1;
        let first = decoder.decode(&bytes[..split]);
        let second = decoder.decode(&bytes[split..]);
        assert_eq!(format!("{first}{second}"), "data: café\n\n");
        // 非法字节 → U+FFFD。
        let mut decoder = Utf8Decoder::new();
        assert_eq!(decoder.decode(&[0x61, 0xff, 0x62]), "a\u{fffd}b");
        // 流首 BOM 被剥掉；后续 chunk 里的 U+FEFF 保留。
        let mut decoder = Utf8Decoder::new();
        assert_eq!(decoder.decode("\u{feff}a".as_bytes()), "a");
        assert_eq!(decoder.decode("\u{feff}b".as_bytes()), "\u{feff}b");
        // EOF 时未完成的多字节序列按替换处理。
        let mut decoder = Utf8Decoder::new();
        assert_eq!(decoder.decode(&[0xe4, 0xb8]), "");
        assert_eq!(decoder.finish(), "\u{fffd}");
    }

    /// `reconnectDelay`：服务器给了 `retry` 就用它；否则 `min(initial * 2^attempt, maxDelay)`。
    #[test]
    fn reconnect_delay_follows_the_typescript_formula() {
        let transport = StreamableHttpTransport::new(StreamableHttpTransportOptions {
            url: "http://127.0.0.1:1/mcp".to_owned(),
            ..StreamableHttpTransportOptions::default()
        })
        .expect("transport");
        let inner = &transport.inner;
        assert_eq!(inner.reconnect_delay(0, None), 1_000.0);
        assert_eq!(inner.reconnect_delay(1, None), 2_000.0);
        assert_eq!(
            inner.reconnect_delay(5, None),
            30_000.0,
            "clamped by maxDelay"
        );
        assert_eq!(inner.reconnect_delay(3, Some(7.0)), 7.0);
        assert!(inner.reconnect_delay(200, None).is_finite());

        let tuned = StreamableHttpTransport::new(StreamableHttpTransportOptions {
            url: "http://127.0.0.1:1/mcp".to_owned(),
            reconnect: Some(StreamableHttpReconnectOptions {
                initial_delay_ms: Some(10),
                max_delay_ms: Some(50),
                max_retries: Some(2),
            }),
            ..StreamableHttpTransportOptions::default()
        })
        .expect("transport");
        assert_eq!(tuned.inner.reconnect_delay(0, None), 10.0);
        assert_eq!(tuned.inner.reconnect_delay(1, None), 20.0);
        assert_eq!(tuned.inner.reconnect_delay(2, None), 40.0);
        assert_eq!(tuned.inner.reconnect_delay(3, None), 50.0);
        assert_eq!(tuned.inner.max_retries(), 2);
        assert_eq!(tuned.inner.max_delay(), 50.0);
    }

    /// `tests/fixtures/utf8_vectors.json`：由
    /// `/home/ctyun/kiana-work/scratch/kiana-mcp__transports-http/utf8_vectors.mjs` 在 node 26 上用
    /// `new TextDecoder()` 生成（含流式两段解码）。覆盖非法字节、过长编码、代理区编码、被截断的
    /// 序列、BOM 与跨 chunk 拆分的多字节字符。
    #[test]
    fn utf8_decoder_matches_text_decoder_on_generated_vectors() {
        let cases: Vec<Value> =
            serde_json::from_str(include_str!("../../tests/fixtures/utf8_vectors.json"))
                .expect("fixture");
        assert!(cases.len() >= 36);
        for case in cases {
            let bytes: Vec<u8> = case["bytes"]
                .as_array()
                .expect("bytes")
                .iter()
                .map(|byte| byte.as_u64().expect("byte") as u8)
                .collect();
            let expected = case["text"].as_str().expect("text");
            let actual = match case.get("split").and_then(Value::as_u64) {
                Some(split) => {
                    let split = split as usize;
                    let mut decoder = Utf8Decoder::new();
                    let mut text = decoder.decode(&bytes[..split]);
                    text.push_str(&decoder.decode(&bytes[split..]));
                    text.push_str(&decoder.finish());
                    text
                }
                None => {
                    let mut decoder = Utf8Decoder::new();
                    let mut text = decoder.decode(&bytes);
                    text.push_str(&decoder.finish());
                    text
                }
            };
            assert_eq!(
                actual,
                expected,
                "bytes {bytes:?} split {:?}",
                case.get("split")
            );
        }
    }

    /// 常量的公开值（§17）。
    #[test]
    fn transport_constants_match_the_typescript_source() {
        assert_eq!(MAX_ERROR_BODY_BYTES, 8 * 1024);
        assert_eq!(ERROR_MESSAGE_BODY_CHARS, 500);
        assert_eq!(DEFAULT_RECONNECT_INITIAL_DELAY_MS, 1_000);
        assert_eq!(DEFAULT_RECONNECT_MAX_DELAY_MS, 30_000);
        assert_eq!(DEFAULT_RECONNECT_MAX_RETRIES, 5);
        assert_eq!(DEFAULT_MAX_MESSAGE_BYTES, 16 * 1024 * 1024);
    }
}
