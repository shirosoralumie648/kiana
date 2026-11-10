//! `api-classifier-images` 集成测试共用的假 `fetch`（TS 测试里的 `vi.fn` + `Response.json`）。
//!
//! TS 直接用 `vi.fn(async (url, init) => Response.json(...))`；Rust 侧用实现了 [`HttpClient`]
//! 的 [`MockHttpClient`]：记录每次 `HttpRequest`，按请求返回预设的 `MockResponse`。

#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use indexmap::IndexMap;
use kiana_ai::http::{BoxFuture, ByteStream, HttpClient, HttpError, HttpRequest, HttpResponse};
use kiana_ai::types::{
    ClassifierContext, ClassifierQuestion, ImageContent, ImageKind, Model, ModelCost,
    ModelInputModality, ModelType, ProviderResponse,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// 一条被记录下来的请求。
#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub request: HttpRequest,
    /// 请求发出时调用方 signal 是否已中止。
    pub signal_cancelled: bool,
}

impl RecordedRequest {
    /// 请求体（JSON）。
    pub fn json(&self) -> Value {
        let body = self.request.body.as_deref().unwrap_or_default();
        serde_json::from_slice(body).expect("request body is JSON")
    }

    /// 请求体原始文本。
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(self.request.body.as_deref().unwrap_or_default()).into_owned()
    }

    /// 请求头（大小写不敏感）。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.request
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// 请求头名（原始大小写）。
    pub fn header_names(&self) -> Vec<&str> {
        self.request
            .headers
            .iter()
            .map(|(key, _)| key.as_str())
            .collect()
    }
}

/// 假响应。
#[derive(Clone, Debug)]
pub struct MockResponse {
    pub status: u16,
    pub headers: IndexMap<String, String>,
    pub body: Vec<u8>,
}

impl MockResponse {
    /// `Response.json(value)`。
    pub fn json(value: Value) -> Self {
        Self::json_status(200, value)
    }

    /// 指定状态的 `Response.json(value, { status })`。
    pub fn json_status(status: u16, value: Value) -> Self {
        let mut headers = IndexMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        MockResponse {
            status,
            headers,
            body: kiana_json::js_object::stringify(&value).into_bytes(),
        }
    }

    /// `new Response(text, { status })`。
    pub fn text(status: u16, body: impl Into<String>) -> Self {
        let mut headers = IndexMap::new();
        headers.insert("content-type".to_string(), "text/plain".to_string());
        MockResponse {
            status,
            headers,
            body: body.into().into_bytes(),
        }
    }

    /// `new Response(text, { status, headers })`。
    pub fn with_headers(status: u16, body: impl Into<String>, headers: &[(&str, &str)]) -> Self {
        MockResponse {
            status,
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
            body: body.into().into_bytes(),
        }
    }
}

type Handler = Box<dyn Fn(&HttpRequest) -> Result<MockResponse, HttpError> + Send + Sync>;

/// 记录请求并返回预设响应的 `HttpClient`。
pub struct MockHttpClient {
    handler: Handler,
    requests: Mutex<Vec<RecordedRequest>>,
    /// 每次成功请求前的等待（让并发调用真正重叠）。
    delay: Option<Duration>,
}

impl MockHttpClient {
    pub fn new(
        handler: impl Fn(&HttpRequest) -> MockResponse + Send + Sync + 'static,
    ) -> Arc<Self> {
        Self::try_new(move |request| Ok(handler(request)))
    }

    /// 可返回传输错误的客户端（模拟 `fetch` 拒绝：超时、连接失败）。
    pub fn try_new(
        handler: impl Fn(&HttpRequest) -> Result<MockResponse, HttpError> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Self::build(handler, None)
    }

    /// 与 [`MockHttpClient::new`] 相同，但每次成功请求先等待 `delay`。
    pub fn new_delayed(
        handler: impl Fn(&HttpRequest) -> MockResponse + Send + Sync + 'static,
        delay: Duration,
    ) -> Arc<Self> {
        Self::build(move |request| Ok(handler(request)), Some(delay))
    }

    fn build(
        handler: impl Fn(&HttpRequest) -> Result<MockResponse, HttpError> + Send + Sync + 'static,
        delay: Option<Duration>,
    ) -> Arc<Self> {
        Arc::new(MockHttpClient {
            handler: Box::new(handler),
            requests: Mutex::new(Vec::new()),
            delay,
        })
    }

    /// 总是返回同一个响应的客户端。
    pub fn always(response: MockResponse) -> Arc<Self> {
        Self::new(move |_| response.clone())
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn request_count(&self) -> usize {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    pub fn last_json(&self) -> Value {
        self.requests().last().expect("a request").json()
    }

    /// 作为 `StreamOptions.fetch` 使用。
    pub fn fetch(self: &Arc<Self>) -> Option<Arc<dyn HttpClient>> {
        Some(Arc::clone(self) as Arc<dyn HttpClient>)
    }
}

impl HttpClient for MockHttpClient {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
        signal: Option<&'a CancellationToken>,
    ) -> BoxFuture<'a, Result<HttpResponse, HttpError>> {
        let cancelled = signal.is_some_and(CancellationToken::is_cancelled);
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(RecordedRequest {
                request: request.clone(),
                signal_cancelled: cancelled,
            });
        if cancelled {
            return Box::pin(async {
                Err(HttpError::new(
                    kiana_ai::http::reqwest_client::ABORTED_MESSAGE,
                ))
            });
        }
        let response = match (self.handler)(&request) {
            Ok(response) => response,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let delay = self.delay;
        Box::pin(async move {
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            let body: ByteStream =
                Box::pin(futures::stream::once(async move { Ok(response.body) }));
            Ok(HttpResponse {
                response: ProviderResponse {
                    status: response.status,
                    headers: response.headers,
                },
                body,
            })
        })
    }
}

/// `ClassifierContext.state` 的 JSON 对象构造。
pub fn state(value: Value) -> IndexMap<String, Value> {
    match value {
        Value::Object(map) => map.into_iter().collect(),
        other => panic!("state must be an object, got {other}"),
    }
}

/// `{ type: "image", data, mimeType }`。
pub fn image(data: &str, mime_type: &str) -> ImageContent {
    ImageContent {
        kind_image: ImageKind::Image,
        data: data.to_string(),
        mime_type: mime_type.to_string(),
    }
}

/// 分类器模型（`type: "classifier"`）。
pub fn classifier_model(api: &str, provider: &str, id: &str, base_url: &str) -> Model {
    Model {
        id: id.to_string(),
        name: id.to_string(),
        api: api.to_string(),
        provider: provider.to_string(),
        base_url: base_url.to_string(),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        model_type: Some(ModelType::Classifier),
        context_window: Some(64_000),
        ..Model::default()
    }
}

/// 一个问题集合的便捷构造。
pub fn questions(entries: Vec<(&str, ClassifierQuestion)>) -> IndexMap<String, ClassifierQuestion> {
    entries
        .into_iter()
        .map(|(id, question)| (id.to_string(), question))
        .collect()
}

/// 分类上下文（无图片）。
pub fn context(
    state_value: Value,
    questions: IndexMap<String, ClassifierQuestion>,
) -> ClassifierContext {
    ClassifierContext {
        state: state(state_value),
        images: None,
        questions,
    }
}

/// 响应体流在第一个 chunk 就报错的客户端（模拟读体超时 / 传输错误）。
pub struct BodyStreamErrorClient {
    attempts: AtomicUsize,
    status: u16,
    message: &'static str,
}

impl BodyStreamErrorClient {
    pub fn new(status: u16, message: &'static str) -> Arc<Self> {
        Arc::new(Self {
            attempts: AtomicUsize::new(0),
            status,
            message,
        })
    }

    pub fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }

    /// 作为 `StreamOptions.fetch` 使用。
    pub fn fetch(self: &Arc<Self>) -> Option<Arc<dyn HttpClient>> {
        Some(Arc::clone(self) as Arc<dyn HttpClient>)
    }
}

impl HttpClient for BodyStreamErrorClient {
    fn execute<'a>(
        &'a self,
        _request: HttpRequest,
        _signal: Option<&CancellationToken>,
    ) -> BoxFuture<'a, Result<HttpResponse, HttpError>> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        let status = self.status;
        let message = self.message;
        Box::pin(async move {
            let headers: IndexMap<String, String> =
                [("content-type".to_string(), "application/json".to_string())]
                    .into_iter()
                    .collect();
            let body: ByteStream = Box::pin(futures::stream::once(async move {
                Err::<Vec<u8>, HttpError>(HttpError::with_status(message, status))
            }));
            Ok(HttpResponse {
                response: ProviderResponse { status, headers },
                body,
            })
        })
    }
}
