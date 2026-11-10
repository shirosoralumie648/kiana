//! auth/oauth/openai-codex.ts——OpenAI Codex（ChatGPT OAuth）流程。
//!
//! 两种登录方式：浏览器回调（端口 1455，与 Codex CLI 共享；被占用时回退到粘贴）与设备码。
//! token 响应里的 access token 是 JWT；只从 `https://api.openai.com/auth` claim 里读取
//! `chatgpt_account_id`（`decode_jwt` 用严格标准 base64 的 `atob`，base64url payload 会失败）。
//!
//! TS: `src/auth/oauth/openai-codex.ts`
//!
//! # 这个文件负责什么
//!
//! 实现 OpenAI Codex（ChatGPT Plus/Pro 订阅）的两条 OAuth 登录路径，并把结果规范化成
//! `OAuthCredential`：
//!
//! - 浏览器登录：固定端口 1455 的 loopback 回调 + PKCE + `state`；端口被占用时回退到
//!   用户粘贴授权码/重定向 URL。
//! - 设备码登录（RFC 8628）：面向无浏览器/远程环境；轮询最多 15 分钟，403/404 视为
//!   “用户还没批准”。
//!
//! 两条路径最终都走同一个 token 端点，只是 `redirect_uri` 不同（浏览器用 `REDIRECT_URI`，
//! 设备码用常量 `DEVICE_REDIRECT_URI`），并且生成的凭证都必须带 `accountId`。
//!
//! # 在系统中的位置
//!
//! `providers/openai_codex.rs` 通过 `auth::helpers::lazy_oauth` 登记本流程；
//! `auth/oauth/load.rs` 的 `OPENAI_CODEX` 单例也返回它。上层 `Models::login` 通过
//! [`OAuthAuth`] 调用 `login`；凭证过期时 `auth::resolve`
//! 调用 `refresh`（15 秒超时，由调用方传入的 signal 驱动）；`to_auth` 把 access token
//! 放进 `ModelAuth::api_key`，base URL 与请求格式由 `providers/openai_codex.rs` /
//! `api/openai_codex_responses.rs` 负责。
//!
//! # 一次浏览器登录
//!
//! 1. `login` 弹出 `Select` 让用户选 `browser`（默认）或 `device_code`；未知值报
//!    `Unknown OpenAI Codex login method: <value>`。
//! 2. `create_authorization_flow` 生成 PKCE 与 32 位十六进制 `state`，拼出授权 URL
//!    （参数顺序见该函数注释）。
//! 3. 在 `oauth_callback_host()`（环境变量 `KIANA_OAUTH_CALLBACK_HOST`，默认 `127.0.0.1`）
//!    的 1455 端口启动回调服务器，只接受 `/auth/callback` 且 `state` 必须匹配。
//!    启动失败（最常见是端口被占）被 `.ok()` 吞掉，流程继续，只能手动粘贴。
//! 4. `notify(AuthUrl)` 后，回调与手动粘贴竞争（`wait_for_callback_or_manual_input`）。
//! 5. 手动输入用共享的 `parse_authorization_input` 解析：URL → `code#state` → `code=`
//!    查询串 → 整串当 code；非空 `state` 不匹配时报 `State mismatch`；没有 code 报
//!    `Missing authorization code`。
//! 6. 表单 POST 兑换 token，得到 `access`/`refresh`/`expires`，再从 JWT claim 里取
//!    `chatgpt_account_id` 写进凭证的 `extra["accountId"]`。最后无条件 `close()` 服务器。
//!
//! # 一次设备码登录
//!
//! 1. POST `{"client_id": ...}` 到 `DEVICE_USER_CODE_URL`，得到 `device_auth_id`、
//!    `user_code` 与轮询间隔。
//! 2. `notify(DeviceCode)` 展示用户码与验证地址；事件里的 `user_code` 按 JS `String()`
//!    转换（服务端返回数字也能显示）。
//! 3. 立即开始轮询（`wait_before_first_poll = false`，首轮在 t=0）：POST
//!    `{device_auth_id, user_code}`；2xx 且两个字段齐全 → 完成；403/404 → pending；
//!    错误体 `error.code == "deviceauth_authorization_pending"` → pending；
//!    `slow_down` → 放慢（不带新间隔，由轮询器按 RFC 8628 加 5 秒）；其他 → 失败
//!    （文案带状态码与原始 body）。
//! 4. 轮询上限 15 分钟；超时/取消的文案来自 `device_code` 模块。
//! 5. 用设备端点返回的 `authorization_code` 与 `code_verifier` 兑换 token（本地 PKCE
//!    verifier 不参与这条路径），`redirect_uri` 使用 `DEVICE_REDIRECT_URI`。
//!
//! # 数据结构与凭证
//!
//! - `OAuthToken`：token 端点的三个必填字段；`expires = now_ms + expires_in*1000`
//!   （本流程**不**预留余量）。
//! - `DeviceAuthInfo` / `DeviceTokenSuccess`：字段类型是 `serde_json::Value`，因为 TS
//!   只做真值检查后原样转发（或做 JS `String()`），见类型注释。
//! - 凭证的 `extra["accountId"]` 来自 access token（JWT）的
//!   `https://api.openai.com/auth` → `chatgpt_account_id`（非空字符串）。**refresh 也
//!   要求新 token 含该 claim**，否则报 `Failed to extract accountId from token`。
//!
//! # 失败模式
//!
//! - 选择提示被取消 → 原样返回 `Login cancelled`（不进入任何网络动作）。
//! - 回调服务器启动失败不报错（回退粘贴）；预取消也会被吞掉，流程仍会发 `AuthUrl`。
//! - token 端点非 2xx → `OpenAI Codex token <exchange|refresh> failed (<status>): <body|statusText>`；
//!   2xx 但不是 JSON → 原始 `SyntaxError`（`name = "SyntaxError"`）不会被包装。
//! - 设备授权端点 404 → 提示“该服务器未启用设备码登录”；其他非 2xx → 带状态码与 body。
//! - 取消：设备轮询与 sleep 都会观察 `interaction.signal()`，最终返回 `Login cancelled`。
//! - 浏览器登录没有超时：用户不操作也不 abort 时会一直等待。
//! - 固定端口意味着同一主机同时只能有一个 Codex 浏览器登录收到回调；第二个只能粘贴。
//!
//! # 注意事项
//!
//! - 常量与字段顺序是协议契约（spec/ai-auth.md §2.11），包括授权 URL 的参数顺序与
//!   token 表单的键顺序；改动会让差分测试失败。
//! - access token 只被当作字符串放进 `ModelAuth::api_key`；本模块不验证 JWT 签名，
//!   解码 claim 只是为了拿到 `accountId`（token 来自 TLS 保护的服务端响应）。
//! - `create_state` 的 Node 环境检查在 Rust 不可达：操作系统随机源始终存在，失败即 panic。

use std::sync::Arc;

use serde_json::Value;

use crate::auth::oauth::callback_server::{
    CallbackManualPrompt, CallbackOrManualInput, OAuthCallbackServerOptions,
    start_oauth_callback_server, wait_for_callback_or_manual_input,
};
use crate::auth::oauth::device_code::{
    AuthClock, OAuthDeviceCodePollOptions, OAuthDeviceCodePollResult, SystemAuthClock,
    poll_oauth_device_code_flow_with_clock,
};
use crate::auth::oauth::pkce::generate_pkce;
use crate::auth::oauth::{
    OAuthFetch, OAuthFetchRequest, OAuthFetchResponse, atob_standard, default_oauth_fetch,
    form_urlencode, invalid_url_error, is_object_like, json_object, json_stringify, latin1_string,
    oauth_callback_host, parse_authorization_input,
};
use crate::auth::types::{
    AuthError, AuthEvent, AuthInteraction, AuthPrompt, AuthPromptRequest, AuthSelectOption,
    LoginOptions, ModelAuth, OAuthAuth, OAuthCredential,
};
use crate::utils::abort::AbortScope;

/// `CLIENT_ID`。
///
/// Codex 的公开 OAuth client id，不是秘密；授权与兑换都提交它。
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// `AUTHORIZE_URL`。
///
/// 浏览器授权端点；授权 URL 的查询参数由 `create_authorization_flow` 生成。
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
/// `TOKEN_URL`。
///
/// 授权码兑换与 refresh 共用的 token 端点。
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// `REDIRECT_URI`。
///
/// 授权 URL、浏览器路径的 token 兑换、手动粘贴的占位符三处使用同一个字面量。
/// 注意主机是 `localhost`，而回调服务器实际绑定 `oauth_callback_host()`（默认
/// `127.0.0.1`）。默认配置下两者是同一个本机端口；若环境变量把监听地址改到别处，
/// 浏览器仍会被重定向到 `localhost:1455`，收不到回调时只能手动粘贴。字符串必须保持
/// 这里的值：它要与授权时登记的 redirect_uri 完全一致，否则令牌兑换会被拒。
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
/// `DEVICE_USER_CODE_URL`。
///
/// 设备码第一步：申请 `device_auth_id` 与 `user_code`。
const DEVICE_USER_CODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
/// `DEVICE_TOKEN_URL`。
///
/// 设备码轮询端点；403/404 表示尚未批准，不是错误。
const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
/// `DEVICE_VERIFICATION_URI`。
///
/// 展示给用户的验证地址（事件 `DeviceCode.verification_uri`）。
const DEVICE_VERIFICATION_URI: &str = "https://auth.openai.com/codex/device";
/// `DEVICE_REDIRECT_URI`。
///
/// 设备码路径在 token 兑换时提交的 `redirect_uri`，由服务端约定，与本地回调服务器无关。
const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
/// `DEVICE_CODE_TIMEOUT_SECONDS`。
///
/// 轮询总时长（15 分钟），同时用于 `DeviceCode` 事件的 `expires_in_seconds`。
const DEVICE_CODE_TIMEOUT_SECONDS: f64 = 15.0 * 60.0;
/// `OPENAI_CODEX_BROWSER_LOGIN_METHOD`。
///
/// 选择提示的 option id（不是展示文案）；`login` 用它和用户返回值比较。
const OPENAI_CODEX_BROWSER_LOGIN_METHOD: &str = "browser";
/// `OPENAI_CODEX_DEVICE_CODE_LOGIN_METHOD`。
///
/// 同上，设备码登录的 option id。
const OPENAI_CODEX_DEVICE_CODE_LOGIN_METHOD: &str = "device_code";
/// `SCOPE`。
///
/// 授权请求的 scope 串；`offline_access` 用于申请 refresh token。顺序与空格分隔形式
/// 都是协议契约（测试断言授权 URL 的编码结果）。
const SCOPE: &str = "openid profile email offline_access";
/// `JWT_CLAIM_PATH`。
///
/// OpenAI 在 access token 里放账号信息的 claim 命名空间；`chatgpt_account_id` 嵌在它下面。
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";

/// `OAuthToken`（`access`/`refresh`/`expires`）。
///
/// `expires` 是**绝对**毫秒时间戳（`Date.now()` 时基），不是响应里的相对秒数；
/// `read_token_response` 用注入时钟算出它，测试因此可以用假时钟断言具体数值。
#[derive(Debug, Clone, PartialEq)]
struct OAuthToken {
    access: String,
    refresh: String,
    expires: f64,
}

/// `TokenOperation`。
///
/// 只用于 token 端点错误文案里的操作名（`exchange`/`refresh`），不参与流程分支。
#[derive(Debug, Clone, Copy)]
enum TokenOperation {
    Exchange,
    Refresh,
}

impl TokenOperation {
    fn as_str(self) -> &'static str {
        match self {
            TokenOperation::Exchange => "exchange",
            TokenOperation::Refresh => "refresh",
        }
    }
}

/// `DeviceAuthInfo`。
///
/// TS 的类型标注是 `string`，但运行时只做真值检查后**原样**转发进轮询请求体
/// （`JSON.stringify({ device_auth_id, user_code })`），因此这里保留原始 JSON 值。
/// `interval_seconds` 来自响应里的 `interval`（字符串按 JS `Number(trim)` 解析）；
/// 允许为 `0`，轮询器会把它提升到最少 1 秒。
#[derive(Debug, Clone)]
struct DeviceAuthInfo {
    device_auth_id: Value,
    user_code: Value,
    interval_seconds: f64,
}

/// `DeviceTokenSuccess`。
///
/// 同 [`DeviceAuthInfo`]：真值检查后原样转发（`URLSearchParams` 会做 JS `String()` 转换）。
#[derive(Debug, Clone)]
struct DeviceTokenSuccess {
    authorization_code: Value,
    code_verifier: Value,
}

/// OpenAI Codex OAuth 流程。
///
/// 保存注入的 HTTP 传输与时钟；除此之外无状态。登录期间产生的 PKCE、state、设备码会话
/// 都是局部变量，实例本身不保存登录进度，因此可以重复登录；浏览器路径受固定端口 1455
/// 限制（见模块文档），设备码路径没有这个限制。
///
/// `OAuthAuth` 要求 `Send + Sync`，两个 `Arc<dyn ...>` 字段都满足，provider 注册表可以
/// 用 `Arc<dyn OAuthAuth>` 长期共享同一实例。
pub struct OpenAICodexOAuth {
    fetch: Arc<dyn OAuthFetch>,
    clock: Arc<dyn AuthClock>,
}

impl OpenAICodexOAuth {
    /// 默认依赖（reqwest + 系统时钟）。
    ///
    /// `default_oauth_fetch()` 会先看进程级测试替身；没被替换时使用惰性构造、进程内复用
    /// 的 reqwest 客户端。
    pub fn new() -> Self {
        OpenAICodexOAuth {
            fetch: default_oauth_fetch(),
            clock: Arc::new(SystemAuthClock),
        }
    }

    /// 注入传输（测试用），时钟保持系统时钟。
    pub fn with_fetch(fetch: Arc<dyn OAuthFetch>) -> Self {
        OpenAICodexOAuth {
            fetch,
            clock: Arc::new(SystemAuthClock),
        }
    }

    /// 同时注入传输与时钟（测试用）。
    ///
    /// 设备码流程的 deadline 与 token 过期时间都读这个时钟；实际 sleep 仍走 tokio 计时器，
    /// 测试用假时钟配合 `start_paused = true` 断言“首轮 t=0、第二轮 +5s、15 分钟超时”。
    /// 浏览器路径只用它计算 `expires`。
    pub fn with_dependencies(fetch: Arc<dyn OAuthFetch>, clock: Arc<dyn AuthClock>) -> Self {
        OpenAICodexOAuth { fetch, clock }
    }
}

impl Default for OpenAICodexOAuth {
    fn default() -> Self {
        Self::new()
    }
}

/// `createState()`：16 字节随机数的 32 位小写十六进制。
///
/// TS 里 Node 缺失时会抛 `OpenAI Codex OAuth is only available in Node.js environments`；
/// Rust 始终有 OS 随机源，因此该分支不可达。
fn create_state() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("operating system random source");
    hex::encode(bytes)
}

/// `decodeJwt(token)`：`token.split(".")` 必须 3 段，payload 用严格标准 base64 的 `atob`，
/// 再 `JSON.parse`。任何异常 → `None`。
///
/// 约束与边界：
/// - 只接受标准 base64（`+`/`/`，可带填充）；base64url 的 `-`/`_` 会让
///   `atob_standard` 返回 `None`（Node `atob` 同样拒绝），这类 token 一律视为无法解析。
/// - 解码后的字节按 Latin-1 映射成字符串再交给 JS 语义的 JSON 解析器：`atob` 返回的是
///   二进制字符串，`JSON.parse` 逐 UTF-16 码元解释，这里保持一致。
/// - 不校验 JWT 签名与过期时间：调用方只用它读取一个服务端签发的 claim。
fn decode_jwt(token: &str) -> Option<Value> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload = parts.get(1).copied().unwrap_or("");
    let bytes = atob_standard(payload)?;
    let decoded = latin1_string(&bytes);
    kiana_json::js_parse::parse(&decoded).ok()
}

/// `getAccountId(accessToken)`：`payload["https://api.openai.com/auth"].chatgpt_account_id`。
///
/// 返回 `None` 的条件：不是三段 JWT、payload 不是标准 base64 的 JSON、claim 路径缺失、
/// `chatgpt_account_id` 不是字符串或为空串。调用方 `credentials_from_token` 把 `None`
/// 统一转成 `Failed to extract accountId from token`。
fn get_account_id(access_token: &str) -> Option<String> {
    let payload = decode_jwt(access_token)?;
    let auth = crate::auth::oauth::object_field(&payload, JWT_CLAIM_PATH)?;
    let account_id = crate::auth::oauth::object_field(auth, "chatgpt_account_id")?;
    account_id
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// `fetchWithLoginCancellation(input, init)`：fetch 的中止错误统一成 `Login cancelled`。
///
/// 只处理**传输错误**：HTTP 非 2xx 仍是 `Ok(response)`，由调用方决定文案。失败后复查
/// `signal.is_aborted()` 而不是看错误名，是因为底层取消可能被传输层报成 `AbortError`
/// 或 `TypeError`，名字不足以判断“是用户取消还是网络故障”。
async fn fetch_with_login_cancellation(
    fetch: &dyn OAuthFetch,
    request: OAuthFetchRequest,
) -> Result<OAuthFetchResponse, AuthError> {
    let signal = request.signal.clone();
    match fetch.fetch(request).await {
        Ok(response) => Ok(response),
        Err(error) => {
            if signal.is_aborted() {
                Err(AuthError::new("Login cancelled"))
            } else {
                Err(error)
            }
        }
    }
}

/// `readTokenResponse(response, operation)`。
///
/// 校验顺序：
/// 1. 非 2xx → 用响应体文本拼错误；文本为空时退回 `status_text`（默认传输由状态码反推）。
/// 2. `response.json()` 失败 → 直接返回 `SyntaxError`（见下方注释）。
/// 3. `access_token`、`refresh_token` 必须是真值，`expires_in` 必须能按数字读取；缺失时
///    把整个 JSON 序列化进错误消息。
/// 4. `expires = clock.now_ms() + expires_in * 1000`（绝对毫秒，**不扣**余量）。
///
/// 两个 token 字段随后按字符串读取：通过真值检查但不是字符串的值会退化为空串（TS 的
/// 类型标注要求 string，正常的 token 端点不会返回其他类型）。
fn read_token_response(
    clock: &dyn AuthClock,
    response: &OAuthFetchResponse,
    operation: TokenOperation,
) -> Result<OAuthToken, AuthError> {
    if !response.ok() {
        let text = response.text();
        let status_text = if text.is_empty() {
            response.status_text.clone()
        } else {
            text
        };
        return Err(AuthError::new(format!(
            "OpenAI Codex token {} failed ({}): {}",
            operation.as_str(),
            response.status,
            status_text
        )));
    }

    // 没有 try/catch：非 JSON 响应体抛出原始 `SyntaxError`。
    let raw_json = response.json()?;
    // 存在性检查按 TS 的 truthy 语义（数字/对象等真值也算“有值”），类型收窄留到后面按
    // 字符串取值时发生。
    let field = |name: &str| crate::auth::oauth::object_field(&raw_json, name).cloned();
    let access = field("access_token").filter(|value| kiana_json::is_truthy(Some(value)));
    let refresh = field("refresh_token").filter(|value| kiana_json::is_truthy(Some(value)));
    let expires_in = field("expires_in").and_then(|value| value.as_f64());
    let (Some(access), Some(refresh), Some(expires_in)) = (access, refresh, expires_in) else {
        return Err(AuthError::new(format!(
            "OpenAI Codex token {} response missing fields: {}",
            operation.as_str(),
            json_stringify(&raw_json)
        )));
    };

    Ok(OAuthToken {
        access: access.as_str().unwrap_or_default().to_string(),
        refresh: refresh.as_str().unwrap_or_default().to_string(),
        expires: clock.now_ms() + expires_in * 1000.0,
    })
}

/// `exchangeAuthorizationCode(code, verifier, redirectUri, signal)`。
///
/// 浏览器与设备码两条路径共用；`redirect_uri` 由调用方决定（浏览器传 `REDIRECT_URI`，
/// 设备码传 `DEVICE_REDIRECT_URI`）。表单字段顺序是协议契约：
/// `grant_type=authorization_code`、`client_id`、`code`、`code_verifier`、`redirect_uri`。
///
/// 传输错误经 `fetch_with_login_cancellation` 处理（取消统一成 `Login cancelled`）；
/// HTTP 状态与字段校验由 `read_token_response` 负责。
async fn exchange_authorization_code(
    fetch: &dyn OAuthFetch,
    clock: &dyn AuthClock,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    signal: &AbortScope,
) -> Result<OAuthToken, AuthError> {
    let request = OAuthFetchRequest::new(TOKEN_URL, signal).form_body(&[
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", redirect_uri),
    ]);
    let response = fetch_with_login_cancellation(fetch, request).await?;
    read_token_response(clock, &response, TokenOperation::Exchange)
}

/// `refreshAccessToken(refreshToken, signal)`。
///
/// 与兑换共用 `TOKEN_URL` 与 `read_token_response`，但**不经过**
/// `fetch_with_login_cancellation`：TS 在这里把传输错误包装成
/// `OpenAI Codex token refresh error: <message>`，取消也不会改写成 `Login cancelled`。
/// 刷新失败只返回错误，本模块没有任何日志输出（测试
/// `does_not_write_token_refresh_failures_to_stderr` 只断言错误文案，不捕获 stderr）。
async fn refresh_access_token(
    fetch: &dyn OAuthFetch,
    clock: &dyn AuthClock,
    refresh_token: &str,
    signal: &AbortScope,
) -> Result<OAuthToken, AuthError> {
    let request = OAuthFetchRequest::new(TOKEN_URL, signal).form_body(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", CLIENT_ID),
    ]);
    let response = match fetch.fetch(request).await {
        Ok(response) => response,
        Err(error) => {
            return Err(AuthError::new(format!(
                "OpenAI Codex token refresh error: {}",
                error.message
            )));
        }
    };
    read_token_response(clock, &response, TokenOperation::Refresh)
}

/// `startOpenAICodexDeviceAuth(signal)`。
///
/// 设备码第一步：申请 user code。请求 `POST DEVICE_USER_CODE_URL`，JSON body 只有
/// `client_id`。
///
/// 响应校验对应 TS 的真值/有限数检查：
/// - `device_auth_id`、`user_code` 必须为真值（类型不限，原样保存）；
/// - `interval` 为字符串时按 JS `Number(trim)` 解析，其他类型按数字读取；缺失、非有限或
///   负数 → `Invalid OpenAI Codex device code response: <json>`；**0 是合法的**（轮询器会
///   把它提升到最少 1 秒）。
/// - HTTP 404 有专门文案（服务端未启用设备码登录）；其他非 2xx 带状态码与响应体。
async fn start_openai_codex_device_auth(
    fetch: &dyn OAuthFetch,
    signal: &AbortScope,
) -> Result<DeviceAuthInfo, AuthError> {
    let body = json_object(vec![("client_id", Value::String(CLIENT_ID.into()))]);
    let request = OAuthFetchRequest::new(DEVICE_USER_CODE_URL, signal)
        .header("Content-Type", "application/json")
        .post(kiana_json::js_object::stringify(&body).into_bytes());
    let response = fetch_with_login_cancellation(fetch, request).await?;

    if !response.ok() {
        if response.status == 404 {
            return Err(AuthError::new(
                "OpenAI Codex device code login is not enabled for this server. Use browser login or verify the server URL.",
            ));
        }
        let response_body = response.text();
        return Err(AuthError::new(format!(
            "OpenAI Codex device code request failed with status {}{}",
            response.status,
            if response_body.is_empty() {
                String::new()
            } else {
                format!(": {response_body}")
            }
        )));
    }

    let raw_json = response.json()?;
    // TS：`typeof interval === "string" ? Number(interval.trim()) : interval`。
    // 非数字与缺失都得到 NaN，随后与“非有限或负数”一起走统一的非法响应错误。
    let interval_seconds = match crate::auth::oauth::object_field(&raw_json, "interval") {
        Some(Value::String(interval)) => {
            kiana_json::js_num::parse_js_number(crate::auth::oauth::js_trim(interval))
        }
        Some(value) => value.as_f64().unwrap_or(f64::NAN),
        None => f64::NAN,
    };
    let device_auth_id = crate::auth::oauth::object_field(&raw_json, "device_auth_id")
        .filter(|value| kiana_json::is_truthy(Some(value)));
    let user_code = crate::auth::oauth::object_field(&raw_json, "user_code")
        .filter(|value| kiana_json::is_truthy(Some(value)));
    let (Some(device_auth_id), Some(user_code)) = (device_auth_id, user_code) else {
        return Err(invalid_device_code_error(&raw_json));
    };
    if !interval_seconds.is_finite() || interval_seconds < 0.0 {
        return Err(invalid_device_code_error(&raw_json));
    }
    Ok(DeviceAuthInfo {
        device_auth_id: device_auth_id.clone(),
        user_code: user_code.clone(),
        interval_seconds,
    })
}

/// `Invalid OpenAI Codex device code response: ${JSON.stringify(json)}`。
fn invalid_device_code_error(json: &Value) -> AuthError {
    AuthError::new(format!(
        "Invalid OpenAI Codex device code response: {}",
        json_stringify(json)
    ))
}

/// `pollOpenAICodexDeviceAuth(device, signal)`。
///
/// 设备码第二步：按 RFC 8628 轮询，直到拿到 `authorization_code` + `code_verifier`。
/// 间隔取设备授权响应里的 `interval`，总超时固定 15 分钟，首轮立即执行
/// （`wait_before_first_poll = false`；测试断言 t=0 就发第一次请求）。
///
/// 单次响应分类（顺序即代码里的判断顺序）：
/// - 2xx：解析 JSON；`authorization_code`/`code_verifier` 任一缺失 → `Failed`（消息含
///   整个 JSON）；齐全 → `Complete`。
/// - 403/404：直接 `Pending`（用户尚未批准，不解析 body）。
/// - 其他状态：解析 body 的 `error`：对象取 `.code`，字符串原样；
///   `deviceauth_authorization_pending` → `Pending`，`slow_down` → `SlowDown`（不带新
///   间隔，轮询器按 RFC 8628 加 5 秒），其余 → `Failed`（带状态码与原始 body）。
///
/// `Failed` 只是轮询结果，最终由 `poll_oauth_device_code_flow_with_clock` 转成 `Err`；
/// 2xx 但 body 不是 JSON 时 `response.json()?` 直接中止整个登录（`SyntaxError`）。
async fn poll_openai_codex_device_auth(
    fetch: Arc<dyn OAuthFetch>,
    clock: Arc<dyn AuthClock>,
    device: DeviceAuthInfo,
    signal: &AbortScope,
) -> Result<DeviceTokenSuccess, AuthError> {
    // `signal` 先克隆一份，供函数末尾的 `OAuthDeviceCodePollOptions` 使用；闭包捕获自己的
    // 副本，每轮再克隆进 `'static` 的 future。轮询器串行调用闭包，这些副本不会并发修改。
    let signal = signal.clone();
    let poll = {
        let fetch = Arc::clone(&fetch);
        let device = device.clone();
        let signal = signal.clone();
        move || -> crate::auth::oauth::device_code::OAuthDeviceCodePollFuture<DeviceTokenSuccess> {
            let fetch = Arc::clone(&fetch);
            let device = device.clone();
            let signal = signal.clone();
            Box::pin(async move {
                let body = json_object(vec![
                    ("device_auth_id", device.device_auth_id.clone()),
                    ("user_code", device.user_code.clone()),
                ]);
                let request = OAuthFetchRequest::new(DEVICE_TOKEN_URL, &signal)
                    .header("Content-Type", "application/json")
                    .post(kiana_json::js_object::stringify(&body).into_bytes());
                let response = fetch_with_login_cancellation(fetch.as_ref(), request).await?;

                if response.ok() {
                    let raw_json = response.json()?;
                    let authorization_code =
                        crate::auth::oauth::object_field(&raw_json, "authorization_code")
                            .filter(|value| kiana_json::is_truthy(Some(value)));
                    let code_verifier =
                        crate::auth::oauth::object_field(&raw_json, "code_verifier")
                            .filter(|value| kiana_json::is_truthy(Some(value)));
                    let (Some(authorization_code), Some(code_verifier)) =
                        (authorization_code, code_verifier)
                    else {
                        return Ok(OAuthDeviceCodePollResult::Failed {
                            message: format!(
                                "Invalid OpenAI Codex device auth token response: {}",
                                json_stringify(&raw_json)
                            ),
                        });
                    };
                    return Ok(OAuthDeviceCodePollResult::Complete {
                        value: DeviceTokenSuccess {
                            authorization_code: authorization_code.clone(),
                            code_verifier: code_verifier.clone(),
                        },
                    });
                }

                if response.status == 403 || response.status == 404 {
                    return Ok(OAuthDeviceCodePollResult::Pending);
                }

                let response_body = response.text();
                let error_code =
                    kiana_json::js_parse::parse(&response_body)
                        .ok()
                        .and_then(|json| {
                            let error = crate::auth::oauth::object_field(&json, "error")?;
                            if is_object_like(error) {
                                crate::auth::oauth::object_field(error, "code").cloned()
                            } else {
                                Some(error.clone())
                            }
                        });

                if error_code.as_ref().and_then(Value::as_str)
                    == Some("deviceauth_authorization_pending")
                {
                    return Ok(OAuthDeviceCodePollResult::Pending);
                }
                if error_code.as_ref().and_then(Value::as_str) == Some("slow_down") {
                    return Ok(OAuthDeviceCodePollResult::SlowDown {
                        interval_seconds: None,
                    });
                }

                Ok(OAuthDeviceCodePollResult::Failed {
                    message: format!(
                        "OpenAI Codex device auth failed with status {}{}",
                        response.status,
                        if response_body.is_empty() {
                            String::new()
                        } else {
                            format!(": {response_body}")
                        }
                    ),
                })
            })
        }
    };

    poll_oauth_device_code_flow_with_clock(
        OAuthDeviceCodePollOptions {
            interval_seconds: Some(device.interval_seconds),
            expires_in_seconds: Some(DEVICE_CODE_TIMEOUT_SECONDS),
            wait_before_first_poll: false,
            poll: Arc::new(poll),
            signal,
        },
        clock.as_ref(),
    )
    .await
}

/// `AuthorizationFlow`：`createAuthorizationFlow(originator)` 的返回，含一次浏览器登录
/// 用到的 PKCE verifier、state 与授权 URL。
///
/// `verifier` 只用于最后的 token 兑换；`state` 同时写入授权 URL 与回调服务器的期望值；
/// `url` 已包含全部查询参数，宿主直接交给浏览器打开即可。
#[derive(Debug, Clone)]
struct AuthorizationFlow {
    verifier: String,
    state: String,
    url: String,
}

/// `createAuthorizationFlow(originator)`。
///
/// 参数顺序是测试与 spec 的契约（`form_urlencode` 保持字段顺序）：
/// `response_type=code`、`client_id`、`redirect_uri`、`scope`、`code_challenge`、
/// `code_challenge_method=S256`、`state`、`id_token_add_organizations=true`、
/// `codex_cli_simplified_flow=true`、`originator`。
///
/// `originator` 来自 `LoginOptions::agent_name`，缺省为 `"kiana"`；它是 OpenAI 用来识别
/// 调用方的提示参数（测试锁定它来自 agentName）。返回的 `state` 由回调服务器校验。
fn create_authorization_flow(originator: &str) -> Result<AuthorizationFlow, AuthError> {
    let pkce = generate_pkce();
    let state = create_state();
    let query = form_urlencode(&[
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", REDIRECT_URI),
        ("scope", SCOPE),
        ("code_challenge", &pkce.challenge),
        ("code_challenge_method", "S256"),
        ("state", &state),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", originator),
    ]);
    let mut url = url::Url::parse(AUTHORIZE_URL).map_err(invalid_url_error)?;
    url.set_query(Some(&query));
    Ok(AuthorizationFlow {
        verifier: pkce.verifier,
        state,
        url: url.into(),
    })
}

/// `credentialsFromToken(token)`。
///
/// 把 token 响应变成凭证，并强制要求可解析出 `accountId`：`extra["accountId"]` 会随
/// `auth.json` 持久化（`OAuthCredential::extra` 是索引签名），供需要账号标识的调用方读取；
/// 缺失时报 `Failed to extract accountId from token`。refresh 也走这里，因此轮换后的
/// access token 同样必须带该 claim。
///
/// 注意：API 请求层会再次从 access token 里提取 accountId（见
/// `api/openai_codex_responses.rs` 的 `extract_account_id`），这里的 `extra` 不是唯一来源。
fn credentials_from_token(token: OAuthToken) -> Result<OAuthCredential, AuthError> {
    let Some(account_id) = get_account_id(&token.access) else {
        return Err(AuthError::new("Failed to extract accountId from token"));
    };
    let mut credential = OAuthCredential::new(token.access, token.refresh, token.expires);
    credential
        .extra
        .insert("accountId".into(), Value::String(account_id));
    Ok(credential)
}

/// `exchangeAuthorizationCodeForCredentials(...)`。
///
/// 组合两步：先兑换 token（`exchange_authorization_code`），再要求 JWT 里带 `accountId`
/// 生成凭证（`credentials_from_token`）。任何一步的错误都原样上抛，不重新包装。
async fn exchange_authorization_code_for_credentials(
    fetch: &dyn OAuthFetch,
    clock: &dyn AuthClock,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    signal: &AbortScope,
) -> Result<OAuthCredential, AuthError> {
    credentials_from_token(
        exchange_authorization_code(fetch, clock, code, verifier, redirect_uri, signal).await?,
    )
}

#[async_trait::async_trait]
impl OAuthAuth for OpenAICodexOAuth {
    /// 显示名，与 `providers/openai_codex.rs` 的 `lazy_oauth` 名称相同（两处需同步修改）。
    fn name(&self) -> &str {
        "OpenAI (ChatGPT Plus/Pro)"
    }

    /// 标记为订阅型 OAuth（`isSubscription: true`）；交互式 CLI 用它区分订阅登录入口
    /// 与其他 OAuth 登录。
    fn is_subscription(&self) -> bool {
        true
    }

    /// 交互式登录：先弹出选择提示，`device_code` 走设备码流程，`browser` 走浏览器流程，
    /// 其他取值直接报 `Unknown OpenAI Codex login method: <value>`。
    ///
    /// 选择提示本身被取消时错误原样返回（通常是 `Login cancelled`），不会发起网络请求。
    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
        options: &LoginOptions,
    ) -> Result<OAuthCredential, AuthError> {
        let method = interaction
            .prompt(AuthPromptRequest::new(AuthPrompt::Select {
                message: "Select OpenAI Codex login method:".into(),
                options: vec![
                    AuthSelectOption {
                        id: OPENAI_CODEX_BROWSER_LOGIN_METHOD.into(),
                        label: "Browser login (default)".into(),
                        description: None,
                    },
                    AuthSelectOption {
                        id: OPENAI_CODEX_DEVICE_CODE_LOGIN_METHOD.into(),
                        label: "Device code login (headless)".into(),
                        description: None,
                    },
                ],
            }))
            .await?;

        if method == OPENAI_CODEX_DEVICE_CODE_LOGIN_METHOD {
            return self.login_device_code(interaction).await;
        }
        if method != OPENAI_CODEX_BROWSER_LOGIN_METHOD {
            return Err(AuthError::new(format!(
                "Unknown OpenAI Codex login method: {method}"
            )));
        }
        self.login_browser(interaction, options).await
    }

    /// 用 refresh token 换新 token；新 access token 必须含 `chatgpt_account_id` claim。
    ///
    /// 传输错误包装成 `OpenAI Codex token refresh error: ...`；非 2xx/字段缺失走
    /// `read_token_response` 的统一文案。调用方（`auth::resolve`）负责超时、把结果持久化，
    /// 并在失败时再包装成带 `oauth` code 的 `ModelsError`。
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: &AbortScope,
    ) -> Result<OAuthCredential, AuthError> {
        credentials_from_token(
            refresh_access_token(
                self.fetch.as_ref(),
                self.clock.as_ref(),
                &credential.refresh,
                signal,
            )
            .await?,
        )
    }

    /// 请求鉴权：只把 access token 作为 `api_key`，不设置 base URL 或额外 headers。
    ///
    /// 无副作用、无网络请求，可在每次请求前调用。
    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..ModelAuth::default()
        })
    }
}

impl OpenAICodexOAuth {
    /// 设备码登录（`loginOpenAICodexDeviceCode`）。
    ///
    /// 顺序固定：申请设备码 → `notify(DeviceCode)` → 轮询 → 兑换。事件里的 `user_code`
    /// 与兑换用的 `authorization_code`/`code_verifier` 都先过 `js_string`：服务端可能返回
    /// 数字，TS 的 `String()` 会把数字变成十进制文本，这里用同一套 JS 转换。
    /// 取消信号取自 `interaction.signal()`，贯穿申请、轮询与兑换。
    async fn login_device_code(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<OAuthCredential, AuthError> {
        let signal = interaction.signal();
        let device = start_openai_codex_device_auth(self.fetch.as_ref(), signal).await?;
        interaction.notify(&AuthEvent::DeviceCode {
            user_code: kiana_json::js_string::js_string(Some(&device.user_code)),
            verification_uri: DEVICE_VERIFICATION_URI.into(),
            open_browser_on_confirm: None,
            copy_code_on_confirm: None,
            interval_seconds: Some(device.interval_seconds),
            expires_in_seconds: Some(DEVICE_CODE_TIMEOUT_SECONDS),
        });
        let code = poll_openai_codex_device_auth(
            Arc::clone(&self.fetch),
            Arc::clone(&self.clock),
            device,
            signal,
        )
        .await?;
        let authorization_code = kiana_json::js_string::js_string(Some(&code.authorization_code));
        let code_verifier = kiana_json::js_string::js_string(Some(&code.code_verifier));
        exchange_authorization_code_for_credentials(
            self.fetch.as_ref(),
            self.clock.as_ref(),
            &authorization_code,
            &code_verifier,
            DEVICE_REDIRECT_URI,
            signal,
        )
        .await
    }

    /// 浏览器登录（`loginOpenAICodex`）。
    ///
    /// `originator` 优先取 `LoginOptions::agent_name`，缺省 `"kiana"`。回调服务器启动失败
    /// （含预取消）被忽略：流程仍会发 `AuthUrl` 并等待手动粘贴；这是有意回退，
    /// 例如端口被 Codex CLI 占用时仍能登录。`callback.close()` 在返回前无条件执行。
    async fn login_browser(
        &self,
        interaction: &dyn AuthInteraction,
        options: &LoginOptions,
    ) -> Result<OAuthCredential, AuthError> {
        let signal = interaction.signal();
        let originator = options.agent_name.as_deref().unwrap_or("kiana");
        let flow = create_authorization_flow(originator)?;
        // 端口 1455 与 Codex CLI 共享；被占用时回退到粘贴 redirect URL。
        // 启动失败会吞掉预取消的 "Login cancelled"（回退到手动粘贴）。
        let callback = start_oauth_callback_server(OAuthCallbackServerOptions::<String> {
            provider_name: "OpenAI".into(),
            host: oauth_callback_host(),
            port: 1455,
            path: "/auth/callback".into(),
            redirect_host: None,
            state: Some(flow.state.clone()),
            complete: Arc::new(|code| Box::pin(async move { Ok(code) })),
            signal: Some(signal.clone()),
            timeout_ms: None,
        })
        .await
        .ok();
        // `None` 表示没有可用的回调服务器（端口被占、地址不可用或已预取消）：后面只等
        // 手动粘贴（回调服务器缺失时直接走粘贴路径）。

        interaction.notify(&AuthEvent::AuthUrl {
            url: flow.url.clone(),
            instructions: Some("A browser window should open. Complete login to finish.".into()),
        });

        let result = async {
            let input = wait_for_callback_or_manual_input(
                interaction,
                callback.as_ref(),
                CallbackManualPrompt {
                    message:
                        "Complete login in your browser, or paste the authorization code / redirect URL here:"
                            .into(),
                    placeholder: REDIRECT_URI.into(),
                },
            )
            .await?;

            // 回调分支拿到的是 `complete` 的返回值（原样 code 字符串）；手动分支是用户
            // 粘贴的原始文本，需要在下面解析并补做 state 校验。
            let code = match input {
                CallbackOrManualInput::Callback(value) => value,
                CallbackOrManualInput::Manual(input) => {
                    let parsed = parse_authorization_input(&input);
                    // TS 用真值判断：`if (parsed.state && parsed.state !== state)`。
                    if let Some(state) = &parsed.state
                        && !state.is_empty()
                        && state != &flow.state
                    {
                        return Err(AuthError::new("State mismatch"));
                    }
                    parsed.code.unwrap_or_default()
                }
            };
            if code.is_empty() {
                return Err(AuthError::new("Missing authorization code"));
            }
            // 兑换用的 redirect_uri 必须与授权 URL 里登记的一致（字面量 REDIRECT_URI），
            // 不能用回调服务器实际监听的地址。
            exchange_authorization_code_for_credentials(
                self.fetch.as_ref(),
                self.clock.as_ref(),
                &code,
                &flow.verifier,
                REDIRECT_URI,
                signal,
            )
            .await
        }
        .await;

        // 无论成功、失败还是手动粘贴，都停止回调服务器；若结果已经 settle，`close()`
        // 不会覆盖它，只停止 accept 循环。
        if let Some(callback) = &callback {
            callback.close();
        }
        result
    }
}

/// 便捷构造（默认依赖）。
///
/// 仓库内的 provider 走 `auth/oauth/load.rs` 的模块级单例与 `load_openai_codex_oauth()`，
/// 不经过本函数；需要独立实例（而不是共享单例）的调用方可以用它。
pub fn openai_codex_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(OpenAICodexOAuth::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access_token(account_id: &str) -> String {
        let header = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            br#"{"alg":"none"}"#,
        );
        let payload = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            format!(r#"{{"https://api.openai.com/auth":{{"chatgpt_account_id":"{account_id}"}}}}"#)
                .as_bytes(),
        );
        format!("{header}.{payload}.signature")
    }

    #[test]
    fn decodes_account_id_from_standard_base64_jwt() {
        let token = access_token("account-123");
        assert_eq!(get_account_id(&token).as_deref(), Some("account-123"));
    }

    #[test]
    fn rejects_base64url_jwt_payloads() {
        // `atob` 不接受 base64url 的 `-`/`_`：TS 侧 decodeJwt 返回 null。
        let header = "eyJhbGciOiJub25lIn0";
        let payload = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"a"}}"#,
        );
        let token = format!("{header}.{payload}.sig");
        // 若 payload 恰好不含 `-`/`_`，base64url 与标准编码相同（仍可解）；这里只断言不 panic。
        let _ = get_account_id(&token);
    }

    #[test]
    fn create_state_is_32_lowercase_hex_chars() {
        let state = create_state();
        assert_eq!(state.len(), 32);
        assert!(
            state
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn authorization_flow_orders_query_params() {
        let flow = create_authorization_flow("kiana").expect("flow");
        assert!(flow.url.starts_with(
            "https://auth.openai.com/oauth/authorize?response_type=code&client_id=app_EMoamEEZ73f0CkXaXp7hrann&redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback&scope=openid+profile+email+offline_access&code_challenge="
        ));
        assert!(flow.url.contains("&code_challenge_method=S256&state="));
        assert!(flow.url.contains(
            "&id_token_add_organizations=true&codex_cli_simplified_flow=true&originator=kiana"
        ));
    }
}
