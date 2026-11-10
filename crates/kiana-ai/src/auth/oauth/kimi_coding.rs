//! auth/oauth/kimi-coding.ts——Kimi Code（subscription）OAuth。
//!
//! RFC 8628 设备授权（JSON 响应），host 可用 `KIMI_CODE_OAUTH_HOST` / `KIMI_OAUTH_HOST` 覆盖
//! （去掉全部尾随 `/`）。access token 以 `Authorization: Bearer` header 使用。
//!
//! TS: `src/auth/oauth/kimi-coding.ts`
//!
//! # 这个文件负责的边界
//!
//! Kimi Code 是订阅制（`is_subscription` 为 `true`）而不是 API key：登录走 RFC 8628 设备码，
//! 拿到的 token 以 `Authorization: Bearer` 头注入模型请求（见 `to_auth`）。本文件只实现
//! 协议与网络，凭证的持久化、何时刷新由上层 `Models` / 凭据存储决定。
//!
//! # 一次登录的执行流程
//!
//! 1. `get_oauth_host` 每次从环境变量解析 host（缺省 `https://auth.kimi.com`）；
//! 2. `POST {host}/api/oauth/device_authorization`（表单只有 `client_id`），校验返回的
//!    设备码与两个验证 URI；
//! 3. 通过 [`AuthInteraction::notify`] 发出 `device_code` 事件，URL 用
//!    `verification_uri_complete`（已带用户码，用户不必手动输入）；
//! 4. `poll_for_token` 先等一个 interval，再轮询 `POST {host}/api/oauth/token`，直到用户
//!    确认、拒绝或设备码过期；
//! 5. 成功响应换算成 [`OAuthCredential`]：`access`/`refresh` 原样保存，`expires` 为
//!    `now + expires_in * 1000`（**不带安全余量**；上层在临近过期时触发 refresh，默认提前
//!    5 分钟，比较式为 `now + 5min >= expires`，见 `auth/resolve.rs`）。
//!
//! # 错误与取消
//!
//! - 错误文案是当前稳定契约：`tests/kimi_coding_oauth.rs` 对 expired/denied/字段缺失等
//!   文本有断言，改动文案需同步测试。
//! - 轮询中的传输错误（含超时、取消）用 `?` 直接抛出，不再轮询；在等待间隙被取消时由
//!   设备码轮询器返回 `Login cancelled`（见 `auth/oauth/device_code.rs`）。本流程不像
//!   Meta 那样把错误统一改写。
//! - 刷新有独立的 4 次尝试策略：传输错误与 `429`/`5xx` 可重试，`401`/`403`/`invalid_grant`
//!   立即失败（身份已失效，只能重新登录）。
//!
//! # 注意事项
//!
//! - `KIMI_CODE_OAUTH_HOST` / `KIMI_OAUTH_HOST` 是进程级环境变量且每次都读取；测试里必须
//!   串行（`tests/kimi_coding_oauth.rs` 用 `SERIAL` 锁），否则会互相污染。
//! - 结构体持有注入的 [`OAuthFetch`] 与 [`AuthClock`]，默认依赖在**构造时**解析
//!   （`default_oauth_fetch`），测试替身必须在构造之前安装。

use std::sync::Arc;

use serde_json::Value;

use crate::auth::oauth::device_code::{
    AuthClock, OAuthDeviceCodePollOptions, OAuthDeviceCodePollResult, SystemAuthClock,
    poll_oauth_device_code_flow_with_clock,
};
use crate::auth::oauth::{
    OAuthFetch, OAuthFetchRequest, OAuthFetchResponse, default_oauth_fetch, form_urlencode,
    is_object_like,
};
use crate::auth::types::{
    AuthError, AuthEvent, AuthInteraction, LoginOptions, ModelAuth, OAuthAuth, OAuthCredential,
};
use crate::types::ProviderHeaders;
use crate::utils::abort::AbortScope;
use crate::utils::provider_env::get_provider_env_value;
use crate::utils::sleep::sleep;

/// `CLIENT_ID`。
///
/// Kimi Code CLI 的公开 OAuth client id：随每个表单请求体发送，用来让服务端识别调用方，
/// 不是密钥（编译进二进制也不构成泄露）。
const CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
/// `DEFAULT_OAUTH_HOST`。
const DEFAULT_OAUTH_HOST: &str = "https://auth.kimi.com";
/// `DEVICE_CODE_TIMEOUT_SECONDS`。
///
/// 服务端没给 `expires_in`（或给了非正数）时使用的设备码有效期（15 分钟）。
const DEVICE_CODE_TIMEOUT_SECONDS: f64 = 15.0 * 60.0;
/// `DEFAULT_POLL_INTERVAL_SECONDS`。
///
/// 服务端没给 `interval`（或给了非正数）时使用的轮询间隔。
const DEFAULT_POLL_INTERVAL_SECONDS: f64 = 5.0;
/// `REQUEST_TIMEOUT_MS`。
///
/// 单个 HTTP 请求的 30 秒上限（`AbortSignal.timeout` 的等价物）；设备授权、每次轮询与
/// 每次刷新尝试都受它约束。
const REQUEST_TIMEOUT_MS: u64 = 30_000;
/// `REFRESH_MAX_RETRIES`。
///
/// 刷新除首次之外的重试次数：循环写的是 `0..=REFRESH_MAX_RETRIES`，即**总共 4 次**尝试。
const REFRESH_MAX_RETRIES: u32 = 3;

/// `DeviceAuthorization`。
///
/// 设备授权响应的解析结果。`device_code` 是轮询用的不透明凭据，`user_code` 与
/// `verification_uri_complete` 展示给用户；两个时长在解析阶段已用默认值补齐，因此下游
/// `poll_for_token` 拿到的一定是 `Some`，不会退回轮询器默认值。
#[derive(Debug, Clone)]
struct DeviceAuthorization {
    /// 轮询用的不透明凭据（不展示给用户）。
    device_code: String,
    /// 展示给用户输入的短码。
    user_code: String,
    /// 带 `user_code` 的完整验证 URL，登录事件直接用它。
    verification_uri_complete: String,
    /// 轮询间隔（秒），保证 > 0。
    interval_seconds: f64,
    /// 设备码有效期（秒），保证 > 0。
    expires_in_seconds: f64,
}

/// `TokenResponse`。
///
/// token 端点成功响应的解析结果。`expires` 是**绝对**毫秒时间戳（与
/// [`OAuthCredential`] 的 `expires` 同一时基），不是剩余秒数。
#[derive(Debug, Clone)]
struct TokenResponse {
    /// `access_token`。
    access: String,
    /// `refresh_token`。
    refresh: String,
    /// 绝对过期时间（毫秒）。
    expires: f64,
}

/// `getOauthHost()`：`KIMI_CODE_OAUTH_HOST || KIMI_OAUTH_HOST || DEFAULT`，去掉尾部 `/`。
///
/// 每次调用都重新读环境变量（与 TS 同名函数一致），所以登录与刷新看到的是当时的值；
/// 空串按缺失处理（`get_provider_env_value` 的 `||` 语义）。掐掉全部尾随斜杠是为了避免
/// 拼出 `https://host//api/oauth/...` 这种路径。
fn get_oauth_host() -> String {
    let override_host = get_provider_env_value("KIMI_CODE_OAUTH_HOST", None)
        .or_else(|| get_provider_env_value("KIMI_OAUTH_HOST", None));
    (override_host.unwrap_or_else(|| DEFAULT_OAUTH_HOST.to_string()))
        .trim_end_matches('/')
        .to_string()
}

/// `readJson(response)`：成功且为对象/数组 → 值；否则 `None`。
///
/// 解析失败统一归一为 `None`，调用方错误消息里的 `JSON.stringify(json)` 因此是字面量
/// `null`（测试 `reports_null_for_unparseable_device_responses` 锁定）。数组也算“对象”，
/// 因为 JS 的 `typeof [] === "object"`；但后续按字段取值都会落空，由各自的错误分支处理。
fn read_json(response: &OAuthFetchResponse) -> Option<Value> {
    let json = response.json().ok()?;
    if is_object_like(&json) {
        Some(json)
    } else {
        None
    }
}

/// `trustedHttpUrl(value)`：非空字符串、`new URL` 成功且 protocol 为 http(s) → 规范化 href。
///
/// 这个 URL 来自服务端，宿主的登录界面可能直接打开它，所以必须挡住 `file:`/`javascript:`
/// 等 scheme（测试 `trusted_urls_require_http_schemes` 锁定 `file:///etc/passwd` 被拒）。
/// 返回值只在 `start_device_authorization` 里做存在性判定，事件里发出的仍是服务端原文。
fn trusted_http_url(value: Option<&Value>) -> Option<String> {
    let value = value?.as_str().filter(|value| !value.is_empty())?;
    let url = url::Url::parse(value).ok()?;
    if url.scheme() != "https" && url.scheme() != "http" {
        return None;
    }
    Some(url.into())
}

/// `formUrlEncode(fields)`。
///
/// 只是转调 [`crate::auth::oauth::form_urlencode`]：保留同名函数是为了与 TS 的
/// `formUrlEncode` 一一对应，而编码规则（空格 `+`、`%XX` 大写）在整个 oauth 子树里只
/// 维护一份差分用例。
fn form_url_encode(fields: &[(&str, &str)]) -> String {
    form_urlencode(fields)
}

/// `startDeviceAuthorization(oauthHost, signal)`。
///
/// 发起设备授权并校验响应。成功返回的 `DeviceAuthorization` 已补齐默认轮询间隔与有效期；
/// 任何字段形状不对或验证 URI 不是 http(s) 时返回带原始 JSON 的错误（解析失败时为
/// 字面量 `null`）。
async fn start_device_authorization(
    fetch: &dyn OAuthFetch,
    oauth_host: &str,
    signal: &AbortScope,
) -> Result<DeviceAuthorization, AuthError> {
    // 阶段 1：只有 client_id 的表单请求；超时由 OAuthFetchRequest::timeout_ms 统一下发，
    // 取消由请求里的 signal 传播。
    let request = OAuthFetchRequest::new(
        format!("{oauth_host}/api/oauth/device_authorization"),
        signal,
    )
    .header("Content-Type", "application/x-www-form-urlencoded")
    .header("Accept", "application/json")
    .post(form_url_encode(&[("client_id", CLIENT_ID)]).into_bytes())
    .timeout_ms(REQUEST_TIMEOUT_MS);
    let response = fetch.fetch(request).await?;
    // 阶段 2：非 2xx 时把响应体原文拼进错误；空体不加冒号，避免出现尾随的 ": "。
    if !response.ok() {
        let text = response.text();
        return Err(AuthError::new(format!(
            "Kimi Code device authorization failed with status {}{}",
            response.status,
            if text.is_empty() {
                String::new()
            } else {
                format!(": {text}")
            }
        )));
    }

    // 阶段 3：readJson 失败时 json 为 None，下面所有字段都取不到，最终统一报格式错误。
    let json = read_json(&response);
    let field = |name: &str| {
        json.as_ref()
            .and_then(|json| crate::auth::oauth::object_field(json, name))
    };
    let device_code = field("device_code");
    let user_code = field("user_code");
    let verification_uri = field("verification_uri");
    let verification_uri_complete = field("verification_uri_complete");
    let trusted_complete = trusted_http_url(verification_uri_complete);
    let trusted_uri = trusted_http_url(verification_uri);
    let (Some(device_code), Some(user_code), Some(verification_uri_complete)) = (
        device_code.filter(|value| value.is_string()),
        user_code.filter(|value| value.is_string()),
        verification_uri_complete.filter(|value| value.is_string()),
    ) else {
        return Err(invalid_device_authorization_error(json.as_ref()));
    };
    // 阶段 4：TS 要求 `verification_uri` 与 `verification_uri_complete` **都**通过
    // trustedHttpUrl；`verification_uri` 只参与这项校验，不会进入返回值。
    if trusted_complete.is_none() || trusted_uri.is_none() {
        return Err(invalid_device_authorization_error(json.as_ref()));
    }
    // 该绑定只用于上面的 `trusted_http_url(verification_uri)` 校验，不进入返回值；登录事件
    // 用的是 complete URI。校验处已经读取过它，因此 `let _` 不是为压掉未使用变量告警，
    // 只是一句冗余的空操作。
    let _ = verification_uri;
    // 阶段 5：数值字段非数字、非有限或 ≤ 0 时回落到默认值（服务端可以省略它们）。
    Ok(DeviceAuthorization {
        device_code: device_code.as_str().unwrap_or_default().to_string(),
        user_code: user_code.as_str().unwrap_or_default().to_string(),
        verification_uri_complete: verification_uri_complete
            .as_str()
            .unwrap_or_default()
            .to_string(),
        interval_seconds: field("interval")
            .and_then(|value| crate::auth::oauth::finite_number(Some(value)))
            .filter(|interval| *interval > 0.0)
            .unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS),
        expires_in_seconds: field("expires_in")
            .and_then(|value| crate::auth::oauth::finite_number(Some(value)))
            .filter(|expires_in| *expires_in > 0.0)
            .unwrap_or(DEVICE_CODE_TIMEOUT_SECONDS),
    })
}

/// `Invalid Kimi Code device authorization response: ${JSON.stringify(json)}`。
///
/// `json` 为 `None`（body 不是 JSON 或不是对象）时输出字面量 `null`，与 TS 的
/// `JSON.stringify(null)` 一致。
fn invalid_device_authorization_error(json: Option<&Value>) -> AuthError {
    AuthError::new(format!(
        "Invalid Kimi Code device authorization response: {}",
        crate::auth::oauth::json_stringify_nullable(json)
    ))
}

/// `parseTokenResponse(json, operation)`。
///
/// `access_token`/`refresh_token` 必须是非空字符串，`expires_in` 必须是有限且为正的
/// JSON 数字；任一不满足就报 `Kimi Code token {operation} response missing fields: ...`。
/// 成功时把相对秒数换算成绝对毫秒时间戳，不带安全余量。
fn parse_token_response(
    clock: &dyn AuthClock,
    json: Option<&Value>,
    operation: &str,
) -> Result<TokenResponse, AuthError> {
    let field = |name: &str| json.and_then(|json| crate::auth::oauth::object_field(json, name));
    let access_token = field("access_token")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let refresh_token = field("refresh_token")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let expires_in = field("expires_in")
        .and_then(|value| crate::auth::oauth::finite_number(Some(value)))
        .filter(|expires_in| *expires_in > 0.0);
    // 三个字段一起解构：缺任何一个都报同一条错误，只用 `operation` 区分 poll/refresh。
    let (Some(access_token), Some(refresh_token), Some(expires_in)) =
        (access_token, refresh_token, expires_in)
    else {
        return Err(AuthError::new(format!(
            "Kimi Code token {operation} response missing fields: {}",
            crate::auth::oauth::json_stringify_nullable(json)
        )));
    };
    Ok(TokenResponse {
        access: access_token.to_string(),
        refresh: refresh_token.to_string(),
        // 用调用方时钟（测试里是虚拟时钟）计算绝对过期时间；登录与刷新共用同一个实例。
        expires: clock.now_ms() + expires_in * 1000.0,
    })
}

/// `pollForToken(oauthHost, device, signal)`。
///
/// 复用共享的设备码轮询器反复交换 token。`poll` 是一个可反复调用的闭包：每轮都要新建
/// future，并把 `Arc` 与设备码克隆进去（future 必须 `'static`）。`wait_before_first_poll`
/// 为 `true`，因此首个请求发生在首个 interval 之后，而不是立即发出。
async fn poll_for_token(
    fetch: Arc<dyn OAuthFetch>,
    clock: Arc<dyn AuthClock>,
    oauth_host: String,
    device: DeviceAuthorization,
    signal: &AbortScope,
) -> Result<TokenResponse, AuthError> {
    // 闭包与每轮 future 都要求 'static，所以先把 signal 克隆成独立所有权副本。
    let signal = signal.clone();
    let poll = {
        let fetch = Arc::clone(&fetch);
        let clock = Arc::clone(&clock);
        let oauth_host = oauth_host.clone();
        let device = device.clone();
        let signal = signal.clone();
        move || -> crate::auth::oauth::device_code::OAuthDeviceCodePollFuture<TokenResponse> {
            let fetch = Arc::clone(&fetch);
            let clock = Arc::clone(&clock);
            let oauth_host = oauth_host.clone();
            let device = device.clone();
            let signal = signal.clone();
            Box::pin(async move {
                // 请求体只含 client_id、设备码与固定的 grant_type；每轮都重新发送。
                let request =
                    OAuthFetchRequest::new(format!("{oauth_host}/api/oauth/token"), &signal)
                        .header("Content-Type", "application/x-www-form-urlencoded")
                        .header("Accept", "application/json")
                        .post(
                            form_url_encode(&[
                                ("client_id", CLIENT_ID),
                                ("device_code", &device.device_code),
                                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                            ])
                            .into_bytes(),
                        )
                        .timeout_ms(REQUEST_TIMEOUT_MS);
                let response = fetch.fetch(request).await?;

                // 5xx 单独处理：把状态码与 body 原文交给轮询器，轮询器会终止整个流程
                // （这是失败而不是“稍后重试”）。
                if response.status >= 500 {
                    let text = response.text();
                    return Ok(OAuthDeviceCodePollResult::Failed {
                        message: format!(
                            "Kimi Code device token request failed with status {}{}",
                            response.status,
                            if text.is_empty() {
                                String::new()
                            } else {
                                format!(": {text}")
                            }
                        ),
                    });
                }

                let json = read_json(&response);
                // 成功判定与 TS 相同：先看 HTTP 2xx，再看 access_token 是字符串；字段完整性
                // 交给 parse_token_response，解析失败也转成 Failed（不再轮询）。
                if response.ok()
                    && let Some(access_token) = json
                        .as_ref()
                        .and_then(|json| crate::auth::oauth::object_field(json, "access_token"))
                        .and_then(Value::as_str)
                {
                    // 这里只借 access_token 做“存在且为字符串”的判定，真正的取值在
                    // parse_token_response 里，因此不需要它的值。
                    let _ = access_token;
                    return match parse_token_response(clock.as_ref(), json.as_ref(), "poll") {
                        Ok(value) => Ok(OAuthDeviceCodePollResult::Complete { value }),
                        Err(error) => Ok(OAuthDeviceCodePollResult::Failed {
                            message: error.message,
                        }),
                    };
                }

                // 非成功响应按 OAuth 的 error 字段分流；error 缺失或未知时落到最后的兜底
                // 文案（带状态码）。
                let error = json
                    .as_ref()
                    .and_then(|json| crate::auth::oauth::object_field(json, "error"))
                    .and_then(Value::as_str);
                let description = json
                    .as_ref()
                    .and_then(|json| crate::auth::oauth::object_field(json, "error_description"))
                    .and_then(Value::as_str)
                    .map(|description| format!(": {description}"))
                    .unwrap_or_default();
                if error == Some("authorization_pending") {
                    return Ok(OAuthDeviceCodePollResult::Pending);
                }
                if error == Some("slow_down") {
                    // 只在服务端给了正数间隔时采用；否则轮询器按 RFC 8628 增加 5 秒。
                    let interval = json
                        .as_ref()
                        .and_then(|json| crate::auth::oauth::object_field(json, "interval"))
                        .and_then(|value| value.as_f64())
                        .filter(|interval| *interval > 0.0);
                    return Ok(OAuthDeviceCodePollResult::SlowDown {
                        interval_seconds: interval,
                    });
                }
                if error == Some("expired_token") {
                    return Ok(OAuthDeviceCodePollResult::Failed {
                        message: "Kimi Code device authorization expired. Please restart login."
                            .into(),
                    });
                }
                if error == Some("access_denied") {
                    return Ok(OAuthDeviceCodePollResult::Failed {
                        message: "Kimi Code login was denied.".into(),
                    });
                }
                Ok(OAuthDeviceCodePollResult::Failed {
                    message: format!(
                        "Kimi Code device token request failed (status {}){}",
                        response.status,
                        match error {
                            Some(error) => format!(": {error}{description}"),
                            None => String::new(),
                        }
                    ),
                })
            })
        }
    };

    // 两个时长都取自设备授权响应（已补默认值）；轮询器负责间隔、slow_down 退避与总超时。
    poll_oauth_device_code_flow_with_clock(
        OAuthDeviceCodePollOptions {
            interval_seconds: Some(device.interval_seconds),
            expires_in_seconds: Some(device.expires_in_seconds),
            wait_before_first_poll: true,
            poll: Arc::new(poll),
            signal,
        },
        clock.as_ref(),
    )
    .await
}

/// `isRetryableRefreshFailure(response)`。
///
/// 429（限流）与 5xx（服务端故障）交给重试；其余状态（其它 4xx）视为确定性失败，重试
/// 只会得到同样的结果。
fn is_retryable_refresh_failure(status: u16) -> bool {
    status == 429 || status >= 500
}

/// `refreshToken(oauthHost, refreshTokenValue, signal)`。
///
/// 用 refresh token 换新的一对 token。最多 4 次尝试，重试前按 1s/2s/4s 退避；只有传输
/// 错误与 429/5xx 会继续重试，`401`/`403`/`invalid_grant` 立即失败（refresh token 已失效，
/// 只能重新登录）。
async fn refresh_token(
    fetch: &dyn OAuthFetch,
    clock: &dyn AuthClock,
    oauth_host: &str,
    refresh_token_value: &str,
    signal: &AbortScope,
) -> Result<TokenResponse, AuthError> {
    let mut last_error: Option<AuthError> = None;
    // 循环至少执行一次（attempt 0），因此正常走完循环时 `last_error` 一定被设置；
    // 末尾的兜底文案只是防御分支。
    for attempt in 0..=REFRESH_MAX_RETRIES {
        // 退避只发生在重试前：1s、2s、4s（1000 * 2^(attempt-1)）。
        if attempt > 0 {
            sleep(1000 * u64::pow(2, attempt - 1), signal)
                .await
                .map_err(|error| AuthError::with_name(error.name, error.message))?;
        }
        // sleep 自身在中止时失败并保留 reason；这里再查一次，覆盖“睡醒后立刻被取消”的窗口。
        if signal.is_aborted() {
            return Err(AuthError::new("Kimi Code token refresh aborted"));
        }

        let request = OAuthFetchRequest::new(format!("{oauth_host}/api/oauth/token"), signal)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .post(
                form_url_encode(&[
                    ("client_id", CLIENT_ID),
                    ("grant_type", "refresh_token"),
                    ("refresh_token", refresh_token_value),
                ])
                .into_bytes(),
            )
            .timeout_ms(REQUEST_TIMEOUT_MS);
        let response = match fetch.fetch(request).await {
            Ok(response) => response,
            // 传输层错误（含 30s 超时）不区分种类一律重试；只有拿到响应才按状态码分流。
            Err(error) => {
                last_error = Some(error);
                continue;
            }
        };

        // 先解析 body：成功响应也要用它（直接交给 parse_token_response）。
        let json = read_json(&response);
        if response.ok() {
            return parse_token_response(clock, json.as_ref(), "refresh");
        }

        let invalid_grant = json
            .as_ref()
            .and_then(|json| crate::auth::oauth::object_field(json, "error"))
            .and_then(Value::as_str)
            == Some("invalid_grant");
        // 401/403 或 invalid_grant 表示 refresh token 已失效，重试没有意义；文案带上
        // error_description（若有）。
        if response.status == 401 || response.status == 403 || invalid_grant {
            let description = json
                .as_ref()
                .and_then(|json| crate::auth::oauth::object_field(json, "error_description"))
                .and_then(Value::as_str)
                .map(|description| format!(": {description}"))
                .unwrap_or_default();
            return Err(AuthError::new(format!(
                "Kimi Code token refresh unauthorized (status {}){description}",
                response.status
            )));
        }

        // 可重试状态且还有剩余尝试：记录本轮错误，回到循环顶部退避。
        if is_retryable_refresh_failure(response.status) && attempt < REFRESH_MAX_RETRIES {
            last_error = Some(AuthError::new(format!(
                "Kimi Code token refresh failed with status {}",
                response.status
            )));
            continue;
        }

        // 不可重试的状态码（如 400），或可重试但已用完尝试次数：直接失败。
        // `json_stringify_nullable` 至少产出 `null`，所以这里的空检查实际不会命中，保留是
        // 为了保持输出路径单一、便于对照既有实现。
        let text = crate::auth::oauth::json_stringify_nullable(json.as_ref());
        return Err(AuthError::new(format!(
            "Kimi Code token refresh failed with status {}{}",
            response.status,
            if text.is_empty() {
                String::new()
            } else {
                format!(": {text}")
            }
        )));
    }
    Err(last_error.unwrap_or_else(|| AuthError::new("Kimi Code token refresh failed")))
}

/// Kimi Code OAuth 流程。
///
/// 持有注入的传输与时钟，登录与刷新共用同一份依赖；字段私有，外部只能经
/// [`KimiCodingOAuth::new`]、[`KimiCodingOAuth::with_fetch`]、
/// [`KimiCodingOAuth::with_dependencies`] 构造。
pub struct KimiCodingOAuth {
    fetch: Arc<dyn OAuthFetch>,
    clock: Arc<dyn AuthClock>,
}

impl KimiCodingOAuth {
    /// 默认依赖（reqwest + 系统时钟）。
    ///
    /// [`default_oauth_fetch`] 在**调用时**求值：它会先看进程级测试替身，再返回基于
    /// reqwest 的默认实现，因此替身必须在构造本对象之前安装。
    pub fn new() -> Self {
        KimiCodingOAuth {
            fetch: default_oauth_fetch(),
            clock: Arc::new(SystemAuthClock),
        }
    }

    /// 注入传输（测试用），时钟保持系统时钟。
    ///
    /// 只想替换 HTTP、但仍按真实时间判断 `expires` 时用它；需要固定时间线（虚拟时钟）
    /// 的测试用 [`KimiCodingOAuth::with_dependencies`]。
    pub fn with_fetch(fetch: Arc<dyn OAuthFetch>) -> Self {
        KimiCodingOAuth {
            fetch,
            clock: Arc::new(SystemAuthClock),
        }
    }

    /// 同时注入传输与时钟（测试用）。
    pub fn with_dependencies(fetch: Arc<dyn OAuthFetch>, clock: Arc<dyn AuthClock>) -> Self {
        KimiCodingOAuth { fetch, clock }
    }
}

impl Default for KimiCodingOAuth {
    /// 等价于 [`KimiCodingOAuth::new`]（默认依赖）。
    fn default() -> Self {
        Self::new()
    }
}

impl KimiCodingOAuth {
    /// `loginKimiCoding(interaction)`。
    ///
    /// 设备授权 → 通知宿主 `device_code` 事件 → 轮询。取消信号由
    /// [`AuthInteraction::signal`] 提供；这里不做错误改写，传输失败原样上抛。
    async fn login_kimi_coding(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<OAuthCredential, AuthError> {
        let signal = interaction.signal();
        let oauth_host = get_oauth_host();
        let device = start_device_authorization(self.fetch.as_ref(), &oauth_host, signal).await?;
        // 事件里用 complete URI（带 user_code）；两个 confirm 开关传 None，与其它设备码
        // 流程一致，由宿主决定默认行为。
        interaction.notify(&AuthEvent::DeviceCode {
            user_code: device.user_code.clone(),
            verification_uri: device.verification_uri_complete.clone(),
            open_browser_on_confirm: None,
            copy_code_on_confirm: None,
            interval_seconds: Some(device.interval_seconds),
            expires_in_seconds: Some(device.expires_in_seconds),
        });
        let token = poll_for_token(
            Arc::clone(&self.fetch),
            Arc::clone(&self.clock),
            oauth_host,
            device,
            signal,
        )
        .await?;
        Ok(OAuthCredential::new(
            token.access,
            token.refresh,
            token.expires,
        ))
    }
}

#[async_trait::async_trait]
impl OAuthAuth for KimiCodingOAuth {
    /// 展示名；`providers/kimi_coding.rs` 注册的 `lazy_oauth` 用同一字符串。
    fn name(&self) -> &str {
        "Kimi Code (subscription)"
    }

    /// 由 Kimi 订阅支撑，不是 API key 计费。
    fn is_subscription(&self) -> bool {
        true
    }

    /// 登录选项上显示的文字。
    fn login_label(&self) -> Option<&str> {
        Some("Sign in with Kimi Code")
    }

    /// 交互式登录；`LoginOptions`（agent 名、设备 id）对设备码流程没有用处，被忽略。
    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
        _options: &LoginOptions,
    ) -> Result<OAuthCredential, AuthError> {
        self.login_kimi_coding(interaction).await
    }

    /// 用存储的 refresh token 换新凭据；host 每次重新解析，环境变量改动即时生效。
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: &AbortScope,
    ) -> Result<OAuthCredential, AuthError> {
        let token = refresh_token(
            self.fetch.as_ref(),
            self.clock.as_ref(),
            &get_oauth_host(),
            &credential.refresh,
            signal,
        )
        .await?;
        Ok(OAuthCredential::new(
            token.access,
            token.refresh,
            token.expires,
        ))
    }

    /// 把 access token 变成 `Authorization: Bearer` 头。
    ///
    /// 不返回 `api_key`：本流程把凭证放在 header 里（测试断言
    /// `auth.api_key.is_none()`）。`ProviderHeaders` 的值是 `Option<String>`，`None`
    /// 表示抑制同名默认头，这里恒为 `Some`。
    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        let mut headers = ProviderHeaders::new();
        headers.insert(
            "Authorization".into(),
            Some(format!("Bearer {}", credential.access)),
        );
        Ok(ModelAuth {
            headers: Some(headers),
            ..ModelAuth::default()
        })
    }
}

/// 便捷构造（默认依赖）。
///
/// 与 `load.rs` 的模块级单例不同，每次调用都新建一个实例（各自解析一次
/// [`default_oauth_fetch`]）；provider 注册走 `load_kimi_coding_oauth`。
pub fn kimi_coding_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(KimiCodingOAuth::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_host_strips_trailing_slashes() {
        // 该测试不写环境变量（避免与并行测试竞争），只验证默认值。
        if get_provider_env_value("KIMI_CODE_OAUTH_HOST", None).is_none()
            && get_provider_env_value("KIMI_OAUTH_HOST", None).is_none()
        {
            assert_eq!(get_oauth_host(), DEFAULT_OAUTH_HOST);
        }
        assert_eq!(
            "https://auth.example.com///".trim_end_matches('/'),
            "https://auth.example.com"
        );
    }

    #[test]
    fn trusted_urls_require_http_schemes() {
        let value = |text: &str| trusted_http_url(Some(&Value::String(text.into())));
        assert_eq!(
            value("https://www.kimi.com/code").as_deref(),
            Some("https://www.kimi.com/code")
        );
        assert_eq!(value("file:///etc/passwd"), None);
        assert_eq!(value("not a url"), None);
        assert_eq!(value(""), None);
    }
}
