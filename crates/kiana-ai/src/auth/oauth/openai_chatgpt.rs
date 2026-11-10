//! auth/oauth/openai-chatgpt.ts——"Sign in with ChatGPT"（OpenAI Responses token sharing）。
//!
//! 公开客户端流程：每次登录都以 `dynamic_agent_client` 注册一个新 client，OpenAI 在回调里返回
//! 签发的 client id；凭证保存 `clientId` 与 `scopes`。
//!
//! 本文件**自带**一套回调服务器（不复用 `callback-server.ts`），与通用实现的三处差异必须保留
//! （spec/ai-auth.md §2.12）：
//! - 不检查 HTTP 方法（只看 pathname）；
//! - 回调解析失败时只回 400 页面，`result` 保持 pending（只有 `error` 查询参数会 reject）；
//! - 监听失败（EADDRINUSE）转成明确的端口占用文案，不换端口。
//!
//! TS: `src/auth/oauth/openai-chatgpt.ts`
//!
//! # 为什么需要它
//!
//! OpenAI 的「用 ChatGPT 订阅登录」不绑定固定的 OAuth client：每次登录都以
//! `client_id=dynamic_agent_client` 向授权端点发起**动态注册**，服务端在回调的
//! `client_id` 查询参数里返回本次签发的 client id。因此凭证必须保存这个签发的
//! `clientId`（refresh 时要用）和授予的 `scopes`；`DYNAMIC_CLIENT_ID` 只是注册占位值。
//!
//! 回调服务器默认监听 `127.0.0.1:1455` 的 `/auth/callback`（host 可被
//! `KIANA_OAUTH_CALLBACK_HOST` 覆盖）：授权请求里的 `redirect_uri`
//! 必须与兑换 token 时提交的完全一致，固定值让两处天然相同。代价是同一主机同一时刻只能
//! 有一个 ChatGPT 登录占用这个端口；被另一个 kiana 登录或 Codex CLI 占住时直接报错，
//! 不能「换个端口再试」——浏览器手里的回调地址仍然是 1455。
//!
//! # 在系统中的位置
//!
//! `providers/openai.rs` 通过 `auth::helpers::lazy_oauth` 登记本流程，
//! `auth/oauth/load.rs` 的 `OPENAI_CHATGPT` 单例也返回它；`Models` 通过
//! [`OAuthAuth`] 的 `login`/`refresh`/`to_auth` 调用本文件。
//! `to_auth` 只把 access token 放进 `ModelAuth::api_key`，base URL 与 headers 由 provider
//! 侧提供。本模块不依赖任何上层（agent/session 等），只用 `auth`、`utils` 与
//! `auth::oauth` 的共享工具。
//!
//! # 一次登录的执行流程
//!
//! 1. 取 `LoginOptions::get_device_id`，把 UUID 规范成 `urn:uuid:<小写>`；缺失或非法时在
//!    任何监听/网络动作之前失败（对应 TS 在创建服务器之前求值）。
//! 2. `generate_pkce()` 生成 verifier/challenge；`random_value()` 生成 `state` 与 `nonce`。
//! 3. 启动回调服务器；绑定失败时把 `EADDRINUSE` 换成固定的端口占用文案。
//! 4. 拼授权 URL 并 `notify(AuthEvent::AuthUrl)`，参数顺序与 spec/ai-auth.md §2.12 一致。
//! 5. 让「浏览器回调」与「用户粘贴回调 URL」两个 future 竞争，先完成者生效。
//! 6. 用 `code` + `verifier` + 回调里签发的 `client_id` 兑换 token，校验 `id_token` 与
//!    `chatgpt.tokens.use.direct` scope 后生成 `OAuthCredential`。
//! 7. 无论成败都 abort 手动输入的取消信号并关闭服务器（含强制断开所有连接）。
//!
//! # 主要数据结构
//!
//! - `AuthorizationResult`：一次回调解析出的 `code` 与签发的 `client_id`。
//! - `ServerShared`：首个 settle 的结果 + `Notify` + 两个 `CancellationToken`（停止
//!   accept / 断开所有连接）；多个连接任务并发写入，`Mutex` 只保护那个结果槽。
//! - `ChatGptCallbackServer`：监听句柄。`wait()` 取结果，`close()` 停止接受新连接，
//!   `close_all_connections()` 额外断开已建立的连接。**没有 `Drop` 实现**，不显式关闭时
//!   accept 任务与监听 socket 会活到进程结束。
//!
//! # 边界与失败模式
//!
//! - 回调解析失败只给浏览器 400 页面、**不** settle 结果：登录继续挂起，只能靠手动粘贴或
//!   abort 结束（与 TS 的 `result` promise 保持 pending 一致）。
//! - URL 带非空 `error` 查询参数时会 settle Err 并回 400 失败页。
//! - accept 循环出现 server 级错误时同样 settle Err，但不显示页面（请求根本没有到达处理逻辑）。
//! - 监听固定端口，因此并发登录不可用；测试（`tests/openai_chatgpt_oauth.rs`）用 `SERIAL`
//!   互斥锁串行化占用 1455 的用例。
//! - token 响应必须是 JSON 对象，`access_token`/`refresh_token`/`scope` 为非空白字符串，
//!   `expires_in` 为有限正数；refresh 响应必须包含非空 `refresh_token`，客户端不校验
//!   它是否与旧值不同（OpenAI 侧通常轮换）。
//! - 登录没有超时：用户不操作也不 abort 时会一直等待。
//!
//! # 注意事项
//!
//! - 所有字符串文案、表单字段顺序、授权 URL 参数顺序都按 spec/ai-auth.md §2.12 逐字对齐，
//!   改动会破坏与 TS 的差分测试，不是「顺手美化」的对象。
//! - `REDIRECT_URI` 是字面量 `127.0.0.1`；`KIANA_OAUTH_CALLBACK_HOST` 只影响监听地址
//!   （`oauth_callback_host_at_init()`），不会改变授权/兑换/粘贴校验里使用的回调地址。

use std::sync::{Arc, LazyLock, Mutex};

use regex::Regex;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::auth::oauth::device_code::{AuthClock, SystemAuthClock};
use crate::auth::oauth::pkce::{base64url_encode, generate_pkce};
use crate::auth::oauth::{
    OAuthFetch, OAuthFetchRequest, default_oauth_fetch, form_urlencode, js_trim,
    oauth_callback_host_at_init, query_param, resolve_url, split_js_whitespace, url_with_search,
};
use crate::auth::types::{
    AuthError, AuthEvent, AuthInteraction, AuthPrompt, AuthPromptRequest, LoginOptions, ModelAuth,
    OAuthAuth, OAuthCredential,
};
use crate::utils::abort::AbortScope;
use crate::utils::abort_signals::combine_abort_signals;
use crate::utils::oauth_page::{oauth_error_html, oauth_success_html};

/// `DYNAMIC_CLIENT_ID`。
///
/// 动态注册时提交的占位 client id；真正签发的 id 由回调的 `client_id` 参数带回，并写入
/// 凭证的 `clientId` 字段供 refresh 使用。它**不能**直接用于 token 兑换。
const DYNAMIC_CLIENT_ID: &str = "dynamic_agent_client";
/// `AGENT_NAME_HINT`。
///
/// 授权 URL 中 `agent_name_hint` 的默认值；`LoginOptions::agent_name` 为 `Some` 时被覆盖。
const AGENT_NAME_HINT: &str = "Kiana";
/// `AUTHORIZE_URL`。
///
/// 浏览器授权端点；只在 `login_openai_chatgpt` 里拼授权 URL 时使用。
const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
/// `TOKEN_URL`。
///
/// 授权码兑换与 refresh 共用的 token 端点。
const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
/// `RESOURCE`。
///
/// RFC 8707 的 resource indicator；授权请求、兑换请求、refresh 请求三处都带同一个值。
const RESOURCE: &str = "https://api.openai.com/v1";
/// `CALLBACK_PORT`。
///
/// 固定端口而不是随机空闲端口：`REDIRECT_URI` 里写死了它。占用时只报错、不回退
/// （见 `login_openai_chatgpt`）。
const CALLBACK_PORT: u16 = 1455;
/// `CALLBACK_PATH`。
///
/// 回调服务器只处理这个 pathname，其余路径一律 404。
const CALLBACK_PATH: &str = "/auth/callback";
/// `REDIRECT_URI`。
///
/// 授权 URL、token 兑换表单、手动粘贴校验三处都使用这个字面量；即使
/// `KIANA_OAUTH_CALLBACK_HOST` 改了监听地址，这里仍是 `127.0.0.1`。
const REDIRECT_URI: &str = "http://127.0.0.1:1455/auth/callback";
/// `DIRECT_TOKEN_SCOPE`。
///
/// `credential_from_token_response` 只强制要求这一个 scope：缺失即报错，其余 scope 不校验。
const DIRECT_TOKEN_SCOPE: &str = "chatgpt.tokens.use.direct";
/// `SCOPE`。
///
/// 请求的完整 scope 串（空格分隔、顺序固定），最后一项就是 `DIRECT_TOKEN_SCOPE`。
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
/// `EXPIRY_MARGIN_MS`。
///
/// 计算 `expires` 时提前 3 分钟（`now + expires_in*1000 - EXPIRY_MARGIN_MS`），把提前量
/// 算进凭证的过期时间，而不是在发请求时另做判断。TS 同值。
const EXPIRY_MARGIN_MS: f64 = 3.0 * 60.0 * 1000.0;

/// `UUID_PATTERN`。
///
/// 大小写不敏感（`(?i)`）、锚定首尾的标准 8-4-4-4-12 十六进制形式。用于校验
/// `LoginOptions::get_device_id` 的返回值；`agent_host_id` 再把通过的 UUID 整体小写。
static UUID_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
        .expect("valid UUID pattern")
});

/// `AuthorizationResult`。
///
/// 一次成功回调的解析结果，两个字段都经过非空校验：`code` 用于兑换 token；`client_id`
/// 是本次登录签发的 OAuth client id（**不是** `DYNAMIC_CLIENT_ID`），既作为兑换请求的
/// `client_id`，也会写入凭证供后续 refresh。
#[derive(Debug, Clone, PartialEq, Eq)]
struct AuthorizationResult {
    code: String,
    client_id: String,
}

/// `randomValue()`：32 字节随机数的 base64url（43 字符，无填充）。
///
/// 对应 Node `randomBytes(32).toString("base64url")`，用于 `state` 与 `nonce`。
/// 取不到系统随机源时 `expect` 会 panic——调用链上没有把它设计成可恢复错误。
fn random_value() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("operating system random source");
    base64url_encode(&bytes)
}

/// `authorizationResultFromCallback(url, expectedState)`。
///
/// 校验顺序与错误文案是当前稳定契约（spec/ai-auth.md §2.12）：
/// 1. 缺 `code` → `Missing authorization code`；
/// 2. 缺 `state` → `Missing OAuth state`；
/// 3. `state` 不等于期望值 → `OAuth state mismatch`；
/// 4. `client_id` 缺失、或 JS `trim` 后为空 → 提示注册回调未携带签发的 client ID。
///
/// `code`/`state` 只判空、不做 trim（TS 写的是 `!value`）；`client_id` 返回 trim 后的值。
/// 同名查询参数取第一个（`URLSearchParams.get` 语义，见 `query_param`）。
fn authorization_result_from_callback(
    url: &Url,
    expected_state: &str,
) -> Result<AuthorizationResult, AuthError> {
    let Some(code) = query_param(url, "code").filter(|code| !code.is_empty()) else {
        return Err(AuthError::new("Missing authorization code"));
    };
    let Some(state) = query_param(url, "state").filter(|state| !state.is_empty()) else {
        return Err(AuthError::new("Missing OAuth state"));
    };
    if state != expected_state {
        return Err(AuthError::new("OAuth state mismatch"));
    }
    let client_id = query_param(url, "client_id").map(|client_id| js_trim(&client_id).to_string());
    let Some(client_id) = client_id.filter(|client_id| !client_id.is_empty()) else {
        return Err(AuthError::new(
            "OpenAI OAuth registration callback did not contain an issued client ID",
        ));
    };
    Ok(AuthorizationResult { code, client_id })
}

/// `new URL(input)` 的失败文案。
///
/// 用户粘贴的内容不是绝对 URL（或被 WHATWG 解析拒绝）时使用；`authorization_result_from_manual_input`
/// 里解析常量 `REDIRECT_URI` 失败也复用它，但那是编译期常量，实际不会发生。
fn invalid_pasted_url_error() -> AuthError {
    AuthError::new("Paste the full callback URL from the browser")
}

/// `authorizationResultFromManualInput(input, expectedState)`。
///
/// 手动粘贴路径：先 `js_trim` 再按 WHATWG 解析；只要求与 `REDIRECT_URI` **同源且同
/// pathname**（query 与 fragment 不参与比较）。错误文案说 "must start with"，实现其实是
/// 等值比较。
/// 带非空 `error` 参数时直接返回 `ChatGPT authorization failed: <error>`；否则复用
/// `authorization_result_from_callback` 做 code/state/client_id 校验。
///
/// 与浏览器回调路径的差异：粘贴内容完全不经过本地服务器，所以必须自己校验地址，否则
/// 用户可以把任意站点的 URL 当成回调交进来。
fn authorization_result_from_manual_input(
    input: &str,
    expected_state: &str,
) -> Result<AuthorizationResult, AuthError> {
    let url = Url::parse(js_trim(input)).map_err(|_| invalid_pasted_url_error())?;
    let expected = Url::parse(REDIRECT_URI).map_err(|_| invalid_pasted_url_error())?;
    if !same_origin(&url, &expected) || url.path() != expected.path() {
        return Err(AuthError::new(format!(
            "The pasted callback URL must start with {REDIRECT_URI}"
        )));
    }
    if let Some(error) = query_param(&url, "error").filter(|error| !error.is_empty()) {
        return Err(AuthError::new(format!(
            "ChatGPT authorization failed: {error}"
        )));
    }
    authorization_result_from_callback(&url, expected_state)
}

/// `url.origin === expected.origin`。
///
/// 逐项比较 scheme、host 与端口；`port_or_known_default` 让显式的 `:80`/`:443` 与缺省
/// 端口视为相同（WHATWG origin 语义）。host 比较区分大小写，但 `Url` 已把 host 规范化为
/// 小写，因此直接字符串比较即可。
fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

// ---------------------------------------------------------------------------
// 自带回调服务器
// ---------------------------------------------------------------------------
//
// 与 callback_server.rs 的通用实现相比，这里没有 claimed/settled 状态机、没有超时、
// 也没有 abort 监听：请求只要 pathname 匹配就处理，首个 settle 的结果生效。三处刻意的
// 行为差异（不检查 method、解析失败不 settle、监听失败不换端口）见文件头说明。

/// 共享结果（首个 settle 生效，之后的结果被忽略，与 Promise 语义一致）。
///
/// 用 `Mutex<Option<..>>` 而不是 oneshot 通道：结果可能被并发的连接任务同时写入，而
/// `wait()` 需要「先检查槽位、再等待唤醒」且不能丢唤醒。锁只保护这个 `Option`，临界区
/// 内没有 `.await`；中毒时用 `into_inner()` 继续（被保护的数据不可能停在半更新状态）。
struct ServerShared {
    /// 首个 settle 的结果；被 `take_result` 取走后回到 `None`。
    result: Mutex<Option<Result<AuthorizationResult, AuthError>>>,
    /// 结果写入后唤醒 `wait()`；配合 `enable()` 解决「检查与注册之间」的竞态。
    notify: Notify,
    /// `server.close()`：停止接受新连接。
    shutdown: CancellationToken,
    /// `server.closeAllConnections()`：断开**所有**已建立连接（含浏览器预开的空闲连接）。
    /// 每个连接任务都在 `select!` 中监听它，取消即 drop handler 并关闭 socket。
    connections: CancellationToken,
}

impl ServerShared {
    fn new() -> Self {
        ServerShared {
            result: Mutex::new(None),
            notify: Notify::new(),
            shutdown: CancellationToken::new(),
            connections: CancellationToken::new(),
        }
    }

    /// `settle(result)`：首个结果生效，后续调用被忽略。
    ///
    /// 先写槽位、释放锁，再 `notify_waiters()`，避免被唤醒的 waiter 立刻抢锁。
    /// `notify_waiters` 不为「当前没有 waiter」保存许可，因此 `wait()` 不能只依赖唤醒，
    /// 必须在等待前检查槽位（见 `ChatGptCallbackServer::wait`）。
    fn settle(&self, result: Result<AuthorizationResult, AuthError>) {
        {
            let mut slot = self
                .result
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if slot.is_some() {
                return;
            }
            *slot = Some(result);
        }
        self.notify.notify_waiters();
    }

    /// 取出结果（`Option::take`）。取走后槽位为空，因此本流程只允许一个 waiter；
    /// 后续到达的回调仍可重新填入，但不会再有人读取。
    fn take_result(&self) -> Option<Result<AuthorizationResult, AuthError>> {
        self.result
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }
}

/// Node `server.listen(port, host)` 的错误名（libuv `uv_strerror` 的常见条目）。
///
/// 只映射 Node/libuv 常见的几个 errno（TS 只对 `EADDRINUSE` 特判，其余原样上抛）；返回
/// `(code, text)` 用于拼 Node 的 `listen <code>: <text> <address>` 文案。未列出的 errno 由
/// 调用方退化成通用 `listen <io::Error>` 文案，此时不设置 `error_code`，登录流程也不会识别成
/// 端口占用。
fn listen_strerror(errno: i32) -> Option<(&'static str, &'static str)> {
    match errno {
        libc::EADDRINUSE => Some(("EADDRINUSE", "address already in use")),
        libc::EACCES => Some(("EACCES", "permission denied")),
        libc::EADDRNOTAVAIL => Some(("EADDRNOTAVAIL", "address not available")),
        libc::EAFNOSUPPORT => Some(("EAFNOSUPPORT", "address family not supported")),
        libc::EINVAL => Some(("EINVAL", "invalid argument")),
        libc::ENOMEM => Some(("ENOMEM", "not enough memory")),
        _ => None,
    }
}

/// `listen <code>: <strerror> <host>:<port>`（Node 的监听错误文案）。
///
/// IPv6 地址按 Node 的习惯加方括号。只有 errno 能映射时才写 `error.code`：登录流程正是靠
/// `error_code == "EADDRINUSE"` 决定是否替换成固定的端口占用文案（`login_openai_chatgpt`），
/// 因此这里映射失败会让「端口被占用」退化成一条普通错误。
fn listen_error(host: &str, port: u16, error: &std::io::Error) -> AuthError {
    let address = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let Some(errno) = error.raw_os_error() else {
        return AuthError::new(format!("listen {error}"));
    };
    let Some((code, text)) = listen_strerror(errno) else {
        return AuthError::new(format!("listen {error}"));
    };
    let mut auth_error = AuthError::new(format!("listen {code}: {text} {address}"));
    // TS 的登录流程只按 `error.code === "EADDRINUSE"` 分支。
    auth_error.error_code = Some(code.to_string());
    auth_error
}

/// `getaddrinfo <code> <host>`（Node 的 dns.lookup 错误文案）。
///
/// `tokio::net::lookup_host` 的解析失败走 std 的 `ToSocketAddrs`，通常只暴露拼接好的
/// 错误文本（测试用 `std::io::Error::other` 构造样本），拿不到原始 EAI 码，因此这里按文本
/// 区分：含 `Temporary failure in name resolution` 的算 `EAI_AGAIN`（暂时性，可重试），
/// 其余一律 `ENOTFOUND`。这是对 libc 文案的依赖：非英文 locale 或不同实现可能落入
/// `ENOTFOUND` 分支；两种情况都只是错误文案，调用方都当失败处理。
fn getaddrinfo_error(host: &str, error: &std::io::Error) -> AuthError {
    let message = error.to_string();
    let code = if message.contains("Temporary failure in name resolution") {
        "EAI_AGAIN"
    } else {
        // Linux 的 "Name or service not known"、macOS 的 "nodename nor servname provided"
        // 以及其余解析失败在 Node 里都是 ENOTFOUND。
        "ENOTFOUND"
    };
    AuthError::new(format!("getaddrinfo {code} {host}"))
}

/// Node `server.listen(port, host)`：先 `dns.lookup` 取**第一个**地址，再 bind 该地址
/// （不做多地址回退）。
///
/// `lookup_host` 空结果按 `ENOTFOUND` 处理；bind 失败交给 `listen_error` 生成 Node 文案。
/// host 在默认路径上是字面量 `127.0.0.1`，解析只是顺带；只有
/// `KIANA_OAUTH_CALLBACK_HOST` 指向域名/IPv6 时才会真正走到 DNS。
async fn bind_listener(host: &str, port: u16) -> Result<TcpListener, AuthError> {
    let mut addresses = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| getaddrinfo_error(host, &error))?;
    let Some(address) = addresses.next() else {
        return Err(AuthError::new(format!("getaddrinfo ENOTFOUND {host}")));
    };
    TcpListener::bind(address)
        .await
        .map_err(|error| listen_error(host, port, &error))
}

/// chatgpt 流程的 loopback 服务器。
///
/// 与通用的 `callback_server::OAuthCallbackServer` 不同：没有 claim/settled 状态机、
/// 没有超时、没有 abort 监听，只做「解析回调 → 首个结果写入 `ServerShared`」。
/// 句柄**没有 `Drop`**：丢弃它不会停止 accept 循环，必须显式调用
/// `close`/`close_all_connections`（`login_openai_chatgpt` 在返回前无条件执行）。
struct ChatGptCallbackServer {
    shared: Arc<ServerShared>,
    accept_task: tokio::task::JoinHandle<()>,
}

impl ChatGptCallbackServer {
    /// `startCallbackServer(expectedState)`。
    ///
    /// host 来自 `oauth_callback_host_at_init()`（`KIANA_OAUTH_CALLBACK_HOST`，进程内首次
    /// 求值后固定），端口固定为 `CALLBACK_PORT`。
    async fn start(expected_state: String) -> Result<Self, AuthError> {
        Self::start_on(oauth_callback_host_at_init(), CALLBACK_PORT, expected_state).await
    }

    /// 指定 host/port 的 [`Self::start`]（测试用 `port = 0` 取空闲端口）。
    ///
    /// 错误原样返回，是否改写成端口占用文案由 `login_openai_chatgpt` 决定。
    async fn start_on(host: &str, port: u16, expected_state: String) -> Result<Self, AuthError> {
        let listener = bind_listener(host, port).await?;
        Ok(Self::start_with_listener(listener, expected_state))
    }

    /// 已绑定监听器的启动路径（测试用：自行 `bind("127.0.0.1:0")` 取空闲端口）。
    ///
    /// accept 循环的行为：
    /// - `shutdown` 触发时退出循环（`close`/`close_all_connections`）；
    /// - 每个 accept 到的连接单独 spawn；任务内再 `select!` `connections` 与 handler，
    ///   取消时直接 drop handler，连带关闭 `TcpStream`——这是断开空闲连接的关键；
    /// - accept 本身出错时把错误 settle 成结果并睡 10ms 再继续，避免错误触发 busy-loop。
    ///   `settle` 首个生效，所以重复错误不会覆盖首个结果。
    fn start_with_listener(listener: TcpListener, expected_state: String) -> Self {
        let shared = Arc::new(ServerShared::new());
        let expected_state = Arc::new(expected_state);
        let accept_shared = Arc::clone(&shared);
        let accept_expected = Arc::clone(&expected_state);
        let shutdown = shared.shutdown.clone();
        let accept_task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => return,
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _peer)) => {
                            // `closeAllConnections()` 必须能断开已经 accept、尚未发请求的
                            // 空闲连接（浏览器会预开）；见 spec/ai-auth.md §2.12。
                            let connection_shared = Arc::clone(&accept_shared);
                            let connection_expected = Arc::clone(&accept_expected);
                            tokio::spawn(async move {
                                tokio::select! {
                                    _ = connection_shared.connections.cancelled() => {}
                                    _ = handle_connection(
                                        stream,
                                        Arc::clone(&connection_shared),
                                        connection_expected,
                                    ) => {}
                                }
                            });
                        }
                        Err(error) => {
                            // 监听成功后的 server 级错误 → rejectResult（Node 的 server.on("error")）。
                            accept_shared.settle(Err(AuthError::new(format!("listen {error}"))));
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        }
                    }
                }
            }
        });
        ChatGptCallbackServer {
            shared,
            accept_task,
        }
    }

    /// `callback.result`：等待第一个 settle 的结果。
    ///
    /// `Notify::notified()` 在首次 poll 时才注册；若写成「先检查槽位、再 await」，两次操作
    /// 之间到达的 settle 会永久丢失。这里先 `enable()` 注册、再 `take_result()`，覆盖两种
    /// 顺序：settle 在前 → 检查时拿到；settle 在后 → 唤醒已注册的 waiter。外层 `loop` 只
    /// 负责重新检查槽位（防御性写法）。结果取出后槽位为空，本流程只允许一个 waiter。
    async fn wait(&self) -> Result<AuthorizationResult, AuthError> {
        loop {
            // `Notified` 在首次 poll 时才注册；先 `enable()` 再检查，避免 settle 落在
            // 「检查之后、注册之前」导致永久挂起。
            let notified = self.shared.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(result) = self.shared.take_result() {
                return result;
            }
            notified.await;
        }
    }

    /// `server.close()`：停止接受新连接。
    ///
    /// 取消 `shutdown` 之外还 `abort` accept 任务：两者任一都足以让循环退出，重复调用无
    /// 副作用。已在处理的连接不受影响，那是 `close_all_connections()` 的职责。
    fn close(&self) {
        self.shared.shutdown.cancel();
        self.accept_task.abort();
    }

    /// `server.closeAllConnections()`：在 `close()` 之外强制断开所有已建立连接。
    ///
    /// 仅取消 accept 循环不够：浏览器预开的空闲连接仍挂在旧 server 的 handler 上，下一次登录
    /// 的回调可能沿这条连接送达并被旧 handler 以旧的 state 拒绝（spec §2.12）。
    ///
    /// 实现上再取消 `connections`：每个连接任务都 `select!` 在这个 token 上，取消即 drop
    /// `handle_connection` 并关闭 socket。`login_openai_chatgpt` 在返回前依次调用
    /// `close()` 与 `close_all_connections()`，与 TS 的 finally 顺序一致。
    fn close_all_connections(&self) {
        self.shared.shutdown.cancel();
        self.shared.connections.cancel();
        self.accept_task.abort();
    }
}

/// 一次处理的结果：状态码 + 页面。
///
/// 只服务于 handler 内部：`route` 是同步函数，先算出它，再由 `handle_connection` 异步写
/// 响应。这样「同步异常 → 500」与「写 socket 失败」两条错误路径彼此独立。
struct HandlerOutcome {
    status: u16,
    body: String,
}

fn page_outcome(status: u16, body: String) -> HandlerOutcome {
    HandlerOutcome { status, body }
}

/// `sendHtml` 的响应字节（`Content-Type: text/html; charset=utf-8`，无 cache-control）。
///
/// 手写 HTTP/1.1 响应：状态行 + 三个头 + 空行 + body。reason phrase 只覆盖本流程会用到的
/// 200/400/404，其余一律 `Internal Server Error`（与 `route` 的取值一致）。`Content-Length`
/// 按 UTF-8 字节数计算；`Connection: close` 表示每个连接只服务一个请求，写完即关。
///
/// 与 Node `sendHtml` 的差异：TS 不设 `Content-Length`（走分块传输），这里显式给出长度；
/// 测试契约只断言状态码与 `content-type`（spec/ai-auth.md §6.2 #20），不受影响。
fn html_response(status: u16, body: &str) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut bytes = Vec::with_capacity(head.len() + body.len());
    bytes.extend_from_slice(head.as_bytes());
    bytes.extend_from_slice(body.as_bytes());
    bytes
}

/// 从原始请求文本里取请求行第二个空白分隔 token（request-target）。
///
/// 不解析 HTTP 方法：这正是本流程「不检查方法」的实现方式——POST/PUT 只要 path 对也会被
/// 当作回调处理（spec §2.12）。格式不符时返回空串，随后 `resolve_url` 会把它解析成
/// `REDIRECT_URI` 本身（空相对引用按 WHATWG 规则继承 base，`url::Url::join("")` 即如此），
/// 于是进入 callback 路径并因缺参数返回 400；与 TS `new URL(request.url || "", REDIRECT_URI)`
/// 的结果一致。
fn request_target(raw: &str) -> String {
    raw.lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("")
        .to_string()
}

/// handler 主体（同步，对应 TS 的同步 `try/catch`）。
///
/// 处理顺序与 TS handler 一致（spec §2.12）：
/// 1. 把 request-target 相对 `REDIRECT_URI` 解析；失败 → 500（TS 里 `new URL` 抛异常）。
/// 2. pathname 不是 `CALLBACK_PATH` → 404。
/// 3. 带非空 `error` 参数 → `settle(Err)` 并回 400，页面上显示 `Error: <error>`。
/// 4. 解析 code/state/client_id 失败 → 400，但**不 settle**，结果保持 pending。
/// 5. 成功 → `settle(Ok)` 并回成功页。
///
/// 第 4 步是与通用回调服务器的关键差异：错误页只给浏览器看，不结束登录等待（用户仍可
/// 手动粘贴，或靠 abort 收场）。函数是同步的，`settle` 只短暂持锁，不跨 `await`。
fn route(target: &str, shared: &ServerShared, expected_state: &str) -> HandlerOutcome {
    // `new URL(request.url || "", REDIRECT_URI)`：解析失败在 TS 里抛出 → 500。
    let Ok(url) = resolve_url(REDIRECT_URI, target) else {
        return page_outcome(
            500,
            oauth_error_html("Internal error while processing the callback.", None),
        );
    };
    if url.path() != CALLBACK_PATH {
        return page_outcome(404, oauth_error_html("Callback route not found.", None));
    }

    if let Some(error) = query_param(&url, "error").filter(|error| !error.is_empty()) {
        shared.settle(Err(AuthError::new(format!(
            "ChatGPT authorization failed: {error}"
        ))));
        return page_outcome(
            400,
            oauth_error_html(
                "ChatGPT was not connected.",
                Some(&format!("Error: {error}")),
            ),
        );
    }

    let authorization_result = match authorization_result_from_callback(&url, expected_state) {
        Ok(result) => result,
        Err(error) => {
            // 解析失败只回 400：result promise 保持 pending（spec §2.12）。
            return page_outcome(400, oauth_error_html(&error.message, None));
        }
    };

    shared.settle(Ok(authorization_result));
    page_outcome(
        200,
        oauth_success_html("ChatGPT authentication completed. You can close this window."),
    )
}

/// 读取一个 HTTP 请求头（读到 `\r\n\r\n` 为止），返回解码后的文本。
///
/// - 读满空行即停；不读 body，也不支持 `Expect: 100-continue`（本服务器只关心请求行）；
/// - EOF、读错误、非法 UTF-8、或缓冲超过 64 KiB 都返回 `None`，调用方直接关连接、不回响应；
/// - **没有读超时**：空闲连接会让 handler 任务一直阻塞在这里，直到
///   `close_all_connections` 取消它——这正是必须调用 `closeAllConnections` 的原因。
async fn read_request(stream: &mut TcpStream) -> Option<String> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if buffer.len() > 64 * 1024 {
            return None;
        }
    }
    String::from_utf8(buffer).ok()
}

/// 一个连接的生命周期：读请求 → 同步路由 → 写响应 → flush → shutdown。
///
/// 写失败一律忽略（浏览器可能中途断开），且不影响已 settle 的结果；所有错误都表现为
/// 连接被关闭，没有日志或重试。
async fn handle_connection(
    mut stream: TcpStream,
    shared: Arc<ServerShared>,
    expected_state: Arc<String>,
) {
    let Some(raw) = read_request(&mut stream).await else {
        return;
    };
    let target = request_target(&raw);
    let outcome = route(&target, &shared, &expected_state);
    let _ = stream
        .write_all(&html_response(outcome.status, &outcome.body))
        .await;
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
}

// ---------------------------------------------------------------------------
// token 交换
// ---------------------------------------------------------------------------
//
// 兑换与刷新共用 requestToken + credentialFromTokenResponse；两者的区别只在表单字段
// （兑换带 code/verifier/redirect_uri，刷新带旧 refresh_token、不带 scope）以及兑换额外
// 要求 id_token 存在。所有字段顺序都按 spec/ai-auth.md §2.12 固定。

/// 流程的 token 响应字段（`TokenResponse`）。
///
/// TS 类型只声明已知字段，运行时 JSON 对象仍可带未知键；Rust 用 `Value` 保留整个对象，
/// 未知字段不会被丢弃；读取统一走 `object_field`（非对象返回 `None`，与 JS 属性访问一致）。
type TokenResponse = Value;

/// `requestToken(body, signal)`。
///
/// `body` 必须是已用 `form_urlencode` 编码的字符串（键顺序由调用方决定）。非 2xx 时拼出
/// `OpenAI OAuth token request failed (<status>): <响应体或 statusText>`；2xx 后按
/// `response.json()` 解析（失败原样返回 V8 文案的 `SyntaxError`），并要求顶层是对象，
/// 数组/标量一律拒绝。`OAuthFetchResponse` 已把响应体完整缓冲，`text()`/`json()` 可以
/// 任意重复调用，不会像流式响应那样第二次读空。
async fn request_token(
    fetch: &dyn OAuthFetch,
    body: String,
    signal: &AbortScope,
) -> Result<TokenResponse, AuthError> {
    let request = OAuthFetchRequest::new(TOKEN_URL, signal)
        .header("accept", "application/json")
        .header("content-type", "application/x-www-form-urlencoded")
        .post(body.into_bytes());
    let response = fetch.fetch(request).await?;
    if !response.ok() {
        let response_body = response.text();
        let detail = if response_body.is_empty() {
            response.status_text.clone()
        } else {
            response_body
        };
        return Err(AuthError::new(format!(
            "OpenAI OAuth token request failed ({}): {detail}",
            response.status
        )));
    }
    let data = response.json()?;
    if !matches!(data, Value::Object(_)) {
        return Err(AuthError::new(
            "OpenAI OAuth token response must be an object",
        ));
    }
    Ok(data)
}

/// `requireTokenString(value, field)`。
///
/// 只要求「是字符串且 JS `trim` 后非空」，返回**原始**字符串（保留两侧空白）；
/// `field` 由调用方给出（`access_token`/`refresh_token`/`scope`），直接进错误文案。
fn require_token_string(value: Option<&Value>, field: &str) -> Result<String, AuthError> {
    match value.and_then(Value::as_str) {
        Some(value) if !js_trim(value).is_empty() => Ok(value.to_string()),
        _ => Err(AuthError::new(format!(
            "OpenAI OAuth token response has invalid {field}"
        ))),
    }
}

/// `credentialFromTokenResponse(token, clientId)`。
///
/// 校验并构造凭证，顺序与 TS 相同：
/// 1. `access_token`/`refresh_token`/`scope` 必须是非空白字符串（原值保留，不 trim）；
/// 2. `expires_in` 必须是有限正数（JSON 整数也能被 `as_f64` 读到；非数字 → `None`）；
/// 3. scope 按 JS 正则空白切分，必须包含 `DIRECT_TOKEN_SCOPE`；
/// 4. `expires = now_ms + expires_in*1000 - EXPIRY_MARGIN_MS`（f64 毫秒，未做下界截断，
///    因此 `expires_in` 小于 3 分钟时结果为过去时间）。
///
/// 额外字段按 TS 的插入顺序写入 `extra`：先 `clientId`、后 `scopes`（`IndexMap` 保序，
/// `auth.json` 的字段顺序固定，改动需同步测试）。
fn credential_from_token_response(
    token: &TokenResponse,
    client_id: &str,
    clock: &dyn AuthClock,
) -> Result<OAuthCredential, AuthError> {
    let field = |name: &str| crate::auth::oauth::object_field(token, name);
    let access = require_token_string(field("access_token"), "access_token")?;
    let refresh = require_token_string(field("refresh_token"), "refresh_token")?;
    let scope = require_token_string(field("scope"), "scope")?;
    let expires_in = field("expires_in").and_then(Value::as_f64);
    let Some(expires_in) = expires_in.filter(|value| value.is_finite() && *value > 0.0) else {
        return Err(AuthError::new(
            "OpenAI OAuth token response has invalid expires_in",
        ));
    };
    let scopes: Vec<String> = split_js_whitespace(js_trim(&scope))
        .into_iter()
        .map(str::to_string)
        .collect();
    if !scopes.iter().any(|scope| scope == DIRECT_TOKEN_SCOPE) {
        return Err(AuthError::new(format!(
            "OpenAI OAuth grant did not include {DIRECT_TOKEN_SCOPE}"
        )));
    }
    let mut credential = OAuthCredential::new(
        access,
        refresh,
        clock.now_ms() + expires_in * 1000.0 - EXPIRY_MARGIN_MS,
    );
    credential
        .extra
        .insert("clientId".into(), Value::String(client_id.to_string()));
    credential.extra.insert(
        "scopes".into(),
        Value::Array(scopes.into_iter().map(Value::String).collect()),
    );
    Ok(credential)
}

/// `exchangeAuthorizationCode(code, verifier, clientId, signal)`。
///
/// 表单字段顺序是当前稳定契约（spec/ai-auth.md §2.12）：`grant_type`、`client_id`、`code`、
/// `code_verifier`、`redirect_uri`、`resource`。`client_id` 必须是回调里签发的 id（不是
/// `DYNAMIC_CLIENT_ID`），`redirect_uri` 必须与授权请求里的完全相同。
///
/// 额外要求响应里有非空 `id_token`，但它只做存在性检查、**不**参与后续逻辑（TS 同样如此，
/// 属于契约检查而非使用）；最终凭证仍由 `credential_from_token_response` 构造。
async fn exchange_authorization_code(
    fetch: &dyn OAuthFetch,
    clock: &dyn AuthClock,
    code: &str,
    verifier: &str,
    client_id: &str,
    signal: &AbortScope,
) -> Result<OAuthCredential, AuthError> {
    let body = form_urlencode(&[
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", REDIRECT_URI),
        ("resource", RESOURCE),
    ]);
    let token = request_token(fetch, body, signal).await?;
    let id_token = crate::auth::oauth::object_field(&token, "id_token").and_then(Value::as_str);
    if !id_token.is_some_and(|id_token| !js_trim(id_token).is_empty()) {
        return Err(AuthError::new(
            "OpenAI OAuth token response did not contain an ID token",
        ));
    }
    credential_from_token_response(&token, client_id, clock)
}

/// `refreshAccessToken(credential, signal)`。
///
/// 从凭证的 `clientId` 额外字段取 client id；缺失或空白时报「reconnect ChatGPT」——这类
/// 凭证无法刷新，只能重新登录。
/// 表单顺序：`grant_type=refresh_token`、`client_id`、`refresh_token`、`resource`；
/// **不带** `scope`，新凭证的 `scopes` 完全来自响应（以响应为准），`clientId` 沿用同一个值。
async fn refresh_access_token(
    fetch: &dyn OAuthFetch,
    clock: &dyn AuthClock,
    credential: &OAuthCredential,
    signal: &AbortScope,
) -> Result<OAuthCredential, AuthError> {
    let client_id = match credential.get_str("clientId") {
        Some(client_id) if !js_trim(client_id).is_empty() => client_id.to_string(),
        _ => {
            return Err(AuthError::new(
                "Stored OpenAI OAuth credential does not contain an issued client ID; reconnect ChatGPT",
            ));
        }
    };
    let body = form_urlencode(&[
        ("grant_type", "refresh_token"),
        ("client_id", &client_id),
        ("refresh_token", &credential.refresh),
        ("resource", RESOURCE),
    ]);
    let token = request_token(fetch, body, signal).await?;
    credential_from_token_response(&token, &client_id, clock)
}

/// `agentHostId(deviceId)`。
///
/// 把安装级设备 ID 规范成 URN（`urn:uuid:<小写 UUID>`），作为授权 URL 的
/// `ext_agent_host_id`。缺失与格式非法共用同一条错误文案（TS 行为一致），调用方无法也
/// 不需要区分。该值在启动回调服务器之前求值，所以非法配置不会占用端口、也不会发出授权事件。
fn agent_host_id(device_id: Option<String>) -> Result<String, AuthError> {
    let Some(device_id) = device_id else {
        return Err(AuthError::new(
            "Sign in with ChatGPT requires a device ID (UUID) for this installation",
        ));
    };
    if !UUID_PATTERN.is_match(&device_id) {
        return Err(AuthError::new(
            "Sign in with ChatGPT requires a device ID (UUID) for this installation",
        ));
    }
    Ok(format!("urn:uuid:{}", device_id.to_ascii_lowercase()))
}

/// OpenAI ChatGPT OAuth 流程。
///
/// 实现 [`OAuthAuth`]：`name`/`is_subscription`/`login_label`
/// 提供元数据，`login`/`refresh` 走网络，`to_auth` 只做本地映射。
///
/// 两个依赖都以 `Arc<dyn ...>` 保存，便于测试注入：`OAuthFetch` 替换 HTTP 层，`AuthClock`
/// 固定 `Date.now()`（`expires` 的计算依赖它）。结构体自身无内部可变状态，可被多个调用方
/// 共享；刷新调用的串行化由外部 `Models` 在 store 锁内完成，不在本类型里。
pub struct OpenAIChatGPTOAuth {
    fetch: Arc<dyn OAuthFetch>,
    clock: Arc<dyn AuthClock>,
}

impl OpenAIChatGPTOAuth {
    /// 默认依赖（reqwest + 系统时钟）。
    pub fn new() -> Self {
        OpenAIChatGPTOAuth {
            fetch: default_oauth_fetch(),
            clock: Arc::new(SystemAuthClock),
        }
    }

    /// 注入传输（测试用），时钟保持系统时钟。
    ///
    /// 集成测试用脚本化的 `MockFetch` 替换真实网络；生产路径走 [`Self::new`]。
    pub fn with_fetch(fetch: Arc<dyn OAuthFetch>) -> Self {
        OpenAIChatGPTOAuth {
            fetch,
            clock: Arc::new(SystemAuthClock),
        }
    }

    /// 同时注入传输与时钟（测试用）。
    ///
    /// 注入自定义时钟可固定 `now`，便于精确断言 `expires`；当前集成测试未使用本构造器
    /// （`tests/openai_chatgpt_oauth.rs` 走 `with_fetch` + 系统时钟做区间比较）。
    pub fn with_dependencies(fetch: Arc<dyn OAuthFetch>, clock: Arc<dyn AuthClock>) -> Self {
        OpenAIChatGPTOAuth { fetch, clock }
    }
}

impl Default for OpenAIChatGPTOAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAIChatGPTOAuth {
    /// `loginOpenAIChatGPT(interaction, options)`。
    ///
    /// 完整流程（阶段编号对应函数内的 `//` 注释）：
    ///
    /// 1. 设备 ID → `urn:uuid:...`；失败在任何网络/监听动作之前返回，不产生授权事件。
    /// 2. PKCE + `state` + `nonce`（verifier/state/nonce 各取 32 字节 CSPRNG，challenge 由
    ///    verifier 派生）。
    /// 3. 启动固定端口的回调服务器；`EADDRINUSE` 改写为固定文案，不回退端口。
    /// 4. 拼授权 URL 并 `notify(AuthEvent::AuthUrl)`，随后创建「手动粘贴」future。
    /// 5. `select!` 同时等浏览器回调与粘贴，谁先完成用谁，另一个分支被丢弃。
    /// 6. `notify(Progress)` 后用选中的 `code` 兑换 token。
    /// 7. 收尾：abort 手动信号的 controller、`close()`、`close_all_connections()`。
    ///
    /// 错误处理：兑换失败原样返回；但如果 `interaction.signal()` 已 abort，则统一改写成
    /// `Login cancelled`（TS catch 分支），因此 abort 期间的真实网络错误会被掩盖。服务器
    /// 启动失败在 `select!` 之前返回（端口占用文案就是在这里生成的）；授权 URL 构造的 `?`
    /// 同样在收尾之前，但它只依赖常量 base，实际不可达。
    ///
    /// 不变量：
    /// - 服务器成功启动后到第 7 步之间一直监听 1455，期间到达的额外回调只会尝试填充
    ///   结果槽，不影响已被取走的结果；
    /// - 回调与粘贴共享同一个 `state`，但校验强度不同：回调只查 pathname 与 state（不校验
    ///   请求来源），粘贴额外要求与 `REDIRECT_URI` 同源同 path；
    /// - 本函数没有超时，用户既不完成也不 abort 时会一直等待。
    async fn login_openai_chatgpt(
        &self,
        interaction: &dyn AuthInteraction,
        options: &LoginOptions,
    ) -> Result<OAuthCredential, AuthError> {
        // 阶段 1：先校验设备 ID。失败时不监听端口、不发授权事件（TS 在
        // startCallbackServer 之前求值）。
        let device_id = options.get_device_id.as_ref().map(|get| get());
        let host_id = agent_host_id(device_id)?;
        // 阶段 2：一次性随机值。state 同时用于浏览器回调与手动粘贴两路的校验。
        let pkce = generate_pkce();
        let state = random_value();
        let nonce = random_value();
        // 阶段 3：固定端口回调服务器；占用时只报错、不回退（理由见文件头）。
        // 没有这个服务器时，浏览器回调可能打到占用端口的另一个登录 / Codex CLI 上并被以
        // state mismatch 拒绝。这里明确报出端口占用，而不是回退到别的端口。
        let server = ChatGptCallbackServer::start(state.clone())
            .await
            .map_err(|error| {
                if error.error_code.as_deref() == Some("EADDRINUSE") {
                    AuthError::new(format!(
                        "Port {CALLBACK_PORT} is in use, probably by an unfinished login in another kiana session or by the Codex CLI. Cancel that login and try again."
                    ))
                } else {
                    error
                }
            })?;

        // 阶段 4：参数顺序是当前稳定契约；agent_name_hint 可由调用方覆盖。
        let authorization_url = url_with_search(
            AUTHORIZE_URL,
            &[
                ("client_id", DYNAMIC_CLIENT_ID),
                (
                    "agent_name_hint",
                    options.agent_name.as_deref().unwrap_or(AGENT_NAME_HINT),
                ),
                ("ext_agent_host_id", &host_id),
                ("response_type", "code"),
                ("redirect_uri", REDIRECT_URI),
                ("resource", RESOURCE),
                ("scope", SCOPE),
                ("state", &state),
                ("code_challenge", &pkce.challenge),
                ("code_challenge_method", "S256"),
                ("nonce", &nonce),
            ],
        )?;
        interaction.notify(&AuthEvent::AuthUrl {
            url: authorization_url,
            instructions: Some(
                "Complete sign-in in your browser. If the callback does not complete, paste the final redirect URL here."
                    .into(),
            ),
        });

        // 阶段 5：手动粘贴与浏览器回调并行。合并 manual_abort 与 interaction 的 signal，
        // 任一来源 abort 都能结束 prompt。两个输入都非 None，因此 signal 必为 Some，
        // unwrap_or_default 只是防御。
        let manual_abort = AbortScope::new();
        let combined = combine_abort_signals(&[
            Some(manual_abort.clone()),
            Some(interaction.signal().clone()),
        ]);
        let prompt_signal = combined.signal.clone().unwrap_or_default();
        let manual = async {
            let input = interaction
                .prompt(AuthPromptRequest::with_signal(
                    AuthPrompt::ManualCode {
                        message:
                            "Complete login in your browser, or paste the final redirect URL here:"
                                .into(),
                        placeholder: Some(REDIRECT_URI.into()),
                    },
                    prompt_signal,
                ))
                .await?;
            authorization_result_from_manual_input(&input, &state)
        };
        tokio::pin!(manual);

        // 两个 future 竞争：先完成者决定登录走哪条路。落败的分支在这里被 drop——回调先到时
        // 尚未返回的 prompt 被取消；粘贴先到时 `server.wait()` 的 future 被丢弃，但服务器
        // 仍在监听，直到第 7 步才关闭。
        let raced = tokio::select! {
            result = server.wait() => result,
            result = &mut manual => result,
        };

        // 阶段 6：兑换。无论结果来自回调还是粘贴，client_id 都是本次签发的值，
        // verifier 都是第 2 步生成的那一个。
        let result = match raced {
            Ok(authorization) => {
                interaction.notify(&AuthEvent::Progress {
                    message: "Exchanging authorization code for tokens...".into(),
                });
                exchange_authorization_code(
                    self.fetch.as_ref(),
                    self.clock.as_ref(),
                    &authorization.code,
                    &pkce.verifier,
                    &authorization.client_id,
                    interaction.signal(),
                )
                .await
            }
            Err(error) => Err(error),
        };

        // 阶段 7：无条件收尾（对应 TS 的 finally）。abort 手动信号的 controller，让 prompt
        // 一侧（其 signal 可能仍被宿主持有）收到取消；随后停止 accept 并强制断开所有连接，
        // 否则浏览器预开的空闲 socket 会一直挂在旧 handler 上。
        manual_abort.abort();
        server.close();
        server.close_all_connections();

        // 已 abort 的登录统一报 "Login cancelled"，优先于底层错误（TS catch 分支）。
        match result {
            Ok(credential) => Ok(credential),
            Err(error) => {
                if interaction.signal().is_aborted() {
                    Err(AuthError::new("Login cancelled"))
                } else {
                    Err(error)
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl OAuthAuth for OpenAIChatGPTOAuth {
    /// 显示名。provider 侧 `lazy_oauth` 还持有一份同样的字面量
    /// （`providers/openai.rs`），两处由人工保持一致：provider 展示走的是 LazyOAuth 自己的
    /// 字段，不读这里。
    fn name(&self) -> &str {
        "OpenAI (ChatGPT subscription)"
    }

    /// ChatGPT 登录由订阅支撑；provider 的 LazyOAuth 也硬编码了 `true`。
    fn is_subscription(&self) -> bool {
        true
    }

    /// 登录按钮文案；同样与 provider 侧的字面量保持一致。
    fn login_label(&self) -> Option<&str> {
        Some("Sign in with ChatGPT")
    }

    /// 交互式登录；完整阶段见 `login_openai_chatgpt`。这里只做转发。
    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
        options: &LoginOptions,
    ) -> Result<OAuthCredential, AuthError> {
        self.login_openai_chatgpt(interaction, options).await
    }

    /// 用凭证里的 `clientId` 刷新；失败即返回错误（重试由外部 `Models` 决定）。
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: &AbortScope,
    ) -> Result<OAuthCredential, AuthError> {
        refresh_access_token(self.fetch.as_ref(), self.clock.as_ref(), credential, signal).await
    }

    /// 只产出 `apiKey = access`，不设 `headers`/`baseUrl`：请求地址沿用 provider 配置的
    /// baseUrl（内置 openai provider 是 `https://api.openai.com/v1`）。本实现不会失败
    /// （没有需要校验的输入）。
    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..ModelAuth::default()
        })
    }
}

/// 便捷构造（默认依赖）。
///
/// 等价于 `Arc::new(OpenAIChatGPTOAuth::new())` 转成 `Arc<dyn OAuthAuth>`。内置接线
/// （`auth/oauth/load.rs` 的 `OPENAI_CHATGPT` 单例、`providers/openai.rs` 的 `lazy_oauth`）
/// 不经过这个函数，而是直接构造同一个实现。
pub fn openai_chatgpt_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(OpenAIChatGPTOAuth::new())
}

#[cfg(test)]
mod tests {
    //! 单测：手动粘贴的 origin/path 校验、回调校验的错误文案、agent host id 规范化、
    //! JS 空白切分、Node 监听错误文案，以及 `closeAllConnections` 断开空闲连接的回归。
    //!
    //! 固定端口 1455 的端到端用例在 `tests/openai_chatgpt_oauth.rs`（用 `SERIAL` 串行）；
    //! 这里只调用纯函数或 `start_with_listener`（端口 0），不占用固定端口，可并行执行。

    use super::*;

    #[test]
    fn manual_input_requires_the_expected_origin_and_path() {
        let url = format!("{REDIRECT_URI}?code=c&state=s&client_id=cid");
        let result = authorization_result_from_manual_input(&url, "s").expect("result");
        assert_eq!(result.code, "c");
        assert_eq!(result.client_id, "cid");

        let error = authorization_result_from_manual_input("http://127.0.0.1:1455/other", "s")
            .expect_err("wrong path");
        assert!(error.message.contains("must start with"));

        let error = authorization_result_from_manual_input("not a url", "s").expect_err("bad url");
        assert_eq!(
            error.message,
            "Paste the full callback URL from the browser"
        );
    }

    #[test]
    fn callback_validation_messages_match() {
        let url = Url::parse("http://127.0.0.1:1455/auth/callback?code=c&state=s").unwrap();
        assert!(
            authorization_result_from_callback(&url, "other")
                .unwrap_err()
                .message
                .contains("OAuth state mismatch")
        );
        let url = Url::parse("http://127.0.0.1:1455/auth/callback?code=c&state=s").unwrap();
        assert!(
            authorization_result_from_callback(&url, "s")
                .unwrap_err()
                .message
                .contains("issued client ID")
        );
    }

    #[test]
    fn agent_host_ids_are_normalized() {
        assert_eq!(
            agent_host_id(Some("E61BBE28-07EF-466D-8E5D-A344F94AB305".into())).unwrap(),
            "urn:uuid:e61bbe28-07ef-466d-8e5d-a344f94ab305"
        );
        assert!(agent_host_id(None).is_err());
        assert!(agent_host_id(Some("not-a-uuid".into())).is_err());
    }

    #[test]
    fn scopes_are_split_on_js_whitespace() {
        let scopes = split_js_whitespace(js_trim("openid  profile\temail"));
        assert_eq!(scopes, vec!["openid", "profile", "email"]);
    }

    #[test]
    fn listen_errors_use_node_messages() {
        // Node 26 实测：`listen EADDRINUSE: address already in use 127.0.0.1:1455`。
        let error = std::io::Error::from_raw_os_error(libc::EADDRINUSE);
        let auth = listen_error("127.0.0.1", 1455, &error);
        assert_eq!(
            auth.message,
            "listen EADDRINUSE: address already in use 127.0.0.1:1455"
        );
        assert_eq!(auth.error_code.as_deref(), Some("EADDRINUSE"));

        let error = std::io::Error::from_raw_os_error(libc::EACCES);
        let auth = listen_error("127.0.0.1", 1455, &error);
        assert_eq!(
            auth.message,
            "listen EACCES: permission denied 127.0.0.1:1455"
        );
        assert_eq!(auth.error_code.as_deref(), Some("EACCES"));

        // Node 26 实测：`getaddrinfo ENOTFOUND no-such-host.invalid`。
        let error = std::io::Error::other(
            "failed to lookup address information: Name or service not known",
        );
        let auth = getaddrinfo_error("no-such-host.invalid", &error);
        assert_eq!(auth.message, "getaddrinfo ENOTFOUND no-such-host.invalid");
    }

    /// `closeAllConnections()` 必须断开已经 accept、尚未发请求的空闲连接。
    ///
    /// 回归：只取消 accept 循环时，旧 handler 仍然阻塞在读取请求上，这条连接会活到进程结束，
    /// 下一次登录的回调可能沿它送达并被旧 state 拒绝（spec/ai-auth.md §2.12）。
    #[tokio::test]
    async fn close_all_connections_drops_idle_connections() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let server = ChatGptCallbackServer::start_with_listener(listener, "expected".into());
        let mut stream = TcpStream::connect(address).await.expect("connect");
        // 让 accept 循环把连接交给 handler（handler 此时阻塞在读取请求上）。
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        server.close_all_connections();

        let mut buffer = [0u8; 1];
        let read =
            tokio::time::timeout(std::time::Duration::from_secs(2), stream.read(&mut buffer)).await;
        match read {
            Ok(Ok(0)) | Ok(Err(_)) => {}
            other => panic!("idle connection survived closeAllConnections: {other:?}"),
        }
    }
}
