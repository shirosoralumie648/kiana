//! `oauth/errors.ts`：OAuth 子系统的错误类（`spec/telemetry-mcp.md` §18.2、§27.2）。
//!
//! TS 源：`packages/mcp/src/oauth/errors.ts`。
//!
//! 五个类各自是 `Error` 的子类，`name` 逐字（[`OAuthFailure::name`] 返回同一个字符串）。
//! 它们的消息构造逐字对应 `spec/telemetry-mcp.md` §27.2：
//!
//! | 类 | 消息 |
//! |---|---|
//! | [`OAuthError`] | `message \|\| code`；字段 `code`、`error_uri` |
//! | [`OAuthIssuerMismatchError`] | `OAuth issuer mismatch: expected {fmt(expected)}, received {fmt(received)}`，`fmt = undefined → "none" : JSON.stringify` |
//! | [`OAuthInsecureEndpointError`] | `Refusing to send OAuth credentials to non-HTTPS endpoint {endpoint}` |
//! | [`OAuthRegistrationError`] | `OAuth dynamic client registration failed with status {status}: {body}` |
//! | [`McpOAuthAuthorizationRequiredError`] | `MCP OAuth authorization requires user interaction` |
//!
//! # 为什么还有一个 [`OAuthFailure`]
//!
//! TS 的调用方用 `instanceof` 分支（`error instanceof OAuthError && error.code !== "server_error"`
//! 等），而 Rust 的 `Result` 只能有一个错误类型。`oauth/{discovery,flow,provider}.ts` 的这些函数
//! 又会抛出**全部**五类错误、普通 `Error`，以及 `fetch` 的 `TypeError`，因此本模块用一个信封
//! [`OAuthFailure`] 把它们收在一起：`name()` 返回稳定的错误类名（与原 `error.name` 一致），`as_oauth()` /
//! `as_issuer_mismatch()` 等对应 `instanceof`。
//!
//! 与传输层的边界（[`crate::auth_provider::AuthProvider::on_unauthorized`] 返回
//! [`McpTransportError`]）用 [`McpTransportError::from`] 转换：每个 OAuth 类都保留 `name`
//! （[`McpTransportError::named`]）与逐字消息，`TypeError` 仍是 `TypeError`——coding-agent 的
//! `needsSignIn` 依赖 `error instanceof McpOAuthAuthorizationRequiredError`。
//!
//! 本 crate 之外，`kiana-builtin-extensions` 的 MCP 登录流程直接消费这个信封：
//! `McpSignInError::Failure` 原样保留 [`OAuthFailure`]，只有用户取消单独用
//! `McpSignInCancelledError` 表示。
//!
//! # 设计原因与实现约定
//!
//! 五个类都写成「公开字段 + `name()` + `message()` + 手写 `Display`」的普通结构体，
//! 没有用 `thiserror` 派生。具体约定：
//!
//! - `OAuthError` 的消息在**构造时**做 `message || code` 回落并存进字段，`message()` 只是
//!   读出它；
//! - 其余类的消息是字段拼接或固定文本（issuer 还要把 `None` 渲染成 `none`），由 `message()`
//!   用 `format!` 现场拼出；
//! - `name()` 返回 `&'static str` 是硬约束：跨层转换把类名交给 [`McpTransportError::named`]，
//!   后者只接受 `&'static str`；
//! - 各类型的 `std::error::Error` 实现都是空的（没有 `source()`）：TS 侧这些类不携带 `cause`，
//!   唯一的结构化信息就是各自的公开字段。
//!
//! # 一次错误的产生、判定与出站
//!
//! 1. **产生**：`oauth::discovery` 的 issuer 校验与 `oauth::flow` 的授权响应 `iss` 校验都会
//!    构造 [`OAuthIssuerMismatchError`]；`oauth::flow` 的 `secure_endpoint`、`register_client`、
//!    `token_request` 分别构造 [`OAuthInsecureEndpointError`]、[`OAuthRegistrationError`]、
//!    [`OAuthError`]。参数/解析辅助函数的失败用 [`OAuthFailure::Error`]（`Invalid …`）与
//!    [`OAuthFailure::TypeError`]（`Invalid URL`、复刻 V8 的 `TypeError` 文案）。
//! 2. **传递**：[`McpFetch`](crate::auth_provider::McpFetch) 的失败是 [`McpTransportError`]，
//!    经 `From` 落进 [`OAuthFailure::Transport`]（网络失败在里面仍叫 `TypeError`）。
//! 3. **判定**：调用方用 `as_oauth()` / `as_issuer_mismatch()` / `as_insecure_endpoint()` /
//!    `is_type_error()` 复刻 TS 的 `instanceof`，不要按消息文本判断。`is_type_error()` 同时
//!    覆盖裸 `TypeError` 变体与 `Transport` 里的网络失败——`discover_oauth_server_info` 就是
//!    靠它决定「重新抛出还是吞掉」。
//! 4. **出站**：`AuthProvider::on_unauthorized` 只能返回 [`McpTransportError`]，于是
//!    `McpTransportError::from(OAuthFailure)` 把 OAuth 类的类名与消息搬进 `Named` 错误；
//!    此后只剩 `is_oauth_authorization_required()` 这类按 `name` 字符串判定的入口，因此这些
//!    字符串是不能改的跨 crate 契约。
//!
//! # 注意事项
//!
//! - `OAuthError::message` 是**构造时**算好并存下的字段：`OAuthError::new` 里的
//!   `message || code` 回落只发生一次。字段公开是为了对齐 TS 的可读属性，直接用结构体字面量
//!   构造会跳过回落，写出空消息。
//! - 除 `OAuthError::message()`（返回已存字段的 `&str`）外，其余四类的 `message()` 每次调用都
//!   重新拼装并返回拥有所有权的 `String`（`format!` 或固定文本的 `to_owned`）；`Display` 直接
//!   委托给它。`OAuthFailure::message()` 再在这之上转发一层。
//! - [`OAuthIssuerMismatchError`] 的 `expected`/`received` 在**不同调用点含义不同**，详见该
//!   类型的文档。
//! - [`OAuthFailure`] 只派生 `Debug, Clone`（包装的 [`McpTransportError`] 不是 `PartialEq`），
//!   测试里断言类与消息要用 `name()` / `message()` / `as_*()`，不能用 `==`。

use std::fmt;

use kiana_json::js_string::js_quote;

use crate::auth_provider::McpTransportError;

/// `class OAuthError extends Error`（`oauth/errors.ts`）。
///
/// `new OAuthError(code, message, errorUri?)`：`super(message || code)`，因此空消息回退到 `code`。
///
/// 通过 [`OAuthError::new`] 构造时，空 `message` 会被 `code` 顶替（两者都为空串时消息仍为空，
/// 与 TS 的 `||` 一致）；`message()` 与 `Display` 都返回这个已存字段。字段公开是为了对齐 TS
/// 的可读属性，结构体字面量会绕开上述回落，不要用它替代 `new`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthError {
    /// `error.code`（OAuth 错误码，例如 `invalid_grant`、`server_error`）。
    pub code: String,
    /// `error.message`（逐字；`Display` 同值）。
    pub message: String,
    /// `error.errorUri`（TS 的 `errorUri`，缺失为 `undefined`）。
    pub error_uri: Option<String>,
}

impl OAuthError {
    /// `new OAuthError(code, message, errorUri?)`。
    ///
    /// `message` 为空串时用 `code`（TS 的 `super(message || code)`）。`error_uri` 的 `None`
    /// 对应 TS 的 `undefined`；构造器不校验 `code` 是否为已知 OAuth 错误码，也不校验
    /// `error_uri` 是否是合法 URL——两者都原样保存。
    ///
    /// 这是唯一执行「空消息回退到 `code`」的入口；`code` 也是空串时消息保持空串。
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        error_uri: Option<String>,
    ) -> Self {
        let code = code.into();
        let message = message.into();
        let message = if message.is_empty() {
            code.clone()
        } else {
            message
        };
        OAuthError {
            code,
            message,
            error_uri,
        }
    }

    /// `error.name`。
    pub fn name(&self) -> &'static str {
        "OAuthError"
    }

    /// `error.message`。
    ///
    /// 返回 `&str` 而不是 `String`：回落已在构造时完成，这里只是读出字段。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for OAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OAuthError {}

/// `class OAuthIssuerMismatchError extends Error`（RFC 9207 的 `iss` 校验）。
///
/// `expected` 在完全没有发现授权服务器元数据时为 `None`；`received` 在授权响应缺少 `iss`
/// 时为 `None`。消息里的两者分别格式化为 `"none"` 或 `JSON.stringify(value)`。
///
/// # 两个字段的含义随调用点变化
///
/// 读消息时不要预设 `expected` 一定是元数据里的 issuer：
///
/// - `oauth::discovery` 的 `discover_authorization_server_metadata`：`expected` 是调用方传入的
///   `authorizationServerUrl`（这里总是 `Some`），`received` 是元数据里的 `issuer`（元数据没写
///   `issuer` 时为 `None`）。
/// - `oauth::flow` 的授权响应校验：`expected` 是元数据的 `issuer`（没有元数据时为 `None`），
///   `received` 是授权响应里的 `iss` 参数（缺失时为 `None`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthIssuerMismatchError {
    /// 期望的 issuer（`undefined` → `None`）。
    pub expected: Option<String>,
    /// 实际收到的 issuer（`undefined` → `None`）。
    pub received: Option<String>,
}

/// `format = issuer => issuer === undefined ? "none" : JSON.stringify(issuer)`。
///
/// `None` 对应 JS 的 `undefined`，渲染成不带引号的 `none`；`Some` 走
/// `kiana_json::js_string::js_quote`（双引号包裹并对控制字符做 JSON 转义），因此消息里会出现
/// 引号。`Option<String>` 无法区分 JS 的 `null`——本类在 TS 里的签名只有
/// `string | undefined`，不存在 `null` 输入。
fn format_issuer(issuer: Option<&str>) -> String {
    match issuer {
        None => "none".to_owned(),
        Some(issuer) => js_quote(issuer),
    }
}

impl OAuthIssuerMismatchError {
    /// `new OAuthIssuerMismatchError(expected, received)`。
    pub fn new(expected: Option<String>, received: Option<String>) -> Self {
        OAuthIssuerMismatchError { expected, received }
    }

    /// `error.name`。
    pub fn name(&self) -> &'static str {
        "OAuthIssuerMismatchError"
    }

    /// `error.message`（逐字）。
    pub fn message(&self) -> String {
        format!(
            "OAuth issuer mismatch: expected {}, received {}",
            format_issuer(self.expected.as_deref()),
            format_issuer(self.received.as_deref())
        )
    }
}

impl fmt::Display for OAuthIssuerMismatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for OAuthIssuerMismatchError {}

/// `class OAuthInsecureEndpointError extends Error`：拒绝把凭据发往非 HTTPS 端点。
///
/// 本 crate 里由 `oauth::flow` 的 `secure_endpoint` 产生：scheme 不是 `https` 且 host 不是
/// loopback 时抛出（loopback 只认 `localhost`、`127.0.0.1`、`[::1]`、`::1`，其余明文 HTTP
/// 地址都会被拒）。`endpoint` 是 URL 解析后的 `as_str()`，可能已被规范化（例如裸 origin 会补上
/// 尾斜杠），不保证等于调用方的原始输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthInsecureEndpointError {
    /// `error.endpoint`（`URL.href`）。
    pub endpoint: String,
}

impl OAuthInsecureEndpointError {
    /// `new OAuthInsecureEndpointError(endpoint)`。
    pub fn new(endpoint: impl Into<String>) -> Self {
        OAuthInsecureEndpointError {
            endpoint: endpoint.into(),
        }
    }

    /// `error.name`。
    pub fn name(&self) -> &'static str {
        "OAuthInsecureEndpointError"
    }

    /// `error.message`（逐字）。
    pub fn message(&self) -> String {
        format!(
            "Refusing to send OAuth credentials to non-HTTPS endpoint {}",
            self.endpoint
        )
    }
}

impl fmt::Display for OAuthInsecureEndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for OAuthInsecureEndpointError {}

/// `class OAuthRegistrationError extends Error`：动态客户端注册（DCR）失败。
///
/// 本 crate 里由 `oauth::flow` 的 `register_client` 在 DCR 响应非 2xx 时构造：`status` 是响应
/// 的 HTTP 状态码，`body` 是 `response.text()` 的完整结果。与传输层 [`McpTransportError`] 的
/// HTTP 错误不同，本类型不截断 `body`（8192 单元上限只由 `transports::streamable_http` 的
/// `check_response` 在构造传输错误前施加）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthRegistrationError {
    /// `error.status`（HTTP 状态码）。
    pub status: u16,
    /// `error.body`（响应体全文）。
    pub body: String,
}

impl OAuthRegistrationError {
    /// `new OAuthRegistrationError(status, body)`。
    pub fn new(status: u16, body: impl Into<String>) -> Self {
        OAuthRegistrationError {
            status,
            body: body.into(),
        }
    }

    /// `error.name`。
    pub fn name(&self) -> &'static str {
        "OAuthRegistrationError"
    }

    /// `error.message`（逐字）。
    pub fn message(&self) -> String {
        format!(
            "OAuth dynamic client registration failed with status {}: {}",
            self.status, self.body
        )
    }
}

impl fmt::Display for OAuthRegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for OAuthRegistrationError {}

/// `class McpOAuthAuthorizationRequiredError extends Error`：需要用户交互才能继续授权。
///
/// 本 crate 里由 `oauth::flow` 中 `adapt_oauth_provider` 返回的认证提供方产生：它的
/// `on_unauthorized` 在 `authorize_mcp` 返回 `OAuthFlowResult::Redirect` 时，把这个标记立刻
/// 转成 [`McpTransportError`]（`McpTransportError::from(OAuthFailure::from(…))`），再经
/// `McpTransportError::is_oauth_authorization_required` 被上层识别为“需要用户重新登录”。
/// 它是无字段的标记类型：`new()` 与 `Default::default()` 等价。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct McpOAuthAuthorizationRequiredError;

impl McpOAuthAuthorizationRequiredError {
    /// `new McpOAuthAuthorizationRequiredError()`。
    pub fn new() -> Self {
        McpOAuthAuthorizationRequiredError
    }

    /// `error.name`。
    pub fn name(&self) -> &'static str {
        "McpOAuthAuthorizationRequiredError"
    }

    /// `error.message`（逐字）。
    pub fn message(&self) -> String {
        "MCP OAuth authorization requires user interaction".to_owned()
    }
}

impl fmt::Display for McpOAuthAuthorizationRequiredError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for McpOAuthAuthorizationRequiredError {}

/// `oauth/*` 抛出的全部错误，收在一个信封里（见模块文档）。
///
/// `name()` 逐字等于 TS 的 `error.name`；`instanceof` 判定用各 `as_*` 方法。
///
/// 只派生 `Debug, Clone`：内层 [`McpTransportError`] 没有 `PartialEq`，所以整个信封也不能用
/// `==` 比较；测试断言请走 `name()` / `message()` / `as_*()`。`name()` 与 `message()` 的
/// `match` 是穷尽的：新增变体时编译器会强制在这两处补分支。
#[derive(Debug, Clone)]
pub enum OAuthFailure {
    /// 普通 `new Error(message)`（`name === "Error"`）。
    ///
    /// 构造入口是 [`OAuthFailure::error`]；`oauth::types` 的 `Invalid …` 与 `oauth::callback`
    /// 的监听/超时错误都走这里。
    Error(String),
    /// 裸 `TypeError(message)`：本 crate 里由 URL 解析失败（`"Invalid URL"`）与复刻 V8 的
    /// `TypeError` 文案构造（例如 `Cannot read properties of undefined (reading 'includes')`）。
    ///
    /// `fetch` 的网络失败**不**走这个变体：`McpFetch` 的失败类型是 [`McpTransportError`]，
    /// 经 `From` 落进 [`OAuthFailure::Transport`]；但它的 `name` 仍是 `TypeError`，
    /// `is_type_error()` 因此同时覆盖两条路径（这是 `discovery` 决定“TypeError 向外传播”的
    /// 依据）。
    TypeError(String),
    /// [`OAuthError`]。
    OAuth(OAuthError),
    /// [`OAuthIssuerMismatchError`]。
    IssuerMismatch(OAuthIssuerMismatchError),
    /// [`OAuthInsecureEndpointError`]。
    InsecureEndpoint(OAuthInsecureEndpointError),
    /// [`OAuthRegistrationError`]。
    Registration(OAuthRegistrationError),
    /// [`McpOAuthAuthorizationRequiredError`]。
    AuthorizationRequired(McpOAuthAuthorizationRequiredError),
    /// `fetch`/传输层抛出的其他错误（`SyntaxError`、`DOMException`、`McpError` …）。
    ///
    /// 网络失败也在这里（`name` 为 `TypeError`）；`From<McpTransportError>` 原样包装，
    /// 转回 [`McpTransportError`] 时也原样返回，因此 HTTP 错误的 status/body 不会丢失。
    Transport(McpTransportError),
}

impl OAuthFailure {
    /// `new Error(message)`。
    pub fn error(message: impl Into<String>) -> Self {
        OAuthFailure::Error(message.into())
    }

    /// `new TypeError(message)`。
    pub fn type_error(message: impl Into<String>) -> Self {
        OAuthFailure::TypeError(message.into())
    }

    /// `error.name`（逐字）。
    ///
    /// 返回值会被 `McpTransportError::named` 存成静态字符串，是跨 crate 按名字分支的契约；
    /// 与 TS 类名不一致会破坏 `is_oauth_authorization_required()` 的判定。
    pub fn name(&self) -> &'static str {
        match self {
            OAuthFailure::Error(_) => "Error",
            OAuthFailure::TypeError(_) => "TypeError",
            OAuthFailure::OAuth(error) => error.name(),
            OAuthFailure::IssuerMismatch(error) => error.name(),
            OAuthFailure::InsecureEndpoint(error) => error.name(),
            OAuthFailure::Registration(error) => error.name(),
            OAuthFailure::AuthorizationRequired(error) => error.name(),
            OAuthFailure::Transport(error) => error.name(),
        }
    }

    /// `error.message`（逐字；`Display` 同值）。
    pub fn message(&self) -> String {
        match self {
            OAuthFailure::Error(message) | OAuthFailure::TypeError(message) => message.clone(),
            OAuthFailure::OAuth(error) => error.message.clone(),
            OAuthFailure::IssuerMismatch(error) => error.message(),
            OAuthFailure::InsecureEndpoint(error) => error.message(),
            OAuthFailure::Registration(error) => error.message(),
            OAuthFailure::AuthorizationRequired(error) => error.message(),
            OAuthFailure::Transport(error) => error.message(),
        }
    }

    /// `error instanceof TypeError`（`discoverOAuthServerInfo` 用它决定是否向上传播）。
    ///
    /// 两条路径都算：裸 [`OAuthFailure::TypeError`]，以及 [`OAuthFailure::Transport`] 中
    /// `McpTransportError::is_type_error()` 为真的网络失败。因此只由 `fetch` 产生的网络错误
    /// 也能被这个判定命中。
    pub fn is_type_error(&self) -> bool {
        match self {
            OAuthFailure::TypeError(_) => true,
            OAuthFailure::Transport(error) => error.is_type_error(),
            _ => false,
        }
    }

    /// `error instanceof OAuthError`。
    ///
    /// 调用方按 `code` 分支：`authorize_mcp` 只对 `invalid_client`/`unauthorized_client`/
    /// `invalid_grant` 做凭据失效并重试，其余错误原样传播。
    pub fn as_oauth(&self) -> Option<&OAuthError> {
        match self {
            OAuthFailure::OAuth(error) => Some(error),
            _ => None,
        }
    }

    /// `error instanceof OAuthIssuerMismatchError`。
    pub fn as_issuer_mismatch(&self) -> Option<&OAuthIssuerMismatchError> {
        match self {
            OAuthFailure::IssuerMismatch(error) => Some(error),
            _ => None,
        }
    }

    /// `error instanceof OAuthInsecureEndpointError`。
    ///
    /// `run_flow` 的刷新回落用它：刷新失败时不安全端点不重走授权流程，直接上抛。
    pub fn as_insecure_endpoint(&self) -> Option<&OAuthInsecureEndpointError> {
        match self {
            OAuthFailure::InsecureEndpoint(error) => Some(error),
            _ => None,
        }
    }

    /// `error instanceof OAuthRegistrationError`。
    pub fn as_registration(&self) -> Option<&OAuthRegistrationError> {
        match self {
            OAuthFailure::Registration(error) => Some(error),
            _ => None,
        }
    }

    /// `error instanceof McpOAuthAuthorizationRequiredError`。
    pub fn as_authorization_required(&self) -> Option<&McpOAuthAuthorizationRequiredError> {
        match self {
            OAuthFailure::AuthorizationRequired(error) => Some(error),
            _ => None,
        }
    }

    /// 包装的 [`McpTransportError`]（若有）。
    ///
    /// 只有 [`OAuthFailure::Transport`] 变体返回 `Some`；`TypeError` / `Error` 变体虽然出站时
    /// 也会转成传输错误，但在这里是 `None`。
    pub fn as_transport(&self) -> Option<&McpTransportError> {
        match self {
            OAuthFailure::Transport(error) => Some(error),
            _ => None,
        }
    }
}

impl fmt::Display for OAuthFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for OAuthFailure {}

impl From<McpTransportError> for OAuthFailure {
    fn from(error: McpTransportError) -> Self {
        OAuthFailure::Transport(error)
    }
}

impl From<OAuthError> for OAuthFailure {
    fn from(error: OAuthError) -> Self {
        OAuthFailure::OAuth(error)
    }
}

impl From<OAuthIssuerMismatchError> for OAuthFailure {
    fn from(error: OAuthIssuerMismatchError) -> Self {
        OAuthFailure::IssuerMismatch(error)
    }
}

impl From<OAuthInsecureEndpointError> for OAuthFailure {
    fn from(error: OAuthInsecureEndpointError) -> Self {
        OAuthFailure::InsecureEndpoint(error)
    }
}

impl From<OAuthRegistrationError> for OAuthFailure {
    fn from(error: OAuthRegistrationError) -> Self {
        OAuthFailure::Registration(error)
    }
}

impl From<McpOAuthAuthorizationRequiredError> for OAuthFailure {
    fn from(error: McpOAuthAuthorizationRequiredError) -> Self {
        OAuthFailure::AuthorizationRequired(error)
    }
}

impl From<OAuthFailure> for McpTransportError {
    /// 传输边界的转换（`AuthProvider::on_unauthorized` 返回 [`McpTransportError`]）。
    ///
    /// TS 里这些错误**原样**穿过传输层，上层用 `instanceof` 分支（coding-agent 的 `needsSignIn`
    /// 靠 `error instanceof McpOAuthAuthorizationRequiredError`）。Rust 的 `AuthProvider` 签名把
    /// 错误收窄成 [`McpTransportError`]，因此这里用 [`McpTransportError::named`] 保留每个 OAuth
    /// 类的 `name`（`OAuthError`、`OAuthIssuerMismatchError`、`OAuthInsecureEndpointError`、
    /// `OAuthRegistrationError`、`McpOAuthAuthorizationRequiredError`），消息逐字保留；
    /// `TypeError` 仍是 `TypeError`（传输层的重试/网络分类依赖它），普通 `Error` 仍是 `Error`。
    ///
    /// 映射规则与 `match` 的分支一一对应：
    ///
    /// - [`OAuthFailure::TypeError`] → `McpTransportError::network`：`name()` 为 `TypeError`，
    ///   `is_type_error()` 为真；
    /// - [`OAuthFailure::Transport`] → 原样返回（HTTP 的 `status`/`body` 保留）；
    /// - [`OAuthFailure::Error`] → `McpTransportError::new`：`name()` 为 `Error`；
    /// - 其余五个 OAuth 类 → `McpTransportError::named`：`name()` 是各自的类名，消息逐字保留。
    ///
    /// 转换后 `as_oauth()` 等判定不再可用，只剩按 `name()` 字符串的
    /// `is_oauth_authorization_required()`。
    fn from(error: OAuthFailure) -> Self {
        match error {
            OAuthFailure::TypeError(message) => McpTransportError::network(message),
            OAuthFailure::Transport(error) => error,
            OAuthFailure::Error(message) => McpTransportError::new(message),
            other => McpTransportError::named(other.name(), other.message()),
        }
    }
}

// 这些测试锁定三件事：五类消息的逐字文本（含 issuer 的 `none`/引号格式与 `message || code`
// 回落）、信封的 `name()`/`as_*()`/`is_type_error()` 判定，以及转换到 `McpTransportError`
// 之后类名与消息的保留（`needsSignIn` 依赖 `McpOAuthAuthorizationRequiredError` 这个名字）。
// 断言只用公开 API：`name()`/`message()`/`to_string()`、`as_*`/`is_type_error()` 与公开字段
// （如 `OAuthError` 的 `code`/`message`/`error_uri`），不依赖私有表示。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_error_falls_back_to_the_code_for_an_empty_message() {
        let error = OAuthError::new("invalid_grant", "", None);
        assert_eq!(error.message, "invalid_grant");
        assert_eq!(error.to_string(), "invalid_grant");
        let error = OAuthError::new("server_error", "HTTP 500: boom", Some("https://e".into()));
        assert_eq!(error.to_string(), "HTTP 500: boom");
        assert_eq!(error.code, "server_error");
        assert_eq!(error.error_uri.as_deref(), Some("https://e"));
        assert_eq!(error.name(), "OAuthError");
    }

    /// §27.2：`fmt = undefined → "none" : JSON.stringify(issuer)`。
    #[test]
    fn issuer_mismatch_formats_missing_issuers_as_none() {
        let error =
            OAuthIssuerMismatchError::new(Some("https://a".into()), Some("https://b".into()));
        assert_eq!(
            error.to_string(),
            "OAuth issuer mismatch: expected \"https://a\", received \"https://b\""
        );
        let error = OAuthIssuerMismatchError::new(None, Some("https://b".into()));
        assert_eq!(
            error.to_string(),
            "OAuth issuer mismatch: expected none, received \"https://b\""
        );
        let error = OAuthIssuerMismatchError::new(Some("https://a".into()), None);
        assert_eq!(
            error.to_string(),
            "OAuth issuer mismatch: expected \"https://a\", received none"
        );
        assert_eq!(error.name(), "OAuthIssuerMismatchError");
    }

    #[test]
    fn remaining_messages_are_verbatim() {
        assert_eq!(
            OAuthInsecureEndpointError::new("http://x.example/token").to_string(),
            "Refusing to send OAuth credentials to non-HTTPS endpoint http://x.example/token"
        );
        assert_eq!(
            OAuthRegistrationError::new(400, "bad request").to_string(),
            "OAuth dynamic client registration failed with status 400: bad request"
        );
        assert_eq!(
            McpOAuthAuthorizationRequiredError::new().to_string(),
            "MCP OAuth authorization requires user interaction"
        );
        assert_eq!(
            McpOAuthAuthorizationRequiredError::new().name(),
            "McpOAuthAuthorizationRequiredError"
        );
    }

    #[test]
    fn failure_envelope_maps_names_and_instanceof_checks() {
        let failure: OAuthFailure = OAuthIssuerMismatchError::new(None, None).into();
        assert_eq!(failure.name(), "OAuthIssuerMismatchError");
        assert!(failure.as_issuer_mismatch().is_some());
        assert!(failure.as_oauth().is_none());
        assert!(!failure.is_type_error());

        let failure = OAuthFailure::type_error("fetch failed");
        assert_eq!(failure.name(), "TypeError");
        assert!(failure.is_type_error());

        let failure: OAuthFailure = OAuthError::new("invalid_grant", "nope", None).into();
        assert_eq!(
            failure.as_oauth().map(|error| error.code.as_str()),
            Some("invalid_grant")
        );
    }

    /// 传输边界：每个 OAuth 类保留 `name`（coding-agent 的 `needsSignIn` 依赖
    /// `McpOAuthAuthorizationRequiredError`）与逐字消息；`TypeError` 仍是 `TypeError`。
    #[test]
    fn conversion_to_transport_error_keeps_names_and_messages() {
        let transport = McpTransportError::from(OAuthFailure::type_error("fetch failed"));
        assert_eq!(transport.name(), "TypeError");
        assert_eq!(transport.to_string(), "fetch failed");

        let transport =
            McpTransportError::from(OAuthFailure::from(McpOAuthAuthorizationRequiredError::new()));
        assert_eq!(transport.name(), "McpOAuthAuthorizationRequiredError");
        assert!(
            transport.is_oauth_authorization_required(),
            "needsSignIn 依赖这个判定"
        );
        assert!(!transport.is_auth_required());
        assert!(!transport.is_type_error());
        assert!(!transport.is_http());
        assert_eq!(
            transport.to_string(),
            "MCP OAuth authorization requires user interaction"
        );

        let class_cases: [(OAuthFailure, &str); 4] = [
            (
                OAuthError::new("invalid_grant", "bad grant", None).into(),
                "OAuthError",
            ),
            (
                OAuthIssuerMismatchError::new(Some("https://a".into()), None).into(),
                "OAuthIssuerMismatchError",
            ),
            (
                OAuthInsecureEndpointError::new("http://x.example/token").into(),
                "OAuthInsecureEndpointError",
            ),
            (
                OAuthRegistrationError::new(400, "bad request").into(),
                "OAuthRegistrationError",
            ),
        ];
        for (failure, name) in class_cases {
            let message = failure.message();
            let transport = McpTransportError::from(failure);
            assert_eq!(transport.name(), name);
            assert_eq!(transport.to_string(), message);
            assert!(!transport.is_oauth_authorization_required());
        }

        let transport = McpTransportError::from(OAuthFailure::error("plain"));
        assert_eq!(transport.name(), "Error");

        let original = McpTransportError::network("boom");
        let round_trip = McpTransportError::from(OAuthFailure::from(original));
        assert_eq!(round_trip.name(), "TypeError");
        assert_eq!(round_trip.to_string(), "boom");
    }
}
