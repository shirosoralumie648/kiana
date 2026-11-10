//! `WebTransport`：一次 HTTP 请求的最小契约。
//!
//! 为什么要有这一层：搜索与抓取的策略（引擎链、SSRF、截断）必须可测试，而真实网络
//! 既慢又不可控。生产实现 [`ReqwestWebTransport`] 使用专用的受保护 client；测试注入
//! 脚本化替身（见 `search` / `fetch` 的单元测试），因此默认测试不访问网络。
//!
//! 响应体是 `Vec<u8>` 而不是流：WebFetch 的语义是“取回整页文本再截断”。硬上限由
//! [`WebHttpRequest::max_response_bytes`] 控制，生产路径默认
//! [`crate::web::fetch::MAX_FETCH_BYTES`]；读取时逐块累计，超过上限立即失败，不会先把
//! 整个 body 读进内存。
//!
//! 安全约束（与 `resolve_public_host` 的预检配合）：
//! - client 使用 [`GuardedWebDnsResolver`]，在**连接时**重新解析并拒绝私网/元数据地址，
//!   关闭“预检拿到公网 IP、连接时 rebinding 到内网”的窗口；
//! - client 关闭环境变量代理（[`reqwest::ClientBuilder::no_proxy`]），否则代理会在代理端
//!   解析目标域名，绕过本进程的 DNS 校验。

use std::net::SocketAddr;
use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;
use url::Url;

use super::ssrf::{is_blocked_ip, is_metadata_ip, resolve_public_host, validate_url};

/// 一次 Web HTTP 请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebHttpRequest {
    /// HTTP 方法（只支持 GET/POST/HEAD）。
    pub method: String,
    /// 绝对 URL（已通过 SSRF 校验的形态）。
    pub url: String,
    /// 附加请求头，按顺序发送。
    pub headers: Vec<(String, String)>,
    /// 请求体；`None` 表示没有 body。
    pub body: Option<Vec<u8>>,
    /// 整条请求的超时（毫秒）。
    pub timeout_ms: Option<u64>,
    /// 响应体硬上限；超出时返回 [`WebTransportError::TooLarge`]。
    pub max_response_bytes: usize,
}

impl Default for WebHttpRequest {
    fn default() -> Self {
        Self {
            method: "GET".to_string(),
            url: String::new(),
            headers: Vec::new(),
            body: None,
            timeout_ms: Some(30_000),
            max_response_bytes: 2 * 1024 * 1024,
        }
    }
}

/// 响应快照；body 已经完整读取（受上限约束）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebHttpResponse {
    /// HTTP 状态码。
    pub status: u16,
    /// 响应头（名字小写化由传输实现决定，查找时按大小写不敏感处理）。
    pub headers: Vec<(String, String)>,
    /// 响应体。
    pub body: Vec<u8>,
    /// 跟随重定向后的最终 URL。
    pub final_url: String,
}

impl WebHttpResponse {
    /// 按大小写不敏感查响应头。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// `content-type` 的小写主类型（去掉参数），如 `text/html`。
    pub fn content_type(&self) -> Option<String> {
        self.header("content-type").map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
    }
}

/// 传输错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WebTransportError {
    /// 请求构造/连接/读取失败；文本是可读原因。
    #[error("request failed: {0}")]
    Request(String),
    /// 请求超时。
    #[error("request timed out after {timeout_ms} ms")]
    Timeout {
        /// 超时预算。
        timeout_ms: u64,
    },
    /// 响应体超过上限。
    #[error("response body exceeds {limit} bytes")]
    TooLarge {
        /// 字节上限。
        limit: usize,
    },
    /// 不支持的方法。
    #[error("unsupported HTTP method {0:?}")]
    UnsupportedMethod(String),
}

/// HTTP 传输契约。实现必须 `Send + Sync`，因为引擎链与并行 fetch 会共享句柄。
// `async_trait` 展开的 Future 方法自带 `#[must_use]`，clippy 看不到宏内属性。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait WebTransport: Send + Sync {
    /// 执行一次请求；`Ok` 只表示响应头与 body 都成功取回（非 2xx 也是 `Ok`）。
    async fn execute(&self, request: WebHttpRequest) -> Result<WebHttpResponse, WebTransportError>;
}

/// 生产实现：逐跳跟随重定向，并在每次连接时重新做 DNS 校验。
///
/// 每次收到 3xx 都先用 [`validate_url`] + [`resolve_public_host`] 校验下一跳，再进行
/// 下一跳请求；被禁地址（metadata/loopback/私网）在**发送前**就被拒绝，请求字节不会
/// 发出。相对 `Location` 按当前 URL 解析，最多 [`MAX_REDIRECTS`] 跳。
///
/// 实际发送使用进程级共享的**受保护 client**：[`guarded_web_client`]。它的 DNS resolver
/// 在 connect 时再次校验解析结果（防 rebinding），并显式禁用环境变量代理（防代理端解析
/// 绕过校验）。
#[derive(Debug, Clone)]
pub struct ReqwestWebTransport {
    /// 发送的 `user-agent`。
    pub user_agent: String,
}

impl Default for ReqwestWebTransport {
    fn default() -> Self {
        Self {
            user_agent: "kiana/0.1 (+https://github.com/earendil-works/kiana)".to_string(),
        }
    }
}

impl ReqwestWebTransport {
    /// 用默认 user-agent 构造。
    pub fn new() -> Self {
        Self::default()
    }
}

/// 手动重定向的最大跳数（与 `http_dispatcher::MAX_REDIRECTS` 一致）。
pub const MAX_REDIRECTS: usize = 20;

/// 逐跳跟随重定向，每一跳在发送前做 SSRF 校验。
///
/// `send` 是实际发送一跳的闭包；测试注入记录型替身即可断言“被禁的下一跳从未发出”。
pub async fn follow_redirects<F, Fut>(
    mut request: WebHttpRequest,
    mut send: F,
) -> Result<WebHttpResponse, WebTransportError>
where
    F: FnMut(WebHttpRequest) -> Fut,
    Fut: std::future::Future<Output = Result<WebHttpResponse, WebTransportError>>,
{
    let mut hops = 0usize;
    loop {
        let mut response = send(request.clone()).await?;
        if !matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            response.final_url = request.url;
            return Ok(response);
        }
        let Some(location) = response.header("location").map(str::to_string) else {
            return Err(WebTransportError::Request(format!(
                "HTTP {} redirect without a Location header",
                response.status
            )));
        };
        hops += 1;
        if hops > MAX_REDIRECTS {
            return Err(WebTransportError::Request(format!(
                "too many redirects (limit {MAX_REDIRECTS})"
            )));
        }
        let current = Url::parse(&request.url).map_err(|error| {
            WebTransportError::Request(format!("invalid request URL {:?}: {error}", request.url))
        })?;
        let next = current.join(&location).map_err(|error| {
            WebTransportError::Request(format!("invalid redirect target {location:?}: {error}"))
        })?;
        let next_url = next.to_string();
        let parsed = validate_url(&next_url).map_err(|error| {
            WebTransportError::Request(format!("redirect to blocked URL {next_url:?}: {error}"))
        })?;
        resolve_public_host(&parsed, false).await.map_err(|error| {
            WebTransportError::Request(format!("redirect to blocked URL {next_url:?}: {error}"))
        })?;
        // 浏览器语义：303 一律改 GET；301/302 上的 POST 也改 GET；307/308 保留方法与 body。
        let mut method = request.method.clone();
        let mut body = request.body.clone();
        if response.status == 303
            || ((response.status == 301 || response.status == 302)
                && method.eq_ignore_ascii_case("POST"))
        {
            method = "GET".to_string();
            body = None;
        }
        // 跨源（含 https → http 降级）不转发任何调用方 header，也不重放 body：
        // 搜索/抓取 API 的凭据（X-Subscription-Token、X-Goog-Api-Key 等）不能跟着
        // Location 泄露给第三方。user-agent 由传输层按自身配置重新添加。
        if next.origin() != current.origin() {
            request.headers.clear();
            method = "GET".to_string();
            body = None;
        }
        request.url = next_url;
        request.method = method;
        request.body = body;
    }
}

#[async_trait]
impl WebTransport for ReqwestWebTransport {
    async fn execute(&self, request: WebHttpRequest) -> Result<WebHttpResponse, WebTransportError> {
        let client = guarded_web_client()?;
        let user_agent = self.user_agent.clone();
        let overall_timeout_ms = request.timeout_ms;
        let fetch = follow_redirects(request, move |hop| {
            let client = client.clone();
            let user_agent = user_agent.clone();
            async move {
                let method = hop.method.to_ascii_uppercase();
                let timeout_ms = hop.timeout_ms;
                let mut builder = match method.as_str() {
                    "GET" => client.get(hop.url.as_str()),
                    "POST" => client.post(hop.url.as_str()),
                    "HEAD" => client.head(hop.url.as_str()),
                    other => {
                        return Err(WebTransportError::UnsupportedMethod(other.to_string()));
                    }
                };
                builder = builder.header("user-agent", user_agent.as_str());
                for (name, value) in &hop.headers {
                    builder = builder.header(name.as_str(), value.as_str());
                }
                if let Some(timeout) = timeout_ms {
                    builder = builder.timeout(Duration::from_millis(timeout));
                }
                if let Some(body) = &hop.body {
                    builder = builder.body(body.clone());
                }
                let mut response = builder
                    .send()
                    .await
                    .map_err(|error| map_error(error, timeout_ms))?;
                let status = response.status().as_u16();
                let headers: Vec<(String, String)> = response
                    .headers()
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.as_str().to_string(),
                            value.to_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect();
                // 逐块读取并在累计超过上限时立即失败：`bytes().await` 会先把整个 body
                // 读进内存，2 MiB 的语义上限挡不住多 GB 的流式响应。
                let mut body: Vec<u8> = Vec::new();
                while let Some(chunk) = response
                    .chunk()
                    .await
                    .map_err(|error| map_error(error, timeout_ms))?
                {
                    if body.len().saturating_add(chunk.len()) > hop.max_response_bytes {
                        return Err(WebTransportError::TooLarge {
                            limit: hop.max_response_bytes,
                        });
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(WebHttpResponse {
                    status,
                    headers,
                    body,
                    final_url: hop.url,
                })
            }
        });
        // `timeout_ms` 是整条请求（含全部重定向跳数）的预算；每一跳的 reqwest
        // `timeout` 只防止单跳挂死，不能把总时长放大到 N 倍。
        match overall_timeout_ms {
            Some(limit) => match tokio::time::timeout(Duration::from_millis(limit), fetch).await {
                Ok(result) => result,
                Err(_) => Err(WebTransportError::Timeout { timeout_ms: limit }),
            },
            None => fetch.await,
        }
    }
}

/// reqwest 错误只能通过 `Display` 使用（本 crate 没有 reqwest 依赖，不能命名
/// `reqwest::Error`，也不能调用 `is_timeout`）。所有传输错误在引擎链里都按可重试处理，
/// 因此这里不需要细分超时。
fn map_error(error: impl std::fmt::Display, timeout_ms: Option<u64>) -> WebTransportError {
    let text = error.to_string();
    if timeout_ms.is_some() && text.to_ascii_lowercase().contains("timed out") {
        return WebTransportError::Timeout {
            timeout_ms: timeout_ms.unwrap_or(0),
        };
    }
    WebTransportError::Request(text)
}

/// 进程级受保护 client：手动重定向、禁用代理、连接时校验 DNS。
///
/// 构造失败（TLS 后端初始化等）会被缓存成错误文本，后续调用直接返回
/// [`WebTransportError::Request`]，不会退回不设防的 `Client::new()`。
fn guarded_web_client() -> Result<&'static reqwest::Client, WebTransportError> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                // 代理会在代理端解析域名，绕过 Guarded resolver 的检查。
                .no_proxy()
                .dns_resolver(GuardedWebDnsResolver)
                .connect_timeout(Duration::from_secs(10))
                .build()
                .map_err(|error| format!("failed to build the WebFetch HTTP client: {error}"))
        })
        .as_ref()
        .map_err(|error| WebTransportError::Request(error.clone()))
}

/// 在 connect 时校验 DNS 结果的 resolver。
///
/// `resolve_public_host` 的预检与真正连接之间存在时间差，攻击者可在窗口内把域名改指向
/// 内网（DNS rebinding）。这里在每次连接解析后、返回地址前再做一次同样的拒绝，使预检
/// 只承担“快速失败 + 更清晰错误”的职责。
#[derive(Debug, Clone, Copy)]
struct GuardedWebDnsResolver;

impl reqwest::dns::Resolve for GuardedWebDnsResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?
                .collect();
            if addrs.is_empty() {
                return Err(format!("DNS returned no addresses for {host:?}").into());
            }
            if let Some(blocked) = addrs
                .iter()
                .find(|addr| is_metadata_ip(addr.ip()) || is_blocked_ip(addr.ip(), false))
            {
                return Err(format!(
                    "WebFetch host {host:?} resolved to blocked address {}",
                    blocked.ip()
                )
                .into());
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn guarded_client_builds() {
        // 编译期/构造期回归：resolver 与 no_proxy 的装配必须能成功构建 client，
        // 否则所有 WebFetch 都会退化成 “failed to build the WebFetch HTTP client”。
        assert!(guarded_web_client().is_ok());
    }

    fn request(url: &str) -> WebHttpRequest {
        WebHttpRequest {
            url: url.to_string(),
            ..Default::default()
        }
    }

    fn redirect(status: u16, location: &str) -> WebHttpResponse {
        WebHttpResponse {
            status,
            headers: vec![("location".to_string(), location.to_string())],
            body: Vec::new(),
            final_url: String::new(),
        }
    }

    #[tokio::test]
    async fn redirect_to_metadata_is_rejected_before_the_next_hop_is_sent() {
        let sent: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&sent);
        let error = follow_redirects(
            request("https://93.184.216.34/redirect"),
            move |hop: WebHttpRequest| {
                let recorded = Arc::clone(&recorded);
                async move {
                    recorded.lock().expect("sent").push(hop.url.clone());
                    if hop.url.contains("redirect") {
                        Ok(redirect(302, "http://169.254.169.254/latest/meta-data"))
                    } else {
                        panic!("blocked redirect hop must never be sent: {}", hop.url);
                    }
                }
            },
        )
        .await
        .expect_err("metadata redirect is rejected");
        assert!(error.to_string().contains("blocked URL"), "{error}");
        assert_eq!(
            sent.lock().expect("sent").as_slice(),
            ["https://93.184.216.34/redirect"],
            "the transport must not issue the second-hop request"
        );
    }

    #[tokio::test]
    async fn relative_redirects_are_resolved_and_followed() {
        let sent: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&sent);
        let response = follow_redirects(
            request("https://93.184.216.34/start"),
            move |hop: WebHttpRequest| {
                let recorded = Arc::clone(&recorded);
                async move {
                    recorded.lock().expect("sent").push(hop.url.clone());
                    if hop.url.ends_with("/start") {
                        Ok(redirect(302, "/final"))
                    } else {
                        Ok(WebHttpResponse {
                            status: 200,
                            headers: vec![("content-type".to_string(), "text/plain".to_string())],
                            body: b"ok".to_vec(),
                            final_url: hop.url,
                        })
                    }
                }
            },
        )
        .await
        .expect("follow");
        assert_eq!(response.status, 200);
        assert_eq!(response.final_url, "https://93.184.216.34/final");
        assert_eq!(
            sent.lock().expect("sent").as_slice(),
            [
                "https://93.184.216.34/start".to_string(),
                "https://93.184.216.34/final".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn cross_origin_redirect_strips_headers_and_body() {
        let seen: Arc<Mutex<Vec<WebHttpRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&seen);
        let mut initial = request("https://93.184.216.34/start");
        initial.method = "POST".to_string();
        initial.headers = vec![("authorization".to_string(), "Bearer secret".to_string())];
        initial.body = Some(b"payload".to_vec());
        follow_redirects(initial, move |hop: WebHttpRequest| {
            let recorded = Arc::clone(&recorded);
            async move {
                recorded.lock().expect("seen").push(hop.clone());
                if hop.url.ends_with("/start") {
                    // 307 默认保留方法与 body，但跨源时必须按安全语义降级。
                    Ok(redirect(307, "https://93.184.216.35/final"))
                } else {
                    Ok(WebHttpResponse {
                        status: 200,
                        headers: Vec::new(),
                        body: Vec::new(),
                        final_url: hop.url,
                    })
                }
            }
        })
        .await
        .expect("follow");
        let seen = seen.lock().expect("seen");
        let second = seen.last().expect("second hop");
        assert_eq!(second.url, "https://93.184.216.35/final");
        assert_eq!(
            second.method, "GET",
            "cross-origin redirect must not replay the method"
        );
        assert!(
            second.headers.is_empty(),
            "credentials must not cross origins"
        );
        assert!(
            second.body.is_none(),
            "body must not be replayed cross-origin"
        );
    }

    #[tokio::test]
    async fn redirect_loop_is_bounded() {
        let error = follow_redirects(
            request("https://93.184.216.34/loop"),
            |hop: WebHttpRequest| async move { Ok(redirect(302, &hop.url)) },
        )
        .await
        .expect_err("must stop after MAX_REDIRECTS");
        assert!(error.to_string().contains("too many redirects"), "{error}");
    }
}
