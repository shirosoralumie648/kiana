//! `extensions/mcp/oauth.ts`：远程 MCP 服务器的 OAuth 登录与凭据存储。
//!
//! TS: `packages/coding-agent/src/extensions/mcp/oauth.ts`。
//!
//! 连接本身从不主动打开浏览器：先发存储的 access token，遇到 401 再尝试 refresh token；
//! 两者都不行时以 `McpOAuthAuthorizationRequiredError` 失败，用户通过 `/mcp` 或
//! `kiana mcp login` 走授权码流程（PKCE、CIMD 或动态注册、回环回调）。
//!
//! 凭据存放在 `<agentDir>/mcp-auth.json`，基础键为 `<mcpNamespace(name)>|<new URL(serverUrl)>`；
//! 发现授权服务器 issuer 后写入键追加 `|<issuer>`，issuer 不匹配的旧条目在带期望 issuer 的
//! 读写路径上被条件性失效。
//!
//! # 与 TS 的对应
//!
//! - `AbortSignal` → [`CancellationToken`]（`ARCHITECTURE.md` §4.2）。
//! - `proper-lockfile` 的目录锁（`<name>.lock` 目录 + mtime 心跳）在这里手写，可与 TS 版
//!   互操作（spec §8.4 #7）。
//! - `McpOAuthStateStore` 是异步 trait，错误用 [`OAuthFailure`]；本模块的公开错误
//!   `Display` 逐字等于 TS 的 `error.message`。
//! - `signInMcpServer` 的取消在 TS 是异常 `McpSignInCancelledError`；Rust 用
//!   [`McpSignInError::Cancelled`] 表达，其余错误保留 [`OAuthFailure`] 以便按类判定。
//!
//! # 在调用链中的位置
//!
//! - 连接期：`crate::mcp::runtime::McpServerConnection` 用 [`create_mcp_auth_provider`] 构造
//!   [`McpAuthProvider`]，交给 MCP 传输层当 `AuthProvider` 使用。401 时由本文件决定“刷新”
//!   还是“要求用户登录”。
//! - 登录期：`crate::mcp::cli`（`kiana mcp login`）与 `crate::mcp::index`（`/mcp` 界面）调用
//!   [`sign_in_mcp_server`]，并各自实现 [`McpSignInPrompt`] 负责展示 URL、收集粘贴的地址。
//! - 存储：本文件只决定 `mcp-auth.json` 的键与写回的 JSON 文本；状态对象每个字段的形状、
//!   键序与显式 `null` 的语义在 `kiana_mcp::oauth::McpOAuthState`（见该类型的文档）。
//!
//! # 执行流程
//!
//! 1. 发请求前 [`McpAuthProvider`] 的 `token()` 读存储：令牌存在且未接近过期就直接返回；
//!    接近过期且存在非空 refresh token 才在跨进程锁内刷新。
//! 2. 收到 401 时 `on_unauthorized()` 解析 `WWW-Authenticate`：`insufficient_scope` 直接要求
//!    重新登录（刷新不会扩大 scope），其余情况刷新一次后由传输层重试。
//! 3. 用户登录（[`sign_in_mcp_server`]）：先在回环地址监听回调 → 生成授权 URL 交给
//!    [`McpSignInPrompt`] → 浏览器回调或用户粘贴的地址先到者胜出 → 用 `code` 换令牌。
//!
//! # 并发与持久化约束
//!
//! - 同一进程内的并发刷新合并成一个 future（[`McpAuthProvider`] 的 `refreshing` 槽）；
//! - 跨进程用 `<lock_dir>/mcp-auth-refresh-<key 的 SHA-256 前 16 位>.lock` **目录**互斥。
//!   持有者每 10 s 更新一次 mtime，20 s 未更新视为陈旧锁可被接管，等待上限 25 s；
//!   `lock_dir` 默认是 `<agentDir>`，调用方传了自定义 backend 又没给目录时没有跨进程锁。
//! - `mcp-auth.json` 的读改写都在 `AuthStorageBackend::with_lock` 内完成，且是同步调用：
//!   文件后端在等锁时会阻塞当前线程。内容解析失败按 `SyntaxError` 上抛（有测试锁定），
//!   不会被当成空表——否则随后的写回会丢掉其它服务器的条目。
//!
//! # 阅读顺序
//!
//! [`McpOAuthSettings`] → [`McpOAuthCredentialStore`] / [`McpOAuthServerStore`]（键与 legacy
//! 迁移）→ [`McpAuthProvider`]（连接期刷新）→ [`sign_in_mcp_server`] / `run_sign_in_flow`
//! （交互登录）。PKCE/发现/回调服务器等协议细节在 `kiana-mcp` 的 `oauth` 模块。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::FutureExt as _;
use futures::future::{BoxFuture, Shared};
use kiana_ai::auth::oauth::pkce::base64url_encode;
use kiana_ai::utils::oauth_page::{oauth_error_html, oauth_success_html};
use kiana_config::auth::storage::{AuthStorageBackend, FileAuthStorageBackend, LockResult};
use kiana_core::config::{APP_NAME, get_agent_dir};
use kiana_core::mcp_servers::{McpClientRegistration, mcp_namespace};
use kiana_mcp::auth_provider::{
    AuthProvider, FetchInit, FetchResponse, McpFetch, McpTransportError, ReqwestFetch,
    UnauthorizedContext,
};
use kiana_mcp::oauth::errors::OAuthFailure;
use kiana_mcp::oauth::{
    AuthorizationServerMetadata, McpOAuthAuthorizationRequiredError, McpOAuthProvider,
    McpOAuthProviderOptions, McpOAuthState, McpOAuthStateStore, OAuthCallbackPage,
    OAuthCallbackServer, OAuthCallbackServerOptions, OAuthChallenge, OAuthClientInformationMixed,
    OAuthClientMetadata, OAuthClientMetadataDocument, OAuthFlowOptions, OAuthFlowResult,
    OAuthTokens, authorize_mcp, parse_www_authenticate, step_up_scope,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use url::Url;

const CALLBACK_HOST: &str = "127.0.0.1";
const CALLBACK_PATH: &str = "/callback";
/// kiana 的 Client ID Metadata Document 所在位置：`client.json` 与 `<callback ID>/client.json`。
///
/// 根地址是 `KIANA_API_BASE_URL/oauth`；测试可覆盖（TS 通过 `vi.stubGlobal("fetch")`
/// 观察同一个 URL）。
///
/// 这是进程级全局状态：`Some` 时优先于由 `KIANA_API_BASE_URL` 派生的地址，同一测试二进制里
/// 并发运行的测试会互相看见（[`set_client_metadata_base_url_for_tests`] 只用于测试）。
static CLIENT_METADATA_BASE_URL_OVERRIDE: Mutex<Option<String>> = Mutex::new(None);

/// 仅供测试：覆盖 CIMD 基地址（`None` 恢复由 `KIANA_API_BASE_URL` 派生）。
#[doc(hidden)]
pub fn set_client_metadata_base_url_for_tests(url: Option<String>) {
    let mut guard = CLIENT_METADATA_BASE_URL_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = url;
}

/// 生效的 CIMD 基地址：测试覆盖优先，其次 `KIANA_API_BASE_URL/oauth`；末尾的 `/` 被去掉。
///
/// 两者都没有时返回 `None`，由 [`client_metadata_document`] 将其转成一条明确的错误，
/// 而不是让任何 URL 变成相对路径。
fn client_metadata_base_url() -> Option<String> {
    CLIENT_METADATA_BASE_URL_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .or_else(|| kiana_core::config::get_api_url("oauth"))
        .map(|url| url.trim_end_matches('/').to_string())
}
/// 没有存储的 redirect URI 时刷新用的地址。刷新永远不会重定向用户。
const FALLBACK_REDIRECT_URL: &str = "http://127.0.0.1/callback";
/// 距离过期这么近的 access token 先刷新再发送。
const REFRESH_SKEW_MS: f64 = 30_000.0;
/// 每个授权服务器请求的上限。
const OAUTH_REQUEST_TIMEOUT_MS: u64 = 15_000;
/// 停止续期（进程被杀）的刷新锁在这么久之后被接管。
const REFRESH_LOCK_STALE_MS: u64 = 20_000;
/// 等待别的进程刷新：比 stale 锁的寿命更长。
const REFRESH_LOCK_WAIT_MS: u64 = 25_000;
const REFRESH_LOCK_RETRY_MS: u64 = 100;

/// OAuth 设置。TS: `oauth.ts#McpOAuthSettings`。
///
/// 所有字段都是可选的：`None` 表示沿用默认行为（自动发现授权服务器、系统分配回调端口、
/// 不额外请求 scope 等）。[`McpAuthProvider`] 只在需要刷新时才通过 `settings` 闭包读取它，
/// 因此配置解析失败不阻止普通连接，只让刷新失败；[`sign_in_mcp_server`] 则一开始就要求
/// 能解析出设置（`callback_url` 不合法会在监听端口之前失败）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpOAuthSettings {
    /// 预注册的 client id。
    pub client_id: Option<String>,
    /// 已解析的 client secret。
    pub client_secret: Option<String>,
    /// 回环回调端口。
    pub callback_port: Option<u16>,
    /// 回环 redirect URI；见 `McpOAuthConfig.callback_url`。
    pub callback_url: Option<String>,
    /// 请求的 scope（空格分隔）。
    pub scope: Option<String>,
    /// 动态注册时的 `client_name`；默认 `APP_NAME`。
    pub client_name: Option<String>,
    /// 见 `McpOAuthConfig.client_registration`。
    pub client_registration: Option<McpClientRegistration>,
    /// 见 `McpOAuthConfig.auth_server_metadata_url`。
    pub auth_server_metadata_url: Option<Url>,
}

/// 回环回调服务器的监听参数。TS: `oauth.ts#CallbackSettings`（不导出）。
#[derive(Debug, Clone)]
struct CallbackSettings {
    /// 监听地址。
    host: String,
    /// redirect URI 里的主机名。
    redirect_host: String,
    port: Option<u16>,
    path: String,
    /// 端口固定时的完整 redirect URI。
    fixed_redirect_url: Option<String>,
}

/// `new URL(...)` 失败的 TS `TypeError("Invalid URL")`。
fn invalid_url() -> OAuthFailure {
    OAuthFailure::type_error("Invalid URL")
}

/// 去掉 IPv6 字面量的一对方括号（`url.hostname.replace(/^\[|\]$/g, "")`）。
fn strip_brackets(host: &str) -> String {
    let host = host.strip_prefix('[').unwrap_or(host);
    let host = host.strip_suffix(']').unwrap_or(host);
    host.to_string()
}

/// 把设置里的回调字段归一成监听参数。
///
/// 端口优先级：`callback_url` 解析出的端口（默认端口视为未写）> `callback_port` > 交给系统
/// 分配。`callback_url` 缺省是 `http://127.0.0.1/callback`。只要端口来自配置（上面两种来源
/// 之一），`fixed_redirect_url` 就固定为对应字符串：登录时按它原样注册，服务器端按字符串
/// 比较 redirect URI 才不会失配；端口留给系统分配时它是 `None`，redirect URI 由监听后的
/// 真实端口拼出（见 `OAuthCallbackServer::redirect_url`）。
///
/// `localhost` 只在监听地址上映射成 `127.0.0.1`，redirect URI 里仍写 `localhost`
/// （`redirect_host`），因为浏览器会自行决定连 `::1` 还是 `127.0.0.1`。
///
/// 失败：`callback_url` 不能按 URL 解析、或端口无法写回 URL 时返回
/// `TypeError("Invalid URL")`。
fn callback_settings(settings: &McpOAuthSettings) -> Result<CallbackSettings, OAuthFailure> {
    let raw = settings
        .callback_url
        .clone()
        .unwrap_or_else(|| format!("http://{CALLBACK_HOST}{CALLBACK_PATH}"));
    let mut url = Url::parse(&raw).map_err(|_| invalid_url())?;
    let address = strip_brackets(url.host_str().unwrap_or(""));
    let explicit_port = url.port();
    let port = explicit_port.or(settings.callback_port);
    // 配置里带端口的 URI 原样使用（服务器按字符串比较）。
    let fixed_redirect_url = if explicit_port.is_some() {
        Some(settings.callback_url.clone().unwrap_or(raw))
    } else if let Some(port) = port {
        url.set_port(Some(port)).map_err(|_| invalid_url())?;
        Some(url.to_string())
    } else {
        None
    };
    Ok(CallbackSettings {
        // `localhost` 监听在 127.0.0.1；浏览器在 ::1 拒绝时会回退到它。
        host: if address == "localhost" {
            CALLBACK_HOST.to_string()
        } else {
            address.clone()
        },
        redirect_host: address,
        port,
        path: url.path().to_string(),
        fixed_redirect_url,
    })
}

/// JS 的正则 `\s` 集合（ECMAScript WhiteSpace + LineTerminator）。
fn is_js_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\t' | '\n' | '\u{000B}' | '\u{000C}' | '\r' | ' ' | '\u{00A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// `scope.split(/\s+/).filter(Boolean)`。
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

/// 两份 scope 列表的并集，各出现一次。
///
/// 顺序 = 首次出现的顺序；输入全部为空时返回 `None`（请求里就完全不发送 scope 参数）。
fn merge_scopes(scopes: &[Option<&str>]) -> Option<String> {
    let mut merged: Vec<&str> = Vec::new();
    for scope in scopes.iter().flatten() {
        for item in split_js_whitespace(scope) {
            if !item.is_empty() && !merged.contains(&item) {
                merged.push(item);
            }
        }
    }
    if merged.is_empty() {
        None
    } else {
        Some(merged.join(" "))
    }
}

/// `JSON.parse` 的结果：只有非数组对象才算状态表。
///
/// 文件不存在（`None`）与纯空白内容都按空表处理；解析失败保留 JS 的 `SyntaxError` 名字与
/// 消息（有测试按名字断言），数组、数字、`null` 之类的非对象值也按空表处理。
fn parse_states(content: Option<&str>) -> Result<Map<String, Value>, OAuthFailure> {
    let Some(content) = content else {
        return Ok(Map::new());
    };
    if kiana_json::js_string::js_trim(content).is_empty() {
        return Ok(Map::new());
    }
    let parsed = kiana_json::js_parse::parse(content).map_err(|error| {
        OAuthFailure::Transport(McpTransportError::named("SyntaxError", error.message))
    })?;
    match parsed {
        Value::Object(map) => Ok(map),
        _ => Ok(Map::new()),
    }
}

/// `JSON.stringify(states, null, 2) + "\n"`（JS 的数字格式与键序，不只是缩进）。
///
/// 返回值是**整份文件**的新内容（`LockResult::next`），带结尾换行；不能改用 `serde_json`
/// 默认的漂亮打印，否则数字（如 `1.2345678901234567e+19`）与键序会变。
fn serialize_states(states: &Map<String, Value>) -> String {
    let body = kiana_json::js_object::stringify_pretty(&Value::Object(states.clone()), 2);
    format!("{body}\n")
}

/// `String(new URL(serverUrl))`：WHATWG 序列化（`href`）。
///
/// 解析失败时原样返回输入串（TS 的 `new URL` 会抛错，但键的生成在这里不该让调用失败：
/// 后续真正发请求/构造 provider 时会各自报错）。
fn normalized_server_url(server_url: &str) -> String {
    Url::parse(server_url)
        .map(|url| url.to_string())
        .unwrap_or_else(|_| server_url.to_string())
}

/// 键：按名字与 URL（共享 URL 的服务器各有账号），以及旧版的按 URL 键。
///
/// 现名键形如 `mcp__my_work|https://mcp.example.com/mcp`：经 `mcp_namespace` 处理，
/// 名字里的 `-` 会变成 `_`，因此 `my-work` 与 `my_work` 是同一个存储名。legacy 键是旧版本
/// 只按 URL 存的入口，只被 `McpOAuthServerStore` 的 `load` 迁移、`tokens` 的回退读取与
/// `remove` 的回退删除使用（`remove` 在现名键缺席且 legacy 键存在时会删除该 legacy 条目
/// 并写回文件）。
fn store_keys(name: &str, server_url: &str) -> (String, String) {
    let legacy_key = normalized_server_url(server_url);
    (
        format!("{}|{}", mcp_namespace(name), legacy_key),
        legacy_key,
    )
}

/// 带 issuer 的键：`<base>|<issuer>`。
///
/// issuer 来自授权服务器元数据的 `issuer`（RFC 8414）。同一 server URL 换了授权服务器时
/// 键不同，旧凭据不会被错误复用；`McpOAuthServerStore::load` 在期望 issuer 与存储值
/// 不一致时让它失效。
fn issuer_store_key(base_key: &str, issuer: &str) -> String {
    format!("{base_key}|{issuer}")
}

/// 从持久化状态里读 issuer（`discovery.authorizationServerMetadata.issuer`）。
fn state_issuer(state: &McpOAuthState) -> Option<String> {
    state
        .discovery
        .as_ref()?
        .authorization_server_metadata
        .as_ref()?
        .issuer()
        .map(str::to_string)
}

/// `Value` 形态的状态里的 issuer（宽松：非对象/缺字段都是 `None`）。
fn value_issuer(value: &Value) -> Option<String> {
    state_from_value(Some(value))
        .as_ref()
        .and_then(state_issuer)
}

/// `McpOAuthStateStore` 的读取错误包装（`AuthStorageBackend` 的 `AuthError`）。
fn auth_error(error: impl std::fmt::Display) -> OAuthFailure {
    OAuthFailure::Transport(McpTransportError::new(error.to_string()))
}

/// `JSON.parse` 的值 → `McpOAuthState`；非对象按缺席处理。
///
/// `null` 与反序列化失败都返回 `None`；普通对象即使缺字段也能成功（`serverUrl` 缺省为空串），
/// 因此 `{}` 会得到一个空状态而不是 `None`（与 `McpOAuthState` 的 `Deserialize` 实现一致）。
fn state_from_value(value: Option<&Value>) -> Option<McpOAuthState> {
    let value = value?;
    if value.is_null() {
        return None;
    }
    serde_json::from_value(value.clone()).ok()
}

/// `OAuthFailure` → `AuthError`（`AuthStorageBackend::withLock` 回调的错误类型）。
fn to_auth_error(error: &OAuthFailure) -> kiana_ai::auth::types::AuthError {
    kiana_ai::auth::types::AuthError::with_name(error.name(), error.message())
}

/// `AuthError` → `OAuthFailure`。`McpTransportError::named` 的 name 必须是 `&'static str`，
/// 只保留已知的固定名字（其余按 `Error`，与 `new Error(message)` 一致）。
fn from_auth_error(error: kiana_ai::auth::types::AuthError) -> OAuthFailure {
    fn known(name: &str) -> &'static str {
        match name {
            "SyntaxError" => "SyntaxError",
            "TypeError" => "TypeError",
            "AbortError" => "AbortError",
            _ => "Error",
        }
    }
    OAuthFailure::Transport(McpTransportError::named(known(&error.name), error.message))
}

/// 删除 `<base_key>` 家族里显式写了其它 issuer 的条目；返回是否删除。
///
/// 家族 = 基础键本身、`<base_key>|*` 的 issuer 键；URL-only 的 legacy 键不属于任何
/// issuer（旧版没有该字段），由调用方决定是否处理。缺 issuer 的值也保留：无法证明
/// 它属于别的授权服务器。
fn remove_mismatched_issuer_entries(
    states: &mut Map<String, Value>,
    base_key: &str,
    expected: &str,
) -> bool {
    let prefix = format!("{base_key}|");
    let stale: Vec<String> = states
        .iter()
        .filter(|(key, value)| {
            let matches_family = key.as_str() == base_key || key.starts_with(&prefix);
            matches_family && value_issuer(value).is_some_and(|issuer| issuer != expected)
        })
        .map(|(key, _)| key.clone())
        .collect();
    if stale.is_empty() {
        return false;
    }
    for key in stale {
        states.shift_remove(&key);
    }
    true
}

/// 一个服务器的 OAuth 状态读写器（`mcp-auth.json` 的一个键）。
///
/// 由 [`McpOAuthCredentialStore::for_server`] 构造；持有共享的后端句柄、现名键与 legacy 键。
/// 所有读写都走 `AuthStorageBackend::with_lock`：读改写在同一把文件锁内完成，`Clone` 只是
/// 复制句柄与字符串、指向同一份文件内容。`lock_dir` 为 `None` 时 [`Self::with_refresh_lock`]
/// 退化为直接执行（只有本进程内的 single-flight 去重）。
///
/// TS: `oauth.ts#McpOAuthCredentialStore.forServer()` 的返回值。
#[derive(Clone)]
pub struct McpOAuthServerStore {
    backend: AuthStorageBackend,
    /// 不带 issuer 的 `<namespace>|<url>`，用于扫描 issuer 键与迁移。
    base_key: String,
    /// 当前写入键：期望 issuer 存在时是 `<base>|<issuer>`，否则等于 `base_key`。
    key: String,
    /// 期望的 issuer；`None` 表示未指定（从现有 issuer 键回退读取）。
    issuer: Option<String>,
    legacy_key: String,
    lock_dir: Option<PathBuf>,
}

impl McpOAuthServerStore {
    /// 换一个期望 issuer（同一个 backend/名字/URL）。
    ///
    /// 期望 issuer 存在时读写 `<base>|<issuer>`；`None` 时保持旧版 `<base>` 行为，并在读取时
    /// 回退到唯一的 issuer 键。发现的 issuer 与期望值不一致时该状态不会被使用。
    pub fn with_issuer(mut self, issuer: Option<&str>) -> Self {
        self.issuer = issuer.map(str::to_string);
        self.key = match &self.issuer {
            Some(issuer) => issuer_store_key(&self.base_key, issuer),
            None => self.base_key.clone(),
        };
        self
    }

    /// 该值是否满足期望 issuer（`None` 期望接受任意/缺失）。
    fn issuer_matches(&self, value: &Value) -> bool {
        match (&self.issuer, value_issuer(value)) {
            (None, _) => true,
            (Some(expected), Some(actual)) => expected == &actual,
            // 期望 issuer 存在但存储值没有 issuer：无法验证，按不匹配处理（条件性失效）。
            (Some(_), None) => false,
        }
    }

    /// 在状态表里选当前服务器的值。
    ///
    /// 返回 `(value, removals, migration)`：
    /// - `removals`：需要从文件里删除的键（issuer 不匹配的现名/基础键）；
    /// - `migration`：把旧基础键/legacy 键迁移到 issuer 键的 `(new_key, value)`。
    ///
    /// 期望 issuer 为 `None` 且基础键缺席时，会接受**唯一一个** `<base>|<issuer>` 条目
    /// （这是 `for_server` 对已升级存储的兼容路径；多个 issuer 条目说明用户换过授权服务器，
    /// 此时不猜，返回 `None` 由调用方重新登录）。
    fn select_value(
        &self,
        states: &Map<String, Value>,
    ) -> (Option<Value>, Vec<String>, Option<(String, Value)>) {
        if let Some(value) = states.get(&self.key) {
            if kiana_json::js_string::is_truthy(Some(value)) {
                if self.issuer_matches(value) {
                    return (Some(value.clone()), Vec::new(), None);
                }
                return (None, vec![self.key.clone()], None);
            }
        }
        // 基础键（旧版无 issuer 键）。
        if let Some(value) = states.get(&self.base_key)
            && kiana_json::js_string::is_truthy(Some(value))
        {
            if self.issuer_matches(value) {
                if self.issuer.is_some() {
                    return (
                        Some(value.clone()),
                        vec![self.base_key.clone()],
                        Some((self.key.clone(), value.clone())),
                    );
                }
                return (Some(value.clone()), Vec::new(), None);
            }
            // 值里有明确的 issuer 且与期望不同：失效。没有 issuer（旧版）时也按不匹配处理。
            return (None, vec![self.base_key.clone()], None);
        }
        // 只按 URL 的 legacy 键（更旧版本）。
        if let Some(value) = states.get(&self.legacy_key)
            && kiana_json::js_string::is_truthy(Some(value))
        {
            if self.issuer_matches(value) {
                // 无论是否指定 issuer，都迁移到当前写入键（issuer 键或基础键），并把
                // 只按 URL 的 legacy 条目清掉——旧行为是“第一个加载的服务器接管它”。
                return (
                    Some(value.clone()),
                    vec![self.legacy_key.clone()],
                    Some((self.key.clone(), value.clone())),
                );
            }
            return (None, vec![self.legacy_key.clone()], None);
        }
        // 未指定期望 issuer：回退到唯一的 issuer 键。
        if self.issuer.is_none() {
            let prefix = format!("{}|", self.base_key);
            let mut matches = states
                .iter()
                .filter(|(key, value)| {
                    key.starts_with(&prefix) && kiana_json::js_string::is_truthy(Some(value))
                })
                .map(|(_, value)| value);
            if let Some(value) = matches.next()
                && matches.next().is_none()
            {
                return (Some(value.clone()), Vec::new(), None);
            }
        }
        (None, Vec::new(), None)
    }

    /// `backend.withLock(fn)`：在（文件后端是跨进程的）锁内运行回调，并把 `LockResult`
    /// 解包成返回给调用方的值。回调返回的 `next` 决定是否覆盖整份文件；回调错误先转成
    /// `AuthError` 交给后端，后端错误再转回 [`OAuthFailure`]。
    fn with_lock<T>(
        &self,
        f: impl FnOnce(Option<&str>) -> Result<LockResult<T>, OAuthFailure>,
    ) -> Result<T, OAuthFailure> {
        self.backend
            .with_lock(|current| f(current).map_err(|error| to_auth_error(&error)))
            .map_err(from_auth_error)
    }

    /// 只读地解析当前状态表。回调返回 `LockResult::new`（没有 `next`），因此不会写文件。
    fn read_states(&self) -> Result<Map<String, Value>, OAuthFailure> {
        self.with_lock(|current| {
            let states = parse_states(current)?;
            Ok(LockResult::new(states))
        })
    }

    /// `write(update)`：解析 → 修改 → 写回。
    ///
    /// 解析失败以 `SyntaxError` 返回，且不会写盘（`update` 还没执行）；成功时写回的是
    /// [`serialize_states`] 生成的整份内容，文件里其它服务器的条目原样保留。
    fn write_states(
        &self,
        update: impl FnOnce(&mut Map<String, Value>),
    ) -> Result<(), OAuthFailure> {
        self.with_lock(|current| {
            let mut states = parse_states(current)?;
            update(&mut states);
            Ok(LockResult::with_next((), serialize_states(&states)))
        })
    }

    /// 在服务器刷新令牌期间独占（跨进程）。
    ///
    /// `proper-lockfile` 的磁盘协议：目标路径后追加 `.lock` 并创建**目录**，mtime 是心跳；
    /// 超过 `REFRESH_LOCK_STALE_MS` 未续期的锁被接管。重试 250 次、每次 100 ms
    /// （`REFRESH_LOCK_WAIT_MS`）。
    ///
    /// 两层结构：先确保 `lock_dir` 存在（unix 上权限 0700），再对
    /// `mcp-auth-refresh-<sha256(key) 的前 16 个十六进制字符>` 这个**按服务器**的路径加锁，
    /// 因此不同服务器互不阻塞、同一服务器的多个进程串行。
    ///
    /// `f` 被要求返回 `BoxFuture<'static>`：它在锁 guard 存活期间被 await，不能借用 `&self`，
    /// 所以实现里先 clone 出 store/server_url 等再 move 进闭包。
    ///
    /// 失败：创建目录、读取锁元数据或加锁的 IO 错误都包成 `OAuthFailure::Error`；等待超过
    /// 25 s 返回 `"Lock file is already being held"`。锁目录被判定陈旧后只再尝试一次
    /// （`stale_allowed`），避免两个进程互相删除对方刚建的锁。
    pub async fn with_refresh_lock<T, F>(&self, f: F) -> Result<T, OAuthFailure>
    where
        F: FnOnce() -> BoxFuture<'static, Result<T, OAuthFailure>>,
    {
        let Some(lock_dir) = &self.lock_dir else {
            return f().await;
        };
        create_lock_dir(lock_dir).map_err(auth_error)?;
        let hash = kiana_ai::model_catalog::sha256_hex(&self.key);
        let lock_path = lock_dir.join(format!("mcp-auth-refresh-{}", &hash[..16]));
        let _guard = acquire_refresh_lock(&lock_path).await?;
        f().await
    }
}

/// `mkdirSync(lockDir, { recursive: true, mode: 0o700 })`。
fn create_lock_dir(lock_dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(lock_dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(lock_dir)
    }
}

/// 更新锁目录的 mtime（Windows 打开目录需要 `FILE_FLAG_BACKUP_SEMANTICS`）。
///
/// mtime 就是锁的心跳。心跳任务触碰失败时直接退出，不再上报错误：锁会在 20 s 后变“陈旧”，
/// 由别的进程接管。
fn touch_lock(lock_path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(lock_path)?
    };
    #[cfg(not(windows))]
    let file = std::fs::File::open(lock_path)?;
    let times = std::fs::FileTimes::new().set_modified(SystemTime::now());
    file.set_times(times)
}

/// 刷新锁：持有时每隔 10 s 触碰一次 mtime（`stale / 2`），Drop 时删除目录。
///
/// Drop 先取消心跳再尝试 `remove_dir`；删除失败被忽略（目录可能已被接管者或用户删掉）。
struct RefreshLockGuard {
    path: PathBuf,
    stop: CancellationToken,
}

impl Drop for RefreshLockGuard {
    fn drop(&mut self) {
        self.stop.cancel();
        let _ = std::fs::remove_dir(&self.path);
    }
}

/// 忙等重试期间不阻塞运行时。
async fn sleep_ms(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

/// `lockfile.lock(target, {...})` 的等价物；返回时已持有 `<target>.lock` 目录。
///
/// 阶段：① `create_dir` 原子地抢锁，成功即 touch 一次并启动心跳；② 已存在时读 mtime 判断
/// 是否陈旧，陈旧则删除并只再试一次；③ 否则每 100 ms 重试，总预算 `REFRESH_LOCK_WAIT_MS`。
/// `create_dir` 不是递归的——父目录由调用方（[`McpOAuthServerStore::with_refresh_lock`]）先
/// 创建。锁目录恰好在检查时消失（`NotFound`）视为释放，立即重试且不消耗一次预算。
async fn acquire_refresh_lock(target: &Path) -> Result<RefreshLockGuard, OAuthFailure> {
    let lock_path = PathBuf::from(format!("{}.lock", target.to_string_lossy()));
    let mut stale_allowed = true;
    let mut retries: u64 = 0;
    loop {
        match std::fs::create_dir(&lock_path) {
            Ok(()) => {
                let _ = touch_lock(&lock_path);
                return Ok(start_heartbeat(lock_path));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                match std::fs::metadata(&lock_path) {
                    Ok(metadata) => {
                        let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
                        let age = SystemTime::now()
                            .duration_since(modified)
                            .unwrap_or_default();
                        if stale_allowed && age > Duration::from_millis(REFRESH_LOCK_STALE_MS) {
                            let _ = std::fs::remove_dir(&lock_path);
                            // `acquireLock(file, { ...options, stale: 0 })`：只再试一次，不再判 stale。
                            stale_allowed = false;
                            continue;
                        }
                    }
                    Err(stat_error) if stat_error.kind() == std::io::ErrorKind::NotFound => {
                        // 锁目录刚好被释放：立即重试，不消耗一次重试。
                        continue;
                    }
                    Err(stat_error) => return Err(auth_error(stat_error)),
                }
                if retries >= REFRESH_LOCK_WAIT_MS / REFRESH_LOCK_RETRY_MS {
                    return Err(OAuthFailure::error("Lock file is already being held"));
                }
                retries += 1;
                sleep_ms(REFRESH_LOCK_RETRY_MS).await;
            }
            Err(error) => return Err(auth_error(error)),
        }
    }
}

/// 启动心跳任务并返回 guard。
///
/// 第一次 `interval.tick()` 立即返回（创建锁时刚 touch 过），所以循环先跳过它，之后每
/// `stale / 2`（10 s）触碰一次。任务在 `touch_lock` 失败或 guard 的 stop 令牌触发时结束；
/// 心跳停止不会让当前持锁者立即失锁——另一个进程要等 mtime 超过 20 s 才会接管。
fn start_heartbeat(lock_path: PathBuf) -> RefreshLockGuard {
    let stop = CancellationToken::new();
    let task_stop = stop.clone();
    let path = lock_path.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(REFRESH_LOCK_STALE_MS / 2));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // 第一次 tick 立即返回；创建锁时已经触碰过。
        interval.tick().await;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if touch_lock(&path).is_err() {
                        break;
                    }
                }
                () = task_stop.cancelled() => break,
            }
        }
    });
    RefreshLockGuard {
        path: lock_path,
        stop,
    }
}

/// 每个服务器的 OAuth 状态（客户端注册、令牌、进行中的 PKCE verifier）在 `mcp-auth.json`。
///
/// 一个实例对应一份存储文件；[`Self::for_server`] 产出按 `(name, serverUrl)` 定位的
/// [`McpOAuthServerStore`]。后端是文件时，读写通过 `AuthStorageBackend::with_lock` 跨进程互斥；
/// 内存后端只有进程内串行门（测试用）。注意对文件的读改写是**同步**的（内部是
/// `with_lock`）：在 async 上下文里调用会阻塞当前执行线程，直到拿到文件锁或失败，而不是
/// 让出任务。
///
/// TS: `oauth.ts#McpOAuthCredentialStore`。
#[derive(Clone)]
pub struct McpOAuthCredentialStore {
    backend: AuthStorageBackend,
    /// 刷新锁文件所在目录；没有时刷新只在本进程内串行。
    lock_dir: Option<PathBuf>,
}

impl McpOAuthCredentialStore {
    /// `new McpOAuthCredentialStore(backend?, lockDir?)`。
    ///
    /// `backend = None` 使用 `<agentDir>/mcp-auth.json`，且 `lockDir` 默认 `<agentDir>`；
    /// 传入 backend 而没给 `lockDir` 时**不**做跨进程锁（与 TS 构造函数的条件一致）。
    ///
    /// 注意：`backend` 为 `None` 时传入的 `lock_dir` 会被忽略（始终用 agent 目录）。文件后端
    /// 的第一次读写会创建父目录与 `mcp-auth.json`（即使只是读），见 `FileAuthStorageBackend`。
    pub fn new(backend: Option<AuthStorageBackend>, lock_dir: Option<PathBuf>) -> Self {
        match backend {
            Some(backend) => McpOAuthCredentialStore { backend, lock_dir },
            None => {
                let agent_dir = get_agent_dir();
                let auth_path = agent_dir.join("mcp-auth.json");
                McpOAuthCredentialStore {
                    backend: AuthStorageBackend::File(FileAuthStorageBackend::new(
                        &auth_path.to_string_lossy(),
                    )),
                    lock_dir: Some(agent_dir),
                }
            }
        }
    }

    /// 某个服务器的状态读写器。TS: `forServer(name, serverUrl)`。
    ///
    /// 键由 `store_keys` 决定：名字经 `mcp_namespace` 归一（`-`→`_`、前缀 `mcp__`）后与
    /// 规范化 URL 拼接，因此共享同一 URL 的两个服务器各有独立凭据；legacy 键只按 URL，用于
    /// 兼容旧文件。
    pub fn for_server(&self, name: &str, server_url: &str) -> McpOAuthServerStore {
        self.for_server_with_issuer(name, server_url, None)
    }

    /// 带期望 issuer 的 [`Self::for_server`]。
    ///
    /// 键变成 `<namespace>|<url>|<issuer>`；`load`/`save` 都只使用这个键，issuer 不匹配的
    /// 旧状态会被删除（条件性失效）。不知道 issuer 时传 `None`，仍可从唯一的 issuer 键读回。
    pub fn for_server_with_issuer(
        &self,
        name: &str,
        server_url: &str,
        issuer: Option<&str>,
    ) -> McpOAuthServerStore {
        let (base_key, legacy_key) = store_keys(name, server_url);
        let issuer = issuer.map(str::to_string);
        let key = match &issuer {
            Some(issuer) => issuer_store_key(&base_key, issuer),
            None => base_key.clone(),
        };
        McpOAuthServerStore {
            backend: self.backend.clone(),
            base_key,
            key,
            issuer,
            legacy_key,
            lock_dir: self.lock_dir.clone(),
        }
    }

    /// 存储的 issuer（`discovery.authorizationServerMetadata.issuer`），没有则 `None`。
    pub fn stored_issuer(
        &self,
        name: &str,
        server_url: &str,
    ) -> Result<Option<String>, OAuthFailure> {
        let store = self.for_server(name, server_url);
        let states = store.read_states()?;
        let (state, _, _) = store.select_value(&states);
        Ok(state.as_ref().and_then(value_issuer))
    }

    /// 按**已存储的 issuer** 打开作用域存储（运行时登录/刷新路径用）。
    ///
    /// 不知道 issuer 时退化为 [`Self::for_server`]（基础键，可从唯一的 issuer 键读回）。
    /// 这把 `stored_issuer` 与 `for_server_with_issuer` 接进生产路径：登录/刷新不再总是
    /// 以“无期望 issuer”读写，已有 issuer 的凭据会被严格限定在同一个授权服务器下。
    pub fn for_server_with_stored_issuer(
        &self,
        name: &str,
        server_url: &str,
    ) -> McpOAuthServerStore {
        let issuer = self.stored_issuer(name, server_url).ok().flatten();
        self.for_server_with_issuer(name, server_url, issuer.as_deref())
    }

    /// 按当前存储的 issuer 清理其它 issuer 的条目（运行时登录成功后调用）。
    ///
    /// 读取失败时返回错误；没有 issuer 时是 no-op。返回值与
    /// [`Self::invalidate_if_issuer_mismatch`] 一致（是否删除了条目）。
    pub fn invalidate_stored_issuer_mismatches(
        &self,
        name: &str,
        server_url: &str,
    ) -> Result<bool, OAuthFailure> {
        let issuer = self.stored_issuer(name, server_url)?;
        self.invalidate_if_issuer_mismatch(name, server_url, issuer.as_deref())
    }

    /// 条件性失效：删除所有 issuer 与 `expected` 不一致的条目；返回是否删除。
    ///
    /// `expected: None` 是 no-op（没有可比较的目标）。只在存储里**明确写了** issuer 且与
    /// 期望不同时删除；缺失 issuer 的旧条目保留（由调用方决定是否用
    /// [`Self::for_server_with_issuer`] 的期望模式强制失效）。
    pub fn invalidate_if_issuer_mismatch(
        &self,
        name: &str,
        server_url: &str,
        expected: Option<&str>,
    ) -> Result<bool, OAuthFailure> {
        let Some(expected) = expected else {
            return Ok(false);
        };
        let store = self.for_server(name, server_url);
        store.with_lock(|current| {
            let mut states = parse_states(current)?;
            if !remove_mismatched_issuer_entries(&mut states, &store.base_key, expected) {
                return Ok(LockResult::new(false));
            }
            Ok(LockResult::with_next(true, serialize_states(&states)))
        })
    }

    /// 某个服务器已存储的令牌（用于发现别的进程完成的登录）。不接管 legacy 键。
    ///
    /// TS 的 `backend.withLock` 是同步调用：文件读不到或 `JSON.parse` 失败会把异常抛给调用方，
    /// 因此这里返回 [`Result`] 而不是把错误折叠成 `None`。键的选择与 `load` 的迁移判定不同：
    /// `tokens` 只在现名键缺席或显式为 `null` 时回退到 legacy 键（`load` 用的是 JS 真值判定，
    /// 且会迁移），并且 `tokens` 不迁移 legacy 状态。取到 `tokens` 字段后按宽松方式反序列化，
    /// 形状不对则返回 `Ok(None)`（不视为文件损坏）。
    pub fn tokens(
        &self,
        name: &str,
        server_url: &str,
    ) -> Result<Option<OAuthTokens>, OAuthFailure> {
        let store = self.for_server(name, server_url);
        let states = store.read_states()?;
        let (state, _, _) = store.select_value(&states);
        let Some(state) = state else {
            return Ok(None);
        };
        let Some(tokens) = state.as_object().and_then(|state| state.get("tokens")) else {
            return Ok(None);
        };
        Ok(serde_json::from_value(tokens.clone()).ok())
    }

    /// 删除某个服务器的凭据；返回是否删除了东西。也会删除服务器会接管的 legacy 状态。
    ///
    /// 没有可删的东西时**不**写回文件（返回 `{result:false}`、没有 `next`），避免无谓的磁盘写入；
    /// 读取/解析/写入错误向上传播。
    ///
    /// 若现名键与 legacy 键同时存在，只删现名键——legacy 条目会留下。
    /// 判断“是否存在”用的是键存在性而非值真值：值为 `null` 的条目也会被删掉并写回。
    pub fn remove(&self, name: &str, server_url: &str) -> Result<bool, OAuthFailure> {
        let store = self.for_server(name, server_url);
        store.with_lock(|current| {
            let mut states = parse_states(current)?;
            let prefix = format!("{}|", store.base_key);
            let stored: Vec<String> = states
                .keys()
                .filter(|key| {
                    *key == &store.base_key || *key == &store.legacy_key || key.starts_with(&prefix)
                })
                .cloned()
                .collect();
            if stored.is_empty() {
                return Ok(LockResult::new(false));
            }
            for key in stored {
                states.shift_remove(&key);
            }
            Ok(LockResult::with_next(true, serialize_states(&states)))
        })
    }
}

impl Default for McpOAuthCredentialStore {
    fn default() -> Self {
        McpOAuthCredentialStore::new(None, None)
    }
}

// 存储契约（见 `kiana_mcp::oauth::McpOAuthStateStore`）：`load` 可能写盘（迁移 legacy 键），
// `save` 只替换本服务器的条目；两者都在文件锁内完成读改写。
#[async_trait]
impl McpOAuthStateStore for McpOAuthServerStore {
    /// `load()`：第一个加载 legacy 状态的服务器接管它，同 URL 的其它服务器重新登录。
    ///
    /// 判定用 JS truthiness（`is_truthy`）：现名键是真值时直接反序列化返回；否则若 legacy 键
    /// 是真值，就把它的值写到现名键下、删除 legacy，并在同一个 `withLock` 里写回文件——
    /// **一次只读的 `load` 也可能写盘**。两个键都不真时返回现名键的反序列化结果（通常是
    /// `None`）。损坏的 JSON 以 `SyntaxError` 上抛，不会被当成“没有凭据”。
    async fn load(&self) -> Result<Option<McpOAuthState>, OAuthFailure> {
        self.with_lock(|current| {
            let mut states = parse_states(current)?;
            let (value, removals, migration) = self.select_value(&states);
            // 有失效条目或需要迁移时才写回：普通读取保持只读。
            if removals.is_empty() && migration.is_none() {
                return Ok(LockResult::new(
                    value
                        .as_ref()
                        .and_then(|value| state_from_value(Some(value))),
                ));
            }
            for key in removals {
                states.shift_remove(&key);
            }
            if let Some((new_key, value)) = migration {
                states.insert(new_key, value);
            }
            let result = value
                .as_ref()
                .and_then(|value| state_from_value(Some(value)));
            Ok(LockResult::with_next(result, serialize_states(&states)))
        })
    }

    /// `save(state)`：整体替换这个服务器的状态。
    ///
    /// 只改 `self.key` 一项；文件里其它服务器的条目原样保留。序列化失败（`to_value` 报错）
    /// 以 `OAuthFailure::Error` 返回且不写盘。
    async fn save(&self, state: McpOAuthState) -> Result<(), OAuthFailure> {
        let value =
            serde_json::to_value(&state).map_err(|error| OAuthFailure::error(error.to_string()))?;
        // issuer 进凭据键：状态里已有 discovery 时写到 `<base>|<issuer>`；没有 issuer 时
        // 保持旧键。写入后清掉同族的旧基础键/其它 issuer 键，避免同一凭据存在两份，
        // 也避免换过授权服务器后旧 issuer 的凭据被再次读回。
        let issuer = state_issuer(&state);
        let key = match &issuer {
            Some(issuer) => issuer_store_key(&self.base_key, issuer),
            None => self.key.clone(),
        };
        self.write_states(|states| {
            states.insert(key.clone(), value);
            if let Some(issuer) = &issuer {
                remove_mismatched_issuer_entries(states, &self.base_key, issuer);
            }
            // issuer 键取代同名的旧基础键；legacy 键是“同 URL 的任意服务器”共用的旧入口，
            // 不能因为一个服务器保存就删掉（其它服务器可能仍依赖它）。
            if key != self.base_key {
                states.shift_remove(&self.base_key);
            }
        })
    }
}

/// `fetch` 加上每次请求 [`OAUTH_REQUEST_TIMEOUT_MS`] 的上限（叠加调用方自己的 signal）。
///
/// 超时取 `init.timeout` 与 15 s 的较小值：调用方更紧的预算不被放宽，缺失时补上 15 s。
/// `init` 里的 `signal` 原样透传，两者在传输层叠加生效。
struct TimedFetch {
    inner: Arc<dyn McpFetch>,
    timeout: Duration,
}

#[async_trait]
impl McpFetch for TimedFetch {
    async fn fetch(
        &self,
        url: &Url,
        mut init: FetchInit,
    ) -> Result<FetchResponse, McpTransportError> {
        // 只收紧、不放宽：已有超时更短就保留，没有则用本模块的 15 s 预算。
        init.timeout = Some(match init.timeout {
            Some(existing) => existing.min(self.timeout),
            None => self.timeout,
        });
        self.inner.fetch(url, init).await
    }
}

/// `timedFetch(fetch = globalThis.fetch)`。
fn timed_fetch(inner: Arc<dyn McpFetch>) -> Arc<dyn McpFetch> {
    Arc::new(TimedFetch {
        inner,
        timeout: Duration::from_millis(OAUTH_REQUEST_TIMEOUT_MS),
    })
}

/// `globalThis.fetch` 的等价物（共享的 reqwest 客户端）。
fn default_fetch() -> Arc<dyn McpFetch> {
    Arc::new(ReqwestFetch::new())
}

/// 已注册的 redirect URI。`"redirect_uris" in client ? client.redirect_uris : []`。
///
/// 字段不是数组（含 `null`/缺席）时返回空；数组里的非字符串元素被跳过，不报错——
/// 这份数据来自旧的 `mcp-auth.json`，坏元素不应阻止登录，只影响“复用端口/客户端”的优化。
fn registered_redirect_urls(client: Option<&OAuthClientInformationMixed>) -> Vec<String> {
    let Some(client) = client else {
        return Vec::new();
    };
    let Some(Value::Array(items)) = client.get("redirect_uris") else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// 12 个字符的服务器 URL 标识（与 Codex 相同的算法）。
///
/// 去掉 fragment 后对 URL 文本做 SHA-256，取前 9 字节做 base64url（9 字节恰好编码成 12 个
/// 字符）。它同时是 CIMD 文档路径与回调路径的一部分，因此同一服务器每次登录必须得到相同
/// ID；URL 解析失败返回 `TypeError("Invalid URL")`。
fn callback_id(server_url: &str) -> Result<String, OAuthFailure> {
    let mut url = Url::parse(server_url).map_err(|_| invalid_url())?;
    url.set_fragment(None);
    let digest = Sha256::digest(url.as_str().as_bytes());
    Ok(base64url_encode(&digest[..9]))
}

/// kiana 的 Client ID Metadata Document（`clientRegistration: "cimd"`）。
///
/// 输入授权服务器的元数据，输出“文档 URL + 本次应使用的 redirect URI”。只有服务器同时声明
/// 支持 CIMD 文档（`client_id_metadata_document_supported`）与公共客户端
/// （`token_endpoint_auth_methods_supported` 含 `"none"`）才可用，否则返回明确的错误——本
/// 仓库不会在这种情况下静默回退到动态注册（有测试锁定）。
///
/// 两种文档形态：
/// - 服务器支持 RFC 9207 的 `iss` 参数时，所有服务器共用一个 `/client.json`，redirect URI
///   原样返回；
/// - 否则用按服务器区分的 `/<callback_id>/client.json`，并把 redirect URI 的路径改写成
///   `/callback/<callback_id>`（会覆盖设置里的自定义 path）。调用方负责把这个路径加入回调
///   服务器的 `extra_paths`，否则回调收不到。
///
/// 基地址缺失（未配置 `KIANA_API_BASE_URL` 且没有测试覆盖）时也返回错误，而不是拼出相对 URL。
fn client_metadata_document(
    server_url: &str,
    redirect_url: &str,
    metadata: Option<&AuthorizationServerMetadata>,
) -> Result<OAuthClientMetadataDocument, OAuthFailure> {
    let supports_documents = metadata
        .and_then(|metadata| metadata.boolean("client_id_metadata_document_supported"))
        .unwrap_or(false);
    // 公共客户端：不带 client secret，认证方式必须是 none。
    let supports_none = metadata
        .and_then(|metadata| metadata.string_array("token_endpoint_auth_methods_supported"))
        .is_some_and(|methods| methods.contains(&"none"));
    if !supports_documents || !supports_none {
        return Err(OAuthFailure::error(
            "The authorization server does not support Client ID Metadata Documents for public clients; remove oauth.clientRegistration \"cimd\"",
        ));
    }
    let Some(client_metadata_base_url) = client_metadata_base_url() else {
        return Err(OAuthFailure::error(
            "MCP OAuth Client ID Metadata Documents are unavailable: set KIANA_API_BASE_URL or use oauth.clientRegistration \"dcr\"",
        ));
    };
    let names_issuer = metadata
        .and_then(|metadata| metadata.boolean("authorization_response_iss_parameter_supported"))
        .unwrap_or(false);
    if names_issuer {
        return Ok(OAuthClientMetadataDocument {
            url: format!("{client_metadata_base_url}/client.json"),
            redirect_url: redirect_url.to_string(),
        });
    }
    let id = callback_id(server_url)?;
    let mut redirect = Url::parse(redirect_url).map_err(|_| invalid_url())?;
    redirect.set_path(&format!("{CALLBACK_PATH}/{id}"));
    Ok(OAuthClientMetadataDocument {
        url: format!("{client_metadata_base_url}/{id}/client.json"),
        redirect_url: redirect.to_string(),
    })
}

/// 空 `onRedirect`（刷新永远不打开浏览器）。
///
/// 刷新路径若仍需要用户授权，流程会返回 `Redirect` 而不是调用它；`run_refresh` 把这种结果
/// 转成 `AuthorizationRequired`。
fn noop_on_redirect() -> kiana_mcp::oauth::provider::OnRedirectFn {
    Arc::new(|_url: Url| Box::pin(async { Ok(()) }) as BoxFuture<'static, Result<(), OAuthFailure>>)
}

/// `createProvider(serverUrl, store, settings, redirectUrl, onRedirect)`。
///
/// 把 [`McpOAuthSettings`] 映射成 provider 选项：`client_name` 缺省取 `APP_NAME`；
/// `client_id`/`client_secret` 原样传入（空串由 provider 自己当作未配置）；状态存储绑定到
/// 这个 provider；配置了 CIMD 时安装 `client_metadata_document` 闭包，文档 URL 在 provider
/// 真正需要客户端标识时才计算。刷新路径传空 `onRedirect`，登录路径传记录 URL 的闭包。
///
/// 这里不做任何 IO，错误只在 provider 构造阶段产生（`McpOAuthProvider::new` 的校验）。
fn create_provider(
    server_url: &str,
    store: Arc<dyn McpOAuthStateStore>,
    settings: &McpOAuthSettings,
    redirect_url: &str,
    on_redirect: kiana_mcp::oauth::provider::OnRedirectFn,
) -> Result<McpOAuthProvider, OAuthFailure> {
    let mut client_metadata_fields = Map::new();
    client_metadata_fields.insert(
        "client_name".to_string(),
        Value::String(
            settings
                .client_name
                .clone()
                .unwrap_or_else(|| APP_NAME.to_string()),
        ),
    );
    let client_metadata = OAuthClientMetadata::from_fields(client_metadata_fields);
    let mut options =
        McpOAuthProviderOptions::new(server_url, redirect_url, client_metadata, on_redirect);
    options.client_id = settings.client_id.clone();
    options.client_secret = settings.client_secret.clone();
    options.store = Some(store);
    if settings.client_registration == Some(McpClientRegistration::Cimd) {
        let server = server_url.to_string();
        let redirect = redirect_url.to_string();
        options.client_metadata_document = Some(Arc::new(move |metadata| {
            client_metadata_document(&server, &redirect, metadata).map(Some)
        }));
    }
    McpOAuthProvider::new(options)
}

/// 进行中的令牌刷新（同进程共享）。
///
/// 用 `Shared` 而不是普通 future：多个等待者各持一个句柄 await 同一个结果，先完成的调用者
/// 不会把结果“消费”掉；`Result<(), OAuthFailure>` 可克隆，因此失败也能共享。
type RefreshFuture = Shared<BoxFuture<'static, Result<(), OAuthFailure>>>;

/// `createMcpAuthProvider` 的参数。
///
/// `settings` 是闭包而不是值：解析被推迟到真正需要刷新时，因此配置错误只让刷新失败。
/// `on_challenge` 在每次 401（或 `insufficient_scope` 403）解析出挑战后立即调用（包括随后
/// 不刷新、直接要求登录的情况），调用方据此记录“需要登录”，见 `crate::mcp::runtime` 的
/// `McpServerConnection`。
pub struct McpAuthProviderOptions {
    /// MCP 服务器 URL。
    pub server_url: String,
    /// 这个服务器的状态存储（含刷新锁）。
    pub store: McpOAuthServerStore,
    /// 只在需要刷新时调用，因此解析失败只让刷新失败。
    pub settings: Arc<dyn Fn() -> Result<McpOAuthSettings, String> + Send + Sync>,
    /// 收到服务器的 `WWW-Authenticate` 挑战。
    pub on_challenge: Arc<dyn Fn(OAuthChallenge) + Send + Sync>,
}

/// MCP 连接的 auth provider：发送存储的 access token，快过期或 401 后刷新。
///
/// 需要用户登录（包括 `insufficient_scope`）时以 `McpOAuthAuthorizationRequiredError`
/// 失败。TS: `oauth.ts#McpAuthProvider`。
///
/// # 刷新去重（single-flight）
///
/// 同一进程中并发触发刷新时只发一次网络请求：第一个调用者创建共享 future 放进
/// `refreshing` 槽，其余调用者 await 同一个 future；该 future 结束时槽被清空。Rust 的 future
/// 是惰性的，所以第一个调用者还会（在 Tokio 运行时内）spawn 一个驱动任务：即使发起者的
/// await 被取消，刷新仍会跑完并把轮换后的令牌存盘。
///
/// # 与传输层的契约
///
/// `has_on_unauthorized()` 返回 `true`，传输层才会在 401 后调用 `on_unauthorized` 并重试；
/// 缺省实现返回 `false`，401 会直接失败。
#[derive(Clone)]
pub struct McpAuthProvider {
    server_url: String,
    store: McpOAuthServerStore,
    settings: Arc<dyn Fn() -> Result<McpOAuthSettings, String> + Send + Sync>,
    on_challenge: Arc<dyn Fn(OAuthChallenge) + Send + Sync>,
    refreshing: Arc<tokio::sync::Mutex<Option<RefreshFuture>>>,
}

impl McpAuthProvider {
    /// 当前进行中的刷新（若有）。
    async fn current_refresh(&self) -> Option<RefreshFuture> {
        self.refreshing.lock().await.clone()
    }

    /// 用新令牌替换 `stale_token`（过期或被拒的那个）。
    ///
    /// 单飞逻辑：整个进程内同一 provider 的并发刷新共享一次网络往返。槽锁只保护“是否有
    /// 进行中的刷新”，在 await 刷新 future 之前已经释放。`Shared` future 由槽与驱动任务共同
    /// 持有，因此发起者取消自己那次 await 不会中断刷新。
    ///
    /// `stale_token` 是调用方看到并被拒/判过期的令牌；`run_refresh` 在跨进程锁内会重新读
    /// 存储并与它比较，若别的进程已经刷新过就直接成功返回。`challenge` 提供
    /// `resource_metadata` 与 scope，只在 401 路径有值。
    async fn refresh(
        &self,
        stale_token: Option<String>,
        fetch: Arc<dyn McpFetch>,
        challenge: Option<OAuthChallenge>,
    ) -> Result<(), OAuthFailure> {
        let mut guard = self.refreshing.lock().await;
        if let Some(existing) = guard.clone() {
            // 已有刷新在跑：等同一个结果，不再发第二次请求。
            drop(guard);
            return existing.await;
        }
        let this = self.clone();
        let slot = Arc::clone(&self.refreshing);
        let work = async move { this.run_refresh(stale_token, fetch, challenge).await };
        // 结束（成功或失败）先清槽再交还结果：之后的调用者会重新发起刷新，而不是拿到旧结果。
        let flight: RefreshFuture = async move {
            let result = work.await;
            *slot.lock().await = None;
            result
        }
        .boxed()
        .shared();
        *guard = Some(flight.clone());
        drop(guard);
        // TS 的 promise 创建后立即开始执行；Rust 的 future 惰性，因此用后台任务驱动。
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let driver = flight.clone();
            handle.spawn(async move {
                let _ = driver.await;
            });
        }
        // 没有 Tokio 运行时则无法 spawn；此时由下面的 `flight.await` 驱动，只要还有等待者，
        // 刷新仍会跑完。
        flight.await
    }

    /// 在一次跨进程刷新锁内完成刷新。
    ///
    /// 步骤：① 加锁后重新 `load()`，若存储里的 access token 已不是 `stale_token`，说明别的
    /// 进程刚刷新过，直接返回成功；② 没有非空 refresh token → `AuthorizationRequired`；
    /// ③ 解析设置并确定 redirect URI（固定 URI > 上次注册的 > 占位地址）；④ 用空
    /// `onRedirect` 构造 provider，带着挑战里的 `resource_metadata`/scope 跑 `authorize_mcp`
    /// （`skip_refresh: false`）；⑤ 若流程仍返回 `Redirect`（拿不到令牌），同样报
    /// `AuthorizationRequired`。
    ///
    /// 错误：锁、存储、网络与授权服务器错误都以 `OAuthFailure` 返回；设置解析错误被包成
    /// `OAuthFailure::Error`。本函数不吞错误——调用方 [`McpAuthProvider::refresh`] 决定是否
    /// 忽略。
    async fn run_refresh(
        &self,
        stale_token: Option<String>,
        fetch: Arc<dyn McpFetch>,
        challenge: Option<OAuthChallenge>,
    ) -> Result<(), OAuthFailure> {
        let store = self.store.clone();
        let locked_store = store.clone();
        let server_url = self.server_url.clone();
        let settings_fn = Arc::clone(&self.settings);
        store
            .with_refresh_lock(move || {
                let store = locked_store.clone();
                let server_url = server_url.clone();
                let settings_fn = Arc::clone(&settings_fn);
                Box::pin(async move {
                    // 锁内重新读：等待锁期间可能有别的进程完成了刷新。
                    let state = store.load().await?;
                    let current_token = state
                        .as_ref()
                        .and_then(|state| state.tokens.as_ref())
                        .and_then(|tokens| tokens.string("access_token"))
                        .map(str::to_string);
                    // 存储里的令牌已经不是调用方看到的那个：别的刷新已经生效，直接成功。
                    if current_token != stale_token {
                        return Ok(());
                    }
                    let refresh_token = state
                        .as_ref()
                        .and_then(|state| state.tokens.as_ref())
                        .and_then(|tokens| tokens.string("refresh_token"))
                        .filter(|token| !token.is_empty());
                    // 没有 refresh token（或被清空）：只能让用户走浏览器登录。
                    if refresh_token.is_none() {
                        return Err(OAuthFailure::AuthorizationRequired(
                            McpOAuthAuthorizationRequiredError::new(),
                        ));
                    }
                    // 设置在这里才解析：解析失败只让刷新失败，不影响普通请求。
                    let settings = settings_fn().map_err(OAuthFailure::error)?;
                    let callback = callback_settings(&settings)?;
                    let stored_client = state
                        .as_ref()
                        .and_then(|state| state.client_information.as_ref());
                    // redirect URI 只用于构造 provider / 令牌请求：刷新永远不会真的重定向。
                    // 优先级：配置里固定的 URI > 上次注册的第一个 redirect_uri > 占位地址。
                    let redirect_url = callback
                        .fixed_redirect_url
                        .clone()
                        .or_else(|| registered_redirect_urls(stored_client).into_iter().next());
                    let redirect_url =
                        redirect_url.unwrap_or_else(|| FALLBACK_REDIRECT_URL.to_string());
                    let provider = create_provider(
                        &server_url,
                        Arc::new(store.clone()),
                        &settings,
                        &redirect_url,
                        noop_on_redirect(),
                    )?;
                    // `skip_refresh: false`：authorize_mcp 会先用存储里的 refresh token 换新
                    // 令牌；挑战里的 resource_metadata 与 scope 原样传给流程。
                    let result = authorize_mcp(
                        &provider,
                        &OAuthFlowOptions {
                            server_url: server_url.clone(),
                            resource_metadata_url: challenge
                                .as_ref()
                                .and_then(|challenge| challenge.resource_metadata_url.clone()),
                            authorization_server_metadata_url: settings
                                .auth_server_metadata_url
                                .clone(),
                            scope: challenge
                                .as_ref()
                                .and_then(|challenge| challenge.scope.clone()),
                            fetch: Some(timed_fetch(fetch)),
                            skip_refresh: false,
                            ..OAuthFlowOptions::default()
                        },
                    )
                    .await?;
                    // 防御：刷新路径本不应需要用户。真走到重定向说明 refresh token 不可用，
                    // 按“需要登录”上报。
                    if result == OAuthFlowResult::Redirect {
                        return Err(OAuthFailure::AuthorizationRequired(
                            McpOAuthAuthorizationRequiredError::new(),
                        ));
                    }
                    Ok(())
                })
            })
            .await
    }

    /// 等没有刷新在跑（退出前不让轮换后的令牌丢失）。
    ///
    /// 只等调用时快照到的那一次刷新，之后新发起的刷新不在等待范围内；刷新的失败被忽略。
    /// 调用方是 `McpServerConnection::close`：服务器已答复的刷新可能轮换了 refresh token，
    /// 不等它写完就退出会丢掉 grant。
    pub async fn settled(&self) {
        if let Some(flight) = self.current_refresh().await {
            let _ = flight.await;
        }
    }
}

/// 当前时间（epoch 毫秒）。
fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as f64)
        .unwrap_or(0.0)
}

#[async_trait]
impl AuthProvider for McpAuthProvider {
    /// 发送存储的 access token；快过期时先刷新。失败则继续用旧 token，由 401 决定。
    ///
    /// 顺序：① 本进程有刷新在跑时先等它结束（避免发出正在被轮换的旧令牌）；② 读存储；
    /// ③ 判定“接近过期”：`tokensExpireAt` 有值时按 30 s 余量比较，缺失但 JSON 里是显式
    /// `null` 时同样视为已过期（见 `McpOAuthState::null_keys`）；④ 只有存在非空 refresh
    /// token 才尝试刷新，否则把现状交给 401 路径；⑤ 刷新结果不在这里上报：重新读一次存储，
    /// 能读到就用（刷新失败时通常还是旧令牌），读不到返回 `None`。
    ///
    /// 读存储失败也返回 `None`：`AuthProvider::token` 没有错误通道。
    async fn token(&self) -> Option<String> {
        // 等本进程的刷新落地，再读存储，避免拿到即将被替换的令牌。
        if let Some(flight) = self.current_refresh().await {
            let _ = flight.await;
        }
        let Ok(state) = self.store.load().await else {
            return None;
        };
        let state = state?;
        let token = state
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.string("access_token"))
            .map(str::to_string);
        // 显式 null 的 tokensExpireAt 表示“已过期”（TS 里 null !== undefined，参与算术后
        // 恒为已过期）；只有字段缺席或类型不符才算“没有过期信息”。
        let expired = match state.tokens_expire_at {
            Some(expires_at) => expires_at - REFRESH_SKEW_MS <= now_ms(),
            None => state.null_keys.iter().any(|key| key == "tokensExpireAt"),
        };
        let has_refresh_token = state
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.string("refresh_token"))
            .is_some_and(|token| !token.is_empty());
        if !expired || !has_refresh_token {
            // 没过期，或没有可用的 refresh token：先照发，401 会再触发 on_unauthorized。
            return token;
        }
        // 刷新错误被忽略；能否恢复由 401 重试或登录流程决定。
        let _ = self.refresh(token.clone(), default_fetch(), None).await;
        match self.store.load().await {
            Ok(Some(state)) => state
                .tokens
                .as_ref()
                .and_then(|tokens| tokens.string("access_token"))
                .map(str::to_string),
            _ => None,
        }
    }

    /// 401：解析挑战并刷新；需要更多 scope 时要求重新登录。
    ///
    /// 先解析 `WWW-Authenticate` 并调用 `on_challenge` 通知调用方（即使接下来不刷新），
    /// 再决定：`insufficient_scope` 不刷新——刷新保留已授予的 scope，扩大 scope 只能走浏览器
    /// 登录；其余情况把被拒的令牌与挑战交给 `McpAuthProvider::refresh`。
    async fn on_unauthorized(&self, context: UnauthorizedContext) -> Result<(), McpTransportError> {
        let challenge = parse_www_authenticate(context.header("www-authenticate").as_deref());
        // 先通知调用方本次挑战（无论能否自动恢复），供 UI 标记“需要登录”。
        (self.on_challenge)(challenge.clone());
        // 刷新保留已授予的 scope；要更多 scope 只能重新登录。
        if challenge.error.as_deref() == Some("insufficient_scope") {
            return Err(McpTransportError::from(
                OAuthFailure::AuthorizationRequired(McpOAuthAuthorizationRequiredError::new()),
            ));
        }
        self.refresh(
            context.token.clone(),
            context.fetch.clone(),
            Some(challenge),
        )
        .await
        .map_err(McpTransportError::from)
    }

    /// 返回 `true`：本实现提供 `on_unauthorized`，传输层据此在 401 后重试一次。
    fn has_on_unauthorized(&self) -> bool {
        true
    }
}

/// `createMcpAuthProvider(options)`。
///
/// 新建一个空的刷新槽；同一个 provider 的 clone 共享它（single-flight 的前提）。
pub fn create_mcp_auth_provider(options: McpAuthProviderOptions) -> McpAuthProvider {
    McpAuthProvider {
        server_url: options.server_url,
        store: options.store,
        settings: options.settings,
        on_challenge: options.on_challenge,
        refreshing: Arc::new(tokio::sync::Mutex::new(None)),
    }
}

/// 展示授权 URL 并（在无法回调时）询问粘贴的重定向 URL。
///
/// TS: `oauth.ts#McpSignInPrompt`。
///
/// 实现被 `Arc` 共享，跨 `await` 使用，因此要求 `Send + Sync`。契约：
/// - `show_authorization_url` 同步返回，实现可自行 `spawn` 打开浏览器的任务；
/// - `prompt_for_redirect_url` 在回调到达或登录被取消时必须尽快返回；返回 `None`/空白串会被
///   映射成 [`McpSignInError::Cancelled`]，因此“登录超时”最终也表现为取消。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait McpSignInPrompt: Send + Sync {
    /// 把授权 URL 展示给用户并在浏览器中打开。
    fn show_authorization_url(&self, url: &Url);

    /// 请用户从浏览器地址栏粘贴重定向 URL（浏览器访问不到回环回调时）。
    ///
    /// 回调到达或登录被取消时 `signal` 被取消。用户取消时返回 `None` 或空串。
    async fn prompt_for_redirect_url(&self, signal: CancellationToken) -> Option<String>;
}

/// `McpSignInCancelledError`：`new Error("Sign-in cancelled")`，名字逐字。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSignInCancelledError;

impl std::fmt::Display for McpSignInCancelledError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Sign-in cancelled")
    }
}

impl std::error::Error for McpSignInCancelledError {}

/// `signInMcpServer` 的错误：取消，或其它 OAuth 错误。
///
/// `Display` 与 [`Self::message`] 都逐字等于 TS 的 `error.message`；[`Self::name`] 返回 TS 的
/// 类名（取消时 `McpSignInCancelledError`，其余取 `OAuthFailure` 的 `name()`），调用方据此做
/// 与 TS `instanceof` 等价的判断。
#[derive(Debug, Clone)]
pub enum McpSignInError {
    /// 用户取消或超时（TS 的 `instanceof McpSignInCancelledError`）。
    Cancelled(McpSignInCancelledError),
    /// 其余错误（`OAuthFailure`，含授权服务器返回的、issuer 不匹配等）。
    Failure(OAuthFailure),
}

impl std::fmt::Display for McpSignInError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpSignInError::Cancelled(_) => formatter.write_str("Sign-in cancelled"),
            McpSignInError::Failure(error) => formatter.write_str(&error.message()),
        }
    }
}

impl std::error::Error for McpSignInError {}

impl From<OAuthFailure> for McpSignInError {
    fn from(error: OAuthFailure) -> Self {
        McpSignInError::Failure(error)
    }
}

impl McpSignInError {
    /// `error.message`（逐字）。
    pub fn message(&self) -> String {
        match self {
            McpSignInError::Cancelled(_) => "Sign-in cancelled".to_string(),
            McpSignInError::Failure(error) => error.message(),
        }
    }

    /// `error.name`（`McpSignInCancelledError` 或 OAuth 错误类名）。
    pub fn name(&self) -> &'static str {
        match self {
            McpSignInError::Cancelled(_) => "McpSignInCancelledError",
            McpSignInError::Failure(error) => error.name(),
        }
    }
}

/// 授权响应里的 `code`/`iss`。
struct AuthorizationResponse {
    code: String,
    iss: Option<String>,
}

/// `responseFromRedirectUrl(input, state, redirectUrl)`。
///
/// 校验顺序（与错误文案一一对应）：
/// 1. `input` 必须是完整 URL，否则 `"Expected the full redirect URL from the browser address bar"`；
/// 2. `origin` 与 `path` 必须与本次登录的 redirect URI 一致（端口属于 origin 的一部分；
///    query 不参与比较），否则 `"The redirect URL does not match this sign-in's redirect URI"`；
/// 3. 服务器返回了 `error` 时优先报它（描述取 `error_description`）；
/// 4. `state` 必须等于本次登录的 state，否则 `"The redirect URL belongs to a different sign-in"`；
/// 5. 最后要求非空 `code`。
///
/// 返回 `code` 与可选的 `iss`（RFC 9207）；`iss` 留给 `authorize_mcp` 做 issuer 校验。
fn response_from_redirect_url(
    input: &str,
    state: &str,
    redirect_url: &Url,
) -> Result<AuthorizationResponse, McpSignInError> {
    let url = Url::parse(kiana_json::js_string::js_trim(input)).map_err(|_| {
        OAuthFailure::error("Expected the full redirect URL from the browser address bar")
    })?;
    // 服务器专属的 redirect URI 用来区分授权服务器，因此必须精确匹配。
    // 先比 origin/path 再读参数：别的服务器/别的登录发到同一回调路径的响应会在这里被拒。
    if url.origin() != redirect_url.origin() || url.path() != redirect_url.path() {
        return Err(OAuthFailure::error(
            "The redirect URL does not match this sign-in's redirect URI",
        )
        .into());
    }
    let parameter = |name: &str| -> Option<String> {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    if let Some(error) = parameter("error").filter(|error| !error.is_empty()) {
        return Err(OAuthFailure::error(parameter("error_description").unwrap_or(error)).into());
    }
    if parameter("state").as_deref() != Some(state) {
        return Err(OAuthFailure::error("The redirect URL belongs to a different sign-in").into());
    }
    let code = parameter("code").filter(|code| !code.is_empty());
    let Some(code) = code else {
        return Err(
            OAuthFailure::error("The redirect URL does not contain an authorization code").into(),
        );
    };
    Ok(AuthorizationResponse {
        code,
        iss: parameter("iss"),
    })
}

/// 等浏览器回调或粘贴的 URL，谁先到用谁。`signal` 取消时取消提问，从而取消登录。
///
/// 两个来源在 `tokio::select!` 里竞争：浏览器回调用 `OAuthCallbackServer::wait_for_callback`
/// 注册（路径限定为本次 redirect URI 的 path），粘贴路径经 [`response_from_redirect_url`]
/// 校验。先完成的分支获胜，另一个 future 被丢弃。
///
/// 取消只能通过提问侧传递：`signal` 与 `controller` 合成 `prompt_signal`，提问实现看到取消
/// 后返回 `None`，从而得到 `Cancelled`。回调 future 没有取消通道，丢弃它**不会**注销
/// `state`（条目会留到超时或 `close()`），因此调用方必须在流程结束后关闭回调服务器——
/// [`sign_in_mcp_server`] 的 finally 等价逻辑就是做这件事。
async fn wait_for_authorization_response(
    callback: &OAuthCallbackServer,
    state: &str,
    redirect_url: &Url,
    prompt: &dyn McpSignInPrompt,
    signal: Option<&CancellationToken>,
) -> Result<AuthorizationResponse, McpSignInError> {
    let controller = CancellationToken::new();
    let prompt_signal = CancellationToken::new();
    // 注册浏览器回调，并限定它必须落在本次 redirect URI 的 path 上（CIMD 可能换成
    // `/callback/<id>`，所以不能写死）；落在别的路径的响应会被回调服务器以错误拒绝。
    let from_browser =
        callback.wait_for_callback(state.to_string(), Some(redirect_url.path().to_string()));
    let from_browser = match from_browser {
        Ok(future) => future,
        Err(error) => return Err(McpSignInError::Failure(error)),
    };
    // `AbortSignal.any([controller.signal, signal])`：任一来源触发都取消提问。
    {
        let prompt_signal = prompt_signal.clone();
        let controller = controller.clone();
        let user = signal.cloned();
        tokio::spawn(async move {
            match user {
                Some(user) => {
                    tokio::select! {
                        () = controller.cancelled() => prompt_signal.cancel(),
                        () = user.cancelled() => prompt_signal.cancel(),
                    }
                }
                None => controller.cancelled().await,
            }
            prompt_signal.cancel();
        });
    }
    let state_owned = state.to_string();
    let redirect_owned = redirect_url.clone();
    let from_user = async {
        let input = prompt.prompt_for_redirect_url(prompt_signal.clone()).await;
        match input {
            Some(input) if !kiana_json::js_string::js_trim(&input).is_empty() => {
                response_from_redirect_url(&input, &state_owned, &redirect_owned)
            }
            _ => Err(McpSignInError::Cancelled(McpSignInCancelledError)),
        }
    };
    let mut browser = Box::pin(from_browser);
    let mut user = Box::pin(from_user);
    // 两个 future 竞争；粘贴路径的空输入会在 from_user 内部变成 Cancelled。
    let result = tokio::select! {
        result = &mut browser => match result {
            Ok(response) => Ok(AuthorizationResponse { code: response.code, iss: response.iss }),
            Err(error) => Err(McpSignInError::Failure(error)),
        },
        result = &mut user => result,
    };
    // 输的一方被丢弃：回调服务器 close 或提问被取消时结束。
    controller.cancel();
    result
}

/// 在 `port` 监听，端口被占用且不是必需时换一个空闲端口。
///
/// `port` 是首选端口，`None`（或 `Some(0)`）表示直接让系统分配；`required` 表示调用方在
/// 配置里写死了端口，此时绑定失败必须上抛——悄悄换端口会让已注册的 redirect URI 失配。
/// `extra_paths` 是额外接收回调的路径（CIMD 的服务器专属 `/callback/<id>`）。
///
/// 第一次失败且允许回退时，第二次用 `port: None`（系统分配）重试；第二次的错误同样以
/// [`McpSignInError::Failure`] 返回。
async fn listen_for_callback(
    settings: &CallbackSettings,
    extra_paths: Vec<String>,
    port: Option<u16>,
    required: bool,
) -> Result<OAuthCallbackServer, McpSignInError> {
    // 回调页面的成功/失败文案由 kiana-ai 的共享 HTML 模板提供。
    let render_page: kiana_mcp::oauth::callback::RenderPageFn =
        Arc::new(|page: &OAuthCallbackPage| {
            if page.is_ok() {
                oauth_success_html("Signed in to the MCP server. You may now close this page.")
            } else {
                oauth_error_html(page.message().unwrap_or_default(), page.details())
            }
        });
    let options = OAuthCallbackServerOptions {
        host: Some(settings.host.clone()),
        redirect_host: Some(settings.redirect_host.clone()),
        port: port.or(Some(0)),
        path: Some(settings.path.clone()),
        extra_paths: extra_paths.clone(),
        timeout_ms: None,
        render_page: Some(Arc::clone(&render_page)),
    };
    match OAuthCallbackServer::listen(options).await {
        Ok(server) => Ok(server),
        Err(error) => {
            if required || port.is_none() {
                // 固定端口失败，或本来就让系统分配：没有可回退的空间。
                return Err(McpSignInError::Failure(error));
            }
            let options = OAuthCallbackServerOptions {
                host: Some(settings.host.clone()),
                redirect_host: Some(settings.redirect_host.clone()),
                port: None,
                path: Some(settings.path.clone()),
                extra_paths,
                timeout_ms: None,
                render_page: Some(render_page),
            };
            OAuthCallbackServer::listen(options)
                .await
                .map_err(McpSignInError::Failure)
        }
    }
}

/// `signInMcpServer` 的参数。
///
/// `challenge` 来自连接时缓存的 `WWW-Authenticate`（决定 `resource_metadata` 与 scope；
/// `error == "insufficient_scope"` 时本次是“加 scope”而不是首次登录）。`signal` 触发后整个
/// 登录尽快以 [`McpSignInError::Cancelled`] 结束，超时也走这条路径。
pub struct McpSignInOptions {
    /// MCP 服务器 URL。
    pub server_url: String,
    /// 这个服务器的状态存储。
    pub store: Arc<dyn McpOAuthStateStore>,
    /// OAuth 设置。
    pub settings: McpOAuthSettings,
    /// 服务器上一次的 OAuth 挑战（`resource_metadata`/scope）。
    pub challenge: Option<OAuthChallenge>,
    /// 浏览器/终端交互。
    pub prompt: Arc<dyn McpSignInPrompt>,
    /// 取消整个登录。
    pub signal: Option<CancellationToken>,
}

/// 登录一个 MCP 服务器。可能时用存储的 refresh token，否则跑浏览器授权码流程。
///
/// TS: `oauth.ts#signInMcpServer`。
///
/// 流程：
/// 1. 已取消 → 立即 `Cancelled`；
/// 2. 读存储与设置，算出回调参数（配置端口优先，其次复用已注册 redirect URI 的端口）；
/// 3. 监听回环回调（配置了固定端口时绑定失败直接报错，否则可回退到系统分配端口）；
/// 4. `run_sign_in_flow`：清理过期状态 → 第一次 `authorize_mcp`（可能直接用 refresh token
///    完成）→ 需要时展示 URL 并等待回调/粘贴的地址 → 用 `code` 换令牌；
/// 5. 无论成功失败都 `close()` 回调服务器，关闭错误被忽略（`finally` 等价）。
///
/// 取消优先级：流程返回错误时，如果 `signal` 已被取消，统一改报 `Cancelled`——超时/用户
/// 中断不应表现为 `"OAuth callback server closed"` 之类的次要错误。
pub async fn sign_in_mcp_server(options: McpSignInOptions) -> Result<(), McpSignInError> {
    let McpSignInOptions {
        server_url,
        store,
        settings,
        challenge,
        prompt,
        signal,
    } = options;
    if signal.as_ref().is_some_and(CancellationToken::is_cancelled) {
        return Err(McpSignInError::Cancelled(McpSignInCancelledError));
    }
    // 先读已存状态：登录会复用客户端注册与令牌，并据此决定是否保留它们。
    let stored = store.load().await.map_err(McpSignInError::Failure)?;
    // 挑战里的 error 是 insufficient_scope ⇒ 这次是“补 scope”，不是首次登录。
    let step_up = challenge
        .as_ref()
        .and_then(|challenge| challenge.error.as_deref())
        == Some("insufficient_scope");
    let callback_options = callback_settings(&settings).map_err(McpSignInError::Failure)?;
    // 复用已注册 redirect URI 的端口，让注册过的 client 继续有效。
    let registered = registered_redirect_urls(
        stored
            .as_ref()
            .and_then(|state| state.client_information.as_ref()),
    );
    let preferred_port = callback_options.port.or_else(|| {
        registered
            .first()
            .and_then(|url| Url::parse(url).ok())
            .and_then(|url| url.port())
    });
    let cimd = settings.client_registration == Some(McpClientRegistration::Cimd);
    // CIMD 可能把 redirect_uri 指到服务器专属的 `/callback/<id>`：即使本次用的是共享路径，
    // 回调服务器也要接收这个路径。
    let extra_paths = if cimd {
        vec![format!(
            "{CALLBACK_PATH}/{}",
            callback_id(&server_url).map_err(McpSignInError::Failure)?
        )]
    } else {
        Vec::new()
    };
    // 固定端口（配置里给了）失败时必须抛错。
    let callback = listen_for_callback(
        &callback_options,
        extra_paths,
        preferred_port,
        callback_options.port.is_some(),
    )
    .await?;
    let redirect_url = callback_options
        .fixed_redirect_url
        .clone()
        .unwrap_or_else(|| callback.redirect_url().to_string());
    let outcome = run_sign_in_flow(
        &server_url,
        Arc::clone(&store),
        &settings,
        challenge.as_ref(),
        step_up,
        stored,
        &redirect_url,
        &callback,
        prompt.as_ref(),
        signal.as_ref(),
    )
    .await;
    let result = match outcome {
        Err(error) => {
            // 被取消的请求以 signal 的原因为失败；统一报成取消。
            if signal.as_ref().is_some_and(CancellationToken::is_cancelled) {
                Err(McpSignInError::Cancelled(McpSignInCancelledError))
            } else {
                Err(error)
            }
        }
        Ok(()) => Ok(()),
    };
    // finally 等价：无论结果如何都关闭回调服务器；关闭错误不覆盖登录结果。
    let _ = callback.close().await;
    result
}

/// `signInMcpServer` 的 try 块（`finally` 里的 `callback.close()` 由调用方执行）。
///
/// 阶段：
/// 1. **清理旧状态**：每次登录换新的 `state`；若已有的 client 注册不能用于本次 redirect
///    URI，就丢掉 `clientInformation`/`tokens`/`tokensExpireAt`，迫使重新注册或授权；
/// 2. **第一次 authorize_mcp**：`skip_refresh = step_up`。非 step-up 时若存储的 refresh
///    token 仍有效会直接完成，不需要打开浏览器；
/// 3. **交互**：只有返回 `Redirect` 才取授权 URL；`provider.state()` 取本次 `state`，
///    redirect URI 取授权 URL 里实际的 `redirect_uri`（CIMD 可能换成服务器专属路径），
///    然后展示 URL 并等待回调或粘贴；
/// 4. **第二次 authorize_mcp**：带 `authorization_code` 与 `iss` 完成换令牌。
///
/// 若流程声称 `Redirect` 却没有通过 `on_redirect` 交出 URL，视为错误。
#[allow(clippy::too_many_arguments)]
async fn run_sign_in_flow(
    server_url: &str,
    store: Arc<dyn McpOAuthStateStore>,
    settings: &McpOAuthSettings,
    challenge: Option<&OAuthChallenge>,
    step_up: bool,
    stored: Option<McpOAuthState>,
    redirect_url: &str,
    callback: &OAuthCallbackServer,
    prompt: &dyn McpSignInPrompt,
    signal: Option<&CancellationToken>,
) -> Result<(), McpSignInError> {
    let cimd = settings.client_registration == Some(McpClientRegistration::Cimd);
    if let Some(stored) = &stored {
        let mut next = stored.clone();
        // 每次登录都有新的 `state` 参数。
        delete_state_key(&mut next, "oauthState");
        // 注册过的 client 不能用别的 redirect URI，它的令牌也只属于它；CIMD 不落盘 client，
        // 因此有 clientInformation 说明是以前 DCR 注册的，需要替换。
        // DCR 分支还要求已注册的 redirect URI 包含本次要用的那个；否则视为换了注册。
        let keep_client = settings
            .client_id
            .as_deref()
            .is_some_and(|client_id| !client_id.is_empty())
            || if cimd {
                stored.client_information.is_none()
            } else {
                registered_redirect_urls(stored.client_information.as_ref())
                    .iter()
                    .any(|url| url == redirect_url)
            };
        if !keep_client {
            delete_state_key(&mut next, "clientInformation");
            delete_state_key(&mut next, "tokens");
            delete_state_key(&mut next, "tokensExpireAt");
        }
        // 先落盘再跑流程：之后的 provider 读到的是清理后的状态。
        store.save(next).await.map_err(McpSignInError::Failure)?;
    }

    // provider 在需要用户授权时回调这里；我们只记录 URL，稍后交给 prompt 展示。
    let authorization_url: Arc<std::sync::Mutex<Option<Url>>> =
        Arc::new(std::sync::Mutex::new(None));
    let slot = Arc::clone(&authorization_url);
    let on_redirect: kiana_mcp::oauth::provider::OnRedirectFn = Arc::new(move |url: Url| {
        *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(url);
        Box::pin(async { Ok(()) }) as BoxFuture<'static, Result<(), OAuthFailure>>
    });
    let provider = create_provider(
        server_url,
        Arc::clone(&store),
        settings,
        redirect_url,
        on_redirect,
    )
    .map_err(McpSignInError::Failure)?;
    let flow = OAuthFlowOptions {
        server_url: server_url.to_string(),
        fetch: Some(timed_fetch(default_fetch())),
        signal: signal.cloned(),
        resource_metadata_url: challenge
            .and_then(|challenge| challenge.resource_metadata_url.clone()),
        authorization_server_metadata_url: settings.auth_server_metadata_url.clone(),
        // 要更多 scope 的服务器拿到的是配置 scope 与已授予 scope 的并集。
        // step-up 时先把“已授予”与“挑战要求”合并，再与配置的 scope 取并集。
        scope: {
            let challenged_scope = if step_up {
                step_up_scope(
                    stored
                        .as_ref()
                        .and_then(|state| state.tokens.as_ref())
                        .and_then(|tokens| tokens.string("scope")),
                    challenge.and_then(|challenge| challenge.scope.as_deref()),
                )
            } else {
                challenge.and_then(|challenge| challenge.scope.clone())
            };
            merge_scopes(&[settings.scope.as_deref(), challenged_scope.as_deref()])
        },
        // 刷新保留已授予的 scope；要更多 scope 需要浏览器流程。
        skip_refresh: step_up,
        ..OAuthFlowOptions::default()
    };
    // 非 step-up 时 provider 会先尝试用存储的 refresh token 直接换到令牌；成功就无需交互。
    if authorize_mcp(&provider, &flow)
        .await
        .map_err(McpSignInError::Failure)?
        == OAuthFlowResult::Authorized
    {
        return Ok(());
    }
    // 走到这里说明流程要求用户授权；没有 onRedirect 记录的 URL 就是实现缺陷。
    let authorization_url = authorization_url
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .ok_or_else(|| {
            McpSignInError::Failure(OAuthFailure::error(
                "OAuth flow did not produce an authorization URL",
            ))
        })?;
    // state 必须与授权 URL 里的相同（粘贴回来的地址要带回它），否则 response 校验会失败。
    let state = provider.state().await.map_err(McpSignInError::Failure)?;
    // 流程选择的 redirect URI 可能是服务器专属的。
    let authorization_redirect_url = authorization_url
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .map(|(_, value)| value.into_owned())
        .unwrap_or_else(|| redirect_url.to_string());
    let authorization_redirect_url = Url::parse(&authorization_redirect_url)
        .map_err(|_| McpSignInError::Failure(invalid_url()))?;
    // 展示与等待都用授权 URL 里实际的 redirect URI：CIMD 可能把它换成服务器专属路径。
    prompt.show_authorization_url(&authorization_url);
    let response = wait_for_authorization_response(
        callback,
        &state,
        &authorization_redirect_url,
        prompt,
        signal,
    )
    .await?;
    // 第二次调用：带 code 完成兑换；`iss` 交给 authorize_mcp 做 RFC 9207 的 issuer 校验。
    authorize_mcp(
        &provider,
        &OAuthFlowOptions {
            authorization_code: Some(response.code),
            iss: response.iss,
            ..flow
        },
    )
    .await
    .map_err(McpSignInError::Failure)?;
    Ok(())
}

/// `delete next[key]`（含显式 `null` 标记与键序记录）。
///
/// 三处必须同步清理，否则写回的形状会错：① 字段本体；② `null_keys`（否则序列化会再写出
/// `"key": null`）；③ `key_order`（否则键序表还记着已删除的键）。`discovery` 要连
/// `discovery_raw` 一起清（原始 JSON 是它的序列化数据源）；未知键从 `extra` 删除。
fn delete_state_key(state: &mut McpOAuthState, key: &str) {
    match key {
        "clientInformation" => state.client_information = None,
        "tokens" => state.tokens = None,
        "tokensExpireAt" => state.tokens_expire_at = None,
        "codeVerifier" => state.code_verifier = None,
        "oauthState" => state.oauth_state = None,
        "discovery" => {
            state.discovery = None;
            state.discovery_raw = None;
        }
        _ => {
            state.extra.shift_remove(key);
        }
    }
    state.null_keys.retain(|existing| existing != key);
    state.key_order.retain(|existing| existing != key);
}
