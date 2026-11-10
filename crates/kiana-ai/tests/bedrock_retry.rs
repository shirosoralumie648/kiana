//! 回归：SDK 的 StandardRetryStrategy（默认 maxAttempts=3）。
//!
//! 旧实现完全没有重试：一次 429/500 就把错误交给上层。真实 SDK 会对
//! THROTTLING（429 或错误码在列表里）与 TRANSIENT（500/502/503/504、超时类错误码、
//! 传输失败）重试，最多 3 次尝试；403 等 CLIENT_ERROR 不重试。

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kiana_ai::api::bedrock_converse_stream::{BedrockOptions, stream};
use kiana_ai::http::{BoxFuture, HttpClient, HttpError, HttpRequest, HttpResponse};
use kiana_ai::types::{
    Context, Message, Model, ModelInputModality, StopReason, StreamOptions, UserContent,
    UserMessage, UserRole,
};
use kiana_ai::utils::transcript::normalize_context;
use serde_json::Value;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn model(server: &MockServer) -> Model {
    Model {
        id: "us.anthropic.claude-opus-4-8".into(),
        name: "Claude Opus 4.8".into(),
        api: "bedrock-converse-stream".into(),
        provider: "amazon-bedrock".into(),
        base_url: server.uri(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        context_window: Some(200_000),
        max_tokens: Some(64_000),
        ..Model::default()
    }
}

fn context() -> kiana_ai::types::TranscriptContext {
    normalize_context(&Context {
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("hello".into()),
            timestamp: 1,
        })],
        ..Context::default()
    })
}

fn options(env: &[(&str, &str)]) -> BedrockOptions {
    let mut vars: kiana_ai::types::ProviderEnv = [("AWS_BEDROCK_SKIP_AUTH", "1")]
        .into_iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    for (name, value) in env {
        vars.insert(name.to_string(), value.to_string());
    }
    BedrockOptions {
        base: StreamOptions {
            cache_retention: Some(kiana_ai::types::CacheRetention::None),
            env: Some(vars),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn diagnostic_details(message: &kiana_ai::types::AssistantMessage) -> Value {
    message
        .diagnostics
        .as_ref()
        .and_then(|diagnostics| {
            diagnostics
                .iter()
                .find(|diagnostic| diagnostic.kind == "bedrock_response_failure")
        })
        .and_then(|diagnostic| diagnostic.details.clone())
        .map(|details| serde_json::to_value(details).unwrap_or(Value::Null))
        .unwrap_or(Value::Null)
}

async fn mount_status(server: &MockServer, status: u16, headers: &[(&str, &str)], body: &str) {
    let mut template = ResponseTemplate::new(status)
        .insert_header("content-type", "application/json")
        .set_body_string(body.to_string());
    for (name, value) in headers {
        template = template.insert_header(*name, *value);
    }
    Mock::given(method("POST"))
        .respond_with(template)
        .mount(server)
        .await;
}

#[tokio::test]
async fn throttling_429_is_retried_three_times() {
    let server = common::wiremock_util::start().await;
    mount_status(
        &server,
        429,
        &[
            ("x-amzn-errortype", "ThrottlingException"),
            ("x-amzn-requestid", "REQ-429"),
        ],
        "{\"message\":\"Too many requests, please wait.\"}",
    )
    .await;
    let message = stream(&model(&server), &context(), options(&[]))
        .result()
        .await
        .expect("result");
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("Throttling error: Too many requests, please wait.")
    );
    assert_eq!(
        diagnostic_details(&message),
        serde_json::json!({
            "status": 429,
            "errorCode": "ThrottlingException",
            "requestId": "REQ-429"
        })
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn transient_500_is_retried_three_times() {
    let server = common::wiremock_util::start().await;
    mount_status(
        &server,
        500,
        &[("x-amzn-errortype", "InternalServerException")],
        "{\"message\":\"internal blew up\"}",
    )
    .await;
    let message = stream(&model(&server), &context(), options(&[]))
        .result()
        .await
        .expect("result");
    assert_eq!(
        message.error_message.as_deref(),
        Some("Internal server error: internal blew up")
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn client_errors_are_not_retried() {
    let server = common::wiremock_util::start().await;
    mount_status(
        &server,
        403,
        &[],
        "{\"message\":\"blocked by gateway WAF\"}",
    )
    .await;
    let message = stream(&model(&server), &context(), options(&[]))
        .result()
        .await
        .expect("result");
    assert_eq!(
        message.error_message.as_deref(),
        Some("Unknown: blocked by gateway WAF")
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn aws_max_attempts_one_disables_retries() {
    let server = common::wiremock_util::start().await;
    mount_status(
        &server,
        429,
        &[("x-amzn-errortype", "ThrottlingException")],
        "{\"message\":\"too many\"}",
    )
    .await;
    let message = stream(
        &model(&server),
        &context(),
        options(&[("AWS_MAX_ATTEMPTS", "1")]),
    )
    .result()
    .await
    .expect("result");
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_retry_can_turn_a_throttle_into_a_successful_stream() {
    let server = common::wiremock_util::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_cb = Arc::clone(&attempts);
    Mock::given(method("POST"))
        .respond_with(move |_request: &wiremock::Request| {
            let attempt = attempts_cb.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                ResponseTemplate::new(429)
                    .insert_header("content-type", "application/json")
                    .insert_header("x-amzn-errortype", "ThrottlingException")
                    .insert_header("retry-after", "0")
                    .set_body_string("{\"message\":\"slow down\"}")
            } else {
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/vnd.amazon.eventstream")
                    .set_body_bytes(stream_stop_body())
            }
        })
        .mount(&server)
        .await;
    let message = stream(&model(&server), &context(), options(&[]))
        .result()
        .await
        .expect("result");
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

fn stream_stop_body() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&frame(
        &[
            (":message-type", "event"),
            (":event-type", "messageStart"),
            (":content-type", "application/json"),
        ],
        r#"{"role":"assistant"}"#,
    ));
    body.extend_from_slice(&frame(
        &[
            (":message-type", "event"),
            (":event-type", "messageStop"),
            (":content-type", "application/json"),
        ],
        r#"{"stopReason":"end_turn"}"#,
    ));
    body
}

fn frame(headers: &[(&str, &str)], payload: &str) -> Vec<u8> {
    let mut header_bytes = Vec::new();
    for (name, value) in headers {
        header_bytes.push(name.len() as u8);
        header_bytes.extend_from_slice(name.as_bytes());
        header_bytes.push(7);
        header_bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
        header_bytes.extend_from_slice(value.as_bytes());
    }
    let total = 12 + header_bytes.len() + payload.len() + 4;
    let mut message = Vec::with_capacity(total);
    message.extend_from_slice(&(total as u32).to_be_bytes());
    message.extend_from_slice(&(header_bytes.len() as u32).to_be_bytes());
    let prelude_crc = crc32fast::hash(&message);
    message.extend_from_slice(&prelude_crc.to_be_bytes());
    message.extend_from_slice(&header_bytes);
    message.extend_from_slice(payload.as_bytes());
    let message_crc = crc32fast::hash(&message);
    message.extend_from_slice(&message_crc.to_be_bytes());
    message
}

#[tokio::test]
async fn a_500_html_body_is_retried_as_transient_and_keeps_the_syntax_error_text() {
    let server = common::wiremock_util::start().await;
    mount_status(&server, 500, &[], "<html>oops</html>").await;
    let message = stream(&model(&server), &context(), options(&[]))
        .result()
        .await
        .expect("result");
    assert!(
        message
            .error_message
            .as_deref()
            .unwrap_or_default()
            .starts_with("Unexpected token '<', \"<html>oops</html>\" is not valid JSON")
    );
    assert_eq!(
        diagnostic_details(&message),
        serde_json::json!({"status": 500})
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn an_invalid_max_attempts_env_fails_before_sending_like_the_sdk() {
    let server = common::wiremock_util::start().await;
    let message = stream(
        &model(&server),
        &context(),
        options(&[("AWS_MAX_ATTEMPTS", "nope")]),
    )
    .result()
    .await
    .expect("result");
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("Environment variable AWS_MAX_ATTEMPTS mast be a number, got \"nope\"")
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
}

struct FlakyClient {
    inner: Arc<dyn HttpClient>,
    failures_left: AtomicUsize,
    attempts: AtomicUsize,
}

impl HttpClient for FlakyClient {
    fn execute<'a>(
        &'a self,
        request: HttpRequest,
        signal: Option<&'a tokio_util::sync::CancellationToken>,
    ) -> BoxFuture<'a, Result<HttpResponse, HttpError>> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        if self.failures_left.load(Ordering::SeqCst) > 0 {
            self.failures_left.fetch_sub(1, Ordering::SeqCst);
            return Box::pin(async { Err(HttpError::new("read ECONNRESET")) });
        }
        self.inner.execute(request, signal)
    }
}

#[tokio::test]
async fn transport_failures_are_retried_like_transient_errors() {
    let server = common::wiremock_util::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/vnd.amazon.eventstream")
                .set_body_bytes(stream_stop_body()),
        )
        .mount(&server)
        .await;
    let inner = kiana_ai::http::reqwest_client::ReqwestHttpClient::new()
        .expect("client")
        .shared();
    let flaky = Arc::new(FlakyClient {
        inner,
        failures_left: AtomicUsize::new(2),
        attempts: AtomicUsize::new(0),
    });
    let mut bedrock_options = options(&[]);
    bedrock_options.base.fetch = Some(flaky.clone());
    let message = stream(&model(&server), &context(), bedrock_options)
        .result()
        .await
        .expect("result");
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(flaky.attempts.load(Ordering::SeqCst), 3);
}
