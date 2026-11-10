//! auth/oauth/radius.ts——Radius 网关 OAuth 流程。
//!
//! Radius 是 kiana-messages 网关。OAuth 客户端 API 全在配置的 gateway 上；只有交互式浏览器的
//! authorization endpoint 需要 discovery。回调端口固定 1456（不回落），交换在服务器内完成
//! （页面可显示失败）。
//!
//! TS: `src/auth/oauth/radius.ts`
//!
//! # 设计原因与执行流程
//!
//! Radius 把 OAuth 客户端 API 全部放在网关上：设备码（`/v1/oauth/device`）、token 交换与刷新
//! （`/v1/oauth/token`）都只依赖 gateway；只有浏览器授权码流程需要先访问 `/v1/oauth` 取
//! `authorizationEndpoint`。本文件按这个边界拆成三个网络函数（discovery/token/device）、一个
//! 错误体解析辅助与两个登录编排函数；[`RadiusOAuth`] 保存展示名、规范化后的 gateway，以及
//! 注入的 `fetch`/`clock`（传输与时钟），端点一律由 `resolve_url` 以 `/v1/...` 相对路径拼出。
//!
//! 浏览器方式的调用顺序：
//! 1. [`OAuthAuth::login`] 用 select 提示选择 `browser`；
//! 2. `load_radius_oauth_discovery` 取授权端点；
//! 3. `login_with_browser` 生成 PKCE(S256) 与随机 `state`，在 `127.0.0.1:1456` 启动回调服务器，
//!    再用 [`AuthEvent::Progress`] 与 [`AuthEvent::AuthUrl`] 把监听地址与授权 URL 交给宿主；
//! 4. 浏览器回调到达后，服务器内的 `complete` 闭包用授权码向 `/v1/oauth/token` 交换凭证
//!    （失败信息显示在浏览器页面），`wait()` 把凭证交回登录流程。
//!
//! 设备码方式按 RFC 8628：先从 `/v1/oauth/device` 取码并发出 [`AuthEvent::DeviceCode`]，随后
//! 立即轮询 `/v1/oauth/token`（`wait_before_first_poll: false`）。
//!
//! # 注意事项
//!
//! - 本文件是 TS 的逐行移植，JS 真值判断与 `ToNumber` 语义是有意保留的：`expires_in` 非数字、
//!   `scope` 非字符串、错误体里的空串字段都按 TS 原样处理；改动这些分支会破坏字节级对齐
//!   （文件末尾的单测锁定了其中大部分）。
//! - 取消统一走 [`AbortScope`]，但改写为 `"Login cancelled"` 只发生在 token/device 请求：
//!   `request_oauth_token` 与 `request_device_authorization` 在 fetch 抛错后检查
//!   `signal.is_aborted()`，已中止则改写。discovery（`load_radius_oauth_discovery`）不检查，
//!   fetch 错误原样冒泡——默认实现在取消时返回 `AbortError`、超时返回 `TimeoutError`，
//!   因此浏览器登录在 discovery 阶段取消得到的是这两类错误而不是 `"Login cancelled"`；
//!   设备码轮询的 sleep 也可取消（见 [`crate::auth::oauth::device_code`]）。
//! - 浏览器方式的 host/端口固定为 `127.0.0.1:1456`，不读 `KIANA_OAUTH_CALLBACK_HOST`，
//!   也不会在端口被占用时换端口：启动会直接失败（TS 为 `EADDRINUSE`），并行浏览器登录会冲突。
//! - 错误形状：token 端点（授权码交换、刷新、设备码轮询）使用私有的 `RadiusRequestError`
//!   携带 `status` 与 `oauthError`，设备码轮询靠 `oauthError` 区分 pending/slow_down/
//!   expired/denied；`/v1/oauth/device` 的非 2xx 只返回 `.auth`，`status`/`oauthError` 被丢弃。
//! - 对调用方的错误面：`read_oauth_response_error` 生成的非 2xx 错误确实只暴露拼接后的
//!   message（`name` 为 `"Error"`）；但并非所有路径的 name 都是 `"Error"`：fetch 错误保留
//!   `TypeError`/`TimeoutError`/`AbortError`，响应体 JSON 解析失败是 `SyntaxError`，
//!   token 成功响应为 JSON `null` 时是 `TypeError`；`refresh` 的
//!   `map_err(|error| error.auth)` 原样交出这些 name（`tests/radius_oauth.rs` 断言
//!   `name == "TypeError"`）。

use std::sync::Arc;

use serde_json::Value;

use crate::auth::oauth::callback_server::{
    OAuthCallbackServerOptions, start_oauth_callback_server,
};
use crate::auth::oauth::device_code::{
    AuthClock, OAuthDeviceCodePollOptions, OAuthDeviceCodePollResult, SystemAuthClock,
    poll_oauth_device_code_flow_with_clock,
};
use crate::auth::oauth::pkce::generate_pkce;
use crate::auth::oauth::{
    OAuthFetch, OAuthFetchRequest, default_oauth_fetch, form_urlencode, resolve_url,
    url_with_search,
};
use crate::auth::types::{
    AuthError, AuthEvent, AuthInteraction, AuthPrompt, AuthPromptRequest, AuthSelectOption,
    LoginOptions, ModelAuth, OAuthAuth, OAuthCredential,
};
use crate::providers::radius_config::normalize_radius_gateway_url;
use crate::utils::abort::AbortScope;

/// `RadiusOAuthOptions`：`{ name, gateway }`。
///
/// 由宿主/配置层构造后交给 [`create_radius_oauth`]。`gateway` 允许不带 scheme 或带尾随 `/`，
/// 真正发请求前会在 [`RadiusOAuth::new`] 里规范化；`name` 不做校验或 trim，只用于展示与
/// 错误文案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RadiusOAuthOptions {
    /// 展示名（也用于 select/prompt 文案与错误消息）。
    pub name: String,
    /// 网关地址（构造时经 [`normalize_radius_gateway_url`] 规范化）。
    /// 规则：无 `http(s)://` 前缀时补 `https://`，并去掉全部尾随 `/`。
    pub gateway: String,
}

/// `CALLBACK_HOST`（固定，不读 `KIANA_OAUTH_CALLBACK_HOST`）。
const CALLBACK_HOST: &str = "127.0.0.1";
/// `CALLBACK_PORT`。
const CALLBACK_PORT: u16 = 1456;
/// `CALLBACK_PATH`。
const CALLBACK_PATH: &str = "/oauth/callback";
/// `REDIRECT_URI`。
const REDIRECT_URI: &str = "http://127.0.0.1:1456/oauth/callback";
/// `TOKEN_EXPIRY_SKEW_MS`。
const TOKEN_EXPIRY_SKEW_MS: f64 = 60_000.0;
/// `LOGIN_METHOD_BROWSER`。
const LOGIN_METHOD_BROWSER: &str = "browser";
/// `LOGIN_METHOD_DEVICE_CODE`（连字符）。
const LOGIN_METHOD_DEVICE_CODE: &str = "device-code";
/// `OAUTH_CLIENT_ID`。
const OAUTH_CLIENT_ID: &str = "kiana-gateway";
/// `OAUTH_SCOPE`。
const OAUTH_SCOPE: &str = "gateway offline_access";
/// `OAUTH_DEVICE_CODE_GRANT_TYPE`。
const OAUTH_DEVICE_CODE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// `RadiusOAuthDiscovery`。
///
/// discovery 响应里唯一被使用的字段；`issuer` 等其他键被忽略。`authorizationEndpoint` 缺失
/// 或不是字符串时整个 discovery 失败，而不是回退到某个默认地址。
struct RadiusOAuthDiscovery {
    authorization_endpoint: String,
}

/// `OAuthResponseError` 在 Rust 侧的投影：保留 `status` 与 `oauthError` 供设备轮询分支，
/// `auth` 是已经拼好的可观察错误（`name` 保持 `"Error"`）。
///
/// 不让 `request_oauth_token` 直接返回 [`AuthError`] 的原因：设备码轮询需要根据
/// `oauth_error`（RFC 8628 的 `authorization_pending`、`slow_down` 等）决定继续等待还是
/// 结束，而用户可见文案已经提前拼好。`status` 目前只被单测断言，生产路径不读它
/// （字段上的 `#[allow(dead_code)]` 即由此而来）。
struct RadiusRequestError {
    auth: AuthError,
    /// `error.status`。
    #[allow(dead_code)]
    status: u16,
    /// `error.oauthError`。
    oauth_error: Option<String>,
}

impl RadiusRequestError {
    /// 无 HTTP 响应可读时的包装（URL 解析失败、传输失败、成功响应解析失败）：
    /// `status = 0`、无 `oauth_error`，因此设备轮询会直接以 `auth` 失败而不是分类。
    fn other(error: AuthError) -> Self {
        RadiusRequestError {
            auth: error,
            status: 0,
            oauth_error: None,
        }
    }
}

/// `DeviceAuthorizationResponse`。
///
/// 对应 `/v1/oauth/device` 的成功响应：字段保留原始 JSON 值，因为 TS 直接把它们放进事件与
/// 下一次表单，只在轮询器里才做数值运算。
///
/// TS 只做真值检查后**原样**转发这些字段（`URLSearchParams` 负责 `String()` 转换），
/// 因此字段保留原始 JSON 值；`expires_in` 只有 `typeof === "number"` 时才作为有效期
/// （非 number 的真值在轮询里等价于“无 deadline”）。`interval` 原样保留，轮询器用
/// `(interval ?? 5) * 1000` 的 JS 数值语义（`"2"` → 2000ms、`true` → 1000ms）。
#[derive(Debug, Clone)]
struct DeviceAuthorizationResponse {
    device_code: Value,
    user_code: Value,
    verification_uri: Value,
    expires_in: Option<f64>,
    interval: Option<Value>,
}

/// `loadRadiusOAuthDiscovery(gateway, signal)`。
///
/// `GET {gateway}/v1/oauth`，只读取 `authorizationEndpoint`。非 2xx 的错误文案包含状态码与
/// 响应体原文；响应体不是合法 JSON 时 `json()` 的 `SyntaxError` 直接冒泡（TS 同样没有
/// try/catch）。只有浏览器登录分支调用它，设备码与刷新都不做 discovery。
async fn load_radius_oauth_discovery(
    fetch: &dyn OAuthFetch,
    gateway: &str,
    signal: &AbortScope,
) -> Result<RadiusOAuthDiscovery, AuthError> {
    let url = resolve_url(gateway, "/v1/oauth")?;
    let request = OAuthFetchRequest::new(url.as_str(), signal).header("accept", "application/json");
    let response = fetch.fetch(request).await?;
    if !response.ok() {
        return Err(AuthError::new(format!(
            "Could not load Radius OAuth config from {gateway}: {} {}",
            response.status,
            response.text()
        )));
    }
    let discovery = response.json()?;
    // TS：`discovery.authorizationEndpoint`，null 会抛 TypeError（在 try/catch 之外冒泡）。
    if discovery.is_null() {
        return Err(AuthError::with_name(
            "TypeError",
            "Cannot read properties of null (reading 'authorizationEndpoint')",
        ));
    }
    // 字段缺失或非字符串统一报 Invalid config；TS 的判断是 `typeof !== "string"`，
    // 因此数字、布尔等同样落到这里。
    let Some(authorization_endpoint) =
        crate::auth::oauth::object_field(&discovery, "authorizationEndpoint")
            .and_then(Value::as_str)
    else {
        return Err(AuthError::new(format!(
            "Invalid Radius OAuth config from {gateway}"
        )));
    };
    Ok(RadiusOAuthDiscovery {
        authorization_endpoint: authorization_endpoint.to_string(),
    })
}

/// `readOAuthResponseError(response, message)`。
///
/// 把非 2xx 响应体解析成 `RadiusRequestError`：
/// - `oauth_error` 只取字符串形式的 `error`（空串也保留在字段里；文案拼接按真值忽略它，
///   设备轮询则因匹配不到状态码而直接失败）；
/// - 用户可见文案按 TS 的三元规则拼接：`error: description` → `error` → `description` →
///   状态码；描述来自 `error_description`，JSON 解析失败或解析结果为 `null` 时改用整个
///   原始文本（`"null"`）。
///
/// `response.text()` 不会失败（响应体已在 fetch 层完整缓冲），因此没有 TS 的
/// `.catch(() => "")` 分支。
async fn read_oauth_response_error(
    response: &crate::auth::oauth::OAuthFetchResponse,
    message: &str,
) -> RadiusRequestError {
    let text = response.text();
    let mut oauth_error: Option<String> = None;
    let mut description: Option<String> = None;
    if !text.is_empty() {
        match kiana_json::js_parse::parse(&text) {
            // TS 的 `data.error` 在 `JSON.parse` 得到 null 时抛 TypeError，被同一个
            // try/catch 捕获 → description = 原始文本（"null"）。
            Ok(Value::Null) => description = Some(text),
            Ok(data) => {
                oauth_error = crate::auth::oauth::object_field(&data, "error")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                description = crate::auth::oauth::object_field(&data, "error_description")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            Err(_) => description = Some(text),
        }
    }
    // TS 的三元都用真值判断：空串 `oauthError`/`description` 等于“没有”。
    let detail = match oauth_error.as_deref().filter(|value| !value.is_empty()) {
        Some(oauth_error) => match description.as_deref().filter(|value| !value.is_empty()) {
            Some(description) => format!("{oauth_error}: {description}"),
            None => oauth_error.to_string(),
        },
        None => description
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| response.status.to_string()),
    };
    RadiusRequestError {
        auth: AuthError::new(format!("{message}: {detail}")),
        status: response.status,
        oauth_error,
    }
}

/// `requestOAuthToken(gateway, body, signal)`。
///
/// `POST {gateway}/v1/oauth/token`（表单编码），成功时返回带 `expires` 的凭证：
/// `expires = now + expires_in * 1000 - 60000`。
///
/// 成功响应不做字段校验：`access_token`/`refresh_token` 缺失或非字符串时回落为
/// 空串，`expires_in` 按数字转换（非法值得到 NaN）。差别在于 Rust 的
/// [`OAuthCredential`] 字段不可省略，而 TS 的凭证字面量会带上 `undefined`（序列化时被省略）；
/// 只有畸形响应才能观察到这一差异。
///
/// 失败路径：传输错误（中止时改写为 `"Login cancelled"`）、非 2xx（交给
/// `read_oauth_response_error`）、以及成功响应不是合法 JSON / 为 `null`（`SyntaxError`/
/// `TypeError` 投影，原样冒泡）。
async fn request_oauth_token(
    fetch: &dyn OAuthFetch,
    clock: &dyn AuthClock,
    gateway: &str,
    body: &[(&str, &str)],
    signal: &AbortScope,
) -> Result<OAuthCredential, RadiusRequestError> {
    let url = resolve_url(gateway, "/v1/oauth/token").map_err(RadiusRequestError::other)?;
    let request = OAuthFetchRequest::new(url.as_str(), signal)
        .header("accept", "application/json")
        .header("content-type", "application/x-www-form-urlencoded")
        .post(form_urlencode(body).into_bytes());
    let response = match fetch.fetch(request).await {
        Ok(response) => response,
        Err(error) => {
            if signal.is_aborted() {
                return Err(RadiusRequestError::other(AuthError::new("Login cancelled")));
            }
            return Err(RadiusRequestError::other(error));
        }
    };
    if !response.ok() {
        return Err(
            read_oauth_response_error(&response, "Radius OAuth token request failed").await,
        );
    }
    let data = response.json().map_err(RadiusRequestError::other)?;
    // TS：`data.access_token`，`JSON.parse` 得到 null 时抛 TypeError（无字段校验）。
    if data.is_null() {
        return Err(RadiusRequestError::other(AuthError::with_name(
            "TypeError",
            "Cannot read properties of null (reading 'access_token')",
        )));
    }
    let access = crate::auth::oauth::object_field(&data, "access_token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let refresh = crate::auth::oauth::object_field(&data, "refresh_token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // TS 不校验 `expires_in` 的类型，直接做 `data.expires_in * 1000`（JS `ToNumber`）。
    let expires_in = crate::auth::oauth::js_to_number_or_nan(crate::auth::oauth::object_field(
        &data,
        "expires_in",
    ));
    let mut credential = OAuthCredential::new(
        access,
        refresh,
        clock.now_ms() + expires_in * 1000.0 - TOKEN_EXPIRY_SKEW_MS,
    );
    // TS 的凭证字面量无条件写入 `scope: data.scope`：键存在时保留原始 JSON 值
    // （非字符串、`null` 也一样）；键不存在时 `undefined` 被 `JSON.stringify` 省略。
    if let Some(scope) = crate::auth::oauth::object_field(&data, "scope") {
        credential.extra.insert("scope".into(), scope.clone());
    }
    Ok(credential)
}

/// `loginWithBrowser(gateway, authorizationEndpoint, interaction)`。
///
/// 授权码 + PKCE(S256) 流程：生成 verifier/challenge 与随机 `state`，在固定回环地址启动回调
/// 服务器，再用事件把监听地址与授权 URL 交给宿主。`complete` 闭包在服务器写响应页之前完成
/// token 交换（失败信息显示在浏览器里），因此 `wait()` 返回的是已经拿到的凭证。
///
/// `callback.close()` 在 `wait()` 结束后无条件执行（成功或失败都走同一出口），确保监听
/// 端口被释放；`wait()` 返回 `None` 的分支在正常流程里不可达（没有调用 `cancel()` 的路径），
/// 保留它只为对齐 TS 的 `if (!credential)` 检查。
async fn login_with_browser(
    fetch: Arc<dyn OAuthFetch>,
    clock: Arc<dyn AuthClock>,
    gateway: &str,
    authorization_endpoint: &str,
    interaction: &dyn AuthInteraction,
) -> Result<OAuthCredential, AuthError> {
    let signal = interaction.signal();
    let pkce = generate_pkce();
    let state = uuid::Uuid::new_v4().to_string();
    // 字段与顺序逐字对齐 TS 的 `URLSearchParams`（会覆盖 authorizationEndpoint 已有的 query）；
    // `handoff=url` 是 TS 固定的参数，原样保留。
    let authorize_url = url_with_search(
        authorization_endpoint,
        &[
            ("response_type", "code"),
            ("client_id", OAUTH_CLIENT_ID),
            ("redirect_uri", REDIRECT_URI),
            ("scope", OAUTH_SCOPE),
            ("code_challenge", &pkce.challenge),
            ("code_challenge_method", "S256"),
            ("handoff", "url"),
            ("state", &state),
        ],
    )?;

    let gateway_owned = gateway.to_string();
    let verifier = pkce.verifier.clone();
    let complete_signal = signal.clone();
    // 端口固定 1456 且不设 timeout（TS 未传 timeout）：被占用时启动直接失败，不会换端口。
    // complete 闭包只把交换失败映射成 `error.auth`；`status`/`oauthError` 只用于设备码轮询。
    let callback = start_oauth_callback_server(OAuthCallbackServerOptions::<OAuthCredential> {
        provider_name: "Radius".into(),
        host: CALLBACK_HOST.into(),
        port: CALLBACK_PORT,
        path: CALLBACK_PATH.into(),
        redirect_host: None,
        state: Some(state),
        complete: Arc::new(move |code| {
            let fetch = Arc::clone(&fetch);
            let clock = Arc::clone(&clock);
            let gateway = gateway_owned.clone();
            let verifier = verifier.clone();
            let signal = complete_signal.clone();
            Box::pin(async move {
                request_oauth_token(
                    fetch.as_ref(),
                    clock.as_ref(),
                    &gateway,
                    &[
                        ("grant_type", "authorization_code"),
                        ("client_id", OAUTH_CLIENT_ID),
                        ("redirect_uri", REDIRECT_URI),
                        ("code", &code),
                        ("code_verifier", &verifier),
                    ],
                    &signal,
                )
                .await
                .map_err(|error| error.auth)
            })
        }),
        signal: Some(signal.clone()),
        timeout_ms: None,
    })
    .await?;

    // 事件顺序是 TS 的可见行为：先报监听地址（Progress），再给授权 URL（AuthUrl）。
    interaction.notify(&AuthEvent::Progress {
        message: format!("Listening for OAuth callback on {REDIRECT_URI}"),
    });
    interaction.notify(&AuthEvent::AuthUrl {
        url: authorize_url,
        instructions: Some("Continue in your browser.".into()),
    });

    // 与 TS 的 try/finally 等价：无论 wait 成功还是失败，随后都 close。
    let result = async {
        let credential = callback.wait().await?;
        match credential {
            // 正常流程不会返回 None；保留该分支以对齐 TS 的 `if (!credential)`。
            Some(credential) => Ok(credential),
            None => Err(AuthError::new("OAuth callback did not complete.")),
        }
    }
    .await;
    callback.close();
    result
}

/// `requestDeviceAuthorization(gateway, signal)`。
///
/// `POST {gateway}/v1/oauth/device`，表单只有 `client_id` 与 `scope`。成功响应按 TS 的真值
/// 检查要求 `device_code`、`user_code`、`verification_uri`、`expires_in` 全部为真值；字段值
/// 保留原始 JSON（不强制字符串），因为 TS 之后直接把它们交给事件与表单。
///
/// `expires_in` 只有 JSON number 才作为 deadline；其他真值（字符串、布尔、数组…）表示
/// “没有 deadline”，轮询会一直持续到成功、失败或取消。
async fn request_device_authorization(
    fetch: &dyn OAuthFetch,
    gateway: &str,
    signal: &AbortScope,
) -> Result<DeviceAuthorizationResponse, AuthError> {
    let url = resolve_url(gateway, "/v1/oauth/device")?;
    let request = OAuthFetchRequest::new(url.as_str(), signal)
        .header("accept", "application/json")
        .header("content-type", "application/x-www-form-urlencoded")
        .post(
            form_urlencode(&[("client_id", OAUTH_CLIENT_ID), ("scope", OAUTH_SCOPE)]).into_bytes(),
        );
    let response = match fetch.fetch(request).await {
        Ok(response) => response,
        Err(error) => {
            if signal.is_aborted() {
                return Err(AuthError::new("Login cancelled"));
            }
            return Err(error);
        }
    };
    if !response.ok() {
        return Err(read_oauth_response_error(
            &response,
            "Radius OAuth device authorization failed",
        )
        .await
        .auth);
    }
    let data = response.json()?;
    // TS：`data.device_code`，`JSON.parse` 得到 null 时抛 TypeError。
    if data.is_null() {
        return Err(AuthError::with_name(
            "TypeError",
            "Cannot read properties of null (reading 'device_code')",
        ));
    }
    // 统一用对象属性访问：非对象 body（数组/字符串）在 TS 里同样取不到这些字段。
    let field = |name: &str| crate::auth::oauth::object_field(&data, name);
    // TS 只做真值检查（`!data.device_code`），非字符串的真值也会被接受并原样转发。
    let truthy = |name: &str| field(name).filter(|value| kiana_json::is_truthy(Some(value)));
    let (Some(device_code), Some(user_code), Some(verification_uri)) = (
        truthy("device_code"),
        truthy("user_code"),
        truthy("verification_uri"),
    ) else {
        return Err(AuthError::new(
            "Radius OAuth device authorization response is missing required fields",
        ));
    };
    let Some(expires_in_value) = truthy("expires_in") else {
        return Err(AuthError::new(
            "Radius OAuth device authorization response is missing required fields",
        ));
    };
    // `typeof expiresInSeconds === "number"` 才作为 deadline；其余真值 → 无 deadline。
    let expires_in = if matches!(expires_in_value, Value::Number(_)) {
        expires_in_value.as_f64()
    } else {
        None
    };
    Ok(DeviceAuthorizationResponse {
        device_code: device_code.clone(),
        user_code: user_code.clone(),
        verification_uri: verification_uri.clone(),
        expires_in,
        // `interval ?? 5`：只有 null/undefined 走默认值，其余原样交给 JS `ToNumber`。
        interval: field("interval").filter(|value| !value.is_null()).cloned(),
    })
}

/// `loginWithDeviceCode(gateway, interaction)`。
///
/// 设备授权 → [`AuthEvent::DeviceCode`] → 轮询。与 xAI 不同，这里
/// `wait_before_first_poll: false`：拿到设备码后立即发第一次 token 请求。
///
/// 轮询回调把 `RadiusRequestError.oauth_error` 映射成共享状态机的 pending/slow_down/
/// failed；没有 `oauth_error`（传输错误、非 JSON 错误体…）或未知错误码时以 `Err` 结束登录。
async fn login_with_device_code(
    fetch: Arc<dyn OAuthFetch>,
    clock: Arc<dyn AuthClock>,
    gateway: &str,
    interaction: &dyn AuthInteraction,
) -> Result<OAuthCredential, AuthError> {
    let signal = interaction.signal();
    let device = request_device_authorization(fetch.as_ref(), gateway, signal).await?;
    // TS 把原始值放进事件与轮询器；轮询器做 `(interval ?? 5) * 1000` 的 JS 数值运算。
    let interval_seconds = device
        .interval
        .as_ref()
        .map(crate::auth::oauth::js_to_number);
    interaction.notify(&AuthEvent::DeviceCode {
        user_code: kiana_json::js_string::js_string(Some(&device.user_code)),
        verification_uri: kiana_json::js_string::js_string(Some(&device.verification_uri)),
        open_browser_on_confirm: None,
        copy_code_on_confirm: None,
        interval_seconds,
        expires_in_seconds: device.expires_in,
    });

    // 轮询闭包要求 `'static`，因此每次调用都重新 clone Arc 与设备码。
    let signal = signal.clone();
    let poll = {
        let fetch = Arc::clone(&fetch);
        let clock = Arc::clone(&clock);
        let gateway = gateway.to_string();
        let device = device.clone();
        let signal = signal.clone();
        move || -> crate::auth::oauth::device_code::OAuthDeviceCodePollFuture<OAuthCredential> {
            let fetch = Arc::clone(&fetch);
            let clock = Arc::clone(&clock);
            let gateway = gateway.clone();
            let device = device.clone();
            let signal = signal.clone();
            Box::pin(async move {
                match request_oauth_token(
                    fetch.as_ref(),
                    clock.as_ref(),
                    &gateway,
                    &[
                        ("grant_type", OAUTH_DEVICE_CODE_GRANT_TYPE),
                        ("client_id", OAUTH_CLIENT_ID),
                        (
                            "device_code",
                            &kiana_json::js_string::js_string(Some(&device.device_code)),
                        ),
                    ],
                    &signal,
                )
                .await
                {
                    Ok(credentials) => {
                        Ok(OAuthDeviceCodePollResult::Complete { value: credentials })
                    }
                    Err(error) => {
                        let Some(oauth_error) = error.oauth_error.as_deref() else {
                            return Err(error.auth);
                        };
                        match oauth_error {
                            "authorization_pending" => Ok(OAuthDeviceCodePollResult::Pending),
                            // Radius 不返回新的 interval，轮询器按 RFC 8628 的规则 +5s。
                            "slow_down" => Ok(OAuthDeviceCodePollResult::SlowDown {
                                interval_seconds: None,
                            }),
                            "expired_token" => Ok(OAuthDeviceCodePollResult::Failed {
                                message: "Device authorization expired.".into(),
                            }),
                            "access_denied" => Ok(OAuthDeviceCodePollResult::Failed {
                                message: "Device authorization was denied.".into(),
                            }),
                            _ => Err(error.auth),
                        }
                    }
                }
            })
        }
    };

    // `expires_in` 为 None 表示没有 deadline；轮询器只在 deadline 有限时才超时。
    poll_oauth_device_code_flow_with_clock(
        OAuthDeviceCodePollOptions {
            interval_seconds,
            expires_in_seconds: device.expires_in,
            wait_before_first_poll: false,
            poll: Arc::new(poll),
            signal,
        },
        clock.as_ref(),
    )
    .await
}

/// Radius OAuth 流程。
///
/// 所有状态在构造时固定：规范化后的 gateway、展示名，以及可注入的 `fetch`/`clock`。
/// 类型本身没有内部可变状态，可以放进 `Arc` 被多处共享。
pub struct RadiusOAuth {
    name: String,
    gateway: String,
    fetch: Arc<dyn OAuthFetch>,
    clock: Arc<dyn AuthClock>,
}

impl RadiusOAuth {
    /// 用选项构造（默认依赖）。
    ///
    /// `gateway` 会经 [`normalize_radius_gateway_url`] 规范化（无 `http(s)://` 时补
    /// `https://`，去掉全部尾随 `/`），`name` 原样保留。`fetch` 使用 `default_oauth_fetch()`
    /// （测试可全局替换），时钟为 [`SystemAuthClock`]。
    pub fn new(options: RadiusOAuthOptions) -> Self {
        RadiusOAuth {
            name: options.name,
            gateway: normalize_radius_gateway_url(&options.gateway),
            fetch: default_oauth_fetch(),
            clock: Arc::new(SystemAuthClock),
        }
    }

    /// 注入依赖（测试用）。
    ///
    /// 注入的 `fetch` 覆盖所有请求（discovery/token/device），`clock.now_ms()` 是凭证
    /// `expires` 的基准时间；生产路径请用 [`new`](Self::new)。
    pub fn with_dependencies(
        options: RadiusOAuthOptions,
        fetch: Arc<dyn OAuthFetch>,
        clock: Arc<dyn AuthClock>,
    ) -> Self {
        RadiusOAuth {
            name: options.name,
            gateway: normalize_radius_gateway_url(&options.gateway),
            fetch,
            clock,
        }
    }

    /// 规范化后的 gateway。
    ///
    /// 所有端点都以它为 base 拼接：`/v1/oauth`、`/v1/oauth/token`、`/v1/oauth/device`。
    pub fn gateway(&self) -> &str {
        &self.gateway
    }
}

#[async_trait::async_trait]
impl OAuthAuth for RadiusOAuth {
    /// 构造时传入的展示名（未做任何转换）。
    fn name(&self) -> &str {
        &self.name
    }

    /// 先提示选择登录方式，再分派到浏览器或设备码流程。
    ///
    /// 返回的选择只要不是 `"device-code"`/`"browser"`（提示本身只提供这两个 id）就走
    /// `Unknown <name> sign-in method: <value>`，与 TS 的兜底分支一致。
    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
        _options: &LoginOptions,
    ) -> Result<OAuthCredential, AuthError> {
        let login_method = interaction
            .prompt(AuthPromptRequest::new(AuthPrompt::Select {
                message: format!("Sign in to {}:", self.name),
                options: vec![
                    AuthSelectOption {
                        id: LOGIN_METHOD_BROWSER.into(),
                        label: "Sign in with browser (recommended)".into(),
                        description: None,
                    },
                    AuthSelectOption {
                        id: LOGIN_METHOD_DEVICE_CODE.into(),
                        label: "Sign in with device code (when signing in from another device)"
                            .into(),
                        description: None,
                    },
                ],
            }))
            .await?;

        if login_method == LOGIN_METHOD_DEVICE_CODE {
            return login_with_device_code(
                Arc::clone(&self.fetch),
                Arc::clone(&self.clock),
                &self.gateway,
                interaction,
            )
            .await;
        }
        if login_method == LOGIN_METHOD_BROWSER {
            let discovery = load_radius_oauth_discovery(
                self.fetch.as_ref(),
                &self.gateway,
                interaction.signal(),
            )
            .await?;
            return login_with_browser(
                Arc::clone(&self.fetch),
                Arc::clone(&self.clock),
                &self.gateway,
                &discovery.authorization_endpoint,
                interaction,
            )
            .await;
        }
        Err(AuthError::new(format!(
            "Unknown {} sign-in method: {login_method}",
            self.name
        )))
    }

    /// 直接用 refresh token 换新凭证；不调用 discovery（刷新只依赖 gateway）。
    ///
    /// 失败时只返回 `error.auth`，`status`/`oauthError` 不对外暴露。
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: &AbortScope,
    ) -> Result<OAuthCredential, AuthError> {
        request_oauth_token(
            self.fetch.as_ref(),
            self.clock.as_ref(),
            &self.gateway,
            &[
                ("grant_type", "refresh_token"),
                ("client_id", OAUTH_CLIENT_ID),
                ("refresh_token", &credential.refresh),
            ],
            signal,
        )
        .await
        .map_err(|error| error.auth)
    }

    /// 把 `access` 当作 API key；不设置 `baseUrl`/`headers`。
    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..ModelAuth::default()
        })
    }
}

/// `createRadiusOAuth(options)`。
///
/// 返回 `Arc<dyn OAuthAuth>`；`load` 的加载表与 `bun_oauth` 的登记都用它构造实际流程，
/// gateway 规范化发生在 [`RadiusOAuth::new`]。
pub fn create_radius_oauth(options: RadiusOAuthOptions) -> Arc<dyn OAuthAuth> {
    Arc::new(RadiusOAuth::new(options))
}

/// 带注入依赖的 [`create_radius_oauth`]（测试用）。
///
/// 所有网络调用与过期时间都走注入依赖；生产路径用 [`create_radius_oauth`]。
pub fn create_radius_oauth_with(
    options: RadiusOAuthOptions,
    fetch: Arc<dyn OAuthFetch>,
    clock: Arc<dyn AuthClock>,
) -> Arc<dyn OAuthAuth> {
    Arc::new(RadiusOAuth::with_dependencies(options, fetch, clock))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_gateway_is_used_for_all_endpoints() {
        let oauth = RadiusOAuth::new(RadiusOAuthOptions {
            name: "Radius".into(),
            gateway: "radius.example///".into(),
        });
        assert_eq!(oauth.gateway(), "https://radius.example");
        assert_eq!(oauth.name(), "Radius");
        assert!(!oauth.is_subscription());
        assert_eq!(oauth.login_label(), None);
    }

    #[test]
    fn oauth_response_errors_keep_status_and_code() {
        let response = crate::auth::oauth::OAuthFetchResponse::text_response(
            403,
            r#"{"error":"invalid_grant","error_description":"code expired"}"#,
        );
        let error = futures::executor::block_on(read_oauth_response_error(
            &response,
            "Radius OAuth token request failed",
        ));
        assert_eq!(
            error.auth.message,
            "Radius OAuth token request failed: invalid_grant: code expired"
        );
        assert_eq!(error.oauth_error.as_deref(), Some("invalid_grant"));
        assert_eq!(error.status, 403);
    }

    #[test]
    fn non_json_error_bodies_become_the_description() {
        let response = crate::auth::oauth::OAuthFetchResponse::text_response(500, "boom");
        let error = futures::executor::block_on(read_oauth_response_error(
            &response,
            "Radius OAuth token request failed",
        ));
        assert_eq!(
            error.auth.message,
            "Radius OAuth token request failed: boom"
        );
        assert_eq!(error.oauth_error, None);
    }

    /// TS 的凭证字面量无条件写入 `scope: data.scope`，因此 `scope` 的任何 JSON 值
    /// （不只是字符串）都保留在凭证里。
    #[tokio::test]
    async fn token_responses_keep_non_string_scope_values() {
        let fetch = crate::auth::oauth::testing::MockFetch::new(|_request| {
            Box::pin(async {
                Ok(crate::auth::oauth::OAuthFetchResponse::json_response(
                    200,
                    &serde_json::json!({
                        "access_token": "access",
                        "refresh_token": "refresh",
                        "expires_in": 3600,
                        "scope": 5,
                    }),
                ))
            })
        });
        let clock = crate::auth::oauth::testing::VirtualClock::new(0.0);
        let credential = match request_oauth_token(
            &*fetch,
            &*clock,
            "https://radius.example",
            &[],
            &AbortScope::new(),
        )
        .await
        {
            Ok(credential) => credential,
            Err(error) => panic!("token request failed: {}", error.auth.message),
        };
        assert_eq!(credential.extra.get("scope"), Some(&serde_json::json!(5)));
    }

    /// 回归（评审发现）：TS 用真值判断 `oauthError`/`description`，空串等于“没有”：
    /// `{"error_description":""}` → 只用 status；`{"error":"invalid_grant",…:""}` → 只有
    /// error；空 `error` 回落到 description。
    #[test]
    fn empty_error_fields_are_falsy_in_oauth_response_errors() {
        let cases = [
            (
                r#"{"error_description":""}"#,
                "Radius OAuth token request failed: 400",
            ),
            (
                r#"{"error":"invalid_grant","error_description":""}"#,
                "Radius OAuth token request failed: invalid_grant",
            ),
            (
                r#"{"error":"","error_description":"desc"}"#,
                "Radius OAuth token request failed: desc",
            ),
            (
                r#"{"error":5,"error_description":"desc"}"#,
                "Radius OAuth token request failed: desc",
            ),
            (
                r#"{"error":"invalid_grant","error_description":5}"#,
                "Radius OAuth token request failed: invalid_grant",
            ),
            (r#"{}"#, "Radius OAuth token request failed: 400"),
            ("", "Radius OAuth token request failed: 400"),
        ];
        for (body, expected) in cases {
            let response = crate::auth::oauth::OAuthFetchResponse::text_response(400, body);
            let error = futures::executor::block_on(read_oauth_response_error(
                &response,
                "Radius OAuth token request failed",
            ));
            assert_eq!(error.auth.message, expected, "body {body:?}");
        }
    }

    /// 回归（评审发现）：`expires_in` 不校验类型，`data.expires_in * 1000` 用 JS `ToNumber`
    /// （字符串 `"3600"` → +3,540,000ms，`true` → -59,000ms，`null` → -60,000ms）。
    #[tokio::test(start_paused = true)]
    async fn token_responses_apply_js_numeric_coercion_to_expires_in() {
        let cases: Vec<(Value, Option<f64>)> = vec![
            (serde_json::json!("3600"), Some(3600.0)),
            (serde_json::json!(true), Some(1.0)),
            (serde_json::json!(null), Some(0.0)),
            (serde_json::json!(["7"]), Some(7.0)),
            (serde_json::json!("abc"), None),
        ];
        for (expires_in, expected_seconds) in cases {
            let fetch = crate::auth::oauth::testing::MockFetch::new(move |_request| {
                let expires_in = expires_in.clone();
                Box::pin(async move {
                    Ok(crate::auth::oauth::OAuthFetchResponse::json_response(
                        200,
                        &serde_json::json!({
                            "access_token": "access",
                            "refresh_token": "refresh",
                            "expires_in": expires_in,
                        }),
                    ))
                })
            });
            let clock = crate::auth::oauth::testing::VirtualClock::new(0.0);
            let credential = request_oauth_token(
                &*fetch,
                &*clock,
                "https://radius.example",
                &[],
                &AbortScope::new(),
            )
            .await
            .unwrap_or_else(|error| panic!("token request failed: {}", error.auth.message));
            match expected_seconds {
                Some(seconds) => {
                    assert_eq!(credential.expires, seconds * 1000.0 - TOKEN_EXPIRY_SKEW_MS)
                }
                None => assert!(credential.expires.is_nan()),
            }
        }
    }
}
