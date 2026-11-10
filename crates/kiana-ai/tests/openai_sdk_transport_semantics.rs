//! OpenAI SDK 传输层语义的回归测试（`openai-completions` / `openai-responses` /
//! `azure-openai-responses` 共用 `api/openai_responses_shared.rs` 的传输层）。
//!
//! 这些用例没有直接的 TS 测试文件：它们固定的是 `openai` npm SDK 的可观察行为
//! （`client.js#fetchWithTimeout`/`makeRequest`、`azure.js#buildRequest`、
//! `core/streaming.js#_iterSSEMessages`、`internal/qs`），用 TS 参考实现探测后写成断言：
//!
//! - `timeoutMs` 只覆盖拿到响应对象（响应头）之前，定时器在 fetch resolve 后 `clearTimeout`；
//!   已开始的 SSE body 不会被它截断。未给出 `timeoutMs` 时用 SDK 默认的 600000ms。
//! - 头等待超时 → `APIConnectionTimeoutError.message = "Request timed out."`。
//! - fetch 拒绝（`TypeError: fetch failed`）→ `APIConnectionError.message = "Connection error."`。
//! - 响应没有 body（204/205、`new Response(null)`）→
//!   `"Attempted to iterate over a response with no body"`。
//! - `buildURL`：`new URL(baseURL + path)` + `qs.stringify` 重新序列化 query（RFC3986）。
//! - Azure SDK 在带 `api-key` 头时 `redirect: "manual"`：不跟随重定向。
//! - `if (sessionId)`：空字符串不发送会话亲和头。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use std::sync::Arc;
use std::time::Duration;

use kiana_ai::api::azure_openai_responses::AzureOpenAIResponsesOptions;
use kiana_ai::api::openai_completions::OpenAICompletionsOptions;
use kiana_ai::api::openai_responses::OpenAIResponsesOptions;
use kiana_ai::http::{
    BoxFuture, HttpClient, HttpError, HttpRequest, HttpResponse, SharedHttpClient,
};
use kiana_ai::types::{AssistantContentBlock, Model, ProviderResponse, StopReason, StreamOptions};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// ---------------------------------------------------------------------------
// 模型
// ---------------------------------------------------------------------------

fn completions_model(base_url: &str) -> Model {
    let mut model = mock_http::catalog_model("openai", "gpt-4o-mini");
    model.api = "openai-completions".into();
    model.base_url = base_url.to_string();
    model.compat = None;
    model
}

fn responses_model(base_url: &str) -> Model {
    let mut model = mock_http::catalog_model("openai", "gpt-4o-mini");
    model.api = "openai-responses".into();
    model.base_url = base_url.to_string();
    model
}

fn azure_model() -> Model {
    mock_http::catalog_model("azure", "gpt-4o-mini")
}

// ---------------------------------------------------------------------------
// 真实的 `ReqwestHttpClient`（默认实现）与最小 HTTP/1.1 流式服务器
// ---------------------------------------------------------------------------

/// 与生产默认 client 相同的装配，但 `no_proxy`（本地回环测试不经过环境代理）。
fn real_client() -> SharedHttpClient {
    let client = reqwest::Client::builder()
        .no_proxy()
        .no_gzip()
        .no_deflate()
        .no_brotli()
        .no_zstd()
        .build()
        .expect("reqwest client");
    kiana_ai::http::reqwest_client::ReqwestHttpClient::from_client(client).shared()
}

/// 最小 HTTP/1.1 服务器：立即回响应头，然后按 `delay` 逐块写 chunked body。
///
/// 返回 `http://127.0.0.1:<port>`。
async fn spawn_streaming_server(chunks: Vec<String>, delay: Duration) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let chunks = chunks.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0u8; 2048];
                loop {
                    match socket.read(&mut buffer).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => {
                            request.extend_from_slice(&buffer[..read]);
                            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                                break;
                            }
                        }
                    }
                }
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
                    )
                    .await;
                for chunk in chunks {
                    let payload = format!("{:x}\r\n{chunk}\r\n", chunk.len());
                    if socket.write_all(payload.as_bytes()).await.is_err() {
                        return;
                    }
                    let _ = socket.flush().await;
                    tokio::time::sleep(delay).await;
                }
                let _ = socket.write_all(b"0\r\n\r\n").await;
                let _ = socket.flush().await;
            });
        }
    });
    format!("http://{address}")
}

/// 只等响应头就会耗掉 `delay` 的 client（模拟慢服务器）。
struct DelayedHeadersClient {
    delay: Duration,
    response: mock_http::MockResponse,
}

impl HttpClient for DelayedHeadersClient {
    fn execute<'a>(
        &'a self,
        _request: HttpRequest,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
    ) -> BoxFuture<'a, Result<HttpResponse, HttpError>> {
        Box::pin(async move {
            tokio::time::sleep(self.delay).await;
            let body: kiana_ai::http::ByteStream =
                Box::pin(futures::stream::iter(vec![Ok(self.response.body.clone())]));
            Ok(HttpResponse {
                response: ProviderResponse {
                    status: self.response.status,
                    headers: self.response.headers.clone(),
                },
                body,
            })
        })
    }
}

/// fetch 阶段失败（Node 的传输层文案）。
struct FailingClient;

impl HttpClient for FailingClient {
    fn execute<'a>(
        &'a self,
        _request: HttpRequest,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
    ) -> BoxFuture<'a, Result<HttpResponse, HttpError>> {
        Box::pin(async move { Err(HttpError::new("fetch failed")) })
    }
}

/// 永远拿不到响应头（模拟挂起的服务器）。
struct HangingClient;

impl HttpClient for HangingClient {
    fn execute<'a>(
        &'a self,
        _request: HttpRequest,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
    ) -> BoxFuture<'a, Result<HttpResponse, HttpError>> {
        Box::pin(std::future::pending())
    }
}

fn stop_completions_sse() -> mock_http::MockResponse {
    mock_http::MockResponse::sse(&[json!({
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
    })])
}

fn completed_responses_sse() -> mock_http::MockResponse {
    mock_http::MockResponse::sse(&[json!({
        "type": "response.completed",
        "sequence_number": 0,
        "response": { "id": "resp_test", "status": "completed" },
    })])
}

fn text_of(message: &kiana_ai::types::AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// timeoutMs 只覆盖「等响应头」
// ---------------------------------------------------------------------------

#[tokio::test]
async fn completions_keeps_streaming_after_timeout_ms_elapsed() {
    // 每个 chunk 间隔 120ms、共 5 个 chunk（~600ms），timeoutMs=300；TS 里 body 不受
    // timeoutMs 约束（定时器在响应头到手时就清掉了）。
    let base_url = spawn_streaming_server(
        vec![
            "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"}}]}\n\n"
                .into(),
            "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"b\"}}]}\n\n"
                .into(),
            "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n"
                .into(),
            ":keep-alive\n\n".into(),
            "data: [DONE]\n\n".into(),
        ],
        Duration::from_millis(120),
    )
    .await;
    let message = mock_http::run_completions(
        &completions_model(&base_url),
        &mock_http::text_context("hi"),
        OpenAICompletionsOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(real_client()),
                timeout_ms: Some(300),
                ..StreamOptions::default()
            },
            ..OpenAICompletionsOptions::default()
        },
    )
    .await;
    assert_eq!(
        message.stop_reason,
        StopReason::Stop,
        "{:?}",
        message.error_message
    );
    assert_eq!(text_of(&message), "ab");
    assert!(message.error_message.is_none());
}

#[tokio::test]
async fn responses_keeps_streaming_after_timeout_ms_elapsed() {
    let base_url = spawn_streaming_server(
        vec![
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"delta\":\"a\"}\n\n"
                .into(),
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"delta\":\"b\"}\n\n"
                .into(),
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_1\",\"status\":\"completed\"}}\n\n"
                .into(),
        ],
        Duration::from_millis(150),
    )
    .await;
    let message = mock_http::run_responses(
        &responses_model(&base_url),
        &mock_http::text_context("hi"),
        OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(real_client()),
                timeout_ms: Some(250),
                ..StreamOptions::default()
            },
            ..OpenAIResponsesOptions::default()
        },
    )
    .await;
    assert_eq!(
        message.stop_reason,
        StopReason::Stop,
        "{:?}",
        message.error_message
    );
}

#[tokio::test]
async fn header_timeout_reports_the_sdk_timeout_message() {
    let delayed = Arc::new(DelayedHeadersClient {
        delay: Duration::from_millis(400),
        response: stop_completions_sse(),
    });
    let message = mock_http::run_completions(
        &completions_model("https://api.openai.com/v1"),
        &mock_http::text_context("hi"),
        OpenAICompletionsOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(delayed.clone()),
                timeout_ms: Some(100),
                ..StreamOptions::default()
            },
            ..OpenAICompletionsOptions::default()
        },
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.error_message.as_deref(), Some("Request timed out."));

    let delayed = Arc::new(DelayedHeadersClient {
        delay: Duration::from_millis(400),
        response: completed_responses_sse(),
    });
    let message = mock_http::run_responses(
        &responses_model("https://api.openai.com/v1"),
        &mock_http::text_context("hi"),
        OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(delayed),
                timeout_ms: Some(100),
                ..StreamOptions::default()
            },
            ..OpenAIResponsesOptions::default()
        },
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.error_message.as_deref(), Some("Request timed out."));
}

#[tokio::test(start_paused = true)]
async fn omitted_timeout_uses_the_sdk_default_of_ten_minutes() {
    // SDK 的 `OpenAI.DEFAULT_TIMEOUT = 600000`：未显式给出 timeoutMs 时也约束等响应头。
    let message = mock_http::run_completions(
        &completions_model("https://api.openai.com/v1"),
        &mock_http::text_context("hi"),
        OpenAICompletionsOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(Arc::new(HangingClient)),
                ..StreamOptions::default()
            },
            ..OpenAICompletionsOptions::default()
        },
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.error_message.as_deref(), Some("Request timed out."));
}

#[tokio::test]
async fn fetch_failure_reports_connection_error() {
    let message = mock_http::run_completions(
        &completions_model("https://api.openai.com/v1"),
        &mock_http::text_context("hi"),
        OpenAICompletionsOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(Arc::new(FailingClient)),
                ..StreamOptions::default()
            },
            ..OpenAICompletionsOptions::default()
        },
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.error_message.as_deref(), Some("Connection error."));

    let message = mock_http::run_responses(
        &responses_model("https://api.openai.com/v1"),
        &mock_http::text_context("hi"),
        OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(Arc::new(FailingClient)),
                ..StreamOptions::default()
            },
            ..OpenAIResponsesOptions::default()
        },
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.error_message.as_deref(), Some("Connection error."));
}

// ---------------------------------------------------------------------------
// 无响应体
// ---------------------------------------------------------------------------

#[tokio::test]
async fn response_without_body_reports_the_sdk_error() {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::empty(204));
    let message = mock_http::run_completions(
        &completions_model("https://api.openai.com/v1"),
        &mock_http::text_context("hi"),
        OpenAICompletionsOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(Arc::new(mock.clone())),
                ..StreamOptions::default()
            },
            ..OpenAICompletionsOptions::default()
        },
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("Attempted to iterate over a response with no body")
    );

    let mock = mock_http::MockHttp::single(mock_http::MockResponse::empty(205));
    let message = mock_http::run_responses(
        &responses_model("https://api.openai.com/v1"),
        &mock_http::text_context("hi"),
        OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(Arc::new(mock.clone())),
                ..StreamOptions::default()
            },
            ..OpenAIResponsesOptions::default()
        },
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("Attempted to iterate over a response with no body")
    );

    let mock = mock_http::MockHttp::single(mock_http::MockResponse::empty(204));
    let message = mock_http::run_azure(
        &azure_model(),
        &mock_http::text_context("hi"),
        AzureOpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(Arc::new(mock)),
                ..StreamOptions::default()
            },
            azure_base_url: Some("https://my-resource.openai.azure.com".into()),
            ..AzureOpenAIResponsesOptions::default()
        },
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("Attempted to iterate over a response with no body")
    );
}

// ---------------------------------------------------------------------------
// WHATWG URL 构建
// ---------------------------------------------------------------------------

#[tokio::test]
async fn base_url_query_is_rebuilt_like_the_sdk() {
    let mock =
        mock_http::MockHttp::single(mock_http::MockResponse::text(500, "application/json", ""));
    let _ = mock_http::run_completions(
        &completions_model("https://proxy.example/v1?x=1"),
        &mock_http::text_context("hi"),
        OpenAICompletionsOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(Arc::new(mock.clone())),
                ..StreamOptions::default()
            },
            ..OpenAICompletionsOptions::default()
        },
    )
    .await;
    assert_eq!(
        mock.last_request().url,
        "https://proxy.example/v1?x=1%2Fchat%2Fcompletions"
    );

    let mock =
        mock_http::MockHttp::single(mock_http::MockResponse::text(500, "application/json", ""));
    let _ = mock_http::run_responses(
        &responses_model("https://proxy.example/v1?x=1"),
        &mock_http::text_context("hi"),
        OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(Arc::new(mock.clone())),
                ..StreamOptions::default()
            },
            ..OpenAIResponsesOptions::default()
        },
    )
    .await;
    assert_eq!(
        mock.last_request().url,
        "https://proxy.example/v1?x=1%2Fresponses"
    );
}

#[tokio::test]
async fn azure_query_merges_default_api_version() {
    let mock =
        mock_http::MockHttp::single(mock_http::MockResponse::text(500, "application/json", ""));
    let _ = mock_http::run_azure(
        &azure_model(),
        &mock_http::text_context("hi"),
        AzureOpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(Arc::new(mock.clone())),
                ..StreamOptions::default()
            },
            azure_base_url: Some("https://my-proxy.example/v1?custom=true".into()),
            ..AzureOpenAIResponsesOptions::default()
        },
    )
    .await;
    assert_eq!(
        mock.last_request().url,
        "https://my-proxy.example/v1?custom=true%2Fresponses&api-version=v1"
    );

    // 已有 api-version 会被 defaultQuery 覆盖（不是追加第二个同名参数）。
    let mock =
        mock_http::MockHttp::single(mock_http::MockResponse::text(500, "application/json", ""));
    let _ = mock_http::run_azure(
        &azure_model(),
        &mock_http::text_context("hi"),
        AzureOpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(Arc::new(mock.clone())),
                ..StreamOptions::default()
            },
            azure_base_url: Some("https://my-proxy.example/v1?api-version=old".into()),
            ..AzureOpenAIResponsesOptions::default()
        },
    )
    .await;
    assert_eq!(
        mock.last_request().url,
        "https://my-proxy.example/v1?api-version=v1"
    );
}

// ---------------------------------------------------------------------------
// Azure 不跟随重定向
// ---------------------------------------------------------------------------

#[tokio::test]
async fn azure_does_not_follow_redirects_and_keeps_the_api_key_local() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // 默认 client 会读取环境代理；本地回环必须直连。
    let previous_no_proxy = std::env::var("NO_PROXY").ok();
    let merged_no_proxy = match &previous_no_proxy {
        Some(value) if !value.is_empty() => format!("{value},127.0.0.1,localhost"),
        _ => "127.0.0.1,localhost".to_string(),
    };
    unsafe { std::env::set_var("NO_PROXY", &merged_no_proxy) };

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/openai/v1/responses"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("location", format!("{}/elsewhere", server.uri())),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/elsewhere"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r\",\"status\":\"completed\"}}\n\n",
        ))
        .mount(&server)
        .await;

    let message = kiana_ai::api::azure_openai_responses::stream(
        &azure_model(),
        &mock_http::text_context("hi"),
        AzureOpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("secret-key".into()),
                ..StreamOptions::default()
            },
            azure_base_url: Some(format!("{}/openai/v1", server.uri())),
            ..AzureOpenAIResponsesOptions::default()
        },
    )
    .result()
    .await
    .expect("stream result");

    match previous_no_proxy {
        Some(value) => unsafe { std::env::set_var("NO_PROXY", value) },
        None => unsafe { std::env::remove_var("NO_PROXY") },
    }

    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("Azure OpenAI API error (307): 307 status code (no body)")
    );
    let requests = server.received_requests().await.expect("requests");
    assert!(
        requests
            .iter()
            .all(|request| request.url.path() != "/elsewhere"),
        "redirect target must not be requested: {:?}",
        requests
            .iter()
            .map(|request| request.url.path())
            .collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// 空 sessionId 不发送会话亲和头
// ---------------------------------------------------------------------------

#[tokio::test]
async fn empty_session_id_sends_no_affinity_headers() {
    // completions 只在 OpenRouter 上发送会话亲和头（`sendSessionAffinityHeaders: isOpenRouter`）。
    let mut openrouter = mock_http::catalog_model("openrouter", "openrouter/auto");
    openrouter.api = "openai-completions".into();
    openrouter.compat = None;
    openrouter.base_url = "https://openrouter.ai/api/v1".into();

    let mock = mock_http::MockHttp::single(stop_completions_sse());
    let _ = mock_http::run_completions(
        &openrouter,
        &mock_http::text_context("hi"),
        OpenAICompletionsOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(Arc::new(mock.clone())),
                session_id: Some("session-1".into()),
                ..StreamOptions::default()
            },
            ..OpenAICompletionsOptions::default()
        },
    )
    .await;
    assert_eq!(
        mock.last_header("x-session-id").as_deref(),
        Some("session-1")
    );

    let mock = mock_http::MockHttp::single(stop_completions_sse());
    let _ = mock_http::run_completions(
        &openrouter,
        &mock_http::text_context("hi"),
        OpenAICompletionsOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(Arc::new(mock.clone())),
                session_id: Some(String::new()),
                ..StreamOptions::default()
            },
            ..OpenAICompletionsOptions::default()
        },
    )
    .await;
    for name in [
        "session_id",
        "x-client-request-id",
        "x-session-affinity",
        "x-session-id",
    ] {
        assert!(mock.last_header(name).is_none(), "{name}");
    }

    // responses 对任何非空 sessionId 都发（`if (sessionId)`），空字符串不发。
    let mock = mock_http::MockHttp::single(completed_responses_sse());
    let _ = mock_http::run_responses(
        &responses_model("https://api.openai.com/v1"),
        &mock_http::text_context("hi"),
        OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(Arc::new(mock.clone())),
                session_id: Some("session-1".into()),
                ..StreamOptions::default()
            },
            ..OpenAIResponsesOptions::default()
        },
    )
    .await;
    assert_eq!(
        mock.last_header("x-client-request-id").as_deref(),
        Some("session-1")
    );

    let mock = mock_http::MockHttp::single(completed_responses_sse());
    let _ = mock_http::run_responses(
        &responses_model("https://api.openai.com/v1"),
        &mock_http::text_context("hi"),
        OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(Arc::new(mock.clone())),
                session_id: Some(String::new()),
                ..StreamOptions::default()
            },
            ..OpenAIResponsesOptions::default()
        },
    )
    .await;
    for name in ["session_id", "x-client-request-id", "x-session-affinity"] {
        assert!(mock.last_header(name).is_none(), "{name}");
    }
}
