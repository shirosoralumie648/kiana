//! `test/bedrock-raw-stop-reason.test.ts` 的移植。

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use kiana_ai::api::bedrock_converse_stream::{BedrockOptions, stream_with_items};
use kiana_ai::types::{
    AssistantContentBlock, Context, Message, Model, ModelInputModality, StreamOptions, UserContent,
    UserMessage, UserRole,
};

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

fn options(on_event: Option<kiana_ai::types::OnProviderStreamEventFn>) -> BedrockOptions {
    BedrockOptions {
        base: StreamOptions {
            on_provider_stream_event: on_event,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test]
async fn forwards_stream_items_in_order_before_normalizing_them() {
    let items = vec![
        json!({"messageStart": {"role": "assistant"}}),
        json!({"contentBlockDelta": {"contentBlockIndex": 0, "delta": {"text": "hello"}}}),
        json!({"messageStop": {"stopReason": "end_turn", "additionalModelResponseFields": {"source": "test"}}}),
        json!({"metadata": {"usage": {"inputTokens": 1, "outputTokens": 1, "totalTokens": 2}}}),
    ];
    let received: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cb = Arc::clone(&received);
    let event_models: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let models_cb = Arc::clone(&event_models);
    let result = stream_with_items(
        &model(),
        &context(),
        options(Some(Arc::new(move |item, event_model| {
            received_cb.lock().unwrap().push(item);
            models_cb.lock().unwrap().push(event_model.provider.clone());
        }))),
        None,
        items.clone().into_iter().map(Ok).collect(),
    )
    .result()
    .await
    .expect("result");

    assert_eq!(*received.lock().unwrap(), items);
    assert_eq!(event_models.lock().unwrap().len(), items.len());
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    assert_eq!(result.content.len(), 1);
    match &result.content[0] {
        AssistantContentBlock::Text(text) => assert_eq!(text.text, "hello"),
        other => panic!("expected text, got {other:?}"),
    }
}

#[tokio::test]
async fn forwards_error_items_before_reporting_them() {
    let exception = json!({"message": "bedrock stream failed"});
    let items = vec![
        json!({"messageStart": {"role": "assistant"}}),
        json!({"internalServerException": exception}),
    ];
    let received: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cb = Arc::clone(&received);
    let result = stream_with_items(
        &model(),
        &context(),
        options(Some(Arc::new(move |item, _model| {
            received_cb.lock().unwrap().push(item);
        }))),
        None,
        items.clone().into_iter().map(Ok).collect(),
    )
    .result()
    .await
    .expect("result");

    assert_eq!(*received.lock().unwrap(), items);
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Error);
    // 模型化异常抛的是裸对象，`safeJsonStringify` 后 errorMessage 是 payload 的 JSON。
    assert_eq!(
        result.error_message.as_deref(),
        Some("{\"message\":\"bedrock stream failed\"}")
    );
}

async fn run_stop_reason(stop_reason: &str) -> kiana_ai::types::AssistantMessage {
    stream_with_items(
        &model(),
        &context(),
        options(None),
        None,
        vec![
            Ok(json!({"messageStart": {"role": "assistant"}})),
            Ok(json!({"messageStop": {"stopReason": stop_reason}})),
        ],
    )
    .result()
    .await
    .expect("result")
}

#[tokio::test]
async fn preserves_raw_stop_reasons_for_successful_stops() {
    let message = run_stop_reason("end_turn").await;
    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Stop);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("end_turn"));
    assert!(message.error_message.is_none());
}

#[tokio::test]
async fn preserves_raw_stop_reasons_for_provider_error_stops() {
    let message = run_stop_reason("guardrail_intervened").await;
    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Error);
    assert_eq!(
        message.raw_stop_reason.as_deref(),
        Some("guardrail_intervened")
    );
    assert_eq!(
        message.error_message.as_deref(),
        Some("Provider stopped with: guardrail_intervened")
    );
}
