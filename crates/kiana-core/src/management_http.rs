//! utils/management-http.ts：带**有界立即重试**的管理类 HTTP 传输层。
//!
//! TS: `packages/coding-agent/src/utils/management-http.ts#fetchWithRetry`（78 行）。
//!
//! 这是版本检查、目录刷新、工具下载等**幂等**管理请求专用的传输层；agent/model 请求不走这里
//! （它们可能在 HTTP 请求开始后才失败，由语义调用方重试）。
//!
//! ## TS `fetch` → `reqwest` 的逐条映射
//!
//! - 可重试状态码集合 `{408, 425, 429, 500, 502, 503, 504}`（[`RETRYABLE_STATUS_CODES`]）。
//! - `maxRetries`：缺省/非有限 → `2`；否则 `max(0, floor(x))`。Rust 的 `Option<i64>` 无法表示
//!   `NaN`/`Infinity`（TS 里它们回退到 2）；调用方传 `None` 即等价，负数被 clamp 到 0。
//! - `retryOnStatus` 缺省 `true`。
//! - `timeoutMs` 是**所有尝试共享**的总预算（TS 在函数入口创建一次 `AbortSignal.timeout`）；
//!   `attemptTimeoutMs` 是**每次尝试**独立计时（每次尝试重新创建信号，超时只中止当前尝试）。
//! - 父 signal 的取消优先于一切：预取消时一次请求都不发（TS 在循环顶端 `throwIfAborted()`）。
//!
//! ## 错误文案（`ARCHITECTURE.md` §4.1：公开错误 `Display` == TS `error.message`）
//!
//! - 总预算或单次尝试超时：`The operation was aborted due to timeout`
//!   （`AbortSignal.timeout` 的 `DOMException.message`，[`TIMEOUT_MESSAGE`]）。
//! - 调用方取消：`This operation was aborted`（默认 abort reason 的文案，[`ABORTED_MESSAGE`]）。
//! - 传输失败：`fetch failed`（Node `fetch` 的 `TypeError.message`）；底层 `reqwest` 错误放进
//!   [`FetchError::Transport::cause`] 供 [`crate::changelog::format_version_check_error`] 与
//!   `ensureTool` 的错误链使用。Node 只在 URL 解析失败时给出别的文案
//!   （`Failed to parse URL from <input>`）；`reqwest` 的 builder 错误无法区分，统一按
//!   `fetch failed` + cause 处理（文档化差异）。
//!
//! ## 已知差异（`reqwest` 无法一一对应）
//!
//! - TS 的 `AbortSignal` 同时覆盖**等响应头与读响应体**。`timeoutMs` 已用
//!   [`RequestBuilder::timeout`]（剩余预算）附加到请求上，返回的 `Response` 会在同一
//!   deadline 中止 body（`reqwest` 的 `TotalTimeoutBody`，错误 `is_timeout() == true`）；
//!   调用方读取 body 时应把 `is_timeout()` 映射为 [`FetchError::Timeout`]（本 crate 的版本
//!   检查与工具下载都这样做，并保留 `timeout_at(同一 deadline)` 作为双保险）。
//!   父 `CancellationToken` 仍只覆盖到响应头：`reqwest::Response` 无法携带取消，body 读取
//!   期间的调用方取消不会被感知（TS 覆盖）。
//! - TS 的 `response.body?.cancel()` 与 Rust 的 drop 语义等价（丢弃 body 即放弃连接），
//!   不再复用该连接。
//! - Node 的 `fetch` 失败有两种 cause 形状：单地址是普通 `Error`；多地址（`autoSelectFamily`
//!   全部失败）是 `AggregateError`——`message` 为空串，逐地址错误在 `.errors` 里。`reqwest`
//!   只有一条源链（hyper-util 不聚合地址），`FetchError::from_reqwest` 取最深的
//!   `std::io::Error` 生成单个 cause。`formatVersionCheckError` 展平 `.errors` 后取首个非空
//!   消息，与本实现一致；`ensureTool` 的遍历不展平 `.errors`，Node 侧会得到空消息
//!   （`fetch failed: `），见 `tools_manager` 模块文档的已知差异。
//! - **errno code/文案**：`reqwest` 没有结构化的 `cause.code`，这里从错误链最深的
//!   `std::io::Error` 反推 Node 的 errno（`ECONNREFUSED`/`ENOTFOUND`/`ETIMEDOUT` 等），并按
//!   Node 的格式合成 cause 文案（`connect ECONNREFUSED <host>:<port>`、
//!   `getaddrinfo ENOTFOUND <host>`、`connect ETIMEDOUT <host>:<port>`），使
//!   `formatVersionCheckError` 与 `ensureTool` 的 warning 输出上述稳定文案（改动需同步测试）。
//!   差异：Node 在主机名解析出**多个**地址且全部失败时用 `AggregateError`，`reqwest` 只报
//!   最后一个地址；Node 的地址文案是解析后的 IP，Rust 侧只能拿到 URL 里的主机名。
//!   `reqwest` 的 `connect_timeout` 超时被映射为 `ETIMEDOUT`，而 undici 自己的连接超时是
//!   `UND_ERR_CONNECT_TIMEOUT`（文案 `Connect Timeout Error (...)`）。经代理请求时
//!   `error.url()` 是**目标** URL，代理主机连不上时 Node 打印的是代理地址，Rust 打印目标主机。
//!
//! ## 响应体读取失败（`response.json()`）
//!
//! TS 里 `response.json()` 的 body 读取失败是 `TypeError("terminated", { cause })`；本 crate
//! 的调用方用 `management_http::terminated_error` 把 `reqwest` 的 body 错误映射成同样的形状
//! （`content-length` 截断 → `UND_ERR_RES_CONTENT_LENGTH_MISMATCH`，与 `tools_manager` 的
//! 下载路径共用同一份映射）。
//!
//! ## 执行流程（一次调用）
//!
//! 1. 归一化选项：`max_retries` 缺省 2、负数取 0；`retry_on_status` 缺省 `true`；
//!    `timeout_ms`/`attempt_timeout_ms` 的 `Some(0)` 视同未设置。总预算 deadline 在函数
//!    入口固定（对应 TS 在入口创建一次 `AbortSignal.timeout`）。
//! 2. 每轮尝试先检查父取消与总预算是否已过；然后调用一次 `build()` 构造新请求，并把
//!    **剩余预算**通过 [`RequestBuilder::timeout`] 附到请求上。
//! 3. `run_attempt` 用 `biased` 的 `select!` 让父取消优先；否则按总预算与单次尝试超时中
//!    较早到期者等待响应头。
//! 4. 拿到响应后，若 `retry_on_status` 为 true（缺省）且状态码在可重试集合且还有额度，丢弃
//!    响应并重试（在 HTTP/1 连接上丢弃 `Response` 即放弃连接，等价 TS 的
//!    `response.body?.cancel()`）；否则返回响应——包括重试额度耗尽时的 5xx 响应，以及
//!    `retry_on_status: Some(false)` 时收到的可重试状态码响应（见
//!    `crates/kiana-core/tests/management_http.rs` 的
//!    `retry_on_status_false_returns_the_response_immediately`）。`max_retries` 是“首次
//!    尝试之外的次数”，总尝试次数为 `max_retries + 1`。
//! 5. 失败分支：父取消立即返回 `Aborted`；总预算到期立即返回 `Timeout`；单次尝试超时与
//!    传输错误在有额度时重试，耗尽后分别返回 `Timeout` 与 `FetchError::from_reqwest`
//!    映射的 `Transport`。重试之间没有退避或延迟。
//!
//! ## 并发与取消
//!
//! 本函数不创建任务、不持有锁；`build()` 在调用者任务上同步执行，应保持轻量（只构造
//! 请求，body 必须每次重建，因为 `RequestBuilder` 发送后即被消耗）。父
//! `CancellationToken` 在每轮尝试开始与等待响应头期间被监听，返回的 `Response` 不再感知
//! 取消；设置了 `timeout_ms` 时，总预算通过 `RequestBuilder::timeout` 延续到 body 读取，
//! 单次尝试超时不会（详见上文“已知差异”）。

use std::time::Duration;

use reqwest::{RequestBuilder, Response};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// `AbortSignal.timeout(ms)` 触发时 Node `fetch` 抛出的 `DOMException.message`。
pub const TIMEOUT_MESSAGE: &str = "The operation was aborted due to timeout";

/// 调用方 `AbortController.abort()`（无自定义 reason）时的 `DOMException.message`。
pub const ABORTED_MESSAGE: &str = "This operation was aborted";

/// 传输失败错误消息的稳定文本（`fetch failed`），改动需同步测试。
pub const FETCH_FAILED_MESSAGE: &str = "fetch failed";

/// TS 的可重试状态码集合，保持声明顺序。
pub const RETRYABLE_STATUS_CODES: [u16; 7] = [408, 425, 429, 500, 502, 503, 504];

/// `maxRetries` 的缺省值（TS：`undefined` 或非有限数）。
const DEFAULT_MAX_RETRIES: i64 = 2;

/// 一条 `cause` 链节点的信息。
///
/// TS 的 `error.cause` 是任意 `Error`；`formatVersionCheckError` 读取它的 `message` 与
/// `code` 字符串属性。`reqwest` 的错误链没有 `code`，`FetchError::from_reqwest` 从最深的
/// `std::io::Error` 反推 Node 的 errno（如 `ECONNREFUSED`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchErrorCause {
    /// `cause.message`（逐字）。
    pub message: String,
    /// `cause.code`（例如 Node 的 `ECONNREFUSED`；无法反推时为 `None`）。
    pub code: Option<String>,
}

/// `fetchWithRetry` 的公开错误。`Display` 逐字等于 TS 的 `error.message`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// 总预算（`timeoutMs`）或单次尝试超时（`attemptTimeoutMs`）。
    Timeout,
    /// 调用方 signal 已取消。
    Aborted,
    /// 传输层失败。`message` 是 TS `error.message`（通常是 `fetch failed`），
    /// `cause` 是 `error.cause` 链（`AggregateError.errors` 被平铺成多个元素）。
    Transport {
        /// TS `error.message`。
        message: String,
        /// TS `error.cause`（可能为空）。
        cause: Option<Vec<FetchErrorCause>>,
    },
}

impl FetchError {
    /// 用一条普通 `Error` 构造传输失败（`cause` 为空）。
    pub fn transport(message: impl Into<String>, cause: Option<Vec<FetchErrorCause>>) -> Self {
        Self::Transport {
            message: message.into(),
            cause,
        }
    }

    /// 把 `reqwest` 错误映射成 TS 的 `TypeError("fetch failed", {cause})` 形状。
    ///
    /// - `message` 恒为 [`FETCH_FAILED_MESSAGE`]（Node `fetch` 的请求阶段失败文案）。
    /// - `cause` 优先由最深的 `std::io::Error` 生成 Node 的 errno cause（code + 文案，见
    ///   [`node_error_cause`]）；没有可识别的 errno 时取 `reqwest` 源链**最深处**的错误文本
    ///   （Node 的 cause 是 undici 的 socket/DNS/TLS 错误，即最具体的那一层）；没有源链时
    ///   退化为 `reqwest` 自身的 `Display`，两者都为空则无 cause。
    pub(crate) fn from_reqwest(error: reqwest::Error) -> Self {
        let cause = node_error_cause(&error).or_else(|| {
            deepest_source_message(&error).map(|message| FetchErrorCause {
                message,
                code: None,
            })
        });
        Self::Transport {
            message: FETCH_FAILED_MESSAGE.to_string(),
            cause: cause.map(|cause| vec![cause]),
        }
    }
}

/// `reqwest` 源链里最深的非空 `Display` 文本。
fn deepest_source_message(error: &reqwest::Error) -> Option<String> {
    let mut deepest: Option<String> = None;
    let mut source = std::error::Error::source(error);
    while let Some(current) = source {
        let message = current.to_string();
        if !message.is_empty() {
            deepest = Some(message);
        }
        source = current.source();
    }
    deepest.or_else(|| {
        let message = error.to_string();
        (!message.is_empty()).then_some(message)
    })
}

/// 源链里最深的 `std::io::Error`（Node 的 cause 就是这一层的 errno）。
fn deepest_io_error(error: &reqwest::Error) -> Option<&std::io::Error> {
    let mut deepest = None;
    let mut source = std::error::Error::source(error);
    while let Some(current) = source {
        if let Some(io_error) = current.downcast_ref::<std::io::Error>() {
            deepest = Some(io_error);
        }
        source = current.source();
    }
    deepest
}

/// 把 `std::io::Error` 映射成 Node 的 errno 字符串。
///
/// `reqwest`/`hyper` 只保留 OS 错误；`libuv` 在 Node 侧把同一批错误命名成 errno 字符串。
/// DNS 失败（`getaddrinfo`）在 Unix 上是 `ErrorKind::Uncategorized` + 固定文案，在 Windows 上
/// 是 `WSAHOST_NOT_FOUND`（11001），两者都对应 Node 的 `ENOTFOUND`。
fn node_error_code(error: &std::io::Error) -> Option<&'static str> {
    match error.kind() {
        std::io::ErrorKind::ConnectionRefused => Some("ECONNREFUSED"),
        std::io::ErrorKind::ConnectionReset => Some("ECONNRESET"),
        std::io::ErrorKind::ConnectionAborted => Some("ECONNABORTED"),
        std::io::ErrorKind::NotConnected => Some("ENOTCONN"),
        std::io::ErrorKind::TimedOut => Some("ETIMEDOUT"),
        std::io::ErrorKind::BrokenPipe => Some("EPIPE"),
        std::io::ErrorKind::HostUnreachable => Some("EHOSTUNREACH"),
        std::io::ErrorKind::NetworkUnreachable => Some("ENETUNREACH"),
        _ => {
            let is_dns = error.raw_os_error() == Some(11001)
                || error
                    .to_string()
                    .starts_with("failed to lookup address information");
            is_dns.then_some("ENOTFOUND")
        }
    }
}

/// 由 `reqwest` 错误生成 Node 形状的 cause（`code` + `message`）。
///
/// 文案逐字对齐 Node 26：
/// - `ECONNREFUSED` → `connect ECONNREFUSED <host>:<port>`（IPv6 不带方括号）
/// - `ETIMEDOUT` → `connect ETIMEDOUT <host>:<port>`
/// - `ENOTFOUND` → `getaddrinfo ENOTFOUND <host>`
/// - 其它 errno → 保留 OS 文案，只补 `code`。
///
/// 主机名取 `error.url()` 里的目标（Node 打印的是解析后的 IP；`reqwest` 失败后没有该信息）。
fn node_error_cause(error: &reqwest::Error) -> Option<FetchErrorCause> {
    let io_error = deepest_io_error(error)?;
    let code = node_error_code(io_error)?;
    let url = error.url()?;
    let raw_host = url.host_str()?;
    let host = raw_host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(raw_host);
    let port = url.port_or_known_default().unwrap_or(0);
    let message = match code {
        "ECONNREFUSED" => format!("connect ECONNREFUSED {host}:{port}"),
        "ETIMEDOUT" => format!("connect ETIMEDOUT {host}:{port}"),
        "ENOTFOUND" => format!("getaddrinfo ENOTFOUND {host}"),
        _ => io_error.to_string(),
    };
    Some(FetchErrorCause {
        message,
        code: Some(code.to_string()),
    })
}

/// undici 在 `content-length` 与实际字节数不符时的 cause 文案
/// （`ResponseContentLengthMismatchError`，实测 Node 26 / undici 8.10.2）。
pub(crate) const CONTENT_LENGTH_MISMATCH_MESSAGE: &str =
    "Response body length does not match content-length header";

/// `ResponseContentLengthMismatchError.code`。
const CONTENT_LENGTH_MISMATCH_CODE: &str = "UND_ERR_RES_CONTENT_LENGTH_MISMATCH";

/// 把 `reqwest` 的响应体读取错误映射成 undici 的 `TypeError("terminated", { cause })`。
///
/// TS 的 `response.json()`/body 读取失败统一是 `TypeError("terminated")`：
/// - `content-length` 截断 → `ResponseContentLengthMismatchError`（[`CONTENT_LENGTH_MISMATCH_MESSAGE`]，
///   code `UND_ERR_RES_CONTENT_LENGTH_MISMATCH`）；`reqwest`/`hyper` 对应的最小错误是
///   `end of file before message length reached`。
/// - 其它 body 错误（chunked 解析、压缩等）的 cause 取决于 llhttp 的内部状态，`reqwest`
///   无法一一对应；此时保留 TS 的 `terminated` 单条消息（不给 cause）。
pub(crate) fn body_termination_cause(error: &reqwest::Error) -> Option<FetchErrorCause> {
    let deepest_message = deepest_source_message(error)?;
    (deepest_message == "end of file before message length reached").then(|| FetchErrorCause {
        message: CONTENT_LENGTH_MISMATCH_MESSAGE.to_string(),
        code: Some(CONTENT_LENGTH_MISMATCH_CODE.to_string()),
    })
}

/// [`body_termination_cause`] 的完整错误形状（`Display` == `terminated`）。
pub(crate) fn terminated_error(error: &reqwest::Error) -> FetchError {
    match body_termination_cause(error) {
        Some(cause) => FetchError::Transport {
            message: "terminated".to_string(),
            cause: Some(vec![cause]),
        },
        None => FetchError::transport("terminated", None),
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => formatter.write_str(TIMEOUT_MESSAGE),
            Self::Aborted => formatter.write_str(ABORTED_MESSAGE),
            Self::Transport { message, .. } => formatter.write_str(message),
        }
    }
}

impl std::error::Error for FetchError {}

/// `FetchRetryOptions`（TS: `management-http.ts#FetchRetryOptions`）。
///
/// `signal` 对应 TS 的 `init.signal`（父 signal）；`tokio_util::sync::CancellationToken`
/// 不携带 reason（`ARCHITECTURE.md` §4.2）。
///
/// `Default::default()`（全部字段为 `None`）等价于“最多重试 2 次 + 状态码重试开启 +
/// 无超时预算 + 无父取消”。
#[derive(Debug, Clone, Default)]
pub struct FetchRetryOptions {
    /// 初始请求之后的额外尝试次数。`None` → 2；负数 clamp 到 0。
    pub max_retries: Option<i64>,
    /// 是否把瞬时 HTTP 状态码也当作可重试。`None` → `true`。
    ///
    /// 只影响状态码判断；传输失败与单次尝试超时始终按 `max_retries` 重试，本开关
    /// 不能关闭它们。
    pub retry_on_status: Option<bool>,
    /// 所有尝试共享的总预算（毫秒）。`Some(0)` 与 `None` 一样表示不设预算。
    pub timeout_ms: Option<u64>,
    /// 每次尝试独立的超时（毫秒）。`Some(0)` 与 `None` 一样表示不设。
    pub attempt_timeout_ms: Option<u64>,
    /// 调用方取消令牌（TS `init.signal`）。
    pub signal: Option<CancellationToken>,
}

/// 单次尝试的失败原因（在 [`fetch_with_retry`] 内部区分“重试还是抛出”）。
enum AttemptError {
    /// 总预算用尽：立即抛出（TS 的 `timeoutSignal.aborted` 分支）。
    TotalTimeout,
    /// 单次尝试超时：耗尽重试额度才抛出（TS 的 `attemptTimedOut` 分支）。
    AttemptTimeout,
    /// 父 signal 取消。
    Aborted,
    /// `reqwest` 传输错误。
    Transport(reqwest::Error),
}

/// `fetchWithRetry(build, options)`。
///
/// `build` 在**每次尝试前**重新调用，返回一个新的 [`RequestBuilder`]（对应 TS 每次尝试
/// 用同一个 `input`/`init` 重新 `fetch`；`reqwest` 的 builder 是一次性的，body 也必须重建）。
///
/// 成功时返回最后一次的响应（包括可重试状态码在重试额度用尽后的响应 —— TS 也是直接返回）。
///
/// # 错误
///
/// - 父 `signal` 已取消（或等待响应头期间被取消）→ [`FetchError::Aborted`]；
/// - 总预算或最后一次尝试超时 → [`FetchError::Timeout`]；
/// - 传输失败且重试额度耗尽 → `FetchError::from_reqwest` 映射的
///   [`FetchError::Transport`]。
///
/// 任何错误都发生在拿到响应头之前；一旦返回 `Ok(Response)`，body 读取阶段的错误由调用方
/// 自行映射（见模块文档“响应体读取失败”）。
pub async fn fetch_with_retry<F>(
    build: F,
    options: FetchRetryOptions,
) -> Result<Response, FetchError>
where
    F: Fn() -> RequestBuilder,
{
    let max_retries = match options.max_retries {
        Some(value) => value.max(0),
        None => DEFAULT_MAX_RETRIES,
    };
    let retry_on_status = options.retry_on_status.unwrap_or(true);
    let deadline = options
        .timeout_ms
        .filter(|timeout| *timeout > 0)
        .and_then(|timeout| Instant::now().checked_add(Duration::from_millis(timeout)));
    let attempt_timeout = options
        .attempt_timeout_ms
        .filter(|timeout| *timeout > 0)
        .map(Duration::from_millis);
    let signal = options.signal.as_ref();

    // `attempt` 记录已经用掉的“额外尝试”次数：0 表示首次尝试，只有
    // `attempt < max_retries` 才允许再发一次，因此总尝试次数是 `max_retries + 1`。
    let mut attempt: i64 = 0;
    loop {
        // TS：`parentSignal?.throwIfAborted(); timeoutSignal?.throwIfAborted();`
        if let Some(signal) = signal
            && signal.is_cancelled()
        {
            return Err(FetchError::Aborted);
        }
        if let Some(deadline) = deadline
            && Instant::now() >= deadline
        {
            return Err(FetchError::Timeout);
        }

        // TS 的 `timeoutMs` 是**从函数入口开始**的一个 `AbortSignal.timeout`，它同样中止
        // 响应体读取。`reqwest` 的 `RequestBuilder::timeout` 是“请求开始 + 该时长”，把剩余
        // 预算设进去后，返回的 `Response` 会在同一 deadline 中止 body（`TotalTimeoutBody`）。
        let request = match deadline {
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(FetchError::Timeout);
                }
                build().timeout(remaining)
            }
            None => build(),
        };
        match run_attempt(request, deadline, attempt_timeout, signal).await {
            Ok(response) => {
                let status = response.status().as_u16();
                // 可重试状态码同样消耗一次额度；额度耗尽时不做转换，直接返回该响应。
                let should_retry = retry_on_status
                    && RETRYABLE_STATUS_CODES.contains(&status)
                    && attempt < max_retries;
                if !should_retry {
                    return Ok(response);
                }
                // TS：`await response.body?.cancel()`，异常忽略。丢弃 `Response` 即放弃连接，
                // 与取消 body 的可观察行为一致。
                drop(response);
                attempt += 1;
            }
            Err(AttemptError::Aborted) => return Err(FetchError::Aborted),
            Err(AttemptError::TotalTimeout) => return Err(FetchError::Timeout),
            Err(AttemptError::AttemptTimeout) => {
                // TS：尝试超时只中止当前尝试；还有额度就重试，否则抛出 TimeoutError。
                if attempt >= max_retries {
                    return Err(FetchError::Timeout);
                }
                attempt += 1;
            }
            Err(AttemptError::Transport(error)) => {
                if attempt >= max_retries {
                    return Err(FetchError::from_reqwest(error));
                }
                attempt += 1;
            }
        }
    }
}

/// 跑一次尝试：父取消优先，其次是总预算，最后是单次尝试超时。
///
/// `biased` 的 `select!` 先轮询取消分支：取消与响应同时就绪时返回 `Aborted` 而不是响应。
/// 分支结束时未完成的 `send` future 被丢弃，请求不会再被轮询。
async fn run_attempt(
    request: RequestBuilder,
    deadline: Option<Instant>,
    attempt_timeout: Option<Duration>,
    signal: Option<&CancellationToken>,
) -> Result<Response, AttemptError> {
    let send = request.send();
    match signal {
        Some(signal) => tokio::select! {
            biased;
            _ = signal.cancelled() => Err(AttemptError::Aborted),
            result = run_with_timeouts(send, deadline, attempt_timeout) => result,
        },
        None => run_with_timeouts(send, deadline, attempt_timeout).await,
    }
}

/// 把总预算与单次尝试超时套在 `send` 上。
///
/// 两者同时存在时取更早到期的那个；**相等时算总预算**（TS 的 `timeoutSignal` 在函数入口
/// 创建、先于 `attemptTimeoutSignal` 注册，`AbortSignal.any` 会以先中止者为准）。
async fn run_with_timeouts(
    send: impl std::future::Future<Output = reqwest::Result<Response>>,
    deadline: Option<Instant>,
    attempt_timeout: Option<Duration>,
) -> Result<Response, AttemptError> {
    let classify = |error: reqwest::Error| classify_send_error(error, deadline);
    match (deadline, attempt_timeout) {
        (Some(deadline), Some(attempt_timeout)) => {
            let now = Instant::now();
            if deadline <= now {
                return Err(AttemptError::TotalTimeout);
            }
            if attempt_timeout < deadline - now {
                match tokio::time::timeout(attempt_timeout, send).await {
                    Ok(result) => result.map_err(classify),
                    Err(_) => Err(AttemptError::AttemptTimeout),
                }
            } else {
                match tokio::time::timeout_at(deadline, send).await {
                    Ok(result) => result.map_err(classify),
                    Err(_) => Err(AttemptError::TotalTimeout),
                }
            }
        }
        (Some(deadline), None) => match tokio::time::timeout_at(deadline, send).await {
            Ok(result) => result.map_err(classify),
            Err(_) => Err(AttemptError::TotalTimeout),
        },
        (None, Some(attempt_timeout)) => match tokio::time::timeout(attempt_timeout, send).await {
            Ok(result) => result.map_err(classify),
            Err(_) => Err(AttemptError::AttemptTimeout),
        },
        (None, None) => send.await.map_err(classify),
    }
}

/// `reqwest` 的请求级总超时（`RequestBuilder::timeout`，与我们的 deadline 相同）触发时，
/// `send` 返回的是 `is_timeout()` 的 `reqwest::Error`，而不是 tokio 的 `Elapsed`。
/// 这类错误必须归为总预算超时（终止重试），否则会被当成可重试的传输失败。
///
/// 只在其到期时间确实已过时才这样归类：`read_timeout`（idle）也可能产生 `is_timeout()`，
/// 但它在总预算之前触发时仍是 TS 的 headersTimeout（可重试的传输错误）。
fn classify_send_error(error: reqwest::Error, deadline: Option<Instant>) -> AttemptError {
    if error.is_timeout()
        && let Some(deadline) = deadline
        && Instant::now() >= deadline
    {
        return AttemptError::TotalTimeout;
    }
    AttemptError::Transport(error)
}
