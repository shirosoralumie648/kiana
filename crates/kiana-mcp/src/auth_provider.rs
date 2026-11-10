//! `auth-provider.ts`（外加 crate 内共用的 `fetch` 桥）。
//!
//! TS 源：`packages/mcp/src/auth-provider.ts`、`packages/mcp/src/protocol/jsonrpc.ts`（`toError`）
//! 与 Web 平台 `fetch`/`Headers`/`Response`（`spec/telemetry-mcp.md` §12、§17.4、§21）。
//!
//! TS 的 `McpFetch` 就是全局 `fetch` 的类型别名，传输与 OAuth 用它做**全部** HTTP 访问，因此调用方
//! 可以整体替换 HTTP 栈（测试、代理、Cloudflare Workers）。Rust 侧保留同一能力：
//! [`McpFetch`] 是对象安全的异步 trait，默认实现是 [`ReqwestFetch`]。
//!
//! # 为什么错误信封在这里
//!
//! TS 的传输层用 `instanceof` 区分 `McpError` / `McpHttpError`（含 `status`/`body`）/
//! `McpAuthRequiredError` / `McpSessionExpiredError` / `TypeError`，并按 `.name` 与 `.status`
//! 分支。Rust 侧把这些收进一个可克隆的错误信封 [`McpTransportError`]：`name()` 返回逐字的
//! JS `error.name`，`status()`/`body()`/`www_authenticate()` 等返回对应字段。所有跨层的
//! `Result<_, McpTransportError>`（fetch、transport、auth provider）都用它，从而不需要 `anyhow`
//! （`ARCHITECTURE.md` §4.1 禁止）。
//!
//! # 与 TS 的已知偏差
//!
//! - `AbortSignal` → [`tokio_util::sync::CancellationToken`]：取消没有 `reason` 载荷，`abort()`
//!   与 `abort(undefined)` 都等价（TS 的 reason 文案由 `McpClient` 显式拼出）。
//! - `Headers` 用 [`JsHeaders`] 复刻 WHATWG 规则（只裁剪首尾空白、内部空白原样保留、
//!   大小写不敏感覆盖、`append` 以 `", "` 合并、非法名/值报错），而不是直接暴露 `HeaderMap`
//!   （`HeaderValue` 允许首尾空格与 `\t`，但拒绝 `0x01`、`0x7F` 等控制字节；`HeaderMap`
//!   的迭代顺序也不保留插入序，值不做 WHATWG 裁剪）。
//! - `Response.statusText`：值取自 HTTP/1 状态行的原因短语（hyper 只在短语与注册短语不同
//!   或为空时把它放进响应 extensions 的 `ReasonPhrase`，其余情况用
//!   `StatusCode::canonical_reason()`）。与 Node 一致：`HTTP/1.1 200 Weird Reason` →
//!   `"Weird Reason"`、`HTTP/1.1 200` → `""`、`HTTP/1.1 404 Not Found` → `"Not Found"`。
//!   HTTP/2 没有原因短语，值总是 `""`（Node 同样如此）；HTTP/1.0 与 obs-text 字节按 hyper 的
//!   解析结果（非 UTF-8 字节以 U+FFFD 替换）。
//!
//! # 文件结构与阅读顺序
//!
//! 1. [`McpTransportError`]：跨层错误信封，HTTP 传输、OAuth 与 `fetch` 的错误都收敛到它。
//! 2. [`JsHeaders`] 与 [`header_joined`] / [`content_type_header`]：WHATWG 头的兼容层与读头
//!    辅助函数。
//! 3. [`FetchInit`] / [`FetchBody`] / [`FetchResponse`] / [`McpFetch`] / [`ReqwestFetch`]：
//!    `fetch` 的输入输出类型与默认实现。
//! 4. [`UnauthorizedContext`] / [`AuthProvider`]：认证提供方接口（`auth-provider.ts` 的本体）。
//!
//! # 谁在用这个模块
//!
//! - `crate::transports::streamable_http`：用 [`FetchInit`] / [`FetchResponse`] / [`JsHeaders`]
//!   发 MCP 请求，并持有 `Arc<dyn AuthProvider>`；收到 401（或带 `insufficient_scope` 的 403）
//!   时把 [`UnauthorizedContext`] 交给提供方——前提是提供方的 `has_on_unauthorized()` 为
//!   `true`；否则不重试，响应原样返回，由 `check_response` 按状态分类成错误。
//! - `crate::oauth` 的 `flow` / `discovery`：用 [`McpFetch`] 做发现与令牌请求。
//! - `crate::transports::stdio` 与 `in_memory`：只用错误信封 [`McpTransportError`]。
//! - 另一个 crate `kiana-builtin-extensions`：实现 [`AuthProvider`]（OAuth），并包装
//!   [`McpFetch`] 给每次 OAuth 请求套一层超时。
//!
//! 由此，改 [`JsHeaders`] 的校验规则、改 `fetch` 的默认头或改错误 `name()`，都会同时波及
//! MCP 传输与 OAuth 两条链路。`name()` 返回的字符串是跨 crate 的契约：例如
//! `kiana-builtin-extensions` 的 `needs_sign_in` 经
//! [`McpTransportError::is_oauth_authorization_required`] 按
//! `McpOAuthAuthorizationRequiredError` 这个名字分支。
//!
//! # 一次 `ReqwestFetch::fetch` 的执行顺序
//!
//! 1. 拒绝 GET/HEAD 带体（在发请求**之前**报 `TypeError`，与 `fetch` 规范一致）。
//! 2. 把 `Method` 转成 `reqwest::Method`；请求体对应的 `content-type` 只在调用方没设时补齐。
//! 3. 补齐 Node `fetch` 的默认头（`accept`、`accept-language`、`sec-fetch-mode`、
//!    `user-agent`、`accept-encoding`）；调用方显式设置的同名头优先。
//! 4. `JsHeaders::try_to_header_map()`：头值里的控制字符到这一步才报 `TypeError`。
//! 5. 在发请求**之前**算出绝对截止时刻 `deadline`；`send()` 外面套 `tokio::time::timeout`，
//!    取消令牌用 `biased` 的 `select!` 与它竞争。
//! 6. 构造响应：`statusText` 优先取 hyper 的 `ReasonPhrase` 扩展，缺失时退回
//!    `StatusCode::canonical_reason()`；响应体保持为流。
//! 7. 用 [`FetchResponse::with_abort`] 把 `signal` 与 `deadline` 装到 [`FetchResponse`] 上，
//!    后续 `bytes` / `text` / `json` 的读取因此也受同一个取消信号与截止时刻约束。
//!
//! # 取消、超时与失败模式
//!
//! - **超时是一个总预算**：`deadline` 与响应头阶段的 `timeout` 同源，体读取用
//!   `timeout_at(deadline)`，头阶段花掉的时间不会补给体读取。
//! - **取消优先**：两处 `select!`（发请求与读体）都是 `biased`，同一轮里取消信号已就绪时，
//!   即使请求或读取也已完成，返回的仍是 `AbortError`。
//! - **体只能读一次**：`bytes` / `text` / `json` 先把流取走再等待读取；取消、超时或流错误之后
//!   `consumed` 已置位、`body` 已为 `None`，再读只会得到
//!   `TypeError("Body is unusable: Body has already been read")`。
//! - **错误身份**：取消是 `AbortError`、`AbortSignal.timeout` 是 `TimeoutError`（两者都是
//!   `DOMException` 身份）、网络失败是 `TypeError`、`json()` 的解析失败是 `SyntaxError`；
//!   上层按 `name()` 分支，不要按消息文本判断。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use http::{HeaderMap, HeaderName, HeaderValue, Method};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::jsonrpc::McpError;

/// 传输层错误信封（TS 的 `Error` 家族）。
///
/// `Display` == `error.message`（逐字）；[`McpTransportError::name`] == `error.name`。
#[derive(Debug, Clone)]
pub struct McpTransportError {
    kind: ErrorKind,
}

#[derive(Debug, Clone)]
enum ErrorKind {
    /// `McpError` / `McpConnectionClosedError` / `McpTimeoutError` / `McpAbortError`。
    Mcp(McpError),
    /// `McpHttpError` 家族：`name` 为三者之一，带 HTTP 状态与响应体摘录。
    Http {
        name: &'static str,
        status: u16,
        message: String,
        body: String,
        www_authenticate: Option<String>,
    },
    /// `SyntaxError`（`Response.json()` 解析失败）。
    Syntax { message: String },
    /// `DOMException`（`AbortSignal` 的取消与超时）：`name` 是 `AbortError` / `TimeoutError`。
    Dom { name: &'static str, message: String },
    /// 普通 `Error` / `TypeError`（网络失败）以及带 `code` 的 Node 系统错误。
    Plain {
        message: String,
        /// `error instanceof TypeError`（fetch 把网络失败报成 TypeError）。
        network: bool,
        /// Node 的 `error.code`（`ECONNRESET`、`UND_ERR_SOCKET` …）。
        code: Option<String>,
    },
    /// 任意 `name` 的普通 `Error` 子类（OAuth 的 `OAuthError`、
    /// `McpOAuthAuthorizationRequiredError` 等）。`instanceof TypeError`/`DOMException` 为假，
    /// 也没有 `code`/`status`。
    Named { name: &'static str, message: String },
}

impl McpTransportError {
    /// `new Error(message)`（`name === "Error"`）。
    pub fn new(message: impl Into<String>) -> Self {
        McpTransportError {
            kind: ErrorKind::Plain {
                message: message.into(),
                network: false,
                code: None,
            },
        }
    }

    /// `fetch` 的网络失败：`error instanceof TypeError`（§17.10 的重试分类依赖它）。
    pub fn network(message: impl Into<String>) -> Self {
        McpTransportError {
            kind: ErrorKind::Plain {
                message: message.into(),
                network: true,
                code: None,
            },
        }
    }

    /// 带 Node 风格 `code` 的错误（`ECONNREFUSED`/`UND_ERR_*`；重试分类同样识别它们）。
    pub fn with_code(message: impl Into<String>, code: impl Into<String>) -> Self {
        McpTransportError {
            kind: ErrorKind::Plain {
                message: message.into(),
                network: false,
                code: Some(code.into()),
            },
        }
    }

    /// `new TypeError(message)` 且带 Node 风格 `code`（例如 `ERR_INVALID_ARG_VALUE`）。
    ///
    /// Node 的 `ERR_INVALID_ARG_VALUE` 是 `TypeError` 的子类：`name === "TypeError"`、
    /// `code === "ERR_INVALID_ARG_VALUE"`。`process` 层（stdio 传输的 spawn 参数校验）用它。
    pub fn type_error_with_code(message: impl Into<String>, code: impl Into<String>) -> Self {
        McpTransportError {
            kind: ErrorKind::Plain {
                message: message.into(),
                network: true,
                code: Some(code.into()),
            },
        }
    }

    /// 带自定义 `name` 的普通 `Error` 子类（`class X extends Error { name = "X" }`）。
    ///
    /// OAuth 的五个错误类在 TS 里就是这样（`OAuthError`、`McpOAuthAuthorizationRequiredError`
    /// …）。`instanceof Error` 为真，`instanceof TypeError`/`DOMException` 为假，没有
    /// `code`/`status`/`body`。跨到传输层时保留 `name`，上层才能按类名分支（例如 coding-agent
    /// 的 `needsSignIn` 用 `error instanceof McpOAuthAuthorizationRequiredError`）。
    pub fn named(name: &'static str, message: impl Into<String>) -> Self {
        McpTransportError {
            kind: ErrorKind::Named {
                name,
                message: message.into(),
            },
        }
    }

    /// `new McpHttpError(status, message, body)`（`streamable-http.ts`）。
    pub fn http(status: u16, message: impl Into<String>, body: impl Into<String>) -> Self {
        McpTransportError {
            kind: ErrorKind::Http {
                name: "McpHttpError",
                status,
                message: message.into(),
                body: body.into(),
                www_authenticate: None,
            },
        }
    }

    /// `new McpAuthRequiredError(response, body)`：`status = 401`、消息逐字
    /// `MCP server requires authentication`、`wwwAuthenticate` 来自响应头。
    pub fn auth_required(www_authenticate: Option<String>, body: impl Into<String>) -> Self {
        McpTransportError {
            kind: ErrorKind::Http {
                name: "McpAuthRequiredError",
                status: 401,
                message: "MCP server requires authentication".to_owned(),
                body: body.into(),
                www_authenticate,
            },
        }
    }

    /// `new McpSessionExpiredError(body)`：`status = 404`、消息逐字 `MCP session expired`。
    pub fn session_expired(body: impl Into<String>) -> Self {
        McpTransportError {
            kind: ErrorKind::Http {
                name: "McpSessionExpiredError",
                status: 404,
                message: "MCP session expired".to_owned(),
                body: body.into(),
                www_authenticate: None,
            },
        }
    }

    /// `error.name`（逐字；`McpError` 家族见 [`McpError::name`]）。
    pub fn name(&self) -> &'static str {
        match &self.kind {
            ErrorKind::Mcp(error) => error.name(),
            ErrorKind::Http { name, .. } => name,
            ErrorKind::Dom { name, .. } => name,
            ErrorKind::Syntax { .. } => "SyntaxError",
            ErrorKind::Plain { network: true, .. } => "TypeError",
            ErrorKind::Plain { .. } => "Error",
            ErrorKind::Named { name, .. } => name,
        }
    }

    /// `error.message`（`Display` 同值；`McpTimeoutError` 的消息是拼出来的，因此返回 `String`）。
    pub fn message(&self) -> String {
        match &self.kind {
            ErrorKind::Mcp(error) => match error {
                McpError::JsonRpc { message, .. } | McpError::ConnectionClosed { message } => {
                    message.clone()
                }
                McpError::Aborted { message } => message.clone(),
                McpError::Timeout { timeout_ms } => {
                    format!("MCP request timed out after {timeout_ms}ms")
                }
            },
            ErrorKind::Http { message, .. }
            | ErrorKind::Dom { message, .. }
            | ErrorKind::Syntax { message }
            | ErrorKind::Plain { message, .. }
            | ErrorKind::Named { message, .. } => message.clone(),
        }
    }

    /// 包装的 [`McpError`]（若有）。
    pub fn as_mcp(&self) -> Option<&McpError> {
        match &self.kind {
            ErrorKind::Mcp(error) => Some(error),
            _ => None,
        }
    }

    /// `error.status`（只有 `McpHttpError` 家族才有）。
    pub fn status(&self) -> Option<u16> {
        match &self.kind {
            ErrorKind::Http { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// `error.body`（`McpHttpError` 家族；最多 8192 个 UTF-16 代码单元，见 §30.13）。
    ///
    /// 构造器本身不截断：这里返回的 `&str` 就是构造时传入的整串。8192 单元的上限由调用方
    /// （`streamable_http::check_response`）在构造错误**之前**施加。
    pub fn body(&self) -> Option<&str> {
        match &self.kind {
            ErrorKind::Http { body, .. } => Some(body),
            _ => None,
        }
    }

    /// `McpAuthRequiredError.wwwAuthenticate`（缺失为 `None`，TS 用 `null`）。
    pub fn www_authenticate(&self) -> Option<&str> {
        match &self.kind {
            ErrorKind::Http {
                www_authenticate, ..
            } => www_authenticate.as_deref(),
            _ => None,
        }
    }

    /// Node 风格 `error.code`。
    pub fn code(&self) -> Option<&str> {
        match &self.kind {
            ErrorKind::Plain { code, .. } => code.as_deref(),
            _ => None,
        }
    }

    /// `error instanceof McpHttpError`。
    pub fn is_http(&self) -> bool {
        matches!(self.kind, ErrorKind::Http { .. })
    }

    /// `error instanceof McpAuthRequiredError`。
    pub fn is_auth_required(&self) -> bool {
        matches!(
            self.kind,
            ErrorKind::Http {
                name: "McpAuthRequiredError",
                ..
            }
        )
    }

    /// `error instanceof McpSessionExpiredError`。
    pub fn is_session_expired(&self) -> bool {
        matches!(
            self.kind,
            ErrorKind::Http {
                name: "McpSessionExpiredError",
                ..
            }
        )
    }

    /// `error instanceof McpOAuthAuthorizationRequiredError`。
    ///
    /// OAuth 错误类跨到传输层时用 [`McpTransportError::named`] 保留类名（见
    /// `oauth::errors::OAuthFailure` 的 `From` 实现）；coding-agent 的 `needsSignIn` 靠它区分
    /// 「需要用户重新登录」与普通的 `McpAuthRequiredError`。
    pub fn is_oauth_authorization_required(&self) -> bool {
        matches!(
            self.kind,
            ErrorKind::Named {
                name: "McpOAuthAuthorizationRequiredError",
                ..
            }
        )
    }

    /// `error instanceof TypeError`。
    pub fn is_type_error(&self) -> bool {
        matches!(self.kind, ErrorKind::Plain { network: true, .. })
    }

    /// `name === "AbortError"`：`McpAbortError`，或 `fetch` 抛出的 `DOMException`。
    ///
    /// TS 的 `AbortSignal` 取消由平台的 `fetch` 报成 `DOMException(name = "AbortError")`，
    /// 上层（例如 coding-agent 的 abort 处理）按 `error.name` 分支，因此两者都必须命中。
    pub fn is_abort(&self) -> bool {
        match &self.kind {
            ErrorKind::Mcp(error) => error.is_abort(),
            ErrorKind::Dom { name, .. } => *name == "AbortError",
            _ => false,
        }
    }

    /// `name === "TimeoutError"`：`AbortSignal.timeout` 触发的 `DOMException`。
    pub fn is_timeout_error(&self) -> bool {
        matches!(&self.kind, ErrorKind::Dom { name, .. } if *name == "TimeoutError")
    }

    /// `error instanceof DOMException`。
    ///
    /// 只有 `Dom` 变体（[`McpTransportError::aborted`] / [`McpTransportError::timed_out`]）为真；
    /// 由 [`McpError::aborted`] 转进来的 `AbortError` 是 `Mcp` 变体，这里为 `false`
    /// （但 `is_abort()` 仍为 `true`）。
    pub fn is_dom_exception(&self) -> bool {
        matches!(&self.kind, ErrorKind::Dom { .. })
    }

    /// `fetch` 取消：`new DOMException("This operation was aborted", "AbortError")`。
    pub fn aborted() -> Self {
        McpTransportError {
            kind: ErrorKind::Dom {
                name: "AbortError",
                message: "This operation was aborted".to_owned(),
            },
        }
    }

    /// `TypeError("Body is unusable: Body has already been read")`（Node 的 `Response`）。
    pub fn body_unusable() -> Self {
        McpTransportError::network("Body is unusable: Body has already been read")
    }

    /// `SyntaxError`（`Response.json()` 的解析失败）。
    pub fn syntax(message: impl Into<String>) -> Self {
        McpTransportError {
            kind: ErrorKind::Syntax {
                message: message.into(),
            },
        }
    }

    /// `AbortSignal.timeout(ms)` 触发：
    /// `new DOMException("The operation was aborted due to timeout", "TimeoutError")`。
    pub fn timed_out() -> Self {
        McpTransportError {
            kind: ErrorKind::Dom {
                name: "TimeoutError",
                message: "The operation was aborted due to timeout".to_owned(),
            },
        }
    }

    /// `streamable-http.ts` 的 `isTransientStatus`：408、429、5xx。
    pub fn is_transient_status(&self) -> bool {
        self.status()
            .is_some_and(|status| status == 408 || status == 429 || status >= 500)
    }

    /// `streamable-http.ts` 的 `isRetryable(error)`：
    /// `McpHttpError` → 只重试瞬时状态；`TypeError` → 重试；`code` 以 `E`/`UND_ERR` 开头 → 重试。
    pub fn is_retryable(&self) -> bool {
        if self.is_http() {
            return self.is_transient_status();
        }
        if self.is_type_error() {
            return true;
        }
        self.code()
            .is_some_and(|code| code.starts_with('E') || code.starts_with("UND_ERR"))
    }
}

/// `Display` 与 [`McpTransportError::message`] 保持一致：`McpError` 变体走 thiserror 的模板，
/// 其余变体直接写消息。日志与断言可以互换使用。
impl std::fmt::Display for McpTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            ErrorKind::Mcp(error) => write!(f, "{error}"),
            ErrorKind::Http { message, .. }
            | ErrorKind::Dom { message, .. }
            | ErrorKind::Syntax { message }
            | ErrorKind::Plain { message, .. }
            | ErrorKind::Named { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for McpTransportError {}

/// `McpError` 家族 → 同一个信封：[`McpTransportError::name`] 逐字返回四个 TS 类名
/// （`McpError`、`McpConnectionClosedError`、`McpTimeoutError`、`AbortError`），`Display`
/// 逐字输出对应的 `error.message`（`Timeout` 的消息由 `timeout_ms` 拼出），
/// [`McpTransportError::is_abort`] 委派给 `McpError::is_abort`。
impl From<McpError> for McpTransportError {
    fn from(error: McpError) -> Self {
        McpTransportError {
            kind: ErrorKind::Mcp(error),
        }
    }
}

impl From<reqwest::Error> for McpTransportError {
    /// `fetch` 把网络失败报成 `TypeError`；HTTP 状态错误不是错误（由调用方看 `status`）。
    ///
    /// `is_timeout()` 也归到这里：TS 的 `fetch` 本身没有超时，底层 socket 超时在 Node 里是
    /// `TypeError: fetch failed`（cause 为 `UND_ERR_HEADERS_TIMEOUT` 之类）——即 `TypeError`。
    ///
    /// 只有上面列出的网络/构建/请求/连接/体/解码/超时变体算 `TypeError`；其余变体
    /// （`is_status`、`is_redirect` 等）落到普通 `Error`。
    fn from(error: reqwest::Error) -> Self {
        let message = error.to_string();
        if error.is_builder()
            || error.is_request()
            || error.is_connect()
            || error.is_body()
            || error.is_decode()
            || error.is_timeout()
        {
            McpTransportError::network(message)
        } else {
            McpTransportError::new(message)
        }
    }
}

/// `kiana_json::ParseError` → 普通 `Error`（`message` 逐字，**不是** `SyntaxError`）。
///
/// `Response.json()` 不走这个转换：它用 [`McpTransportError::syntax`] 显式构造 `SyntaxError`
/// （Node 的 `Response.json()` 正是抛 `SyntaxError`）。需要区分两者时按 `name()` 判断。
impl From<kiana_json::ParseError> for McpTransportError {
    fn from(error: kiana_json::ParseError) -> Self {
        McpTransportError::new(error.message)
    }
}

// ---------------------------------------------------------------------------
// JS `Headers`
// ---------------------------------------------------------------------------

/// WHATWG `Headers` 的兼容层（`spec/telemetry-mcp.md` §17.4 / §21 / §25 第 11 条）。
///
/// 与 `http::HeaderMap` 的关键差异（都已在本机 Node 26 实测）：
/// - 只裁剪**首尾** HTTP 空白（空格、`\t`、`\r`、`\n`），**内部空白原样保留**
///   （`"  x   y  "` → `"x   y"`；`"a\tb"` 保留 tab）；
/// - `set` 大小写不敏感地覆盖（保持首次插入的位置）；
/// - `append` 用 `", "` 连接；
/// - 名字含空格等非法字符或值含 `\n`/`\r`/`\0` 时抛 `TypeError`（这里返回
///   `McpTransportError { name: "TypeError" }`）；
/// - `get` 大小写不敏感。
///
/// 内部用 `Vec<(小写名字, 值)>` 而不是 `http::HeaderMap`：WHATWG 的裁剪与 `append` 合并必须
/// 在**写入时**完成。`HeaderValue` 允许首尾空格与 `\t`，却拒绝 `0x01`、`0x7F` 等控制字节，
/// 因此 JsHeaders 只能在 [`JsHeaders::try_to_header_map`] 时才失败，不能把校验提前交给
/// `HeaderMap`；`set` 覆盖时还要保住首次插入的位置，而 `HeaderMap` 的迭代顺序不保留插入序。
/// 按名字查找是线性扫描，没有额外索引。
#[derive(Debug, Clone, Default)]
pub struct JsHeaders {
    /// 小写名字 + 有序列（重复名字来自 `append`，按插入序合并）。
    entries: Vec<(String, String)>,
}

impl JsHeaders {
    /// `new Headers()`。
    pub fn new() -> Self {
        JsHeaders {
            entries: Vec::new(),
        }
    }

    /// `new Headers(obj)`：对象字面量 → 头（值同样裁剪首尾）。
    ///
    /// 与 WHATWG 一致，同名键（大小写不敏感）以 `append` 合并，因此
    /// `{"Accept":"a", accept:"b"}` 得到 `"a, b"`。
    ///
    /// 非法名/值返回错误（TS 抛 `TypeError`）。
    pub fn from_pairs<I, K, V>(pairs: I) -> Result<Self, McpTransportError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let mut headers = JsHeaders::new();
        for (name, value) in pairs {
            // `new Headers(record)` 走 `append`（HTML 规范的 fill 算法）。
            headers.append(name, value)?;
        }
        Ok(headers)
    }

    /// `headers.set(name, value)`。
    pub fn set(
        &mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), McpTransportError> {
        let name = name.into();
        let value = value.into();
        let (name, value) = normalize_header_pair("set", &name, &value)?;
        match self.entries.iter_mut().find(|(key, _)| *key == name) {
            Some(entry) => entry.1 = value,
            None => self.entries.push((name, value)),
        }
        Ok(())
    }

    /// `headers.append(name, value)`：同名时以 `", "` 合并。
    pub fn append(
        &mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), McpTransportError> {
        let name = name.into();
        let value = value.into();
        let (name, value) = normalize_header_pair("append", &name, &value)?;
        match self.entries.iter_mut().find(|(key, _)| *key == name) {
            Some(entry) => {
                entry.1.push_str(", ");
                entry.1.push_str(&value);
            }
            None => self.entries.push((name, value)),
        }
        Ok(())
    }

    /// `headers.get(name)`（大小写不敏感；缺失为 `None`）。
    ///
    /// 非法名在 TS 里抛 `TypeError`（Node 的 `Headers.get` 会先校验名字）；这里签名是
    /// `Option<&str>`，非法名读作 `None`。需要抛错语义时用 [`JsHeaders::try_get`]。
    pub fn get(&self, name: &str) -> Option<&str> {
        let name = normalize_header_name("get", name).ok()?;
        self.entries
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    }

    /// `headers.get(name)` 的可报错版本（调用方需要区分"缺失"与"非法名"时用）。
    pub fn try_get(&self, name: &str) -> Result<Option<&str>, McpTransportError> {
        let name = normalize_header_name("get", name)?;
        Ok(self
            .entries
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str()))
    }

    /// `headers.has(name)`：只判断存在，返回 `bool`（与 TS 的用法一致）。
    ///
    /// 非法名在 TS 里抛 `TypeError`；这里没有 `Result` 可返回，因此读作 `false`（本 crate 内部
    /// 的头名都是字面量常量）。需要区分"缺失"与"非法名"的调用方用 [`JsHeaders::try_get`]。
    pub fn has(&self, name: &str) -> bool {
        self.try_get(name).ok().flatten().is_some()
    }

    /// `headers.delete(name)`；非法名报 `TypeError`。
    pub fn delete(&mut self, name: &str) -> Result<(), McpTransportError> {
        let name = normalize_header_name("delete", name)?;
        self.entries.retain(|(key, _)| *key != name);
        Ok(())
    }

    /// 按头名（小写、升序）迭代，等价于 `[...headers]`。
    ///
    /// 每次调用都会把所有条目的引用复制进一个 `Vec` 再排序（不缓存顺序）；只需要遍历一次时
    /// 用它，需要插入顺序时用 [`JsHeaders::iter_insertion`]。
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        let mut sorted: Vec<&(String, String)> = self.entries.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        sorted
            .into_iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    /// 按插入序迭代（`buildHeaders` 的装配顺序）。
    pub fn iter_insertion(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    /// 是否没有任何头。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 转换成 `http::HeaderMap`；**任一条目**无法表达时整体退化成空 map。
    ///
    /// 值按 **latin1** 编码写出（WHATWG 的 `ByteString`）：`U+0080..=U+00FF` 是单字节。
    ///
    /// 失败模式：内部走 [`JsHeaders::try_to_header_map`] 再 `unwrap_or_default()`，所以坏条目
    /// 不是「被跳过」，而是让**所有**头一起消失。需要看到具体错误的调用方可以直接调用
    /// [`JsHeaders::try_to_header_map`]；想在坏条目存在时保留其余条目，只能自己用
    /// [`JsHeaders::iter`] / [`JsHeaders::iter_insertion`] 过滤后重建
    /// （`try_to_header_map` 不提供部分结果：在第一个坏条目就返回 `Err`，已转换成功的部分随之丢弃）。
    pub fn to_header_map(&self) -> HeaderMap {
        self.try_to_header_map().unwrap_or_default()
    }

    /// 转换成 `http::HeaderMap`；有无法表达的条目时返回 `TypeError`。
    ///
    /// Node 的 `Headers.set` 接受控制字符（0x01、0x7F），但 `fetch` 在发请求前会以
    /// `TypeError: fetch failed` 拒绝它们（undici 的 `invalid <name> header`，已实测）。
    /// 这里同样在**上线时**才失败，而不是在 `set` 时静默丢弃。
    ///
    /// 在**第一个**无法表达的条目上就返回：错误名是 `TypeError`，消息固定为 `fetch failed`
    /// （只保留 Node 的通用前缀，具体 cause 不建模）。调用方拿不到已经转换成功的部分。
    pub fn try_to_header_map(&self) -> Result<HeaderMap, McpTransportError> {
        let mut map = HeaderMap::new();
        for (name, value) in &self.entries {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| McpTransportError::network("fetch failed"))?;
            let header_value = HeaderValue::from_bytes(&latin1_bytes(value))
                .map_err(|_| McpTransportError::network("fetch failed"))?;
            map.append(header_name, header_value);
        }
        Ok(map)
    }

    /// 从 `http::HeaderMap` 建（响应头；同名多值以 `", "` 合并，与 JS 一致）。
    ///
    /// 头字节按 **latin1** 解码（Node/undici 的行为），而不是 UTF-8。
    pub fn from_header_map(map: &HeaderMap) -> Self {
        let mut headers = JsHeaders::new();
        for (name, value) in map.iter() {
            let value: String = value
                .as_bytes()
                .iter()
                .map(|byte| char::from(*byte))
                .collect();
            let _ = headers.append(name.as_str().to_owned(), value);
        }
        headers
    }
}

/// `String` → WHATWG `ByteString`（latin1）的字节。
///
/// 只需要处理 `<= U+00FF`；更大的码位在写入时已被拒绝。
///
/// 这条不变量由所有写入口共同维持：`set`/`append` 先过 `byte_string`，`from_header_map`
/// 逐字节构造。超过 `U+00FF` 的字符若真到了这里，`as u8` 会**截断**而不是报错。
fn latin1_bytes(value: &str) -> Vec<u8> {
    if value.is_ascii() {
        return value.as_bytes().to_vec();
    }
    value.chars().map(|ch| ch as u32 as u8).collect()
}

/// HTTP 名字的合法字符：RFC 9110 `token`。
fn is_token_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

/// `headers.<method>` 的名字规范：先做 `ByteString` 转换（`> U+00FF` 抛），再校验 token。
///
/// WHATWG 的 WebIDL 转换对 `name` / `value` 都按 **ByteString** 处理，且顺序是**参数顺序**
/// （先 `name` 后 `value`），之后才做名字/值的合法性检查。Node 26 实测：
/// - `set("bad name", "a\nb")` → 名字错（两个 ByteString 都通过，名字检查在前）；
/// - `set("中", "😀")` → 报 `中` 的索引 0（名字的 ByteString 先失败）；
/// - `set("bad name", "中")` → 报 `中` 的 ByteString 错（名字的 ByteString 通过、值的不通过，
///   还没走到名字检查）。
fn normalize_header_name(method: &str, name: &str) -> Result<String, McpTransportError> {
    byte_string(name)?;
    if name.is_empty() || !name.bytes().all(is_token_char) {
        return Err(McpTransportError::network(format!(
            "Headers.{method}: \"{name}\" is an invalid header name."
        )));
    }
    Ok(name.to_ascii_lowercase())
}

/// `set`/`append` 的完整校验顺序（见 [`normalize_header_name`] 的文档）。
///
/// 返回 `(小写名字, 裁剪后的值)`。
fn normalize_header_pair(
    method: &str,
    name: &str,
    value: &str,
) -> Result<(String, String), McpTransportError> {
    // 1) 两个参数按**参数顺序**做 ByteString 转换。
    let name_bytes = byte_string(name)?;
    let value_bytes = byte_string(value)?;
    // 2) 名字校验。
    if name_bytes.is_empty() || !name_bytes.iter().copied().all(is_token_char) {
        return Err(McpTransportError::network(format!(
            "Headers.{method}: \"{name}\" is an invalid header name."
        )));
    }
    // 3) 值校验（裁剪首尾 HTTP 空白后再查 `\n`/`\r`/`\0`）。
    let value: String = value_bytes.iter().map(|byte| char::from(*byte)).collect();
    let trimmed = value.trim_matches(|ch| matches!(ch, ' ' | '\t' | '\r' | '\n'));
    if trimmed.contains(['\n', '\r', '\0']) {
        return Err(McpTransportError::network(format!(
            "Headers.{method}: \"{trimmed}\" is an invalid header value."
        )));
    }
    Ok((name.to_ascii_lowercase(), trimmed.to_owned()))
}

/// WHATWG `ByteString` 转换：任一码位 `> U+00FF` 即 `TypeError`。
///
/// 返回按 UTF-16 索引的原始码位（错误消息里要报 `index` 与 `value`）。
///
/// 错误里的 `index` 按 **UTF-16 代码单元**计（JS 字符串的索引），不是 Unicode 码位：非 BMP
/// 字符（如 emoji）报的是其高代理项的位置与值（`😀` → index 2、value 55357）。
fn byte_string(value: &str) -> Result<Vec<u8>, McpTransportError> {
    let mut bytes = Vec::with_capacity(value.len());
    for (index, unit) in value.encode_utf16().enumerate() {
        if unit > 0xff {
            return Err(McpTransportError::network(format!(
                "Cannot convert argument to a ByteString because the character at index {index} \
                 has a value of {unit} which is greater than 255."
            )));
        }
        bytes.push(unit as u8);
    }
    Ok(bytes)
}

/// `headers.get(name)`，同名多值用 `", "` 连接（WHATWG `Headers.get` 的合并语义）。
///
/// `HeaderMap` 里的字节按 latin1 解码（Node/undici 的行为）。
pub fn header_joined(headers: &HeaderMap, name: &str) -> Option<String> {
    let mut values = headers.get_all(name).iter();
    let first = values.next()?;
    let mut joined: String = first
        .as_bytes()
        .iter()
        .map(|byte| char::from(*byte))
        .collect();
    for value in values {
        joined.push_str(", ");
        joined.extend(value.as_bytes().iter().map(|byte| char::from(*byte)));
    }
    Some(joined)
}

/// `contentType(response)`：取 `content-type`，按 `;` 截断、trim、小写。
///
/// 这里的 `trim` / 小写是 **Rust** 的（`str::trim()` 与 `to_ascii_lowercase()`），与 JS 的
/// `trim()` / `toLowerCase()` 集合不同：U+0085（NEL）会被 Rust trim 掉而 JS 不会，`À`–`Þ`
/// 在 JS 里会小写而这里不会。需要逐字对齐 JS 的调用方（`crate::transports::streamable_http`）
/// 因此不使用本函数，而是自己实现 `content_type`。
pub fn content_type_header(headers: &HeaderMap) -> Option<String> {
    let raw = header_joined(headers, "content-type")?;
    let first = raw.split(';').next().unwrap_or("");
    Some(first.trim().to_ascii_lowercase())
}

// ---------------------------------------------------------------------------
// fetch
// ---------------------------------------------------------------------------

/// `RequestInit` 的 Rust 形态。
///
/// 只建模本 crate 用到的字段：`method`、`headers`、`body`、`signal`，另加 `fetch` 不支持、
/// 但 `AbortSignal.timeout(1000)` 需要的 `timeout`（`close()` 的 DELETE）。
#[derive(Debug, Clone, Default)]
pub struct FetchInit {
    /// HTTP 方法（`GET`/`POST`/`DELETE`）。
    pub method: Method,
    /// 请求头（JS `Headers` 语义）。
    pub headers: JsHeaders,
    /// 请求体；`None` 表示无体。
    pub body: Option<FetchBody>,
    /// 取消信号（TS 的 `signal`）。
    pub signal: Option<CancellationToken>,
    /// 整个交换（**含响应体读取**）的超时；`AbortSignal.timeout(ms)` 的等价物。
    ///
    /// [`ReqwestFetch`] 把它换成一个绝对截止时刻装到 [`FetchResponse`] 上，因此
    /// [`FetchResponse::text`]/[`FetchResponse::bytes`] 也会在到期时报
    /// `TimeoutError`（TS 的 signal 会中止体读取）。
    ///
    /// 这是一个**总**预算而不是两段各一份：截止时刻在发请求前算出，响应头阶段用掉的时间会
    /// 从体读取的剩余时间里扣掉（见模块文档「取消、超时与失败模式」）。
    pub timeout: Option<Duration>,
}

impl FetchInit {
    /// `fetch(url)`：GET、无体。
    pub fn get() -> Self {
        FetchInit::default()
    }

    /// `fetch(url, { method })`。
    pub fn with_method(method: Method) -> Self {
        FetchInit {
            method,
            ..FetchInit::default()
        }
    }

    /// 设置一个请求头（等价 `headers.set`）。
    ///
    /// 注意：`JsHeaders::set` 会失败（非法名/值），但 builder 没有 `Result`，这里用 `let _`
    /// **静默丢弃**该头。要让非法头报错的调用方必须自己构造 [`JsHeaders`] 再
    /// [`FetchInit::with_headers`]（`oauth::discovery` 就是这么做的）。
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let _ = self.headers.set(name, value);
        self
    }

    /// 整体替换请求头。
    pub fn with_headers(mut self, headers: JsHeaders) -> Self {
        self.headers = headers;
        self
    }

    /// 设置请求体。
    pub fn with_body(mut self, body: FetchBody) -> Self {
        self.body = Some(body);
        self
    }

    /// 设置取消令牌。
    pub fn with_signal(mut self, signal: Option<CancellationToken>) -> Self {
        self.signal = signal;
        self
    }

    /// 设置超时。
    pub fn with_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }
}

/// `RequestInit.body` 的三种实际形态。
#[derive(Debug, Clone)]
pub enum FetchBody {
    /// 字符串体（UTF-8 编码后发送）。
    Text(String),
    /// 原始字节。
    Bytes(Vec<u8>),
    /// `URLSearchParams`：按 `application/x-www-form-urlencoded` 序列化
    /// （`fetch` 会同时把它自己设成 `content-type: application/x-www-form-urlencoded;charset=UTF-8`）。
    Form(Vec<(String, String)>),
}

impl FetchBody {
    /// 编码成线上字节。
    ///
    /// `Text` 按 UTF-8 编码；`Form` 逐对写出 `name=value`，用 `&` 连接，按
    /// `application/x-www-form-urlencoded` 规则做百分号编码（非 ASCII 先按 UTF-8 拆字节，
    /// 再逐字节编码；空格写成 `+`），结果不带前导 `?`。
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            FetchBody::Text(text) => text.as_bytes().to_vec(),
            FetchBody::Bytes(bytes) => bytes.clone(),
            FetchBody::Form(pairs) => {
                let mut out = String::new();
                for (index, (name, value)) in pairs.iter().enumerate() {
                    if index > 0 {
                        out.push('&');
                    }
                    encode_form_component(&mut out, name);
                    out.push('=');
                    encode_form_component(&mut out, value);
                }
                out.into_bytes()
            }
        }
    }

    /// `fetch` 为请求体自动补的 `content-type`（调用方没设时才补）。
    ///
    /// Node 26 实测：字符串体 → `text/plain;charset=UTF-8`，`URLSearchParams` →
    /// `application/x-www-form-urlencoded;charset=UTF-8`；`ArrayBuffer`/TypedArray 不补
    /// （与 `FetchBody::Bytes` 对应）。
    pub fn content_type(&self) -> Option<&'static str> {
        match self {
            FetchBody::Form(_) => Some("application/x-www-form-urlencoded;charset=UTF-8"),
            FetchBody::Text(_) => Some("text/plain;charset=UTF-8"),
            FetchBody::Bytes(_) => None,
        }
    }
}

/// `URLSearchParams` 的百分号编码（§22.7）：除 `A-Za-z0-9*-._` 外全部编码，空格写 `+`。
///
/// 注意 `~!*'()`：只有 `*` 不编码（`"~!*'()" → "%7E%21*%27%28%29"`）。
fn encode_form_component(out: &mut String, component: &str) {
    for byte in component.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                out.push(*byte as char);
            }
            b' ' => out.push('+'),
            other => {
                out.push('%');
                out.push_str(&format!("{other:02X}"));
            }
        }
    }
}

/// 响应体字节流（按到达顺序；错误时给出传输错误）。
pub type FetchByteStream =
    std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<Vec<u8>, McpTransportError>> + Send>>;

/// `Response` 的 Rust 形态：状态、状态文本、头、**流式**体。
///
/// 只建模本 crate 用到的字段（`status`、`statusText`、`headers`、体、体是否已读）；
/// `url`、`redirected`、`type` 等其余 `Response` 属性没有对应物。
///
/// `fetch` 在响应头到达时即 resolve，体按需读取（SSE 场景必须流式）。与 Node 的 `Response`
/// 一致，体只能读一次：读过的响应再读会报 `TypeError("Body is unusable: Body has already been
/// read")`，`json()` 的解析失败是 `SyntaxError`。
pub struct FetchResponse {
    status: u16,
    status_text: String,
    headers: HeaderMap,
    body: Option<FetchByteStream>,
    /// 体是否已被读取（`Response.bodyUsed` 的等价物）。
    consumed: bool,
    /// 取消信号（体读取也受它约束）。
    signal: Option<CancellationToken>,
    /// 超时的绝对时刻（`AbortSignal.timeout` 会中止体读取）。
    deadline: Option<tokio::time::Instant>,
}

impl FetchResponse {
    /// 用状态、头与体流构造（`statusText` 为空）。
    pub fn new(status: u16, headers: HeaderMap, body: FetchByteStream) -> Self {
        FetchResponse {
            status,
            status_text: String::new(),
            headers,
            body: Some(body),
            consumed: false,
            signal: None,
            deadline: None,
        }
    }

    /// 用整块字节构造（测试与无流场景）。
    pub fn from_bytes(status: u16, headers: HeaderMap, bytes: Vec<u8>) -> Self {
        FetchResponse {
            status,
            status_text: String::new(),
            headers,
            body: Some(single_chunk_stream(bytes)),
            consumed: false,
            signal: None,
            deadline: None,
        }
    }

    /// 带上 `statusText`（`Response.statusText` 的原值）。
    ///
    /// 子类/测试构造响应时用它给出真实短语。[`ReqwestFetch`] 从状态行的原因短语取值（见
    /// [`FetchResponse::status_text`]）。
    pub fn with_status_text(mut self, status_text: impl Into<String>) -> Self {
        self.status_text = status_text.into();
        self
    }

    /// 让体读取也受取消信号与超时约束（`AbortSignal`/`AbortSignal.timeout` 会中止体读取）。
    ///
    /// [`ReqwestFetch`] 用它在返回响应前把这两个约束装上；不装时体读取不受限（测试与
    /// 无流场景）。
    pub fn with_abort(
        mut self,
        signal: Option<CancellationToken>,
        deadline: Option<tokio::time::Instant>,
    ) -> Self {
        self.signal = signal;
        self.deadline = deadline;
        self
    }

    /// `response.status`。
    pub fn status(&self) -> u16 {
        self.status
    }

    /// `response.statusText`：状态行里的原因短语（`ReqwestFetch` 从 hyper 的
    /// `ReasonPhrase` 扩展取，缺失时用注册短语；见模块文档）。
    pub fn status_text(&self) -> &str {
        &self.status_text
    }

    /// `response.ok`（200–299）。
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// `response.headers`。
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// `response.headers.get(name)`（同名多值以 `", "` 合并）。
    pub fn header(&self, name: &str) -> Option<String> {
        header_joined(&self.headers, name)
    }

    /// `contentType(response)`。
    ///
    /// 语义与 [`content_type_header`] 相同（Rust 的 `trim` 与 ASCII 小写，**不是** JS 的
    /// `trim()` / `toLowerCase()`）。
    pub fn content_type(&self) -> Option<String> {
        content_type_header(&self.headers)
    }

    /// 取走体流（SSE / 一次性读取）。取走后其余读取方法报 `TypeError`（体已不可用）。
    ///
    /// 与 [`FetchResponse::bytes`] 不同，它不置 `consumed`；但 `body` 已经是 `None`，因此
    /// `bytes`/`text`/`json` 会走 `body_unusable()` 分支。
    pub fn take_stream(&mut self) -> Option<FetchByteStream> {
        self.body.take()
    }

    /// 是否还有未读取的体。
    ///
    /// 只反映本结构是否还持有流，**不**模拟 `fetch` 的「无体状态」（101/103/204/205/304 在
    /// Node 里 `response.body === null`）；需要那个判定时看 `streamable_http` 的
    /// `response_has_body`。
    pub fn has_body(&self) -> bool {
        self.body.is_some()
    }

    /// `await response.arrayBuffer()`。
    ///
    /// 失败模式：流在等待读取**之前**就被取走，且 `consumed` 立即置位。因此取消、超时或流错误
    /// 之后体已永久不可用，再读只会得到 `body_unusable`（`TypeError`），不能重试。
    pub async fn bytes(&mut self) -> Result<Vec<u8>, McpTransportError> {
        if self.consumed || self.body.is_none() {
            return Err(McpTransportError::body_unusable());
        }
        let Some(stream) = self.body.take() else {
            unreachable!("checked above");
        };
        self.consumed = true;
        collect_stream(stream, self.signal.clone(), self.deadline).await
    }

    /// `await response.text()`：UTF-8 解码（非法序列替换为 U+FFFD），并去掉开头的 BOM。
    pub async fn text(&mut self) -> Result<String, McpTransportError> {
        let bytes = self.bytes().await?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        Ok(text
            .strip_prefix('\u{feff}')
            .map(str::to_owned)
            .unwrap_or(text))
    }

    /// `await response.json()`：`JSON.parse(await text())`（V8 语义，见 `kiana_json::js_parse`）。
    ///
    /// 解析失败是 `SyntaxError`（Node 的 `Response.json()` 正是如此），而不是普通 `Error`。
    pub async fn json(&mut self) -> Result<Value, McpTransportError> {
        let text = self.text().await?;
        kiana_json::js_parse::parse(&text).map_err(|error| McpTransportError::syntax(error.message))
    }

    /// `discard(response)`：`response.body?.cancel().catch(() => {})`。
    ///
    /// Rust 中 drop 流即取消底层连接，因此这里只丢弃体。
    ///
    /// 它不置 `consumed`，但 `body` 已为 `None`；之后任何读取仍报
    /// `TypeError("Body is unusable: Body has already been read")`
    /// （`tests/fetch.rs` 的 `response_body_semantics_match_node` 钉住了这一点）。
    pub async fn discard(&mut self) {
        self.body = None;
    }
}

/// 读完整个字节流，期间响应取消信号与超时（与 `AbortSignal` 中止体读取一致）。
///
/// 分三层叠加，顺序即嵌套顺序：
/// 1. 内层把流排空进 `Vec<u8>`，流的错误原样向上传播（它已经是 [`McpTransportError`]）；
/// 2. 中层用绝对截止时刻 `timeout_at`，到点报 `TimeoutError`；
/// 3. 外层用 `biased` 的 `select!` 让取消优先，命中时报 `AbortError`。
///
/// 内层用 `Option<Vec<u8>>` 记录结果：成功完成（`Ready(None)`）后再被 poll 会命中
/// `expect("polled after completion")`；错误完成（`Ready(Some(Err(_)))`）时 `collected`
/// 仍为 `Some`，再次 poll 会继续调用 `poll_next` 驱动流而不会 panic。两种情况都依赖
/// `Future` 的「完成后不再 poll」契约，这里不自己实现幂等。
async fn collect_stream(
    stream: FetchByteStream,
    signal: Option<CancellationToken>,
    deadline: Option<tokio::time::Instant>,
) -> Result<Vec<u8>, McpTransportError> {
    // 阶段 1：把响应体流排空到内存。每个 chunk 追加，`Ready(None)` 时取走结果；
    // 流返回的错误直接上抛，不再包装。
    let read_all = async move {
        let mut stream = stream;
        let mut collected: Option<Vec<u8>> = Some(Vec::new());
        std::future::poll_fn(move |cx| {
            loop {
                let buffer = collected.as_mut().expect("polled after completion");
                match stream.as_mut().poll_next(cx) {
                    std::task::Poll::Ready(Some(Ok(chunk))) => buffer.extend_from_slice(&chunk),
                    std::task::Poll::Ready(Some(Err(error))) => {
                        return std::task::Poll::Ready(Err(error));
                    }
                    std::task::Poll::Ready(None) => {
                        let done = collected.take().expect("polled after completion");
                        return std::task::Poll::Ready(Ok(done));
                    }
                    std::task::Poll::Pending => return std::task::Poll::Pending,
                }
            }
        })
        .await
    };

    // 体读取同样受超时与取消约束：`AbortSignal.timeout` 会中止整个交换，不只是响应头。
    // 阶段 2：绝对截止时刻。它与 `ReqwestFetch::fetch` 里约束响应头的是同一个时刻，
    // 因此体读取只拿剩余预算。
    let with_deadline = async {
        match deadline {
            Some(deadline) => tokio::time::timeout_at(deadline, read_all)
                .await
                .map_err(|_| McpTransportError::timed_out())?,
            None => read_all.await,
        }
    };

    // 阶段 3：取消优先。`biased` 让取消分支先被 poll，因此读取刚好完成与取消同时发生时
    // 报的是 `AbortError`（体已经被取走，结果被丢弃）。
    match signal {
        Some(signal) => tokio::select! {
            biased;
            _ = signal.cancelled() => Err(McpTransportError::aborted()),
            result = with_deadline => result,
        },
        None => with_deadline.await,
    }
}
/// 单块字节流（`FetchResponse::from_bytes` 用）。
fn single_chunk_stream(bytes: Vec<u8>) -> FetchByteStream {
    Box::pin(SingleChunk { bytes: Some(bytes) })
}

/// 只产出一块的流。
struct SingleChunk {
    bytes: Option<Vec<u8>>,
}

impl futures_core::Stream for SingleChunk {
    type Item = Result<Vec<u8>, McpTransportError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::task::Poll::Ready(self.bytes.take().map(Ok))
    }
}

/// `reqwest` 的字节流 → [`FetchByteStream`]。
struct ReqwestByteStreamAdapter<S> {
    inner: S,
}

impl<S, B> futures_core::Stream for ReqwestByteStreamAdapter<S>
where
    S: futures_core::Stream<Item = Result<B, reqwest::Error>> + Send + Unpin,
    B: AsRef<[u8]>,
{
    type Item = Result<Vec<u8>, McpTransportError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match std::pin::Pin::new(&mut self.inner).poll_next(cx) {
            std::task::Poll::Ready(Some(Ok(bytes))) => {
                std::task::Poll::Ready(Some(Ok(bytes.as_ref().to_vec())))
            }
            std::task::Poll::Ready(Some(Err(error))) => {
                std::task::Poll::Ready(Some(Err(McpTransportError::from(error))))
            }
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

/// `McpFetch`：可替换的 HTTP 栈（TS 的 `type McpFetch = (input, init) => Promise<Response>`）。
///
/// 必须是对象安全的（`Arc<dyn McpFetch>`）：`streamable-http` 把 `Arc<dyn McpFetch>` 交给
/// `AuthProvider::on_unauthorized`，OAuth 也用它做发现与令牌请求。
///
/// 与 TS 一样**不带接收者**调用：Rust 的 `Arc<dyn McpFetch>` 天然没有 `this`，
/// 因此不存在 Cloudflare Workers 的 "Illegal invocation" 问题（§30.19）。
///
/// 实现者的契约：`fetch` 返回时只代表**响应头**已到，响应体在 [`FetchResponse`] 里按需读取；
/// `init.signal` 与 `init.timeout` 还必须约束体读取（默认实现用
/// [`FetchResponse::with_abort`] 装上）。不装时 [`FetchResponse::bytes`] 的读取不会被取消或
/// 超时打断，只能等流自己结束或上层直接丢弃响应。
// `async-trait` 会给每个方法生成 `#[must_use]`，clippy 因此报 `double_must_use`；
// 这是宏产物，不是本 crate 能改的（`ARCHITECTURE.md` §4.10 的 0-warning 门槛要求显式豁免）。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait McpFetch: Send + Sync {
    /// 发起请求；响应头到达即返回（体是流）。
    async fn fetch(&self, url: &Url, init: FetchInit) -> Result<FetchResponse, McpTransportError>;
}

/// 默认 `McpFetch`：`reqwest` + rustls。
///
/// 行为对齐 `fetch`：默认跟随重定向、自动解压（gzip/deflate/brotli）、无整体超时
/// （除非 [`FetchInit::timeout`]）、网络失败报成 `TypeError` 等价物。
#[derive(Debug, Clone)]
pub struct ReqwestFetch {
    client: reqwest::Client,
}

/// 默认最多跟随的重定向次数（与 reqwest/Node `fetch` 的 10 次一致）。
pub const DEFAULT_MAX_REDIRECTS: usize = 10;

/// 重定向重新校验：新 URL 必须是 http/https；不许把 HTTPS 降级到非 loopback 的 HTTP。
///
/// 返回 `Err` 时策略拒绝跟随（reqwest 把该文案包成请求错误）。跨**源**（scheme/host/
/// port 任一不同）时 reqwest 自己会剥离 `Authorization`/`Cookie`/`Proxy-Authorization`/
/// `Www-Authenticate`，这里只需要补充“目标是否可接受”的校验。保留 loopback 的
/// HTTPS→HTTP 例外是为了本地开发服务器。
fn validate_redirect(previous: &[Url], next: &Url) -> Result<(), &'static str> {
    if previous.len() >= DEFAULT_MAX_REDIRECTS {
        return Err("too many redirects");
    }
    if !matches!(next.scheme(), "http" | "https") {
        return Err("redirect target must use http or https");
    }
    let downgrades = previous
        .last()
        .is_some_and(|last| last.scheme() == "https" && next.scheme() == "http");
    if downgrades && !is_loopback_host(next) {
        return Err("refusing to follow an HTTPS-to-HTTP redirect");
    }
    Ok(())
}

/// 主机是否是 loopback（`localhost` / `127.0.0.0/8` / `::1`）。
fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

/// `fetch` 的默认重定向策略：每跳重新校验，跨源敏感头由 reqwest 剥离。
fn default_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if let Err(message) = validate_redirect(attempt.previous(), attempt.url()) {
            return attempt.error(message);
        }
        attempt.follow()
    })
}

impl Default for ReqwestFetch {
    fn default() -> Self {
        // `ClientBuilder::build` 只在 TLS 后端初始化失败时报错（极罕见）；退回
        // `Client::new()` 仍保留 reqwest 的跨源头剥离，只是失去自定义策略。
        let client = reqwest::Client::builder()
            .redirect(default_redirect_policy())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        ReqwestFetch { client }
    }
}

impl ReqwestFetch {
    /// 用默认配置构造。
    pub fn new() -> Self {
        ReqwestFetch::default()
    }

    /// 复用调用方给的 `reqwest::Client`（代理/超时由嵌入方统一装配）。
    pub fn from_client(client: reqwest::Client) -> Self {
        ReqwestFetch { client }
    }

    /// 共享句柄，可直接塞进 [`UnauthorizedContext::fetch`]。
    pub fn shared(self) -> Arc<dyn McpFetch> {
        Arc::new(self)
    }

    /// 底层 client 的只读访问（测试断言代理配置时用）。
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }
}

#[async_trait]
impl McpFetch for ReqwestFetch {
    async fn fetch(&self, url: &Url, init: FetchInit) -> Result<FetchResponse, McpTransportError> {
        // `fetch` 的规范：GET/HEAD 不允许带体（Node 抛 `TypeError`）。
        if init.body.is_some() && matches!(init.method, Method::GET | Method::HEAD) {
            return Err(McpTransportError::network(
                "Request with GET/HEAD method cannot have body.",
            ));
        }
        let method = reqwest::Method::from_bytes(init.method.as_str().as_bytes())
            .map_err(|error| McpTransportError::network(error.to_string()))?;
        let mut headers = init.headers.clone();
        // fetch 的请求体提取规则：`Text`/`Form` 在调用方没设 `content-type` 时补上默认值，
        // `Bytes` 不补（见 [`FetchBody::content_type`]）。
        if let Some(content_type) = init
            .body
            .as_ref()
            .and_then(FetchBody::content_type)
            .filter(|_| !headers.has("content-type"))
        {
            let _ = headers.set("content-type", content_type);
        }
        // Node 的 `fetch` 默认头（本机 Node 26 抓包实测）：`accept: */*`、
        // `accept-language: *`、`sec-fetch-mode: cors`、`user-agent: node`、
        // `accept-encoding: gzip, deflate`。reqwest 只自己补 `accept: */*`（值相同），
        // 其余都不设，服务器能观察到差别，所以这里按 Node 的默认值补齐（调用方显式设置时保留
        // 其值；插入顺序也按 Node 的抓包顺序）。评审 finding 7：修复前漏了 accept-language 与
        // sec-fetch-mode。
        if !headers.has("accept") {
            let _ = headers.set("accept", "*/*");
        }
        if !headers.has("accept-language") {
            let _ = headers.set("accept-language", "*");
        }
        if !headers.has("sec-fetch-mode") {
            let _ = headers.set("sec-fetch-mode", "cors");
        }
        if !headers.has("user-agent") {
            let _ = headers.set("user-agent", "node");
        }
        if !headers.has("accept-encoding") {
            let _ = headers.set("accept-encoding", "gzip, deflate");
        }
        // 头在这里才做最终校验：控制字符会在这里变成 `TypeError`（见 `try_to_header_map`）。
        let mut request = self
            .client
            .request(method, url.clone())
            .headers(headers.try_to_header_map()?);
        if let Some(body) = &init.body {
            request = request.body(body.to_bytes());
        }

        // 截止时刻在发请求前算好；下面的 `timeout(timeout, send)` 与它同源，响应头阶段花掉的
        // 时间会从体读取的剩余预算里扣掉。
        // `AbortSignal.timeout(ms)` 的截止时刻（绝对时刻，装到响应上约束体读取）。
        let deadline = init
            .timeout
            .map(|timeout| tokio::time::Instant::now() + timeout);
        let send = async {
            let response = request.send().await?;
            let status = response.status().as_u16();
            // `statusText`：hyper 在状态行的短语与注册短语不同（或为空）时把它放进
            // `extensions` 的 `ReasonPhrase`；与注册短语相同则不放。
            let status_text = match response.extensions().get::<hyper::ext::ReasonPhrase>() {
                Some(reason) => String::from_utf8_lossy(reason.as_bytes()).into_owned(),
                None => response
                    .status()
                    .canonical_reason()
                    .map(str::to_owned)
                    .unwrap_or_default(),
            };
            let headers = response.headers().clone();
            let stream: FetchByteStream = Box::pin(ReqwestByteStreamAdapter {
                inner: response.bytes_stream(),
            });
            Ok::<FetchResponse, McpTransportError>(
                FetchResponse::new(status, headers, stream)
                    .with_status_text(status_text)
                    .with_abort(init.signal.clone(), deadline),
            )
        };

        // 超时同时约束 `send()`（响应头）与响应体读取：`AbortSignal.timeout` 中止整个交换，
        // 错误是 `DOMException(name = "TimeoutError")`。
        let with_timeout = async {
            match init.timeout {
                Some(timeout) => tokio::time::timeout(timeout, send)
                    .await
                    .map_err(|_| McpTransportError::timed_out())?,
                None => send.await,
            }
        };

        match &init.signal {
            Some(signal) => tokio::select! {
                biased;
                _ = signal.cancelled() => Err(McpTransportError::aborted()),
                result = with_timeout => result,
            },
            None => with_timeout.await,
        }
    }
}

/// `UnauthorizedContext`：401（或要求扩大 scope 的 403）被交给认证提供方时的上下文。
///
/// TS 传的是活的 `Response` 与 `fetch`；Rust 侧把体读成 `body`（传输层随后丢弃它），
/// 因此提供方可以像 `context.fetch(url)` 一样用同一个 [`McpFetch`] 再发请求。
pub struct UnauthorizedContext {
    /// 被拒响应的状态（401，或带 `insufficient_scope` 的 403）。
    pub status: u16,
    /// 被拒响应的头（含 `www-authenticate`）。
    pub headers: HeaderMap,
    /// 被拒响应的完整响应体（未截断，§12）。
    pub body: Vec<u8>,
    /// MCP 服务器 URL。
    pub server_url: Url,
    /// 与传输相同的 `fetch` 句柄。
    pub fetch: Arc<dyn McpFetch>,
    /// 被拒请求携带的令牌；`None` 表示没有（或为空串，§30.14）。
    pub token: Option<String>,
}

impl UnauthorizedContext {
    /// `headers.get(name)`，多值以 `", "` 合并。
    pub fn header(&self, name: &str) -> Option<String> {
        header_joined(&self.headers, name)
    }

    /// 被拒响应的体按 UTF-8 解码（`await response.text()`）。
    ///
    /// 与 [`FetchResponse::text`] 的差别：这里**不**剥离开头的 U+FEFF（BOM）。非法 UTF-8
    /// 序列同样替换为 U+FFFD。
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// 提供 bearer token，并在 401 后刷新（`auth-provider.ts` 的 `AuthProvider`）。
///
/// `token()` 可能走网络刷新，因此传输只在**需要等待答复**的请求（POST）之前调用它；
/// `close()` 必须**不**调用（复用最后一个请求的 token）。
// 同 `McpFetch`：`async-trait` 生成的 `#[must_use]` 触发 `double_must_use`。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait AuthProvider: Send + Sync {
    /// 下一个请求用的令牌（可能触发刷新）。
    ///
    /// 签名没有错误通道：实现读取令牌失败时只能返回 `None`（与「没有令牌」不可区分）。
    /// `crate::oauth` 的适配器就是这么降级的，见 `oauth::flow` 里 `AdaptedOAuthProvider::token`
    /// 的文档。
    async fn token(&self) -> Option<String>;

    /// 收到 401（或 `insufficient_scope` 403）时调用一次；返回 `Err` 会把错误抛给请求方。
    ///
    /// 缺省实现是 no-op（TS 的 `onUnauthorized?` 可选）。**覆盖本方法的类型必须同时让
    /// [`Self::has_on_unauthorized`] 返回 `true`**，否则传输视作「没有处理器」，401 不会重试。
    async fn on_unauthorized(
        &self,
        _context: UnauthorizedContext,
    ) -> Result<(), McpTransportError> {
        Ok(())
    }

    /// 是否实现了 [`Self::on_unauthorized`]（TS 的 `onUnauthorized !== undefined`）。
    ///
    /// TS 里 `onUnauthorized` 是可选的：只有 `token()` 的 provider 收到的 401 会**直接**返回，
    /// 不重试。Rust 的缺省 `on_unauthorized` 是 no-op，无法从行为上区分「没实现」与「实现了但
    /// 什么都不做」，而后者在 TS 里会重试一次（§17.6）。实现 `on_unauthorized` 的类型必须同时
    /// 覆盖本方法返回 `true`（否则传输按「没有处理器」处理，不会重试）。
    fn has_on_unauthorized(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_headers_trim_only_outer_whitespace() {
        let mut headers = JsHeaders::new();
        headers.set("X-Test", "  x   y  ").unwrap();
        assert_eq!(headers.get("x-test"), Some("x   y"));
        headers.set("X-Tab", "a\tb").unwrap();
        assert_eq!(headers.get("x-tab"), Some("a\tb"));
    }

    #[test]
    fn js_headers_set_is_case_insensitive_and_keeps_position() {
        let mut headers = JsHeaders::new();
        headers.set("A", "1").unwrap();
        headers.set("B", "2").unwrap();
        headers.set("a", "3").unwrap();
        assert_eq!(
            headers.iter_insertion().collect::<Vec<_>>(),
            vec![("a", "3"), ("b", "2")]
        );
    }

    #[test]
    fn js_headers_append_joins_with_comma_space() {
        let mut headers = JsHeaders::new();
        headers.append("X", "v1").unwrap();
        headers.append("X", "v2").unwrap();
        assert_eq!(headers.get("x"), Some("v1, v2"));
    }

    #[test]
    fn js_headers_reject_invalid_names_and_values() {
        let mut headers = JsHeaders::new();
        assert!(headers.set("bad name", "1").is_err());
        assert!(headers.set("X", "a\nb").is_err());
        assert!(headers.set("X", "a\0b").is_err());
        assert!(headers.set("", "1").is_err());
        assert!(headers.set("X-Ok", "v").is_ok());
    }

    /// 回归测试（评审 finding #4，major）：WHATWG `Headers` 的 ByteString 与错误文案，
    /// 全部对照本机 Node 26 实测结果。
    #[test]
    fn js_headers_match_whatwg_bytestring_and_error_messages() {
        // > U+00FF 抛 TypeError（消息带 UTF-16 索引与码位值）。
        let error = JsHeaders::new().set("x", "中").unwrap_err();
        assert_eq!(error.name(), "TypeError");
        assert_eq!(
            error.to_string(),
            "Cannot convert argument to a ByteString because the character at index 0 has a \
             value of 20013 which is greater than 255."
        );
        let error = JsHeaders::new().set("x", "aé😀").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("at index 2 has a value of 55357")
        );

        // U+0080..=U+00FF 合法，且按 latin1 单字节上线。
        let mut headers = JsHeaders::new();
        headers.set("x", "café").unwrap();
        assert_eq!(headers.get("x"), Some("café"));
        let map = headers.to_header_map();
        assert_eq!(map.get("x").unwrap().as_bytes(), b"caf\xe9");

        // 0x01 / 0x7F 在 Headers 层合法（undici 稍后在 fetch 时才拒），不能静默丢弃：
        // `try_to_header_map` 在上线时报 `TypeError`。
        let mut headers = JsHeaders::new();
        headers.set("x", "a\x01b").unwrap();
        assert_eq!(headers.get("x"), Some("a\x01b"));
        let error = headers.try_to_header_map().unwrap_err();
        assert_eq!(error.name(), "TypeError");

        // 非法名的错误文案与 `Headers.<method>` 前缀。
        let error = JsHeaders::new().set("bad name", "1").unwrap_err();
        assert_eq!(
            error.to_string(),
            "Headers.set: \"bad name\" is an invalid header name."
        );
        assert_eq!(
            JsHeaders::new().get("bad name").map(str::to_owned),
            None,
            "get on an invalid name returns None (use try_get for the error)"
        );
        assert!(JsHeaders::new().try_get("bad name").is_err());
        let mut headers = JsHeaders::new();
        assert!(headers.delete("bad name").is_err());

        // 非法值：先裁剪首尾空白再检查（`"  a\nb  "` 报的是裁剪后的值）。
        let error = JsHeaders::new().set("x", "  a\nb  ").unwrap_err();
        assert_eq!(
            error.to_string(),
            "Headers.set: \"a\nb\" is an invalid header value.",
            "message keeps the real newline (probed with Node)"
        );

        // 校验顺序（评审 finding #2，major）：WebIDL 按**参数顺序**先对 name 再做 value 的
        // ByteString 转换，两个都通过后才做名字检查、最后值检查。Node 26 实测：
        // - `set("bad name", "a\nb")` → 名字错（ByteString 都过，名字检查在前）；
        // - `set("中", "😀")` → 报 name 的索引 0（20013），不是 value 的 55357；
        // - `set("bad name", "中")` → 报 value 的 ByteString 错（还没走到名字检查）。
        let error = JsHeaders::new().set("bad name", "a\nb").unwrap_err();
        assert_eq!(
            error.to_string(),
            "Headers.set: \"bad name\" is an invalid header name."
        );
        let error = JsHeaders::new().set("中", "😀").unwrap_err();
        assert_eq!(
            error.to_string(),
            "Cannot convert argument to a ByteString because the character at index 0 has a \
             value of 20013 which is greater than 255."
        );
        let error = JsHeaders::new().append("中", "😀").unwrap_err();
        assert!(
            error.to_string().contains("value of 20013"),
            "append reports the name's ByteString error: {error}"
        );
        let error = JsHeaders::new().set("bad name", "中").unwrap_err();
        assert!(error.to_string().contains("ByteString"), "{error}");
    }

    /// 回归测试（评审 finding #4b）：`new Headers({Accept:"a", accept:"b"})` 以 `append`
    /// 合并为 `"a, b"`（不是后者覆盖）。
    #[test]
    fn js_headers_from_pairs_appends_case_insensitive_duplicates() {
        let headers = JsHeaders::from_pairs([("Accept", "a"), ("accept", "b")]).unwrap();
        assert_eq!(headers.get("accept"), Some("a, b"));
        assert_eq!(headers.iter().collect::<Vec<_>>(), vec![("accept", "a, b")]);
    }

    /// 回归测试（评审 finding #4c）：响应头字节按 latin1 解码，不是 UTF-8。
    #[test]
    fn response_header_bytes_decode_as_latin1() {
        let mut map = HeaderMap::new();
        map.insert("x-echo", HeaderValue::from_bytes(&[0xe9, 0x41]).unwrap());
        let headers = JsHeaders::from_header_map(&map);
        assert_eq!(headers.get("x-echo"), Some("éA"));
        assert_eq!(header_joined(&map, "x-echo").as_deref(), Some("éA"));
    }

    #[test]
    fn headers_iterate_sorted_by_lowercase_name() {
        let mut headers = JsHeaders::new();
        headers.set("b", "2").unwrap();
        headers.set("A", "1").unwrap();
        assert_eq!(
            headers.iter().collect::<Vec<_>>(),
            vec![("a", "1"), ("b", "2")]
        );
    }

    #[test]
    fn form_encoding_matches_url_search_params() {
        assert_eq!(
            FetchBody::Form(vec![
                ("grant_type".to_owned(), "authorization_code".to_owned()),
                ("code".to_owned(), "a b".to_owned()),
                (
                    "redirect_uri".to_owned(),
                    "http://127.0.0.1/cb?x=1".to_owned()
                ),
            ])
            .to_bytes(),
            b"grant_type=authorization_code&code=a+b&redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb%3Fx%3D1"
                .to_vec()
        );
        assert_eq!(
            FetchBody::Form(vec![("k".to_owned(), "~!*'()".to_owned())]).to_bytes(),
            b"k=%7E%21*%27%28%29".to_vec()
        );
        assert_eq!(
            FetchBody::Form(vec![("k".to_owned(), "é 中".to_owned())]).to_bytes(),
            b"k=%C3%A9+%E4%B8%AD".to_vec()
        );
    }

    #[test]
    fn transport_error_names_match_typescript() {
        assert_eq!(McpTransportError::new("x").name(), "Error");
        assert_eq!(McpTransportError::network("x").name(), "TypeError");
        assert_eq!(McpTransportError::http(400, "m", "").name(), "McpHttpError");
        assert_eq!(
            McpTransportError::auth_required(None, "").name(),
            "McpAuthRequiredError"
        );
        assert_eq!(
            McpTransportError::session_expired("").name(),
            "McpSessionExpiredError"
        );
        assert_eq!(
            McpTransportError::from(McpError::aborted()).name(),
            "AbortError"
        );
        assert_eq!(
            McpTransportError::auth_required(Some("Bearer".to_owned()), "").status(),
            Some(401)
        );
        assert_eq!(
            McpTransportError::session_expired("").to_string(),
            "MCP session expired"
        );
    }

    #[tokio::test]
    async fn fetch_response_reads_text_and_json() {
        let mut response = FetchResponse::from_bytes(200, HeaderMap::new(), b"{\"n\":1}".to_vec());
        assert!(response.ok());
        let value = response.json().await.unwrap();
        assert_eq!(value["n"], serde_json::json!(1));

        let mut empty = FetchResponse::from_bytes(404, HeaderMap::new(), Vec::new());
        assert!(!empty.ok());
        assert_eq!(empty.text().await.unwrap(), "");
    }

    #[test]
    fn redirect_validation_rechecks_scheme_and_downgrade() {
        let https = Url::parse("https://example.com/a").unwrap();
        let http = Url::parse("http://example.com/b").unwrap();
        let ftp = Url::parse("ftp://example.com/b").unwrap();
        let loopback = Url::parse("http://127.0.0.1:8080/b").unwrap();

        assert_eq!(
            validate_redirect(&[https.clone()], &http),
            Err("refusing to follow an HTTPS-to-HTTP redirect")
        );
        assert_eq!(validate_redirect(&[https.clone()], &loopback), Ok(()));
        assert_eq!(validate_redirect(&[https.clone()], &https), Ok(()));
        assert_eq!(
            validate_redirect(&[https.clone()], &ftp),
            Err("redirect target must use http or https")
        );
        let chain = vec![https; DEFAULT_MAX_REDIRECTS];
        assert_eq!(validate_redirect(&chain, &http), Err("too many redirects"));
    }

    #[test]
    fn loopback_hosts_are_recognised() {
        for url in [
            "http://localhost:1/",
            "http://127.0.0.1/",
            "http://127.9.8.7/",
            "http://[::1]/",
        ] {
            assert!(is_loopback_host(&Url::parse(url).unwrap()), "{url}");
        }
        for url in ["http://example.com/", "http://10.0.0.1/"] {
            assert!(!is_loopback_host(&Url::parse(url).unwrap()), "{url}");
        }
    }

    #[test]
    fn content_type_is_split_and_lowercased() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            HeaderValue::from_static("text/Event-Stream; charset=utf-8"),
        );
        assert_eq!(
            content_type_header(&headers).as_deref(),
            Some("text/event-stream")
        );
        assert_eq!(header_joined(&headers, "missing"), None);
    }
}
