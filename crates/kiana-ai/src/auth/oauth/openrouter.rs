//! auth/oauth/openrouter.ts——OpenRouter OAuth（一次性 loopback 回调 + 永久 API key 兑换）。
//!
//! OpenRouter 不发 `state`；随机 callback path 防止误请求完成登录。回调服务器内部完成
//! 兑换（页面能显示失败），同时与手动粘贴提示竞速（headless/远程场景）。
//!
//! TS: `src/auth/oauth/openrouter.ts`
//!
//! # 这个文件负责什么
//!
//! OpenRouter 的 OAuth 登录与其他 provider 不同：授权回调换回来的不是可刷新的
//! access/refresh token，而是一把**永久 API key**。因此流程只有登录，没有真正的刷新
//! （`refresh` 直接克隆凭证）；凭证的 `expires` 固定为 `Number.MAX_SAFE_INTEGER`，让
//! 标准解析路径的“快过期才刷新”判断永不触发。
//!
//! # 在系统中的位置
//!
//! `providers/openrouter.rs` 通过 `auth::helpers::lazy_oauth` 登记本流程，
//! `auth/oauth/load.rs` 的 `OPENROUTER` 单例也返回它。上层 `Models::login` 通过
//! [`crate::auth::types::OAuthAuth`] 调用 `login`；`to_auth` 把凭证的 `access` 放进
//! `ModelAuth::api_key`，base URL 与各 API 实现由 `providers/openrouter.rs` 提供。
//! 本文件不依赖任何上层模块。
//!
//! # 一次登录的执行流程
//!
//! 1. `generate_pkce()` 生成 S256 的 verifier/challenge。
//! 2. 启动一次性 loopback 回调服务器：端口 `0`（由内核挑空闲端口），路径
//!    `/oauth/callback/<UUID v4>`。OpenRouter 不回传 `state`，随机路径就是“这次登录”
//!    的凭据——不知道路径的请求只会得到 404，效果等同于别家流程的 state 校验。
//! 3. 拼授权 URL：`callback_url`、`code_challenge`、`code_challenge_method=S256`
//!    三个参数按此顺序编码。
//! 4. 先 `notify(Progress)` 报告监听地址，再 `notify(AuthUrl)` 让宿主打开浏览器。
//! 5. 浏览器回调与“粘贴最终重定向 URL/授权码”两个输入源竞争：
//!    - 浏览器先到：回调处理函数在返回网页**之前**完成 key 兑换，失败会显示在页面上
//!      （HTTP 502），同时让登录 future 返回错误；
//!    - 手动粘贴先到：登录流程自己解析 `code` 并兑换，并再发一条 `Progress`。
//! 6. 无论成败都 `close()` 回调服务器（停止接受新连接）。
//!
//! # 主要数据结构
//!
//! - [`OpenRouterOAuth`]：保存可注入的 `OAuthFetch`；每次 `login` 共用同一个实现。
//! - 凭证：`access` = 返回的 `key`，`refresh` = `""`，`expires` = `MAX_SAFE_INTEGER`。
//!
//! # 边界与失败模式
//!
//! - 预取消（signal 已 abort）时在启动服务器之前返回 `Login cancelled`，且不发任何事件；
//!   回调服务器启动失败（地址/端口问题）直接上抛，没有 Codex 那样的“回退到粘贴”。
//! - key 兑换有 30 秒超时；超时文案固定为
//!   `OpenRouter OAuth token exchange timed out`。
//! - 回调页面与登录 future 共享同一次兑换的结果：页面显示失败（502）时登录也失败。
//! - 成功响应体必须是 JSON 对象且 `key` 为非空字符串；否则分别报
//!   `OpenRouter OAuth returned invalid JSON` / `OpenRouter OAuth response carries no "key"`。
//! - 手动输入支持三种形式：完整重定向 URL、`code=...` 查询串、裸授权码；空输入报
//!   `Missing authorization code`，且不会发兑换请求。
//! - 登录整体有 5 分钟兜底超时（`LOGIN_TIMEOUT_MS`，由回调服务器计时），超时文案为
//!   `OpenRouter sign-in timed out`。
//!
//! # 注意事项
//!
//! - 常量、请求字段顺序与错误文案按 spec/ai-auth.md §2.14 逐字对齐，改动会破坏差分测试。
//! - 回调服务器绑定 `oauth_callback_host()`（环境变量 `KIANA_OAUTH_CALLBACK_HOST`，默认
//!   `127.0.0.1`）；`callback_url` 里的主机也来自它。跨机器登录时要把它设成浏览器能访问
//!   的地址，或改用粘贴。
//! - 随机回调路径依赖 UUID v4 的不可预测性；不要换成固定的 `/oauth/callback`，否则无关
//!   请求也可能触发兑换（TS 注释明确它是“防止误请求完成登录”的手段）。

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::auth::oauth::callback_server::{
    CallbackManualPrompt, CallbackOrManualInput, OAuthCallbackServerOptions,
    start_oauth_callback_server, wait_for_callback_or_manual_input,
};
use crate::auth::oauth::device_code::AuthClock;
use crate::auth::oauth::pkce::generate_pkce;
use crate::auth::oauth::{
    OAuthFetch, OAuthFetchRequest, default_oauth_fetch, form_urldecode, js_trim,
    oauth_callback_host, url_with_search,
};
use crate::auth::types::{
    AuthError, AuthEvent, AuthInteraction, LoginOptions, ModelAuth, OAuthAuth, OAuthCredential,
};
use crate::utils::abort::AbortScope;

/// `AUTHORIZE_URL`。
///
/// 浏览器授权端点；本次登录的 `callback_url` 与 PKCE 参数以 query 形式附在它后面。
const AUTHORIZE_URL: &str = "https://openrouter.ai/auth";
/// `TOKEN_URL`。
///
/// 授权码换永久 API key 的端点（OpenRouter 专用，不是常规的 `/oauth/token`）。
const TOKEN_URL: &str = "https://openrouter.ai/api/v1/auth/keys";
/// `LOGIN_TIMEOUT_MS`。
///
/// 整个登录的兜底超时，交给回调服务器计时；到点后登录以
/// `OpenRouter sign-in timed out` 失败。
const LOGIN_TIMEOUT_MS: u64 = 5 * 60 * 1000;
/// `TOKEN_EXCHANGE_TIMEOUT_MS`。
///
/// 单次 key 兑换的超时，由本文件的 `tokio::select!` 计时（回调路径与手动粘贴路径共用）。
const TOKEN_EXCHANGE_TIMEOUT_MS: u64 = 30_000;
/// `Number.MAX_SAFE_INTEGER`。
///
/// 用作“永不过期”的哨兵：`expires` 写成 `2^53-1`，上层标准的过期判断
/// （`now + 阈值` 与过期时间比较）永远达不到。它也是 JS 能精确表示的最大整数，
/// 序列化到 `auth.json` 不会丢精度（黄金 fixture 锁定 `9007199254740991`）。
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// `parseAuthorizationInput(input)`：URL → `code=` 串 → trim 后的整串。
///
/// 优先级与返回值：
/// 1. 能按绝对 URL 解析 → 只取 `code` 查询参数；没有 `code` 返回 `None`（**不会**退回把
///    整串当授权码）；
/// 2. 含子串 `code=` → 按 `URLSearchParams` 语义解码（`+` 变空格、`%XX` 解码），取第一个
///    `code` 键；缺失返回 `None`；
/// 3. 其他情况返回 `js_trim` 后的整串（裸授权码）；输入全是空白时在入口就返回 `None`。
///
/// 与 anthropic/openai-codex 共用的同名解析器不同：这里没有 `#state` 分支，也不返回
/// `state`——OpenRouter 不校验 state，防误触发依赖随机回调路径。
fn parse_authorization_input(input: &str) -> Option<String> {
    let value = js_trim(input);
    if value.is_empty() {
        return None;
    }
    if let Ok(url) = url::Url::parse(value) {
        return crate::auth::oauth::query_param(&url, "code");
    }
    if value.contains("code=") {
        return form_urldecode(value)
            .into_iter()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value);
    }
    Some(value.to_string())
}

/// `errorDetail(body)`：`error_description` → `message` → `error`（字符串）→ `error.message`。
///
/// 只接受字符串值：数字、数组等一律跳过。`error` 是对象时才取它的 `message`；数组不算
/// 对象（与 TS 的 `typeof error === "object" && !Array.isArray(error)` 一致）。
/// 返回空串是可能的，调用方用 `filter(|detail| !detail.is_empty())` 决定是否追加后缀。
fn error_detail(body: &Value) -> Option<String> {
    let field = |name: &str| crate::auth::oauth::object_field(body, name);
    if let Some(value) = field("error_description").and_then(Value::as_str) {
        return Some(value.to_string());
    }
    if let Some(value) = field("message").and_then(Value::as_str) {
        return Some(value.to_string());
    }
    if let Some(value) = field("error").and_then(Value::as_str) {
        return Some(value.to_string());
    }
    if let Some(error) = field("error").filter(|error| matches!(error, Value::Object(_)))
        && let Some(message) =
            crate::auth::oauth::object_field(error, "message").and_then(Value::as_str)
    {
        return Some(message.to_string());
    }
    None
}

/// `exchangeAuthorizationCode(code, verifier, signal)`。
///
/// 用授权码换永久 API key：
/// - 请求：`POST TOKEN_URL`，JSON body 键顺序 `code`、`code_verifier`、
///   `code_challenge_method: "S256"`；`accept` 与 `content-type` 都是 `application/json`。
/// - 并发：请求与 30 秒计时器 `select!`；调用方 signal 通过 [`OAuthFetchRequest`] 交给
///   传输层，默认 `fetch` 实现在等响应头和读响应体阶段都会响应取消。
/// - 返回：`OAuthCredential { access: key, refresh: "", expires: MAX_SAFE_INTEGER }`。
///
/// 错误（部分文案被测试逐字断言）：
/// - signal 已中止（入口检查或 fetch 失败后复查）→ `Login cancelled`；
/// - 30 秒未完成 → `OpenRouter OAuth token exchange timed out`；
/// - 2xx 但响应体不是合法 JSON → `OpenRouter OAuth returned invalid JSON`；
/// - 非 2xx → `OpenRouter OAuth key exchange failed (HTTP <status>)`，解析出的 detail
///   非空时追加 `: <detail>`；
/// - 2xx 但 `key` 缺失/为空/非字符串 → `OpenRouter OAuth response carries no "key"`。
async fn exchange_authorization_code(
    fetch: &dyn OAuthFetch,
    code: &str,
    verifier: &str,
    signal: &AbortScope,
) -> Result<OAuthCredential, AuthError> {
    // 进入网络请求前先复查取消：future 可能在 signal 中止之后才被轮询。
    if signal.is_aborted() {
        return Err(AuthError::new("Login cancelled"));
    }
    let body = crate::auth::oauth::json_object(vec![
        ("code", Value::String(code.into())),
        ("code_verifier", Value::String(verifier.into())),
        ("code_challenge_method", Value::String("S256".into())),
    ]);
    let request = OAuthFetchRequest::new(TOKEN_URL, signal)
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .post(kiana_json::js_object::stringify(&body).into_bytes());

    // 自建 controller：调用方 signal 或 30s 超时都中止请求。
    // TS 用一个 AbortController 把两种中止接到 fetch 上；Rust 用 `select!` 直接等两个
    // future——超时分支返回时，未完成的 fetch future 被丢弃（传输层随之取消请求）。
    // 调用方的 signal 仍然通过 `request.signal` 交给传输层（默认实现会响应），两者互补。
    let fetch_future = fetch.fetch(request);
    tokio::pin!(fetch_future);
    let timer = tokio::time::sleep(Duration::from_millis(TOKEN_EXCHANGE_TIMEOUT_MS));
    tokio::pin!(timer);

    let response = tokio::select! {
        // `biased`：按书写顺序轮询，因此两者同时就绪时采用响应分支。
        biased;
        result = &mut fetch_future => match result {
            Ok(response) => response,
            Err(error) => {
                if signal.is_aborted() {
                    return Err(AuthError::new("Login cancelled"));
                }
                return Err(error);
            }
        },
        _ = &mut timer => {
            return Err(AuthError::new(
                "OpenRouter OAuth token exchange timed out",
            ));
        }
    };

    // 响应体按 TS 规则宽松处理：只有 JSON 对象才当作 body；解析失败且 HTTP 成功才算
    // “invalid JSON”，非 2xx 时保留空对象，让错误文案退回“无 detail”的形式。
    let mut parsed_body = Value::Object(serde_json::Map::new());
    match response.json() {
        Ok(parsed) => {
            if matches!(parsed, Value::Object(_)) {
                parsed_body = parsed;
            }
        }
        Err(_) => {
            if response.ok() {
                return Err(AuthError::new("OpenRouter OAuth returned invalid JSON"));
            }
        }
    }

    if !response.ok() {
        // TS：`${detail ? `: ${detail}` : ""}` —— 空串 detail 不追加后缀。
        let detail = error_detail(&parsed_body).filter(|detail| !detail.is_empty());
        return Err(AuthError::new(format!(
            "OpenRouter OAuth key exchange failed (HTTP {}){}",
            response.status,
            detail
                .map(|detail| format!(": {detail}"))
                .unwrap_or_default()
        )));
    }

    let key = crate::auth::oauth::object_field(&parsed_body, "key").and_then(Value::as_str);
    let Some(key) = key.filter(|key| !key.is_empty()) else {
        return Err(AuthError::new(
            "OpenRouter OAuth response carries no \"key\"",
        ));
    };

    Ok(OAuthCredential::new(key, "", MAX_SAFE_INTEGER))
}

/// OpenRouter OAuth 流程。
///
/// 除了保存注入的 HTTP 传输实现之外无状态：每次 `login` 都会生成新的 PKCE 与随机回调
/// 路径，实例本身不保存登录进度；不同 `login` 调用之间没有共享的可变状态（各自的回调
/// 服务器监听内核分配的空闲端口）。`OAuthAuth` 要求 `Send + Sync`，这里的
/// `Arc<dyn OAuthFetch>` 满足该约束。
pub struct OpenRouterOAuth {
    fetch: Arc<dyn OAuthFetch>,
}

impl OpenRouterOAuth {
    /// 默认依赖（reqwest 传输）。
    ///
    /// OpenRouter 不需要时钟（没有 token 过期时间要计算），因此不保存 `AuthClock`。
    pub fn new() -> Self {
        OpenRouterOAuth {
            fetch: default_oauth_fetch(),
        }
    }

    /// 注入传输（测试用）；端口选择、超时等仍走真实运行时。
    pub fn with_fetch(fetch: Arc<dyn OAuthFetch>) -> Self {
        OpenRouterOAuth { fetch }
    }

    /// 同时接受传输与时钟。
    ///
    /// OpenRouter 不使用时钟（没有 token 过期时间要计算），`_clock` 被直接丢弃；
    /// 当前仓库没有调用方，测试统一用 `OpenRouterOAuth::with_fetch` 构造。
    pub fn with_dependencies(fetch: Arc<dyn OAuthFetch>, _clock: Arc<dyn AuthClock>) -> Self {
        OpenRouterOAuth { fetch }
    }
}

impl Default for OpenRouterOAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenRouterOAuth {
    /// `loginOpenRouter(interaction)`。
    ///
    /// 完整的 6 步流程见模块文档。要点：
    /// - 使用 `interaction.signal()` 作为唯一取消源；预取消会在创建回调服务器时直接失败，
    ///   不会发事件、也不会打开监听。
    /// - 回调服务器负责“浏览器回调 + 兑换”这一路，手动粘贴由本函数自己兑换；两路结果通过
    ///   `wait_for_callback_or_manual_input` 竞争，先到者生效。
    /// - `callback.close()` 在函数末尾无条件执行（Rust 没有 `finally`，因此先把结果存进
    ///   `result`，收尾后再返回）。
    async fn login_openrouter(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<OAuthCredential, AuthError> {
        let signal = interaction.signal();
        let pkce = generate_pkce();
        // OpenRouter 不发 state；随机路径防止误请求完成登录。
        let callback_path = format!("/oauth/callback/{}", uuid::Uuid::new_v4());

        // 回调处理函数在网页返回前完成兑换：fetch/verifier/signal 都必须克隆进闭包，
        // 因为 `complete` 要求 `'static` 且可能在登录 future 之外的任务里执行。
        let fetch = Arc::clone(&self.fetch);
        let verifier = pkce.verifier.clone();
        let complete_signal = signal.clone();
        let callback = start_oauth_callback_server(OAuthCallbackServerOptions::<OAuthCredential> {
            provider_name: "OpenRouter".into(),
            host: oauth_callback_host(),
            port: 0,
            path: callback_path,
            redirect_host: None,
            state: None,
            complete: Arc::new(move |code| {
                let fetch = Arc::clone(&fetch);
                let verifier = verifier.clone();
                let signal = complete_signal.clone();
                Box::pin(async move {
                    exchange_authorization_code(fetch.as_ref(), &code, &verifier, &signal).await
                })
            }),
            signal: Some(signal.clone()),
            timeout_ms: Some(LOGIN_TIMEOUT_MS),
        })
        .await?;
        let redirect_uri = callback.redirect_uri().to_string();

        let result = async {
            // 参数顺序即序列化顺序（spec/ai-auth.md §2.14）；`url_with_search` 会整体替换
            // `AUTHORIZE_URL` 上原有的 query。`callback_url` 必须与服务器实际监听地址一致。
            let authorize_url = url_with_search(
                AUTHORIZE_URL,
                &[
                    ("callback_url", &redirect_uri),
                    ("code_challenge", &pkce.challenge),
                    ("code_challenge_method", "S256"),
                ],
            )?;
            interaction.notify(&AuthEvent::Progress {
                message: format!("Listening for OpenRouter OAuth callback on {redirect_uri}"),
            });
            interaction.notify(&AuthEvent::AuthUrl {
                url: authorize_url,
                instructions: Some(
                    "Complete sign-in in your browser. If the browser is on another machine, paste the final redirect URL here."
                        .into(),
                ),
            });

            let input = wait_for_callback_or_manual_input(
                interaction,
                Some(&callback),
                CallbackManualPrompt {
                    message:
                        "Complete sign-in in your browser, or paste the authorization code / redirect URL here:"
                            .into(),
                    placeholder: redirect_uri.clone(),
                },
            )
            .await?;

            // 回调分支的凭证已经由服务器里的 `complete` 兑换好，直接返回；手动分支拿到
            // 的是原始文本，需要在这里解析并兑换（两边调用同一个兑换函数）。
            let input = match input {
                CallbackOrManualInput::Callback(credential) => return Ok(credential),
                CallbackOrManualInput::Manual(input) => input,
            };
            let Some(code) = parse_authorization_input(&input).filter(|code| !code.is_empty()) else {
                return Err(AuthError::new("Missing authorization code"));
            };
            interaction.notify(&AuthEvent::Progress {
                message: "Exchanging authorization code for an API key...".into(),
            });
            exchange_authorization_code(self.fetch.as_ref(), &code, &pkce.verifier, signal).await
        }
        .await;

        callback.close();
        result
    }
}

#[async_trait::async_trait]
impl OAuthAuth for OpenRouterOAuth {
    /// 显示名，与 `providers/openrouter.rs` 登记的 OAuth 名称相同；测试逐字断言。
    fn name(&self) -> &str {
        "OpenRouter OAuth"
    }

    /// 登录入口文案 `"Sign in with OpenRouter"`；返回 `None` 的流程由宿主使用自己的
    /// 默认文案（交互式 CLI 用 `"Sign in with an account"`）。
    fn login_label(&self) -> Option<&str> {
        Some("Sign in with OpenRouter")
    }

    /// 交互式登录；忽略 [`LoginOptions`]（OpenRouter 不需要 app 名或设备 ID）。
    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
        _options: &LoginOptions,
    ) -> Result<OAuthCredential, AuthError> {
        self.login_openrouter(interaction).await
    }

    /// OpenRouter 的 key 永久有效：不发送刷新请求，按值克隆并返回同一凭证。
    ///
    /// `_signal` 被忽略：本实现没有可取消的工作。标准解析路径的过期判断因
    /// `expires = MAX_SAFE_INTEGER` 恒为“无需刷新”，因此这个方法通常只在显式调用时
    /// 执行到；保留它是为了满足 trait 契约。
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        _signal: &AbortScope,
    ) -> Result<OAuthCredential, AuthError> {
        // OpenRouter 的 key 永久有效：返回同一凭证（Rust 侧按值克隆）。
        Ok(credential.clone())
    }

    /// 请求鉴权：把永久 key 直接作为 `api_key`，不设置 base URL 或额外 headers。
    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, AuthError> {
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            ..ModelAuth::default()
        })
    }
}

/// 便捷构造（默认依赖）。
///
/// 仓库内的 provider 走 `auth/oauth/load.rs` 的模块级单例与 `load_openrouter_oauth()`，
/// 不经过本函数；需要独立实例（而不是共享单例）的调用方可以用它。
pub fn openrouter_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(OpenRouterOAuth::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_url_form_and_bare_codes() {
        assert_eq!(
            parse_authorization_input("https://openrouter.ai/x?code=abc&state=1").as_deref(),
            Some("abc")
        );
        assert_eq!(
            parse_authorization_input("code=abc&x=1").as_deref(),
            Some("abc")
        );
        assert_eq!(
            parse_authorization_input("  manual-code  ").as_deref(),
            Some("manual-code")
        );
        assert_eq!(parse_authorization_input("   "), None);
    }

    #[test]
    fn error_detail_prefers_description() {
        let body = crate::auth::oauth::json_object(vec![
            ("message", Value::String("msg".into())),
            ("error_description", Value::String("desc".into())),
        ]);
        assert_eq!(error_detail(&body).as_deref(), Some("desc"));
        let body = crate::auth::oauth::json_object(vec![(
            "error",
            crate::auth::oauth::json_object(vec![("message", Value::String("inner".into()))]),
        )]);
        assert_eq!(error_detail(&body).as_deref(), Some("inner"));
    }
}
