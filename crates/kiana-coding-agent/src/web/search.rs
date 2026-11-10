//! Web 搜索引擎：统一 trait、有序引擎链与官方 API 适配器。
//!
//! 设计参照：
//! - `reference_project/oh-my-pi/packages/coding-agent/src/web/search/{index,types,providers/base}.ts`
//!   —— 有序候选链、`SearchResponse`/`SearchFailureDetail`、失败后顺序切换下一引擎；
//! - `reference_project/ClaudeCode/src/tools/WebSearchTool/WebSearchTool.ts` —— 每个引擎的
//!   错误归一化与“所有引擎失败”提示；
//! - `reference_project/gemini-cli/packages/core/src/tools/web-search*.ts` —— 官方 API 的
//!   请求/响应形状。
//!
//! 行为重写，没有复制参考代码。链的顺序固定为「首选 → fallback → 其余」；每个引擎最多
//! `per_engine_attempts`（默认 3）次尝试，指数退避；整条链的**额外重试**总数由
//! `chain_max_retries` 限制（首次尝试不计入）。
//!
//! # 失败契约
//!
//! - 可重试：传输失败、超时、HTTP 429/5xx；
//! - 不可重试：未配置、401/403、响应缺字段（换下一个引擎比重试更可能成功）；
//! - 全部失败时 [`SearchChainError`] 保留每个引擎的尝试次数与原因，`Display` 可直接
//!   展示给用户或模型。
//!
//! # 默认值
//!
//! `per_engine_attempts = 3`、`backoff_base_ms = 250`、`backoff_max_ms = 2_000`、
//! `chain_max_retries = 6`、`max_results = 10`。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::http::{WebHttpRequest, WebTransport, WebTransportError};

/// 单引擎默认尝试次数。
pub const DEFAULT_PER_ENGINE_ATTEMPTS: usize = 3;
/// 整链默认额外重试次数（不含每个引擎的首次尝试）。
pub const DEFAULT_CHAIN_MAX_RETRIES: usize = 6;
/// 默认返回结果数上限。
pub const DEFAULT_MAX_RESULTS: usize = 10;
/// 首次退避（毫秒）。
pub const DEFAULT_BACKOFF_BASE_MS: u64 = 250;
/// 单次退避上限（毫秒）。
pub const DEFAULT_BACKOFF_MAX_MS: u64 = 2_000;

/// 时间范围过滤。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recency {
    /// 最近一天。
    Day,
    /// 最近一周。
    Week,
    /// 最近一月。
    Month,
    /// 最近一年。
    Year,
}

/// 一次搜索请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    /// 查询文本。
    pub query: String,
    /// 期望结果数（引擎链最终还会按 `max_results` 截断）。
    pub limit: usize,
    /// 可选时间范围。
    pub recency: Option<Recency>,
}

impl SearchQuery {
    /// 用默认 `limit = 10` 构造。
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            limit: DEFAULT_MAX_RESULTS,
            recency: None,
        }
    }
}

/// 归一化后的单条搜索结果。
///
/// 所有引擎输出同一 shape；`engine` 由链在成功时补齐（引擎自己也会填）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    /// 标题。
    pub title: String,
    /// 结果 URL（只保留 http/https）。
    pub url: String,
    /// 摘要。
    pub snippet: String,
    /// 产出该结果的引擎 id。
    pub engine: String,
    /// 可选发布时间文本（引擎原样透传，不解析）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
}

/// 一次成功搜索的响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    /// 原始查询。
    pub query: String,
    /// 最终成功的引擎 id。
    pub engine: String,
    /// 归一化结果（已按请求/配置截断）。
    pub results: Vec<SearchResult>,
    /// 成功前失败的引擎；为空表示首选引擎直接成功。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<EngineFailure>,
}

/// 一个引擎的失败记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineFailure {
    /// 引擎 id。
    pub engine: String,
    /// 链中的角色。
    pub role: EngineRole,
    /// 实际尝试次数。
    pub attempts: usize,
    /// 最后一次失败原因。
    pub error: String,
}

/// 引擎在链中的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineRole {
    /// 首选。
    Primary,
    /// 显式 fallback。
    Fallback,
    /// 其余候选。
    Remaining,
}

/// 引擎级错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SearchEngineError {
    /// 缺少 API key 等配置，本引擎不可用。
    #[error("engine is not configured")]
    Unavailable,
    /// 可重试失败（网络、超时、429、5xx）。
    #[error("engine request failed: {0}")]
    Retryable(String),
    /// 不可重试失败（401/403、非法响应）。
    #[error("engine request failed permanently: {0}")]
    Fatal(String),
    /// 响应 shape 不符合预期。
    #[error("engine returned an invalid response: {0}")]
    BadResponse(String),
}

impl SearchEngineError {
    /// 是否值得重试。
    pub fn is_retryable(&self) -> bool {
        matches!(self, SearchEngineError::Retryable(_))
    }
}

/// 搜索引擎契约。
// `async_trait` 展开的 Future 方法自带 `#[must_use]`，clippy 看不到宏内属性。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait SearchEngine: Send + Sync {
    /// 稳定引擎 id（如 `brave`、`google-cse`、`bing`）。
    fn id(&self) -> &str;

    /// 是否有足够配置可以发起请求。
    fn is_available(&self) -> bool {
        true
    }

    /// 执行一次搜索；返回未截断的归一化结果。
    async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>, SearchEngineError>;
}

/// 链级配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchChainConfig {
    /// 单引擎最多尝试次数（含首次）。
    pub per_engine_attempts: usize,
    /// 首次退避。
    pub backoff_base_ms: u64,
    /// 单次退避上限。
    pub backoff_max_ms: u64,
    /// 整链额外重试总数。
    pub chain_max_retries: usize,
    /// 返回结果上限。
    pub max_results: usize,
}

impl Default for SearchChainConfig {
    fn default() -> Self {
        Self {
            per_engine_attempts: DEFAULT_PER_ENGINE_ATTEMPTS,
            backoff_base_ms: DEFAULT_BACKOFF_BASE_MS,
            backoff_max_ms: DEFAULT_BACKOFF_MAX_MS,
            chain_max_retries: DEFAULT_CHAIN_MAX_RETRIES,
            max_results: DEFAULT_MAX_RESULTS,
        }
    }
}

impl SearchChainConfig {
    fn attempt_limit(&self) -> usize {
        self.per_engine_attempts.max(1)
    }

    fn backoff_for(&self, attempt: usize) -> u64 {
        let exponent = attempt.saturating_sub(1).min(16) as u32;
        self.backoff_base_ms
            .saturating_mul(1u64 << exponent)
            .min(self.backoff_max_ms)
    }
}

/// 全部引擎失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchChainError {
    /// 按链顺序记录的失败。
    pub failures: Vec<EngineFailure>,
}

impl std::fmt::Display for SearchChainError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "All web search engines failed: {}",
            format_failures(&self.failures)
        )
    }
}

impl std::error::Error for SearchChainError {}

fn format_failures(failures: &[EngineFailure]) -> String {
    if failures.is_empty() {
        return "no web search engine configured".to_string();
    }
    failures
        .iter()
        .map(|failure| {
            format!(
                "{} ({} attempt{}): {}",
                failure.engine,
                failure.attempts,
                if failure.attempts == 1 { "" } else { "s" },
                failure.error
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// 有序引擎链：首选 + fallback + 其余。
pub struct SearchChain {
    engines: Vec<(EngineRole, Arc<dyn SearchEngine>)>,
    config: SearchChainConfig,
}

impl SearchChain {
    /// 按三段构造；空引擎列表是合法的，此时 `run` 返回“未配置”错误。
    pub fn new(
        primary: Option<Arc<dyn SearchEngine>>,
        fallback: Vec<Arc<dyn SearchEngine>>,
        remaining: Vec<Arc<dyn SearchEngine>>,
    ) -> Self {
        Self::with_config(primary, fallback, remaining, SearchChainConfig::default())
    }

    /// 带配置构造。
    pub fn with_config(
        primary: Option<Arc<dyn SearchEngine>>,
        fallback: Vec<Arc<dyn SearchEngine>>,
        remaining: Vec<Arc<dyn SearchEngine>>,
        config: SearchChainConfig,
    ) -> Self {
        let mut engines = Vec::new();
        if let Some(primary) = primary {
            engines.push((EngineRole::Primary, primary));
        }
        engines.extend(
            fallback
                .into_iter()
                .map(|engine| (EngineRole::Fallback, engine)),
        );
        engines.extend(
            remaining
                .into_iter()
                .map(|engine| (EngineRole::Remaining, engine)),
        );
        Self { engines, config }
    }

    /// 当前链顺序（id）。
    pub fn ordered_ids(&self) -> Vec<&str> {
        self.engines.iter().map(|(_, engine)| engine.id()).collect()
    }

    /// 链配置。
    pub fn config(&self) -> &SearchChainConfig {
        &self.config
    }

    /// 依次尝试引擎；第一个成功的结果立即返回。
    pub async fn run(&self, query: &SearchQuery) -> Result<SearchResponse, SearchChainError> {
        let mut failures = Vec::new();
        let mut retries_used = 0usize;

        for (role, engine) in &self.engines {
            let engine_id = engine.id().to_string();
            if !engine.is_available() {
                failures.push(EngineFailure {
                    engine: engine_id,
                    role: *role,
                    attempts: 0,
                    error: SearchEngineError::Unavailable.to_string(),
                });
                continue;
            }

            let mut attempts = 0usize;
            loop {
                attempts += 1;
                match engine.search(query).await {
                    Ok(mut results) => {
                        let limit = self.config.max_results.min(query.limit.max(1));
                        results.truncate(limit);
                        for result in &mut results {
                            if result.engine.is_empty() {
                                result.engine.clone_from(&engine_id);
                            }
                        }
                        return Ok(SearchResponse {
                            query: query.query.clone(),
                            engine: engine_id,
                            results,
                            failures,
                        });
                    }
                    Err(error) => {
                        let can_retry = error.is_retryable()
                            && attempts < self.config.attempt_limit()
                            && retries_used < self.config.chain_max_retries;
                        if can_retry {
                            retries_used += 1;
                            let delay = self.config.backoff_for(attempts);
                            if delay > 0 {
                                tokio::time::sleep(Duration::from_millis(delay)).await;
                            }
                            continue;
                        }
                        failures.push(EngineFailure {
                            engine: engine_id,
                            role: *role,
                            attempts,
                            error: error.to_string(),
                        });
                        break;
                    }
                }
            }
        }

        Err(SearchChainError { failures })
    }
}

/// Brave Search API 适配器。
///
/// 端点：`GET https://api.search.brave.com/res/v1/web/search`；鉴权头
/// `X-Subscription-Token`。响应：`web.results[]` 的 `title`/`url`/`description`/`age`。
pub struct BraveSearchEngine {
    transport: Arc<dyn WebTransport>,
    api_key: String,
    endpoint: String,
    timeout_ms: u64,
}

impl BraveSearchEngine {
    /// 用官方端点构造。
    pub fn new(transport: Arc<dyn WebTransport>, api_key: impl Into<String>) -> Self {
        Self::with_endpoint(
            transport,
            api_key,
            "https://api.search.brave.com/res/v1/web/search",
        )
    }

    /// 自定义端点（测试使用）。
    pub fn with_endpoint(
        transport: Arc<dyn WebTransport>,
        api_key: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Self {
        Self {
            transport,
            api_key: api_key.into(),
            endpoint: endpoint.into(),
            timeout_ms: 30_000,
        }
    }
}

#[async_trait]
impl SearchEngine for BraveSearchEngine {
    fn id(&self) -> &str {
        "brave"
    }

    fn is_available(&self) -> bool {
        !self.api_key.trim().is_empty()
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>, SearchEngineError> {
        let count = query.limit.max(1);
        let mut url = format!(
            "{}?q={}&count={}",
            self.endpoint.trim_end_matches('?'),
            encode_component(&query.query),
            count
        );
        if let Some(recency) = query.recency {
            url.push_str("&freshness=");
            url.push_str(match recency {
                Recency::Day => "pd",
                Recency::Week => "pw",
                Recency::Month => "pm",
                Recency::Year => "py",
            });
        }
        let headers = vec![
            ("accept".to_string(), "application/json".to_string()),
            ("x-subscription-token".to_string(), self.api_key.clone()),
        ];
        let value = get_json(&*self.transport, &url, headers, self.timeout_ms).await?;
        let mut results = Vec::new();
        let web = value.get("web");
        if let Some(web) = web {
            let items = web
                .get("results")
                .and_then(Value::as_array)
                .ok_or_else(|| SearchEngineError::BadResponse("web.results is missing".into()))?;
            for item in items {
                if let Some(result) = normalize_item(
                    self.id(),
                    item.get("title").and_then(Value::as_str),
                    item.get("url").and_then(Value::as_str),
                    item.get("description").and_then(Value::as_str),
                    item.get("age").and_then(Value::as_str),
                ) {
                    results.push(result);
                }
            }
        }
        Ok(results)
    }
}

/// Google Programmable Search (Custom Search JSON API) 适配器。
///
/// 端点：`GET https://www.googleapis.com/customsearch/v1?key=&cx=&q=`；
/// 响应：`items[]` 的 `title`/`link`/`snippet`。
pub struct GoogleCseEngine {
    transport: Arc<dyn WebTransport>,
    api_key: String,
    cx: String,
    endpoint: String,
    timeout_ms: u64,
}

impl GoogleCseEngine {
    /// 用官方端点构造。
    pub fn new(
        transport: Arc<dyn WebTransport>,
        api_key: impl Into<String>,
        cx: impl Into<String>,
    ) -> Self {
        Self::with_endpoint(
            transport,
            api_key,
            cx,
            "https://www.googleapis.com/customsearch/v1",
        )
    }

    /// 自定义端点（测试使用）。
    pub fn with_endpoint(
        transport: Arc<dyn WebTransport>,
        api_key: impl Into<String>,
        cx: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Self {
        Self {
            transport,
            api_key: api_key.into(),
            cx: cx.into(),
            endpoint: endpoint.into(),
            timeout_ms: 30_000,
        }
    }
}

#[async_trait]
impl SearchEngine for GoogleCseEngine {
    fn id(&self) -> &str {
        "google-cse"
    }

    fn is_available(&self) -> bool {
        !self.api_key.trim().is_empty() && !self.cx.trim().is_empty()
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>, SearchEngineError> {
        let count = query.limit.max(1);
        let mut url = format!(
            "{}?key={}&cx={}&q={}&num={}",
            self.endpoint.trim_end_matches('?'),
            encode_component(&self.api_key),
            encode_component(&self.cx),
            encode_component(&query.query),
            count
        );
        if let Some(recency) = query.recency {
            url.push_str("&dateRestrict=");
            url.push_str(match recency {
                Recency::Day => "d1",
                Recency::Week => "w1",
                Recency::Month => "m1",
                Recency::Year => "y1",
            });
        }
        let headers = vec![("accept".to_string(), "application/json".to_string())];
        let value = get_json(&*self.transport, &url, headers, self.timeout_ms).await?;
        let mut results = Vec::new();
        if let Some(items) = value.get("items") {
            let items = items
                .as_array()
                .ok_or_else(|| SearchEngineError::BadResponse("items is not an array".into()))?;
            for item in items {
                if let Some(result) = normalize_item(
                    self.id(),
                    item.get("title").and_then(Value::as_str),
                    item.get("link").and_then(Value::as_str),
                    item.get("snippet").and_then(Value::as_str),
                    None,
                ) {
                    results.push(result);
                }
            }
        }
        Ok(results)
    }
}

/// Bing Web Search API 适配器。
///
/// 端点：`GET https://api.bing.microsoft.com/v7.0/search?q=`；鉴权头
/// `Ocp-Apim-Subscription-Key`；响应：`webPages.value[]` 的 `name`/`url`/`snippet`。
pub struct BingSearchEngine {
    transport: Arc<dyn WebTransport>,
    api_key: String,
    endpoint: String,
    timeout_ms: u64,
}

impl BingSearchEngine {
    /// 用官方端点构造。
    pub fn new(transport: Arc<dyn WebTransport>, api_key: impl Into<String>) -> Self {
        Self::with_endpoint(
            transport,
            api_key,
            "https://api.bing.microsoft.com/v7.0/search",
        )
    }

    /// 自定义端点（测试使用）。
    pub fn with_endpoint(
        transport: Arc<dyn WebTransport>,
        api_key: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Self {
        Self {
            transport,
            api_key: api_key.into(),
            endpoint: endpoint.into(),
            timeout_ms: 30_000,
        }
    }
}

#[async_trait]
impl SearchEngine for BingSearchEngine {
    fn id(&self) -> &str {
        "bing"
    }

    fn is_available(&self) -> bool {
        !self.api_key.trim().is_empty()
    }

    async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>, SearchEngineError> {
        let mut url = format!(
            "{}?q={}&count={}",
            self.endpoint.trim_end_matches('?'),
            encode_component(&query.query),
            query.limit.max(1)
        );
        if let Some(recency) = query.recency {
            url.push_str("&freshness=");
            url.push_str(match recency {
                Recency::Day => "Day",
                Recency::Week => "Week",
                Recency::Month => "Month",
                Recency::Year => "Year",
            });
        }
        let headers = vec![
            ("accept".to_string(), "application/json".to_string()),
            (
                "ocp-apim-subscription-key".to_string(),
                self.api_key.clone(),
            ),
        ];
        let value = get_json(&*self.transport, &url, headers, self.timeout_ms).await?;
        let mut results = Vec::new();
        if let Some(pages) = value.get("webPages") {
            let items = pages
                .get("value")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    SearchEngineError::BadResponse("webPages.value is missing".into())
                })?;
            for item in items {
                if let Some(result) = normalize_item(
                    self.id(),
                    item.get("name").and_then(Value::as_str),
                    item.get("url").and_then(Value::as_str),
                    item.get("snippet").and_then(Value::as_str),
                    item.get("datePublished").and_then(Value::as_str),
                ) {
                    results.push(result);
                }
            }
        }
        Ok(results)
    }
}

async fn get_json(
    transport: &dyn WebTransport,
    url: &str,
    headers: Vec<(String, String)>,
    timeout_ms: u64,
) -> Result<Value, SearchEngineError> {
    // 引擎端点来自配置/官方常量，只做结构校验，不做 DNS 预解析：真正的 SSRF 边界在
    // WebFetch（用户可控 URL）；搜索端点不是用户输入。
    if super::ssrf::validate_url(url).is_err() {
        return Err(SearchEngineError::Fatal(format!(
            "invalid search endpoint {url:?}"
        )));
    }
    let request = WebHttpRequest {
        method: "GET".to_string(),
        url: url.to_string(),
        headers,
        body: None,
        timeout_ms: Some(timeout_ms),
        max_response_bytes: 2 * 1024 * 1024,
    };
    let response = transport.execute(request).await.map_err(transport_error)?;
    if !response.final_url.is_empty() && response.final_url != url {
        super::ssrf::validate_redirect_target(&response.final_url)
            .map_err(|error| SearchEngineError::Fatal(error.to_string()))?;
    }
    match response.status {
        200..=299 => {}
        401 | 403 => {
            return Err(SearchEngineError::Fatal(format!(
                "authorization failed (HTTP {})",
                response.status
            )));
        }
        429 => {
            return Err(SearchEngineError::Retryable(format!(
                "rate limited (HTTP {})",
                response.status
            )));
        }
        500..=599 => {
            return Err(SearchEngineError::Retryable(format!(
                "server error (HTTP {})",
                response.status
            )));
        }
        status => {
            return Err(SearchEngineError::Fatal(format!(
                "unexpected HTTP status {status}"
            )));
        }
    }
    serde_json::from_slice(&response.body)
        .map_err(|error| SearchEngineError::BadResponse(format!("invalid JSON: {error}")))
}

fn transport_error(error: WebTransportError) -> SearchEngineError {
    match error {
        WebTransportError::UnsupportedMethod(_) => SearchEngineError::Fatal(error.to_string()),
        WebTransportError::TooLarge { .. } => SearchEngineError::Fatal(error.to_string()),
        WebTransportError::Request(_) | WebTransportError::Timeout { .. } => {
            SearchEngineError::Retryable(error.to_string())
        }
    }
}

fn normalize_item(
    engine: &str,
    title: Option<&str>,
    url: Option<&str>,
    snippet: Option<&str>,
    published: Option<&str>,
) -> Option<SearchResult> {
    let title = title?.trim();
    let url = url?.trim();
    if title.is_empty() || url.is_empty() {
        return None;
    }
    // scheme 大小写不敏感（RFC 3986）；引擎偶尔返回 `HTTP://`。
    let scheme_probe = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    if !(scheme_probe.starts_with("http://") || scheme_probe.starts_with("https://")) {
        return None;
    }
    Some(SearchResult {
        title: title.to_string(),
        url: url.to_string(),
        snippet: snippet.unwrap_or("").trim().to_string(),
        engine: engine.to_string(),
        published: published
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
    })
}

/// RFC 3986 unreserved 之外全部 percent-encode；空格编码为 `%20`。
fn encode_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;

    #[test]
    fn normalize_item_accepts_case_insensitive_schemes() {
        let item = normalize_item(
            "brave",
            Some("Example"),
            Some("HTTPS://Example.com/x"),
            None,
            None,
        )
        .expect("uppercase scheme is valid");
        assert_eq!(item.url, "HTTPS://Example.com/x");
        assert!(
            normalize_item("brave", Some("T"), Some("ftp://example.com"), None, None).is_none()
        );
    }

    #[derive(Default)]
    struct ScriptedTransport {
        responses: Mutex<VecDeque<Result<super::super::http::WebHttpResponse, WebTransportError>>>,
        requests: Mutex<Vec<WebHttpRequest>>,
    }

    impl ScriptedTransport {
        fn new(
            responses: Vec<Result<super::super::http::WebHttpResponse, WebTransportError>>,
        ) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(responses.into()),
                requests: Mutex::new(Vec::new()),
            })
        }

        fn request_count(&self) -> usize {
            self.requests.lock().expect("lock").len()
        }
    }

    #[async_trait]
    impl WebTransport for ScriptedTransport {
        async fn execute(
            &self,
            request: WebHttpRequest,
        ) -> Result<super::super::http::WebHttpResponse, WebTransportError> {
            self.requests.lock().expect("lock").push(request);
            self.responses
                .lock()
                .expect("lock")
                .pop_front()
                .expect("scripted response")
        }
    }

    fn json_response(status: u16, value: Value) -> super::super::http::WebHttpResponse {
        super::super::http::WebHttpResponse {
            status,
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body: serde_json::to_vec(&value).expect("json"),
            final_url: String::new(),
        }
    }

    #[test]
    fn encodes_queries_without_ambiguous_characters() {
        assert_eq!(encode_component("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(encode_component("中文"), "%E4%B8%AD%E6%96%87");
    }

    #[tokio::test]
    async fn brave_response_is_normalized() {
        let transport = ScriptedTransport::new(vec![Ok(json_response(
            200,
            serde_json::json!({
                "web": {"results": [
                    {"title": "Rust", "url": "https://www.rust-lang.org/", "description": "lang", "age": "2 days"},
                    {"title": "bad", "url": "ftp://example.com/x"}
                ]}
            }),
        ))]);
        let engine = BraveSearchEngine::with_endpoint(
            transport.clone(),
            "token",
            "https://brave.test/res/v1/web/search",
        );
        let results = engine
            .search(&SearchQuery::new("rust lang"))
            .await
            .expect("ok");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].engine, "brave");
        assert_eq!(results[0].title, "Rust");
        assert_eq!(results[0].published.as_deref(), Some("2 days"));
        let requests = transport.requests.lock().expect("lock");
        assert!(requests[0].url.contains("q=rust%20lang"));
        assert!(
            requests[0]
                .headers
                .iter()
                .any(|(name, value)| name == "x-subscription-token" && value == "token")
        );
    }

    #[tokio::test]
    async fn google_and_bing_responses_share_the_same_schema() {
        let google = ScriptedTransport::new(vec![Ok(json_response(
            200,
            serde_json::json!({"items": [{"title": "G", "link": "https://g.example/", "snippet": "s"}]}),
        ))]);
        let engine = GoogleCseEngine::with_endpoint(
            google,
            "key",
            "cx",
            "https://google.test/customsearch/v1",
        );
        let results = engine.search(&SearchQuery::new("q")).await.expect("ok");
        assert_eq!(results[0].engine, "google-cse");
        assert_eq!(results[0].url, "https://g.example/");

        let bing = ScriptedTransport::new(vec![Ok(json_response(
            200,
            serde_json::json!({"webPages": {"value": [{"name": "B", "url": "https://b.example/", "snippet": "s"}]}}),
        ))]);
        let engine = BingSearchEngine::with_endpoint(bing, "key", "https://bing.test/v7.0/search");
        let results = engine.search(&SearchQuery::new("q")).await.expect("ok");
        assert_eq!(results[0].engine, "bing");
        assert_eq!(results[0].url, "https://b.example/");
    }

    struct StaticEngine {
        id: &'static str,
        results: Mutex<VecDeque<Result<Vec<SearchResult>, SearchEngineError>>>,
    }

    impl StaticEngine {
        fn new(
            id: &'static str,
            results: Vec<Result<Vec<SearchResult>, SearchEngineError>>,
        ) -> Arc<Self> {
            Arc::new(Self {
                id,
                results: Mutex::new(results.into()),
            })
        }
    }

    #[async_trait]
    impl SearchEngine for StaticEngine {
        fn id(&self) -> &str {
            self.id
        }

        async fn search(
            &self,
            _query: &SearchQuery,
        ) -> Result<Vec<SearchResult>, SearchEngineError> {
            self.results
                .lock()
                .expect("lock")
                .pop_front()
                .expect("scripted engine result")
        }
    }

    fn result(engine: &str, n: usize) -> Vec<SearchResult> {
        vec![SearchResult {
            title: format!("r{n}"),
            url: format!("https://example.com/{n}"),
            snippet: String::new(),
            engine: engine.to_string(),
            published: None,
        }]
    }

    #[tokio::test]
    async fn primary_failure_falls_back_to_next_engine_with_same_schema() {
        let primary = StaticEngine::new(
            "primary",
            vec![Err(SearchEngineError::Fatal("boom".into()))],
        );
        let fallback = StaticEngine::new("fallback", vec![Ok(result("fallback", 1))]);
        let chain = SearchChain::with_config(
            Some(primary),
            vec![fallback],
            vec![],
            SearchChainConfig {
                backoff_base_ms: 0,
                ..Default::default()
            },
        );
        let response = chain
            .run(&SearchQuery::new("q"))
            .await
            .expect("fallback works");
        assert_eq!(response.engine, "fallback");
        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].url, "https://example.com/1");
        assert_eq!(response.failures.len(), 1);
        assert_eq!(response.failures[0].engine, "primary");
        assert_eq!(response.failures[0].attempts, 1);
    }

    #[tokio::test]
    async fn retryable_engine_is_retried_three_times_before_fallback() {
        let primary = StaticEngine::new(
            "primary",
            vec![
                Err(SearchEngineError::Retryable("500".into())),
                Err(SearchEngineError::Retryable("500".into())),
                Err(SearchEngineError::Retryable("500".into())),
            ],
        );
        let fallback = StaticEngine::new("fallback", vec![Ok(result("fallback", 2))]);
        let chain = SearchChain::with_config(
            Some(primary.clone()),
            vec![fallback],
            vec![],
            SearchChainConfig {
                backoff_base_ms: 0,
                ..Default::default()
            },
        );
        let response = chain.run(&SearchQuery::new("q")).await.expect("ok");
        assert_eq!(response.engine, "fallback");
        assert_eq!(response.failures[0].attempts, 3);
        assert!(primary.results.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn all_failures_produce_actionable_error_with_engine_and_reason() {
        let primary = StaticEngine::new(
            "primary",
            vec![Err(SearchEngineError::Fatal("401 bad key".into()))],
        );
        let secondary = StaticEngine::new(
            "secondary",
            vec![Err(SearchEngineError::Fatal("timeout".into()))],
        );
        let chain = SearchChain::with_config(
            Some(primary),
            vec![],
            vec![secondary],
            SearchChainConfig {
                backoff_base_ms: 0,
                ..Default::default()
            },
        );
        let error = chain
            .run(&SearchQuery::new("q"))
            .await
            .expect_err("all fail");
        let text = error.to_string();
        assert!(text.contains("primary"), "{text}");
        assert!(text.contains("401 bad key"), "{text}");
        assert!(text.contains("secondary"), "{text}");
        assert!(text.contains("timeout"), "{text}");
        assert_eq!(error.failures.len(), 2);
    }

    #[tokio::test]
    async fn chain_max_retries_caps_global_retry_budget() {
        let primary = StaticEngine::new(
            "primary",
            (0..10)
                .map(|_| Err(SearchEngineError::Retryable("flaky".into())))
                .collect(),
        );
        let secondary = StaticEngine::new(
            "secondary",
            (0..10)
                .map(|_| Err(SearchEngineError::Retryable("flaky".into())))
                .collect(),
        );
        let chain = SearchChain::with_config(
            Some(primary),
            vec![],
            vec![secondary],
            SearchChainConfig {
                backoff_base_ms: 0,
                chain_max_retries: 2,
                ..Default::default()
            },
        );
        let error = chain
            .run(&SearchQuery::new("q"))
            .await
            .expect_err("all fail");
        let total: usize = error.failures.iter().map(|failure| failure.attempts).sum();
        // 2 次全局重试 + 2 个引擎的首次尝试 = 4。
        assert_eq!(total, 4);
        assert_eq!(error.failures[0].attempts, 3);
        assert_eq!(error.failures[1].attempts, 1);
    }

    #[tokio::test]
    async fn unavailable_engine_is_skipped_without_request() {
        struct Unavailable;
        #[async_trait]
        impl SearchEngine for Unavailable {
            fn id(&self) -> &str {
                "unavailable"
            }
            fn is_available(&self) -> bool {
                false
            }
            async fn search(
                &self,
                _query: &SearchQuery,
            ) -> Result<Vec<SearchResult>, SearchEngineError> {
                panic!("must not be called")
            }
        }
        let chain = SearchChain::new(
            Some(Arc::new(Unavailable)),
            vec![StaticEngine::new("ok", vec![Ok(result("ok", 1))])],
            vec![],
        );
        let response = chain.run(&SearchQuery::new("q")).await.expect("ok");
        assert_eq!(response.engine, "ok");
        assert_eq!(response.failures[0].attempts, 0);
    }

    #[tokio::test]
    async fn results_are_truncated_to_config_limit() {
        let engine = StaticEngine::new(
            "ok",
            vec![Ok((0..20)
                .map(|n| SearchResult {
                    title: format!("r{n}"),
                    url: format!("https://example.com/{n}"),
                    snippet: String::new(),
                    engine: String::new(),
                    published: None,
                })
                .collect())],
        );
        let chain = SearchChain::with_config(
            Some(engine),
            vec![],
            vec![],
            SearchChainConfig {
                max_results: 5,
                ..Default::default()
            },
        );
        let response = chain.run(&SearchQuery::new("q")).await.expect("ok");
        assert_eq!(response.results.len(), 5);
        assert_eq!(response.results[0].engine, "ok");
    }

    #[tokio::test]
    async fn non_2xx_auth_failure_is_not_retried() {
        let transport = ScriptedTransport::new(vec![Ok(super::super::http::WebHttpResponse {
            status: 401,
            headers: vec![],
            body: b"nope".to_vec(),
            final_url: String::new(),
        })]);
        let engine = BraveSearchEngine::with_endpoint(
            transport.clone(),
            "token",
            "https://brave.test/search",
        );
        let error = engine
            .search(&SearchQuery::new("q"))
            .await
            .expect_err("401");
        assert!(!error.is_retryable());
        assert!(error.to_string().contains("401"), "{error}");
        assert_eq!(transport.request_count(), 1);
    }
}
