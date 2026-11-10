//! api-openai 单元测试共用的假 HTTP 客户端与模型/请求辅助。
//!
//! 通过 `#[path = "fixtures/api-openai/mock_http.rs"] mod mock_http;` 引入到各测试文件
//! （fixtures 目录属于本单元，不与其他单元共享）。TS 测试用 `vi.mock("openai")` 顶替 SDK；
//! Rust 侧在 `HttpClient` 注入点顶替传输层，断言的是同一份请求体与 SSE 响应。

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use kiana_ai::http::{BoxFuture, HttpClient, HttpError, HttpRequest, HttpResponse};
use kiana_ai::types::{
    AssistantMessage, Context, Message, Model, ProviderResponse, SimpleStreamOptions,
    StreamOptions, TranscriptContext, UserContent, UserMessage, UserRole,
};
use serde_json::Value;

/// 预置响应。
#[derive(Clone)]
pub struct MockResponse {
    pub status: u16,
    pub headers: IndexMap<String, String>,
    pub body: Vec<u8>,
}

impl MockResponse {
    pub fn sse(events: &[Value]) -> Self {
        MockResponse {
            status: 200,
            headers: sse_headers(),
            body: sse_body(events),
        }
    }

    pub fn sse_raw(body: impl Into<Vec<u8>>) -> Self {
        MockResponse {
            status: 200,
            headers: sse_headers(),
            body: body.into(),
        }
    }

    pub fn json(status: u16, value: &Value) -> Self {
        MockResponse {
            status,
            headers: json_headers(),
            body: kiana_json::js_object::stringify(value).into_bytes(),
        }
    }

    pub fn text(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        let mut headers = IndexMap::new();
        headers.insert("content-type".to_string(), content_type.to_string());
        MockResponse {
            status,
            headers,
            body: body.into(),
        }
    }

    pub fn empty(status: u16) -> Self {
        MockResponse {
            status,
            headers: IndexMap::new(),
            body: Vec::new(),
        }
    }
}

pub fn sse_headers() -> IndexMap<String, String> {
    let mut headers = IndexMap::new();
    headers.insert("content-type".to_string(), "text/event-stream".to_string());
    headers
}

pub fn json_headers() -> IndexMap<String, String> {
    let mut headers = IndexMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    headers
}

/// `data: <json>\n\n` … + `data: [DONE]\n\n`。
pub fn sse_body(events: &[Value]) -> Vec<u8> {
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(&kiana_json::js_object::stringify(event));
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body.into_bytes()
}

/// 不带 `[DONE]` 的 SSE 体（SDK 允许流在没有结束标记时自然结束）。
pub fn sse_body_without_done(events: &[Value]) -> String {
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(&kiana_json::js_object::stringify(event));
        body.push_str("\n\n");
    }
    body
}

/// 记录请求并按顺序返回预设响应的 `HttpClient`。
#[derive(Clone, Default)]
pub struct MockHttp {
    pub requests: Arc<Mutex<Vec<HttpRequest>>>,
    pub responses: Arc<Mutex<VecDeque<MockResponse>>>,
}

impl MockHttp {
    pub fn new(responses: Vec<MockResponse>) -> Self {
        MockHttp {
            requests: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(responses.into())),
        }
    }

    pub fn single(response: MockResponse) -> Self {
        Self::new(vec![response])
    }

    pub fn last_request(&self) -> HttpRequest {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last()
            .cloned()
            .expect("a request was sent")
    }

    pub fn requests(&self) -> Vec<HttpRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn request_count(&self) -> usize {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// 最后一个请求的 JSON body（JS `JSON.parse` 语义）。
    pub fn last_body(&self) -> Value {
        let request = self.last_request();
        let bytes = request.body.expect("request body");
        kiana_json::js_parse::parse(&String::from_utf8_lossy(&bytes)).expect("request body is JSON")
    }

    /// 最后一个请求的 header（大小写不敏感）。
    pub fn last_header(&self, name: &str) -> Option<String> {
        let request = self.last_request();
        request
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    }
}

impl HttpClient for MockHttp {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
    ) -> BoxFuture<'a, Result<HttpResponse, HttpError>> {
        Box::pin(async move {
            self.requests
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(request);
            let response = self
                .responses
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop_front()
                .unwrap_or(MockResponse::text(
                    500,
                    "application/json",
                    b"{\"error\":{\"message\":\"no response queued\"}}".to_vec(),
                ));
            let body: kiana_ai::http::ByteStream =
                Box::pin(futures::stream::iter(vec![Ok(response.body)]));
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

/// catalog 模型（`getBuiltinModel` 的等价物）。
pub fn catalog_model(provider: &str, id: &str) -> Model {
    let value = kiana_ai::providers::catalog::get_model(provider, id)
        .unwrap_or_else(|| panic!("catalog model {provider}/{id}"))
        .clone();
    serde_json::from_value(value).expect("catalog model deserializes")
}

/// `UserMessage` 简写。
pub fn user(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.to_string()),
        timestamp: 1_700_000_000_000,
    })
}

/// `normalizeContext({ messages })` 的等价物。
pub fn context(messages: Vec<Message>) -> TranscriptContext {
    context_with(&Context {
        messages,
        ..Context::default()
    })
}

/// `normalizeContext(context)`（system prompt / tools 折进首条 system 消息）。
pub fn context_with(context: &Context) -> TranscriptContext {
    kiana_ai::utils::transcript::normalize_context(context)
}

/// 单条用户消息的上下文。
pub fn text_context(text: &str) -> TranscriptContext {
    context(vec![user(text)])
}

/// `{ base: StreamOptions { apiKey, fetch }, .. }`。
pub fn simple_options(mock: &MockHttp, api_key: &str) -> SimpleStreamOptions {
    SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some(api_key.to_string()),
            fetch: Some(Arc::new(mock.clone())),
            ..StreamOptions::default()
        },
        ..SimpleStreamOptions::default()
    }
}

/// [`simple_options`] 但可继续改字段。
pub fn stream_options(mock: &MockHttp, api_key: &str) -> StreamOptions {
    StreamOptions {
        api_key: Some(api_key.to_string()),
        fetch: Some(Arc::new(mock.clone())),
        ..StreamOptions::default()
    }
}

/// 运行 `openai-completions` 的 `streamSimple` 并取最终消息。
pub async fn run_completions_simple(
    model: &Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
) -> AssistantMessage {
    kiana_ai::api::openai_completions::stream_simple(model, context, options)
        .expect("streamSimple returns a stream")
        .result()
        .await
        .expect("stream result")
}

/// 运行 `openai-completions` 的 `stream` 并取最终消息。
pub async fn run_completions(
    model: &Model,
    context: &TranscriptContext,
    options: kiana_ai::api::openai_completions::OpenAICompletionsOptions,
) -> AssistantMessage {
    kiana_ai::api::openai_completions::stream(model, context, options)
        .result()
        .await
        .expect("stream result")
}

/// `processResponsesStream` 的输入事件流。
pub fn event_stream(
    events: Vec<Value>,
) -> kiana_ai::api::openai_responses_shared::ProviderEventStream {
    Box::pin(futures::stream::iter(events.into_iter().map(Ok)))
}

/// TS 测试里的 `createOutput(model)`：pending、零用量。
pub fn responses_output(model: &Model) -> AssistantMessage {
    AssistantMessage {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        stop_reason: kiana_ai::types::StopReason::Pending,
        timestamp: 1_700_000_000_000,
        ..AssistantMessage::default()
    }
}

/// 把队列里已有的事件全部取出（`processResponsesStream` 不会结束流）。
pub async fn drain_queued(
    stream: &kiana_ai::event_stream::AssistantMessageEventStream,
) -> Vec<kiana_ai::types::AssistantMessageEvent> {
    stream.end(AssistantMessage::default());
    let mut events = Vec::new();
    while let Some(event) = stream.recv().await {
        events.push(event);
    }
    events
}

/// 运行 `openai-responses` 的 `stream` 并取最终消息。
pub async fn run_responses(
    model: &Model,
    context: &TranscriptContext,
    options: kiana_ai::api::openai_responses::OpenAIResponsesOptions,
) -> AssistantMessage {
    kiana_ai::api::openai_responses::stream(model, context, options)
        .result()
        .await
        .expect("stream result")
}

/// 运行 `azure-openai-responses` 的 `stream` 并取最终消息。
pub async fn run_azure(
    model: &Model,
    context: &TranscriptContext,
    options: kiana_ai::api::azure_openai_responses::AzureOpenAIResponsesOptions,
) -> AssistantMessage {
    kiana_ai::api::azure_openai_responses::stream(model, context, options)
        .result()
        .await
        .expect("stream result")
}
