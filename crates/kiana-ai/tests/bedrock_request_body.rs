//! 回归：ConverseStreamRequest 的线上 body 不含 `modelId`。
//!
//! `modelId` 是 Smithy 的 `@httpLabel`，SDK 只把它编进 URL 路径
//! （`/model/<encoded>/converse-stream`）；旧实现把整个命令输入 stringify 成 body，
//! 于是每个请求体都以 `{"modelId":...}` 开头。

mod common;

use kiana_ai::api::bedrock_converse_stream::{BedrockOptions, stream};
use kiana_ai::types::{
    Context, Message, Model, ModelInputModality, StreamOptions, UserContent, UserMessage, UserRole,
};
use kiana_ai::utils::transcript::normalize_context;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

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

fn stop_body() -> Vec<u8> {
    let mut body = Vec::new();
    for (event_type, payload) in [
        ("messageStart", r#"{"role":"assistant"}"#),
        ("messageStop", r#"{"stopReason":"end_turn"}"#),
    ] {
        body.extend_from_slice(&frame(
            &[
                (":message-type", "event"),
                (":event-type", event_type),
                (":content-type", "application/json"),
            ],
            payload,
        ));
    }
    body
}

#[tokio::test]
async fn model_id_is_only_bound_to_the_url_not_the_request_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/vnd.amazon.eventstream")
                .set_body_bytes(stop_body()),
        )
        .mount(&server)
        .await;
    let model = Model {
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
    };
    let context = normalize_context(&Context {
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("hello".into()),
            timestamp: 1,
        })],
        ..Context::default()
    });
    let message = stream(
        &model,
        &context,
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
        },
    )
    .result()
    .await
    .expect("result");
    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Stop);

    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].url.path(),
        "/model/us.anthropic.claude-opus-4-8/converse-stream"
    );
    let body = String::from_utf8(requests[0].body.clone()).expect("utf8 body");
    assert!(
        body.starts_with("{\"messages\":"),
        "body should start with messages, got {body}"
    );
    assert!(
        !body.contains("\"modelId\""),
        "body must not carry modelId: {body}"
    );
}
