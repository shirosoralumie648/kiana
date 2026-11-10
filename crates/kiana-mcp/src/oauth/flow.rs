//! `oauth/flow.ts`：PKCE、授权 URL、令牌请求、动态客户端注册、`runFlow` 与
//! `adaptOAuthProvider`（`spec/telemetry-mcp.md` §18.4、§25 第 12–13 条、§27.3、§29）。
//!
//! TS 源：`packages/mcp/src/oauth/flow.ts`
//! （改编自 `modelcontextprotocol/typescript-sdk` v1.29.0 `src/client/auth.ts`，MIT，
//! 见 `oauth` 模块文档的许可声明）。
//!
//! # 契约面（`oauth/index.ts` 的 13 个名字）
//!
//! [`OAuthClientProvider`]（异步 trait，`oauth-provider` 实现它）、[`OAuthClientMetadataDocument`]、
//! [`OAuthFlowOptions`]、[`OAuthFlowResult`]、[`TokenRequestOptions`]、
//! [`AddClientAuthentication`]、[`start_authorization`]、[`register_client`]、
//! [`exchange_authorization_code`]、[`refresh_authorization`]、[`step_up_scope`]、
//! [`authorize_mcp`]、[`adapt_oauth_provider`]。
//!
//! 模块内（**不**进包根再导出，§30.1）：[`loopback`]、[`application_type`]、[`secure_endpoint`]、
//! [`select_client_auth_method`]、[`apply_client_authentication`]、[`pkce`]、[`token_request`]、
//! [`with_scope`]、[`run_flow`]，以及它们用到的选项/结果结构体与 [`ClientAuthMethod`]、
//! [`CredentialKind`]、[`Pkce`]。
//!
//! # 逐字复刻的怪癖（不要"改进"）
//!
//! - **PKCE**：32 随机字节（`crypto.getRandomValues`）→ verifier = 这些字节的 base64url（无
//!   padding，43 字符）；challenge = base64url(SHA-256(UTF-8(verifier)))。`getrandom` + `sha2`。
//! - **授权 URL 的查询串**：顺序为 `response_type`、`client_id`、`code_challenge`、
//!   `code_challenge_method`、`redirect_uri`，然后按真值追加 `state`、`scope`、
//!   `prompt=consent`（仅当 `scope` 按 `/\s+/` 切分后含 `offline_access`）、`resource`。
//!   全部走 `URLSearchParams.set` 语义：同名参数**原位**替换（保持位置），新参数追加。
//!   这里用 [`search_params_set`] 复刻（`url` crate 没有等价 API）。
//! - **`\s` 是 JS 正则的集合**（`is_js_whitespace`），不是 Rust 的 `char::is_whitespace`
//!   （`U+0085` 在 JS 里不是空白，`U+FEFF` 是）。
//! - **JSON body 的键序/键位**：DCR 体是 `{...clientMetadata, application_type: …,
//!   ...(scope ? {scope} : {})}`，JS 的对象字面量对已存在的键只改值、保持位置；`refresh_token`
//!   的合并是 `{refresh_token: old, ...serverTokens}`，服务端返回的 rotate 后的 token **覆盖**
//!   传入值、但键仍在第一位（`shares one refresh … when refresh tokens rotate` 依赖它）。
//!   写出必须用 `kiana_json::js_object::stringify`（整数样式键前置、`1.0` → `1`、`-0` → `0`），
//!   不能用 `serde_json::to_string`。
//! - **原始属性值的 JS 强制转换**：`??` 只跳过 `undefined`/`null`（`js_nullish`）；
//!   `.includes` 对字符串是子串、对数组是严格相等（`js_includes`）；`.length` 只对
//!   字符串/数组存在（`js_length`）；`new URL(v)`、模板字符串与 `URLSearchParams.set` 都做
//!   `String(v)`；`if (x)` 是 JS 真值（[`kiana_json::js_string::is_truthy`]）。这些语义用于
//!   手工构造/损坏的元数据、客户端信息与令牌，错误文案逐字复刻 V8 的 TypeError。
//! - **令牌错误优先级**：body 里的 `error` 字符串先于 HTTP 状态判定
//!   （`OAuthError(error, error_description ?? error, error_uri ?? undefined)`），否则非 ok →
//!   `OAuthError("server_error", \`HTTP {status}: {text}\`)`。
//! - **`||` 而不是 `??` 的 scope 回退**：`options.scope || scopes_supported.join(" ") ||
//!   clientMetadata.scope`；空串（含 `scopes_supported: []` 拼出的 `""`）会继续下落。
//! - **`iss` 矩阵（RFC 9207）**：`iss !== undefined || metadata?
//!   .authorization_response_iss_parameter_supported` 时校验 `iss === metadata?.issuer`；
//!   没有元数据时 `expected` 是 `undefined`，任何 `iss` 都不匹配。
//! - **刷新失败的下落规则**：重新抛出的只有 signal 已 abort、`OAuthInsecureEndpointError`、
//!   以及 code 不是 `server_error` 的 `OAuthError`；其余错误（`server_error`、
//!   [`OAuthFailure::Transport`] 传输/网络错误、令牌解析失败或 URL 强转产生的普通
//!   `Error`/`TypeError`）都回落到新授权。
//! - **`authorizeMcp` 只重试一次**：`invalid_client`/`unauthorized_client` →
//!   `invalidateCredentials("all")`；`invalid_grant` → `invalidateCredentials("tokens")`。
//! - **`adaptOAuthProvider`**：并发 401 共享**同一个** `inFlight` future；令牌已被别的请求换掉时
//!   直接返回（不再刷新）；`insufficient_scope` 用 [`step_up_scope`] 合并旧 scope 并
//!   `skipRefresh`；`REDIRECT` → [`McpOAuthAuthorizationRequiredError`]。TS 的 `inFlight` 是
//!   **已启动**的 promise：发起它的请求被 abort 后刷新仍会跑完（含 `saveTokens`），因此 Rust
//!   侧用一个后台任务驱动共享 future（见 `AdaptedOAuthProvider::start_flight`）。
//!
//! # 执行流程（[`run_flow`]）
//!
//! 两条入口最终都落到 [`run_flow`]：[`authorize_mcp`] 供嵌入方直接调用
//! （`kiana-builtin-extensions` 的 MCP OAuth 登录/刷新路径）；[`adapt_oauth_provider`] 把
//! provider 适配成传输层的 [`AuthProvider`]（`streamable_http` 在 401 或
//! `insufficient_scope` 403 时调用 `on_unauthorized`）。`run_flow` 的阶段：
//!
//! 1. **元数据地址预检**：配置了 `authorization_server_metadata_url` 时先过
//!    [`secure_endpoint`]；非 https 且非 loopback 在这里就失败，之后才可能发出 HTTP 请求。
//! 2. **发现**：配置了该 URL 时不读、也不写发现缓存；否则读
//!    `provider.discovery_state()`（`authorization_server_url` 为空串视同没有缓存）。
//!    缓存命中但缺授权服务器元数据时只补授权服务器发现；没有缓存时
//!    `discover_oauth_server_info(server_url)` 还会做受保护资源发现。未配置 URL 时**每次都**
//!    调用 `save_discovery_state`（即使结果完全来自缓存）。
//! 3. **resource 与 scope**：`select_resource` 校验受保护资源并给出 RFC 8707 的 `resource`；
//!    scope 按 `options.scope` → `scopes_supported.join(" ")` → `clientMetadata.scope` 回退
//!    （`||` 语义：空串/假值继续下落）。
//! 4. **客户端身份**：已存储的 `clientInformation` 优先；否则取 `clientMetadataDocument`
//!    （CIMD，URL 必须 https 且 path 不是 `/`）。仍然没有时：有授权码直接报
//!    `OAuth client information is missing during code exchange`；`saveClientInformation`
//!    缺失时报 `OAuth client information cannot be persisted`；否则动态注册并持久化。随后决定
//!    重定向 URI：CIMD 的 `redirectUrl` 优先，否则 `provider.redirectUrl()`。
//! 5. **三个互斥分支**：
//!    - 有 `authorizationCode`：先按 RFC 9207 校验 `iss`（`iss` 存在或服务器声明支持时，必须
//!      与元数据 `issuer` 逐字相等；没有元数据时任何 `iss` 都不匹配），再取
//!      `codeVerifier()` 发起 code exchange，`withScope` 后 `saveTokens`，返回 `AUTHORIZED`。
//!    - 否则，若未 `skipRefresh` 且存储的令牌带**真值** `refresh_token`：刷新并保存
//!      （响应缺 `scope` 时沿用旧令牌的原始 `scope` 值）；成功返回 `AUTHORIZED`。失败时仅
//!      「signal 已取消」「非 HTTPS 端点」「非 `server_error` 的 `OAuthError`」重新抛出，
//!      其余（`server_error`、传输/网络错误、普通 `Error`/`TypeError`）继续走下一分支。
//!    - 否则：`start_authorization` 生成 PKCE 授权 URL，依次 `saveCodeVerifier`、
//!      `redirectToAuthorization`，返回 `REDIRECT`。回调换令牌依赖这里保存的 verifier。
//!
//! # 阅读顺序
//!
//! 先看 [`OAuthClientProvider`]（外部要实现的凭据面），再看单步协议
//! [`start_authorization`]/[`token_request`]/[`register_client`]，然后按上面的阶段读
//! [`run_flow`]，最后看 [`authorize_mcp`] 的重试与 [`adapt_oauth_provider`] 的并发 401 适配。
//!
//! # 改动影响面
//!
//! 公开名字/路径受 `tests/api_surface.rs` 的编译期 `use` 清单约束（`OAuthFlowOptions` 等 13 个
//! 在 `oauth` 的再导出面，`run_flow`/`token_request` 等按 `oauth::flow` 路径导入）。行为测试在
//! `tests/oauth.rs` 与本文件的 `tests` 子模块。仓库内走 OAuth 流程的生产实现是
//! `kiana-builtin-extensions` 的 `McpAuthProvider`（由 `create_mcp_auth_provider` 构造，
//! 其刷新直接调用 [`authorize_mcp`]）；`streamable_http` 只依赖 [`AuthProvider`] trait。
//! [`adapt_oauth_provider`] 是公开适配器，本仓库目前只有测试使用（如 `tests/oauth.rs`）。
//!
//! # `AbortSignal` → [`CancellationToken`]
//!
//! 所有请求都带 `signal`；被取消时 `fetch` 层报 `AbortError`。`runFlow` 的刷新分支在
//! `signal.is_cancelled()` 时重新抛出（**不**回落到授权重定向），与 TS 的
//! `stops when its signal aborts, without falling back to a redirect` 一致。
//!
//! # 已知偏差
//!
//! - **`AdaptedOAuthProvider::token()` 吞掉令牌存储的读取错误**。TS 是
//!   `async () => (await provider.tokens())?.access_token`：`tokens()` 的 rejection 会原样穿过
//!   `AuthProvider.token()`，`transport.headers()` 随之失败，请求以该错误终止。Rust 的
//!   [`AuthProvider::token`] 返回 `Option<String>`（`spec/telemetry-mcp.md` §12 冻结的签名），
//!   没有错误通道，因此 `tokens()` 的 `Err` 按「没有令牌」处理（`Option::ok().flatten()`）：
//!   请求不带 `Authorization` 发出，通常得到 401/`McpAuthRequiredError`（并可能触发一次重新授权），
//!   而不是原始的存储错误。自定义 `McpOAuthStateStore` 的 load 失败（文件 I/O 等）因此不可见。
//!
//! # `OAuthClientProvider` 的形状
//!
//! TS 的接口方法可选/可抛异常；Rust 侧：返回 `Result<_, OAuthFailure>`（抛出的异常原样穿过
//! `authorizeMcp`）。可选方法用 trait 缺省实现表达「未提供」，唯一需要显式判别的是
//! `saveClientInformation`（`runFlow` 在它缺失时报 `OAuth client information cannot be
//! persisted`），对应 [`OAuthClientProvider::has_save_client_information`]。
//! `addClientAuthentication` 在 Rust 侧是 [`OAuthClientProvider::add_client_authentication`]
//! 返回的 [`AddClientAuthentication`] 对象（TS 里是一个函数属性）。

use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use base64::Engine as _;
use futures::FutureExt as _;
use http::Method;
use kiana_json::js_string::{is_truthy, js_string};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::auth_provider::{
    AuthProvider, FetchBody, FetchInit, JsHeaders, McpFetch, McpTransportError, ReqwestFetch,
    UnauthorizedContext,
};
use crate::jsonrpc::is_object;
use crate::oauth::discovery::{
    DiscoverAuthorizationServerOptions, DiscoverOAuthServerInfoOptions,
    discover_authorization_server_metadata, discover_oauth_server_info, js_url,
    parse_www_authenticate, select_resource,
};
use crate::oauth::errors::{
    McpOAuthAuthorizationRequiredError, OAuthError, OAuthFailure, OAuthInsecureEndpointError,
    OAuthIssuerMismatchError, OAuthRegistrationError,
};
use crate::oauth::types::{
    AuthorizationServerMetadata, OAuthChallenge, OAuthClientInformation,
    OAuthClientInformationFull, OAuthClientInformationMixed, OAuthClientMetadata,
    OAuthDiscoveryState, OAuthServerInfo, OAuthTokens, is_http_url, parse_client_information,
    parse_oauth_tokens,
};

/// base64（标准字母表、带 padding）：`Buffer.from(...).toString("base64")`。
const BASE64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

/// base64url（无 padding）：`Buffer.from(...).toString("base64url")`。
const BASE64URL: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

// ---------------------------------------------------------------------------
// provider 契约（TS 的 `OAuthClientProvider`）
// ---------------------------------------------------------------------------

/// A Client ID Metadata Document: an https URL used as `client_id`, and a redirect URI it lists.
///
/// 字段本身不校验：[`run_flow`] 只在没有已存储的客户端信息时使用它。它先用 `new_url` 解析
/// `url`：解析失败得到 [`OAuthFailure::TypeError`]（`"Invalid URL"`）；解析成功但 scheme 不是
/// `https` 或 path 是 `/`（`https://client.example/` 会被拒）时报普通
/// `Error("Invalid OAuth client metadata URL")`。
///
/// TS: `oauth/flow.ts#OAuthClientMetadataDocument`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthClientMetadataDocument {
    /// `url`：作为 `client_id` 的 https URL。
    pub url: String,
    /// `redirectUrl`：文档里列出的重定向 URI（可能与 provider 自己的不同）。
    pub redirect_url: String,
}

/// `invalidateCredentials(kind)` 的参数（TS 的字面量联合）。
///
/// TS: `oauth/flow.ts#OAuthClientProvider.invalidateCredentials`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CredentialKind {
    /// `"all"`：客户端信息、令牌、code verifier、发现结果、`oauthState` 全部清除。
    All,
    /// `"client"`：只清客户端信息。
    Client,
    /// `"tokens"`：清令牌（含 `tokensExpireAt`）。
    Tokens,
    /// `"verifier"`：只清 PKCE verifier。
    Verifier,
    /// `"discovery"`：只清发现结果。
    Discovery,
}

impl CredentialKind {
    /// TS 的字面量值。
    pub fn as_str(self) -> &'static str {
        match self {
            CredentialKind::All => "all",
            CredentialKind::Client => "client",
            CredentialKind::Tokens => "tokens",
            CredentialKind::Verifier => "verifier",
            CredentialKind::Discovery => "discovery",
        }
    }
}

/// `AddClientAuthentication`：替换默认的客户端认证方式。
///
/// TS 是函数类型 `(headers, params, url, metadata?) => void | Promise<void>`，并且**不带接收者**
/// 调用（§30.19）。Rust 侧是对象安全的异步 trait；`params` 用 `(name, value)` 列表表示
/// `URLSearchParams`（`set` 语义由实现者自行复刻：同名原位替换、否则追加）。
///
/// TS: `oauth/flow.ts#AddClientAuthentication`。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait AddClientAuthentication: Send + Sync {
    /// 改写请求头与表单参数。
    async fn add_client_authentication(
        &self,
        headers: &mut JsHeaders,
        params: &mut Vec<(String, String)>,
        url: &Url,
        metadata: Option<&AuthorizationServerMetadata>,
    ) -> Result<(), OAuthFailure>;
}

/// `OAuthClientProvider`：`runFlow` 需要的凭据读写面（`oauth-provider` 实现它）。
///
/// TS 的方法大多同步或异步均可；Rust 侧一律异步，返回值失败即 TS 的抛异常
/// （`authorizeMcp` 只对 [`OAuthError`] 的三种 code 做重试，其余原样传播）。
///
/// 实现必须是 `Send + Sync` 且方法只拿 `&self`：[`adapt_oauth_provider`] 之后 provider 由
/// 传输层多个请求共享，`tokens()`/`save_tokens()` 可能被并发调用；共享状态需要实现者自行
/// 同步（`crate::oauth::provider` 的 `McpOAuthProvider` 用互斥量串行化持久化）。[`run_flow`]
/// 自身按顺序调用这些方法。
///
/// TS: `oauth/flow.ts#OAuthClientProvider`。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait OAuthClientProvider: Send + Sync {
    /// `readonly redirectUrl`：回环回调服务器的重定向 URI（已规范化的字符串）。
    fn redirect_url(&self) -> &str;

    /// `readonly clientMetadata`：注册时提交的客户端元数据。
    fn client_metadata(&self) -> &OAuthClientMetadata;

    /// `clientMetadataDocument?(metadata)`：没有存储的客户端信息时，用一个 CIMD URL 充当
    /// `client_id` 而不是动态注册。`metadata` 是发现到的授权服务器元数据（可能没有）。
    ///
    /// TS 里这个方法**同步**返回（不是 Promise），这里同样保持同步。
    /// 缺省返回 `None`（TS 里没有这个方法）。
    fn client_metadata_document(
        &self,
        _metadata: Option<&AuthorizationServerMetadata>,
    ) -> Result<Option<OAuthClientMetadataDocument>, OAuthFailure> {
        Ok(None)
    }

    /// `state?()`：授权请求的 `state`；缺省 `None`（不带 `state` 参数）。
    async fn state(&self) -> Result<Option<String>, OAuthFailure> {
        Ok(None)
    }

    /// `clientInformation()`：已存储/配置的客户端信息。
    async fn client_information(&self)
    -> Result<Option<OAuthClientInformationMixed>, OAuthFailure>;

    /// `saveClientInformation?(information)`：持久化动态注册的结果。
    ///
    /// 实现了本方法的类型必须让 [`Self::has_save_client_information`] 返回 `true`。
    async fn save_client_information(
        &self,
        _information: &OAuthClientInformationMixed,
    ) -> Result<(), OAuthFailure> {
        Ok(())
    }

    /// 是否实现了 [`Self::save_client_information`]（TS 的 `saveClientInformation !== undefined`）。
    ///
    /// `runFlow` 在需要注册但该方法缺失时报 `OAuth client information cannot be persisted`。
    fn has_save_client_information(&self) -> bool {
        false
    }

    /// `tokens()`：已存储的令牌。
    async fn tokens(&self) -> Result<Option<OAuthTokens>, OAuthFailure>;

    /// `saveTokens(tokens)`。
    async fn save_tokens(&self, _tokens: &OAuthTokens) -> Result<(), OAuthFailure>;

    /// `redirectToAuthorization(url)`：打开/记录授权 URL。
    async fn redirect_to_authorization(&self, _url: &Url) -> Result<(), OAuthFailure>;

    /// `saveCodeVerifier(verifier)`。
    async fn save_code_verifier(&self, _verifier: &str) -> Result<(), OAuthFailure>;

    /// `codeVerifier()`：取回 PKCE verifier；缺失时按 TS 抛
    /// `Error("No OAuth PKCE code verifier is stored")`。
    async fn code_verifier(&self) -> Result<String, OAuthFailure>;

    /// `addClientAuthentication?`：替换默认的客户端认证。
    fn add_client_authentication(&self) -> Option<Arc<dyn AddClientAuthentication>> {
        None
    }

    /// `invalidateCredentials?(kind)`：缺省 no-op（TS 里方法缺失等价于不做任何事）。
    async fn invalidate_credentials(&self, _kind: CredentialKind) -> Result<(), OAuthFailure> {
        Ok(())
    }

    /// `saveDiscoveryState?(state)`：缓存发现结果；缺省 no-op。
    async fn save_discovery_state(&self, _state: &OAuthDiscoveryState) -> Result<(), OAuthFailure> {
        Ok(())
    }

    /// `discoveryState?()`：读取缓存的发现结果；缺省 `None`。
    async fn discovery_state(&self) -> Result<Option<OAuthDiscoveryState>, OAuthFailure> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// 选项与结果
// ---------------------------------------------------------------------------

/// `OAuthFlowOptions`：`authorizeMcp` 的输入。
///
/// TS: `oauth/flow.ts#OAuthFlowOptions`。
#[derive(Clone, Default)]
pub struct OAuthFlowOptions {
    /// `serverUrl`：MCP 服务器 URL。
    pub server_url: String,
    /// `authorizationCode`：授权响应里的 code（有值时走 code exchange 而不是刷新/重定向）。
    pub authorization_code: Option<String>,
    /// `iss`：授权响应的 `iss` 参数（RFC 9207）。
    pub iss: Option<String>,
    /// `scope`：请求的 scope（空串按缺省处理）。
    pub scope: Option<String>,
    /// `resourceMetadataUrl`：显式给定的受保护资源元数据 URL。
    pub resource_metadata_url: Option<Url>,
    /// `authorizationServerMetadataUrl`：配置的授权服务器元数据文档（跳过发现、不校验 issuer）。
    ///
    /// 只跳过授权服务器的 well-known 发现：受保护资源发现仍会执行，且这次发现结果**不**写入
    /// 缓存（改 URL 立即生效）。非 https 且非 loopback 的 URL 在发出任何请求前就被拒。
    pub authorization_server_metadata_url: Option<Url>,
    /// `fetch`：HTTP 栈；`None` → [`ReqwestFetch`]。
    pub fetch: Option<Arc<dyn McpFetch>>,
    /// `signal`：中止整个流程的每个请求。
    pub signal: Option<CancellationToken>,
    /// `skipIssuerValidation`：跳过授权服务器元数据的 issuer 校验。
    pub skip_issuer_validation: bool,
    /// `skipRefresh`：不尝试刷新存储的令牌（例如服务器要求更大的 scope）。
    pub skip_refresh: bool,
}

impl std::fmt::Debug for OAuthFlowOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthFlowOptions")
            .field("server_url", &self.server_url)
            .field("authorization_code", &self.authorization_code)
            .field("iss", &self.iss)
            .field("scope", &self.scope)
            .field("resource_metadata_url", &self.resource_metadata_url)
            .field(
                "authorization_server_metadata_url",
                &self.authorization_server_metadata_url,
            )
            .field("fetch", &self.fetch.is_some())
            .field("signal", &self.signal.is_some())
            .field("skip_issuer_validation", &self.skip_issuer_validation)
            .field("skip_refresh", &self.skip_refresh)
            .finish()
    }
}

/// `OAuthFlowResult`：`"AUTHORIZED" | "REDIRECT"`。
///
/// TS: `oauth/flow.ts#OAuthFlowResult`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthFlowResult {
    /// `"AUTHORIZED"`：拿到了令牌。
    Authorized,
    /// `"REDIRECT"`：已把用户导向授权页（PKCE verifier 已保存）。
    Redirect,
}

impl OAuthFlowResult {
    /// TS 的字面量值。
    pub fn as_str(self) -> &'static str {
        match self {
            OAuthFlowResult::Authorized => "AUTHORIZED",
            OAuthFlowResult::Redirect => "REDIRECT",
        }
    }
}

/// `TokenRequestOptions`：令牌请求的公共选项。
///
/// TS: `oauth/flow.ts#TokenRequestOptions`。
#[derive(Clone, Default)]
pub struct TokenRequestOptions {
    /// `metadata`：授权服务器元数据（决定端点与支持的认证方式）。
    pub metadata: Option<AuthorizationServerMetadata>,
    /// `clientInformation`：客户端凭据。
    pub client_information: OAuthClientInformationMixed,
    /// `resource`：RFC 8707 的资源指示器（空串按缺省处理）。
    pub resource: Option<String>,
    /// `addClientAuthentication`：自定义客户端认证。
    pub add_client_authentication: Option<Arc<dyn AddClientAuthentication>>,
    /// `fetch`：HTTP 栈；`None` → [`ReqwestFetch`]。
    pub fetch: Option<Arc<dyn McpFetch>>,
    /// `signal`：取消信号。
    pub signal: Option<CancellationToken>,
}

impl std::fmt::Debug for TokenRequestOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenRequestOptions")
            .field("metadata", &self.metadata)
            .field("client_information", &self.client_information)
            .field("resource", &self.resource)
            .field(
                "add_client_authentication",
                &self.add_client_authentication.is_some(),
            )
            .field("fetch", &self.fetch.is_some())
            .field("signal", &self.signal.is_some())
            .finish()
    }
}

/// [`start_authorization`] 的选项（TS 的第二个参数）。
#[derive(Debug, Clone, Default)]
pub struct StartAuthorizationOptions {
    /// `metadata`：授权服务器元数据。
    pub metadata: Option<AuthorizationServerMetadata>,
    /// `clientInformation`：客户端凭据（只用 `client_id`）。
    pub client_information: OAuthClientInformationMixed,
    /// `redirectUrl`：`String(options.redirectUrl)`。
    pub redirect_url: String,
    /// `scope`：请求的 scope（空串按缺省处理）。
    pub scope: Option<String>,
    /// `state`：CSRF state（空串按缺省处理）。
    pub state: Option<String>,
    /// `resource`：资源指示器（空串按缺省处理）。
    pub resource: Option<String>,
}

/// [`start_authorization`] 的结果：`{authorizationUrl, codeVerifier}`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartAuthorizationResult {
    /// `authorizationUrl`：要打开的授权 URL（http(s)）。
    pub authorization_url: Url,
    /// `codeVerifier`：本次 PKCE 的 verifier（须保存到 code exchange）。
    pub code_verifier: String,
}

/// [`register_client`] 的选项（TS 的第二个参数）。
#[derive(Clone, Default)]
pub struct RegisterClientOptions {
    /// `metadata`：授权服务器元数据（决定注册端点与“是否支持 DCR”）。
    pub metadata: Option<AuthorizationServerMetadata>,
    /// `clientMetadata`：提交的客户端元数据。
    pub client_metadata: OAuthClientMetadata,
    /// `scope`：额外的 `scope` 字段（空串省略）。
    pub scope: Option<String>,
    /// `fetch`：HTTP 栈。
    pub fetch: Option<Arc<dyn McpFetch>>,
    /// `signal`：取消信号。
    pub signal: Option<CancellationToken>,
}

impl std::fmt::Debug for RegisterClientOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisterClientOptions")
            .field("metadata", &self.metadata)
            .field("client_metadata", &self.client_metadata)
            .field("scope", &self.scope)
            .field("fetch", &self.fetch.is_some())
            .field("signal", &self.signal.is_some())
            .finish()
    }
}

/// [`exchange_authorization_code`] 的选项（`TokenRequestOptions & {code, codeVerifier, redirectUrl}`）。
#[derive(Clone, Default)]
pub struct ExchangeAuthorizationCodeOptions {
    /// 令牌请求的公共选项。
    pub token: TokenRequestOptions,
    /// `code`：授权码。
    pub code: String,
    /// `codeVerifier`：PKCE verifier。
    pub code_verifier: String,
    /// `redirectUrl`：与授权请求相同的重定向 URI。
    pub redirect_url: String,
}

impl std::fmt::Debug for ExchangeAuthorizationCodeOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExchangeAuthorizationCodeOptions")
            .field("token", &self.token)
            .field("code", &self.code)
            .field("code_verifier", &self.code_verifier)
            .field("redirect_url", &self.redirect_url)
            .finish()
    }
}

/// [`refresh_authorization`] 的选项（`TokenRequestOptions & {refreshToken}`）。
#[derive(Clone, Default)]
pub struct RefreshAuthorizationOptions {
    /// 令牌请求的公共选项。
    pub token: TokenRequestOptions,
    /// `refreshToken`：用于刷新的令牌。
    pub refresh_token: String,
}

impl std::fmt::Debug for RefreshAuthorizationOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefreshAuthorizationOptions")
            .field("token", &self.token)
            .field("refresh_token", &self.refresh_token)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// 内部工具
// ---------------------------------------------------------------------------

/// 默认的 `McpFetch`（TS 的 `globalThis.fetch`；Rust 侧是 [`ReqwestFetch`]）。
///
/// `OnceLock` 保证进程内只构造一次：所有未显式传 `fetch` 的调用共享同一个实例；测试通过
/// `OAuthFlowOptions::fetch` 等选项注入自己的 [`McpFetch`]。
fn default_fetch() -> Arc<dyn McpFetch> {
    use std::sync::OnceLock;
    static DEFAULT: OnceLock<Arc<dyn McpFetch>> = OnceLock::new();
    Arc::clone(DEFAULT.get_or_init(|| ReqwestFetch::new().shared()))
}

/// `new URL(input)`：WHATWG 绝对 URL 解析，失败是 `TypeError: Invalid URL`。
fn new_url(input: &str) -> Result<Url, OAuthFailure> {
    js_url::parse(input).map_err(|_| OAuthFailure::type_error("Invalid URL"))
}

/// `new URL(relative, base)`：WHATWG relative-URL 解析，失败是 `TypeError: Invalid URL`。
fn new_url_relative(base: &Url, relative: &str) -> Result<Url, OAuthFailure> {
    js_url::parse_relative(base, relative).map_err(|_| OAuthFailure::type_error("Invalid URL"))
}

/// `value ?? fallback`：只有 `undefined`/`null` 是 nullish（JS 的 `??` 不会跳过 `""`、`0`、`false`）。
fn js_nullish(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| !value.is_null())
}

/// `new URL(value)`：参数先经 `String(value)` 强制转换（`new URL(5)`、`new URL(["https://x"])`）。
///
/// TS 的 `metadata?.token_endpoint ?? new URL("/token", …)` 把原始属性值直接交给 `new URL`；
/// 只有 nullish 才走回退路径（见 [`js_nullish`]）。
fn new_url_from_js(value: &Value) -> Result<Url, OAuthFailure> {
    new_url(&js_string(Some(value)))
}

/// `value.includes(needle)`：字符串是子串判定，数组是严格相等（`String.prototype.includes` /
/// `Array.prototype.includes`）；`undefined`/`null` 与其它类型按 V8 的 TypeError 逐字抛出。
///
/// `expression` 只用于复刻 V8 的 `"<expr>.includes is not a function"` 文案。
fn js_includes(
    value: Option<&Value>,
    needle: &str,
    expression: &str,
) -> Result<bool, OAuthFailure> {
    match value {
        None => Err(OAuthFailure::type_error(
            "Cannot read properties of undefined (reading 'includes')",
        )),
        Some(Value::Null) => Err(OAuthFailure::type_error(
            "Cannot read properties of null (reading 'includes')",
        )),
        Some(Value::String(text)) => Ok(text.contains(needle)),
        Some(Value::Array(items)) => Ok(items
            .iter()
            .any(|item| matches!(item, Value::String(text) if text == needle))),
        Some(_) => Err(OAuthFailure::type_error(format!(
            "{expression}.includes is not a function"
        ))),
    }
}

/// `value.length`：数组是元素数，字符串是 UTF-16 码元数；其余类型没有 `length`（`undefined`）。
fn js_length(value: Option<&Value>) -> Option<usize> {
    match value {
        Some(Value::Array(items)) => Some(items.len()),
        Some(Value::String(text)) => Some(text.encode_utf16().count()),
        _ => None,
    }
}

/// `\s`（JS 正则，不含 `u` 标志）：`[\t\n\v\f\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}
/// \u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]`。
///
/// 与 Rust 的 `char::is_whitespace` 不同：`U+0085`（NEL）**不是** JS 的 `\s`，`U+FEFF` 是。
/// 与 `oauth::discovery` 里的同名私有函数一致（避免跨单元改别人文件）。
fn is_js_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// `text.split(/\s+/)` 的等价物（空串得到空列表；JS 得到 `[""]`，但调用点都过滤空串或做
/// `includes`，两者等价）。
fn split_js_whitespace(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start: Option<usize> = None;
    for (index, ch) in text.char_indices() {
        if is_js_whitespace(ch) {
            if let Some(begin) = start.take() {
                parts.push(&text[begin..index]);
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(begin) = start {
        parts.push(&text[begin..]);
    }
    parts
}

/// `URLSearchParams.set(name, value)`：同名参数原位替换第一个、删除其余；不存在则追加。
fn params_set(params: &mut Vec<(String, String)>, name: &str, value: impl Into<String>) {
    let value = value.into();
    let mut replaced = false;
    params.retain_mut(|(key, item)| {
        if key == name {
            if replaced {
                return false;
            }
            replaced = true;
            *item = value.clone();
        }
        true
    });
    if !replaced {
        params.push((name.to_owned(), value));
    }
}

/// `url.searchParams.set(name, value)`：解析现有查询、`params_set` 后重新序列化
/// （`application/x-www-form-urlencoded`：空格写 `+`，`~!*'()` 等转义）。
///
/// `URLSearchParams` 的 `set` 会重写**整个**查询串，因此这里也必须解码后重新编码
/// （`%20` → `+`、`%2D` → `-`），而不是字符串拼接。
pub fn search_params_set(url: &mut Url, name: &str, value: &str) {
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, item)| (key.into_owned(), item.into_owned()))
        .collect();
    params_set(&mut pairs, name, value);
    if pairs.is_empty() {
        url.set_query(None);
        return;
    }
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(
            pairs
                .iter()
                .map(|(key, item)| (key.as_str(), item.as_str())),
        )
        .finish();
    url.set_query(Some(&query));
}

/// `loopback(hostname)`：`"localhost" | "127.0.0.1" | "[::1]" | "::1"`。
///
/// WHATWG 的 `URL.hostname` 对 IPv6 带方括号（`[::1]`），`url::Url::host_str()` 同样如此。
///
/// TS: `oauth/flow.ts#loopback`（模块内）。
pub fn loopback(hostname: &str) -> bool {
    matches!(hostname, "localhost" | "127.0.0.1" | "[::1]" | "::1")
}

/// `applicationType(redirect_uris)`：无法解析的 URI 被跳过；任一已解析 URI 的 scheme 非
/// http(s) 或 hostname 是 loopback → `"native"`，否则 `"web"`（MCP SEP-837）。
///
/// TS: `oauth/flow.ts#applicationType`（模块内）。
pub fn application_type(redirect_uris: &[&str]) -> &'static str {
    let native = redirect_uris.iter().any(|uri| {
        let Ok(url) = js_url::parse(uri) else {
            return false;
        };
        !matches!(url.scheme(), "http" | "https") || loopback(url.host_str().unwrap_or(""))
    });
    if native { "native" } else { "web" }
}

/// `secureEndpoint(value)`：非 https 且非 loopback → [`OAuthInsecureEndpointError`]。
///
/// TS: `oauth/flow.ts#secureEndpoint`（模块内）。
pub fn secure_endpoint(value: &str) -> Result<Url, OAuthFailure> {
    let url = new_url(value)?;
    if url.scheme() != "https" && !loopback(url.host_str().unwrap_or("")) {
        return Err(OAuthFailure::from(OAuthInsecureEndpointError::new(
            url.as_str(),
        )));
    }
    Ok(url)
}

/// `ClientAuthMethod`：令牌端点的客户端认证方式。
///
/// TS: `oauth/flow.ts#ClientAuthMethod`（模块内的类型别名）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientAuthMethod {
    /// `"client_secret_basic"`：`Authorization: Basic base64(client_id:client_secret)`。
    ClientSecretBasic,
    /// `"client_secret_post"`：`client_id` + `client_secret` 放在表单体里。
    ClientSecretPost,
    /// `"none"`：只放 `client_id`（公共客户端）。
    None,
}

impl ClientAuthMethod {
    /// TS 的字面量值。
    pub fn as_str(self) -> &'static str {
        match self {
            ClientAuthMethod::ClientSecretBasic => "client_secret_basic",
            ClientAuthMethod::ClientSecretPost => "client_secret_post",
            ClientAuthMethod::None => "none",
        }
    }
}

/// `information.client_secret` 的 JS 真值判定（空串、`0`、`null` 是假值；对象是真值）。
fn has_client_secret(information: &OAuthClientInformationMixed) -> bool {
    is_truthy(information.get("client_secret"))
}

/// `ClientAuthMethod` 的字面量值解析（只对三种已知取值调用）。
fn client_auth_method_from_str(method: &str) -> ClientAuthMethod {
    match method {
        "client_secret_basic" => ClientAuthMethod::ClientSecretBasic,
        "client_secret_post" => ClientAuthMethod::ClientSecretPost,
        _ => ClientAuthMethod::None,
    }
}

/// `selectClientAuthMethod(information, supported)`。
///
/// 顺序：提示值（在三种方法内且 `supported` 为空或包含它）→ `supported` 为空时按是否有 secret
/// 选 basic/none → basic → post → none → 兜底（有 secret 用 post，否则 none）。
///
/// TS: `oauth/flow.ts#selectClientAuthMethod`（模块内）。
pub fn select_client_auth_method(
    information: &OAuthClientInformationMixed,
    supported: &[&str],
) -> ClientAuthMethod {
    let supported: Vec<Value> = supported
        .iter()
        .map(|method| Value::String((*method).to_owned()))
        .collect();
    // 数组的 `length`/`includes` 永远可用，不会抛 TypeError。
    select_client_auth_method_js(information, Some(&Value::Array(supported)))
        .expect("string arrays never throw in selectClientAuthMethod")
}

/// `selectClientAuthMethod` 的 JS 值版本：`supported` 是 `?? []` 之后的原始属性值
/// （`undefined`/`null` 已变成 `None` = `[]`）。字符串的 `length`/`includes` 是 JS 语义
/// （空串等于空列表；非空串按子串匹配），其它类型按 V8 的 TypeError 抛出。
fn select_client_auth_method_js(
    information: &OAuthClientInformationMixed,
    supported: Option<&Value>,
) -> Result<ClientAuthMethod, OAuthFailure> {
    // `"token_endpoint_auth_method" in information ? information.token_endpoint_auth_method : undefined`
    // 再经 `["client_secret_basic", …].includes(hinted)`：只有这三个字符串能通过。
    let hinted = match information.get("token_endpoint_auth_method") {
        Some(Value::String(hinted))
            if matches!(
                hinted.as_str(),
                "client_secret_basic" | "client_secret_post" | "none"
            ) =>
        {
            Some(hinted.as_str())
        }
        _ => None,
    };
    // `supported.length === 0`：`None` 是 `?? []` 的结果；空串/空数组同样是空列表。
    let supported_empty = supported.is_none() || js_length(supported) == Some(0);
    if let Some(hinted) = hinted
        && (supported_empty || js_includes(supported, hinted, "supported")?)
    {
        return Ok(client_auth_method_from_str(hinted));
    }
    let secret = has_client_secret(information);
    if supported_empty {
        return Ok(if secret {
            ClientAuthMethod::ClientSecretBasic
        } else {
            ClientAuthMethod::None
        });
    }
    if secret && js_includes(supported, "client_secret_basic", "supported")? {
        return Ok(ClientAuthMethod::ClientSecretBasic);
    }
    if secret && js_includes(supported, "client_secret_post", "supported")? {
        return Ok(ClientAuthMethod::ClientSecretPost);
    }
    if js_includes(supported, "none", "supported")? {
        return Ok(ClientAuthMethod::None);
    }
    Ok(if secret {
        ClientAuthMethod::ClientSecretPost
    } else {
        ClientAuthMethod::None
    })
}

/// `applyClientAuthentication(method, information, headers, params)`。
///
/// basic 缺 secret（JS 真值判定）→ `Error("client_secret_basic requires a client secret")`；
/// basic 用标准 base64（带 padding）编码 UTF-8 的 `String(client_id):String(client_secret)`。
/// 其余方法把 `client_id` 的 `String()` 结果放进表单（缺失即 `"undefined"`），post 且 secret
/// 为真值时再放 `String(client_secret)`。
///
/// TS: `oauth/flow.ts#applyClientAuthentication`（模块内）。
pub fn apply_client_authentication(
    method: ClientAuthMethod,
    information: &OAuthClientInformation,
    headers: &mut JsHeaders,
    params: &mut Vec<(String, String)>,
) -> Result<(), OAuthFailure> {
    // 模板字符串与 `URLSearchParams.set` 都做 `String(value)` 强制转换。
    let client_id = js_string(information.get("client_id"));
    let secret = information.get("client_secret");
    if method == ClientAuthMethod::ClientSecretBasic {
        if !is_truthy(secret) {
            return Err(OAuthFailure::error(
                "client_secret_basic requires a client secret",
            ));
        }
        let encoded = BASE64.encode(format!("{client_id}:{}", js_string(secret)));
        headers.set("Authorization", format!("Basic {encoded}"))?;
        return Ok(());
    }
    params_set(params, "client_id", client_id);
    if method == ClientAuthMethod::ClientSecretPost && is_truthy(secret) {
        params_set(params, "client_secret", js_string(secret));
    }
    Ok(())
}

/// [`pkce`] 的结果：`{verifier, challenge}`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    /// `verifier`：32 随机字节的 base64url（43 字符）。
    pub verifier: String,
    /// `challenge`：`base64url(SHA-256(UTF-8(verifier)))`（无 padding）。
    pub challenge: String,
}

/// `pkce()`：`crypto.getRandomValues(new Uint8Array(32))` + `crypto.subtle.digest("SHA-256")`。
///
/// TS: `oauth/flow.ts#pkce`（模块内）。
pub fn pkce() -> Result<Pkce, OAuthFailure> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| {
        OAuthFailure::error(format!("Failed to generate random bytes: {error}"))
    })?;
    let verifier = BASE64URL.encode(bytes);
    let digest = Sha256::digest(verifier.as_bytes());
    Ok(Pkce {
        verifier,
        challenge: BASE64URL.encode(digest),
    })
}

/// `startAuthorization(authorizationServerUrl, options)`：构造 PKCE 授权 URL。
///
/// 校验顺序（逐字）：`response_types_supported` 必须含 `code`；`code_challenge_methods_supported`
/// 为**真值**时必须含 `S256`（`""`/`0`/缺失都跳过这一项）；端点必须 http(s)。参数顺序与
/// `set` 语义见模块文档。
///
/// TS: `oauth/flow.ts#startAuthorization`。
pub async fn start_authorization(
    authorization_server_url: impl AsRef<str>,
    options: StartAuthorizationOptions,
) -> Result<StartAuthorizationResult, OAuthFailure> {
    let metadata = options.metadata.as_ref();
    if let Some(metadata) = metadata {
        // TS: `metadata.response_types_supported.includes("code")` —— 原始值交给
        // `String.prototype.includes`/`Array.prototype.includes`，字段缺失或非字符串/数组是 V8 的
        // TypeError（不是普通 Error），这里照抄。
        let response_types = metadata.get("response_types_supported");
        if !js_includes(response_types, "code", "metadata.response_types_supported")? {
            return Err(OAuthFailure::error(
                "Authorization server does not support authorization codes",
            ));
        }
    }
    // `metadata?.code_challenge_methods_supported && !…includes("S256")`：只有真值才检查。
    let methods = metadata.and_then(|metadata| metadata.get("code_challenge_methods_supported"));
    if is_truthy(methods)
        && !js_includes(methods, "S256", "metadata.code_challenge_methods_supported")?
    {
        return Err(OAuthFailure::error(
            "Authorization server does not support PKCE S256",
        ));
    }
    // `new URL(metadata?.authorization_endpoint ?? new URL("/authorize", authorizationServerUrl))`
    let mut url =
        match metadata.and_then(|metadata| js_nullish(metadata.get("authorization_endpoint"))) {
            Some(endpoint) => new_url_from_js(endpoint)?,
            None => {
                let server = new_url(authorization_server_url.as_ref())?;
                new_url_relative(&server, "/authorize")?
            }
        };
    // 调用方用平台 URL 处理器打开它，`file:`/自定义协议会启动本地程序。
    if !is_http_url(&url) {
        return Err(OAuthFailure::error(format!(
            "Invalid authorization endpoint {}",
            url.as_str()
        )));
    }
    let pkce = pkce()?;
    // `url.searchParams.set("client_id", options.clientInformation.client_id)`：`String()` 语义。
    let client_id = js_string(options.client_information.get("client_id"));
    search_params_set(&mut url, "response_type", "code");
    search_params_set(&mut url, "client_id", &client_id);
    search_params_set(&mut url, "code_challenge", &pkce.challenge);
    search_params_set(&mut url, "code_challenge_method", "S256");
    search_params_set(&mut url, "redirect_uri", &options.redirect_url);
    if let Some(state) = options.state.as_deref().filter(|state| !state.is_empty()) {
        search_params_set(&mut url, "state", state);
    }
    if let Some(scope) = options.scope.as_deref().filter(|scope| !scope.is_empty()) {
        search_params_set(&mut url, "scope", scope);
        if split_js_whitespace(scope).contains(&"offline_access") {
            search_params_set(&mut url, "prompt", "consent");
        }
    }
    if let Some(resource) = options
        .resource
        .as_deref()
        .filter(|resource| !resource.is_empty())
    {
        search_params_set(&mut url, "resource", resource);
    }
    Ok(StartAuthorizationResult {
        authorization_url: url,
        code_verifier: pkce.verifier,
    })
}

/// `tokenRequest(authorizationServerUrl, options, params)`：POST 令牌端点。
///
/// body 里的 `error` 字符串**先于** HTTP 状态判定；否则非 ok →
/// `OAuthError("server_error", "HTTP {status}: {text}")`；成功 → `parseOAuthTokens`。
///
/// # 错误
///
/// 端点解析失败是 `TypeError("Invalid URL")`，非 https 且非 loopback 是
/// [`OAuthInsecureEndpointError`]（两者都在发请求前）。`fetch` 的传输/取消错误以
/// [`OAuthFailure::Transport`] 原样返回。响应体不是 JSON、不是对象或 `error` 不是字符串时
/// 忽略 body 错误、按 HTTP 状态判定；2xx 但 body 没有合法令牌时由 [`parse_oauth_tokens`]
/// 报 `Invalid OAuth token response` / `Invalid token_type` 等普通 `Error`。
///
/// TS: `oauth/flow.ts#tokenRequest`（模块内）。
pub async fn token_request(
    authorization_server_url: impl AsRef<str>,
    options: TokenRequestOptions,
    params: Vec<(String, String)>,
) -> Result<OAuthTokens, OAuthFailure> {
    let metadata = options.metadata.as_ref();
    // `secureEndpoint(metadata?.token_endpoint ?? new URL("/token", authorizationServerUrl))`：
    // 原始属性值经 `String()` 交给 `new URL`，只有 nullish 才走回退路径。
    let url = match metadata.and_then(|metadata| js_nullish(metadata.get("token_endpoint"))) {
        Some(endpoint) => secure_endpoint(&js_string(Some(endpoint)))?,
        None => {
            let server = new_url(authorization_server_url.as_ref())?;
            secure_endpoint(new_url_relative(&server, "/token")?.as_str())?
        }
    };
    let mut headers = JsHeaders::from_pairs([
        ("Accept", "application/json"),
        ("content-type", "application/x-www-form-urlencoded"),
    ])?;
    let mut params = params;
    if let Some(resource) = options
        .resource
        .as_deref()
        .filter(|resource| !resource.is_empty())
    {
        params_set(&mut params, "resource", resource);
    }
    match &options.add_client_authentication {
        Some(add) => {
            add.add_client_authentication(&mut headers, &mut params, &url, metadata)
                .await?;
        }
        None => {
            // `metadata?.token_endpoint_auth_methods_supported ?? []`：只有 nullish 变成空列表，
            // 其余原始值交给 JS 的 `.length`/`.includes` 语义（见 select_client_auth_method_js）。
            let supported = metadata.and_then(|metadata| {
                js_nullish(metadata.get("token_endpoint_auth_methods_supported"))
            });
            apply_client_authentication(
                select_client_auth_method_js(&options.client_information, supported)?,
                &options.client_information,
                &mut headers,
                &mut params,
            )?;
        }
    }
    let fetch = options.fetch.clone().unwrap_or_else(default_fetch);
    let init = FetchInit::with_method(Method::POST)
        .with_headers(headers)
        .with_body(FetchBody::Form(params))
        .with_signal(options.signal.clone());
    let mut response = fetch.fetch(&url, init).await?;
    let status = response.status();
    let ok = response.ok();
    let text = response.text().await?;
    // `JSON.parse(text)` 的失败被吞掉（value 保持 undefined）。
    let parsed = kiana_json::js_parse::parse(&text).ok();
    if let Some(value) = parsed.as_ref()
        && is_object(value)
        && let Some(error) = value.get("error").and_then(Value::as_str)
    {
        let description = value
            .get("error_description")
            .and_then(Value::as_str)
            .unwrap_or(error);
        let error_uri = value
            .get("error_uri")
            .and_then(Value::as_str)
            .map(str::to_owned);
        return Err(OAuthFailure::from(OAuthError::new(
            error,
            description,
            error_uri,
        )));
    }
    if !ok {
        return Err(OAuthFailure::from(OAuthError::new(
            "server_error",
            format!("HTTP {status}: {text}"),
            None,
        )));
    }
    parse_oauth_tokens(parsed.as_ref().unwrap_or(&Value::Null))
}

/// `registerClient(authorizationServerUrl, options)`：动态客户端注册（RFC 7591）。
///
/// 请求体 = `{...clientMetadata, application_type: application_type ??
/// applicationType(redirect_uris), ...(scope ? {scope} : {})}`（JS 键位语义见模块文档）。
/// 非 ok → [`OAuthRegistrationError`]；ok → `parseClientInformation(await response.json())`。
///
/// TS: `oauth/flow.ts#registerClient`。
pub async fn register_client(
    authorization_server_url: impl AsRef<str>,
    options: RegisterClientOptions,
) -> Result<OAuthClientInformationFull, OAuthFailure> {
    // `const endpoint = options.metadata?.registration_endpoint; if (options.metadata && !endpoint)`
    // —— 空串/0/false 都算“不支持 DCR”；非字符串的真值交给 `new URL(String(value))`。
    let endpoint = options
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("registration_endpoint"));
    if options.metadata.is_some() && !is_truthy(endpoint) {
        return Err(OAuthFailure::error(
            "Authorization server does not support dynamic client registration",
        ));
    }
    let url = match endpoint {
        Some(endpoint) => new_url_from_js(endpoint)?,
        None => {
            let server = new_url(authorization_server_url.as_ref())?;
            new_url_relative(&server, "/register")?
        }
    };
    let mut body = options.client_metadata.fields().clone();
    // `application_type ?? applicationType(redirect_uris)`：`??` 只跳过 null/undefined。
    let application_type = match body.get("application_type") {
        Some(value) if !value.is_null() => value.clone(),
        _ => match options.client_metadata.get("redirect_uris") {
            Some(Value::Array(items)) => {
                let redirect_uris: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
                Value::String(application_type(&redirect_uris).to_owned())
            }
            // `undefined.some(...)` / `null.some(...)` 在 TS 里是 TypeError。
            None => {
                return Err(OAuthFailure::type_error(
                    "Cannot read properties of undefined (reading 'some')",
                ));
            }
            Some(Value::Null) => {
                return Err(OAuthFailure::type_error(
                    "Cannot read properties of null (reading 'some')",
                ));
            }
            Some(_) => {
                return Err(OAuthFailure::type_error(
                    "redirectUris.some is not a function",
                ));
            }
        },
    };
    body.insert("application_type".to_owned(), application_type);
    if let Some(scope) = options.scope.as_deref().filter(|scope| !scope.is_empty()) {
        body.insert("scope".to_owned(), Value::String(scope.to_owned()));
    }
    // `JSON.stringify({...clientMetadata, application_type: …, ...(scope ? {scope} : {})})`：
    // 键序（整数样式键前置）与数字格式都必须走 JS 语义，不能直接 serde_json 序列化。
    let json = kiana_json::js_object::stringify(&Value::Object(body));
    let headers = JsHeaders::from_pairs([
        ("Accept", "application/json"),
        ("content-type", "application/json"),
    ])?;
    let fetch = options.fetch.clone().unwrap_or_else(default_fetch);
    let init = FetchInit::with_method(Method::POST)
        .with_headers(headers)
        .with_body(FetchBody::Text(json))
        .with_signal(options.signal.clone());
    let mut response = fetch.fetch(&url, init).await?;
    if !response.ok() {
        let status = response.status();
        let text = response.text().await?;
        return Err(OAuthFailure::from(OAuthRegistrationError::new(
            status, text,
        )));
    }
    let value = response.json().await?;
    parse_client_information(&value)
}

/// `exchangeAuthorizationCode(authorizationServerUrl, options & {code, codeVerifier,
/// redirectUrl})`：params 依次为 `grant_type=authorization_code`、`code`、`code_verifier`、
/// `redirect_uri`（`resource` 与客户端认证参数随后追加）。
///
/// TS: `oauth/flow.ts#exchangeAuthorizationCode`。
pub async fn exchange_authorization_code(
    authorization_server_url: impl AsRef<str>,
    options: ExchangeAuthorizationCodeOptions,
) -> Result<OAuthTokens, OAuthFailure> {
    let params = vec![
        ("grant_type".to_owned(), "authorization_code".to_owned()),
        ("code".to_owned(), options.code),
        ("code_verifier".to_owned(), options.code_verifier),
        ("redirect_uri".to_owned(), options.redirect_url),
    ];
    token_request(authorization_server_url, options.token, params).await
}

/// `refreshAuthorization(authorizationServerUrl, options & {refreshToken})`。
///
/// 结果是 `{refresh_token: options.refreshToken, ...serverTokens}`：服务端返回了 rotate 后的
/// `refresh_token` 时**服务端的值胜出**（键仍在第一位）；只在服务端未返回时才回退到传入值。
///
/// TS: `oauth/flow.ts#refreshAuthorization`。
pub async fn refresh_authorization(
    authorization_server_url: impl AsRef<str>,
    options: RefreshAuthorizationOptions,
) -> Result<OAuthTokens, OAuthFailure> {
    let params = vec![
        ("grant_type".to_owned(), "refresh_token".to_owned()),
        ("refresh_token".to_owned(), options.refresh_token.clone()),
    ];
    let tokens = token_request(authorization_server_url, options.token, params).await?;
    // `{ refresh_token: old, ...tokens }`：JS 的属性定义顺序与覆盖语义。
    let mut fields = Map::new();
    fields.insert(
        "refresh_token".to_owned(),
        Value::String(options.refresh_token),
    );
    for (key, value) in tokens.fields() {
        fields.insert(key.clone(), value.clone());
    }
    Ok(OAuthTokens::from_fields(fields))
}

/// `withScope(tokens, scope)` 的 JS 值版本：`scope` 是 `tokens.scope` 属性访问的原始值。
///
/// `tokens.scope === undefined && scope`：`null`/`""`/`0`/`false` 是假值；键存在（哪怕是
/// `null`）就不追加，因为 `=== undefined` 只认缺席。
fn with_scope_value(tokens: OAuthTokens, scope: Option<&Value>) -> OAuthTokens {
    if !is_truthy(scope) {
        return tokens;
    }
    if tokens.fields().contains_key("scope") {
        return tokens;
    }
    let mut fields = tokens.fields().clone();
    // `is_truthy` 为真保证 `Option` 是 `Some`。
    fields.insert(
        "scope".to_owned(),
        scope.expect("truthy scope is present").clone(),
    );
    OAuthTokens::from_fields(fields)
}

/// `withScope(tokens, scope)`：`tokens.scope === undefined && scope` 时补上 `scope`
/// （追加在最后一个键之后，与 `{...tokens, scope}` 一致）。
///
/// TS: `oauth/flow.ts#withScope`（模块内）。
pub fn with_scope(tokens: OAuthTokens, scope: Option<&str>) -> OAuthTokens {
    let scope = scope.map(|scope| Value::String(scope.to_owned()));
    with_scope_value(tokens, scope.as_ref())
}

/// `stepUpScope(granted, challenged)`：challenged 的 scope 加上已授予的 scope（SEP-2350）。
///
/// 没有 challenged（`undefined` 或空串）→ `None`（让流程选默认值）。两者按 `/\s+/` 切分、
/// 丢弃空串、`Set` 去重（**先 granted 后 challenged**，保留首次出现顺序）、以单个空格连接。
///
/// TS: `oauth/flow.ts#stepUpScope`。
///
/// ```
/// use kiana_mcp::oauth::flow::step_up_scope;
/// assert_eq!(
///     step_up_scope(Some("repo read:org"), Some("repo admin")).as_deref(),
///     Some("repo read:org admin")
/// );
/// assert_eq!(step_up_scope(Some("a"), None), None);
/// ```
pub fn step_up_scope(granted: Option<&str>, challenged: Option<&str>) -> Option<String> {
    let challenged = challenged.filter(|scope| !scope.is_empty())?;
    let mut scopes: Vec<&str> = Vec::new();
    for scope in [granted, Some(challenged)].into_iter().flatten() {
        for item in split_js_whitespace(scope) {
            if !scopes.contains(&item) {
                scopes.push(item);
            }
        }
    }
    Some(scopes.join(" "))
}

/// `granted?.scope` 的**原始值**（`OAuthTokens` 是宽松对象，`scope` 可以不是字符串）。
///
/// TS 把 `granted?.scope` 直接交给 `stepUpScope`：`scope?.split(/\s+/)` 只对 `null`/缺失
/// 短路，其他非字符串值抛 `TypeError: scope?.split is not a function`。类型化的
/// [`OAuthTokens::scope`] 把非字符串读成 `None`，会静默忽略——这里改为返回错误（评审
/// finding 5）。
///
/// TS: `oauth/flow.ts#stepUpScope`（`scope?.split(/\s+/)`）。
fn raw_scope_value(tokens: Option<&OAuthTokens>) -> Result<Option<String>, McpTransportError> {
    match tokens.and_then(|tokens| tokens.get("scope")) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(scope)) => Ok(Some(scope.clone())),
        Some(_) => Err(McpTransportError::network("scope?.split is not a function")),
    }
}

/// `access_token` 的 JS 归一化（[`AuthProvider::token`] 的 `Option<String>` 近似）。
///
/// TS 的 `AuthProvider.token` 类型是 `string | undefined`，但存储（`mcp-auth.json` / 自定义
/// store）是手写 JSON，运行期值可以是任意类型：传输层按**真值**决定是否加 `Authorization`
/// 头，`String()` 只发生在模板字符串里。Rust 的 `Option<String>` 只能表达两态——
/// 真值（非字符串经 `String()` 归一）与"没有令牌"（假值）。
fn normalized_access_token(tokens: &OAuthTokens) -> Option<String> {
    match tokens.get("access_token") {
        Some(Value::String(token)) => Some(token.clone()),
        // 非字符串真值经传输层的模板字符串转换；假值（`0`/`false`/`null`）当作没有令牌。
        Some(value) if is_truthy(Some(value)) => Some(js_string(Some(value))),
        _ => None,
    }
}

/// `runFlow(provider, options)`：发现 → 客户端信息/注册 → code exchange 或刷新或重定向。
///
/// 步骤顺序与回退规则遵循 §18.4（见模块文档）；`metadataUrl` 存在时跳过缓存的发现结果、
/// 也**不**调用 `saveDiscoveryState`。
///
/// # 错误
///
/// provider 方法返回的 `Err` 都原样传播（`discovery_state`、`client_information`、`tokens`、
/// `state`、`client_metadata_document`、各 `save_*`、`code_verifier`、
/// `redirect_to_authorization`），唯一例外是刷新分支里的 `save_tokens`：它的失败与本阶段的
/// 其他错误一样按回落规则处理。`iss` 不匹配是 [`OAuthIssuerMismatchError`]；CIMD 的 `url`
/// 解析失败（`new_url`）是 [`OAuthFailure::TypeError`]（`"Invalid URL"`），解析成功但 scheme
/// 不是 `https` 或 path 是 `/` 才是普通 `Error("Invalid OAuth client metadata URL")`；没有
/// 存储 verifier 时的错误由 provider 的 `code_verifier()` 决定
/// （`provider` 模块报 `No OAuth PKCE code verifier is stored`）。
///
/// TS: `oauth/flow.ts#runFlow`（模块内）。
pub async fn run_flow(
    provider: &dyn OAuthClientProvider,
    options: &OAuthFlowOptions,
) -> Result<OAuthFlowResult, OAuthFailure> {
    let authorization_code = options
        .authorization_code
        .as_deref()
        .filter(|code| !code.is_empty());
    let metadata_url = match &options.authorization_server_metadata_url {
        Some(url) => Some(secure_endpoint(url.as_str())?),
        None => None,
    };
    // 配置的元数据 URL 不缓存：改 URL 立即生效。
    let cached = if metadata_url.is_some() {
        None
    } else {
        provider.discovery_state().await?
    };
    let discovered = match cached.filter(|state| !state.authorization_server_url.is_empty()) {
        Some(cached) => {
            let metadata = match cached.authorization_server_metadata {
                Some(metadata) => Some(metadata),
                None => {
                    discover_authorization_server_metadata(
                        &cached.authorization_server_url,
                        &DiscoverAuthorizationServerOptions {
                            fetch: options.fetch.clone(),
                            protocol_version: None,
                            skip_issuer_validation: options.skip_issuer_validation,
                            signal: options.signal.clone(),
                        },
                    )
                    .await?
                }
            };
            OAuthServerInfo {
                authorization_server_url: cached.authorization_server_url,
                authorization_server_metadata: metadata,
                resource_metadata: cached.resource_metadata,
            }
        }
        None => {
            discover_oauth_server_info(
                &options.server_url,
                &DiscoverOAuthServerInfoOptions {
                    resource_metadata_url: options.resource_metadata_url.clone(),
                    authorization_server_metadata_url: metadata_url.clone(),
                    fetch: options.fetch.clone(),
                    skip_issuer_validation: options.skip_issuer_validation,
                    signal: options.signal.clone(),
                },
            )
            .await?
        }
    };
    if metadata_url.is_none() {
        provider
            .save_discovery_state(&OAuthDiscoveryState {
                authorization_server_url: discovered.authorization_server_url.clone(),
                authorization_server_metadata: discovered.authorization_server_metadata.clone(),
                resource_metadata: discovered.resource_metadata.clone(),
                resource_metadata_url: options
                    .resource_metadata_url
                    .as_ref()
                    .map(|url| url.as_str().to_owned()),
            })
            .await?;
    }
    let metadata = discovered.authorization_server_metadata.as_ref();
    let resource = select_resource(&options.server_url, discovered.resource_metadata.as_ref())?;
    // `||` 而不是 `??`：空串（例如 `scopes_supported: []` 拼出的 `""`）继续下落。
    let scope = options
        .scope
        .clone()
        .filter(|scope| !scope.is_empty())
        .or_else(|| {
            discovered
                .resource_metadata
                .as_ref()
                .and_then(|metadata| metadata.scopes_supported())
                .map(|scopes| scopes.join(" "))
                .filter(|scope| !scope.is_empty())
        })
        .or_else(|| {
            // `||` 同样只看真值；非字符串的真值经 `String()` 进入后续的 `set`/JSON 序列化。
            let scope = provider.client_metadata().get("scope");
            if is_truthy(scope) {
                Some(js_string(scope))
            } else {
                None
            }
        });
    let stored = provider.client_information().await?;
    let client_document = if stored.is_some() {
        None
    } else {
        provider.client_metadata_document(metadata)?
    };
    if let Some(document) = &client_document {
        let url = new_url(&document.url)?;
        if url.scheme() != "https" || url.path() == "/" {
            return Err(OAuthFailure::error("Invalid OAuth client metadata URL"));
        }
    }
    let mut client = match stored {
        Some(stored) => Some(stored),
        None => client_document.as_ref().map(|document| {
            let mut fields = Map::new();
            fields.insert("client_id".to_owned(), Value::String(document.url.clone()));
            OAuthClientInformationMixed::from_fields(fields)
        }),
    };
    if client.is_none() {
        if authorization_code.is_some() {
            return Err(OAuthFailure::error(
                "OAuth client information is missing during code exchange",
            ));
        }
        if !provider.has_save_client_information() {
            return Err(OAuthFailure::error(
                "OAuth client information cannot be persisted",
            ));
        }
        let registered = register_client(
            &discovered.authorization_server_url,
            RegisterClientOptions {
                metadata: discovered.authorization_server_metadata.clone(),
                client_metadata: provider.client_metadata().clone(),
                scope: scope.clone(),
                fetch: options.fetch.clone(),
                signal: options.signal.clone(),
            },
        )
        .await?;
        provider.save_client_information(&registered).await?;
        client = Some(registered);
    }
    let client = client.expect("client is set by registration or provided by the store");
    // 文档里的重定向 URI 可能与 provider 的不同（例如服务器特定的路径）。
    let redirect_url = client_document
        .as_ref()
        .map(|document| document.redirect_url.clone())
        .unwrap_or_else(|| provider.redirect_url().to_owned());
    let token_options = TokenRequestOptions {
        metadata: discovered.authorization_server_metadata.clone(),
        client_information: client.clone(),
        resource: resource.clone(),
        add_client_authentication: provider.add_client_authentication(),
        fetch: options.fetch.clone(),
        signal: options.signal.clone(),
    };
    if let Some(code) = authorization_code {
        // RFC 9207：绝不把别的授权服务器的 code 发给这一台。没有元数据时没有可比的 issuer，
        // 任何 `iss` 都被拒。
        let iss = options.iss.as_deref();
        let expected = metadata.and_then(AuthorizationServerMetadata::issuer);
        // `metadata?.authorization_response_iss_parameter_supported` 是**真值**判定：
        // 字符串 `"false"`、数字 `1` 也算“支持”（TS 没有 typeof 检查）。
        let iss_supported =
            is_truthy(metadata.and_then(|metadata| {
                metadata.get("authorization_response_iss_parameter_supported")
            }));
        if (iss.is_some() || iss_supported) && iss != expected {
            return Err(OAuthFailure::from(OAuthIssuerMismatchError::new(
                expected.map(str::to_owned),
                iss.map(str::to_owned),
            )));
        }
        let code_verifier = provider.code_verifier().await?;
        let tokens = exchange_authorization_code(
            &discovered.authorization_server_url,
            ExchangeAuthorizationCodeOptions {
                token: token_options,
                code: code.to_owned(),
                code_verifier,
                redirect_url,
            },
        )
        .await?;
        // 响应没有 `scope` 时视为授予了请求的 scope（RFC 6749 §5.1）。
        provider
            .save_tokens(&with_scope(tokens, scope.as_deref()))
            .await?;
        return Ok(OAuthFlowResult::Authorized);
    }
    let existing = if options.skip_refresh {
        None
    } else {
        provider.tokens().await?
    };
    // `if (existing?.refresh_token)`：JS 真值判定（`0`/`""`/`null` 为假，非字符串真值也算有）。
    if let Some(existing) = existing
        && is_truthy(existing.get("refresh_token"))
    {
        // `URLSearchParams({refresh_token: options.refreshToken})` 的 `String()` 语义。
        let refresh_token = js_string(existing.get("refresh_token"));
        // TS 的 try 块同时包住 `refreshAuthorization` 与 `provider.saveTokens`，
        // 因此保存失败也走同一套回落规则。
        let outcome = match refresh_authorization(
            &discovered.authorization_server_url,
            RefreshAuthorizationOptions {
                token: token_options,
                refresh_token,
            },
        )
        .await
        {
            // 没有 `scope` 的刷新保持原授权的 scope（RFC 6749 §6）。
            Ok(tokens) => {
                provider
                    .save_tokens(&with_scope_value(tokens, existing.get("scope")))
                    .await
            }
            Err(error) => Err(error),
        };
        match outcome {
            Ok(()) => return Ok(OAuthFlowResult::Authorized),
            Err(error) => {
                let aborted = options
                    .signal
                    .as_ref()
                    .is_some_and(CancellationToken::is_cancelled);
                if aborted
                    || error.as_insecure_endpoint().is_some()
                    || error
                        .as_oauth()
                        .is_some_and(|oauth| oauth.code != "server_error")
                {
                    return Err(error);
                }
                // 其余错误（`server_error` 与网络失败）回落到新的授权流程。
            }
        }
    }
    let state = provider.state().await?;
    let authorization = start_authorization(
        &discovered.authorization_server_url,
        StartAuthorizationOptions {
            metadata: discovered.authorization_server_metadata.clone(),
            client_information: client,
            redirect_url,
            scope,
            state,
            resource,
        },
    )
    .await?;
    provider
        .save_code_verifier(&authorization.code_verifier)
        .await?;
    provider
        .redirect_to_authorization(&authorization.authorization_url)
        .await?;
    Ok(OAuthFlowResult::Redirect)
}

/// `authorizeMcp(provider, options)`：运行 [`run_flow`]，错误时最多重试一次。
///
/// `OAuthError` 且 code 是 `invalid_client`/`unauthorized_client` →
/// `invalidateCredentials("all")` 后重跑；`invalid_grant` → `invalidateCredentials("tokens")`
/// 后重跑；其他错误原样传播（第二次失败传播第二个错误）。
///
/// `invalidateCredentials` 自身失败时直接返回该错误（`?` 在重跑之前），不会再跑第二次。
///
/// TS: `oauth/flow.ts#authorizeMcp`。
pub async fn authorize_mcp(
    provider: &dyn OAuthClientProvider,
    options: &OAuthFlowOptions,
) -> Result<OAuthFlowResult, OAuthFailure> {
    match run_flow(provider, options).await {
        Ok(result) => Ok(result),
        Err(error) => {
            let invalidation = match error.as_oauth() {
                Some(oauth)
                    if oauth.code == "invalid_client" || oauth.code == "unauthorized_client" =>
                {
                    Some(CredentialKind::All)
                }
                Some(oauth) if oauth.code == "invalid_grant" => Some(CredentialKind::Tokens),
                _ => None,
            };
            match invalidation {
                Some(kind) => {
                    provider.invalidate_credentials(kind).await?;
                    run_flow(provider, options).await
                }
                None => Err(error),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// adaptOAuthProvider
// ---------------------------------------------------------------------------

/// 共享的一次刷新：并发 401 都 await 同一个 `Shared` future。
type SharedFlight =
    futures::future::Shared<futures::future::BoxFuture<'static, Result<(), McpTransportError>>>;

/// `adaptOAuthProvider(provider)` 返回的 [`AuthProvider`]。
///
/// `in_flight` 最多保存一次进行中的刷新：第一个 401 在锁内放入，后续并发 401 克隆同一个
/// `Shared` future 并等待。锁内只读写槽位；首个 flight 的构造（`start_flight`，含
/// `Handle::try_current()` 与 `Handle::spawn`）也在锁内，但锁内不 `await`，`flight.await`
/// 在锁外；槽位由 flight 自己在结束时清空（有 tokio 运行时时，后台驱动任务持有一份
/// clone，发起者被丢弃也不会漏掉这次刷新）。
struct AdaptedOAuthProvider {
    provider: Arc<dyn OAuthClientProvider>,
    in_flight: Arc<Mutex<Option<SharedFlight>>>,
}

/// 取 `in_flight` 槽位；互斥锁中毒时继续使用内部值。
///
/// 这个锁保护的只是 `Option<SharedFlight>` 的读取、替换与清空；首个 flight 的构造与后台
/// spawn 也发生在持锁期间（见 `AdaptedOAuthProvider::start_flight`），但锁内从不 `await`。
/// 槽位没有需要回滚的中间状态，因此某个持有者 panic 后没有理由拒绝后续调用（TS 没有锁，
/// 也不会出现这种状态）。
fn lock_in_flight(slot: &Mutex<Option<SharedFlight>>) -> MutexGuard<'_, Option<SharedFlight>> {
    slot.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl AdaptedOAuthProvider {
    /// 启动一次共享的 `authorizeMcp` 链（`Promise.resolve(...).then(...).finally(...)`）。
    fn start_flight(
        &self,
        context: UnauthorizedContext,
        insufficient_scope: bool,
        challenge: OAuthChallenge,
    ) -> SharedFlight {
        let provider = Arc::clone(&self.provider);
        let slot = Arc::clone(&self.in_flight);
        let server_url = context.server_url.as_str().to_owned();
        let fetch = context.fetch;
        let resource_metadata_url = challenge.resource_metadata_url;
        let challenge_scope = challenge.scope;
        let work = async move {
            // `Promise.resolve(insufficientScope ? provider.tokens() : undefined)`
            let granted = if insufficient_scope {
                provider.tokens().await.map_err(McpTransportError::from)?
            } else {
                None
            };
            let scope = if insufficient_scope {
                // TS 的 `stepUpScope(granted?.scope, challenge.scope)`：`if (!challenged) return
                // undefined` 在**读取 granted 之前**短路，所以只有 challenged 非空时才会碰
                // `granted.scope`（非字符串在那里抛 TypeError）。
                match challenge_scope.as_deref().filter(|scope| !scope.is_empty()) {
                    Some(challenged) => step_up_scope(
                        raw_scope_value(granted.as_ref())?.as_deref(),
                        Some(challenged),
                    ),
                    None => None,
                }
            } else {
                challenge_scope
            };
            let options = OAuthFlowOptions {
                server_url,
                authorization_code: None,
                iss: None,
                scope,
                resource_metadata_url,
                authorization_server_metadata_url: None,
                fetch: Some(fetch),
                signal: None,
                skip_issuer_validation: false,
                skip_refresh: insufficient_scope,
            };
            match authorize_mcp(provider.as_ref(), &options).await {
                Ok(OAuthFlowResult::Authorized) => Ok(()),
                Ok(OAuthFlowResult::Redirect) => Err(McpTransportError::from(OAuthFailure::from(
                    McpOAuthAuthorizationRequiredError::new(),
                ))),
                Err(error) => Err(McpTransportError::from(error)),
            }
        };
        // `.finally(() => { inFlight = undefined })`：flight 结束后清空槽位（只可能是它自己）。
        let flight = async move {
            let result = work.await;
            *lock_in_flight(&slot) = None;
            result
        }
        .boxed()
        .shared();
        // TS 的 `inFlight` 是**已经启动**的 promise（`Promise.resolve(...).then(...)`）：即使
        // 发起它的 `onUnauthorized` 被丢弃（请求被 abort），刷新与 `saveTokens` 也会跑完。
        // Rust 的 future 是惰性的，因此用一个后台任务驱动共享 future；没有 tokio 运行时时
        // 退回惰性语义（与修复前一致）。
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let driver = flight.clone();
            handle.spawn(async move {
                let _ = driver.await;
            });
        }
        flight
    }
}

#[allow(clippy::double_must_use)]
#[async_trait]
impl AuthProvider for AdaptedOAuthProvider {
    /// `token: async () => (await provider.tokens())?.access_token`。
    ///
    /// TS 的 rejection 在 Rust 的 `AuthProvider::token` 签名里没有出口（返回
    /// `Option<String>`），因此读取失败按「没有令牌」处理（见模块文档「已知偏差」一节的
    /// 第一条）。
    /// 非字符串的 `access_token` 走 JS 真值与 `String()` 语义：真值经传输层的模板字符串
    /// 转换，假值（`0`、`false`、`null`）在这里变成 `None`；空串仍以 `Some("")` 返回，由传输层
    /// `headers()` 的 `!token.is_empty()` 过滤掉（对出站请求等同于没有令牌，§30.14）。
    async fn token(&self) -> Option<String> {
        let tokens = self.provider.tokens().await.ok().flatten()?;
        normalized_access_token(&tokens)
    }

    /// 401 处理器：并发 401 共享一次刷新；令牌已被别的请求换掉时不重复刷新；
    /// `insufficient_scope` 走 step-up 授权；需要用户交互时抛
    /// [`McpOAuthAuthorizationRequiredError`] 等价的传输错误。
    async fn on_unauthorized(&self, context: UnauthorizedContext) -> Result<(), McpTransportError> {
        let challenge = parse_www_authenticate(context.header("www-authenticate").as_deref());
        let insufficient_scope = challenge.error.as_deref() == Some("insufficient_scope");
        // TS 的顺序：先看有没有 in-flight，再读当前令牌。
        if !insufficient_scope && !self.has_in_flight() && context.token.is_some() {
            let current = self
                .provider
                .tokens()
                .await
                .map_err(McpTransportError::from)?;
            // TS：`const current = (await provider.tokens())?.access_token;`
            //     `if (current !== undefined && current !== context.token) return;`
            // `current !== undefined` 是**属性存在**（值可以是 `null`/数字/对象），不是
            // "非空字符串"：修复前用 `access_token()` 读，非字符串值被当成缺失，于是多刷新
            // 一次（评审 finding 5）。这里先看原始键是否存在，再按 [`Self::token`] 的同一套
            // 归一化与 `context.token` 比较。
            let current_defined = current
                .as_ref()
                .is_some_and(|tokens| tokens.fields().contains_key("access_token"));
            if current_defined {
                let current = current.as_ref().and_then(normalized_access_token);
                // 另一个请求已经刷新过：当前令牌与刚被拒的不同，直接重试即可。
                if current != context.token {
                    return Ok(());
                }
            }
        }
        let flight = {
            let mut slot = lock_in_flight(&self.in_flight);
            match slot.as_ref() {
                Some(flight) => flight.clone(),
                None => {
                    let flight = self.start_flight(context, insufficient_scope, challenge);
                    *slot = Some(flight.clone());
                    flight
                }
            }
        };
        flight.await
    }

    fn has_on_unauthorized(&self) -> bool {
        true
    }
}

impl AdaptedOAuthProvider {
    fn has_in_flight(&self) -> bool {
        lock_in_flight(&self.in_flight).is_some()
    }
}

/// `adaptOAuthProvider(provider)`：把 [`OAuthClientProvider`] 适配成传输层的 [`AuthProvider`]。
///
/// 并发 401 共享**一次**刷新（`inFlight`），令牌已被替换时直接返回；`insufficient_scope`
/// 用 step-up 授权并跳过刷新；`REDIRECT` → [`McpOAuthAuthorizationRequiredError`]。
///
/// TS: `oauth/flow.ts#adaptOAuthProvider`。
pub fn adapt_oauth_provider(provider: Arc<dyn OAuthClientProvider>) -> Arc<dyn AuthProvider> {
    Arc::new(AdaptedOAuthProvider {
        provider,
        in_flight: Arc::new(Mutex::new(None)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use http::{HeaderMap, HeaderName, HeaderValue};
    use serde_json::json;

    use crate::auth_provider::FetchResponse;
    use crate::oauth::types::{
        OAuthProtectedResourceMetadata, parse_authorization_server_metadata, parse_oauth_tokens,
    };

    // -----------------------------------------------------------------------
    // fetch 测试替身
    // -----------------------------------------------------------------------

    /// 一次被记录下来的请求（`FetchInit` 的可观察形态）。
    #[derive(Debug, Clone)]
    struct RecordedRequest {
        url: String,
        method: String,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl RecordedRequest {
        fn path(&self) -> String {
            Url::parse(&self.url)
                .map(|url| url.path().to_owned())
                .unwrap_or_default()
        }

        fn body_json(&self) -> Value {
            serde_json::from_str(&self.body).expect("JSON request body")
        }

        fn body_params(&self) -> Vec<(String, String)> {
            url::form_urlencoded::parse(self.body.as_bytes())
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect()
        }

        fn param(&self, name: &str) -> Option<String> {
            self.body_params()
                .into_iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value)
        }

        fn header(&self, name: &str) -> Option<String> {
            self.headers
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    type Responder = dyn Fn(&RecordedRequest) -> (u16, String) + Send + Sync;

    /// 按闭包应答并记录请求的 [`McpFetch`]。
    struct MockFetch {
        requests: Arc<StdMutex<Vec<RecordedRequest>>>,
        respond: Arc<Responder>,
    }

    impl MockFetch {
        fn new(
            respond: impl Fn(&RecordedRequest) -> (u16, String) + Send + Sync + 'static,
        ) -> Arc<MockFetch> {
            Arc::new(MockFetch {
                requests: Arc::new(StdMutex::new(Vec::new())),
                respond: Arc::new(respond),
            })
        }

        fn requests(&self) -> Vec<RecordedRequest> {
            self.requests.lock().unwrap().clone()
        }

        fn urls(&self) -> Vec<String> {
            self.requests()
                .into_iter()
                .map(|request| request.url)
                .collect()
        }

        fn shared(self: &Arc<Self>) -> Arc<dyn McpFetch> {
            Arc::clone(self) as Arc<dyn McpFetch>
        }
    }

    #[allow(clippy::double_must_use)]
    #[async_trait]
    impl McpFetch for MockFetch {
        async fn fetch(
            &self,
            url: &Url,
            init: FetchInit,
        ) -> Result<FetchResponse, McpTransportError> {
            let recorded = RecordedRequest {
                url: url.as_str().to_owned(),
                method: init.method.as_str().to_owned(),
                headers: init
                    .headers
                    .iter()
                    .map(|(name, value)| (name.to_owned(), value.to_owned()))
                    .collect(),
                body: init
                    .body
                    .as_ref()
                    .map(|body| String::from_utf8_lossy(&body.to_bytes()).into_owned())
                    .unwrap_or_default(),
            };
            let (status, text) = (self.respond)(&recorded);
            self.requests.lock().unwrap().push(recorded);
            Ok(FetchResponse::from_bytes(
                status,
                HeaderMap::new(),
                text.into_bytes(),
            ))
        }
    }

    /// 永不主动应答的 fetch：收到 signal 后报 `AbortError`（模拟被取消的网络请求）。
    struct StallingFetch {
        urls: Arc<StdMutex<Vec<String>>>,
    }

    impl StallingFetch {
        fn new() -> Arc<StallingFetch> {
            Arc::new(StallingFetch {
                urls: Arc::new(StdMutex::new(Vec::new())),
            })
        }

        fn urls(&self) -> Vec<String> {
            self.urls.lock().unwrap().clone()
        }

        fn shared(self: &Arc<Self>) -> Arc<dyn McpFetch> {
            Arc::clone(self) as Arc<dyn McpFetch>
        }
    }

    #[allow(clippy::double_must_use)]
    #[async_trait]
    impl McpFetch for StallingFetch {
        async fn fetch(
            &self,
            url: &Url,
            init: FetchInit,
        ) -> Result<FetchResponse, McpTransportError> {
            self.urls.lock().unwrap().push(url.as_str().to_owned());
            match init.signal {
                Some(signal) => {
                    signal.cancelled().await;
                    Err(McpTransportError::aborted())
                }
                None => Ok(FetchResponse::from_bytes(404, HeaderMap::new(), Vec::new())),
            }
        }
    }

    /// 先睡一会儿再应答（并发刷新测试用）。
    struct DelayedFetch {
        inner: Arc<MockFetch>,
        delay: Duration,
    }

    #[allow(clippy::double_must_use)]
    #[async_trait]
    impl McpFetch for DelayedFetch {
        async fn fetch(
            &self,
            url: &Url,
            init: FetchInit,
        ) -> Result<FetchResponse, McpTransportError> {
            tokio::time::sleep(self.delay).await;
            self.inner.fetch(url, init).await
        }
    }

    /// 记录请求后一直挂起，直到测试放行（模拟“刷新已发出、调用方却被丢弃”）。
    struct GatedFetch {
        requests: Arc<StdMutex<Vec<RecordedRequest>>>,
        gate: Arc<tokio::sync::Semaphore>,
        status: u16,
        body: String,
    }

    #[allow(clippy::double_must_use)]
    #[async_trait]
    impl McpFetch for GatedFetch {
        async fn fetch(
            &self,
            url: &Url,
            init: FetchInit,
        ) -> Result<FetchResponse, McpTransportError> {
            let recorded = RecordedRequest {
                url: url.as_str().to_owned(),
                method: init.method.as_str().to_owned(),
                headers: init
                    .headers
                    .iter()
                    .map(|(name, value)| (name.to_owned(), value.to_owned()))
                    .collect(),
                body: init
                    .body
                    .as_ref()
                    .map(|body| String::from_utf8_lossy(&body.to_bytes()).into_owned())
                    .unwrap_or_default(),
            };
            self.requests.lock().unwrap().push(recorded);
            let _permit = self.gate.acquire().await.expect("gate stays open");
            Ok(FetchResponse::from_bytes(
                self.status,
                HeaderMap::new(),
                self.body.clone().into_bytes(),
            ))
        }
    }

    // -----------------------------------------------------------------------
    // provider 测试替身
    // -----------------------------------------------------------------------

    #[derive(Default)]
    struct ProviderState {
        client: Option<OAuthClientInformationMixed>,
        tokens: Option<OAuthTokens>,
        verifier: Option<String>,
        discovery: Option<OAuthDiscoveryState>,
        authorization_url: Option<Url>,
        invalidations: Vec<CredentialKind>,
        saved_clients: Vec<OAuthClientInformationMixed>,
        saved_tokens: Vec<OAuthTokens>,
        saved_discovery: Vec<OAuthDiscoveryState>,
        saved_verifiers: Vec<String>,
        state: Option<String>,
        client_document: Option<OAuthClientMetadataDocument>,
        can_save_client: bool,
        fail_save_tokens: bool,
        /// `tokens()` 读取失败（模拟 store 的 load 错误）。
        fail_tokens_load: bool,
    }

    struct TestProvider {
        redirect_url: String,
        client_metadata: OAuthClientMetadata,
        data: StdMutex<ProviderState>,
    }

    impl TestProvider {
        fn new() -> TestProvider {
            TestProvider::with_redirect("http://127.0.0.1/callback")
        }

        fn with_redirect(redirect_url: &str) -> TestProvider {
            let client_metadata = OAuthClientMetadata::from_fields(
                match json!({
                    "redirect_uris": [redirect_url],
                    "client_name": "kiana-mcp-test",
                    "grant_types": ["authorization_code", "refresh_token"],
                    "response_types": ["code"],
                    "token_endpoint_auth_method": "none",
                }) {
                    Value::Object(map) => map,
                    _ => unreachable!(),
                },
            );
            TestProvider {
                redirect_url: redirect_url.to_owned(),
                client_metadata,
                data: StdMutex::new(ProviderState::default()),
            }
        }

        fn update(&self, update: impl FnOnce(&mut ProviderState)) -> &Self {
            update(&mut self.data.lock().unwrap());
            self
        }

        fn set_client(&self, value: Value) -> &Self {
            let client = OAuthClientInformation::from_fields(match value {
                Value::Object(map) => map,
                _ => unreachable!(),
            });
            self.update(|state| state.client = Some(client))
        }

        fn set_tokens(&self, value: Value) -> &Self {
            let tokens = parse_oauth_tokens(&value).expect("valid tokens");
            self.update(|state| state.tokens = Some(tokens))
        }

        /// 绕过 `parseOAuthTokens` 的严格校验，直接放一份原始令牌对象（手写 `mcp-auth.json`
        /// 或自定义 store 里可以是任意 JSON）。
        fn set_raw_tokens(&self, value: Value) -> &Self {
            let Value::Object(fields) = value else {
                panic!("raw tokens must be an object")
            };
            let tokens = OAuthTokens::from_fields(fields);
            self.update(|state| state.tokens = Some(tokens))
        }

        fn set_discovery(&self, value: OAuthDiscoveryState) -> &Self {
            self.update(|state| state.discovery = Some(value))
        }

        fn tokens(&self) -> Option<OAuthTokens> {
            self.data.lock().unwrap().tokens.clone()
        }

        fn authorization_url(&self) -> Option<Url> {
            self.data.lock().unwrap().authorization_url.clone()
        }

        fn invalidations(&self) -> Vec<CredentialKind> {
            self.data.lock().unwrap().invalidations.clone()
        }

        fn saved_discovery(&self) -> Vec<OAuthDiscoveryState> {
            self.data.lock().unwrap().saved_discovery.clone()
        }

        fn saved_verifiers(&self) -> Vec<String> {
            self.data.lock().unwrap().saved_verifiers.clone()
        }

        fn saved_tokens(&self) -> Vec<OAuthTokens> {
            self.data.lock().unwrap().saved_tokens.clone()
        }
    }

    #[allow(clippy::double_must_use)]
    #[async_trait]
    impl OAuthClientProvider for TestProvider {
        fn redirect_url(&self) -> &str {
            &self.redirect_url
        }

        fn client_metadata(&self) -> &OAuthClientMetadata {
            &self.client_metadata
        }

        fn client_metadata_document(
            &self,
            _metadata: Option<&AuthorizationServerMetadata>,
        ) -> Result<Option<OAuthClientMetadataDocument>, OAuthFailure> {
            Ok(self.data.lock().unwrap().client_document.clone())
        }

        async fn state(&self) -> Result<Option<String>, OAuthFailure> {
            Ok(self.data.lock().unwrap().state.clone())
        }

        async fn client_information(
            &self,
        ) -> Result<Option<OAuthClientInformationMixed>, OAuthFailure> {
            Ok(self.data.lock().unwrap().client.clone())
        }

        async fn save_client_information(
            &self,
            information: &OAuthClientInformationMixed,
        ) -> Result<(), OAuthFailure> {
            self.update(|state| {
                state.client = Some(information.clone());
                state.saved_clients.push(information.clone());
            });
            Ok(())
        }

        fn has_save_client_information(&self) -> bool {
            self.data.lock().unwrap().can_save_client
        }

        async fn tokens(&self) -> Result<Option<OAuthTokens>, OAuthFailure> {
            let state = self.data.lock().unwrap();
            if state.fail_tokens_load {
                return Err(OAuthFailure::error("Failed to load tokens"));
            }
            Ok(state.tokens.clone())
        }

        async fn save_tokens(&self, tokens: &OAuthTokens) -> Result<(), OAuthFailure> {
            let fail = self.data.lock().unwrap().fail_save_tokens;
            if fail {
                return Err(OAuthFailure::error("Failed to persist tokens"));
            }
            self.update(|state| {
                state.tokens = Some(tokens.clone());
                state.saved_tokens.push(tokens.clone());
            });
            Ok(())
        }

        async fn redirect_to_authorization(&self, url: &Url) -> Result<(), OAuthFailure> {
            self.update(|state| state.authorization_url = Some(url.clone()));
            Ok(())
        }

        async fn save_code_verifier(&self, verifier: &str) -> Result<(), OAuthFailure> {
            self.update(|state| {
                state.verifier = Some(verifier.to_owned());
                state.saved_verifiers.push(verifier.to_owned());
            });
            Ok(())
        }

        async fn code_verifier(&self) -> Result<String, OAuthFailure> {
            self.data
                .lock()
                .unwrap()
                .verifier
                .clone()
                .ok_or_else(|| OAuthFailure::error("No OAuth PKCE code verifier is stored"))
        }

        async fn invalidate_credentials(&self, kind: CredentialKind) -> Result<(), OAuthFailure> {
            self.update(|state| {
                state.invalidations.push(kind);
                if matches!(kind, CredentialKind::All | CredentialKind::Client) {
                    state.client = None;
                }
                if matches!(kind, CredentialKind::All | CredentialKind::Tokens) {
                    state.tokens = None;
                }
                if matches!(kind, CredentialKind::All | CredentialKind::Verifier) {
                    state.verifier = None;
                }
                if matches!(kind, CredentialKind::All | CredentialKind::Discovery) {
                    state.discovery = None;
                }
            });
            Ok(())
        }

        async fn save_discovery_state(
            &self,
            state: &OAuthDiscoveryState,
        ) -> Result<(), OAuthFailure> {
            self.update(|own| {
                own.discovery = Some(state.clone());
                own.saved_discovery.push(state.clone());
            });
            Ok(())
        }

        async fn discovery_state(&self) -> Result<Option<OAuthDiscoveryState>, OAuthFailure> {
            Ok(self.data.lock().unwrap().discovery.clone())
        }
    }

    // -----------------------------------------------------------------------
    // 测试辅助
    // -----------------------------------------------------------------------

    /// 手工构造的元数据（不经 `parse_*` 校验），用于 `??`/`includes`/`.length` 的 JS 语义。
    fn raw_metadata(value: Value) -> AuthorizationServerMetadata {
        AuthorizationServerMetadata::from_fields(match value {
            Value::Object(map) => map,
            _ => unreachable!(),
        })
    }

    fn metadata(origin: &str) -> AuthorizationServerMetadata {
        parse_authorization_server_metadata(&json!({
            "issuer": origin,
            "authorization_endpoint": format!("{origin}/authorize"),
            "token_endpoint": format!("{origin}/token"),
            "registration_endpoint": format!("{origin}/register"),
            "response_types_supported": ["code"],
            "token_endpoint_auth_methods_supported": ["none"],
            "code_challenge_methods_supported": ["S256"],
        }))
        .expect("valid metadata")
    }

    fn cached_discovery(
        origin: &str,
        metadata: Option<AuthorizationServerMetadata>,
    ) -> OAuthDiscoveryState {
        OAuthDiscoveryState {
            authorization_server_url: origin.to_owned(),
            authorization_server_metadata: metadata,
            resource_metadata: None,
            resource_metadata_url: None,
        }
    }

    fn tokens(value: Value) -> OAuthTokens {
        parse_oauth_tokens(&value).expect("valid tokens")
    }

    fn header_map(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                HeaderName::from_static(name),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        headers
    }

    fn unauthorized_context(
        server_url: &str,
        fetch: Arc<dyn McpFetch>,
        token: Option<&str>,
        www_authenticate: &str,
    ) -> UnauthorizedContext {
        let headers = if www_authenticate.is_empty() {
            HeaderMap::new()
        } else {
            header_map(&[("www-authenticate", www_authenticate)])
        };
        UnauthorizedContext {
            status: 401,
            headers,
            body: Vec::new(),
            server_url: Url::parse(server_url).unwrap(),
            fetch,
            token: token.map(str::to_owned),
        }
    }

    // -----------------------------------------------------------------------
    // 纯函数
    // -----------------------------------------------------------------------

    #[test]
    fn loopback_matches_the_typescript_hostnames() {
        assert!(loopback("localhost"));
        assert!(loopback("127.0.0.1"));
        assert!(loopback("[::1]"));
        assert!(loopback("::1"));
        assert!(!loopback("localhost."));
        assert!(!loopback("0.0.0.0"));
        assert!(!loopback("example.com"));
        // WHATWG 的 `URL.hostname` 对 IPv6 带方括号，`url::Url::host_str()` 必须一致。
        assert_eq!(
            js_url::parse("http://[::1]/x").unwrap().host_str(),
            Some("[::1]")
        );
    }

    #[test]
    fn application_type_derives_native_from_custom_schemes_and_loopback() {
        // 与 TS 测试 #10493 的向量一致（`http://localhost` 显式设置时由调用方覆盖）。
        assert_eq!(
            application_type(&["http://127.0.0.1:1234/callback"]),
            "native"
        );
        assert_eq!(application_type(&["http://[::1]/callback"]), "native");
        assert_eq!(application_type(&["com.example.app:/callback"]), "native");
        assert_eq!(application_type(&["https://app.example/callback"]), "web");
        assert_eq!(application_type(&["http://localhost/callback"]), "native");
        // 无法解析的 URI 被跳过。
        assert_eq!(application_type(&["not a url"]), "web");
        assert_eq!(
            application_type(&["not a url", "com.example.app:/x"]),
            "native"
        );
        assert_eq!(application_type(&[]), "web");
    }

    #[test]
    fn secure_endpoint_allows_https_and_loopback_http() {
        assert_eq!(
            secure_endpoint("https://as.example/token")
                .unwrap()
                .as_str(),
            "https://as.example/token"
        );
        assert_eq!(
            secure_endpoint("http://127.0.0.1/token").unwrap().as_str(),
            "http://127.0.0.1/token"
        );
        assert_eq!(
            secure_endpoint("http://localhost:8080/t").unwrap().as_str(),
            "http://localhost:8080/t"
        );
        assert_eq!(
            secure_endpoint("http://[::1]/t").unwrap().as_str(),
            "http://[::1]/t"
        );
        let error = secure_endpoint("http://as.example/token").unwrap_err();
        assert_eq!(error.name(), "OAuthInsecureEndpointError");
        assert_eq!(
            error.message(),
            "Refusing to send OAuth credentials to non-HTTPS endpoint http://as.example/token"
        );
        // `new URL` 失败是 TypeError。
        assert_eq!(
            secure_endpoint("not a url").unwrap_err().name(),
            "TypeError"
        );
    }

    #[test]
    fn select_client_auth_method_prefers_the_hint_then_the_supported_list() {
        let with_secret = OAuthClientInformation::from_fields(
            match json!({"client_id": "c", "client_secret": "s"}) {
                Value::Object(map) => map,
                _ => unreachable!(),
            },
        );
        let without_secret = OAuthClientInformation::from_fields(match json!({"client_id": "c"}) {
            Value::Object(map) => map,
            _ => unreachable!(),
        });
        let hinted = |method: &str| {
            OAuthClientInformation::from_fields(
                match json!({
                    "client_id": "c",
                    "client_secret": "s",
                    "token_endpoint_auth_method": method,
                }) {
                    Value::Object(map) => map,
                    _ => unreachable!(),
                },
            )
        };

        assert_eq!(
            select_client_auth_method(&with_secret, &[]),
            ClientAuthMethod::ClientSecretBasic
        );
        assert_eq!(
            select_client_auth_method(&without_secret, &[]),
            ClientAuthMethod::None
        );
        assert_eq!(
            select_client_auth_method(&hinted("client_secret_post"), &[]),
            ClientAuthMethod::ClientSecretPost
        );
        // 提示值不在 `supported` 里 → 忽略提示，按常规规则（有 secret → basic）。
        assert_eq!(
            select_client_auth_method(&hinted("none"), &["client_secret_basic"]),
            ClientAuthMethod::ClientSecretBasic
        );
        // 不认识的提示值被忽略，走常规规则。
        assert_eq!(
            select_client_auth_method(&hinted("private_key_jwt"), &[]),
            ClientAuthMethod::ClientSecretBasic
        );
        assert_eq!(
            select_client_auth_method(&with_secret, &["client_secret_post"]),
            ClientAuthMethod::ClientSecretPost
        );
        assert_eq!(
            select_client_auth_method(&with_secret, &["private_key_jwt"]),
            ClientAuthMethod::ClientSecretPost,
            "兜底：有 secret 用 client_secret_post"
        );
        assert_eq!(
            select_client_auth_method(&without_secret, &["client_secret_basic"]),
            ClientAuthMethod::None,
            "兜底：没有 secret 用 none"
        );
        assert_eq!(
            select_client_auth_method(&with_secret, &["none"]),
            ClientAuthMethod::None
        );
    }

    #[test]
    fn apply_client_authentication_writes_basic_headers_and_form_fields() {
        let information = OAuthClientInformation::from_fields(
            match json!({"client_id": "cid", "client_secret": "s/e:c"}) {
                Value::Object(map) => map,
                _ => unreachable!(),
            },
        );
        let mut headers = JsHeaders::new();
        let mut params = Vec::new();
        apply_client_authentication(
            ClientAuthMethod::ClientSecretBasic,
            &information,
            &mut headers,
            &mut params,
        )
        .unwrap();
        // `Buffer.from("cid:s/e:c").toString("base64")`
        assert_eq!(headers.get("authorization"), Some("Basic Y2lkOnMvZTpj"));
        assert!(params.is_empty());

        let missing = OAuthClientInformation::from_fields(match json!({"client_id": "cid"}) {
            Value::Object(map) => map,
            _ => unreachable!(),
        });
        let error = apply_client_authentication(
            ClientAuthMethod::ClientSecretBasic,
            &missing,
            &mut JsHeaders::new(),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert_eq!(
            error.message(),
            "client_secret_basic requires a client secret"
        );

        let empty_secret = OAuthClientInformation::from_fields(
            match json!({"client_id": "cid", "client_secret": ""}) {
                Value::Object(map) => map,
                _ => unreachable!(),
            },
        );
        assert!(
            apply_client_authentication(
                ClientAuthMethod::ClientSecretBasic,
                &empty_secret,
                &mut JsHeaders::new(),
                &mut Vec::new(),
            )
            .is_err()
        );

        let mut params = Vec::new();
        apply_client_authentication(
            ClientAuthMethod::ClientSecretPost,
            &information,
            &mut JsHeaders::new(),
            &mut params,
        )
        .unwrap();
        assert_eq!(
            params,
            vec![
                ("client_id".to_owned(), "cid".to_owned()),
                ("client_secret".to_owned(), "s/e:c".to_owned()),
            ]
        );

        let mut params = Vec::new();
        apply_client_authentication(
            ClientAuthMethod::ClientSecretPost,
            &empty_secret,
            &mut JsHeaders::new(),
            &mut params,
        )
        .unwrap();
        assert_eq!(params, vec![("client_id".to_owned(), "cid".to_owned())]);

        let mut params = Vec::new();
        apply_client_authentication(
            ClientAuthMethod::None,
            &information,
            &mut JsHeaders::new(),
            &mut params,
        )
        .unwrap();
        assert_eq!(params, vec![("client_id".to_owned(), "cid".to_owned())]);
    }

    #[test]
    fn pkce_derives_the_challenge_from_the_verifier() {
        let first = pkce().unwrap();
        assert_eq!(first.verifier.len(), 43);
        assert!(
            first
                .verifier
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        );
        let digest = Sha256::digest(first.verifier.as_bytes());
        assert_eq!(first.challenge, BASE64URL.encode(digest));
        // 每次调用都不同。
        assert_ne!(pkce().unwrap().verifier, first.verifier);
    }

    #[test]
    fn search_params_set_replaces_in_place_and_reencodes_the_query() {
        let mut url = Url::parse("https://x.example/a?a=%20&response_type=token&b=x&b=y").unwrap();
        search_params_set(&mut url, "response_type", "code");
        search_params_set(&mut url, "a", "1 2");
        search_params_set(&mut url, "c", "3");
        assert_eq!(url.query(), Some("a=1+2&response_type=code&b=x&b=y&c=3"));
    }

    #[test]
    fn step_up_scope_matches_the_typescript_vectors() {
        assert_eq!(
            step_up_scope(Some("repo read:org"), Some("repo admin")).as_deref(),
            Some("repo read:org admin")
        );
        assert_eq!(step_up_scope(None, Some("a b")).as_deref(), Some("a b"));
        assert_eq!(step_up_scope(Some("a"), Some("")), None);
        assert_eq!(step_up_scope(None, None), None);
        assert_eq!(step_up_scope(Some(""), Some("b")).as_deref(), Some("b"));
        assert_eq!(
            step_up_scope(Some("a  b"), Some(" b c ")).as_deref(),
            Some("a b c")
        );
        assert_eq!(step_up_scope(Some("  "), Some("x")).as_deref(), Some("x"));
        // JS 的 `\s`：`U+0085` 不是空白（留在 token 里），`U+00A0` 是。
        assert_eq!(
            step_up_scope(Some("a\u{85}b"), Some("c")).as_deref(),
            Some("a\u{85}b c")
        );
        assert_eq!(
            step_up_scope(Some("a\u{a0}b"), Some("c\u{200b}c")).as_deref(),
            Some("a b c\u{200b}c")
        );
    }

    #[test]
    fn with_scope_appends_only_a_missing_truthy_scope() {
        let base = tokens(json!({"access_token": "a", "token_type": "Bearer"}));
        let scoped = with_scope(base.clone(), Some("s"));
        assert_eq!(scoped.scope(), Some("s"));
        assert_eq!(
            scoped.fields().keys().cloned().collect::<Vec<_>>(),
            vec!["access_token", "token_type", "scope"],
            "`{{...tokens, scope}}` 追加在末尾"
        );
        assert_eq!(with_scope(base.clone(), None), base);
        assert_eq!(with_scope(base.clone(), Some("")), base);
        let already = tokens(json!({"access_token": "a", "token_type": "B", "scope": "old"}));
        assert_eq!(with_scope(already.clone(), Some("new")), already);
        // `tokens.scope === undefined` 只认缺席：`null` 与非字符串的 scope 也算已有值。
        let raw_scope = |scope: Value| {
            OAuthTokens::from_fields(
                match json!({
                    "access_token": "a",
                    "token_type": "B",
                    "scope": scope,
                }) {
                    Value::Object(map) => map,
                    _ => unreachable!(),
                },
            )
        };
        let null_scope = raw_scope(Value::Null);
        assert_eq!(with_scope(null_scope.clone(), Some("s")), null_scope);
        let number_scope = raw_scope(json!(5));
        assert_eq!(with_scope(number_scope.clone(), Some("s")), number_scope);
    }

    // -----------------------------------------------------------------------
    // startAuthorization
    // -----------------------------------------------------------------------

    #[test]
    fn start_authorization_orders_parameters_like_url_search_params() {
        let server_metadata = parse_authorization_server_metadata(&json!({
            "issuer": "https://as.example",
            "authorization_endpoint": "https://as.example/authorize?foo=1&response_type=token&scope=old",
            "token_endpoint": "https://as.example/token",
            "response_types_supported": ["code"],
            "code_challenge_methods_supported": ["S256"],
        }))
        .unwrap();
        let result = tokio::runtime::Runtime::new().unwrap().block_on(async {
            start_authorization(
                "https://as.example/tenant",
                StartAuthorizationOptions {
                    metadata: Some(server_metadata),
                    client_information: OAuthClientInformation::from_fields(
                        match json!({"client_id": "client id/with+chars"}) {
                            Value::Object(map) => map,
                            _ => unreachable!(),
                        },
                    ),
                    redirect_url: "http://127.0.0.1:1234/callback?a=1".to_owned(),
                    scope: Some("repo offline_access".to_owned()),
                    state: Some("st ate".to_owned()),
                    resource: Some("https://mcp.example/x?y=1".to_owned()),
                },
            )
            .await
            .unwrap()
        });
        let url = &result.authorization_url;
        assert_eq!(url.origin().ascii_serialization(), "https://as.example");
        assert_eq!(url.path(), "/authorize");
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        // 与 TS 实测一致：已存在的 `response_type`/`scope` 原位替换。
        assert_eq!(
            pairs
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>(),
            vec![
                "foo",
                "response_type",
                "scope",
                "client_id",
                "code_challenge",
                "code_challenge_method",
                "redirect_uri",
                "state",
                "prompt",
                "resource",
            ]
        );
        let get = |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .unwrap()
        };
        assert_eq!(get("foo"), "1");
        assert_eq!(get("response_type"), "code");
        assert_eq!(get("scope"), "repo offline_access");
        assert_eq!(get("client_id"), "client id/with+chars");
        assert_eq!(get("code_challenge_method"), "S256");
        assert_eq!(get("redirect_uri"), "http://127.0.0.1:1234/callback?a=1");
        assert_eq!(get("state"), "st ate");
        assert_eq!(get("prompt"), "consent");
        assert_eq!(get("resource"), "https://mcp.example/x?y=1");
        assert_eq!(result.code_verifier.len(), 43);
        assert_eq!(
            get("code_challenge"),
            BASE64URL.encode(Sha256::digest(result.code_verifier.as_bytes()))
        );
    }

    #[test]
    fn start_authorization_defaults_to_the_authorize_path() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime
            .block_on(start_authorization(
                "https://as.example/tenant/",
                StartAuthorizationOptions {
                    client_information: OAuthClientInformation::from_fields(
                        match json!({"client_id": "c"}) {
                            Value::Object(map) => map,
                            _ => unreachable!(),
                        },
                    ),
                    redirect_url: "http://127.0.0.1/cb".to_owned(),
                    ..StartAuthorizationOptions::default()
                },
            ))
            .unwrap();
        assert_eq!(result.authorization_url.path(), "/authorize");
        let pairs: Vec<(String, String)> = result
            .authorization_url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        assert_eq!(
            pairs
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>(),
            vec![
                "response_type",
                "client_id",
                "code_challenge",
                "code_challenge_method",
                "redirect_uri",
            ]
        );
        assert!(pairs.iter().all(|(key, _)| key != "state"));
        assert!(pairs.iter().all(|(key, _)| key != "prompt"));
    }

    #[test]
    fn start_authorization_rejects_unsupported_servers_and_non_http_endpoints() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let client = || {
            OAuthClientInformation::from_fields(match json!({"client_id": "c"}) {
                Value::Object(map) => map,
                _ => unreachable!(),
            })
        };
        let run = |metadata: Option<AuthorizationServerMetadata>| {
            runtime.block_on(start_authorization(
                "https://a.example",
                StartAuthorizationOptions {
                    metadata,
                    client_information: client(),
                    redirect_url: "http://127.0.0.1/cb".to_owned(),
                    ..StartAuthorizationOptions::default()
                },
            ))
        };
        let error = run(Some(
            parse_authorization_server_metadata(&json!({
                "issuer": "https://a.example",
                "authorization_endpoint": "https://a.example/auth",
                "token_endpoint": "https://a.example/t",
                "response_types_supported": ["token"],
            }))
            .unwrap(),
        ))
        .unwrap_err();
        assert_eq!(
            error.message(),
            "Authorization server does not support authorization codes"
        );

        let error = run(Some(
            parse_authorization_server_metadata(&json!({
                "issuer": "https://a.example",
                "authorization_endpoint": "https://a.example/auth",
                "token_endpoint": "https://a.example/t",
                "response_types_supported": ["code"],
                "code_challenge_methods_supported": ["plain"],
            }))
            .unwrap(),
        ))
        .unwrap_err();
        assert_eq!(
            error.message(),
            "Authorization server does not support PKCE S256"
        );

        // 手工构造、缺 `response_types_supported` 的元数据在 TS 里是 TypeError。
        let error = run(Some(AuthorizationServerMetadata::from_fields(
            match json!({
                "issuer": "https://a.example",
                "authorization_endpoint": "https://a.example/auth",
                "token_endpoint": "https://a.example/t",
            }) {
                Value::Object(map) => map,
                _ => unreachable!(),
            },
        )))
        .unwrap_err();
        assert_eq!(error.name(), "TypeError");
        assert_eq!(
            error.message(),
            "Cannot read properties of undefined (reading 'includes')"
        );

        for endpoint in [
            "file://attacker.example/share/x.exe",
            "search-ms:query=x",
            "ms-msdt:/id",
        ] {
            let error = run(Some(AuthorizationServerMetadata::from_fields(
                match json!({
                    "issuer": "https://a.example",
                    "authorization_endpoint": endpoint,
                    "token_endpoint": "https://a.example/t",
                    "response_types_supported": ["code"],
                }) {
                    Value::Object(map) => map,
                    _ => unreachable!(),
                },
            )))
            .unwrap_err();
            assert_eq!(
                error.message(),
                format!("Invalid authorization endpoint {endpoint}")
            );
        }
    }

    /// 回归测试（评审 finding #2，minor）：TS 只对 nullish 走 `??`，其余原始值交给
    /// `String.prototype.includes` / `Array.prototype.includes` / `new URL(String(v))`。
    #[tokio::test]
    async fn start_authorization_follows_js_coercion_for_malformed_metadata() {
        let run = |metadata: AuthorizationServerMetadata| {
            start_authorization(
                "https://a.example",
                StartAuthorizationOptions {
                    metadata: Some(metadata),
                    client_information: client_information(json!({"client_id": 5})),
                    redirect_url: "http://127.0.0.1/cb".to_owned(),
                    ..StartAuthorizationOptions::default()
                },
            )
        };
        let error = run(raw_metadata(json!({
            "issuer": "https://a.example",
            "authorization_endpoint": "https://a.example/auth",
            "token_endpoint": "https://a.example/t",
            "response_types_supported": 5,
        })))
        .await
        .unwrap_err();
        assert_eq!(error.name(), "TypeError");
        assert_eq!(
            error.message(),
            "metadata.response_types_supported.includes is not a function"
        );

        // 字符串也有 `includes`（子串语义）：`"codes"` 含 `"code"`，`"S256x"` 含 `"S256"`。
        // 非字符串真值的 `authorization_endpoint` 经 `String([...])` 变成合法 URL；
        // `client_id: 5` 经 `searchParams.set` 写成 `"5"`。
        let result = run(raw_metadata(json!({
            "issuer": "https://a.example",
            "authorization_endpoint": ["https://a.example/array-auth"],
            "token_endpoint": "https://a.example/t",
            "response_types_supported": "codes",
            "code_challenge_methods_supported": "S256x",
        })))
        .await
        .unwrap();
        assert_eq!(result.authorization_url.path(), "/array-auth");
        assert_eq!(
            query_value(&result.authorization_url, "client_id").as_deref(),
            Some("5")
        );

        let error = run(raw_metadata(json!({
            "issuer": "https://a.example",
            "authorization_endpoint": "https://a.example/auth",
            "token_endpoint": "https://a.example/t",
            "response_types_supported": "token",
        })))
        .await
        .unwrap_err();
        assert_eq!(
            error.message(),
            "Authorization server does not support authorization codes"
        );

        let error = run(raw_metadata(json!({
            "issuer": "https://a.example",
            "authorization_endpoint": "https://a.example/auth",
            "token_endpoint": "https://a.example/t",
            "response_types_supported": ["code"],
            "code_challenge_methods_supported": "plain",
        })))
        .await
        .unwrap_err();
        assert_eq!(
            error.message(),
            "Authorization server does not support PKCE S256"
        );

        let error = run(raw_metadata(json!({
            "issuer": "https://a.example",
            "authorization_endpoint": 5,
            "token_endpoint": "https://a.example/t",
            "response_types_supported": ["code"],
        })))
        .await
        .unwrap_err();
        assert_eq!(error.name(), "TypeError");
        assert_eq!(error.message(), "Invalid URL");
    }

    // -----------------------------------------------------------------------
    // 令牌请求与刷新
    // -----------------------------------------------------------------------

    fn client_information(value: Value) -> OAuthClientInformation {
        OAuthClientInformation::from_fields(match value {
            Value::Object(map) => map,
            _ => unreachable!(),
        })
    }

    fn ok_tokens() -> (u16, String) {
        (
            200,
            json!({"access_token": "a", "token_type": "Bearer"}).to_string(),
        )
    }

    #[tokio::test]
    async fn token_request_sends_form_encoded_bodies_with_client_authentication() {
        let fetch = MockFetch::new(|_| ok_tokens());
        let server_metadata = AuthorizationServerMetadata::from_fields(
            match json!({
                "issuer": "https://as.example",
                "authorization_endpoint": "https://as.example/a",
                "token_endpoint": "https://as.example/token?x=1",
                "response_types_supported": ["code"],
                "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
            }) {
                Value::Object(map) => map,
                _ => unreachable!(),
            },
        );
        exchange_authorization_code(
            "https://as.example",
            ExchangeAuthorizationCodeOptions {
                token: TokenRequestOptions {
                    metadata: Some(server_metadata.clone()),
                    client_information: client_information(
                        json!({"client_id": "cid", "client_secret": "s/e:c"}),
                    ),
                    resource: Some("https://mcp.example/m?q=1".to_owned()),
                    fetch: Some(fetch.shared()),
                    ..TokenRequestOptions::default()
                },
                code: "the code".to_owned(),
                code_verifier: "ver!fier".to_owned(),
                redirect_url: "http://127.0.0.1/cb?x=1".to_owned(),
            },
        )
        .await
        .unwrap();
        let request = &fetch.requests()[0];
        assert_eq!(request.url, "https://as.example/token?x=1");
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.header("accept").as_deref(),
            Some("application/json")
        );
        assert_eq!(
            request.header("content-type").as_deref(),
            Some("application/x-www-form-urlencoded")
        );
        assert_eq!(
            request.header("authorization").as_deref(),
            Some("Basic Y2lkOnMvZTpj")
        );
        assert_eq!(
            request.body,
            "grant_type=authorization_code&code=the+code&code_verifier=ver%21fier&redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb%3Fx%3D1&resource=https%3A%2F%2Fmcp.example%2Fm%3Fq%3D1"
        );

        // `none`：只追加 client_id。
        let mut no_auth = server_metadata.clone();
        no_auth.set("token_endpoint_auth_methods_supported", json!(["none"]));
        exchange_authorization_code(
            "https://as.example",
            ExchangeAuthorizationCodeOptions {
                token: TokenRequestOptions {
                    metadata: Some(no_auth),
                    client_information: client_information(
                        json!({"client_id": "cid", "client_secret": "sec"}),
                    ),
                    fetch: Some(fetch.shared()),
                    ..TokenRequestOptions::default()
                },
                code: "c".to_owned(),
                code_verifier: "v".to_owned(),
                redirect_url: "http://127.0.0.1/cb".to_owned(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            fetch.requests()[1].body,
            "grant_type=authorization_code&code=c&code_verifier=v&redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb&client_id=cid"
        );

        // `client_secret_post`。
        let mut post = server_metadata.clone();
        post.set(
            "token_endpoint_auth_methods_supported",
            json!(["client_secret_post"]),
        );
        exchange_authorization_code(
            "https://as.example",
            ExchangeAuthorizationCodeOptions {
                token: TokenRequestOptions {
                    metadata: Some(post),
                    client_information: client_information(
                        json!({"client_id": "cid", "client_secret": "sec"}),
                    ),
                    fetch: Some(fetch.shared()),
                    ..TokenRequestOptions::default()
                },
                code: "c".to_owned(),
                code_verifier: "v".to_owned(),
                redirect_url: "r".to_owned(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            fetch.requests()[2].body,
            "grant_type=authorization_code&code=c&code_verifier=v&redirect_uri=r&client_id=cid&client_secret=sec"
        );

        // 元数据缺少 `token_endpoint` → `new URL("/token", authorizationServerUrl)`。
        exchange_authorization_code(
            "https://as.example/base/",
            ExchangeAuthorizationCodeOptions {
                token: TokenRequestOptions {
                    metadata: Some(AuthorizationServerMetadata::from_fields(
                        match json!({
                            "issuer": "https://as.example",
                            "authorization_endpoint": "https://as.example/a",
                            "response_types_supported": ["code"],
                        }) {
                            Value::Object(map) => map,
                            _ => unreachable!(),
                        },
                    )),
                    client_information: client_information(json!({"client_id": "c"})),
                    fetch: Some(fetch.shared()),
                    ..TokenRequestOptions::default()
                },
                code: "c".to_owned(),
                code_verifier: "v".to_owned(),
                redirect_url: "r".to_owned(),
            },
        )
        .await
        .unwrap();
        assert_eq!(fetch.requests()[3].url, "https://as.example/token");
    }

    #[tokio::test]
    async fn token_request_prefers_the_body_error_over_the_status() {
        let error_fetch = MockFetch::new(|_| {
            (
                400,
                json!({
                    "error": "invalid_grant",
                    "error_description": "bad code",
                    "error_uri": "https://e.example/x",
                })
                .to_string(),
            )
        });
        let error = refresh_authorization(
            "https://as.example",
            RefreshAuthorizationOptions {
                token: TokenRequestOptions {
                    client_information: client_information(json!({"client_id": "c"})),
                    fetch: Some(error_fetch.shared()),
                    ..TokenRequestOptions::default()
                },
                refresh_token: "r".to_owned(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.name(), "OAuthError");
        let oauth = error.as_oauth().unwrap();
        assert_eq!(oauth.code, "invalid_grant");
        assert_eq!(oauth.message, "bad code");
        assert_eq!(oauth.error_uri.as_deref(), Some("https://e.example/x"));

        // 空 `error_description` → `super(message || code)` 回退到 code。
        let empty_description = MockFetch::new(|_| {
            (
                400,
                json!({"error": "invalid_scope", "error_description": ""}).to_string(),
            )
        });
        let error = refresh_authorization(
            "https://as.example",
            RefreshAuthorizationOptions {
                token: TokenRequestOptions {
                    client_information: client_information(json!({"client_id": "c"})),
                    fetch: Some(empty_description.shared()),
                    ..TokenRequestOptions::default()
                },
                refresh_token: "r".to_owned(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.as_oauth().unwrap().code, "invalid_scope");
        assert_eq!(error.message(), "invalid_scope");

        // 非 JSON 的非 ok → server_error 带状态与 body。
        let plain = MockFetch::new(|_| (500, "boom".to_owned()));
        let error = refresh_authorization(
            "https://as.example",
            RefreshAuthorizationOptions {
                token: TokenRequestOptions {
                    client_information: client_information(json!({"client_id": "c"})),
                    fetch: Some(plain.shared()),
                    ..TokenRequestOptions::default()
                },
                refresh_token: "r".to_owned(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.as_oauth().unwrap().code, "server_error");
        assert_eq!(error.message(), "HTTP 500: boom");

        // ok 但 body 不是合法 JSON → `parseOAuthTokens(undefined)`。
        let invalid = MockFetch::new(|_| (200, "not json".to_owned()));
        let error = refresh_authorization(
            "https://as.example",
            RefreshAuthorizationOptions {
                token: TokenRequestOptions {
                    client_information: client_information(json!({"client_id": "c"})),
                    fetch: Some(invalid.shared()),
                    ..TokenRequestOptions::default()
                },
                refresh_token: "r".to_owned(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.name(), "Error");
        assert_eq!(error.message(), "Invalid OAuth token response");

        // ok 但缺少 `token_type`。
        let incomplete = MockFetch::new(|_| (200, json!({"access_token": "a"}).to_string()));
        let error = refresh_authorization(
            "https://as.example",
            RefreshAuthorizationOptions {
                token: TokenRequestOptions {
                    client_information: client_information(json!({"client_id": "c"})),
                    fetch: Some(incomplete.shared()),
                    ..TokenRequestOptions::default()
                },
                refresh_token: "r".to_owned(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.message(), "Invalid token_type");
    }

    #[tokio::test]
    async fn token_request_rejects_insecure_endpoints_and_missing_basic_secrets() {
        let fetch = MockFetch::new(|_| ok_tokens());
        let error = refresh_authorization(
            "http://as.example",
            RefreshAuthorizationOptions {
                token: TokenRequestOptions {
                    client_information: client_information(json!({"client_id": "c"})),
                    fetch: Some(fetch.shared()),
                    ..TokenRequestOptions::default()
                },
                refresh_token: "r".to_owned(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.name(), "OAuthInsecureEndpointError");
        assert_eq!(
            error.message(),
            "Refusing to send OAuth credentials to non-HTTPS endpoint http://as.example/token"
        );
        assert!(fetch.requests().is_empty(), "不安全的端点不会被请求");

        // 提示值是 `client_secret_basic` 但没有 secret。
        let error = refresh_authorization(
            "https://as.example",
            RefreshAuthorizationOptions {
                token: TokenRequestOptions {
                    client_information: client_information(json!({
                        "client_id": "c",
                        "token_endpoint_auth_method": "client_secret_basic",
                    })),
                    fetch: Some(fetch.shared()),
                    ..TokenRequestOptions::default()
                },
                refresh_token: "r".to_owned(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.message(),
            "client_secret_basic requires a client secret"
        );
    }

    /// 回归测试（评审 finding #2，minor）：令牌端点与
    /// `token_endpoint_auth_methods_supported` 的原始值语义（`new URL(String(v))`、
    /// `String.prototype.includes`、V8 的 `includes is not a function`）。
    #[tokio::test]
    async fn token_request_follows_js_coercion_for_malformed_metadata() {
        let fetch = MockFetch::new(|_| ok_tokens());
        let token = |metadata: Value, information: Value| TokenRequestOptions {
            metadata: Some(raw_metadata(metadata)),
            client_information: client_information(information),
            fetch: Some(fetch.shared()),
            ..TokenRequestOptions::default()
        };

        // `token_endpoint: 5` → `new URL(5)` 是 TypeError，不再回退到 `/token`。
        let error = refresh_authorization(
            "https://as.example",
            RefreshAuthorizationOptions {
                token: token(json!({"token_endpoint": 5}), json!({"client_id": "c"})),
                refresh_token: "r".to_owned(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.name(), "TypeError");
        assert_eq!(error.message(), "Invalid URL");

        // `""` 不是 nullish，同样走 `new URL("")`。
        let error = refresh_authorization(
            "https://as.example",
            RefreshAuthorizationOptions {
                token: token(json!({"token_endpoint": ""}), json!({"client_id": "c"})),
                refresh_token: "r".to_owned(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.message(), "Invalid URL");
        assert!(fetch.requests().is_empty(), "端点解析失败前不发请求");

        // 字符串有 `length`/`includes`：非空串按子串匹配，等于声明了该方法。
        refresh_authorization(
            "https://as.example",
            RefreshAuthorizationOptions {
                token: token(
                    json!({
                        "token_endpoint": "https://as.example/token",
                        "token_endpoint_auth_methods_supported": "client_secret_basic",
                    }),
                    json!({"client_id": "cid", "client_secret": "sec"}),
                ),
                refresh_token: "r".to_owned(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            fetch.requests()[0].header("authorization").as_deref(),
            Some("Basic Y2lkOnNlYw==")
        );

        // 非字符串/数组的值没有 `.includes`：V8 的 `supported.includes is not a function`。
        let error = refresh_authorization(
            "https://as.example",
            RefreshAuthorizationOptions {
                token: token(
                    json!({
                        "token_endpoint": "https://as.example/token",
                        "token_endpoint_auth_methods_supported": 5,
                    }),
                    json!({"client_id": "cid", "client_secret": "sec"}),
                ),
                refresh_token: "r".to_owned(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.name(), "TypeError");
        assert_eq!(error.message(), "supported.includes is not a function");
    }

    /// 回归测试（评审 finding #5，minor）：`client_id`/`client_secret` 走 `String()` 与
    /// JS 真值判定，而不是“非字符串即缺席”。
    #[test]
    fn apply_client_authentication_coerces_non_string_credentials_like_js() {
        let information = client_information(json!({"client_id": 5, "client_secret": 7}));
        let mut headers = JsHeaders::new();
        let mut params = Vec::new();
        apply_client_authentication(
            ClientAuthMethod::ClientSecretBasic,
            &information,
            &mut headers,
            &mut params,
        )
        .unwrap();
        assert_eq!(
            headers.get("authorization"),
            Some("Basic NTo3"),
            "base64(String(5):String(7))"
        );
        assert!(params.is_empty());

        let mut headers = JsHeaders::new();
        let mut params = Vec::new();
        apply_client_authentication(
            ClientAuthMethod::ClientSecretPost,
            &information,
            &mut headers,
            &mut params,
        )
        .unwrap();
        assert_eq!(
            params,
            vec![
                ("client_id".to_owned(), "5".to_owned()),
                ("client_secret".to_owned(), "7".to_owned()),
            ]
        );

        // `client_secret: 0` 是假值：basic 报缺 secret，post 不带 secret。
        let zero_secret = client_information(json!({"client_id": "c", "client_secret": 0}));
        let error = apply_client_authentication(
            ClientAuthMethod::ClientSecretBasic,
            &zero_secret,
            &mut JsHeaders::new(),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert_eq!(
            error.message(),
            "client_secret_basic requires a client secret"
        );
        let mut params = Vec::new();
        apply_client_authentication(
            ClientAuthMethod::ClientSecretPost,
            &zero_secret,
            &mut JsHeaders::new(),
            &mut params,
        )
        .unwrap();
        assert_eq!(params, vec![("client_id".to_owned(), "c".to_owned())]);
    }

    #[tokio::test]
    async fn refresh_authorization_lets_the_server_rotate_the_refresh_token() {
        let rotated = MockFetch::new(|_| {
            (
                200,
                json!({
                    "access_token": "a",
                    "token_type": "Bearer",
                    "refresh_token": "new",
                    "expires_in": 3600,
                })
                .to_string(),
            )
        });
        let result = refresh_authorization(
            "https://as.example",
            RefreshAuthorizationOptions {
                token: TokenRequestOptions {
                    client_information: client_information(json!({"client_id": "c"})),
                    fetch: Some(rotated.shared()),
                    ..TokenRequestOptions::default()
                },
                refresh_token: "old".to_owned(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            result.refresh_token(),
            Some("new"),
            "服务端的 rotate 结果胜出"
        );
        assert_eq!(
            result.fields().keys().cloned().collect::<Vec<_>>(),
            vec!["refresh_token", "access_token", "token_type", "expires_in"],
            "`{{refresh_token: old, ...tokens}}` 的键序"
        );

        let plain = MockFetch::new(|_| {
            (
                200,
                json!({"access_token": "a2", "token_type": "Bearer"}).to_string(),
            )
        });
        let result = refresh_authorization(
            "https://as.example",
            RefreshAuthorizationOptions {
                token: TokenRequestOptions {
                    client_information: client_information(json!({"client_id": "c"})),
                    fetch: Some(plain.shared()),
                    ..TokenRequestOptions::default()
                },
                refresh_token: "old".to_owned(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            result.refresh_token(),
            Some("old"),
            "服务端没返回时回退到传入值"
        );
        assert_eq!(
            result.fields().keys().cloned().collect::<Vec<_>>(),
            vec!["refresh_token", "access_token", "token_type"]
        );
    }

    // -----------------------------------------------------------------------
    // 动态客户端注册
    // -----------------------------------------------------------------------

    fn client_metadata(value: Value) -> OAuthClientMetadata {
        OAuthClientMetadata::from_fields(match value {
            Value::Object(map) => map,
            _ => unreachable!(),
        })
    }

    #[tokio::test]
    async fn register_client_builds_the_dynamic_registration_body() {
        let fetch = MockFetch::new(|_| (201, json!({"client_id": "id"}).to_string()));
        let registration_metadata = |endpoint: &str| {
            AuthorizationServerMetadata::from_fields(
                match json!({
                    "issuer": "https://as.example",
                    "authorization_endpoint": "https://as.example/a",
                    "token_endpoint": "https://as.example/t",
                    "response_types_supported": ["code"],
                    "registration_endpoint": endpoint,
                }) {
                    Value::Object(map) => map,
                    _ => unreachable!(),
                },
            )
        };
        let register = |client_metadata: OAuthClientMetadata, scope: Option<&str>| {
            register_client(
                "https://as.example",
                RegisterClientOptions {
                    metadata: Some(registration_metadata("https://as.example/register")),
                    client_metadata,
                    scope: scope.map(str::to_owned),
                    fetch: Some(fetch.shared()),
                    signal: None,
                },
            )
        };
        register(
            client_metadata(json!({
                "redirect_uris": ["http://127.0.0.1:1234/cb"],
                "client_name": "n",
                "scope": "x",
                "application_type": "web",
            })),
            Some("s1"),
        )
        .await
        .unwrap();
        register(
            client_metadata(json!({
                "redirect_uris": ["https://app.example/cb"],
                "client_name": "n",
            })),
            None,
        )
        .await
        .unwrap();
        register(
            client_metadata(json!({
                "redirect_uris": ["com.example.app:/cb"],
                "application_type": null,
            })),
            Some(""),
        )
        .await
        .unwrap();
        let requests = fetch.requests();
        assert_eq!(requests[0].url, "https://as.example/register");
        assert_eq!(
            requests[0].body,
            r#"{"redirect_uris":["http://127.0.0.1:1234/cb"],"client_name":"n","scope":"s1","application_type":"web"}"#
        );
        assert_eq!(
            requests[1].body,
            r#"{"redirect_uris":["https://app.example/cb"],"client_name":"n","application_type":"web"}"#
        );
        assert_eq!(
            requests[2].body,
            r#"{"redirect_uris":["com.example.app:/cb"],"application_type":"native"}"#
        );
        for request in &requests {
            assert_eq!(
                request.header("content-type").as_deref(),
                Some("application/json")
            );
            assert_eq!(
                request.header("accept").as_deref(),
                Some("application/json")
            );
            assert_eq!(request.method, "POST");
        }
    }

    #[tokio::test]
    async fn register_client_derives_application_type_from_the_redirect_uris() {
        let fetch = MockFetch::new(|_| (201, json!({"client_id": "c"}).to_string()));
        let mut bodies = Vec::new();
        for redirect_uris in [
            vec!["http://127.0.0.1:1234/callback"],
            vec!["http://[::1]/callback"],
            vec!["com.example.app:/callback"],
            vec!["https://app.example/callback"],
            vec!["http://localhost/callback"],
        ] {
            register_client(
                "https://as.example",
                RegisterClientOptions {
                    metadata: None,
                    client_metadata: client_metadata(json!({"redirect_uris": redirect_uris})),
                    scope: None,
                    fetch: Some(fetch.shared()),
                    signal: None,
                },
            )
            .await
            .unwrap();
            bodies.push(fetch.requests().last().unwrap().body_json());
        }
        let kinds: Vec<&str> = bodies
            .iter()
            .map(|body| body["application_type"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["native", "native", "native", "web", "native"]);
    }

    #[tokio::test]
    async fn register_client_reports_registration_failures() {
        let fetch = MockFetch::new(|_| (201, json!({"client_id": "c"}).to_string()));
        // 元数据存在但没有 registration_endpoint。
        let error = register_client(
            "https://as.example",
            RegisterClientOptions {
                metadata: Some(
                    parse_authorization_server_metadata(&json!({
                        "issuer": "https://as.example",
                        "authorization_endpoint": "https://as.example/a",
                        "token_endpoint": "https://as.example/t",
                        "response_types_supported": ["code"],
                    }))
                    .unwrap(),
                ),
                client_metadata: client_metadata(json!({"redirect_uris": []})),
                scope: None,
                fetch: Some(fetch.shared()),
                signal: None,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.message(),
            "Authorization server does not support dynamic client registration"
        );

        let failing = MockFetch::new(|_| (503, "nope".to_owned()));
        let error = register_client(
            "https://as.example",
            RegisterClientOptions {
                metadata: None,
                client_metadata: client_metadata(json!({"redirect_uris": []})),
                scope: None,
                fetch: Some(failing.shared()),
                signal: None,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.name(), "OAuthRegistrationError");
        let registration = error.as_registration().unwrap();
        assert_eq!(registration.status, 503);
        assert_eq!(registration.body, "nope");
        assert_eq!(
            error.message(),
            "OAuth dynamic client registration failed with status 503: nope"
        );

        // 没有元数据时回退到 `new URL("/register", authorizationServerUrl)`。
        let fallback = MockFetch::new(|_| (201, json!({"client_id": "c"}).to_string()));
        register_client(
            "https://as.example/base/",
            RegisterClientOptions {
                metadata: None,
                client_metadata: client_metadata(json!({"redirect_uris": []})),
                scope: None,
                fetch: Some(fallback.shared()),
                signal: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(fallback.requests()[0].url, "https://as.example/register");

        // 缺少 `redirect_uris` 且没有 `application_type`：TS 的 `undefined.some(...)` TypeError。
        let error = register_client(
            "https://as.example",
            RegisterClientOptions {
                metadata: None,
                client_metadata: OAuthClientMetadata::default(),
                scope: None,
                fetch: Some(fetch.shared()),
                signal: None,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.name(), "TypeError");
        assert_eq!(
            error.message(),
            "Cannot read properties of undefined (reading 'some')"
        );
    }

    /// 回归测试（评审 finding #1，minor）：DCR 请求体必须走 `JSON.stringify` 语义——
    /// 整数样式键前置、`1.0` 写成 `1`、`-0` 写成 `0`。
    #[tokio::test]
    async fn register_client_serializes_the_body_like_json_stringify() {
        let fetch = MockFetch::new(|_| (201, json!({"client_id": "c"}).to_string()));
        let mut fields = Map::new();
        fields.insert("a".to_owned(), json!(2));
        fields.insert("10".to_owned(), json!(1));
        fields.insert("v".to_owned(), json!(1.0));
        fields.insert("-0".to_owned(), json!(-0.0));
        fields.insert("redirect_uris".to_owned(), json!(["http://127.0.0.1/cb"]));
        register_client(
            "https://as.example",
            RegisterClientOptions {
                metadata: None,
                client_metadata: OAuthClientMetadata::from_fields(fields),
                scope: Some("s".to_owned()),
                fetch: Some(fetch.shared()),
                signal: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            fetch.requests()[0].body,
            r#"{"10":1,"a":2,"v":1,"-0":0,"redirect_uris":["http://127.0.0.1/cb"],"application_type":"native","scope":"s"}"#
        );
    }

    /// 回归测试（评审 finding #2，minor）：`registration_endpoint` 的真值与
    /// `new URL(String(v))` 语义。
    #[tokio::test]
    async fn register_client_follows_js_coercion_for_the_endpoint() {
        let fetch = MockFetch::new(|_| (201, json!({"client_id": "c"}).to_string()));
        let register = |metadata: AuthorizationServerMetadata| {
            register_client(
                "https://as.example",
                RegisterClientOptions {
                    metadata: Some(metadata),
                    client_metadata: client_metadata(json!({"redirect_uris": []})),
                    scope: None,
                    fetch: Some(fetch.shared()),
                    signal: None,
                },
            )
        };

        // `""` 是假值 → “不支持 DCR”（而不是 `new URL("")` 的 TypeError）。
        let error = register(raw_metadata(json!({"registration_endpoint": ""})))
            .await
            .unwrap_err();
        assert_eq!(
            error.message(),
            "Authorization server does not support dynamic client registration"
        );

        // 非字符串真值 → `new URL(String(5))` 的 TypeError。
        let error = register(raw_metadata(json!({"registration_endpoint": 5})))
            .await
            .unwrap_err();
        assert_eq!(error.name(), "TypeError");
        assert_eq!(error.message(), "Invalid URL");

        // `String(["https://…"])` 是合法 URL。
        register(raw_metadata(
            json!({"registration_endpoint": ["https://as.example/dcr"]}),
        ))
        .await
        .unwrap();
        assert_eq!(fetch.requests()[0].url, "https://as.example/dcr");
    }

    /// `redirect_uris` 的 `.some` 语义：非数组是 V8 的 TypeError
    /// （`undefined`/`null` 的读取消息不同）。
    #[tokio::test]
    async fn register_client_reports_non_array_redirect_uris_like_js() {
        let fetch = MockFetch::new(|_| (201, json!({"client_id": "c"}).to_string()));
        let register = |client_metadata: OAuthClientMetadata| {
            register_client(
                "https://as.example",
                RegisterClientOptions {
                    metadata: None,
                    client_metadata,
                    scope: None,
                    fetch: Some(fetch.shared()),
                    signal: None,
                },
            )
        };
        let error = register(OAuthClientMetadata::from_fields(
            match json!({"redirect_uris": null}) {
                Value::Object(map) => map,
                _ => unreachable!(),
            },
        ))
        .await
        .unwrap_err();
        assert_eq!(
            error.message(),
            "Cannot read properties of null (reading 'some')"
        );
        let error = register(OAuthClientMetadata::from_fields(
            match json!({"redirect_uris": "https://app.example/cb"}) {
                Value::Object(map) => map,
                _ => unreachable!(),
            },
        ))
        .await
        .unwrap_err();
        assert_eq!(error.message(), "redirectUris.some is not a function");
    }

    // -----------------------------------------------------------------------
    // runFlow
    // -----------------------------------------------------------------------

    use crate::oauth::types::parse_protected_resource_metadata;

    fn resource_metadata(resource: &str, scopes: Option<Value>) -> OAuthProtectedResourceMetadata {
        let mut value = json!({"resource": resource});
        if let (Value::Object(map), Some(scopes)) = (&mut value, scopes) {
            map.insert("scopes_supported".to_owned(), scopes);
        }
        parse_protected_resource_metadata(&value).unwrap()
    }

    impl TestProvider {
        fn with_client_metadata(mut self, metadata: OAuthClientMetadata) -> Self {
            self.client_metadata = metadata;
            self
        }
    }

    /// 完整 OAuth 服务器的路由（受保护资源 + 授权服务器元数据 + DCR + token）。
    fn oauth_routes(
        origin: &str,
        server: &str,
    ) -> impl Fn(&RecordedRequest) -> (u16, String) + Send + Sync + 'static {
        let origin = origin.to_owned();
        let server = server.to_owned();
        move |request| match request.path().as_str() {
            "/.well-known/oauth-protected-resource/mcp" => (
                200,
                json!({
                    "resource": server,
                    "authorization_servers": [origin.clone()],
                    "scopes_supported": ["org:read"],
                })
                .to_string(),
            ),
            "/.well-known/oauth-authorization-server" => (
                200,
                json!({
                    "issuer": origin,
                    "authorization_endpoint": format!("{origin}/authorize"),
                    "token_endpoint": format!("{origin}/token"),
                    "registration_endpoint": format!("{origin}/register"),
                    "response_types_supported": ["code"],
                    "token_endpoint_auth_methods_supported": ["none"],
                    "code_challenge_methods_supported": ["S256"],
                })
                .to_string(),
            ),
            "/register" => {
                let mut body = request.body_json();
                if let Value::Object(map) = &mut body {
                    map.insert(
                        "client_id".to_owned(),
                        Value::String("test-client".to_owned()),
                    );
                }
                (201, body.to_string())
            }
            "/token" => (
                200,
                json!({"access_token": "at", "token_type": "Bearer", "refresh_token": "rt"})
                    .to_string(),
            ),
            _ => (404, String::new()),
        }
    }

    fn query_value(url: &Url, name: &str) -> Option<String> {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    }

    #[tokio::test]
    async fn run_flow_discovers_registers_and_redirects() {
        let origin = "https://as.example";
        let server = "https://mcp.example/mcp";
        let fetch = MockFetch::new(oauth_routes(origin, server));
        let provider = TestProvider::new();
        provider.update(|state| {
            state.can_save_client = true;
            state.state = Some("expected-state".to_owned());
        });
        let options = OAuthFlowOptions {
            server_url: server.to_owned(),
            fetch: Some(fetch.shared()),
            ..OAuthFlowOptions::default()
        };
        let result = run_flow(&provider, &options).await.unwrap();
        assert_eq!(result, OAuthFlowResult::Redirect);
        assert_eq!(
            fetch.urls(),
            vec![
                "https://mcp.example/.well-known/oauth-protected-resource/mcp".to_owned(),
                "https://as.example/.well-known/oauth-authorization-server".to_owned(),
                "https://as.example/register".to_owned(),
            ]
        );
        let register = &fetch.requests()[2];
        let body = register.body_json();
        assert_eq!(body["scope"], "org:read");
        assert_eq!(body["application_type"], "native");
        assert_eq!(body["redirect_uris"], json!(["http://127.0.0.1/callback"]));

        let url = provider.authorization_url().expect("redirected");
        assert_eq!(url.origin().ascii_serialization(), "https://as.example");
        assert_eq!(url.path(), "/authorize");
        assert_eq!(
            query_value(&url, "client_id").as_deref(),
            Some("test-client")
        );
        assert_eq!(query_value(&url, "scope").as_deref(), Some("org:read"));
        assert_eq!(query_value(&url, "resource").as_deref(), Some(server));
        assert_eq!(
            query_value(&url, "state").as_deref(),
            Some("expected-state")
        );
        assert_eq!(
            query_value(&url, "redirect_uri").as_deref(),
            Some("http://127.0.0.1/callback")
        );
        assert_eq!(
            query_value(&url, "code_challenge_method").as_deref(),
            Some("S256")
        );
        let verifiers = provider.saved_verifiers();
        assert_eq!(verifiers.len(), 1);
        assert_eq!(
            query_value(&url, "code_challenge"),
            Some(BASE64URL.encode(Sha256::digest(verifiers[0].as_bytes())))
        );

        let saved = provider.saved_discovery();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].authorization_server_url, "https://as.example");
        assert_eq!(
            saved[0]
                .authorization_server_metadata
                .as_ref()
                .and_then(AuthorizationServerMetadata::issuer),
            Some("https://as.example")
        );
        assert_eq!(
            saved[0]
                .resource_metadata
                .as_ref()
                .and_then(|metadata| metadata.resource()),
            Some(server)
        );
        assert_eq!(saved[0].resource_metadata_url, None);
        // 客户端信息已持久化。
        assert!(
            provider
                .data
                .lock()
                .unwrap()
                .saved_clients
                .iter()
                .any(|client| client.client_id() == Some("test-client"))
        );
    }

    #[tokio::test]
    async fn run_flow_uses_a_configured_metadata_document_without_caching_discovery() {
        let server = "https://mcp.example/mcp";
        let fetch = MockFetch::new(|request| match request.path().as_str() {
            "/.well-known/oauth-protected-resource/mcp" => (
                200,
                json!({
                    "resource": "https://mcp.example/mcp",
                    "authorization_servers": ["https://mcp.example"],
                })
                .to_string(),
            ),
            "/idp/metadata.json" => (
                200,
                json!({
                    "issuer": "https://idp.example",
                    "authorization_endpoint": "https://as.example/idp/authorize",
                    "token_endpoint": "https://as.example/idp/token",
                    "response_types_supported": ["code"],
                })
                .to_string(),
            ),
            _ => (404, String::new()),
        });
        let provider = TestProvider::new();
        provider.set_client(json!({"client_id": "client"}));
        let options = OAuthFlowOptions {
            server_url: server.to_owned(),
            authorization_server_metadata_url: Some(
                Url::parse("https://as.example/idp/metadata.json").unwrap(),
            ),
            fetch: Some(fetch.shared()),
            ..OAuthFlowOptions::default()
        };
        let result = run_flow(&provider, &options).await.unwrap();
        assert_eq!(result, OAuthFlowResult::Redirect);
        let url = provider.authorization_url().unwrap();
        assert_eq!(
            format!("{}{}", url.origin().ascii_serialization(), url.path()),
            "https://as.example/idp/authorize"
        );
        assert_eq!(
            query_value(&url, "resource").as_deref(),
            Some("https://mcp.example/mcp")
        );
        // 配置的文档不缓存发现结果。
        assert!(provider.saved_discovery().is_empty());
        assert_eq!(
            fetch.urls(),
            vec![
                "https://mcp.example/.well-known/oauth-protected-resource/mcp".to_owned(),
                "https://as.example/idp/metadata.json".to_owned(),
            ],
            "不请求 well-known 授权服务器元数据"
        );

        // 非 https（且非 loopback）的配置文档在发现之前就被拒。
        let insecure_fetch = MockFetch::new(|_| (500, String::new()));
        let error = authorize_mcp(
            &provider,
            &OAuthFlowOptions {
                server_url: server.to_owned(),
                authorization_server_metadata_url: Some(
                    Url::parse("http://idp.example/metadata.json").unwrap(),
                ),
                fetch: Some(insecure_fetch.shared()),
                ..OAuthFlowOptions::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.name(), "OAuthInsecureEndpointError");
        assert!(insecure_fetch.requests().is_empty());
    }

    #[tokio::test]
    async fn run_flow_falls_through_scope_sources_like_the_or_operator() {
        let origin = "https://as.example";
        let server = "https://mcp.example/mcp";
        let fetch = MockFetch::new(|_| (404, String::new()));

        let run =
            |client_metadata: OAuthClientMetadata, scopes: Option<Value>, scope: Option<&str>| {
                let provider = TestProvider::new().with_client_metadata(client_metadata);
                provider.set_client(json!({"client_id": "client"}));
                let mut discovery = cached_discovery(origin, Some(metadata(origin)));
                discovery.resource_metadata = Some(resource_metadata(server, scopes));
                provider.set_discovery(discovery);
                let options = OAuthFlowOptions {
                    server_url: server.to_owned(),
                    scope: scope.map(str::to_owned),
                    fetch: Some(fetch.shared()),
                    ..OAuthFlowOptions::default()
                };
                let provider = Arc::new(provider);
                let provider_for_run = Arc::clone(&provider);
                async move {
                    run_flow(provider_for_run.as_ref(), &options).await.unwrap();
                    provider.authorization_url()
                }
            };

        // `scopes_supported` 有值时覆盖客户端元数据的 scope。
        let url = run(
            OAuthClientMetadata::default(),
            Some(json!(["a", "b"])),
            Some(""),
        )
        .await
        .unwrap();
        assert_eq!(query_value(&url, "scope").as_deref(), Some("a b"));

        // `scopes_supported: []` 拼出空串 → 落到 `clientMetadata.scope`。
        let url = run(
            client_metadata(json!({"scope": "fallback-scope"})),
            Some(json!([])),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            query_value(&url, "scope").as_deref(),
            Some("fallback-scope")
        );

        // 显式 options.scope 最优先。
        let url = run(
            client_metadata(json!({"scope": "fallback-scope"})),
            Some(json!(["a"])),
            Some("explicit"),
        )
        .await
        .unwrap();
        assert_eq!(query_value(&url, "scope").as_deref(), Some("explicit"));
    }

    #[tokio::test]
    async fn run_flow_exchanges_a_code_only_when_iss_matches() {
        let origin = "https://as.example";
        let server = "https://mcp.example/mcp";
        let fetch = MockFetch::new(|request| {
            if request.path() == "/token" {
                (
                    200,
                    json!({"access_token": "token", "token_type": "Bearer"}).to_string(),
                )
            } else {
                (404, String::new())
            }
        });

        async fn exchange(
            fetch: &Arc<MockFetch>,
            code: &str,
            iss: Option<&str>,
            iss_parameter_supported: bool,
            with_metadata: bool,
        ) -> (Result<OAuthFlowResult, OAuthFailure>, Arc<TestProvider>) {
            let origin = "https://as.example";
            let provider = Arc::new(TestProvider::new());
            provider.set_client(json!({"client_id": "client"}));
            provider.update(|state| state.verifier = Some("verifier".to_owned()));
            let mut server_metadata = None;
            if with_metadata {
                let mut value = metadata(origin);
                value.set(
                    "authorization_response_iss_parameter_supported",
                    json!(iss_parameter_supported),
                );
                server_metadata = Some(value);
            }
            provider.set_discovery(cached_discovery(origin, server_metadata));
            let options = OAuthFlowOptions {
                server_url: "https://mcp.example/mcp".to_owned(),
                authorization_code: Some(code.to_owned()),
                iss: iss.map(str::to_owned),
                fetch: Some(fetch.shared()),
                ..OAuthFlowOptions::default()
            };
            let result = run_flow(provider.as_ref(), &options).await;
            (result, provider)
        }

        let (result, _) = exchange(
            &fetch,
            "other",
            Some("https://attacker.example"),
            false,
            true,
        )
        .await;
        assert_eq!(result.unwrap_err().name(), "OAuthIssuerMismatchError");
        let (result, _) = exchange(&fetch, "missing", None, true, true).await;
        assert_eq!(result.unwrap_err().name(), "OAuthIssuerMismatchError");
        let (result, provider) = exchange(&fetch, "matching", Some(origin), true, true).await;
        assert_eq!(result.unwrap(), OAuthFlowResult::Authorized);
        assert_eq!(
            provider
                .tokens()
                .and_then(|tokens| tokens.access_token().map(str::to_owned)),
            Some("token".to_owned())
        );
        let (result, _) = exchange(&fetch, "omitted", None, false, true).await;
        assert_eq!(result.unwrap(), OAuthFlowResult::Authorized);
        let (result, _) = exchange(
            &fetch,
            "unverifiable",
            Some("https://attacker.example"),
            false,
            false,
        )
        .await;
        assert_eq!(result.unwrap_err().name(), "OAuthIssuerMismatchError");
        let (result, _) = exchange(&fetch, "no-metadata", None, false, false).await;
        assert_eq!(result.unwrap(), OAuthFlowResult::Authorized);

        let codes: Vec<String> = fetch
            .requests()
            .into_iter()
            .filter(|request| request.path() == "/token")
            .filter_map(|request| request.param("code"))
            .collect();
        assert_eq!(codes, ["matching", "omitted", "no-metadata"]);
        let _ = server;
    }

    /// 回归测试（评审 finding #2，minor）：`authorization_response_iss_parameter_supported`
    /// 是真值判定（`typeof === "boolean"` 的校验只发生在解析器里）。
    #[tokio::test]
    async fn run_flow_treats_a_non_boolean_iss_flag_as_truthy() {
        let fetch = MockFetch::new(|request| {
            if request.path() == "/token" {
                (
                    200,
                    json!({"access_token": "token", "token_type": "Bearer"}).to_string(),
                )
            } else {
                (404, String::new())
            }
        });
        let mut server_metadata = metadata("https://as.example");
        server_metadata.set(
            "authorization_response_iss_parameter_supported",
            json!("false"),
        );
        let provider = TestProvider::new();
        provider.set_client(json!({"client_id": "client"}));
        provider.update(|state| state.verifier = Some("verifier".to_owned()));
        provider.set_discovery(cached_discovery(
            "https://as.example",
            Some(server_metadata),
        ));
        let error = run_flow(
            &provider,
            &OAuthFlowOptions {
                server_url: "https://mcp.example/mcp".to_owned(),
                authorization_code: Some("code".to_owned()),
                fetch: Some(fetch.shared()),
                ..OAuthFlowOptions::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.name(), "OAuthIssuerMismatchError");
        assert!(fetch.requests().is_empty(), "iss 不匹配时不发令牌请求");
    }

    /// 回归测试（评审 finding #5，minor）：`existing?.refresh_token` 与
    /// `withScope(tokens, existing.scope)` 使用原始值（真值与 `String()`），
    /// 非字符串的 refresh token 仍然发起刷新，非字符串的已授予 scope 原样保存。
    #[tokio::test]
    async fn run_flow_refreshes_with_a_truthy_non_string_refresh_token() {
        let origin = "https://as.example";
        let fetch = MockFetch::new(|_| {
            (
                200,
                json!({"access_token": "a2", "token_type": "Bearer"}).to_string(),
            )
        });
        let provider = TestProvider::new();
        provider.set_client(json!({"client_id": "client"}));
        // 手工构造的令牌（`parseOAuthTokens` 会拒绝非字符串 scope）。
        provider.update(|state| {
            state.tokens = Some(OAuthTokens::from_fields(
                match json!({
                    "access_token": "a1",
                    "refresh_token": 5,
                    "token_type": "Bearer",
                    "scope": 5,
                }) {
                    Value::Object(map) => map,
                    _ => unreachable!(),
                },
            ));
        });
        provider.set_discovery(cached_discovery(origin, Some(metadata(origin))));
        let result = run_flow(
            &provider,
            &OAuthFlowOptions {
                server_url: "https://mcp.example/mcp".to_owned(),
                fetch: Some(fetch.shared()),
                ..OAuthFlowOptions::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(result, OAuthFlowResult::Authorized);
        // `URLSearchParams({refresh_token: 5})` → `"5"`。
        assert_eq!(
            fetch.requests()[0].param("refresh_token").as_deref(),
            Some("5")
        );
        let saved = provider.tokens().expect("saved tokens");
        assert_eq!(saved.access_token(), Some("a2"));
        // TS 的 `{refresh_token: 5, ...tokens}` 会保留原始 5；Rust 的
        // `RefreshAuthorizationOptions.refresh_token: String` 只能携带 `String()` 结果。
        assert_eq!(saved.get("refresh_token"), Some(&json!("5")));
        // `withScope(tokens, 5)` 原样补上非字符串的已授予 scope。
        assert_eq!(saved.get("scope"), Some(&json!(5)));
    }

    #[tokio::test]
    async fn run_flow_refreshes_and_falls_back_per_the_error_rules() {
        let origin = "https://as.example";
        let server = "https://mcp.example/mcp";
        let provider = || {
            let provider = TestProvider::new();
            provider.set_client(json!({"client_id": "client"}));
            provider.set_tokens(json!({
                "access_token": "a1",
                "refresh_token": "r1",
                "token_type": "Bearer",
                "scope": "old-scope",
            }));
            provider.set_discovery(cached_discovery(origin, Some(metadata(origin))));
            provider
        };
        let options = |fetch: Arc<dyn McpFetch>| OAuthFlowOptions {
            server_url: server.to_owned(),
            fetch: Some(fetch),
            ..OAuthFlowOptions::default()
        };

        // 成功：服务端没返回 scope → 保留旧 scope；rotate 的 refresh_token 胜出。
        let fetch = MockFetch::new(|request| {
            if request.path() == "/token" {
                (
                    200,
                    json!({"access_token": "a2", "refresh_token": "r2", "token_type": "Bearer"})
                        .to_string(),
                )
            } else {
                (404, String::new())
            }
        });
        let test_provider = provider();
        let result = run_flow(&test_provider, &options(fetch.shared()))
            .await
            .unwrap();
        assert_eq!(result, OAuthFlowResult::Authorized);
        let saved = test_provider.tokens().unwrap();
        assert_eq!(saved.access_token(), Some("a2"));
        assert_eq!(saved.refresh_token(), Some("r2"));
        assert_eq!(saved.scope(), Some("old-scope"));
        let refresh_request = fetch
            .requests()
            .into_iter()
            .find(|request| request.path() == "/token")
            .unwrap();
        assert_eq!(
            refresh_request.param("grant_type").as_deref(),
            Some("refresh_token")
        );
        assert_eq!(
            refresh_request.param("refresh_token").as_deref(),
            Some("r1")
        );

        // `server_error` → 回落到新的授权。
        let fetch = MockFetch::new(|_| (500, "boom".to_owned()));
        let test_provider = provider();
        let result = run_flow(&test_provider, &options(fetch.shared()))
            .await
            .unwrap();
        assert_eq!(result, OAuthFlowResult::Redirect);
        assert!(test_provider.authorization_url().is_some());
        assert_eq!(test_provider.tokens().unwrap().access_token(), Some("a1"));

        // 非 `server_error` 的 OAuthError → 重新抛出，不回落。
        let fetch = MockFetch::new(|_| (400, json!({"error": "invalid_grant"}).to_string()));
        let test_provider = provider();
        let error = run_flow(&test_provider, &options(fetch.shared()))
            .await
            .unwrap_err();
        assert_eq!(error.as_oauth().unwrap().code, "invalid_grant");
        assert!(test_provider.authorization_url().is_none());

        // 不安全的令牌端点 → 重新抛出。
        let mut insecure_metadata = metadata(origin);
        insecure_metadata.set("token_endpoint", json!("http://as.example/token"));
        let fetch = MockFetch::new(|_| (200, String::new()));
        let test_provider = provider();
        test_provider.set_discovery(cached_discovery(origin, Some(insecure_metadata)));
        let error = run_flow(&test_provider, &options(fetch.shared()))
            .await
            .unwrap_err();
        assert_eq!(error.name(), "OAuthInsecureEndpointError");
        assert!(test_provider.authorization_url().is_none());

        // 信号已 abort → 重新抛出（即使只是 `server_error` 也不回落）。
        let stalling = StallingFetch::new();
        let signal = CancellationToken::new();
        signal.cancel();
        let test_provider = provider();
        let error = run_flow(
            &test_provider,
            &OAuthFlowOptions {
                signal: Some(signal),
                ..options(stalling.shared())
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.name(), "AbortError");
        assert!(test_provider.authorization_url().is_none());
        assert_eq!(stalling.urls(), vec!["https://as.example/token".to_owned()]);
    }

    #[tokio::test]
    async fn run_flow_falls_back_when_saving_refreshed_tokens_fails() {
        let origin = "https://as.example";
        let server = "https://mcp.example/mcp";
        let test_provider = TestProvider::new();
        test_provider.set_client(json!({"client_id": "client"}));
        test_provider.set_tokens(json!({
            "access_token": "a1",
            "refresh_token": "r1",
            "token_type": "Bearer",
        }));
        test_provider.set_discovery(cached_discovery(origin, Some(metadata(origin))));
        // TS 的 try 块同时包住 `refreshAuthorization` 与 `provider.saveTokens`：保存失败也按
        // 同一套规则回落（普通 Error 且未 abort → 回落到新的授权流程）。
        test_provider.update(|state| state.fail_save_tokens = true);
        let fetch = MockFetch::new(|request| {
            if request.path() == "/token" {
                (
                    200,
                    json!({"access_token": "a2", "refresh_token": "r2", "token_type": "Bearer"})
                        .to_string(),
                )
            } else {
                (404, String::new())
            }
        });
        let result = run_flow(
            &test_provider,
            &OAuthFlowOptions {
                server_url: server.to_owned(),
                fetch: Some(fetch.shared()),
                ..OAuthFlowOptions::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(result, OAuthFlowResult::Redirect);
        assert!(test_provider.authorization_url().is_some());
        // 保存失败：旧令牌保持原样。
        let tokens = test_provider.tokens().unwrap();
        assert_eq!(tokens.access_token(), Some("a1"));
        assert_eq!(tokens.refresh_token(), Some("r1"));
    }

    #[tokio::test]
    async fn run_flow_reports_missing_or_unpersistable_client_information() {
        let origin = "https://as.example";
        let server = "https://mcp.example/mcp";
        let fetch = MockFetch::new(|_| (404, String::new()));
        let options = |authorization_code: Option<&str>| OAuthFlowOptions {
            server_url: server.to_owned(),
            authorization_code: authorization_code.map(str::to_owned),
            fetch: Some(fetch.shared()),
            ..OAuthFlowOptions::default()
        };

        // code exchange 但没有客户端信息。
        let provider = TestProvider::new();
        provider.set_discovery(cached_discovery(origin, Some(metadata(origin))));
        let error = run_flow(&provider, &options(Some("code")))
            .await
            .unwrap_err();
        assert_eq!(
            error.message(),
            "OAuth client information is missing during code exchange"
        );

        // 无授权码、又不能持久化注册结果。
        let provider = TestProvider::new();
        provider.set_discovery(cached_discovery(origin, Some(metadata(origin))));
        let error = run_flow(&provider, &options(None)).await.unwrap_err();
        assert_eq!(
            error.message(),
            "OAuth client information cannot be persisted"
        );
    }

    #[tokio::test]
    async fn run_flow_uses_a_client_metadata_document_as_the_client_id() {
        let origin = "https://as.example";
        let server = "https://mcp.example/mcp";
        let fetch = MockFetch::new(|_| (404, String::new()));
        let provider = TestProvider::new();
        provider.set_discovery(cached_discovery(origin, Some(metadata(origin))));
        provider.update(|state| {
            state.client_document = Some(OAuthClientMetadataDocument {
                url: "https://client.example/client.json".to_owned(),
                redirect_url: "https://client.example/callback".to_owned(),
            })
        });
        let options = OAuthFlowOptions {
            server_url: server.to_owned(),
            fetch: Some(fetch.shared()),
            ..OAuthFlowOptions::default()
        };
        let result = run_flow(&provider, &options).await.unwrap();
        assert_eq!(result, OAuthFlowResult::Redirect);
        let url = provider.authorization_url().unwrap();
        assert_eq!(
            query_value(&url, "client_id").as_deref(),
            Some("https://client.example/client.json")
        );
        assert_eq!(
            query_value(&url, "redirect_uri").as_deref(),
            Some("https://client.example/callback")
        );
        // 文档不作为客户端信息存储。
        assert!(provider.data.lock().unwrap().saved_clients.is_empty());

        for invalid in ["https://client.example/", "http://client.example/x.json"] {
            let provider = TestProvider::new();
            provider.set_discovery(cached_discovery(origin, Some(metadata(origin))));
            provider.update(|state| {
                state.client_document = Some(OAuthClientMetadataDocument {
                    url: invalid.to_owned(),
                    redirect_url: "https://client.example/cb".to_owned(),
                })
            });
            let error = run_flow(&provider, &options).await.unwrap_err();
            assert_eq!(error.message(), "Invalid OAuth client metadata URL");
        }
    }

    // -----------------------------------------------------------------------
    // authorizeMcp 的重试规则
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn authorize_mcp_retries_once_after_invalidating_credentials() {
        let origin = "https://as.example";
        let server = "https://mcp.example/mcp";

        // `invalid_client` → `invalidateCredentials("all")` 后重新注册。
        let routes = oauth_routes(origin, server);
        let fetch = MockFetch::new(move |request| match request.path().as_str() {
            "/token" => (400, json!({"error": "invalid_client"}).to_string()),
            _ => routes(request),
        });
        let provider = TestProvider::new();
        provider.update(|state| state.can_save_client = true);
        provider.set_client(json!({"client_id": "old-client"}));
        provider.set_tokens(
            json!({"access_token": "a1", "refresh_token": "r1", "token_type": "Bearer"}),
        );
        provider.set_discovery(cached_discovery(origin, Some(metadata(origin))));
        let options = OAuthFlowOptions {
            server_url: server.to_owned(),
            fetch: Some(fetch.shared()),
            ..OAuthFlowOptions::default()
        };
        let result = authorize_mcp(&provider, &options).await.unwrap();
        assert_eq!(result, OAuthFlowResult::Redirect);
        assert_eq!(provider.invalidations(), vec![CredentialKind::All]);
        let token_requests = fetch
            .requests()
            .into_iter()
            .filter(|request| request.path() == "/token")
            .count();
        assert_eq!(token_requests, 1, "只重试一次");
        assert_eq!(
            query_value(&provider.authorization_url().unwrap(), "client_id").as_deref(),
            Some("test-client")
        );

        // `invalid_grant` → `invalidateCredentials("tokens")`，保留客户端信息。
        let routes = oauth_routes(origin, server);
        let fetch = MockFetch::new(move |request| match request.path().as_str() {
            "/token" => (400, json!({"error": "invalid_grant"}).to_string()),
            _ => routes(request),
        });
        let provider = TestProvider::new();
        provider.update(|state| state.can_save_client = true);
        provider.set_client(json!({"client_id": "client"}));
        provider.set_tokens(
            json!({"access_token": "a1", "refresh_token": "r1", "token_type": "Bearer"}),
        );
        provider.set_discovery(cached_discovery(origin, Some(metadata(origin))));
        let options = OAuthFlowOptions {
            server_url: server.to_owned(),
            fetch: Some(fetch.shared()),
            ..OAuthFlowOptions::default()
        };
        let result = authorize_mcp(&provider, &options).await.unwrap();
        assert_eq!(result, OAuthFlowResult::Redirect);
        assert_eq!(provider.invalidations(), vec![CredentialKind::Tokens]);
        assert_eq!(
            fetch
                .requests()
                .into_iter()
                .filter(|request| request.path() == "/token")
                .count(),
            1
        );
        assert_eq!(
            fetch
                .requests()
                .into_iter()
                .filter(|request| request.path() == "/register")
                .count(),
            0,
            "客户端信息保留，不需要重新注册"
        );
        assert_eq!(
            query_value(&provider.authorization_url().unwrap(), "client_id").as_deref(),
            Some("client")
        );

        // 其他 OAuthError 不重试。
        let fetch = MockFetch::new(|request| match request.path().as_str() {
            "/token" => (400, json!({"error": "invalid_scope"}).to_string()),
            _ => (404, String::new()),
        });
        let provider = TestProvider::new();
        provider.set_client(json!({"client_id": "client"}));
        provider.set_tokens(
            json!({"access_token": "a1", "refresh_token": "r1", "token_type": "Bearer"}),
        );
        provider.set_discovery(cached_discovery(origin, Some(metadata(origin))));
        let error = authorize_mcp(
            &provider,
            &OAuthFlowOptions {
                server_url: server.to_owned(),
                fetch: Some(fetch.shared()),
                ..OAuthFlowOptions::default()
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.as_oauth().unwrap().code, "invalid_scope");
        assert!(provider.invalidations().is_empty());
    }

    // -----------------------------------------------------------------------
    // adaptOAuthProvider
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn adapt_oauth_provider_returns_the_access_token() {
        let provider = Arc::new(TestProvider::new());
        provider.set_tokens(json!({"access_token": "a1", "token_type": "Bearer"}));
        let auth = adapt_oauth_provider(provider.clone());
        assert_eq!(auth.token().await.as_deref(), Some("a1"));
        provider.update(|state| state.tokens = None);
        assert_eq!(auth.token().await, None);
    }

    /// 评审 finding（minor）：`tokens()` 读取失败在冻结的 `AuthProvider::token` 签名
    /// （`Option<String>`，§12）里没有错误出口，按「没有令牌」处理。TS 会把 store 的 rejection
    /// 原样抛给请求方；该偏差登记在模块文档的「已知偏差」一节，这里钉住 Rust 侧的行为。
    #[tokio::test]
    async fn adapt_oauth_provider_treats_a_failing_token_store_as_no_token() {
        let provider = Arc::new(TestProvider::new());
        provider.set_tokens(json!({"access_token": "a1", "token_type": "Bearer"}));
        provider.update(|state| state.fail_tokens_load = true);
        let auth = adapt_oauth_provider(provider);
        assert_eq!(auth.token().await, None);
    }

    #[tokio::test]
    async fn adapt_oauth_provider_shares_one_refresh_between_concurrent_401s() {
        let origin = "https://as.example";
        let fetch = MockFetch::new(|request| {
            if request.path() == "/token" {
                (
                    200,
                    json!({
                        "access_token": "a2",
                        "refresh_token": "r2",
                        "token_type": "Bearer",
                        "expires_in": 3600,
                    })
                    .to_string(),
                )
            } else {
                (404, String::new())
            }
        });
        let delayed: Arc<dyn McpFetch> = Arc::new(DelayedFetch {
            inner: Arc::clone(&fetch),
            delay: Duration::from_millis(20),
        });
        let provider = Arc::new(TestProvider::new());
        provider.set_client(json!({"client_id": "client"}));
        provider.set_tokens(
            json!({"access_token": "a1", "refresh_token": "r1", "token_type": "Bearer"}),
        );
        provider.set_discovery(cached_discovery(origin, Some(metadata(origin))));
        let auth = adapt_oauth_provider(provider.clone());

        let context = |token: &str| {
            unauthorized_context(
                "https://mcp.example/mcp",
                Arc::clone(&delayed),
                Some(token),
                "Bearer",
            )
        };
        let (first, second) = tokio::join!(
            auth.on_unauthorized(context("a1")),
            auth.on_unauthorized(context("a1"))
        );
        first.unwrap();
        second.unwrap();
        // 迟到的旧 token 401：当前令牌已经变了，不再刷新。
        auth.on_unauthorized(context("a1")).await.unwrap();

        let token_requests: Vec<RecordedRequest> = fetch
            .requests()
            .into_iter()
            .filter(|request| request.path() == "/token")
            .collect();
        assert_eq!(token_requests.len(), 1, "并发 401 共享一次刷新");
        assert_eq!(
            token_requests[0].param("refresh_token").as_deref(),
            Some("r1")
        );
        assert_eq!(auth.token().await.as_deref(), Some("a2"));
        assert_eq!(
            provider
                .tokens()
                .and_then(|tokens| tokens.refresh_token().map(str::to_owned)),
            Some("r2".to_owned())
        );
    }

    /// 回归测试（评审 finding #3，minor）：TS 的 `inFlight` 是**已启动**的 promise，
    /// 发起它的请求被 abort（future 被丢弃）时刷新仍会跑完并保存 rotate 后的令牌。
    #[tokio::test]
    async fn adapt_oauth_provider_completes_a_refresh_after_the_caller_is_dropped() {
        let origin = "https://as.example";
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let fetch = Arc::new(GatedFetch {
            requests: Arc::new(StdMutex::new(Vec::new())),
            gate: Arc::clone(&gate),
            status: 200,
            body: json!({
                "access_token": "a2",
                "refresh_token": "r2",
                "token_type": "Bearer",
                "expires_in": 3600,
            })
            .to_string(),
        });
        let provider = Arc::new(TestProvider::new());
        provider.set_client(json!({"client_id": "client"}));
        provider.set_tokens(
            json!({"access_token": "a1", "refresh_token": "r1", "token_type": "Bearer"}),
        );
        provider.set_discovery(cached_discovery(origin, Some(metadata(origin))));
        let auth = adapt_oauth_provider(provider.clone());

        let task_auth = Arc::clone(&auth);
        let task_fetch: Arc<dyn McpFetch> = Arc::clone(&fetch) as Arc<dyn McpFetch>;
        let handle = tokio::spawn(async move {
            task_auth
                .on_unauthorized(unauthorized_context(
                    "https://mcp.example/mcp",
                    task_fetch,
                    Some("a1"),
                    "Bearer",
                ))
                .await
        });
        let mut spins = 0;
        while fetch.requests.lock().unwrap().is_empty() {
            spins += 1;
            assert!(spins < 100_000, "refresh never started");
            tokio::task::yield_now().await;
        }
        // 传输层在请求被 abort 时会丢弃 `onUnauthorized` 的 future。
        handle.abort();
        let _ = handle.await;
        gate.add_permits(1);
        spins = 0;
        while provider.saved_tokens().is_empty() {
            spins += 1;
            assert!(spins < 100_000, "background refresh never saved tokens");
            tokio::task::yield_now().await;
        }
        let saved = provider.saved_tokens();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].access_token(), Some("a2"));
        assert_eq!(saved[0].refresh_token(), Some("r2"));
    }

    #[tokio::test]
    async fn adapt_oauth_provider_asks_for_a_step_up_when_the_server_needs_more_scope() {
        let origin = "https://as.example";
        let fetch = MockFetch::new(|_| (500, String::new()));
        let provider = Arc::new(TestProvider::new());
        provider.set_client(json!({"client_id": "client"}));
        provider.set_tokens(json!({
            "access_token": "a1",
            "refresh_token": "r1",
            "token_type": "Bearer",
            "scope": "repo read:org",
        }));
        provider.set_discovery(cached_discovery(origin, Some(metadata(origin))));
        let auth = adapt_oauth_provider(provider.clone());
        let mut context = unauthorized_context(
            "https://mcp.example/mcp",
            fetch.shared(),
            Some("a1"),
            r#"Bearer error="insufficient_scope", scope="repo admin""#,
        );
        context.status = 403;
        let error = auth.on_unauthorized(context).await.unwrap_err();
        assert_eq!(error.name(), "McpOAuthAuthorizationRequiredError");
        assert!(error.is_oauth_authorization_required());
        let url = provider.authorization_url().expect("redirected");
        assert_eq!(
            query_value(&url, "scope").as_deref(),
            Some("repo read:org admin")
        );
        // 工作授权保留到用户批准新 scope 为止。
        assert_eq!(provider.tokens().unwrap().access_token(), Some("a1"));
        assert!(fetch.requests().is_empty(), "skipRefresh 不请求令牌端点");
    }

    #[tokio::test]
    async fn adapt_oauth_provider_does_nothing_when_another_request_already_refreshed() {
        let fetch = MockFetch::new(|_| (500, String::new()));
        let provider = Arc::new(TestProvider::new());
        provider.set_tokens(
            json!({"access_token": "a2", "refresh_token": "r2", "token_type": "Bearer"}),
        );
        let auth = adapt_oauth_provider(provider.clone());
        auth.on_unauthorized(unauthorized_context(
            "https://mcp.example/mcp",
            fetch.shared(),
            Some("a1"),
            "Bearer",
        ))
        .await
        .unwrap();
        assert!(fetch.requests().is_empty());
        assert_eq!(auth.token().await.as_deref(), Some("a2"));
    }

    /// 评审 finding 5（minor）：`current !== undefined` 判的是**属性存在**，不是"非空字符串"。
    /// 手写 `mcp-auth.json` 里 `access_token` 为数字/`null` 时 TS 仍然认为"有当前值"，
    /// `current !== context.token` → 直接重试、**不**刷新。修复前 Rust 用 `access_token()`
    /// 读成 `None`，于是错误地启动一次刷新。
    #[tokio::test]
    async fn adapt_oauth_provider_treats_defined_non_string_tokens_as_current() {
        // 数字：TS `43 !== "42"` → 另一个请求已经换过令牌 → 直接重试。
        let fetch = MockFetch::new(|_| (500, String::new()));
        let provider = Arc::new(TestProvider::new());
        provider.set_raw_tokens(json!({"access_token": 43, "token_type": "Bearer"}));
        let auth = adapt_oauth_provider(provider.clone());
        auth.on_unauthorized(unauthorized_context(
            "https://mcp.example/mcp",
            fetch.shared(),
            Some("42"),
            "Bearer",
        ))
        .await
        .unwrap();
        assert!(fetch.requests().is_empty(), "no refresh must start");
        assert_eq!(auth.token().await.as_deref(), Some("43"));

        // `null` 也是"已定义"（`null !== undefined`）且与字符串 context.token 不同。
        let fetch = MockFetch::new(|_| (500, String::new()));
        let provider = Arc::new(TestProvider::new());
        provider.set_raw_tokens(json!({"access_token": null, "token_type": "Bearer"}));
        let auth = adapt_oauth_provider(provider.clone());
        auth.on_unauthorized(unauthorized_context(
            "https://mcp.example/mcp",
            fetch.shared(),
            Some("a1"),
            "Bearer",
        ))
        .await
        .unwrap();
        assert!(fetch.requests().is_empty(), "no refresh must start");
        assert_eq!(auth.token().await, None);

        // 反例：`access_token` 键缺失 → `current === undefined` → 走刷新（开始发现请求）。
        let fetch = MockFetch::new(|_| (500, String::new()));
        let provider = Arc::new(TestProvider::new());
        provider.set_raw_tokens(json!({"token_type": "Bearer"}));
        let auth = adapt_oauth_provider(provider.clone());
        let _ = auth
            .on_unauthorized(unauthorized_context(
                "https://mcp.example/mcp",
                fetch.shared(),
                Some("a1"),
                "Bearer",
            ))
            .await;
        assert!(
            !fetch.requests().is_empty(),
            "a missing access_token must start the refresh flow"
        );

        // 存储里仍是同一个非字符串值（`43 === 43`）→ `current === context.token` → 刷新。
        // Rust 的 `context.token` 已被归一化成 `"43"`，同一套归一化后仍然相等。
        let fetch = MockFetch::new(|_| (500, String::new()));
        let provider = Arc::new(TestProvider::new());
        provider.set_raw_tokens(json!({"access_token": 43, "token_type": "Bearer"}));
        let auth = adapt_oauth_provider(provider.clone());
        let _ = auth
            .on_unauthorized(unauthorized_context(
                "https://mcp.example/mcp",
                fetch.shared(),
                Some("43"),
                "Bearer",
            ))
            .await;
        assert!(
            !fetch.requests().is_empty(),
            "an unchanged non-string token must refresh"
        );
    }

    /// 评审 finding 5（minor，续）：step-up 把 `granted.scope` **原样**交给
    /// `scope?.split(/\s+/)`；非字符串（`null`/缺失除外）抛
    /// `TypeError: scope?.split is not a function`，且在 `authorizeMcp` 之前发生。
    /// 没有 challenged scope 时 TS 在**读取 granted 之前**就返回 `undefined`（`if (!challenged)`），
    /// 因此非字符串 scope 不会报错。
    #[tokio::test]
    async fn adapt_oauth_provider_step_up_rejects_a_non_string_scope() {
        let fetch = MockFetch::new(|_| (500, String::new()));
        let provider = Arc::new(TestProvider::new());
        provider.set_raw_tokens(json!({
            "access_token": "a1",
            "token_type": "Bearer",
            "scope": 5,
        }));
        let auth = adapt_oauth_provider(provider.clone());
        let mut context = unauthorized_context(
            "https://mcp.example/mcp",
            fetch.shared(),
            Some("a1"),
            r#"Bearer error="insufficient_scope", scope="repo""#,
        );
        context.status = 403;
        let error = auth.on_unauthorized(context).await.unwrap_err();
        assert_eq!(error.name(), "TypeError");
        assert_eq!(error.to_string(), "scope?.split is not a function");
        assert!(
            fetch.requests().is_empty(),
            "the TypeError precedes authorizeMcp"
        );

        // 没有 challenged scope：不碰 `granted.scope`，走正常的 step-up 授权。
        let mut no_scope = unauthorized_context(
            "https://mcp.example/mcp",
            fetch.shared(),
            Some("a1"),
            r#"Bearer error="insufficient_scope""#,
        );
        no_scope.status = 403;
        let error = auth.on_unauthorized(no_scope).await.unwrap_err();
        assert!(
            !error.is_type_error(),
            "step-up must not read a non-string scope without a challenge scope: {error:?}"
        );
    }

    // -----------------------------------------------------------------------
    // AbortSignal
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn run_flow_stops_when_the_signal_aborts_without_redirecting() {
        // 发现阶段被 abort：不回落，也不保存 verifier/重定向。
        let stalling = StallingFetch::new();
        let provider = Arc::new(TestProvider::new());
        let signal = CancellationToken::new();
        let options = OAuthFlowOptions {
            server_url: "https://mcp.example/mcp".to_owned(),
            fetch: Some(stalling.shared()),
            signal: Some(signal.clone()),
            ..OAuthFlowOptions::default()
        };
        let task_provider = Arc::clone(&provider);
        let task_options = options.clone();
        let handle =
            tokio::spawn(async move { authorize_mcp(task_provider.as_ref(), &task_options).await });
        let mut spins = 0;
        while stalling.urls().is_empty() {
            spins += 1;
            assert!(spins < 100_000, "flow never started");
            tokio::task::yield_now().await;
        }
        signal.cancel();
        let error = handle.await.unwrap().unwrap_err();
        assert_eq!(error.name(), "AbortError");
        assert!(provider.authorization_url().is_none());
        assert!(provider.saved_verifiers().is_empty());
        assert_eq!(
            stalling.urls()[0],
            "https://mcp.example/.well-known/oauth-protected-resource/mcp"
        );

        // 刷新阶段被 abort：即使只是网络错误也不回落到新授权。
        let stalling = StallingFetch::new();
        let provider = Arc::new(TestProvider::new());
        provider.set_client(json!({"client_id": "client"}));
        provider.set_tokens(
            json!({"access_token": "a1", "refresh_token": "r1", "token_type": "Bearer"}),
        );
        provider.set_discovery(cached_discovery(
            "https://as.example",
            Some(metadata("https://as.example")),
        ));
        let signal = CancellationToken::new();
        let options = OAuthFlowOptions {
            server_url: "https://mcp.example/mcp".to_owned(),
            fetch: Some(stalling.shared()),
            signal: Some(signal.clone()),
            ..OAuthFlowOptions::default()
        };
        let task_provider = Arc::clone(&provider);
        let task_options = options.clone();
        let handle =
            tokio::spawn(async move { authorize_mcp(task_provider.as_ref(), &task_options).await });
        let mut spins = 0;
        while stalling.urls().is_empty() {
            spins += 1;
            assert!(spins < 100_000, "flow never started");
            tokio::task::yield_now().await;
        }
        signal.cancel();
        let error = handle.await.unwrap().unwrap_err();
        assert_eq!(error.name(), "AbortError");
        assert!(provider.authorization_url().is_none());
        assert_eq!(stalling.urls()[0], "https://as.example/token");
    }
}
