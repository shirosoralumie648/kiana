//! auth/oauth/meta.ts——Meta（Muse subscription）OAuth。
//!
//! Meta 把身份与 API 访问分开：设备流程拿到的身份 token 不能用于推理，需要经 Muse Code 的
//! key-mint 端点换成 Model API key（约一天有效）。**身份 token 存为 `refresh`，铸造的 key 存为
//! `access`**，标准 OAuth 调度器在 key 过期时重新铸造。身份 token 本身不可续期
//! （`grant_type=refresh_token` 返回 404、也不下发 refresh_token），因此 mint 返回 401/403
//! 即会话失效，必须重新登录。
//!
//! TS: `src/auth/oauth/meta.ts`
//!
//! # 一次登录的执行流程
//!
//! 1. `POST https://auth.meta.com/oidc/device/authorization/` 取设备码（先读 body 再判
//!    状态，错误详情来自 body）；
//! 2. 通过 [`AuthInteraction::notify`] 发出 `device_code` 事件；
//! 3. `poll_for_identity_token` 轮询 `.../device/token/`，得到**身份 token**；
//! 4. 发一条 `progress` 事件，然后用身份 token 调 key-mint 端点换取 **Model API key**；
//! 5. 返回 [`OAuthCredential`]：`access` = 铸造的 key，`refresh` = 身份 token，
//!    `expires` = 本地时钟 + 24 小时（代码不读取响应里的任何过期字段）。
//!
//! # 错误与取消
//!
//! - mint 返回 401/403 表示身份会话失效：身份 token 不可续期，只能重新登录（文案里直接
//!   提示 `/login meta`）。
//! - mint 成功但没有 `api_key`（例如需要付费/开通）时，错误里附带经校验的 `action_url`。
//! - `login_meta` 在整体失败后检查 `interaction.signal()`：只要已取消就统一改写成
//!   `Login cancelled`，因为宿主 UI 依赖这条文案（在途 fetch 的 DOMException 会被覆盖）。
//! - 超时由每个请求上的 `timeout_ms(30_000)` 提供；设备授权、轮询与 mint（refresh 复用
//!   的 mint 也一样）都受约束。
//!
//! # 注意事项
//!
//! - `refresh` 只是重新 mint：旧 `access` 被丢弃，`refresh` 原样保留，因此上层看到的
//!   “refresh token”始终是身份 token。
//! - mint 的 `expires` 是本地假定的 24 小时：代码不读取服务端有效期，只按 24 小时假设决定
//!   刷新时机。若服务端实际更短，可能在本地下次刷新前就出现请求失败（401/403）。
//! - 错误文案是稳定契约，`tests/meta_oauth.rs` 有断言，改动需同步测试。

use std::sync::Arc;

use serde_json::Value;

use crate::auth::oauth::device_code::{
    AuthClock, OAuthDeviceCodePollOptions, OAuthDeviceCodePollResult, SystemAuthClock,
    poll_oauth_device_code_flow_with_clock,
};
use crate::auth::oauth::{
    OAuthFetch, OAuthFetchRequest, OAuthFetchResponse, default_oauth_fetch, form_urlencode,
    is_object_like, js_trim,
};
use crate::auth::types::{
    AuthError, AuthEvent, AuthInteraction, LoginOptions, ModelAuth, OAuthAuth, OAuthCredential,
};
use crate::utils::abort::AbortScope;

/// `CLIENT_ID`（Muse Code CLI client id）。
///
/// 随表单体发送的公开标识，不是密钥。
const CLIENT_ID: &str = "1031625952748946";
/// `DEVICE_AUTHORIZATION_URL`（注意尾斜杠）。
///
/// 常量自带尾斜杠，拼接时不要再补；改动会直接改变请求路径。
const DEVICE_AUTHORIZATION_URL: &str = "https://auth.meta.com/oidc/device/authorization/";
/// `DEVICE_TOKEN_URL`（注意尾斜杠）。
///
/// 同上：尾斜杠是服务端路径的一部分。
const DEVICE_TOKEN_URL: &str = "https://auth.meta.com/oidc/device/token/";
/// `API_KEY_MINT_URL`。
///
/// 身份 token → Model API key 的交易所端点；`refresh` 也复用它。
const API_KEY_MINT_URL: &str = "https://api.meta.ai/muse-code/key";
/// `API_KEY_LIFETIME_MS`。
///
/// 本地假定的 key 有效期（24 小时）。代码没有从 mint 响应读取有效期，`expires` 直接用它
/// 计算。
const API_KEY_LIFETIME_MS: f64 = 24.0 * 60.0 * 60.0 * 1000.0;
/// `REQUEST_TIMEOUT_MS`。
///
/// 三个请求（设备授权、设备码轮询、mint）共用的 30 秒上限。
const REQUEST_TIMEOUT_MS: u64 = 30_000;

/// `DeviceAuthorization`。
///
/// 设备授权响应中真正需要的字段。与 Kimi 不同，`interval_seconds`/`expires_in_seconds`
/// 保持 `Option`：缺失时设备码轮询器用自己的默认值（5 秒、无 deadline）。
#[derive(Debug, Clone)]
struct DeviceAuthorization {
    /// 轮询用的不透明凭据。
    device_code: String,
    /// 展示给用户输入的短码。
    user_code: String,
    /// **已经过 `trusted_http_url` 规范化**的验证地址（优先 complete URI）。
    verification_uri: String,
    /// 服务端给出的轮询间隔（秒），非正数按缺失处理。
    interval_seconds: Option<f64>,
    /// 设备码有效期（秒），非正数按缺失处理。
    expires_in_seconds: Option<f64>,
}

/// `readJson(response)`：成功且为对象/数组 → 值；否则 `None`。
///
/// 解析失败归一为 `None`，错误消息里的 `JSON.stringify(json)` 因此是字面量 `null`
/// （测试 `reports_null_for_unparseable_device_responses` 锁定）。
fn read_json(response: &OAuthFetchResponse) -> Option<Value> {
    let json = response.json().ok()?;
    if is_object_like(&json) {
        Some(json)
    } else {
        None
    }
}

/// `errorDetail(json)`：`error_description` → `detail` → `message` → `error`（trim 非空）。
///
/// 四个键按固定优先级尝试，只有 `trim` 后非空的字符串才算命中（`js_trim` 与 JS 的
/// `String.prototype.trim` 一致，含 BOM）。返回值自带 `": "` 前缀，调用方直接拼在状态码
/// 后面；全都不命中时返回空串，消息不会留下尾随冒号（测试
/// `error_detail_picks_the_first_non_empty_string` 锁定空白串被跳过）。
fn error_detail(json: Option<&Value>) -> String {
    for key in ["error_description", "detail", "message", "error"] {
        let value = json.and_then(|json| crate::auth::oauth::object_field(json, key));
        if let Some(value) = value.and_then(Value::as_str) {
            let trimmed = js_trim(value);
            if !trimmed.is_empty() {
                return format!(": {trimmed}");
            }
        }
    }
    String::new()
}

/// `trustedHttpUrl(value)`：非空字符串、`new URL` 成功且 protocol 为 http(s) → 规范化 href。
///
/// 地址来自服务端、宿主的登录界面可能直接打开它，因此必须挡掉 `file:`/`javascript:` 等
/// scheme（测试 `trusted_urls_prefer_http_schemes` 锁定 `ftp:` 被拒）。与 Kimi 不同，
/// 返回值会被**保存并展示**（作为 `verification_uri`），不只是做校验。
fn trusted_http_url(value: Option<&Value>) -> Option<String> {
    let value = value?.as_str().filter(|value| !value.is_empty())?;
    let url = url::Url::parse(value).ok()?;
    if url.scheme() != "https" && url.scheme() != "http" {
        return None;
    }
    Some(url.into())
}

/// `positiveNumber(value)`。
///
/// 等价于 `typeof value === "number" && Number.isFinite(value) && value > 0`：JSON 字符串
/// 不算数字。返回 `None` 时调用方回落到设备码轮询器的默认值。
fn positive_number(value: Option<&Value>) -> Option<f64> {
    crate::auth::oauth::finite_number(value).filter(|number| *number > 0.0)
}

/// `startDeviceAuthorization(signal)`。
///
/// `verification_uri_complete` 优先，它缺失或不合格时回退到 `verification_uri`；最终必须
/// 有一个通过 `trusted_http_url`，且 `device_code`/`user_code` 非空。任何字段不合格都返回
/// 带原始 JSON 的错误（解析失败时是字面量 `null`）。
async fn start_device_authorization(
    fetch: &dyn OAuthFetch,
    signal: &AbortScope,
) -> Result<DeviceAuthorization, AuthError> {
    // 请求体只有 client_id；超时与取消分别走 timeout_ms 与 signal。
    let request = OAuthFetchRequest::new(DEVICE_AUTHORIZATION_URL, signal)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .post(form_urlencode(&[("client_id", CLIENT_ID)]).into_bytes())
        .timeout_ms(REQUEST_TIMEOUT_MS);
    let response = fetch.fetch(request).await?;
    // 先读 body 再判 ok（错误详情来自 body）。
    let json = read_json(&response);
    if !response.ok() {
        return Err(AuthError::new(format!(
            "Meta device authorization failed with status {}{}",
            response.status,
            error_detail(json.as_ref())
        )));
    }
    let field = |name: &str| {
        json.as_ref()
            .and_then(|json| crate::auth::oauth::object_field(json, name))
    };
    // complete URI 让用户免输码；它缺失或不是 http(s) 时才回退到 plain URI。
    let verification_uri = trusted_http_url(field("verification_uri_complete"))
        .or_else(|| trusted_http_url(field("verification_uri")));
    let device_code = field("device_code")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let user_code = field("user_code")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    // 三个字段必须同时有效；任一不合格就整体失败，不做部分降级。
    let (Some(device_code), Some(user_code), Some(verification_uri)) =
        (device_code, user_code, verification_uri)
    else {
        return Err(AuthError::new(format!(
            "Invalid Meta device authorization response: {}",
            crate::auth::oauth::json_stringify_nullable(json.as_ref())
        )));
    };
    // interval/expires_in 可选：非数字或非正数都折算成 None，交给轮询器用默认值。
    Ok(DeviceAuthorization {
        device_code: device_code.to_string(),
        user_code: user_code.to_string(),
        verification_uri,
        interval_seconds: positive_number(field("interval")),
        expires_in_seconds: positive_number(field("expires_in")),
    })
}

/// `pollForIdentityToken(device, signal)`。
///
/// 与 Kimi 的轮询结构相同，但返回的是身份 token 字符串（不是兑换好的凭证）。5xx 没有专门
/// 分支，仍按 `json.error` 分流；只有既取不到可用 `access_token`、`error` 也不属于已知
/// 取值（`authorization_pending`/`slow_down`/`access_denied`/`expired_token`）的响应，才落到
/// 带状态码与 `error_detail` 的兜底失败文案。
async fn poll_for_identity_token(
    fetch: Arc<dyn OAuthFetch>,
    clock: Arc<dyn AuthClock>,
    device: DeviceAuthorization,
    signal: &AbortScope,
) -> Result<String, AuthError> {
    // 闭包与每轮 future 都要求 'static，先克隆出独立所有权副本。
    let signal = signal.clone();
    let poll = {
        let fetch = Arc::clone(&fetch);
        let device = device.clone();
        let signal = signal.clone();
        move || -> crate::auth::oauth::device_code::OAuthDeviceCodePollFuture<String> {
            let fetch = Arc::clone(&fetch);
            let device = device.clone();
            let signal = signal.clone();
            Box::pin(async move {
                // 每轮重新发送同样的表单：grant_type、device_code、client_id。
                let request = OAuthFetchRequest::new(DEVICE_TOKEN_URL, &signal)
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .header("Accept", "application/json")
                    .post(
                        form_urlencode(&[
                            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                            ("device_code", &device.device_code),
                            ("client_id", CLIENT_ID),
                        ])
                        .into_bytes(),
                    )
                    .timeout_ms(REQUEST_TIMEOUT_MS);
                let response = fetch.fetch(request).await?;
                let json = read_json(&response);
                let field = |name: &str| {
                    json.as_ref()
                        .and_then(|json| crate::auth::oauth::object_field(json, name))
                };
                // 成功必须有 2xx 且 access_token 为非空字符串；缺字段不在这里报格式错误，
                // 而是落到下面的 error 分流：带已知 error 时按它处理（可能再次 Pending /
                // SlowDown），否则走带状态码的兜底失败文案。
                if response.ok()
                    && let Some(access_token) = field("access_token")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                {
                    return Ok(OAuthDeviceCodePollResult::Complete {
                        value: access_token.to_string(),
                    });
                }
                // RFC 8628 的两个中间状态交给轮询器；其余状态（含未知 error）全部终止。
                match field("error").and_then(Value::as_str) {
                    Some("authorization_pending") => Ok(OAuthDeviceCodePollResult::Pending),
                    Some("slow_down") => Ok(OAuthDeviceCodePollResult::SlowDown {
                        interval_seconds: positive_number(field("interval")),
                    }),
                    Some("access_denied") => Ok(OAuthDeviceCodePollResult::Failed {
                        message: "Meta login was denied.".into(),
                    }),
                    Some("expired_token") => Ok(OAuthDeviceCodePollResult::Failed {
                        message: "Meta device authorization expired. Please restart login.".into(),
                    }),
                    _ => Ok(OAuthDeviceCodePollResult::Failed {
                        message: format!(
                            "Meta device token request failed with status {}{}",
                            response.status,
                            error_detail(json.as_ref())
                        ),
                    }),
                }
            })
        }
    };

    poll_oauth_device_code_flow_with_clock(
        OAuthDeviceCodePollOptions {
            interval_seconds: device.interval_seconds,
            expires_in_seconds: device.expires_in_seconds,
            wait_before_first_poll: true,
            poll: Arc::new(poll),
            signal,
        },
        clock.as_ref(),
    )
    .await
}

/// `mintApiKey(identityToken, signal)`：身份 token → Model API key。
///
/// 这是本流程唯一的“刷新”路径：[`MetaOAuth::refresh`] 也调用它重新铸造。成功时返回的
/// 凭证里 `access` 是新 key、`refresh` 原样保存身份 token、`expires` 为本地时钟 +
/// 24 小时。
async fn mint_api_key(
    fetch: &dyn OAuthFetch,
    clock: &dyn AuthClock,
    identity_token: &str,
    signal: &AbortScope,
) -> Result<OAuthCredential, AuthError> {
    // mint 同样带 30 秒超时；refresh() 复用本函数，因此刷新也受这个上限约束。
    let request = OAuthFetchRequest::new(API_KEY_MINT_URL, signal)
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {identity_token}"))
        .header("Content-Type", "application/json")
        .header("x-api-version", "1.0.0")
        .post(b"{}".to_vec())
        .timeout_ms(REQUEST_TIMEOUT_MS);
    let response = fetch.fetch(request).await?;
    let json = read_json(&response);
    let field = |name: &str| {
        json.as_ref()
            .and_then(|json| crate::auth::oauth::object_field(json, name))
    };
    if response.status == 401 || response.status == 403 {
        // 身份 token 不可续期（见文件头）：只能重新登录。
        return Err(AuthError::new(format!(
            "Meta session expired (status {}). Run `/login meta` to sign in again.{}",
            response.status,
            error_detail(json.as_ref())
        )));
    }
    if !response.ok() {
        // 其余非 2xx：带 error_detail 的通用失败文案。
        return Err(AuthError::new(format!(
            "Meta API key mint failed with status {}{}",
            response.status,
            error_detail(json.as_ref())
        )));
    }
    let api_key = field("api_key")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    // 200 但没有 api_key 属于业务失败（例如尚未开通），不是协议错误；action_url 给用户
    // 下一步该去哪里完成设置。
    let Some(api_key) = api_key else {
        let action_url = trusted_http_url(field("action_url"));
        return Err(AuthError::new(format!(
            "Meta did not issue an API key.{}",
            action_url
                .map(|action_url| format!(" Complete setup at {action_url}"))
                .unwrap_or_default()
        )));
    };
    // 参数顺序容易看反：第一个是 access（铸造的 key），第二个是 refresh（身份 token）。
    Ok(OAuthCredential::new(
        api_key,
        identity_token,
        clock.now_ms() + API_KEY_LIFETIME_MS,
    ))
}

/// Meta OAuth 流程。
///
/// 持有注入的传输与时钟；登录与刷新共用同一份依赖。字段私有，外部只能经
/// [`MetaOAuth::new`]、[`MetaOAuth::with_fetch`]、[`MetaOAuth::with_dependencies`]
/// 构造。
pub struct MetaOAuth {
    fetch: Arc<dyn OAuthFetch>,
    clock: Arc<dyn AuthClock>,
}

impl MetaOAuth {
    /// 默认依赖（reqwest + 系统时钟）。
    ///
    /// [`default_oauth_fetch`] 在**调用时**求值：它会先看进程级测试替身，再返回基于
    /// reqwest 的默认实现，因此替身必须在构造本对象之前安装。
    pub fn new() -> Self {
        MetaOAuth {
            fetch: default_oauth_fetch(),
            clock: Arc::new(SystemAuthClock),
        }
    }

    /// 注入传输（测试用），时钟保持系统时钟。
    ///
    /// 需要固定时间线（断言 `expires` 精确值）的测试用
    /// [`MetaOAuth::with_dependencies`]。
    pub fn with_fetch(fetch: Arc<dyn OAuthFetch>) -> Self {
        MetaOAuth {
            fetch,
            clock: Arc::new(SystemAuthClock),
        }
    }

    /// 同时注入传输与时钟（测试用）。
    pub fn with_dependencies(fetch: Arc<dyn OAuthFetch>, clock: Arc<dyn AuthClock>) -> Self {
        MetaOAuth { fetch, clock }
    }
}

impl Default for MetaOAuth {
    /// 等价于 [`MetaOAuth::new`]（默认依赖）。
    fn default() -> Self {
        Self::new()
    }
}

impl MetaOAuth {
    /// `loginMeta(interaction)`。
    ///
    /// 设备流程 → 身份 token → mint。整体包在一个局部 `async` 块里，是为了在任意一步
    /// 失败后统一检查取消状态（替代 TS 的 try/catch）。
    async fn login_meta(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<OAuthCredential, AuthError> {
        let signal = interaction.signal();
        // 用局部 async 块收集结果：Rust 没有 try/catch，错误在块外统一改写。
        let result = async {
            let device = start_device_authorization(self.fetch.as_ref(), signal).await?;
            interaction.notify(&AuthEvent::DeviceCode {
                user_code: device.user_code.clone(),
                verification_uri: device.verification_uri.clone(),
                open_browser_on_confirm: None,
                copy_code_on_confirm: None,
                interval_seconds: device.interval_seconds,
                expires_in_seconds: device.expires_in_seconds,
            });
            let identity_token = poll_for_identity_token(
                Arc::clone(&self.fetch),
                Arc::clone(&self.clock),
                device,
                signal,
            )
            .await?;
            // mint 在设备流程之后发起，是登录期间的又一次网络请求（至此至少有设备授权 POST、
            // 一次或多次设备码轮询 POST 与这次 mint，共三次以上 HTTP 请求）；发之前先给 UI
            // 一条进度事件（文案是稳定契约，改动需同步测试）。
            interaction.notify(&AuthEvent::Progress {
                message: "Enabling Meta Model API access...".into(),
            });
            mint_api_key(
                self.fetch.as_ref(),
                self.clock.as_ref(),
                &identity_token,
                signal,
            )
            .await
        }
        .await;
        match result {
            Ok(credential) => Ok(credential),
            Err(error) => {
                // in-flight fetch 在 abort 时抛 DOMException；登录 UI 依赖这条消息。
                if signal.is_aborted() {
                    // 只按“是否已取消”判断：取消后即便底层报的是别的错误，也统一成这条文案。
                    Err(AuthError::new("Login cancelled"))
                } else {
                    Err(error)
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl OAuthAuth for MetaOAuth {
    /// 展示名；`providers/meta.rs` 注册的 `lazy_oauth` 用同一字符串。
    fn name(&self) -> &str {
        "Meta (Muse subscription)"
    }

    /// 由 Muse 订阅支撑，不是 API key 计费。
    fn is_subscription(&self) -> bool {
        true
    }

    /// 登录选项上显示的文字。
    fn login_label(&self) -> Option<&str> {
        Some("Sign in with Meta")
    }

    /// 交互式登录；`LoginOptions`（agent 名、设备 id）对设备码流程没有用处，被忽略。
    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
        _options: &LoginOptions,
    ) -> Result<OAuthCredential, AuthError> {
        self.login_meta(interaction).await
    }

    /// 用存储的身份 token 重新铸造 API key。
    ///
    /// 旧 `access` 不参与；`credential.refresh` 原样作为 Bearer 身份令牌发给 mint 端点。
    /// 失败语义见 `mint_api_key`（401/403 → 会话失效，需重新登录）。
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: &AbortScope,
    ) -> Result<OAuthCredential, AuthError> {
        mint_api_key(
            self.fetch.as_ref(),
            self.clock.as_ref(),
            &credential.refresh,
            signal,
        )
        .await
    }

    /// 用铸造出的 API key 作为模型请求的 `api_key`（不是 header；测试
    /// `uses_the_minted_key_as_the_request_api_key` 锁定）。
    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..ModelAuth::default()
        })
    }
}

/// 便捷构造（默认依赖）。
///
/// 与 `load.rs` 的模块级单例不同，每次调用都新建一个实例（各自解析一次
/// [`default_oauth_fetch`]）；provider 注册走 `load_meta_oauth`。
pub fn meta_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(MetaOAuth::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_detail_picks_the_first_non_empty_string() {
        let json = crate::auth::oauth::json_object(vec![
            ("error_description", Value::String("   ".into())),
            ("detail", Value::String(" details ".into())),
        ]);
        assert_eq!(error_detail(Some(&json)), ": details");
        assert_eq!(error_detail(None), "");
    }

    #[test]
    fn trusted_urls_prefer_http_schemes() {
        assert_eq!(
            trusted_http_url(Some(&Value::String(
                "https://auth.meta.com/oauth/device/".into()
            )))
            .as_deref(),
            Some("https://auth.meta.com/oauth/device/")
        );
        assert_eq!(
            trusted_http_url(Some(&Value::String("ftp://x".into()))),
            None
        );
    }
}
