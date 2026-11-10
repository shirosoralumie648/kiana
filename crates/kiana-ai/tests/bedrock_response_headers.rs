//! `test/bedrock-response-headers.test.ts` 的移植（真实本地 HTTP 服务器）。
//!
//! TS 用 `node:http` 返回 200 + `application/vnd.amazon.eventstream` 空 body；Rust 侧用
//! wiremock。服务器故意返回空事件流：断言 onResponse 在消费流之前仍然触发。

mod common;

use std::sync::{Arc, Mutex};

use kiana_ai::api::bedrock_converse_stream::{BedrockOptions, stream};
use kiana_ai::types::{
    Context, Message, Model, ModelInputModality, ProviderResponse, StopReason, StreamOptions,
    UserContent, UserMessage, UserRole,
};
use kiana_ai::utils::transcript::normalize_context;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const MODEL_ID: &str = "us.anthropic.claude-haiku-4-5-20251001-v1:0";

#[tokio::test]
async fn forwards_raw_response_headers_to_on_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/vnd.amazon.eventstream")
                .insert_header("x-bifrost-provider", "bedrock")
                .insert_header("x-bifrost-resolved-model", MODEL_ID)
                .insert_header("x-amzn-requestid", "req-123")
                .set_body_bytes(Vec::new()),
        )
        .mount(&server)
        .await;

    let model = Model {
        id: MODEL_ID.into(),
        name: "Claude Haiku 4.5".into(),
        api: "bedrock-converse-stream".into(),
        provider: "amazon-bedrock".into(),
        base_url: server.uri(),
        reasoning: Some(false),
        input: vec![ModelInputModality::Text],
        context_window: Some(200_000),
        max_tokens: Some(8192),
        ..Model::default()
    };
    let context = normalize_context(&Context {
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("hello".into()),
            timestamp: 1,
        })],
        ..Context::default()
    });
    let responses: Arc<Mutex<Vec<ProviderResponse>>> = Arc::new(Mutex::new(Vec::new()));
    let responses_cb = Arc::clone(&responses);
    let result = stream(
        &model,
        &context,
        BedrockOptions {
            base: StreamOptions {
                cache_retention: Some(kiana_ai::types::CacheRetention::None),
                env: Some(
                    [
                        ("AWS_BEDROCK_FORCE_HTTP1".to_string(), "1".to_string()),
                        ("AWS_BEDROCK_SKIP_AUTH".to_string(), "1".to_string()),
                    ]
                    .into_iter()
                    .collect(),
                ),
                on_response: Some(Arc::new(move |response, _model| {
                    responses_cb.lock().unwrap().push(response);
                })),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .result()
    .await
    .expect("result");

    assert_eq!(result.stop_reason, StopReason::Error);
    let responses = responses.lock().unwrap();
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].status, 200);
    assert_eq!(
        responses[0]
            .headers
            .get("x-amzn-requestid")
            .map(String::as_str),
        Some("req-123")
    );
    assert_eq!(
        responses[0]
            .headers
            .get("x-bifrost-provider")
            .map(String::as_str),
        Some("bedrock")
    );
    assert_eq!(
        responses[0]
            .headers
            .get("x-bifrost-resolved-model")
            .map(String::as_str),
        Some(MODEL_ID)
    );
}
