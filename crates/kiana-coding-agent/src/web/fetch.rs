//! Web fetch：并行抓取、HTML → 文本、10k 字符截断与逐 URL 的 SSRF 校验。
//!
//! 设计参照 `reference_project/ClaudeCode/src/tools/WebFetchTool/utils.ts`：先做 URL
//! 预检（scheme/凭据/私有地址），再取回内容、转成 markdown/纯文本、按上限截断；错误
//! 按 URL 独立返回，单个 URL 失败不影响其它 URL。
//!
//! 与参考实现的差异：本模块不调用模型做“按 prompt 提取”，只返回确定性文本；抽取属于
//! 工具层/模型层，放在这里会让 fetch 不可测试。
//!
//! # 默认值
//!
//! - [`MAX_FETCH_CHARS`] = 10_000：按 Unicode 字符截断，不会切坏 UTF-8；
//! - [`MAX_FETCH_BYTES`] = 2 MiB：传输层响应体硬上限；
//! - [`MAX_FETCH_URLS`] = 20：单次批量抓取的 URL 数上限；
//! - 并发：所有合法 URL 同时抓取，结果按输入顺序返回。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use std::collections::HashMap;

use super::http::{WebHttpRequest, WebTransport};
use super::ssrf::{resolve_public_host, validate_redirect_target, validate_url};

/// 单页返回字符上限（默认 10k）。
pub const MAX_FETCH_CHARS: usize = 10_000;
/// 响应体字节上限。
pub const MAX_FETCH_BYTES: usize = 2 * 1024 * 1024;
/// 单次批量抓取的 URL 数上限。
pub const MAX_FETCH_URLS: usize = 20;

/// 抓取选项。
#[derive(Clone)]
pub struct FetchOptions {
    /// 输出字符上限。
    pub max_chars: usize,
    /// 响应体字节上限。
    pub max_bytes: usize,
    /// URL 数上限。
    pub max_urls: usize,
    /// 单请求超时。
    pub timeout_ms: u64,
    /// 传输实现。
    pub transport: Arc<dyn WebTransport>,
}

impl FetchOptions {
    /// 用默认上限与给定传输构造。
    pub fn new(transport: Arc<dyn WebTransport>) -> Self {
        Self {
            max_chars: MAX_FETCH_CHARS,
            max_bytes: MAX_FETCH_BYTES,
            max_urls: MAX_FETCH_URLS,
            timeout_ms: 30_000,
            transport,
        }
    }
}

/// 成功抓取的一页。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchedPage {
    /// 请求 URL。
    pub url: String,
    /// 跟随重定向后的最终 URL。
    pub final_url: String,
    /// HTTP 状态码。
    pub status: u16,
    /// 小写 content-type 主类型。
    pub content_type: String,
    /// 已转成文本并截断的内容。
    pub content: String,
    /// 是否因 `max_chars` 被截断。
    pub truncated: bool,
    /// 原始响应体字节数（截断前）。
    pub bytes: usize,
}

/// 单个 URL 的失败。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchFailure {
    /// 请求 URL。
    pub url: String,
    /// 可读原因。
    pub reason: String,
}

/// 批量抓取结果；成功与失败都按输入顺序排列。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchBatch {
    /// 成功页。
    pub pages: Vec<FetchedPage>,
    /// 失败项。
    pub failures: Vec<FetchFailure>,
}

/// 并行抓取一组 URL。
///
/// 非法/被 SSRF 拒绝的 URL 不进入传输层；其余 URL 同时发起请求。结果按输入顺序返回，
/// 因此同一组输入的输出是确定的（不依赖完成顺序）。
pub async fn fetch_urls(urls: &[String], options: &FetchOptions) -> FetchBatch {
    // `slots[i]` 保存第 i 个输入 URL 的结果（成功或失败），最后按索引展开，
    // 保证两类输出都保持输入顺序（包括任务 panic/abort 的情况）。
    let mut slots: Vec<Option<Result<FetchedPage, FetchFailure>>> =
        (0..urls.len()).map(|_| None).collect();
    let mut prepared: Vec<(usize, String)> = Vec::new();
    for (index, url) in urls.iter().enumerate() {
        if index >= options.max_urls {
            slots[index] = Some(Err(FetchFailure {
                url: url.clone(),
                reason: format!("too many URLs (limit {})", options.max_urls),
            }));
            continue;
        }
        match validate_url(url) {
            Ok(_) => prepared.push((index, url.clone())),
            Err(error) => {
                slots[index] = Some(Err(FetchFailure {
                    url: url.clone(),
                    reason: error.to_string(),
                }));
            }
        }
    }

    let mut set = tokio::task::JoinSet::new();
    let mut task_urls: HashMap<tokio::task::Id, (usize, String)> = HashMap::new();
    for (index, url) in prepared {
        let transport = Arc::clone(&options.transport);
        let max_chars = options.max_chars;
        let max_bytes = options.max_bytes;
        let timeout_ms = options.timeout_ms;
        let task_url = url.clone();
        let handle = set.spawn(async move {
            let result = fetch_one(url, transport, max_chars, max_bytes, timeout_ms).await;
            (index, result)
        });
        task_urls.insert(handle.id(), (index, task_url));
    }
    while let Some(joined) = set.join_next_with_id().await {
        match joined {
            Ok((_task_id, (index, result))) => slots[index] = Some(result),
            Err(error) => {
                // 任务 panic/abort 时 JoinSet 只给出 JoinError；用 task id 找回它原本
                // 负责的输入位置，避免产生空 URL 且顺序错乱的失败项。
                if let Some((index, url)) = task_urls.remove(&error.id()) {
                    slots[index] = Some(Err(FetchFailure {
                        url,
                        reason: format!("fetch task failed: {error}"),
                    }));
                }
            }
        }
    }

    let mut pages = Vec::new();
    let mut failures = Vec::new();
    for slot in slots.into_iter().flatten() {
        match slot {
            Ok(page) => pages.push(page),
            Err(failure) => failures.push(failure),
        }
    }
    FetchBatch { pages, failures }
}

async fn fetch_one(
    url: String,
    transport: Arc<dyn WebTransport>,
    max_chars: usize,
    max_bytes: usize,
    timeout_ms: u64,
) -> Result<FetchedPage, FetchFailure> {
    let fail = |reason: String| FetchFailure {
        url: url.clone(),
        reason,
    };
    let parsed = validate_url(&url).map_err(|error| fail(error.to_string()))?;
    resolve_public_host(&parsed, false)
        .await
        .map_err(|error| fail(error.to_string()))?;

    let request = WebHttpRequest {
        method: "GET".to_string(),
        url: parsed.to_url(),
        headers: vec![
            (
                "accept".to_string(),
                "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.5".to_string(),
            ),
            (
                "accept-language".to_string(),
                "en;q=0.9,*;q=0.5".to_string(),
            ),
        ],
        body: None,
        timeout_ms: Some(timeout_ms),
        max_response_bytes: max_bytes,
    };
    let response = transport
        .execute(request)
        .await
        .map_err(|error| fail(error.to_string()))?;

    if !response.final_url.is_empty() && response.final_url != parsed.to_url() {
        validate_redirect_target(&response.final_url).map_err(|error| fail(error.to_string()))?;
    }
    if !(200..=299).contains(&response.status) {
        return Err(fail(format!("HTTP {}", response.status)));
    }
    if response.body.len() > max_bytes {
        return Err(fail(format!("response body exceeds {max_bytes} bytes")));
    }
    let content_type = response
        .content_type()
        .unwrap_or_else(|| "text/plain".to_string());
    let raw = String::from_utf8_lossy(&response.body).into_owned();
    let text = if content_type.contains("html") {
        html_to_text(&raw)
    } else if content_type.starts_with("text/")
        || content_type.contains("json")
        || content_type.contains("xml")
    {
        raw
    } else {
        return Err(fail(format!("unsupported content type {content_type:?}")));
    };
    let (content, truncated) = truncate_chars(&text, max_chars);
    Ok(FetchedPage {
        url,
        final_url: if response.final_url.is_empty() {
            parsed.to_url()
        } else {
            response.final_url
        },
        status: response.status,
        content_type,
        content,
        truncated,
        bytes: response.body.len(),
    })
}

/// 按 Unicode 字符截断；返回 `(截断后文本, 是否截断)`。
pub fn truncate_chars(text: &str, max_chars: usize) -> (String, bool) {
    if max_chars == 0 {
        return (String::new(), !text.is_empty());
    }
    match text.char_indices().nth(max_chars) {
        Some((index, _)) => (text[..index].to_string(), true),
        None => (text.to_string(), false),
    }
}

/// 最小 HTML → 文本转换：去掉注释、script/style 内容与标签，解码常见实体，压缩空白。
///
/// 这不是 HTML 解析器；对 malformed HTML 的行为是“尽量保留可见文本”，不会 panic。
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut index = 0usize;
    while index < html.len() {
        let rest = &html[index..];
        if starts_with_ignore_ascii_case(rest, "<!--") {
            index = find_ignore_ascii_case(rest, "-->", 4)
                .map(|offset| index + offset + 3)
                .unwrap_or(html.len());
            continue;
        }
        if starts_with_ignore_ascii_case(rest, "<script") {
            index = skip_raw_element(rest, "script", index, html.len());
            continue;
        }
        if starts_with_ignore_ascii_case(rest, "<style") {
            index = skip_raw_element(rest, "style", index, html.len());
            continue;
        }
        let Some(ch) = rest.chars().next() else {
            break;
        };
        if ch == '<' {
            index = rest
                .find('>')
                .map(|offset| index + offset + 1)
                .unwrap_or(html.len());
            push_space(&mut out);
            continue;
        }
        if ch == '&' {
            if let Some(semicolon) = rest.find(';').filter(|offset| *offset <= 16) {
                let entity = &rest[1..semicolon];
                if let Some(decoded) = kiana_core::html::decode_html_entity(entity) {
                    out.push_str(&decoded);
                    index += semicolon + 1;
                    continue;
                }
            }
            out.push('&');
            index += 1;
            continue;
        }
        out.push(ch);
        index += ch.len_utf8();
    }
    collapse_whitespace(&out)
}

fn skip_raw_element(rest: &str, name: &str, base: usize, limit: usize) -> usize {
    let close = format!("</{name}");
    find_ignore_ascii_case(rest, &close, 0)
        .and_then(|offset| rest[offset..].find('>').map(|end| base + offset + end + 1))
        .unwrap_or(limit)
}

fn starts_with_ignore_ascii_case(haystack: &str, prefix: &str) -> bool {
    haystack.len() >= prefix.len()
        && haystack.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

fn find_ignore_ascii_case(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    if from > haystack.len() || needle.is_empty() {
        return None;
    }
    let bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();
    if needle_bytes.len() > bytes.len() {
        return None;
    }
    for start in from..=bytes.len() - needle_bytes.len() {
        if bytes[start..start + needle_bytes.len()].eq_ignore_ascii_case(needle_bytes) {
            return Some(start);
        }
    }
    None
}

fn push_space(out: &mut String) {
    if !out.is_empty() && !out.ends_with(' ') {
        out.push(' ');
    }
}

fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            in_space = true;
            continue;
        }
        if in_space && !out.is_empty() {
            out.push(' ');
        }
        in_space = false;
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::super::http::{WebHttpResponse, WebTransportError};
    use super::*;

    #[derive(Default)]
    struct CannedTransport {
        responses: Mutex<Vec<Result<WebHttpResponse, WebTransportError>>>,
        requests: Mutex<Vec<WebHttpRequest>>,
        delay: Option<std::time::Duration>,
    }

    impl CannedTransport {
        fn new(responses: Vec<Result<WebHttpResponse, WebTransportError>>) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(responses),
                requests: Mutex::new(Vec::new()),
                delay: None,
            })
        }
    }

    #[async_trait]
    impl WebTransport for CannedTransport {
        async fn execute(
            &self,
            request: WebHttpRequest,
        ) -> Result<WebHttpResponse, WebTransportError> {
            if let Some(delay) = self.delay {
                tokio::time::sleep(delay).await;
            }
            self.requests.lock().expect("lock").push(request.clone());
            let mut responses = self.responses.lock().expect("lock");
            // 优先按 `final_url` 匹配请求 URL，让并发任务与脚本顺序解耦。
            if let Some(index) = responses.iter().position(|response| {
                matches!(response, Ok(response) if response.final_url.is_empty() || response.final_url == request.url)
            }) {
                return responses.remove(index);
            }
            responses.pop().expect("canned response")
        }
    }

    fn response(url: &str, content_type: &str, body: &str) -> WebHttpResponse {
        WebHttpResponse {
            status: 200,
            headers: vec![("content-type".to_string(), content_type.to_string())],
            body: body.as_bytes().to_vec(),
            final_url: url.to_string(),
        }
    }

    #[test]
    fn truncates_on_char_boundaries() {
        let text = "你好世界";
        let (cut, truncated) = truncate_chars(text, 2);
        assert_eq!(cut, "你好");
        assert!(truncated);
        let (all, truncated) = truncate_chars(text, 10);
        assert_eq!(all, text);
        assert!(!truncated);
        let (none, truncated) = truncate_chars(text, 0);
        assert!(none.is_empty());
        assert!(truncated);
    }

    #[test]
    fn html_to_text_strips_scripts_styles_and_tags() {
        let html = r#"<html><head><style>.x{color:red}</style><script>alert('x')</script></head>
            <body><h1>Hello &amp; welcome</h1><p>Second<br>line</p><!-- hidden --></body></html>"#;
        let text = html_to_text(html);
        assert!(!text.contains("alert"), "{text}");
        assert!(!text.contains("color"), "{text}");
        assert!(text.contains("Hello & welcome"), "{text}");
        assert!(text.contains("Second line"), "{text}");
        assert!(!text.contains("hidden"), "{text}");
        assert!(!text.contains('<'), "{text}");
    }

    struct PanickingTransport;

    #[async_trait]
    impl WebTransport for PanickingTransport {
        async fn execute(
            &self,
            _request: WebHttpRequest,
        ) -> Result<WebHttpResponse, WebTransportError> {
            panic!("simulated transport panic");
        }
    }

    #[tokio::test]
    async fn panicking_transport_reports_the_url_in_input_order() {
        let options = FetchOptions::new(Arc::new(PanickingTransport));
        let urls = vec![
            "https://93.184.216.34/a".to_string(),
            "not-a-url".to_string(),
            "https://93.184.216.34/b".to_string(),
        ];
        let batch = fetch_urls(&urls, &options).await;
        assert!(batch.pages.is_empty());
        let reported: Vec<&str> = batch
            .failures
            .iter()
            .map(|failure| failure.url.as_str())
            .collect();
        assert_eq!(reported, urls, "failures must keep input order and URLs");
        assert!(batch.failures.iter().all(|failure| !failure.url.is_empty()));
    }

    #[tokio::test]
    async fn fetches_in_parallel_and_returns_pages_in_input_order() {
        let transport = CannedTransport::new(vec![
            Ok(response("https://93.184.216.34/b", "text/plain", "second")),
            Ok(response(
                "https://93.184.216.34/a",
                "text/html; charset=utf-8",
                "<p>first</p>",
            )),
        ]);
        let options = FetchOptions::new(transport);
        let urls = vec![
            "https://93.184.216.34/a".to_string(),
            "https://93.184.216.34/b".to_string(),
        ];
        let batch = fetch_urls(&urls, &options).await;
        assert!(batch.failures.is_empty(), "{:?}", batch.failures);
        assert_eq!(batch.pages.len(), 2);
        assert_eq!(batch.pages[0].url, urls[0]);
        assert_eq!(batch.pages[0].content, "first");
        assert_eq!(batch.pages[1].url, urls[1]);
        assert_eq!(batch.pages[1].content, "second");
    }

    #[tokio::test]
    async fn private_and_metadata_urls_never_reach_the_transport() {
        let transport = CannedTransport::new(vec![]);
        let options = FetchOptions::new(transport.clone());
        let urls = vec![
            "http://127.0.0.1/secret".to_string(),
            "http://169.254.169.254/latest/meta-data".to_string(),
            "file:///etc/passwd".to_string(),
        ];
        let batch = fetch_urls(&urls, &options).await;
        assert!(batch.pages.is_empty());
        assert_eq!(batch.failures.len(), 3);
        assert!(
            batch.failures[1].reason.contains("metadata"),
            "{:?}",
            batch.failures[1]
        );
        assert!(transport.requests.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn blocked_redirect_final_url_is_rejected() {
        let mut redirected = response("http://10.0.0.5/admin", "text/plain", "secret");
        redirected.final_url = "http://10.0.0.5/admin".to_string();
        let transport = CannedTransport::new(vec![Ok(redirected)]);
        let options = FetchOptions::new(transport);
        let batch = fetch_urls(&["https://93.184.216.34/redirect".to_string()], &options).await;
        assert!(batch.pages.is_empty());
        assert_eq!(batch.failures.len(), 1);
        assert!(
            batch.failures[0].reason.contains("redirect"),
            "{:?}",
            batch.failures[0]
        );
    }

    #[tokio::test]
    async fn long_pages_are_truncated_to_ten_thousand_chars() {
        let body = "a".repeat(MAX_FETCH_CHARS + 500);
        let transport = CannedTransport::new(vec![Ok(response(
            "https://93.184.216.34/long",
            "text/plain",
            &body,
        ))]);
        let options = FetchOptions::new(transport);
        let batch = fetch_urls(&["https://93.184.216.34/long".to_string()], &options).await;
        let page = &batch.pages[0];
        assert_eq!(page.content.chars().count(), MAX_FETCH_CHARS);
        assert!(page.truncated);
        assert_eq!(page.bytes, MAX_FETCH_CHARS + 500);
    }

    #[tokio::test]
    async fn unsupported_content_type_is_a_per_url_failure() {
        let transport = CannedTransport::new(vec![Ok(WebHttpResponse {
            status: 200,
            headers: vec![("content-type".to_string(), "image/png".to_string())],
            body: vec![0, 1, 2],
            final_url: "https://93.184.216.34/img".to_string(),
        })]);
        let options = FetchOptions::new(transport);
        let batch = fetch_urls(&["https://93.184.216.34/img".to_string()], &options).await;
        assert!(batch.pages.is_empty());
        assert!(
            batch.failures[0].reason.contains("content type"),
            "{:?}",
            batch.failures[0]
        );
    }

    #[tokio::test]
    async fn too_many_urls_are_reported_without_calling_transport() {
        let transport = CannedTransport::new(vec![Ok(response(
            "https://93.184.216.34/a",
            "text/plain",
            "ok",
        ))]);
        let options = FetchOptions {
            max_urls: 1,
            ..FetchOptions::new(transport.clone())
        };
        let urls = vec![
            "https://93.184.216.34/a".to_string(),
            "https://93.184.216.34/b".to_string(),
        ];
        let batch = fetch_urls(&urls, &options).await;
        assert_eq!(batch.pages.len(), 1);
        assert_eq!(batch.failures.len(), 1);
        assert!(batch.failures[0].reason.contains("too many"));
        assert_eq!(transport.requests.lock().expect("lock").len(), 1);
    }
}
