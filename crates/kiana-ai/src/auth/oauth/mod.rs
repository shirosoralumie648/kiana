//! auth/oauth 目录：各 provider 的 OAuth 流程。
//!
//! 逐文件对应 `packages/ai/src/auth/oauth/**`。本模块额外提供全部流程共享的两样东西：
//!
//! - [`OAuthFetch`]：TS 全局 `fetch` 的注入点。九个流程在 TS 里都直接调 `fetch`，测试用
//!   `vi.stubGlobal("fetch", ...)` 顶替；Rust 侧把传输抽象成一个小 trait，默认实现走
//!   [`crate::http::HttpClient`]（`http/reqwest_client.rs`），测试注入记录/脚本化的实现。
//! - JS 语义小工具：`URLSearchParams` 编码（[`form_urlencode`]）、`atob` 的严格标准 base64
//!   （[`atob_standard`]）、`String.prototype.trim`（[`js_trim`]）等。它们只在本子树内使用。
//!
//! TS 的九个 `load*OAuth()` 动态 import 见 [`load`]；`bun-oauth.ts` 见 [`crate::bun_oauth`]。
//!
//! # 为什么会有这个没有对应 TS 文件的模块
//!
//! TS 侧没有共享的 `oauth/mod.ts`：`parseAuthorizationInput` 在 anthropic 与 openai-codex
//! 中各复制一份，表单编码、`trim`、URL 解析直接调用 JS 运行时，`fetch` 由测试用
//! `vi.stubGlobal` 全局替换。Rust 侧集中到本文件，原因有二：
//!
//! 1. 这些 JS 语义必须逐字节兼容（spec/ai-auth.md §6.2、§8.4），集中实现才能只维护一份
//!    差分用例，避免九个流程各自漂移；
//! 2. Rust 没有全局可替换的 `fetch`，改用 `Arc<dyn OAuthFetch>` 注入 +
//!    进程级测试替身（`set_default_oauth_fetch_for_tests`）。
//!
//! # 一次默认请求的执行流程
//!
//! 1. 流程用 [`OAuthFetchRequest`] 描述请求（URL、方法、头、body、超时、取消信号）。
//! 2. [`OAuthFetch::fetch`] 返回 boxed future；[`default_oauth_fetch`] 的默认实现先检查
//!    调用方是否已取消，再取进程级 [`crate::http::HttpClient`] 发请求（超时与取消都交给它），
//!    最后把响应体**读满**后返回 [`OAuthFetchResponse`]。
//! 3. 非 2xx 的判定留给流程（provider 要读错误体拼文案）；解析用
//!    [`OAuthFetchResponse::json`] / [`OAuthFetchResponse::text`]。
//!
//! # 注意事项
//!
//! - 响应体在 future 完成前完整缓冲进内存。调用方都是令牌交换、配置读取、模型目录这类 API
//!   请求；不要把大文件下载交给本层。
//! - 超时覆盖**等响应头与读响应体**两个阶段，与 Node 的 `AbortSignal.timeout(ms)` 一致
//!   （细节见 `http::reqwest_client` 的模块文档）。
//! - 取消的真实原因存在 [`AbortScope`] 里，`CancellationToken` 只传“已取消”这一事实；
//!   所以默认实现在映射错误前会回读 `signal.reason()`，保留 `abort(reason)` 的自定义错误。
//! - 请求头与响应头的表示不对称：请求头保留调用方给的顺序和大小写（查找时忽略大小写），
//!   响应头键统一小写（对齐 WHATWG `Headers`）。
//! - [`testing`] 里的替身没有 `#[cfg(test)]`：`kiana-ai` 自己的集成测试（`tests/*.rs`）要用，
//!   而集成测试是独立 crate，看不到本 crate 的 `cfg(test)` 项。其他 crate 的测试若只需要
//!   注入传输，用本模块根下的 [`OAuthFetchFn`] 配 `set_default_oauth_fetch_for_tests`，
//!   不需要 [`testing`]。

use std::pin::Pin;
use std::sync::{Arc, LazyLock, Mutex};

use futures::StreamExt;
use indexmap::IndexMap;
use serde_json::Value;
use url::Url;

use crate::auth::types::AuthError;
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::utils::abort::{AbortReason, AbortScope};

// --- 子模块 ---
pub mod anthropic;
pub mod callback_server;
pub mod device_code;
pub mod github_copilot;
pub mod kimi_coding;
pub mod load;
pub mod meta;
pub mod openai_chatgpt;
pub mod openai_codex;
pub mod openrouter;
pub mod pkce;
pub mod radius;
pub mod xai;

pub use anthropic::AnthropicOAuth;
pub use github_copilot::GitHubCopilotOAuth;
pub use kimi_coding::KimiCodingOAuth;
pub use meta::MetaOAuth;
pub use openai_chatgpt::OpenAIChatGPTOAuth;
pub use openai_codex::OpenAICodexOAuth;
pub use openrouter::OpenRouterOAuth;
pub use radius::{RadiusOAuth, RadiusOAuthOptions, create_radius_oauth};
pub use xai::XaiOAuth;

// ---------------------------------------------------------------------------
// fetch 注入点（TS 的全局 fetch）
// ---------------------------------------------------------------------------

/// 一次 `fetch` 请求。
///
/// TS 的 `fetch(input, init)` 只用到这些字段；header 名大小写不敏感（流程按源码字面量传入，
/// 与 Node 的 `Headers` 一样在传输层按小写处理）。
///
/// 构造走链式 builder：[`new`](Self::new) 之后用 [`header`](Self::header)、
/// [`post`](Self::post)、[`json_body`](Self::json_body)、[`timeout_ms`](Self::timeout_ms)
/// 追加内容。这些方法消费并返回 `self`，便于流程一次成型；`method` 与 `body` 不在这里
/// 做一致性校验，实际发送语义由 `HttpClient` 实现决定。
#[derive(Debug, Clone)]
pub struct OAuthFetchRequest {
    /// 请求 URL（流程已经用 WHATWG URL 规范化）。
    pub url: String,
    /// HTTP 方法（`"GET"` / `"POST"`）。
    pub method: String,
    /// 请求头（保留调用方给的顺序与大小写）。
    pub headers: Vec<(String, String)>,
    /// 请求体字节。
    pub body: Option<Vec<u8>>,
    /// `AbortSignal.timeout(ms)` 合成的超时；`None` 表示不设超时。
    pub timeout_ms: Option<u64>,
    /// 调用方 signal（`AbortSignal`）。
    pub signal: AbortScope,
}

impl OAuthFetchRequest {
    /// 新请求（默认 `GET`、无 body、无超时）。
    pub fn new(url: impl Into<String>, signal: &AbortScope) -> Self {
        OAuthFetchRequest {
            url: url.into(),
            method: "GET".into(),
            headers: Vec::new(),
            body: None,
            timeout_ms: None,
            signal: signal.clone(),
        }
    }

    /// 追加一个请求头。
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// 设置方法与 body。
    pub fn post(mut self, body: Vec<u8>) -> Self {
        self.method = "POST".into();
        self.body = Some(body);
        self
    }

    /// 设置 `Content-Type: application/x-www-form-urlencoded` 的 [`form_urlencode`] body。
    pub fn form_body(self, fields: &[(&str, &str)]) -> Self {
        self.header("Content-Type", "application/x-www-form-urlencoded")
            .post(form_urlencode(fields).into_bytes())
    }

    /// 设置 `Content-Type: application/json` 的 `JSON.stringify(value)` body。
    pub fn json_body(self, value: &Value) -> Self {
        self.header("Content-Type", "application/json")
            .post(kiana_json::js_object::stringify(value).into_bytes())
    }

    /// 设置超时（`AbortSignal.timeout`）。
    pub fn timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = Some(timeout_ms);
        self
    }

    /// 读取同名请求头的值（大小写不敏感，取第一个）。
    pub fn header_value(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// body 的 UTF-8 文本视图（测试断言用）。
    pub fn body_text(&self) -> String {
        self.body
            .as_deref()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default()
    }
}

/// 一次 `fetch` 响应。body 在 `fetch` 返回前已完整读取（见模块文档的差异说明）。
///
/// 因为 body 已经缓冲，`Clone` 会复制整份字节；测试工具 `testing::respond` 依赖这一点
/// 复用同一个固定响应。
#[derive(Debug, Clone)]
pub struct OAuthFetchResponse {
    /// HTTP 状态码。
    pub status: u16,
    /// HTTP reason phrase（`response.statusText`；测试里的 `new Response` 为 `""`）。
    ///
    /// 默认实现只能用 `http::StatusCode::canonical_reason` 从状态码反推，因此服务端自定义的
    /// 短语会丢失，未知或非法状态码得到 `""`。Node 在 HTTP/2 响应下 `statusText` 为空串
    /// （协议本身没有 reason phrase）；但默认实现拿到的响应元数据只含状态码与头、不含协议
    /// 版本，无法区分 HTTP/1.1 与 HTTP/2，已知状态码仍会返回 canonical 短语
    /// （例如 HTTP/2 的 200 也是 `"OK"`）。
    pub status_text: String,
    /// 响应头（键为小写）。
    pub headers: IndexMap<String, String>,
    /// 响应体原始字节。
    pub body: Vec<u8>,
}

impl OAuthFetchResponse {
    /// 空响应（无 header、无 body）。
    pub fn new(status: u16, status_text: impl Into<String>, body: Vec<u8>) -> Self {
        OAuthFetchResponse {
            status,
            status_text: status_text.into(),
            headers: IndexMap::new(),
            body,
        }
    }

    /// `new Response(JSON.stringify(body), { status })`。
    pub fn json_response(status: u16, body: &Value) -> Self {
        let mut response = OAuthFetchResponse::new(
            status,
            "",
            kiana_json::js_object::stringify(body).into_bytes(),
        );
        response
            .headers
            .insert("content-type".into(), "application/json".into());
        response
    }

    /// `new Response(text, { status })`。
    pub fn text_response(status: u16, body: impl Into<String>) -> Self {
        OAuthFetchResponse::new(status, "", body.into().into_bytes())
    }

    /// 追加一个响应头（键按小写保存，与 `response.headers.get` 一致）。
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers
            .insert(name.into().to_lowercase(), value.into());
        self
    }

    /// 替换 reason phrase。
    pub fn with_status_text(mut self, status_text: impl Into<String>) -> Self {
        self.status_text = status_text.into();
        self
    }

    /// `response.ok`（200–299）。
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// `response.headers.get(name)`。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_lowercase()).map(String::as_str)
    }

    /// `await response.text()`。
    ///
    /// Node 的 `text()` 按 UTF-8 解码并去掉开头的 BOM（`ignoreBOM: false` 的默认行为）。
    pub fn text(&self) -> String {
        let decoded = String::from_utf8_lossy(&self.body).into_owned();
        decoded
            .strip_prefix('\u{feff}')
            .map(str::to_owned)
            .unwrap_or(decoded)
    }

    /// `await response.json()`：`JSON.parse(await response.text())`。
    ///
    /// 失败时返回 V8 文案的 `SyntaxError`（[`kiana_json::ParseError::message`]）。
    pub fn json(&self) -> Result<Value, AuthError> {
        let text = self.text();
        kiana_json::js_parse::parse(&text)
            .map_err(|error| AuthError::with_name("SyntaxError", error.message))
    }
}

/// `fetch` 返回的 future。
///
/// 必须是 `Send + 'static`：实现不能借用 `self`，需要的数据要提前 `clone`/`move` 进
/// `async` 块。默认实现就是这样把 `request` move 进去的；`testing::MockFetch` 自身只记录
/// 请求，返回的 `'static` future 由调用方提供的 handler 负责构造。
pub type OAuthFetchFuture = Pin<
    Box<dyn std::future::Future<Output = Result<OAuthFetchResponse, AuthError>> + Send + 'static>,
>;

/// `fetch` 的 Rust 等价物（`ARCHITECTURE.md` §4.2：取消用 [`AbortScope`]）。
///
/// 默认实现见 [`default_oauth_fetch`]（基于 `http::HttpClient`）；测试注入脚本化实现，
/// 对应 TS 测试里的 `vi.stubGlobal("fetch", ...)`。
///
/// 各流程结构体在构造时保存 `Arc<dyn OAuthFetch>`，之后整个登录流程共用同一实现。
/// `Arc<dyn OAuthFetch>` 自身也实现了本 trait，便于把注入点继续向下传。
pub trait OAuthFetch: Send + Sync {
    /// 发起请求。
    fn fetch(&self, request: OAuthFetchRequest) -> OAuthFetchFuture;
}

impl OAuthFetch for Arc<dyn OAuthFetch> {
    fn fetch(&self, request: OAuthFetchRequest) -> OAuthFetchFuture {
        (**self).fetch(request)
    }
}

/// 基于闭包的 [`OAuthFetch`] 适配器（测试用）。
///
/// 元组字段 `.0` 就是请求处理器。跨 crate 的测试用它把固定响应包成
/// [`default_oauth_fetch`] 的进程级替身（配合同模块的 `set_default_oauth_fetch_for_tests`）；
/// 生产默认实现是私有的 `DefaultOAuthFetch`，不经过本类型。
pub struct OAuthFetchFn<F>(pub F);

impl<F> OAuthFetch for OAuthFetchFn<F>
where
    F: Fn(OAuthFetchRequest) -> OAuthFetchFuture + Send + Sync,
{
    fn fetch(&self, request: OAuthFetchRequest) -> OAuthFetchFuture {
        (self.0)(request)
    }
}

/// 进程级默认 HTTP 客户端（reqwest）。构造失败时把原因保留为 `String`，在 fetch 时转成错误。
///
/// `LazyLock` 只求值一次，`Err` 也会被缓存：TLS 后端初始化失败后，本进程之后每次 fetch
/// 都返回同一个 `TypeError`，直到进程重启（没有重试构造的路径）。
static DEFAULT_HTTP_CLIENT: LazyLock<Result<Arc<dyn HttpClient>, String>> = LazyLock::new(|| {
    crate::http::reqwest_client::ReqwestHttpClient::new()
        .map(|client| client.shared())
        .map_err(|error| error.message)
});

/// 测试注入的进程级 `fetch`（对应 TS 的 `vi.stubGlobal("fetch", ...)`）。
///
/// 全局可变状态：同一测试二进制里的用例并行运行时共享它，替换方应在结束时还原。
/// 锁中毒时取回内层值（与仓库其他全局表一致），不让一个 panic 的测试永久毒化后续用例。
static DEFAULT_OAUTH_FETCH_OVERRIDE: Mutex<Option<Arc<dyn OAuthFetch>>> = Mutex::new(None);

/// 仅供测试：替换 [`default_oauth_fetch`]（`None` 恢复真实的 reqwest 实现）。
///
/// 流程结构体在构造时调用 [`default_oauth_fetch`] 并保存 `Arc`，所以替换要发生在创建流程
/// 实例之前；[`crate::providers::openrouter`] 与 [`crate::providers::radius_config`] 里的
/// 函数则在**每次被调用时**才读取 `default_oauth_fetch()`，它们分别在 OpenRouter 模型目录
/// 刷新与 Radius 模型刷新时执行，因此替换要到下一次刷新/调用才生效，而不是对每个模型请求
/// 都重新读取。跨 crate 的集成测试各自是独立进程，互不影响。
#[doc(hidden)]
pub fn set_default_oauth_fetch_for_tests(fetch: Option<Arc<dyn OAuthFetch>>) {
    let mut guard = DEFAULT_OAUTH_FETCH_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = fetch;
}

/// 默认 `fetch`：`http/reqwest_client.rs` 的 [`HttpClient`] 实现。
///
/// 与 TS 的差异只在错误名字上：Node 的传输失败是 `TypeError: fetch failed`（带 `cause`），
/// Rust 侧保留 `fetch failed` 文本但丢失 cause 链；取消/超时分别映射为
/// `AbortError`/`TimeoutError`（文案与 Node 一致）。
///
/// 先看进程级测试替身，没有才返回无状态的 `DefaultOAuthFetch`。真正的客户端由
/// `DEFAULT_HTTP_CLIENT` 惰性构造并复用，因此连续多次登录不会重复建连接池。
pub fn default_oauth_fetch() -> Arc<dyn OAuthFetch> {
    if let Some(fetch) = DEFAULT_OAUTH_FETCH_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    {
        return fetch;
    }
    Arc::new(DefaultOAuthFetch)
}

/// 无状态的默认 [`OAuthFetch`]：每个请求都委托给 `execute_default_fetch`。
struct DefaultOAuthFetch;

impl OAuthFetch for DefaultOAuthFetch {
    fn fetch(&self, request: OAuthFetchRequest) -> OAuthFetchFuture {
        Box::pin(async move { execute_default_fetch(request).await })
    }
}

/// `request.signal` 中止时的 `AbortError`（reason 优先）。
///
/// Node 的默认 reason 是 `DOMException` `AbortError`（legacy `code === 20`），因此
/// `formatErrorDetails` 会追加 `code=20`；手工构造的 `{name:"AbortError"}` 普通 Error
/// （`abortReason()` 的回退）没有 code。
fn aborted_error(signal: &AbortScope) -> AuthError {
    let error = match signal.reason() {
        Some(reason) => AuthError::with_name(reason.name, reason.message),
        None => AuthError::with_name(
            AbortReason::fallback_abort_error().name,
            AbortReason::fallback_abort_error().message,
        ),
    };
    // AuthError 不记录 reason 是否真是 DOMException，只能按 Node 默认文案判断：
    // 名字与消息都精确匹配时才补 legacy code（自定义的同文案 reason 会被一并当作 DOMException）。
    if error.name == "AbortError" && error.message == "This operation was aborted" {
        // DOMException.ABORT_ERR
        error.with_error_code("20")
    } else {
        error
    }
}

/// `HttpError` → 可观察错误（名字按 Node 的 `fetch` 错误面）。
///
/// 超时/取消来自 `AbortSignal.timeout` / `AbortSignal` 的 `DOMException`，它们的
/// legacy `code` 分别是 23（`TIMEOUT_ERR`）与 20（`ABORT_ERR`），`formatErrorDetails`
/// 会把它写进 `code=`。
///
/// 这里按消息文本识别超时/取消：`HttpClient` 的错误契约只有 `message` 与 `status`，
/// 没有结构化的错误种类（见 `http/mod.rs` 的 `HttpError`）。
fn http_error(error: crate::http::HttpError) -> AuthError {
    if error.message == crate::http::reqwest_client::TIMEOUT_MESSAGE {
        AuthError::with_name("TimeoutError", error.message).with_error_code("23")
    } else if error.message == crate::http::reqwest_client::ABORTED_MESSAGE {
        AuthError::with_name("AbortError", error.message).with_error_code("20")
    } else {
        AuthError::with_name("TypeError", error.message)
    }
}

/// JS `ToNumber(value)`（JSON 可达值；数组走 `ToPrimitive` 的 `join(",")`）。
///
/// 用于 TS 里没有类型校验、直接做算术的字段（Anthropic / Radius 的 `expires_in`、
/// Radius 设备码的 `interval`）：`"3600" * 1000 === 3600000`、`true * 1000 === 1000`、
/// `null * 1000 === 0`、缺失/对象 → `NaN`。
pub(crate) fn js_to_number(value: &Value) -> f64 {
    match value {
        Value::Null => 0.0,
        Value::Bool(flag) => {
            if *flag {
                1.0
            } else {
                0.0
            }
        }
        Value::Number(number) => number.as_f64().unwrap_or(f64::NAN),
        Value::String(text) => kiana_json::js_num::parse_js_number(text),
        Value::Array(_) => {
            kiana_json::js_num::parse_js_number(&kiana_json::js_string::js_string(Some(value)))
        }
        Value::Object(_) => f64::NAN,
    }
}

/// `undefined` 参与算术：`undefined * 1000 === NaN`。
pub(crate) fn js_to_number_or_nan(value: Option<&Value>) -> f64 {
    value.map(js_to_number).unwrap_or(f64::NAN)
}

/// 默认 `fetch` 的实现：预检查取消 → 取进程级客户端 → 带超时/取消执行 → 读完 body。
///
/// 错误映射：取消/超时经 [`aborted_error`] / [`http_error`] 变成 `AbortError` /
/// `TimeoutError`；等响应头/连接阶段的传输失败是 `TypeError: fetch failed`。
/// body 阶段的传输错误（内容长度不足、chunked 流断开、非法 chunk、解压失败）由
/// `http::reqwest_client::ResponseByteStream` 统一成文本 `terminated`，再经 [`http_error`]
/// 包成 `TypeError: terminated`。客户端构造失败同样是 `TypeError: fetch failed`：
/// `ReqwestHttpClient::new` 已把底层 reqwest 错误归一为该固定文案（这是 Rust 侧独有的
/// 失败点）。
///
/// 与 `fetch` 的一处结构差异：Node 的 `response` 拿到头就返回，body 在调用 `text()`/`json()`
/// 时才读；这里提前读满，因此 [`OAuthFetchResponse`] 可以 `Clone`，`text()`/`json()` 也没有
/// IO 错误分支。
async fn execute_default_fetch(
    request: OAuthFetchRequest,
) -> Result<OAuthFetchResponse, AuthError> {
    // 与 fetch 一致：signal 已中止时不再发请求，直接按 reason 失败。
    if request.signal.is_aborted() {
        return Err(aborted_error(&request.signal));
    }
    // 客户端只构造一次；Err 会被缓存，见 DEFAULT_HTTP_CLIENT 的文档。
    let client = match &*DEFAULT_HTTP_CLIENT {
        Ok(client) => Arc::clone(client),
        Err(message) => return Err(AuthError::with_name("TypeError", message.clone())),
    };
    let http_request = HttpRequest {
        method: request.method,
        url: request.url,
        headers: request.headers,
        body: request.body,
        timeout_ms: request.timeout_ms,
    };
    // HttpClient 的取消契约只有 token（没有 reason）；超时与取消都覆盖“等头 + 读体”。
    let token = request.signal.token();
    let HttpResponse { response, body } = match client.execute(http_request, Some(&token)).await {
        Ok(response) => response,
        Err(error) => {
            // token 只表示已取消；reason 要回 scope 取，才能保留 abort(reason) 的自定义错误。
            if request.signal.is_aborted() {
                return Err(aborted_error(&request.signal));
            }
            return Err(http_error(error));
        }
    };
    // fetch 返回前读满 body：后续 text()/json() 无 IO，超时与取消也覆盖这一阶段。
    let mut bytes = Vec::new();
    let mut stream = body;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(chunk) => bytes.extend_from_slice(&chunk),
            Err(error) => {
                if request.signal.is_aborted() {
                    return Err(aborted_error(&request.signal));
                }
                return Err(http_error(error));
            }
        }
    }
    // ProviderResponse 不带 reason phrase，只能从状态码的标准表反推；未知名 → ""。
    Ok(OAuthFetchResponse {
        status: response.status,
        status_text: reqwest::StatusCode::from_u16(response.status)
            .ok()
            .and_then(|status| status.canonical_reason())
            .unwrap_or("")
            .to_string(),
        headers: response.headers,
        body: bytes,
    })
}

// ---------------------------------------------------------------------------
// JS 语义小工具
// ---------------------------------------------------------------------------

/// `String.prototype.trim()`（ECMAScript 的 WhiteSpace + LineTerminator，含 BOM）。
///
/// TS: 源码里所有 `.trim()` 调用
pub fn js_trim(text: &str) -> &str {
    kiana_json::js_trim(text)
}

/// ECMAScript 正则 `\s`：`[ \t\n\v\f\r   -     　﻿]`。
///
/// 用于 `scope.trim().split(/\s+/)`（[`split_js_whitespace`]）。
fn is_js_regex_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// `text.split(/\s+/).filter(Boolean)`。
///
/// 不用 `str::split_whitespace`：Rust 的空白判定是 Unicode `White_Space`，不含 U+FEFF，
/// 而 ECMAScript 的 `\s` 含 BOM；scope 字符串带 BOM 时两者结果不同。
pub fn split_js_whitespace(text: &str) -> Vec<&str> {
    text.split(is_js_regex_whitespace)
        .filter(|part| !part.is_empty())
        .collect()
}

/// `new URLSearchParams(fields).toString()`（`application/x-www-form-urlencoded` 序列化）。
///
/// 与 `url::form_urlencoded` 的实现一致：`A-Za-z0-9*-._` 不编码、空格 → `+`、其余（含 `~`）
/// 按 UTF-8 字节 `%XX` 大写十六进制。手写以固定语义（spec/ai-auth.md §6.2 #4）。
///
/// 字段按传入顺序拼接、重复键保留；空切片返回空串（测试
/// `form_urlencode_matches_url_search_params`）。这是**请求体**编码；`url_with_search`
/// 也刻意复用它，因为 `Url::set_query` 不会改写本函数产出的字符——`A-Za-z0-9*-._`、`+`、
/// `%XX`、`&`、`=` 都不在 URL query 的百分号编码集里。注意这**不是**说 `set_query` 不编码：
/// 空格、`"`、`#`、`<`、`>`（special scheme 下还有 `'`）都会被它按 query 编码集改写，
/// 所以复用成立的前提是输出字符集受控。这样得到的结果与 TS 里
/// `url.search = new URLSearchParams(...).toString()` 一致。
pub fn form_urlencode(fields: &[(&str, &str)]) -> String {
    fn encode(value: &str, out: &mut String) {
        for byte in value.bytes() {
            match byte {
                b' ' => out.push('+'),
                b'*' | b'-' | b'.' | b'0'..=b'9' | b'A'..=b'Z' | b'_' | b'a'..=b'z' => {
                    out.push(byte as char)
                }
                _ => {
                    const HEX: &[u8; 16] = b"0123456789ABCDEF";
                    out.push('%');
                    out.push(HEX[(byte >> 4) as usize] as char);
                    out.push(HEX[(byte & 0x0f) as usize] as char);
                }
            }
        }
    }
    let mut out = String::new();
    for (index, (key, value)) in fields.iter().enumerate() {
        if index > 0 {
            out.push('&');
        }
        encode(key, &mut out);
        out.push('=');
        encode(value, &mut out);
    }
    out
}

/// `new URLSearchParams(input)` 的解析结果（保留重复键与顺序）。
///
/// `URLSearchParams` 的字符串构造器会先去掉**一个**前导 `?`（实测：
/// `new URLSearchParams("?code=a").get("code") === "a"`、`"??code=a"` 的首键是 `"?code"`）；
/// `url::form_urlencoded` 不去，因此这里显式去掉一个。
pub fn form_urldecode(input: &str) -> Vec<(String, String)> {
    let input = input.strip_prefix('?').unwrap_or(input);
    url::form_urlencoded::parse(input.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

/// `url.searchParams.get(name)`。
///
/// 缺失 → `None`，存在但为空 → `Some("")`，与 `URLSearchParams.get` 返回 `null` 与空串的
/// 区分一致；重复键取第一个（`get` 的语义）。
pub fn query_param(url: &Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

/// `new URL(base)` + `url.search = new URLSearchParams(fields).toString()`。
///
/// 已存在的 query 被整体替换（测试 `url_with_search_replaces_existing_query`）；字段顺序
/// 即输出顺序。base 必须是绝对 URL，否则返回 [`invalid_url_error`]。
///
/// 边界：`fields` 为空时 `Url::set_query(Some(""))` 会留下末尾的 `?`，而 TS 的
/// `url.search = ""` 会删掉整个 query；当前所有调用方都传非空字段，因此没有走到这条差异。
pub(crate) fn url_with_search(base: &str, fields: &[(&str, &str)]) -> Result<String, AuthError> {
    let mut url = Url::parse(base).map_err(invalid_url_error)?;
    let search = form_urlencode(fields);
    url.set_query(Some(&search));
    Ok(url.into())
}

/// `new URL(relative, base)`（`relative` 以 `/` 开头时替换 base 的 path）。
///
/// 用 `Url::join`（WHATWG 相对解析）：`/x` 替换路径，`?y`/`#z` 只替换 query/fragment。
/// base 解析失败同样映射为 `Invalid URL`。
pub(crate) fn resolve_url(base: &str, relative: &str) -> Result<Url, AuthError> {
    let base = Url::parse(base).map_err(invalid_url_error)?;
    base.join(relative).map_err(invalid_url_error)
}

/// `new URL(value)` 失败时的错误文本（Node：`Invalid URL` + cause；这里只保留可观察部分）。
pub(crate) fn invalid_url_error(_error: url::ParseError) -> AuthError {
    AuthError::with_name("TypeError", "Invalid URL")
}

/// `atob(data)`（Node 26 / simdutf 后端的严格标准 base64 规则，spec/ai-auth.md §8.4 #1）。
///
/// 1. 去掉 ASCII 空白（`\t \n \f \r` 与空格；**不含** `\v`）；
/// 2. 只接受 `A-Za-z0-9+/=`；填充只允许在末尾且补齐到 4 的倍数；
/// 3. 数据字符数 `% 4 == 1` → 错误；
/// 4. 无填充的 2/3 余数合法（`"YQ"` → `"a"`）。
///
/// 失败返回 `None`（TS 侧任一异常都被 `decodeJwt` 的 try/catch 吞掉）。
pub fn atob_standard(data: &str) -> Option<Vec<u8>> {
    // 阶段 1：按 Node atob 的白名单过滤。只忽略 \t \n \f \r 与空格；\v、NBSP、base64url
    // 的 `-`/`_` 等一律判非法（宽松接受会把 TS 的登录失败变成成功）。
    let mut chars: Vec<u8> = Vec::with_capacity(data.len());
    for byte in data.bytes() {
        match byte {
            b'\t' | b'\n' | b'\x0c' | b'\r' | b' ' => continue,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' | b'=' => chars.push(byte),
            _ => return None,
        }
    }
    // 阶段 2：统计末尾填充；填充只能出现在末尾，且最多 2 个（`YQ===` 非法）。
    let padding = chars.iter().rev().take_while(|byte| **byte == b'=').count();
    if padding > 2 {
        return None;
    }
    let data_len = chars.len() - padding;
    if chars[..data_len].contains(&b'=') {
        return None;
    }
    // 有填充但总长不是 4 的倍数（`"YQ="`）→ Invalid character；
    // 无填充的 2/3 余数是合法输入，只有余数 1 才报 not correctly encoded。
    if !chars.len().is_multiple_of(4) && padding > 0 {
        return None;
    }
    // 阶段 3：填充数与数据字符数必须自洽（p=1 要求 n%4==3，p=2 要求 n%4==2）。
    if padding == 1 && data_len % 4 != 3 {
        return None;
    }
    if padding == 2 && data_len % 4 != 2 {
        return None;
    }
    // 阶段 4：数据字符数 % 4 == 1 无法解码（无论是否带填充）。
    if data_len % 4 == 1 {
        return None;
    }
    // 阶段 5：按标准字母表解码；无填充的 2/3 余数在这里按补零处理。
    decode_base64_standard(&chars[..data_len])
}

/// 标准 base64 解码（无填充输入按补零处理）。
///
/// 调用方 [`atob_standard`] 已完成字符与填充校验，这里只做查表与移位；每个 `chunks(4)`
/// 中不足 4 个字符的尾块缺少低位，天然按补零解码。
fn decode_base64_standard(data: &[u8]) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u32> {
        match byte {
            b'A'..=b'Z' => Some((byte - b'A') as u32),
            b'a'..=b'z' => Some((byte - b'a') as u32 + 26),
            b'0'..=b'9' => Some((byte - b'0') as u32 + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    for chunk in data.chunks(4) {
        let mut buffer = 0u32;
        for (index, byte) in chunk.iter().enumerate() {
            buffer |= value(*byte)? << (18 - index * 6);
        }
        out.push((buffer >> 16) as u8);
        if chunk.len() > 2 {
            out.push((buffer >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(buffer as u8);
        }
    }
    Some(out)
}

/// JS 二进制字符串（`atob` 的返回值）→ Rust `String`：每个字节映射为一个 U+00XX 字符。
pub(crate) fn latin1_string(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| *byte as char).collect()
}

/// `parseAuthorizationInput` 的结果（anthropic 与 openai-codex 共用同一解析形状，改动需两边同步）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ParsedAuthorizationInput {
    /// `code`（缺失 → `None`；空串保留为 `Some("")`）。
    pub code: Option<String>,
    /// `state`（同上）。
    pub state: Option<String>,
}

/// anthropic / openai-codex 的 `parseAuthorizationInput`。
///
/// 顺序：绝对 URL → `#`（`split("#", 2)`，第三个及以后的 `#` 被丢弃）→ `code=` 的
/// `URLSearchParams` → 整串当 code。
///
/// 输入先经 [`js_trim`]，空串返回全 `None`。`code=` 分支用 [`form_urldecode`]，因此 `+`
/// 解码为空格、重复键取第一个。绝对 URL 分支用 `Url::parse`，与 `new URL(value)` 一样
/// 要求带 scheme。
pub(crate) fn parse_authorization_input(input: &str) -> ParsedAuthorizationInput {
    let value = js_trim(input);
    if value.is_empty() {
        return ParsedAuthorizationInput::default();
    }
    if let Ok(url) = Url::parse(value) {
        return ParsedAuthorizationInput {
            code: query_param(&url, "code"),
            state: query_param(&url, "state"),
        };
    }
    if let Some((code, rest)) = value.split_once('#') {
        // JS `value.split("#", 2)` 只保留前两段：后续 `#` 之后的内容被丢弃。
        let state = rest.split('#').next().unwrap_or("");
        return ParsedAuthorizationInput {
            code: Some(code.to_string()),
            state: Some(state.to_string()),
        };
    }
    if value.contains("code=") {
        let params = form_urldecode(value);
        let get = |name: &str| {
            params
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };
        return ParsedAuthorizationInput {
            code: get("code"),
            state: get("state"),
        };
    }
    ParsedAuthorizationInput {
        code: Some(value.to_string()),
        state: None,
    }
}

/// `getProviderEnvValue("KIANA_OAUTH_CALLBACK_HOST") || "127.0.0.1"`（每次调用读取）。
///
/// 对应 TS 的函数级 `getCallbackHost()`（openai-codex / openrouter）；空串按缺失处理，
/// 回落到 `127.0.0.1`（[`crate::utils::provider_env::get_provider_env_value`] 的 `||` 语义）。
pub(crate) fn oauth_callback_host() -> String {
    crate::utils::provider_env::get_provider_env_value("KIANA_OAUTH_CALLBACK_HOST", None)
        .unwrap_or_else(|| "127.0.0.1".to_string())
}

/// 模块初始化时读取的 `KIANA_OAUTH_CALLBACK_HOST`（anthropic / openai-chatgpt）。
///
/// 对应 TS 的模块级 `const CALLBACK_HOST = ...`：`LazyLock` 在进程内第一次调用时求值，
/// 之后修改环境变量不再生效。测试若要覆盖，必须在同一进程第一次使用之前设置。
pub(crate) fn oauth_callback_host_at_init() -> &'static str {
    static HOST: LazyLock<String> = LazyLock::new(oauth_callback_host);
    HOST.as_str()
}

/// `Math.max(0, value)` 的时长转换（毫秒，非有限值按 0 处理）。
pub(crate) fn delay_duration_ms(delay_ms: f64) -> u64 {
    if !delay_ms.is_finite() || delay_ms <= 0.0 {
        0
    } else {
        delay_ms as u64
    }
}

/// 构造一个 `serde_json::Value::Object`（键序即插入序）。
///
/// 键序靠工作区 `serde_json` 的 `preserve_order` feature（根 `Cargo.toml`）；没有它
/// `serde_json::Map` 会退化成按字典序排序的 `BTreeMap`，`JSON.stringify` 的字节级输出就
/// 不再一致。
pub(crate) fn json_object(pairs: Vec<(&str, Value)>) -> Value {
    let mut map = serde_json::Map::with_capacity(pairs.len());
    for (key, value) in pairs {
        map.insert(key.to_string(), value);
    }
    Value::Object(map)
}

/// `JSON.stringify(value)`（错误消息里用）。
pub(crate) fn json_stringify(value: &Value) -> String {
    kiana_json::js_object::stringify(value)
}

/// `JSON.stringify(value)`，其中 `None` 对应 TS `readJson` 的失败结果 `null`
/// （错误消息里显示 `"null"`）。
pub(crate) fn json_stringify_nullable(value: Option<&Value>) -> String {
    value
        .map(json_stringify)
        .unwrap_or_else(|| "null".to_string())
}

/// `value` 中名为 `name` 的属性（非对象 / 数组 → `None`，与 JS 属性访问一致）。
pub(crate) fn object_field<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => map.get(name),
        _ => None,
    }
}

/// `typeof value === "object" && value !== null`（数组也算对象，与 JS 一致）。
pub(crate) fn is_object_like(value: &Value) -> bool {
    matches!(value, Value::Object(_) | Value::Array(_))
}

/// `typeof value === "number" && Number.isFinite(value)` 的数值视图。
pub(crate) fn finite_number(value: Option<&Value>) -> Option<f64> {
    let number = value?.as_f64()?;
    if number.is_finite() {
        Some(number)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// 测试工具（TS 测试里的 `vi.stubGlobal("fetch")` 与 `vi.setSystemTime`）
// ---------------------------------------------------------------------------

/// OAuth 流程的测试工具。
///
/// 没有 `#[cfg(test)]`：这些类型要给 `kiana-ai` 的集成测试（`tests/*.rs`）使用，而集成测试
/// 是独立 crate，看不到本 crate 的 `cfg(test)` 项。其他 crate 的测试若只想注入传输，
/// 用 [`crate::auth::oauth::OAuthFetchFn`] 配 `set_default_oauth_fetch_for_tests` 即可，
/// 不需要本模块。
pub mod testing {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::*;
    use crate::auth::oauth::device_code::AuthClock;
    use crate::auth::types::{AuthEvent, AuthInteraction, AuthPromptRequest};

    /// 记录请求并按闭包返回响应的 [`OAuthFetch`]。
    pub struct MockFetch {
        handler: Box<dyn Fn(OAuthFetchRequest) -> OAuthFetchFuture + Send + Sync>,
        requests: Mutex<Vec<OAuthFetchRequest>>,
    }

    impl MockFetch {
        /// 用给定 handler 构造。
        pub fn new(
            handler: impl Fn(OAuthFetchRequest) -> OAuthFetchFuture + Send + Sync + 'static,
        ) -> Arc<MockFetch> {
            Arc::new(MockFetch {
                handler: Box::new(handler),
                requests: Mutex::new(Vec::new()),
            })
        }

        /// 目前收到的请求（按到达顺序）。
        pub fn requests(&self) -> Vec<OAuthFetchRequest> {
            self.requests
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
        }

        /// 收到的请求数。
        pub fn request_count(&self) -> usize {
            self.requests
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .len()
        }
    }

    impl OAuthFetch for MockFetch {
        fn fetch(&self, request: OAuthFetchRequest) -> OAuthFetchFuture {
            self.requests
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(request.clone());
            (self.handler)(request)
        }
    }

    /// 直接返回给定响应的 handler。
    pub fn respond(
        response: OAuthFetchResponse,
    ) -> impl Fn(OAuthFetchRequest) -> OAuthFetchFuture + Send + Sync + 'static {
        move |_request| {
            let response = response.clone();
            Box::pin(async move { Ok(response) })
        }
    }

    /// `Date.now()` 的虚拟时钟：`base_ms` + tokio 虚拟时间（`#[tokio::test(start_paused = true)]`
    /// 下与 `tokio::time::advance` 同步推进）。
    pub struct VirtualClock {
        base_ms: f64,
        epoch: tokio::time::Instant,
    }

    impl VirtualClock {
        /// 以 `base_ms` 为起点（通常是一个固定的 `Date` 毫秒值）。
        pub fn new(base_ms: f64) -> Arc<VirtualClock> {
            Arc::new(VirtualClock {
                base_ms,
                epoch: tokio::time::Instant::now(),
            })
        }

        /// 当前虚拟毫秒。
        pub fn now_ms(&self) -> f64 {
            self.base_ms + self.epoch.elapsed().as_secs_f64() * 1000.0
        }
    }

    impl AuthClock for VirtualClock {
        fn now_ms(&self) -> f64 {
            VirtualClock::now_ms(self)
        }
    }

    /// 让已 spawn 的任务运行到下一个等待点（不触发 tokio 的自动推进）。
    ///
    /// 固定让出 8 次调度，不能当作同步原语；测试通常接着轮询事件（如 `wait_for_auth_url`）
    /// 而不是假设任务已经完成。
    pub async fn yield_tasks() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    /// 测试用 `AuthInteraction`：`prompt` 交给可替换的 handler，`notify` 记录事件。
    ///
    /// 对应 TS 测试里内联的 `{ signal, prompt, notify }` 对象；handler 可在构造后设置，
    /// 便于闭包读取本对象记录的事件（TS 里的 `authUrl` 变量）。
    ///
    /// 未安装 handler 时 `prompt` 返回 `Error("no prompt handler installed")`——这是 Rust
    /// 侧新增的失败分支：TS 的内联对象始终带 `prompt` 函数，而这里允许先构造、再注入
    /// handler（见 [`set_handler`](Self::set_handler)），中间存在未安装的窗口。
    pub struct MockInteraction {
        signal: AbortScope,
        handler: Mutex<Option<Arc<PromptHandler>>>,
        events: Arc<Mutex<Vec<AuthEvent>>>,
    }

    /// `prompt` 的返回值 future。
    pub type PromptFuture =
        Pin<Box<dyn std::future::Future<Output = Result<String, AuthError>> + Send + 'static>>;

    /// `prompt` handler。
    pub type PromptHandler = dyn Fn(AuthPromptRequest) -> PromptFuture + Send + Sync;

    impl MockInteraction {
        /// 只有 signal 的交互对象；在调用 `prompt` 前必须 [`set_handler`](Self::set_handler)。
        pub fn new(signal: AbortScope) -> Arc<MockInteraction> {
            Arc::new(MockInteraction {
                signal,
                handler: Mutex::new(None),
                events: Arc::new(Mutex::new(Vec::new())),
            })
        }

        /// 设置 `prompt` handler（在流程开始前调用）。
        pub fn set_handler(
            &self,
            handler: impl Fn(AuthPromptRequest) -> PromptFuture + Send + Sync + 'static,
        ) {
            *self
                .handler
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(Arc::new(handler));
        }

        /// 事件列表的共享句柄（handler 里读取 `auth_url` 用）。
        pub fn events_handle(&self) -> Arc<Mutex<Vec<AuthEvent>>> {
            Arc::clone(&self.events)
        }

        /// 已记录的事件。
        pub fn events(&self) -> Vec<AuthEvent> {
            self.events
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
        }

        /// 第一个 `auth_url` 事件的 URL。
        pub fn auth_url(&self) -> Option<String> {
            self.events().into_iter().find_map(|event| match event {
                AuthEvent::AuthUrl { url, .. } => Some(url),
                _ => None,
            })
        }
    }

    #[async_trait]
    impl AuthInteraction for MockInteraction {
        fn signal(&self) -> &AbortScope {
            &self.signal
        }

        async fn prompt(&self, request: AuthPromptRequest) -> Result<String, AuthError> {
            let handler = self
                .handler
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            let Some(handler) = handler else {
                return Err(AuthError::new("no prompt handler installed"));
            };
            handler(request).await
        }

        fn notify(&self, event: &AuthEvent) {
            self.events
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(event.clone());
        }
    }

    /// 永不 resolve 的 prompt（TS 测试里的 `new Promise(() => {})`）。
    pub fn pending_prompt() -> PromptFuture {
        Box::pin(std::future::pending::<Result<String, AuthError>>())
    }

    /// 固定返回同一个字符串的 prompt。
    pub fn responding_prompt(value: impl Into<String>) -> PromptFuture {
        let value = value.into();
        Box::pin(async move { Ok(value) })
    }

    /// 以 `AuthError` 失败的 prompt。
    pub fn failing_prompt(message: impl Into<String>) -> PromptFuture {
        let message = message.into();
        Box::pin(async move { Err(AuthError::new(message)) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_urlencode_matches_url_search_params() {
        assert_eq!(form_urlencode(&[]), "");
        assert_eq!(form_urlencode(&[("a", "b c")]), "a=b+c");
        assert_eq!(
            form_urlencode(&[("scope", "read:user")]),
            "scope=read%3Auser"
        );
        assert_eq!(
            form_urlencode(&[("redirect_uri", "http://localhost:1455/auth/callback")]),
            "redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"
        );
        assert_eq!(form_urlencode(&[("a", "~*'()!")]), "a=%7E*%27%28%29%21");
        assert_eq!(form_urlencode(&[("a", "x"), ("b", "y")]), "a=x&b=y");
    }

    #[test]
    fn form_urldecode_matches_url_search_params() {
        let parsed = form_urldecode("code=manual-code&state=s%3A1&plus=a+b");
        assert_eq!(
            parsed,
            vec![
                ("code".to_string(), "manual-code".to_string()),
                ("state".to_string(), "s:1".to_string()),
                ("plus".to_string(), "a b".to_string()),
            ]
        );
    }

    #[test]
    fn form_urldecode_strips_one_leading_question_mark() {
        // `new URLSearchParams("?code=abc&state=x")` → code=abc（Node 26 实测）。
        let parsed = form_urldecode("?code=abc&state=x");
        assert_eq!(
            parsed,
            vec![
                ("code".to_string(), "abc".to_string()),
                ("state".to_string(), "x".to_string()),
            ]
        );
        // 只去掉一个：`new URLSearchParams("??code=a")` 的首键是 `"?code"`。
        assert_eq!(
            form_urldecode("??code=a"),
            vec![("?code".to_string(), "a".to_string())]
        );
    }

    #[test]
    fn atob_matches_node_samples() {
        // spec/ai-auth.md §8.4 #1 的实测样本。
        assert_eq!(atob_standard("YQ").as_deref(), Some(&b"a"[..]));
        assert_eq!(atob_standard("YWI").as_deref(), Some(&b"ab"[..]));
        assert_eq!(atob_standard("YQ==").as_deref(), Some(&b"a"[..]));
        assert_eq!(atob_standard("YQ==="), None);
        assert_eq!(atob_standard("YQ="), None);
        assert_eq!(atob_standard("Y"), None);
        assert_eq!(atob_standard("YWJjZA="), None);
        assert_eq!(atob_standard("YW Jj").as_deref(), Some(&b"abc"[..]));
        assert_eq!(atob_standard("AAA=").map(|bytes| bytes.len()), Some(2));
        assert_eq!(atob_standard("AAAAA"), None);
        assert_eq!(atob_standard("AAAAA="), None);
        assert_eq!(atob_standard("AAAA=="), None);
        assert_eq!(atob_standard("YQ\u{00a0}=="), None);
        // base64url 字符被 Node 的 atob 拒绝。
        assert_eq!(atob_standard("ab-_"), None);
        // `\v` 不是 atob 忽略的空白。
        assert_eq!(atob_standard("YQ\x0b=="), None);
    }

    #[test]
    fn parse_authorization_input_matches_js_split_limit() {
        let parsed = parse_authorization_input("code#state#extra");
        assert_eq!(parsed.code.as_deref(), Some("code"));
        assert_eq!(parsed.state.as_deref(), Some("state"));

        let parsed = parse_authorization_input("  the-code  ");
        assert_eq!(parsed.code.as_deref(), Some("the-code"));
        assert_eq!(parsed.state, None);

        let parsed = parse_authorization_input("a=1&code=x&state=y");
        assert_eq!(parsed.code.as_deref(), Some("x"));
        assert_eq!(parsed.state.as_deref(), Some("y"));

        let parsed =
            parse_authorization_input("http://localhost:1455/auth/callback?code=c&state=s");
        assert_eq!(parsed.code.as_deref(), Some("c"));
        assert_eq!(parsed.state.as_deref(), Some("s"));

        assert_eq!(
            parse_authorization_input(""),
            ParsedAuthorizationInput::default()
        );
    }

    #[test]
    fn url_with_search_replaces_existing_query() {
        assert_eq!(
            url_with_search("https://openrouter.ai/auth", &[("code_challenge", "abc")]).unwrap(),
            "https://openrouter.ai/auth?code_challenge=abc"
        );
        assert_eq!(
            url_with_search("https://x.example/authorize?old=1", &[("state", "s")]).unwrap(),
            "https://x.example/authorize?state=s"
        );
    }

    #[test]
    fn split_js_whitespace_drops_empty_parts() {
        assert_eq!(
            split_js_whitespace("openid  profile\temail"),
            vec!["openid", "profile", "email"]
        );
        assert!(split_js_whitespace("   ").is_empty());
    }

    /// 回归（评审发现）：Anthropic / Radius 不校验 `expires_in` 的类型，直接做
    /// `expires_in * 1000`，因此必须用 JS `ToNumber` 语义（Node 26 实测值）。
    #[test]
    fn js_to_number_matches_javascript_arithmetic_coercion() {
        use serde_json::json;

        assert_eq!(js_to_number(&json!(3600)), 3600.0);
        assert_eq!(js_to_number(&json!("3600")), 3600.0);
        assert_eq!(js_to_number(&json!(true)), 1.0);
        assert_eq!(js_to_number(&json!(false)), 0.0);
        assert_eq!(js_to_number(&json!(null)), 0.0);
        assert_eq!(js_to_number(&json!("0x10")), 16.0);
        assert_eq!(js_to_number(&json!("")), 0.0);
        assert_eq!(js_to_number(&json!(" 12 ")), 12.0);
        assert_eq!(js_to_number(&json!(["7"])), 7.0);
        assert_eq!(js_to_number(&json!("Infinity")), f64::INFINITY);
        assert!(js_to_number(&json!("abc")).is_nan());
        assert!(js_to_number(&json!({})).is_nan());
        assert!(js_to_number(&json!([1, 2])).is_nan());
        assert!(js_to_number_or_nan(None).is_nan());
        assert_eq!(js_to_number_or_nan(Some(&json!("2"))), 2.0);
    }

    /// 回归（评审发现）：Node 的 `AbortSignal.timeout` / 默认 abort 抛出 `DOMException`，
    /// 其 legacy `code` 分别是 23 / 20，`formatErrorDetails` 会写进 `code=`。
    #[test]
    fn dom_exception_codes_match_node_legacy_values() {
        let timeout = http_error(crate::http::HttpError::new(
            crate::http::reqwest_client::TIMEOUT_MESSAGE,
        ));
        assert_eq!(timeout.name, "TimeoutError");
        assert_eq!(timeout.error_code.as_deref(), Some("23"));

        let aborted = http_error(crate::http::HttpError::new(
            crate::http::reqwest_client::ABORTED_MESSAGE,
        ));
        assert_eq!(aborted.name, "AbortError");
        assert_eq!(aborted.error_code.as_deref(), Some("20"));

        let failed = http_error(crate::http::HttpError::new("fetch failed"));
        assert_eq!(failed.error_code, None);

        // 默认 DOMException reason（"This operation was aborted"）→ code=20；
        // `abortReason()` 的手工回退（"The operation was aborted"）是普通 Error，无 code。
        let scope = AbortScope::new();
        scope.abort();
        let aborted = aborted_error(&scope);
        assert_eq!(aborted.name, "AbortError");
        assert_eq!(aborted.error_code.as_deref(), Some("20"));

        let manual = AbortScope::new();
        manual.abort_with(AbortReason::fallback_abort_error());
        assert_eq!(aborted_error(&manual).error_code, None);
    }
}
