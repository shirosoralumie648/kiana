//! `test/provider-error-body-regression.test.ts` 的移植。
//!
//! 三个层级：openai-completions / openai-responses / Bedrock。

mod common;

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use kiana_ai::api::bedrock_converse_stream::{
    BedrockErrorMetadata, BedrockFailure, BedrockOptions, format_bedrock_error, stream,
};
use std::sync::Arc;

use kiana_ai::api::openai_completions::OpenAICompletionsOptions;
use kiana_ai::api::openai_responses::OpenAIResponsesOptions;
use kiana_ai::types::{
    Context, Message, Model, ModelInputModality, StopReason, StreamOptions, UserContent,
    UserMessage, UserRole,
};
use serde_json::json;

#[test]
fn bedrock_surfaces_the_gateway_body_instead_of_unknown_unknown_error() {
    // SDK 对非 2xx 的 `$response.body` 是字符串时，normalizeProviderError 会把它作为 body；
    // message 不含 body 时展示 `<status>: <body>`。
    let failure = BedrockFailure::Named {
        name: "UnknownError".into(),
        message: "UnknownError".into(),
        metadata: Some(BedrockErrorMetadata {
            http_status_code: Some(403.0),
            request_id: None,
        }),
        body: Some("{\"message\":\"blocked by gateway WAF\"}".into()),
    };
    let message = format_bedrock_error(&failure);
    assert!(message.contains("403"), "{message}");
    assert!(message.contains("blocked by gateway WAF"), "{message}");
    assert!(!message.contains("Unknown: UnknownError"), "{message}");
}

#[test]
fn bedrock_preserves_the_sdk_validation_message_when_the_response_body_is_a_stream() {
    // `$response.body` 是可读流：没有 body 文本，message 原样保留。
    let failure = BedrockFailure::Service {
        name: "ValidationException".into(),
        message: "Invocation of model ID anthropic.claude-opus-5 with on-demand throughput isn't supported. Retry with an inference profile.".into(),
        metadata: BedrockErrorMetadata {
            http_status_code: Some(400.0),
            request_id: None,
        },
        body: None,
    };
    let message = format_bedrock_error(&failure);
    assert!(
        message.contains("on-demand throughput isn't supported"),
        "{message}"
    );
    assert!(message.contains("inference profile"), "{message}");
    assert!(!message.contains("_readableState"), "{message}");
    assert!(message.starts_with("Validation error: "), "{message}");
}

fn model(base_url: &str) -> Model {
    Model {
        id: "global.anthropic.claude-opus-5".into(),
        name: "Claude Opus 5".into(),
        api: "bedrock-converse-stream".into(),
        provider: "amazon-bedrock".into(),
        base_url: base_url.into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        context_window: Some(200_000),
        max_tokens: Some(64_000),
        ..Model::default()
    }
}

fn context() -> kiana_ai::types::TranscriptContext {
    kiana_ai::utils::transcript::normalize_context(&Context {
        system_prompt: Some(String::new()),
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![kiana_ai::types::UserContentBlock::Text(
                kiana_ai::types::TextContent {
                    kind_text: kiana_ai::types::TextKind::Text,
                    text: "hi".into(),
                    text_signature: None,
                },
            )]),
            timestamp: 0,
        })],
        tools: Some(vec![]),
    })
}

async fn run_against(
    server: &wiremock::MockServer,
    status: u16,
    headers: &[(&str, &str)],
    body: &str,
) -> kiana_ai::types::AssistantMessage {
    use wiremock::matchers::method;
    use wiremock::{Mock, ResponseTemplate};
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
    stream(
        &model(&server.uri()),
        &context(),
        BedrockOptions {
            base: StreamOptions {
                env: Some(
                    [("AWS_BEDROCK_SKIP_AUTH".to_string(), "1".to_string())]
                        .into_iter()
                        .collect(),
                ),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("result")
}

#[tokio::test]
async fn bedrock_http_403_surfaces_the_gateway_body_instead_of_unknown_unknown_error() {
    // 真实 SDK 的 403-untyped JSON：合成异常 name="Unknown"、message 取 body.message，
    // `$response.body` 是流（status/body 折不进 message），状态码进诊断而不是 errorMessage。
    let server = common::wiremock_util::start().await;
    let message = run_against(
        &server,
        403,
        &[("x-amzn-requestid", "REQ-403")],
        "{\"message\":\"blocked by gateway WAF\"}",
    )
    .await;
    let error = message.error_message.clone().unwrap_or_default();
    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Error);
    assert_eq!(error, "Unknown: blocked by gateway WAF");
    let details = message
        .diagnostics
        .as_ref()
        .and_then(|diagnostics| {
            diagnostics
                .iter()
                .find(|diagnostic| diagnostic.kind == "bedrock_response_failure")
        })
        .and_then(|diagnostic| diagnostic.details.clone())
        .expect("diagnostic details");
    assert_eq!(details.get("status"), Some(&serde_json::json!(403)));
    assert_eq!(
        details.get("requestId"),
        Some(&serde_json::json!("REQ-403"))
    );
    assert_eq!(details.get("errorCode"), None);
}

#[tokio::test]
async fn bedrock_http_validation_error_preserves_the_sdk_validation_message() {
    let server = common::wiremock_util::start().await;
    let validation = "Invocation of model ID anthropic.claude-opus-5 with on-demand throughput isn't supported. Retry with an inference profile.";
    let body = serde_json::json!({"message": validation}).to_string();
    let message = run_against(
        &server,
        400,
        &[("x-amzn-errortype", "ValidationException")],
        &body,
    )
    .await;
    let error = message.error_message.clone().unwrap_or_default();
    assert!(
        error.contains("on-demand throughput isn't supported"),
        "{error}"
    );
    assert!(error.contains("inference profile"), "{error}");
    assert!(error.starts_with("Validation error: "), "{error}");
    assert!(!error.contains("_readableState"), "{error}");
}

fn test_model(api: &str, provider: &str, base_url: &str) -> Model {
    Model {
        id: "test-model".into(),
        name: "Test Model".into(),
        api: api.into(),
        provider: provider.into(),
        base_url: base_url.into(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text],
        context_window: Some(1000),
        max_tokens: Some(100),
        ..Default::default()
    }
}

fn text_context() -> kiana_ai::types::TranscriptContext {
    mock_http::text_context("hi")
}

/// 403 + JSON body（openai SDK 的 `APIError` shape：状态码 + 解析后的 body）。
fn forbidden(body: &serde_json::Value) -> mock_http::MockResponse {
    mock_http::MockResponse::json(403, body)
}

#[tokio::test]
async fn openai_completions_surfaces_status_and_body() {
    let mock = mock_http::MockHttp::single(forbidden(&json!({
        "error": "blocked by gateway WAF",
    })));
    let model = test_model(
        "openai-completions",
        "openrouter",
        "https://openrouter.ai/api/v1",
    );
    let output = mock_http::run_completions(
        &model,
        &text_context(),
        OpenAICompletionsOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(Arc::new(mock)),
                max_retries: Some(0),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;

    assert_eq!(output.stop_reason, StopReason::Error);
    let error = output.error_message.clone().unwrap_or_default();
    assert!(error.contains("403"), "{error}");
    assert!(error.contains("blocked by gateway WAF"), "{error}");
    assert_ne!(error, "403 status code (no body)");
}

#[tokio::test]
async fn openai_completions_does_not_double_print_the_openrouter_metadata_raw_extra() {
    let mock = mock_http::MockHttp::single(forbidden(&json!({
        "message": "Provider returned error",
        "code": 403,
        "metadata": { "raw": "upstream WAF blocked policy XYZ" },
    })));
    let model = test_model(
        "openai-completions",
        "openrouter",
        "https://openrouter.ai/api/v1",
    );
    let output = mock_http::run_completions(
        &model,
        &text_context(),
        OpenAICompletionsOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(Arc::new(mock)),
                max_retries: Some(0),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;

    let error = output.error_message.clone().unwrap_or_default();
    assert!(error.contains("upstream WAF blocked policy XYZ"), "{error}");
    assert_eq!(
        error.matches("upstream WAF blocked policy XYZ").count(),
        1,
        "{error}"
    );
}

#[tokio::test]
async fn openai_responses_keeps_the_prefix_and_surfaces_the_body() {
    let mock = mock_http::MockHttp::single(forbidden(&json!({
        "error": "blocked by gateway WAF",
    })));
    let model = test_model("openai-responses", "openai", "https://api.openai.com/v1");
    let output = mock_http::run_responses(
        &model,
        &text_context(),
        OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("test".into()),
                fetch: Some(Arc::new(mock)),
                max_retries: Some(0),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;

    assert_eq!(output.stop_reason, StopReason::Error);
    let error = output.error_message.clone().unwrap_or_default();
    assert!(error.contains("OpenAI API error (403)"), "{error}");
    assert!(error.contains("blocked by gateway WAF"), "{error}");
}
