//! auth/oauth/xai.ts——xAI OAuth 设备码流程。
//!
//! RFC 8628 设备授权；`expires_in` 缺失时按 1 小时（仅刷新时允许缺失），验证 URL 只允许
//! `https:`。刷新时未轮换的 refresh token 必须保留。
//!
//! TS: `src/auth/oauth/xai.ts`
//!
//! # 设计原因与执行流程
//!
//! xAI 只支持设备码（没有 loopback 回调）：`request_device_code` 向固定端点
//! `https://auth.x.ai/oauth2/device/code` 取码，`login_xai` 把用户码与验证地址发给宿主，
//! `poll_for_tokens` 用 `wait_before_first_poll: true` 交给共享轮询器（首次轮询前先等一个
//! interval）。刷新与设备码轮询走同一个 token 端点，但 `grant_type` 与授权凭据字段不同：
//! 设备码轮询带 `device_code`，刷新带 `refresh_token`。
//!
//! 与 Radius 不同，本文件**严格校验**响应字段：`required_string` 要求非空字符串，
//! `positive_number_field` 要求有限且 > 0，`validate_verification_uri` 只接受可解析的
//! `https:` URL。校验失败发生在解析阶段，坏值不会进入事件或轮询。
//!
//! # 注意事项
//!
//! - 设备码响应缺 `expires_in` 直接报错；`interval` 缺失、非正数或非数字则回退轮询器默认
//!   5s（RFC 8628 允许 `interval: 0`，这里与 TS 一样视为“缺失”）。
//! - token 响应缺 `expires_in` 按 3600s 处理（刷新响应可能省略），但字段存在时必须 > 0；
//!   缺 `refresh_token` 时沿用调用方给出的旧 token（未轮换），两者都没有才报错。
//! - `credentials_from_token_response` 由设备码完成与刷新两条路径共用，因此设备码 token 响应
//!   缺 `expires_in` 时也会回退到 3600s；头部“仅刷新时允许缺失”不是代码里的路径限制。
//! - `post_form` 对非 2xx 也照常解析 JSON：非对象（含数组）一律当作空对象，因此错误详情
//!   只可能来自对象字段；解析失败按 `xAI OAuth returned invalid JSON (HTTP <status>)` 报错。
//! - 取消：fetch 抛错时若 [`AbortScope`] 已中止，统一改写为 `"Login cancelled"`；轮询等待的
//!   取消由 [`crate::auth::oauth::device_code`] 负责。
//! - 错误传播：轮询回调只把 `authorization_pending`/`slow_down`/`access_denied`/
//!   `authorization_denied`/`expired_token` 转成状态机结果，其他非 2xx 变成 `Failed`，
//!   传输层错误则以 `Err` 直接结束整个登录。

use std::sync::Arc;

use serde_json::Value;

use crate::auth::oauth::device_code::{
    AuthClock, OAuthDeviceCodePollOptions, OAuthDeviceCodePollResult, SystemAuthClock,
    poll_oauth_device_code_flow_with_clock,
};
use crate::auth::oauth::{
    OAuthFetch, OAuthFetchRequest, default_oauth_fetch, form_urlencode, is_object_like,
};
use crate::auth::types::{
    AuthError, AuthEvent, AuthInteraction, LoginOptions, ModelAuth, OAuthAuth, OAuthCredential,
};
use crate::utils::abort::AbortScope;

/// `XAI_CLIENT_ID`。
const XAI_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
/// `XAI_SCOPE`。
const XAI_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
/// `XAI_DEVICE_CODE_URL`。
const XAI_DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";
/// `XAI_TOKEN_URL`。
const XAI_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
/// `REFRESH_SKEW_MS`。
const REFRESH_SKEW_MS: f64 = 5.0 * 60.0 * 1000.0;
/// `DEFAULT_TOKEN_LIFETIME_SECONDS`。
const DEFAULT_TOKEN_LIFETIME_SECONDS: f64 = 3600.0;

/// `XaiDeviceCode`。
///
/// `parse_device_code` 的规范化结果：所有字符串字段都已通过校验，`interval_seconds` 只在
/// 服务端给了有限正数时为 `Some`，`expires_in_seconds` 一定是正数。
#[derive(Debug, Clone)]
struct XaiDeviceCode {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    interval_seconds: Option<f64>,
    expires_in_seconds: f64,
}

/// `OAuthHttpResponse`。
///
/// `post_form` 的返回值：保留 `ok`/`status` 与已归一的 `body`，让调用方既能拼错误文案，
/// 也能在成功时解析字段。非 2xx 不会在这里变成 `Err`。
struct OAuthHttpResponse {
    ok: bool,
    status: u16,
    body: Value,
}

/// `requiredString(body, field)`。
///
/// 只接受非空 JSON 字符串；`null`、数字、空串都无效（不做 JS `String()` 转换）。失败文案
/// 统一为 `Invalid xAI OAuth response field: <field>`。
fn required_string(body: &Value, field: &str) -> Result<String, AuthError> {
    match crate::auth::oauth::object_field(body, field).and_then(Value::as_str) {
        Some(value) if !value.is_empty() => Ok(value.to_string()),
        _ => Err(invalid_field_error(field)),
    }
}

/// `positiveNumber(body, field)`：有限数且 > 0。
///
/// 只用于 `expires_in`；`interval` 不用它，因为 RFC 8628 允许 0 表示“无最小间隔”。
fn positive_number_field(body: &Value, field: &str) -> Result<f64, AuthError> {
    crate::auth::oauth::finite_number(crate::auth::oauth::object_field(body, field))
        .filter(|number| *number > 0.0)
        .ok_or_else(|| invalid_field_error(field))
}

/// `Invalid xAI OAuth response field: ${field}`。
///
/// `required_string` / `positive_number_field` 的失败共用这一文案，便于与 TS 测试逐字对齐；
/// 验证 URI 的 scheme 校验不走这里，而是 `validate_verification_uri` 的
/// `Untrusted verification URI in xAI OAuth response`（`tests/xai_oauth.rs` 有断言）。
fn invalid_field_error(field: &str) -> AuthError {
    AuthError::new(format!("Invalid xAI OAuth response field: {field}"))
}

/// `validateVerificationUri(raw)`：只允许 `https:`，返回规范化 href。
///
/// 不可解析的字符串、`http:`/`file:` 等非 TLS scheme 一律拒绝（TS 把非 https 的全部视为
/// 不可信）；返回 `url::Url` 规范化后的序列化结果（绝对 URL）。
fn validate_verification_uri(raw: &str) -> Result<String, AuthError> {
    let url = url::Url::parse(raw)
        .map_err(|_| AuthError::new("Untrusted verification URI in xAI OAuth response"))?;
    if url.scheme() != "https" {
        return Err(AuthError::new(
            "Untrusted verification URI in xAI OAuth response",
        ));
    }
    Ok(url.into())
}

/// `postForm(url, fields, signal)`。
///
/// 表单 POST，固定 `Accept` 与 `Content-Type`。响应体归一化规则如下（稳定契约）：
/// - JSON 对象 → 原样作为 `body`；
/// - 其他合法 JSON（数组、字符串、`null`…）→ 空对象；
/// - 解析失败 → `xAI OAuth returned invalid JSON (HTTP <status>)`，但若 signal 已中止则报
///   `"Login cancelled"`。
///
/// 非 2xx 不在这里报错：错误体可能带 `error`/`error_description`，由调用方决定文案。
async fn post_form(
    fetch: &dyn OAuthFetch,
    url: &str,
    fields: &[(&str, &str)],
    signal: &AbortScope,
) -> Result<OAuthHttpResponse, AuthError> {
    let request = OAuthFetchRequest::new(url, signal)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .post(form_urlencode(fields).into_bytes());
    let response = match fetch.fetch(request).await {
        Ok(response) => response,
        Err(error) => {
            if signal.is_aborted() {
                return Err(AuthError::new("Login cancelled"));
            }
            return Err(error);
        }
    };

    // 错误响应也走同一套解析：调用方只按字段取值。
    let body = match response.json() {
        Ok(parsed) if is_object_like(&parsed) && !matches!(parsed, Value::Array(_)) => parsed,
        Ok(_) => Value::Object(serde_json::Map::new()),
        Err(_) => {
            // 响应体在 fetch 层已完整缓冲（`OAuthFetchResponse`），取消不会造成“未读完的 body
            // 解析失败”；保留此判断只为对齐 TS 的优先级：若 signal 已中止，把解析失败优先
            // 报为 `Login cancelled`。
            if signal.is_aborted() {
                return Err(AuthError::new("Login cancelled"));
            }
            return Err(AuthError::new(format!(
                "xAI OAuth returned invalid JSON (HTTP {})",
                response.status
            )));
        }
    };
    Ok(OAuthHttpResponse {
        ok: response.ok(),
        status: response.status,
        body,
    })
}

/// `requestFailure(action, response)`。
///
/// 取字符串形式的 `error` 与 `error_description`，过滤空串后用 `": "` 连接，生成
/// `xAI OAuth <action> failed (HTTP <status>)`（有详情时追加 `: <detail>`）。
/// 设备授权、刷新与“未知轮询错误”三个调用点共用它。
fn request_failure(action: &str, response: &OAuthHttpResponse) -> AuthError {
    let error = crate::auth::oauth::object_field(&response.body, "error").and_then(Value::as_str);
    let description = crate::auth::oauth::object_field(&response.body, "error_description")
        .and_then(Value::as_str);
    let detail = [error, description]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(": ");
    AuthError::new(format!(
        "xAI OAuth {action} failed (HTTP {}){}",
        response.status,
        if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        }
    ))
}

/// `parseDeviceCode(body)`。
///
/// 逐字段校验并归一化，任何一项不合法都会在发起轮询前失败：
/// - `interval`：有限且 > 0 才保留，0/非数字/缺失都得到 `None`（轮询器默认 5s）；
/// - `verification_uri_complete`：可选，非空字符串才校验并保留；
/// - `device_code`/`user_code`/`verification_uri`：必需的非空字符串，后者还要求 `https:`；
/// - `expires_in`：必需的有限正数（xAI 与 Kimi/Meta 不同，缺失直接报错）。
fn parse_device_code(body: &Value) -> Result<XaiDeviceCode, AuthError> {
    // RFC 8628 允许 interval 0（无最小等待）；非正/非法值回退轮询器默认值。
    let interval_seconds =
        crate::auth::oauth::finite_number(crate::auth::oauth::object_field(body, "interval"))
            .filter(|interval| *interval > 0.0);
    // 可选字段：TS 用真值判断，空串等同于缺失，因此只校验非空字符串。
    let verification_uri_complete =
        crate::auth::oauth::object_field(body, "verification_uri_complete")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(validate_verification_uri)
            .transpose()?;
    Ok(XaiDeviceCode {
        device_code: required_string(body, "device_code")?,
        user_code: required_string(body, "user_code")?,
        verification_uri: validate_verification_uri(&required_string(body, "verification_uri")?)?,
        verification_uri_complete,
        interval_seconds,
        expires_in_seconds: positive_number_field(body, "expires_in")?,
    })
}

/// `credentialsFromTokenResponse(body, previousRefreshToken?)`。
///
/// - `access_token`：必需的非空字符串；
/// - `refresh_token`：字段缺失且调用方给了非空旧 token 时沿用旧值（xAI 未轮换时会省略该
///   字段）；字段存在时必须是有效字符串；
/// - `expires_in`：缺失 → 3600s；存在 → 必须是有限正数；
/// - `expires = now + expires_in * 1000 - REFRESH_SKEW_MS`（提前 5 分钟过期）。
fn credentials_from_token_response(
    clock: &dyn AuthClock,
    body: &Value,
    previous_refresh_token: Option<&str>,
) -> Result<OAuthCredential, AuthError> {
    let access = required_string(body, "access_token")?;
    // xAI 在未轮换时可能省略 refresh_token。
    let refresh = match crate::auth::oauth::object_field(body, "refresh_token") {
        None => match previous_refresh_token.filter(|token| !token.is_empty()) {
            Some(previous) => previous.to_string(),
            None => required_string(body, "refresh_token")?,
        },
        Some(_) => required_string(body, "refresh_token")?,
    };
    // 只有键缺失才走默认 1 小时；显式的 null/0/负数都会报错。
    let expires_in_seconds = match crate::auth::oauth::object_field(body, "expires_in") {
        None => DEFAULT_TOKEN_LIFETIME_SECONDS,
        Some(_) => positive_number_field(body, "expires_in")?,
    };
    Ok(OAuthCredential::new(
        access,
        refresh,
        clock.now_ms() + expires_in_seconds * 1000.0 - REFRESH_SKEW_MS,
    ))
}

/// `requestDeviceCode(signal)`。
///
/// 向固定端点 POST `client_id`/`scope`/`referrer`；`referrer` 是 xAI 特有的来源标识（TS 固定
/// 为 `"kiana"`）。非 2xx 用 `request_failure("device authorization", …)` 报错。
async fn request_device_code(
    fetch: &dyn OAuthFetch,
    signal: &AbortScope,
) -> Result<XaiDeviceCode, AuthError> {
    let response = post_form(
        fetch,
        XAI_DEVICE_CODE_URL,
        &[
            ("client_id", XAI_CLIENT_ID),
            ("scope", XAI_SCOPE),
            ("referrer", "kiana"),
        ],
        signal,
    )
    .await?;
    if !response.ok {
        return Err(request_failure("device authorization", &response));
    }
    parse_device_code(&response.body)
}

/// `pollForTokens(device, signal)`。
///
/// 把“发一次 token 请求并分类结果”的闭包交给共享轮询器；`wait_before_first_poll: true`
/// 表示服务端发码后先等一个 interval 再轮询（测试按 +5s 锁定该时序）。
///
/// 分类规则：
/// - 2xx → `Complete`（解析时不带旧 refresh token）；
/// - `authorization_pending` → `Pending`；
/// - `slow_down` → `SlowDown`，`interval` 只在是 JSON number 时透传；
/// - `access_denied`/`authorization_denied`/`expired_token` → `Failed`（固定文案）；
/// - 其他非 2xx → `Failed`，文案来自 `request_failure("device token polling", …)`。
///
/// 传输层错误不在这里分类，直接以 `Err` 冒出并终止登录。
async fn poll_for_tokens(
    fetch: Arc<dyn OAuthFetch>,
    clock: Arc<dyn AuthClock>,
    device: XaiDeviceCode,
    signal: &AbortScope,
) -> Result<OAuthCredential, AuthError> {
    let signal = signal.clone();
    let poll = {
        let fetch = Arc::clone(&fetch);
        let clock = Arc::clone(&clock);
        let device = device.clone();
        let signal = signal.clone();
        move || -> crate::auth::oauth::device_code::OAuthDeviceCodePollFuture<OAuthCredential> {
            let fetch = Arc::clone(&fetch);
            let clock = Arc::clone(&clock);
            let device = device.clone();
            let signal = signal.clone();
            Box::pin(async move {
                let response = post_form(
                    fetch.as_ref(),
                    XAI_TOKEN_URL,
                    &[
                        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                        ("client_id", XAI_CLIENT_ID),
                        ("device_code", &device.device_code),
                    ],
                    &signal,
                )
                .await?;

                // 成功响应也经过 `post_form` 的归一化；body 不是对象时会因缺 access_token 报错。
                if response.ok {
                    let value =
                        credentials_from_token_response(clock.as_ref(), &response.body, None)?;
                    return Ok(OAuthDeviceCodePollResult::Complete { value });
                }

                // 只有 `error` 是字符串时才参与分类；其他形态落进默认的 Failed 分支。
                let error = crate::auth::oauth::object_field(&response.body, "error")
                    .and_then(Value::as_str);
                match error {
                    Some("authorization_pending") => Ok(OAuthDeviceCodePollResult::Pending),
                    Some("slow_down") => Ok(OAuthDeviceCodePollResult::SlowDown {
                        interval_seconds: crate::auth::oauth::object_field(
                            &response.body,
                            "interval",
                        )
                        .and_then(Value::as_f64),
                    }),
                    Some("access_denied") | Some("authorization_denied") => {
                        Ok(OAuthDeviceCodePollResult::Failed {
                            message: "xAI device authorization was denied".into(),
                        })
                    }
                    Some("expired_token") => Ok(OAuthDeviceCodePollResult::Failed {
                        message: "xAI device code expired".into(),
                    }),
                    _ => Ok(OAuthDeviceCodePollResult::Failed {
                        message: request_failure("device token polling", &response).message,
                    }),
                }
            })
        }
    };

    poll_oauth_device_code_flow_with_clock(
        OAuthDeviceCodePollOptions {
            interval_seconds: device.interval_seconds,
            expires_in_seconds: Some(device.expires_in_seconds),
            wait_before_first_poll: true,
            poll: Arc::new(poll),
            signal,
        },
        clock.as_ref(),
    )
    .await
}

/// `refreshXaiToken(refreshToken, signal)`。
///
/// 用 `grant_type=refresh_token` 调同一个 token 端点，失败用
/// `request_failure("token refresh", …)`。解析时把旧 refresh token 作为 fallback 传入：
/// xAI 未轮换时会省略 `refresh_token` 字段。
async fn refresh_xai_token(
    fetch: &dyn OAuthFetch,
    clock: &dyn AuthClock,
    refresh_token: &str,
    signal: &AbortScope,
) -> Result<OAuthCredential, AuthError> {
    let response = post_form(
        fetch,
        XAI_TOKEN_URL,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", XAI_CLIENT_ID),
            ("refresh_token", refresh_token),
        ],
        signal,
    )
    .await?;
    if !response.ok {
        return Err(request_failure("token refresh", &response));
    }
    credentials_from_token_response(clock, &response.body, Some(refresh_token))
}

/// xAI OAuth 流程。
///
/// 只实现设备码（RFC 8628），没有浏览器回调；`fetch`/`clock` 通过 `Arc` 注入以便测试。
/// 实例没有内部可变状态，可以放进 `Arc` 被多处共享。
pub struct XaiOAuth {
    fetch: Arc<dyn OAuthFetch>,
    clock: Arc<dyn AuthClock>,
}

impl XaiOAuth {
    /// 默认依赖（reqwest + 系统时钟）。
    ///
    /// `auth::oauth::load` 用本函数构造模块级单例；[`xai_oauth`] 每次返回新实例。
    pub fn new() -> Self {
        XaiOAuth {
            fetch: default_oauth_fetch(),
            clock: Arc::new(SystemAuthClock),
        }
    }

    /// 注入传输（测试用），时钟保持系统时钟。
    ///
    /// 只替换 `fetch`；凭证 `expires` 仍按 `SystemAuthClock` 计算。
    pub fn with_fetch(fetch: Arc<dyn OAuthFetch>) -> Self {
        XaiOAuth {
            fetch,
            clock: Arc::new(SystemAuthClock),
        }
    }

    /// 同时注入传输与时钟（测试用）。
    ///
    /// `clock.now_ms()` 是 `expires` 的基准；测试用 `VirtualClock` 得到确定的时间线。
    pub fn with_dependencies(fetch: Arc<dyn OAuthFetch>, clock: Arc<dyn AuthClock>) -> Self {
        XaiOAuth { fetch, clock }
    }
}

impl Default for XaiOAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl XaiOAuth {
    /// `loginXai(interaction)`。
    ///
    /// 设备码 → `DeviceCode` 事件 → 轮询。事件里的 `verification_uri` 优先用
    /// `verification_uri_complete`（带用户码的完整地址），没有才回退 `verification_uri`。
    async fn login_xai(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<OAuthCredential, AuthError> {
        let signal = interaction.signal();
        let device = request_device_code(self.fetch.as_ref(), signal).await?;
        interaction.notify(&AuthEvent::DeviceCode {
            user_code: device.user_code.clone(),
            verification_uri: device
                .verification_uri_complete
                .clone()
                .unwrap_or_else(|| device.verification_uri.clone()),
            open_browser_on_confirm: None,
            copy_code_on_confirm: None,
            interval_seconds: device.interval_seconds,
            expires_in_seconds: Some(device.expires_in_seconds),
        });
        poll_for_tokens(
            Arc::clone(&self.fetch),
            Arc::clone(&self.clock),
            device,
            signal,
        )
        .await
    }
}

#[async_trait::async_trait]
impl OAuthAuth for XaiOAuth {
    /// 展示名；`providers/xai.rs` 的 `lazy_oauth` 硬编码了同一字符串（两处需同步修改）。
    fn name(&self) -> &str {
        "xAI (Grok/X subscription)"
    }

    /// 该凭证走 xAI 订阅计费，不是 API key。
    fn is_subscription(&self) -> bool {
        true
    }

    /// 登录选项上显示的文字。
    fn login_label(&self) -> Option<&str> {
        Some("Sign in with SuperGrok or X Premium")
    }

    /// 交互式登录；`LoginOptions` 对设备码流程没有用处，被忽略。
    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
        _options: &LoginOptions,
    ) -> Result<OAuthCredential, AuthError> {
        self.login_xai(interaction).await
    }

    /// 用 `credential.refresh` 换新凭证；未轮换时保留旧 refresh token。
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: &AbortScope,
    ) -> Result<OAuthCredential, AuthError> {
        refresh_xai_token(
            self.fetch.as_ref(),
            self.clock.as_ref(),
            &credential.refresh,
            signal,
        )
        .await
    }

    /// 把 `access` 当作 API key；不设置 `baseUrl`/`headers`。
    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..ModelAuth::default()
        })
    }
}

/// 便捷构造（默认依赖）。
///
/// 返回 `Arc<dyn OAuthAuth>`；本 crate 的加载路径使用 `auth::oauth::load` 里的模块级单例，
/// 不经过这里，但两者构造出的流程行为相同。
pub fn xai_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(XaiOAuth::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_codes_require_https_verification_urls() {
        let body = crate::auth::oauth::json_object(vec![
            ("device_code", Value::String("d".into())),
            ("user_code", Value::String("u".into())),
            (
                "verification_uri",
                Value::String("http://accounts.x.ai/device".into()),
            ),
            ("expires_in", Value::Number(900.into())),
        ]);
        assert_eq!(
            parse_device_code(&body).unwrap_err().message,
            "Untrusted verification URI in xAI OAuth response"
        );
    }

    #[test]
    fn interval_zero_falls_back_to_none() {
        let body = crate::auth::oauth::json_object(vec![
            ("device_code", Value::String("d".into())),
            ("user_code", Value::String("u".into())),
            (
                "verification_uri",
                Value::String("https://accounts.x.ai/device".into()),
            ),
            ("expires_in", Value::Number(900.into())),
            ("interval", Value::Number(0.into())),
        ]);
        let device = parse_device_code(&body).expect("device");
        assert_eq!(device.interval_seconds, None);
    }

    #[test]
    fn missing_expires_in_is_an_error() {
        let body = crate::auth::oauth::json_object(vec![
            ("device_code", Value::String("d".into())),
            ("user_code", Value::String("u".into())),
            (
                "verification_uri",
                Value::String("https://accounts.x.ai/device".into()),
            ),
        ]);
        assert_eq!(
            parse_device_code(&body).unwrap_err().message,
            "Invalid xAI OAuth response field: expires_in"
        );
    }
}
