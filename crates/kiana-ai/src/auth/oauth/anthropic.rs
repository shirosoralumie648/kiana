//! auth/oauth/anthropic.ts——Anthropic OAuth 流程（Claude Pro/Max）。
//!
//! 两种登录方式（`select` 提示）：浏览器回调（默认；端口被占用时回退到任意空闲端口或纯粘贴）
//! 与复制代码（headless；`code#state`）。PKCE 的 verifier 同时充当 `state`。
//!
//! # 为什么需要这个模块
//! Claude Pro/Max 订阅用的请求凭证不是长期 API key，而是一对会过期的 OAuth access/refresh
//! token。本模块实现 Anthropic 的授权码 + PKCE 流程，把登录结果换成 [`OAuthCredential`]，
//! 并通过 [`OAuthAuth`] 暴露 `login`/`refresh`/`to_auth`。`Models` 经
//! `load_anthropic_oauth`（`auth/oauth/load.rs`）拿到本流程的实例。
//!
//! # 一次登录的执行流程（browser 模式）
//! 1. 生成 PKCE；verifier 同时作为 OAuth `state`。
//! 2. 在 53692 端口起 loopback 回调服务器；失败则用端口 0 重试；再失败则没有回调服务器，
//!    只能粘贴（`redirect_uri` 退回常量 `http://localhost:53692/callback`）。
//! 3. 发 `auth_url` 事件并提示用户：在浏览器完成登录，或粘贴最终重定向 URL / 授权码。
//! 4. 手动输入按“绝对 URL → `code#state` → `code=` 查询串 → 整串当 code”的顺序解析；
//!    state 非空且不等于 verifier 时失败。
//! 5. 用 `authorization_code` 交换 token（30 秒超时），然后关闭回调服务器。
//!
//! # 注意事项
//! - `CLIENT_ID` 是 TS 源码里由 `atob` 解码出的固定 OAuth client id；授权 URL 的查询参数
//!   **顺序**参与测试断言（`authorization_url_uses_url_search_params_encoding`），不要重排。
//! - token 交换**不校验**响应字段：缺 `access_token`/`refresh_token` 不报错——Rust 侧用
//!   空串表示缺失（TS 侧是 `undefined`，JSON 序列化时字段省略），缺 `expires_in` 得到
//!   `NaN`（`expires` 随之为 `NaN`）。不要加字段校验（spec/ai-auth.md §2.10 明确 TS 无校验）。
//! - 浏览器流程在调用前就被取消时，回调服务器的 `"Login cancelled"` 会被两层回退吞掉，
//!   流程仍会发 `auth_url` 并进入手动提示（spec/ai-auth.md §2.10 步骤 3；`login_browser`
//!   内有对应说明）。
//!
//! TS: `src/auth/oauth/anthropic.ts`

use std::sync::Arc;

use serde_json::Value;

use crate::auth::oauth::callback_server::{
    CallbackManualPrompt, CallbackOrManualInput, OAuthCallbackServer, OAuthCallbackServerOptions,
    start_oauth_callback_server, wait_for_callback_or_manual_input,
};
use crate::auth::oauth::device_code::{AuthClock, SystemAuthClock};
use crate::auth::oauth::pkce::generate_pkce;
use crate::auth::oauth::{
    OAuthFetch, OAuthFetchRequest, default_oauth_fetch, form_urlencode, invalid_url_error,
    json_object, oauth_callback_host_at_init, parse_authorization_input,
};
use crate::auth::types::{
    AuthError, AuthEvent, AuthInteraction, AuthPrompt, AuthPromptRequest, LoginOptions, ModelAuth,
    OAuthAuth, OAuthCredential,
};
use crate::utils::abort::AbortScope;

/// `CLIENT_ID`（`atob("OWQxYzI1MGEtZTYxYi00NGQ5LTg4ZWQtNTk0NGQxOTYyZjVl")`）。
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// `AUTHORIZE_URL`。
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
/// `TOKEN_URL`。
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// `CALLBACK_PORT`（优先端口；被占用时回退）。
const CALLBACK_PORT: u16 = 53692;
/// `CALLBACK_PATH`。
const CALLBACK_PATH: &str = "/callback";
/// `REDIRECT_URI`（回调服务器不可用时的固定值）。
const REDIRECT_URI: &str = "http://localhost:53692/callback";
/// `COPY_CODE_REDIRECT_URI`。
const COPY_CODE_REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";
/// `ANTHROPIC_BROWSER_LOGIN_METHOD`。
const ANTHROPIC_BROWSER_LOGIN_METHOD: &str = "browser";
/// `ANTHROPIC_COPY_CODE_LOGIN_METHOD`。
const ANTHROPIC_COPY_CODE_LOGIN_METHOD: &str = "copy_code";
/// `SCOPES`。
const SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
/// `postJson` 的 30 秒超时（`AbortSignal.timeout(30_000)`）。
const POST_TIMEOUT_MS: u64 = 30_000;
/// 过期时间提前量（`5 * 60 * 1000`）。
const EXPIRY_MARGIN_MS: f64 = 5.0 * 60.0 * 1000.0;

/// Anthropic OAuth 流程。
///
/// 两个依赖都以 `Arc<dyn ...>` 注入：`fetch` 是 HTTP 传输（生产为 reqwest，测试可脚本化），
/// `clock` 是墙钟（用于把 `expires_in` 换成绝对过期时间）。实例本身无状态，可在多次登录/
/// 刷新之间复用。
pub struct AnthropicOAuth {
    fetch: Arc<dyn OAuthFetch>,
    clock: Arc<dyn AuthClock>,
}

impl AnthropicOAuth {
    /// 默认依赖（reqwest + 系统时钟）。
    pub fn new() -> Self {
        AnthropicOAuth {
            fetch: default_oauth_fetch(),
            clock: Arc::new(SystemAuthClock),
        }
    }

    /// 注入传输（测试用），时钟保持系统时钟。
    pub fn with_fetch(fetch: Arc<dyn OAuthFetch>) -> Self {
        AnthropicOAuth {
            fetch,
            clock: Arc::new(SystemAuthClock),
        }
    }

    /// 同时注入传输与时钟（测试用）。
    pub fn with_dependencies(fetch: Arc<dyn OAuthFetch>, clock: Arc<dyn AuthClock>) -> Self {
        AnthropicOAuth { fetch, clock }
    }
}

impl Default for AnthropicOAuth {
    fn default() -> Self {
        Self::new()
    }
}

/// `formatErrorDetails(error)`：`name: message` 后依次追加 `code=`/`errno=`/`cause=`/`stack=`。
///
/// 每一步都按 TS 的可观察条件追加：`code` 真值（空串省略；DOMException 的 legacy 数值码
/// 也走这里）、`errno` 已定义、`cause` 已定义（递归）、`stack` 存在。`stack` 的 V8 文本
/// （含 TS 源文件路径与行号）无法在 Rust 侧复刻，只有注入的错误才可能带值。
///
/// 输出会原样嵌入 token 交换/刷新失败的错误消息（`details=...`），因此属于用户可见的
/// 可观察契约；递归深度等于 cause 链长度。
fn format_error_details(error: &AuthError) -> String {
    let mut details = vec![format!("{}: {}", error.name, error.message)];
    // TS 只有一个 `code` 属性：普通错误的 `error.code`（Node 码/DOMException 数值码）在前，
    // `ModelsError` 的判别式码（`code` 字段）在后；空串在 JS 里是假值，继续看下一个来源。
    let code = error
        .error_code
        .as_deref()
        .filter(|code| !code.is_empty())
        .or_else(|| error.code.map(crate::error::ModelsErrorCode::as_str));
    if let Some(code) = code {
        details.push(format!("code={code}"));
    }
    if let Some(errno) = error.errno() {
        details.push(format!("errno={errno}"));
    }
    if let Some(cause) = error.cause() {
        details.push(format!("cause={}", format_error_details(cause)));
    }
    // TS：`if (error.stack)`——空串是假值，不追加。
    if let Some(stack) = error.stack().filter(|stack| !stack.is_empty()) {
        details.push(format!("stack={stack}"));
    }
    details.join("; ")
}

/// `postJson(url, body, signal)`：返回响应文本。
///
/// 请求由 `json_body` 设 `Content-Type: application/json`（只设一次，测试断言线上只有一行），
/// 再追加 `Accept: application/json`；`body` 的键顺序即 `JSON.stringify` 的字节顺序。
///
/// 超时是 TS `AbortSignal.any([signal, AbortSignal.timeout(30_000)])` 的等价物：
/// `timeout_ms` 由传输层执行，与调用方 signal 各自独立生效。
///
/// 失败路径：传输错误原样返回；非 2xx 返回固定文案
/// `HTTP request failed. status=<code>; url=<url>; body=<body>`（body 已完整读入）。
/// 2xx 时返回响应文本，可能为空串，由调用方负责解析。
async fn post_json(
    fetch: &dyn OAuthFetch,
    url: &str,
    body: &[(&str, Value)],
    signal: &AbortScope,
) -> Result<String, AuthError> {
    // 头顺序与 TS 的 `{ "Content-Type", Accept }` 一致；`json_body` 负责唯一的 Content-Type。
    let request = OAuthFetchRequest::new(url, signal)
        .json_body(&json_object(body.to_vec()))
        .header("Accept", "application/json")
        .timeout_ms(POST_TIMEOUT_MS);
    let response = fetch.fetch(request).await?;
    let response_body = response.text();
    if !response.ok() {
        return Err(AuthError::new(format!(
            "HTTP request failed. status={}; url={url}; body={response_body}",
            response.status
        )));
    }
    Ok(response_body)
}

/// `exchangeAuthorizationCode(code, state, verifier, redirectUri, signal)`。
///
/// 参数：`code` 是授权码；`state` 是回调/粘贴带回的 state（正常等于 verifier，但粘贴路径
/// 可能携带用户输入的值）；`verifier` 是 PKCE verifier；`redirect_uri` 必须与本次授权请求
/// 使用与授权请求相同的值（browser 用回调服务器的地址，copy_code 用固定常量），否则 token 交换会失败。
///
/// 请求体键顺序参与字节级比对：`grant_type`、`client_id`、`code`、`state`、`redirect_uri`、
/// `code_verifier`（spec/ai-auth.md §2.10）。返回的 `expires` 是
/// `now + expires_in * 1000 - EXPIRY_MARGIN_MS`，即比服务端给的时间提前 5 分钟。
///
/// 失败：传输错误/非 2xx 包装成 `Token exchange request failed. ... details=...`；响应不是
/// 合法 JSON 时包装成 `Token exchange returned invalid JSON. ... details=SyntaxError: ...`；
/// 两种文案都保留响应体或底层错误链（测试与用户可见输出依赖它们）。
async fn exchange_authorization_code(
    fetch: &dyn OAuthFetch,
    clock: &dyn AuthClock,
    code: &str,
    state: &str,
    verifier: &str,
    redirect_uri: &str,
    signal: &AbortScope,
) -> Result<OAuthCredential, AuthError> {
    let body = [
        ("grant_type", Value::String("authorization_code".into())),
        ("client_id", Value::String(CLIENT_ID.into())),
        ("code", Value::String(code.into())),
        ("state", Value::String(state.into())),
        ("redirect_uri", Value::String(redirect_uri.into())),
        ("code_verifier", Value::String(verifier.into())),
    ];
    let response_body = post_json(fetch, TOKEN_URL, &body, signal).await.map_err(|error| {
        AuthError::new(format!(
            "Token exchange request failed. url={TOKEN_URL}; redirect_uri={redirect_uri}; response_type=authorization_code; details={}",
            format_error_details(&error)
        ))
    })?;

    let token_data = kiana_json::js_parse::parse(&response_body).map_err(|error| {
        AuthError::new(format!(
            "Token exchange returned invalid JSON. url={TOKEN_URL}; body={response_body}; details={}",
            format_error_details(&AuthError::with_name("SyntaxError", error.message))
        ))
    })?;

    let access = token_data
        .get("access_token")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let refresh = token_data
        .get("refresh_token")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // TS 不校验 `expires_in` 的类型，直接做 `tokenData.expires_in * 1000`（JS `ToNumber`）。
    let expires_in = crate::auth::oauth::js_to_number_or_nan(token_data.get("expires_in"));
    Ok(OAuthCredential::new(
        access,
        refresh,
        clock.now_ms() + expires_in * 1000.0 - EXPIRY_MARGIN_MS,
    ))
}

/// `refreshAnthropicToken(refreshToken, signal)`。
///
/// 请求体只有 `grant_type`/`client_id`/`refresh_token`——**不带 `scope`**（集成测试
/// `omits_scope_from_refresh_token_requests` 锁定这一点）。响应处理与授权码交换相同：
/// 缺字段得到空串/`NaN`，`expires` 同样提前 5 分钟。
///
/// 错误文案以 `Anthropic token refresh ...` 开头，便于和授权码交换的失败区分。
async fn refresh_anthropic_token(
    fetch: &dyn OAuthFetch,
    clock: &dyn AuthClock,
    refresh_token: &str,
    signal: &AbortScope,
) -> Result<OAuthCredential, AuthError> {
    let body = [
        ("grant_type", Value::String("refresh_token".into())),
        ("client_id", Value::String(CLIENT_ID.into())),
        ("refresh_token", Value::String(refresh_token.into())),
    ];
    let response_body = post_json(fetch, TOKEN_URL, &body, signal)
        .await
        .map_err(|error| {
            AuthError::new(format!(
                "Anthropic token refresh request failed. url={TOKEN_URL}; details={}",
                format_error_details(&error)
            ))
        })?;

    let data = kiana_json::js_parse::parse(&response_body).map_err(|error| {
        AuthError::new(format!(
            "Anthropic token refresh returned invalid JSON. url={TOKEN_URL}; body={response_body}; details={}",
            format_error_details(&AuthError::with_name("SyntaxError", error.message))
        ))
    })?;

    let access = data
        .get("access_token")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let refresh = data
        .get("refresh_token")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // TS 不校验 `expires_in` 的类型，直接做 `data.expires_in * 1000`（JS `ToNumber`）。
    let expires_in = crate::auth::oauth::js_to_number_or_nan(data.get("expires_in"));
    Ok(OAuthCredential::new(
        access,
        refresh,
        clock.now_ms() + expires_in * 1000.0 - EXPIRY_MARGIN_MS,
    ))
}

/// `startCallbackServer(port)`（`complete: async (code) => code`）。
///
/// `complete` 只把 code 原样返回：真正的 token 交换延后到
/// `wait_for_callback_or_manual_input` 之后，让浏览器回调与手动粘贴共用同一段交换逻辑。
///
/// `state` 固定为 PKCE verifier（spec/ai-auth.md §2.10：TS 把 PKCE verifier 同时用作
/// `state`），HTTP 层因此也会拒绝 state 不符的迟到/伪造请求。`host` 来自
/// `KIANA_OAUTH_CALLBACK_HOST`（模块初始化时读取一次），
/// `redirect_host` 固定为 `localhost`（绑定 `127.0.0.1`，但让浏览器访问 `localhost`）。
async fn start_callback_server(
    port: u16,
    verifier: &str,
    signal: &AbortScope,
) -> Result<OAuthCallbackServer<String>, AuthError> {
    start_oauth_callback_server(OAuthCallbackServerOptions::<String> {
        provider_name: "Anthropic".into(),
        host: oauth_callback_host_at_init().to_string(),
        port,
        path: CALLBACK_PATH.into(),
        redirect_host: Some("localhost".into()),
        state: Some(verifier.to_string()),
        complete: Arc::new(|code| Box::pin(async move { Ok(code) })),
        signal: Some(signal.clone()),
        timeout_ms: None,
    })
    .await
}

/// 授权 URL（browser 与 copy_code 共用；`redirect_uri` 不同）。
///
/// 查询参数顺序固定（`code`、`client_id`、`response_type`、`redirect_uri`、`scope`、
/// `code_challenge`、`code_challenge_method`、`state`），测试逐字断言完整 URL。
/// `code=true` 是 Anthropic 的附加参数；`code_challenge_method=S256` 与 `state`（=verifier）
/// 共同构成 PKCE 绑定。
fn authorization_url(
    redirect_uri: &str,
    challenge: &str,
    verifier: &str,
) -> Result<String, AuthError> {
    let query = form_urlencode(&[
        ("code", "true"),
        ("client_id", CLIENT_ID),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri),
        ("scope", SCOPES),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", verifier),
    ]);
    let mut url = url::Url::parse(AUTHORIZE_URL).map_err(invalid_url_error)?;
    url.set_query(Some(&query));
    Ok(url.into())
}

impl AnthropicOAuth {
    /// `loginAnthropic(interaction)`（browser 模式）。
    ///
    /// 先尽力启动 loopback 回调服务器，再在等待期间同时接受浏览器回调和手动粘贴。无论
    /// 成功还是失败都会在返回前 `close()` 服务器（相当于 TS 的 `finally`），避免遗留监听
    /// 端口；`close()` 对已 settle 的等待是幂等的。
    ///
    /// 失败模式：state 不匹配 → `OAuth state mismatch`；code 缺失或为空串 →
    /// `Missing authorization code`；交换失败按其包装文案返回。
    async fn login_browser(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<OAuthCredential, AuthError> {
        let signal = interaction.signal();
        // 每次登录都重新生成 PKCE；verifier 同时作为 state 传给回调服务器与授权 URL。
        let pkce = generate_pkce();

        // 端口被占 → 任意空闲端口；再失败 → 没有回调服务器（只能粘贴）。
        // 注意：预取消时 `start` 的错误同样被这两个 catch 吞掉（见 spec §2.10 步骤 3）。
        let callback = match start_callback_server(CALLBACK_PORT, &pkce.verifier, signal).await {
            Ok(server) => Some(server),
            Err(_) => start_callback_server(0, &pkce.verifier, signal).await.ok(),
        };
        // 有服务器就用它实际绑定的端口；没有则退回固定常量（该端口此刻可能不可用，
        // 但授权 URL 仍必须与粘贴模式使用的 redirect_uri 一致）。
        let redirect_uri = callback
            .as_ref()
            .map(|callback| callback.redirect_uri().to_string())
            .unwrap_or_else(|| REDIRECT_URI.to_string());

        // 从这一行起的所有错误路径都会经过末尾的 callback.close()（TS 的 finally 语义）。
        let result = async {
            let url = authorization_url(&redirect_uri, &pkce.challenge, &pkce.verifier)?;
            interaction.notify(&AuthEvent::AuthUrl {
                url,
                instructions: Some(
                    "Complete login in your browser. If the browser is on another machine, paste the final redirect URL here."
                        .into(),
                ),
            });

            let input = wait_for_callback_or_manual_input(
                interaction,
                callback.as_ref(),
                CallbackManualPrompt {
                    message:
                        "Complete login in your browser, or paste the authorization code / redirect URL here:"
                            .into(),
                    placeholder: redirect_uri.clone(),
                },
            )
            .await?;

            // 回调分支的 state 由回调服务器按 verifier 校验过，这里直接使用 verifier；
            // 手动分支需要自己解析并校验。
            let (code, state) = match input {
                CallbackOrManualInput::Callback(value) => (Some(value), pkce.verifier.clone()),
                CallbackOrManualInput::Manual(input) => {
                    let parsed = parse_authorization_input(&input);
                    // TS 用真值判断：`if (parsed.state && parsed.state !== verifier)`。
                    // 空串 state 不触发 mismatch，且 `?? verifier` 保留空串（非 nullish）。
                    if let Some(state) = &parsed.state
                        && !state.is_empty()
                        && state != &pkce.verifier
                    {
                        return Err(AuthError::new("OAuth state mismatch"));
                    }
                    let state = parsed.state.clone().unwrap_or_else(|| pkce.verifier.clone());
                    (parsed.code, state)
                }
            };

            // 空串与缺失同样按“没有授权码”处理（`filter` 先于 `Some` 解构）。
            let Some(code) = code.filter(|code| !code.is_empty()) else {
                return Err(AuthError::new("Missing authorization code"));
            };
            interaction.notify(&AuthEvent::Progress {
                message: "Exchanging authorization code for tokens...".into(),
            });
            exchange_authorization_code(
                self.fetch.as_ref(),
                self.clock.as_ref(),
                &code,
                &state,
                &pkce.verifier,
                &redirect_uri,
                signal,
            )
            .await
        }
        .await;

        // 先拿到结果再关闭：成功时 wait() 已 settle（close 无副作用）；授权 URL 构造失败等
        // 早期错误时 wait() 尚未 settle，close 会让它失败，但此刻已无人等待。
        if let Some(callback) = &callback {
            callback.close();
        }
        result
    }

    /// `loginAnthropicCopyCode(interaction)`。
    ///
    /// headless/SSH 场景没有 loopback 服务器：`redirect_uri` 是 Anthropic 的固定回调地址，
    /// 用户自行打开授权 URL，再把页面显示的 `code#state` 粘贴回来。
    ///
    /// 注意 prompt 的 signal 直接复用整个登录流程的 signal（TS 就是这样，没有派生新
    /// signal），因此取消登录会同时取消这次输入。
    async fn login_copy_code(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<OAuthCredential, AuthError> {
        let signal = interaction.signal();
        let pkce = generate_pkce();
        let url = authorization_url(COPY_CODE_REDIRECT_URI, &pkce.challenge, &pkce.verifier)?;
        interaction.notify(&AuthEvent::AuthUrl {
            url,
            instructions: Some(
                "Complete login in your browser, then copy the code Anthropic shows and paste it here."
                    .into(),
            ),
        });

        // 整个登录的 signal 直接交给这次 prompt（不是派生 signal），取消会结束登录。
        let request = AuthPromptRequest::with_signal(
            AuthPrompt::ManualCode {
                message: "Paste the code Anthropic shows after you sign in:".into(),
                placeholder: Some("code#state".into()),
            },
            signal.clone(),
        );
        let input = interaction.prompt(request).await?;
        let parsed = parse_authorization_input(&input);
        // TS 用真值判断：空串 state 视为“无 state”，不触发 mismatch。
        if let Some(state) = &parsed.state
            && !state.is_empty()
            && state != &pkce.verifier
        {
            return Err(AuthError::new("OAuth state mismatch"));
        }
        // 空串与缺失同样按“没有授权码”处理。
        let Some(code) = parsed.code.filter(|code| !code.is_empty()) else {
            return Err(AuthError::new("Missing authorization code"));
        };
        interaction.notify(&AuthEvent::Progress {
            message: "Exchanging authorization code for tokens...".into(),
        });
        // state 的 `?? verifier` 语义：`Some("")` 保留空串，只有 `None` 才回退到 verifier。
        exchange_authorization_code(
            self.fetch.as_ref(),
            self.clock.as_ref(),
            &code,
            parsed.state.as_deref().unwrap_or(&pkce.verifier),
            &pkce.verifier,
            COPY_CODE_REDIRECT_URI,
            signal,
        )
        .await
    }
}

#[async_trait::async_trait]
impl OAuthAuth for AnthropicOAuth {
    /// 展示名。交互式登录选择器（`AuthSelectorMethod::name`）用它作为登录方式名与搜索
    /// 文本；`Models` 本身不使用它（登录错误文案用 provider 名），状态标签来自
    /// `AuthCheck.source`。
    fn name(&self) -> &str {
        "Anthropic (Claude Pro/Max)"
    }

    /// 这是订阅制登录（Claude Pro/Max），不是按量计费的 API key。
    fn is_subscription(&self) -> bool {
        true
    }

    /// 先让用户选择登录方式，再分派到 browser / copy_code。
    ///
    /// 未知返回值直接报错，不静默回退到 browser（避免把无效输入当成默认选择）。`LoginOptions` 在本流程
    /// 没有用途：不需要 device id，也不发送 agent name。
    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
        _options: &LoginOptions,
    ) -> Result<OAuthCredential, AuthError> {
        let method = interaction
            .prompt(AuthPromptRequest::new(AuthPrompt::Select {
                message: "Select Anthropic login method:".into(),
                options: vec![
                    crate::auth::types::AuthSelectOption {
                        id: ANTHROPIC_BROWSER_LOGIN_METHOD.into(),
                        label: "Browser login (default)".into(),
                        description: None,
                    },
                    crate::auth::types::AuthSelectOption {
                        id: ANTHROPIC_COPY_CODE_LOGIN_METHOD.into(),
                        label: "Copy code login (headless)".into(),
                        description: None,
                    },
                ],
            }))
            .await?;

        if method == ANTHROPIC_COPY_CODE_LOGIN_METHOD {
            return self.login_copy_code(interaction).await;
        }
        if method != ANTHROPIC_BROWSER_LOGIN_METHOD {
            return Err(AuthError::new(format!(
                "Unknown Anthropic login method: {method}"
            )));
        }
        self.login_browser(interaction).await
    }

    /// 交换 refresh token。`Models` 在凭证存储的串行门内调用它，实现本身不写存储。
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: &AbortScope,
    ) -> Result<OAuthCredential, AuthError> {
        refresh_anthropic_token(
            self.fetch.as_ref(),
            self.clock.as_ref(),
            &credential.refresh,
            signal,
        )
        .await
    }

    /// 请求时把 access token 当作 `apiKey` 发送；headers/baseUrl 保持默认。
    ///
    /// 无副作用且只依赖传入凭证（每次解析存储的 OAuth auth 都会调用它）。
    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..ModelAuth::default()
        })
    }
}

/// 便捷构造（默认依赖）。
pub fn anthropic_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(AnthropicOAuth::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_url_uses_url_search_params_encoding() {
        let url = authorization_url("http://localhost:53692/callback", "challenge", "verifier")
            .expect("url");
        assert_eq!(
            url,
            "https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A53692%2Fcallback&scope=org%3Acreate_api_key+user%3Aprofile+user%3Ainference+user%3Asessions%3Aclaude_code+user%3Amcp_servers+user%3Afile_upload&code_challenge=challenge&code_challenge_method=S256&state=verifier"
        );
    }

    #[test]
    fn format_error_details_includes_code() {
        let mut error = AuthError::new("boom");
        error.error_code = Some("EADDRINUSE".into());
        assert_eq!(format_error_details(&error), "Error: boom; code=EADDRINUSE");
        // 空串 code / 空串 stack 在 TS 里都是假值，不追加。
        error.error_code = Some(String::new());
        assert_eq!(format_error_details(&error), "Error: boom");
        let error = error.with_stack("");
        assert_eq!(format_error_details(&error), "Error: boom");
    }

    /// 回归（评审发现）：TS 的 `formatErrorDetails` 会递归追加 `code=`/`errno=`/`cause=`/
    /// `stack=`；期望字符串来自 Node 26 实测（probe 里给 fetch 注入同形状的错误）。
    #[test]
    fn format_error_details_matches_node_for_a_cause_chain() {
        let inner = AuthError::new("connect ECONNREFUSED 127.0.0.1:443")
            .with_error_code("ECONNREFUSED")
            .with_errno("-111")
            .with_stack(
                "Error: connect ECONNREFUSED 127.0.0.1:443\n    at TCPConnectWrap.afterConnect [as oncomplete] (node:net:1606:16)",
            );
        let outer = AuthError::with_name("TypeError", "fetch failed")
            .with_cause(inner)
            .with_stack(
                "TypeError: fetch failed\n    at node:internal/deps/undici/undici:14902:13",
            );
        assert_eq!(
            format_error_details(&outer),
            "TypeError: fetch failed; cause=Error: connect ECONNREFUSED 127.0.0.1:443; code=ECONNREFUSED; errno=-111; stack=Error: connect ECONNREFUSED 127.0.0.1:443\n    at TCPConnectWrap.afterConnect [as oncomplete] (node:net:1606:16); stack=TypeError: fetch failed\n    at node:internal/deps/undici/undici:14902:13"
        );
    }

    /// 回归：DOMException 的 legacy `code`（Node 26 实测 `code === 23`）进入 `details`。
    #[test]
    fn format_error_details_includes_dom_exception_codes() {
        let error = AuthError::with_name("TimeoutError", "The operation was aborted due to timeout")
            .with_error_code("23")
            .with_stack(
                "TimeoutError: The operation was aborted due to timeout\n    at node:internal/deps/undici/undici:16114:31",
            );
        assert_eq!(
            format_error_details(&error),
            "TimeoutError: The operation was aborted due to timeout; code=23; stack=TimeoutError: The operation was aborted due to timeout\n    at node:internal/deps/undici/undici:16114:31"
        );
    }

    #[test]
    fn trims_manual_input_before_parsing() {
        assert_eq!(crate::auth::oauth::js_trim("  x "), "x");
    }
}
