//! `api-anthropic-codex` 单元测试的共享夹具：可注入的 [`HttpClient`] 与事件收集。
//!
//! TS 的 `vi.stubGlobal("fetch", ...)` / `options.fetch` 在 Rust 侧对应
//! [`crate::http::HttpClient`]。本模块提供按序返回响应、记录请求的 mock，以及
//! `normalizeContext` 的小构造器。

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::http::{BoxFuture, ByteStream, HttpClient, HttpError, HttpRequest, HttpResponse};
use kiana_ai::types::{
    AssistantMessage, AssistantMessageEvent, Context, Message, Model, ProviderResponse,
    StreamOptions, TranscriptContext,
};
use kiana_ai::utils::transcript::normalize_context;
use serde_json::Value;

/// 一次 mock 响应。
#[derive(Debug, Clone, Default)]
pub struct MockResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl MockResponse {
    /// SSE 响应（`text/event-stream`）。
    pub fn sse(body: impl Into<String>) -> Self {
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/event-stream".into())],
            body: body.into().into_bytes(),
        }
    }

    /// JSON 响应。
    pub fn json(status: u16, body: impl Into<String>) -> Self {
        MockResponse {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: body.into().into_bytes(),
        }
    }

    /// 带自定义头的响应。
    pub fn with_headers(mut self, headers: Vec<(&str, &str)>) -> Self {
        for (name, value) in headers {
            self.headers.push((name.into(), value.into()));
        }
        self
    }
}

type Handler = Arc<dyn Fn(&HttpRequest) -> Result<MockResponse, HttpError> + Send + Sync>;

/// 更灵活的 mock 返回：立即、延迟、流式（可保持打开）或错误。
pub enum MockOutcome {
    /// 立即返回。
    Immediate(MockResponse),
    /// 延迟后返回（超时/中止测试用）。
    Delayed(MockResponse, std::time::Duration),
    /// 分块流式 body；`keep_open` 为真时 body 永不结束。
    Streaming {
        status: u16,
        headers: Vec<(String, String)>,
        chunks: Vec<Vec<u8>>,
        keep_open: bool,
        /// 每块之间（含第一块）延迟。
        chunk_delay: Option<std::time::Duration>,
    },
    /// 传输错误。
    Error(HttpError),
}

type OutcomeHandler = Arc<dyn Fn(&HttpRequest) -> MockOutcome + Send + Sync>;

/// 用 [`MockOutcome`] 表达行为的客户端。
pub struct MockStreamingClient {
    requests: Mutex<Vec<HttpRequest>>,
    handler: OutcomeHandler,
}

impl MockStreamingClient {
    /// 自定义行为。
    pub fn new(handler: impl Fn(&HttpRequest) -> MockOutcome + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            handler: Arc::new(handler),
        })
    }

    /// 每次请求都延迟 `delay` 后返回同一响应。
    pub fn delayed(response: MockResponse, delay: std::time::Duration) -> Arc<Self> {
        Self::new(move |_| MockOutcome::Delayed(response.clone(), delay))
    }

    /// 分块之间有 `delay` 间隔的流式 body。
    pub fn streaming_delayed(
        chunks: Vec<Vec<u8>>,
        keep_open: bool,
        delay: std::time::Duration,
    ) -> Arc<Self> {
        Self::new(move |_| MockOutcome::Streaming {
            status: 200,
            headers: vec![("content-type".into(), "text/event-stream".into())],
            chunks: chunks.clone(),
            keep_open,
            chunk_delay: Some(delay),
        })
    }

    /// 先返回给定的分块，`keep_open` 为真时后续永不结束。
    pub fn streaming(chunks: Vec<Vec<u8>>, keep_open: bool) -> Arc<Self> {
        Self::new(move |_| MockOutcome::Streaming {
            status: 200,
            headers: vec![("content-type".into(), "text/event-stream".into())],
            chunks: chunks.clone(),
            keep_open,
            chunk_delay: None,
        })
    }

    /// 收到的请求。
    pub fn requests(&self) -> Vec<HttpRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl HttpClient for MockStreamingClient {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
    ) -> BoxFuture<'a, Result<HttpResponse, HttpError>> {
        let outcome = (self.handler)(&request);
        self.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(request);
        Box::pin(async move {
            match outcome {
                MockOutcome::Immediate(response) => immediate_response(response),
                MockOutcome::Delayed(response, delay) => {
                    tokio::time::sleep(delay).await;
                    immediate_response(response)
                }
                MockOutcome::Streaming {
                    status,
                    headers,
                    chunks,
                    keep_open,
                    chunk_delay,
                } => {
                    let state: Vec<Vec<u8>> = chunks.into_iter().rev().collect();
                    let stream: ByteStream = Box::pin(futures::stream::unfold(
                        (state, keep_open),
                        move |(mut state, keep_open)| async move {
                            if let Some(delay) = chunk_delay {
                                tokio::time::sleep(delay).await;
                            }
                            if let Some(chunk) = state.pop() {
                                Some((Ok(chunk), (state, keep_open)))
                            } else if keep_open {
                                futures::future::pending::<()>().await;
                                None
                            } else {
                                None
                            }
                        },
                    ));
                    Ok(HttpResponse {
                        response: ProviderResponse {
                            status,
                            headers: headers.into_iter().collect(),
                        },
                        body: stream,
                    })
                }
                MockOutcome::Error(error) => Err(error),
            }
        })
    }
}

fn immediate_response(response: MockResponse) -> Result<HttpResponse, HttpError> {
    let body: ByteStream = Box::pin(futures::stream::iter(vec![Ok(response.body)]));
    Ok(HttpResponse {
        response: ProviderResponse {
            status: response.status,
            headers: response.headers.into_iter().collect(),
        },
        body,
    })
}

/// 记录请求并按序返回 mock 响应的 HTTP 客户端。
pub struct MockHttpClient {
    requests: Mutex<Vec<HttpRequest>>,
    handler: Handler,
}

impl MockHttpClient {
    /// 按顺序返回给定的响应；次数用尽后返回 `500`。
    pub fn sequential(responses: Vec<MockResponse>) -> Arc<Self> {
        let slot = Arc::new(Mutex::new(responses));
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            handler: Arc::new(move |_| {
                let mut responses = slot.lock().unwrap_or_else(|error| error.into_inner());
                if responses.is_empty() {
                    return Ok(MockResponse::json(
                        500,
                        "{\"error\":\"no response queued\"}",
                    ));
                }
                Ok(responses.remove(0))
            }),
        })
    }

    /// 每次都返回同一响应。
    pub fn fixed(response: MockResponse) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            handler: Arc::new(move |_| Ok(response.clone())),
        })
    }

    /// 自定义处理器。
    pub fn with_handler(
        handler: impl Fn(&HttpRequest) -> Result<MockResponse, HttpError> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            handler: Arc::new(handler),
        })
    }

    /// 收到的请求（按到达顺序）。
    pub fn requests(&self) -> Vec<HttpRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// 收到的第一个请求体（JSON 解析；支持 zstd 压缩）。
    pub fn first_json_body(&self) -> Option<Value> {
        let requests = self.requests();
        let request = requests.first()?;
        decode_body(request.body.as_deref()?)
    }
}

/// 解码请求体：优先按 zstd 解压，失败时按 UTF-8 JSON 解析。
pub fn decode_body(bytes: &[u8]) -> Option<Value> {
    if let Ok(decompressed) = zstd::stream::decode_all(bytes)
        && let Ok(value) = serde_json::from_slice(&decompressed)
    {
        return Some(value);
    }
    serde_json::from_slice(bytes).ok()
}

impl HttpClient for MockHttpClient {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
    ) -> BoxFuture<'a, Result<HttpResponse, HttpError>> {
        let result = (self.handler)(&request);
        self.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(request);
        Box::pin(async move {
            let response = result?;
            let body: ByteStream = Box::pin(futures::stream::iter(vec![Ok(response.body)]));
            Ok(HttpResponse {
                response: ProviderResponse {
                    status: response.status,
                    headers: response
                        .headers
                        .into_iter()
                        .collect::<indexmap::IndexMap<_, _>>(),
                },
                body,
            })
        })
    }
}

/// `normalizeContext({ messages, systemPrompt, tools })`。
pub fn context(system_prompt: Option<&str>, messages: Vec<Message>) -> TranscriptContext {
    normalize_context(&Context {
        system_prompt: system_prompt.map(str::to_string),
        messages,
        tools: None,
    })
}

/// 带工具声明的上下文。
pub fn context_with_tools(
    system_prompt: Option<&str>,
    messages: Vec<Message>,
    tools: Vec<kiana_ai::types::Tool>,
) -> TranscriptContext {
    normalize_context(&Context {
        system_prompt: system_prompt.map(str::to_string),
        messages,
        tools: Some(tools),
    })
}

/// 一个带最小必填字段的 chat 模型。
pub fn model(id: &str, provider: &str, base_url: &str) -> Model {
    Model {
        id: id.into(),
        name: id.into(),
        api: "anthropic-messages".into(),
        provider: provider.into(),
        base_url: base_url.into(),
        input: vec![kiana_ai::types::ModelInputModality::Text],
        cost: kiana_ai::types::ModelCost::default(),
        reasoning: Some(false),
        context_window: Some(100_000),
        max_tokens: Some(4096),
        ..Model::default()
    }
}

/// 逐条消费事件流直到结束，返回 `(最终消息, 事件类型序列)`。
pub async fn collect(stream: AssistantMessageEventStream) -> (AssistantMessage, Vec<String>) {
    let mut events = Vec::new();
    loop {
        let next = stream.recv().await;
        match next {
            Some(event) => {
                events.push(event.kind().to_string());
                if matches!(
                    event,
                    AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
                ) {
                    break;
                }
            }
            None => break,
        }
    }
    let message = stream.result().await.unwrap_or_default();
    (message, events)
}

/// 只取最终消息。
pub async fn run_to_result(stream: AssistantMessageEventStream) -> AssistantMessage {
    collect(stream).await.0
}

/// 一个最小的流式选项生成器。
pub fn stream_options(fetch: Arc<MockHttpClient>) -> StreamOptions {
    StreamOptions {
        api_key: Some("test-key".into()),
        fetch: Some(fetch),
        ..StreamOptions::default()
    }
}
