//! `test/bedrock-error-metadata.test.ts` 的移植。

use serde_json::{Value, json};

use kiana_ai::api::bedrock_converse_stream::{
    BedrockErrorMetadata, BedrockFailure, BedrockOptions, stream_with_items,
};
use kiana_ai::types::{
    Context, Message, Model, ModelInputModality, StreamOptions, UserContent, UserMessage, UserRole,
};

const DIAGNOSTIC_TYPE: &str = "bedrock_response_failure";
const VALIDATION_MESSAGE: &str = "The provided model identifier is invalid.";
const REQUEST_ID: &str = "11111111-2222-3333-4444-555555555555";

fn model() -> Model {
    Model {
        id: "us.anthropic.claude-opus-4-8".into(),
        name: "Claude Opus 4.8".into(),
        api: "bedrock-converse-stream".into(),
        provider: "amazon-bedrock".into(),
        base_url: "https://bedrock-runtime.us-east-1.amazonaws.com".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        context_window: Some(200_000),
        max_tokens: Some(64_000),
        ..Model::default()
    }
}

fn context() -> kiana_ai::types::TranscriptContext {
    kiana_ai::utils::transcript::normalize_context(&Context {
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("hello".into()),
            timestamp: 1,
        })],
        ..Context::default()
    })
}

fn service_exception(
    name: &str,
    message: &str,
    status: f64,
    request_id: Option<&str>,
) -> BedrockFailure {
    BedrockFailure::Service {
        name: name.into(),
        message: message.into(),
        metadata: BedrockErrorMetadata {
            http_status_code: Some(status),
            request_id: request_id.map(str::to_string),
        },
        body: None,
    }
}

fn failing_after_message_start(failure: BedrockFailure) -> Vec<Result<Value, BedrockFailure>> {
    vec![
        Ok(json!({"messageStart": {"role": "assistant"}})),
        Err(failure),
    ]
}

async fn run(
    items: Vec<Result<Value, BedrockFailure>>,
    request_id: Option<&str>,
    signal: Option<tokio_util::sync::CancellationToken>,
) -> kiana_ai::types::AssistantMessage {
    stream_with_items(
        &model(),
        &context(),
        BedrockOptions {
            base: StreamOptions {
                cache_retention: Some(kiana_ai::types::CacheRetention::None),
                signal,
                ..Default::default()
            },
            ..Default::default()
        },
        request_id.map(str::to_string),
        items,
    )
    .result()
    .await
    .expect("result")
}

fn diagnostic(
    message: &kiana_ai::types::AssistantMessage,
) -> Option<&kiana_ai::types::AssistantMessageDiagnostic> {
    message
        .diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.iter().find(|d| d.kind == DIAGNOSTIC_TYPE))
}

fn details(diagnostic: &kiana_ai::types::AssistantMessageDiagnostic) -> Value {
    serde_json::to_value(diagnostic.details.clone().unwrap_or_default()).expect("details")
}

#[tokio::test]
async fn records_status_error_code_and_request_id_for_a_non_2xx() {
    let message = run(
        vec![Err(service_exception(
            "ValidationException",
            VALIDATION_MESSAGE,
            400.0,
            Some(REQUEST_ID),
        ))],
        Some(REQUEST_ID),
        None,
    )
    .await;
    let diagnostic = diagnostic(&message).expect("diagnostic");
    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Error);
    assert_eq!(
        details(diagnostic),
        json!({"status": 400, "errorCode": "ValidationException", "requestId": REQUEST_ID})
    );
    assert!(diagnostic.error.is_none());
    let serialized = serde_json::to_value(diagnostic).unwrap();
    let mut keys: Vec<&str> = serialized
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(keys, vec!["details", "timestamp", "type"]);
}

#[tokio::test]
async fn leaves_error_message_untouched_so_retry_classification_is_unaffected() {
    let message = run(
        vec![Err(service_exception(
            "ValidationException",
            VALIDATION_MESSAGE,
            400.0,
            Some(REQUEST_ID),
        ))],
        Some(REQUEST_ID),
        None,
    )
    .await;
    assert_eq!(
        message.error_message.as_deref(),
        Some("Validation error: The provided model identifier is invalid.")
    );
}

#[tokio::test]
async fn reports_only_the_request_id_for_a_modeled_mid_stream_exception() {
    let message = run(
        failing_after_message_start(BedrockFailure::Value {
            message: "{\"message\":\"Too many requests, please wait.\"}".into(),
        }),
        Some(REQUEST_ID),
        None,
    )
    .await;
    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Error);
    // 裸对象走 `safeJsonStringify`：errorMessage 是整个 payload 的 JSON，而不是 `message` 字段。
    assert_eq!(
        message.error_message.as_deref(),
        Some("{\"message\":\"Too many requests, please wait.\"}")
    );
    assert_eq!(
        details(diagnostic(&message).expect("diagnostic")),
        json!({"requestId": REQUEST_ID})
    );
}

#[tokio::test]
async fn captures_the_error_code_for_an_unmodeled_mid_stream_error() {
    let message = run(
        failing_after_message_start(BedrockFailure::named(
            "ModelStreamErrorException",
            "Model stream terminated unexpectedly.",
        )),
        Some(REQUEST_ID),
        None,
    )
    .await;
    assert_eq!(
        details(diagnostic(&message).expect("diagnostic")),
        json!({"errorCode": "ModelStreamErrorException", "requestId": REQUEST_ID})
    );
}

#[tokio::test]
async fn does_not_report_a_transport_failure_name_as_a_provider_error_code() {
    let message = run(
        failing_after_message_start(BedrockFailure::named(
            "TimeoutError",
            "Connection timed out after 1000 ms",
        )),
        Some(REQUEST_ID),
        None,
    )
    .await;
    assert_eq!(
        details(diagnostic(&message).expect("diagnostic")),
        json!({"requestId": REQUEST_ID})
    );
}

#[tokio::test]
async fn emits_no_diagnostic_when_the_failure_carries_no_provider_metadata() {
    let message = run(
        vec![Err(BedrockFailure::named("Error", "socket hang up"))],
        None,
        None,
    )
    .await;
    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Error);
    assert_eq!(message.error_message.as_deref(), Some("socket hang up"));
    assert!(diagnostic(&message).is_none());
}

#[tokio::test]
async fn emits_no_diagnostic_for_an_aborted_turn() {
    let signal = tokio_util::sync::CancellationToken::new();
    signal.cancel();
    let message = run(
        vec![Err(service_exception(
            "ValidationException",
            VALIDATION_MESSAGE,
            400.0,
            Some(REQUEST_ID),
        ))],
        Some(REQUEST_ID),
        Some(signal),
    )
    .await;
    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Aborted);
    assert!(diagnostic(&message).is_none());
}

#[tokio::test]
async fn drops_header_derived_values_that_exceed_the_length_bound() {
    let long_name = format!("{}Exception", "E".repeat(5000));
    let long_request_id = "R".repeat(5000);
    let message = run(
        vec![Err(service_exception(
            &long_name,
            VALIDATION_MESSAGE,
            400.0,
            Some(&long_request_id),
        ))],
        None,
        None,
    )
    .await;
    assert_eq!(
        details(diagnostic(&message).expect("diagnostic")),
        json!({"status": 400})
    );
}

#[tokio::test]
async fn omits_the_sdk_unknown_placeholder_instead_of_reporting_it_as_a_code() {
    let message = run(
        vec![Err(service_exception(
            "Unknown",
            VALIDATION_MESSAGE,
            403.0,
            Some(REQUEST_ID),
        ))],
        Some(REQUEST_ID),
        None,
    )
    .await;
    assert_eq!(
        details(diagnostic(&message).expect("diagnostic")),
        json!({"status": 403, "requestId": REQUEST_ID})
    );
}

#[test]
fn modeled_stream_exception_becomes_a_service_exception_without_extra_metadata() {
    // 真实 SDK（`getMessageUnmarshaller`）在 `:message-type=exception` 上抛
    // `BedrockRuntimeServiceException` 实例：`.name` 是 schema 名、`.message` 取 payload，
    // 且构造器自带 `$metadata` 属性（undefined），所以首帧也不补 deserialization hint。
    use kiana_ai::api::bedrock_converse_stream::{
        DecodedEventStreamItem, EventStreamMessage, eventstream_message_to_item,
        format_bedrock_error,
    };

    let message = EventStreamMessage {
        headers: [
            (":message-type".to_string(), "exception".to_string()),
            (
                ":exception-type".to_string(),
                "throttlingException".to_string(),
            ),
        ]
        .into_iter()
        .collect(),
        payload:
            br#"{"message":"Too many requests, please wait.","$metadata":{"httpStatusCode":429}}"#
                .to_vec(),
    };
    match eventstream_message_to_item(&message).expect("decoded") {
        DecodedEventStreamItem::Exception(failure) => {
            assert!(matches!(
                &failure,
                BedrockFailure::Service { name, .. } if name == "ThrottlingException"
            ));
            assert_eq!(
                format_bedrock_error(&failure),
                "Throttling error: Too many requests, please wait."
            );
        }
        _ => panic!("expected modeled exception"),
    }
}
