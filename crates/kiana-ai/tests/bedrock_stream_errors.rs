//! 回归：event-stream 错误在“首帧预读”与“流中途”两条路径上的差异。
//!
//! 真实 SDK 在 `client.send()` 内部预读第一帧：
//! - 首帧失败时 `onResponse` 未回调、`responseRequestId` 未赋值；
//! - 普通 Error（error 帧 / 未知异常类型 / JSON 语法错误）由
//!   `schemaDeserializationMiddleware` 补 `$metadata {httpStatusCode: 200}` 与 hint；
//! - modeled 服务异常自带 `$metadata` 属性（值 undefined），因此不补 hint；
//! - 流中途的失败由 bedrock 自己的 for-await 抛出：不补 hint、不补 `$metadata`，
//!   诊断里的 requestId 来自 onResponse 记录的 fallback。

mod common;

use std::sync::{Arc, Mutex};

use kiana_ai::api::bedrock_converse_stream::{BedrockOptions, stream};
use kiana_ai::types::{
    Context, Message, Model, ModelInputModality, ProviderResponse, StopReason, StreamOptions,
    UserContent, UserMessage, UserRole,
};
use kiana_ai::utils::transcript::normalize_context;
use serde_json::{Value, json};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const HINT: &str = "\n  Deserialization error: to see the raw response, inspect the hidden field {error}.$response on this object.";

fn string_header(name: &str, value: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.push(name.len() as u8);
    bytes.extend_from_slice(name.as_bytes());
    bytes.push(7); // string
    bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
    bytes.extend_from_slice(value.as_bytes());
    bytes
}

fn frame(headers: &[(&str, &str)], payload: &str) -> Vec<u8> {
    let mut header_bytes = Vec::new();
    for (name, value) in headers {
        header_bytes.extend_from_slice(&string_header(name, value));
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

fn event(event_type: &str, payload: &str) -> Vec<u8> {
    frame(
        &[
            (":message-type", "event"),
            (":event-type", event_type),
            (":content-type", "application/json"),
        ],
        payload,
    )
}

fn exception(exception_type: &str, payload: &str) -> Vec<u8> {
    frame(
        &[
            (":message-type", "exception"),
            (":exception-type", exception_type),
        ],
        payload,
    )
}

fn error_frame(code: &str, message: &str) -> Vec<u8> {
    frame(
        &[
            (":message-type", "error"),
            (":error-code", code),
            (":error-message", message),
        ],
        "{}",
    )
}

fn message_start() -> Vec<u8> {
    event("messageStart", r#"{"role":"assistant"}"#)
}

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

fn options(
    on_response: Option<kiana_ai::types::OnResponseFn>,
    on_event: Option<kiana_ai::types::OnProviderStreamEventFn>,
) -> BedrockOptions {
    BedrockOptions {
        base: StreamOptions {
            cache_retention: Some(kiana_ai::types::CacheRetention::None),
            env: Some(
                [("AWS_BEDROCK_SKIP_AUTH".to_string(), "1".to_string())]
                    .into_iter()
                    .collect(),
            ),
            on_response,
            on_provider_stream_event: on_event,
            ..Default::default()
        },
        ..Default::default()
    }
}

async fn run_stream_body(
    frames: Vec<Vec<u8>>,
    request_id: Option<&str>,
    on_response: Option<kiana_ai::types::OnResponseFn>,
    on_event: Option<kiana_ai::types::OnProviderStreamEventFn>,
) -> (kiana_ai::types::AssistantMessage, MockServer) {
    let server = common::wiremock_util::start().await;
    let mut body = Vec::new();
    for frame in frames {
        body.extend_from_slice(&frame);
    }
    let mut template = ResponseTemplate::new(200)
        .insert_header("content-type", "application/vnd.amazon.eventstream")
        .set_body_bytes(body);
    if let Some(request_id) = request_id {
        template = template.insert_header("x-amzn-requestid", request_id);
    }
    Mock::given(method("POST"))
        .respond_with(template)
        .mount(&server)
        .await;
    let message = stream(&model(&server), &context(), options(on_response, on_event))
        .result()
        .await
        .expect("result");
    (message, server)
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

#[tokio::test]
async fn first_frame_error_gets_the_hint_and_does_not_call_on_response() {
    let responses = Arc::new(Mutex::new(Vec::<ProviderResponse>::new()));
    let responses_cb = Arc::clone(&responses);
    let (message, _server) = run_stream_body(
        vec![error_frame("internalServerException", "boom")],
        Some("REQ-OK"),
        Some(Arc::new(
            move |response: ProviderResponse, _model: &Model| {
                responses_cb.lock().unwrap().push(response);
            },
        )),
        None,
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some(format!("boom{HINT}").as_str())
    );
    assert_eq!(
        diagnostic_details(&message),
        json!({"status": 200, "errorCode": "internalServerException", "requestId": "REQ-OK"})
    );
    assert!(responses.lock().unwrap().is_empty());
}

#[tokio::test]
async fn first_frame_modeled_exception_has_no_hint_and_no_metadata() {
    let (message, _server) = run_stream_body(
        vec![exception(
            "serviceUnavailableException",
            r#"{"message":"try later"}"#,
        )],
        Some("REQ-OK"),
        None,
        None,
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("Service unavailable: try later")
    );
    assert_eq!(
        diagnostic_details(&message),
        json!({"errorCode": "ServiceUnavailableException"})
    );
}

#[tokio::test]
async fn first_frame_unknown_exception_is_a_plain_error_with_the_raw_payload() {
    let (message, _server) = run_stream_body(
        vec![exception("someWeirdException", r#"{"message":"weird"}"#)],
        Some("REQ-OK"),
        None,
        None,
    )
    .await;
    assert_eq!(
        message.error_message.as_deref(),
        Some(format!("{{\"message\":\"weird\"}}{HINT}").as_str())
    );
    assert_eq!(
        diagnostic_details(&message),
        json!({"status": 200, "errorCode": "someWeirdException", "requestId": "REQ-OK"})
    );
}

#[tokio::test]
async fn midstream_modeled_exception_keeps_only_the_fallback_request_id() {
    let (message, _server) = run_stream_body(
        vec![
            message_start(),
            exception(
                "throttlingException",
                r#"{"message":"Too many requests, please wait."}"#,
            ),
        ],
        Some("REQ-OK"),
        None,
        None,
    )
    .await;
    assert_eq!(
        message.error_message.as_deref(),
        Some("Throttling error: Too many requests, please wait.")
    );
    assert_eq!(
        diagnostic_details(&message),
        json!({"errorCode": "ThrottlingException", "requestId": "REQ-OK"})
    );
}

#[tokio::test]
async fn midstream_error_frame_has_no_hint_and_keeps_the_fallback_request_id() {
    let (message, _server) = run_stream_body(
        vec![
            message_start(),
            error_frame("internalServerException", "boom"),
        ],
        Some("REQ-OK"),
        None,
        None,
    )
    .await;
    assert_eq!(message.error_message.as_deref(), Some("boom"));
    assert_eq!(
        diagnostic_details(&message),
        json!({"errorCode": "internalServerException", "requestId": "REQ-OK"})
    );
}

#[tokio::test]
async fn midstream_invalid_exception_json_has_no_hint() {
    let (message, _server) = run_stream_body(
        vec![message_start(), exception("throttlingException", "{oops")],
        Some("REQ-OK"),
        None,
        None,
    )
    .await;
    assert_eq!(
        message.error_message.as_deref(),
        Some("Expected property name or '}' in JSON at position 1 (line 1 column 2)")
    );
    assert_eq!(diagnostic_details(&message), json!({"requestId": "REQ-OK"}));
}

#[tokio::test]
async fn unknown_event_types_are_skipped_without_reaching_the_callback() {
    let events = Arc::new(Mutex::new(Vec::<Value>::new()));
    let events_cb = Arc::clone(&events);
    let (message, _server) = run_stream_body(
        vec![
            message_start(),
            event("someFutureEvent", r#"{"foo":"bar"}"#),
            event("messageStop", r#"{"stopReason":"end_turn"}"#),
        ],
        Some("REQ-OK"),
        None,
        Some(Arc::new(move |item, _model| {
            events_cb.lock().unwrap().push(item);
        })),
    )
    .await;
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            json!({"messageStart": {"role": "assistant"}}),
            json!({"messageStop": {"stopReason": "end_turn"}}),
        ]
    );
}
