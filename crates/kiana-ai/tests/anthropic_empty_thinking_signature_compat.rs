//! `anthropic-empty-thinking-signature-compat.test.ts` 的 Rust 移植。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use std::sync::{Arc, Mutex};

use harness::{MockHttpClient, MockResponse, context};
use kiana_ai::api::anthropic_messages::stream_simple;
use kiana_ai::types::{
    AssistantContentBlock, Model, ModelCost, ModelInputModality, SimpleStreamOptions, TextContent,
    ThinkingContent, UserMessage, UserRole,
};
use serde_json::Value;

fn model(provider: &str, id: &str, allow_empty_signature: Option<bool>) -> Model {
    Model {
        id: id.into(),
        name: id.into(),
        api: "anthropic-messages".into(),
        provider: provider.into(),
        base_url: "http://127.0.0.1:9/anthropic".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(1_048_576),
        max_tokens: Some(1024),
        compat: allow_empty_signature
            .map(|value| serde_json::json!({ "allowEmptySignature": value })),
        ..Model::default()
    }
}

fn user(text: &str) -> kiana_ai::types::Message {
    kiana_ai::types::Message::User(UserMessage {
        role: UserRole::User,
        content: kiana_ai::types::UserContent::Text(text.into()),
        timestamp: 0,
    })
}

fn assistant(
    provider: &str,
    model_id: &str,
    thinking: &str,
    signature: &str,
) -> kiana_ai::types::Message {
    kiana_ai::types::Message::Assistant(kiana_ai::types::AssistantMessage {
        role: Default::default(),
        content: vec![AssistantContentBlock::Thinking(ThinkingContent {
            thinking: thinking.into(),
            thinking_signature: Some(signature.into()),
            ..Default::default()
        })],
        api: "anthropic-messages".into(),
        provider: provider.into(),
        model: model_id.into(),
        usage: kiana_ai::types::Usage::default(),
        stop_reason: kiana_ai::types::StopReason::Stop,
        timestamp: 1,
        ..Default::default()
    })
}

async fn capture_payload(model: &Model, messages: Vec<kiana_ai::types::Message>) -> Value {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_callback = captured.clone();
    let mut options = SimpleStreamOptions {
        base: harness::stream_options(MockHttpClient::fixed(MockResponse::json(400, "{}"))),
        ..Default::default()
    };
    options.base.api_key = Some("fake-key".into());
    options.base.on_payload = Some(Arc::new(move |payload: Value, _model| {
        *captured_for_callback.lock().unwrap() = Some(payload);
        None
    }));
    let context = context(None, messages);
    if let Ok(stream) = stream_simple(model, &context, options) {
        let _ = stream.result().await;
    }
    captured.lock().unwrap().clone().expect("payload")
}

fn assistant_content(payload: &Value) -> Value {
    payload["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|message| message["role"] == "assistant")
        .map(|message| message["content"].clone())
        .expect("assistant message")
}

#[tokio::test]
async fn converts_empty_signature_thinking_to_text_by_default() {
    let model = model("xiaomi-token-plan-ams", "mimo-v2.6-pro", None);
    let payload = capture_payload(
        &model,
        vec![
            user("first"),
            assistant(
                "xiaomi-token-plan-ams",
                "mimo-v2.6-pro",
                "internal reasoning",
                "",
            ),
            user("second"),
        ],
    )
    .await;
    assert_eq!(
        assistant_content(&payload),
        serde_json::json!([{ "type": "text", "text": "internal reasoning" }])
    );
}

#[tokio::test]
async fn preserves_empty_thinking_text_when_the_signature_is_present() {
    let model = model("xiaomi-token-plan-ams", "mimo-v2.6-pro", None);
    let payload = capture_payload(
        &model,
        vec![
            user("first"),
            assistant(
                "xiaomi-token-plan-ams",
                "mimo-v2.6-pro",
                "",
                "signed-thinking",
            ),
            user("second"),
        ],
    )
    .await;
    assert_eq!(
        assistant_content(&payload),
        serde_json::json!([{ "type": "thinking", "thinking": "", "signature": "signed-thinking" }])
    );
}

#[tokio::test]
async fn preserves_empty_signature_thinking_when_allow_empty_signature_is_enabled() {
    let model = model("xiaomi-token-plan-ams", "mimo-v2.6-pro", Some(true));
    let payload = capture_payload(
        &model,
        vec![
            user("first"),
            assistant(
                "xiaomi-token-plan-ams",
                "mimo-v2.6-pro",
                "internal reasoning",
                " ",
            ),
            user("second"),
        ],
    )
    .await;
    assert_eq!(
        assistant_content(&payload),
        serde_json::json!([{ "type": "thinking", "thinking": "internal reasoning", "signature": "" }])
    );
}

#[test]
fn vercel_and_fireworks_models_allow_empty_signatures() {
    let vercel = kiana_ai::providers::catalog::get_models("vercel-ai-gateway");
    assert!(!vercel.is_empty());
    for model in vercel {
        let model: Model = serde_json::from_value(model.clone()).expect("model");
        assert_eq!(
            model
                .compat
                .as_ref()
                .and_then(|compat| compat.get("allowEmptySignature"))
                .and_then(Value::as_bool),
            Some(true),
            "{}",
            model.id
        );
    }
    for id in [
        "accounts/fireworks/models/deepseek-v4p1-flash",
        "accounts/fireworks/models/qwen3p8-max",
        "accounts/fireworks/models/qwen3p8-2p4t-a95b",
        "accounts/fireworks/models/nemotron-3-ultra-nvfp4",
    ] {
        let value = kiana_ai::providers::catalog::get_model("fireworks", id)
            .unwrap_or_else(|| panic!("missing fireworks model {id}"));
        assert_eq!(
            value["compat"]["allowEmptySignature"],
            Value::Bool(true),
            "{id}"
        );
    }
}

#[tokio::test]
async fn preserves_unsigned_thinking_for_fireworks_models() {
    let id = "accounts/fireworks/models/deepseek-v4p1-flash";
    let value = kiana_ai::providers::catalog::get_model("fireworks", id).expect("catalog model");
    let model: Model = serde_json::from_value(value.clone()).expect("model");
    let mut assistant = match assistant("fireworks", id, "internal reasoning", "") {
        kiana_ai::types::Message::Assistant(assistant) => assistant,
        _ => unreachable!(),
    };
    assistant
        .content
        .push(AssistantContentBlock::Text(TextContent {
            text: "answer".into(),
            ..Default::default()
        }));
    let payload = capture_payload(
        &model,
        vec![
            user("first"),
            kiana_ai::types::Message::Assistant(assistant),
            user("second"),
        ],
    )
    .await;
    assert_eq!(
        assistant_content(&payload),
        serde_json::json!([
            { "type": "thinking", "thinking": "internal reasoning", "signature": "" },
            { "type": "text", "text": "answer" },
        ])
    );
}
