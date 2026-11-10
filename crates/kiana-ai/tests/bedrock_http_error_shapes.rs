//! 回归：非 2xx 的 SDK 错误形态（`AwsRestJsonProtocol#deserializeResponse` →
//! `handleError` → `decorateServiceException`）。
//!
//! 真实 SDK 在这些路径上与旧实现（自己猜 message、把原始 body 塞给 Unknown）不同：
//! - JSON.parse 失败 → V8 `SyntaxError` + deserialization hint + `$metadata`；
//! - body 没有错误码 → 合成 `Unknown`，message 取 `message`/`Message`，否则 `"UnknownError"`；
//! - `$response.body` 是流：status/body 折不进 errorMessage，只有 JSON 里的
//!   `body`/`error` 字段会被 `decorateServiceException` 复制出来。

mod common;

use kiana_ai::api::bedrock_converse_stream::{BedrockOptions, stream};
use kiana_ai::types::{
    Context, Message, Model, ModelInputModality, StopReason, StreamOptions, UserContent,
    UserMessage, UserRole,
};
use kiana_ai::utils::transcript::normalize_context;
use serde_json::Value;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const HINT: &str = "\n  Deserialization error: to see the raw response, inspect the hidden field {error}.$response on this object.";

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

fn options() -> BedrockOptions {
    BedrockOptions {
        base: StreamOptions {
            cache_retention: Some(kiana_ai::types::CacheRetention::None),
            env: Some(
                [("AWS_BEDROCK_SKIP_AUTH".to_string(), "1".to_string())]
                    .into_iter()
                    .collect(),
            ),
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

async fn run_http(
    status: u16,
    headers: &[(&str, &str)],
    body: &str,
) -> (kiana_ai::types::AssistantMessage, MockServer) {
    let server = common::wiremock_util::start().await;
    let mut template = ResponseTemplate::new(status)
        .insert_header("content-type", "application/json")
        .set_body_string(body.to_string());
    for (name, value) in headers {
        template = template.insert_header(*name, *value);
    }
    Mock::given(method("POST"))
        .respond_with(template)
        .mount(&server)
        .await;
    let message = stream(&model(&server), &context(), options())
        .result()
        .await
        .expect("result");
    (message, server)
}

#[tokio::test]
async fn html_body_becomes_a_syntax_error_with_the_deserialization_hint() {
    let (message, _server) = run_http(403, &[], "<html>blocked by gateway WAF</html>").await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some(format!("Unexpected token '<', \"<html>bloc\"... is not valid JSON{HINT}").as_str())
    );
    assert_eq!(
        diagnostic_details(&message),
        serde_json::json!({"status": 403})
    );
}

#[tokio::test]
async fn empty_body_maps_to_the_synthetic_unknown_exception() {
    let (message, _server) = run_http(403, &[], "").await;
    assert_eq!(
        message.error_message.as_deref(),
        Some("Unknown: UnknownError")
    );
    assert_eq!(
        diagnostic_details(&message),
        serde_json::json!({"status": 403})
    );
}

#[tokio::test]
async fn json_null_maps_to_the_synthetic_unknown_exception() {
    let (message, _server) = run_http(403, &[], "null").await;
    assert_eq!(
        message.error_message.as_deref(),
        Some("Unknown: UnknownError")
    );
}

#[tokio::test]
async fn json_string_maps_to_the_synthetic_unknown_exception() {
    let (message, _server) = run_http(403, &[], "\"hi\"").await;
    assert_eq!(
        message.error_message.as_deref(),
        Some("Unknown: UnknownError")
    );
}

#[tokio::test]
async fn empty_message_falls_back_to_unknown_error() {
    let (message, _server) = run_http(403, &[], "{\"message\":\"\"}").await;
    assert_eq!(
        message.error_message.as_deref(),
        Some("Unknown: UnknownError")
    );
}

#[tokio::test]
async fn a_body_field_is_copied_onto_the_exception_and_surfaced_with_the_status() {
    let (message, _server) =
        run_http(403, &[], "{\"message\":\"outer\",\"body\":\"raw reason\"}").await;
    assert_eq!(
        message.error_message.as_deref(),
        Some("Unknown: 403: raw reason")
    );
}

#[tokio::test]
async fn an_error_object_is_stringified_and_surfaced_with_the_status() {
    let (message, _server) =
        run_http(403, &[], "{\"message\":\"outer\",\"error\":{\"code\":7}}").await;
    assert_eq!(
        message.error_message.as_deref(),
        Some("Unknown: 403: {\"code\":7}")
    );
}

#[tokio::test]
async fn the_errortype_header_is_sanitized_and_picks_the_human_prefix() {
    let (message, _server) = run_http(
        400,
        &[(
            "x-amzn-errortype",
            "ValidationException:http://internal.amazon.com/coral/com.amazon.bedrock/",
        )],
        "{\"message\":\"The provided model identifier is invalid.\"}",
    )
    .await;
    assert_eq!(
        message.error_message.as_deref(),
        Some("Validation error: The provided model identifier is invalid.")
    );
    assert_eq!(
        diagnostic_details(&message),
        serde_json::json!({"status": 400, "errorCode": "ValidationException"})
    );
}

#[tokio::test]
async fn the_body_type_field_is_sanitized_into_the_error_code() {
    let (message, _server) = run_http(
        400,
        &[],
        "{\"__type\":\"com.amazon.bedrock#ValidationException\",\"message\":\"bad\"}",
    )
    .await;
    assert_eq!(
        message.error_message.as_deref(),
        Some("Validation error: bad")
    );
    assert_eq!(
        diagnostic_details(&message),
        serde_json::json!({"status": 400, "errorCode": "ValidationException"})
    );
}

#[tokio::test]
async fn the_request_id_header_lands_in_the_diagnostic_only() {
    let (message, _server) = run_http(
        403,
        &[("x-amzn-requestid", "REQ-403")],
        "{\"message\":\"blocked by gateway WAF\"}",
    )
    .await;
    assert_eq!(
        message.error_message.as_deref(),
        Some("Unknown: blocked by gateway WAF")
    );
    assert_eq!(
        diagnostic_details(&message),
        serde_json::json!({"status": 403, "requestId": "REQ-403"})
    );
}
