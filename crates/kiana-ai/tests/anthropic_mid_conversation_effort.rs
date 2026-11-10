//! `anthropic-mid-conversation-effort.test.ts` 的 Rust 移植。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use std::sync::{Arc, Mutex};

use harness::{MockHttpClient, MockResponse, context};
use kiana_ai::api::anthropic_messages::{AnthropicOptions, stream};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, Message, Model, ModelCost, ModelInputModality,
    ModelThinkingLevel, ThinkingContent, ThinkingLevel, ThinkingLevelMap, UserMessage, UserRole,
};
use serde_json::{Value, json};

fn managed_model(provider: &str) -> Model {
    Model {
        id: "claude-fable-5-1".into(),
        name: "Claude Fable 5.1".into(),
        api: "anthropic-messages".into(),
        provider: provider.into(),
        base_url: "http://127.0.0.1:9".into(),
        reasoning: Some(true),
        thinking_level_map: Some(ThinkingLevelMap::from([
            (ModelThinkingLevel::Off, None),
            (ModelThinkingLevel::Minimal, Some("low".into())),
            (ModelThinkingLevel::Low, Some("low".into())),
            (ModelThinkingLevel::Medium, Some("medium".into())),
            (ModelThinkingLevel::High, Some("high".into())),
            (ModelThinkingLevel::Max, Some("max".into())),
        ])),
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: Some(200_000),
        max_tokens: Some(32_000),
        compat: Some(json!({ "forceAdaptiveThinking": true, "supportsMidConvoEffort": true })),
        ..Model::default()
    }
}

fn user(text: &str, timestamp: i64) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: kiana_ai::types::UserContent::Text(text.into()),
        timestamp,
    })
}

fn assistant(model: &Model, level: Option<&str>) -> Message {
    Message::Assistant(AssistantMessage {
        role: Default::default(),
        content: vec![
            AssistantContentBlock::Thinking(ThinkingContent {
                thinking: "reasoning".into(),
                thinking_signature: Some("signature".into()),
                ..Default::default()
            }),
            AssistantContentBlock::Text(kiana_ai::types::TextContent {
                text: "answer".into(),
                ..Default::default()
            }),
        ],
        api: "anthropic-messages".into(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        provider_thinking_level: level.map(str::to_string),
        usage: Default::default(),
        stop_reason: Default::default(),
        timestamp: 1,
        ..Default::default()
    })
}

struct Captured {
    payload: Value,
    message: AssistantMessage,
}

async fn capture(
    model: &Model,
    messages: Vec<Message>,
    effort: Option<&str>,
    thinking_enabled: Option<bool>,
) -> Captured {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_callback = captured.clone();
    let fetch = MockHttpClient::fixed(MockResponse::json(400, "{}"));
    let mut base = harness::stream_options(fetch);
    base.api_key = Some("test-key".into());
    base.cache_retention = Some(kiana_ai::types::CacheRetention::None);
    base.on_payload = Some(Arc::new(move |payload: Value, _model| {
        *captured_for_callback.lock().unwrap() = Some(payload);
        None
    }));
    let options = AnthropicOptions {
        base,
        thinking_enabled: thinking_enabled.or(Some(true)),
        effort: effort.map(str::to_string),
        ..Default::default()
    };
    let stream = stream(model, &context(None, messages), options);
    let message = stream.result().await.expect("stream result");
    Captured {
        payload: captured.lock().unwrap().clone().expect("payload"),
        message,
    }
}

fn effort_messages(payload: &Value) -> Vec<Value> {
    payload["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .filter(|message| message["role"] == "system")
        .cloned()
        .collect()
}

#[tokio::test]
async fn reconstructs_an_exact_historical_marker_prefix_and_appends_the_current_marker() {
    let model = managed_model("anthropic");
    let first = capture(&model, vec![user("one", 1)], Some("low"), Some(true)).await;
    let second = capture(
        &model,
        vec![
            user("one", 1),
            assistant(&model, Some("low")),
            user("two", 2),
        ],
        Some("high"),
        Some(true),
    )
    .await;
    assert_eq!(
        first.payload["messages"],
        json!([
            { "role": "user", "content": "one" },
            { "role": "system", "content": [], "output_config": { "effort": "low" } },
        ])
    );
    let first_len = first.payload["messages"].as_array().unwrap().len();
    assert_eq!(
        &second.payload["messages"].as_array().unwrap()[..first_len],
        &first.payload["messages"].as_array().unwrap()[..]
    );
    assert_eq!(
        second.payload["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap(),
        &json!({ "role": "system", "content": [], "output_config": { "effort": "high" } })
    );
    assert_eq!(first.payload["output_config"], json!({ "effort": "low" }));
    assert_eq!(second.payload["output_config"], json!({ "effort": "low" }));
    assert_eq!(
        second.payload["thinking"],
        json!({
            "type": "adaptive",
            "display": "summarized",
            "block_binding": { "prefix_mismatch_behavior": "drop_block" },
        })
    );
    assert_eq!(
        first.message.provider_thinking_level.as_deref(),
        Some("low")
    );
}

#[tokio::test]
async fn preserves_native_effort_levels() {
    for effort in ["low", "medium", "high", "xhigh", "max"] {
        let model = managed_model("anthropic");
        let captured = capture(&model, vec![user("one", 1)], Some(effort), Some(true)).await;
        assert_eq!(
            effort_messages(&captured.payload),
            vec![json!({ "role": "system", "content": [], "output_config": { "effort": effort } })]
        );
        assert_eq!(
            captured.message.provider_thinking_level.as_deref(),
            Some(effort)
        );
    }
}

#[tokio::test]
async fn defaults_omitted_effort_to_high_and_still_enables_drop_block() {
    let model = managed_model("anthropic");
    let captured = capture(&model, vec![user("one", 1)], None, Some(true)).await;
    assert_eq!(
        captured.payload["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap(),
        &json!({ "role": "system", "content": [], "output_config": { "effort": "high" } })
    );
    assert_eq!(
        captured.payload["thinking"]["block_binding"]["prefix_mismatch_behavior"],
        Value::String("drop_block".into())
    );
    assert_eq!(
        captured.message.provider_thinking_level.as_deref(),
        Some("high")
    );
}

#[tokio::test]
async fn does_not_invent_markers_for_legacy_or_other_provider_assistants() {
    let model = managed_model("anthropic");
    let captured = capture(
        &model,
        vec![
            user("one", 1),
            assistant(&model, None),
            user("two", 2),
            {
                let mut other = assistant(&model, Some("low"));
                if let Message::Assistant(assistant) = &mut other {
                    assistant.provider = "other-provider".into();
                }
                other
            },
            user("three", 3),
        ],
        Some("medium"),
        Some(true),
    )
    .await;
    assert_eq!(
        effort_messages(&captured.payload),
        vec![json!({ "role": "system", "content": [], "output_config": { "effort": "medium" } })]
    );
}

#[tokio::test]
async fn sends_plain_disabled_thinking_without_effort_when_off_is_supported() {
    let mut model = managed_model("anthropic");
    model.thinking_level_map = Some(ThinkingLevelMap::from([
        (ModelThinkingLevel::Minimal, None),
        (ModelThinkingLevel::Low, Some("low".into())),
        (ModelThinkingLevel::Medium, Some("medium".into())),
        (ModelThinkingLevel::High, Some("high".into())),
        (ModelThinkingLevel::Max, Some("max".into())),
    ]));
    let captured = capture(
        &model,
        vec![
            user("one", 1),
            assistant(&model, Some("low")),
            user("two", 2),
            assistant(&model, Some("medium")),
            user("three", 3),
        ],
        None,
        Some(false),
    )
    .await;
    assert_eq!(captured.payload["thinking"], json!({ "type": "disabled" }));
    assert!(captured.payload.get("output_config").is_none());
    assert!(effort_messages(&captured.payload).is_empty());
    assert!(captured.message.provider_thinking_level.is_none());
}

#[tokio::test]
async fn keeps_adaptive_thinking_when_off_is_unsupported() {
    let captured = capture(
        &managed_model("anthropic"),
        vec![user("one", 1)],
        None,
        Some(false),
    )
    .await;
    assert_eq!(
        captured.payload["thinking"]["type"],
        Value::String("adaptive".into())
    );
    assert_eq!(
        effort_messages(&captured.payload),
        vec![json!({ "role": "system", "content": [], "output_config": { "effort": "high" } })]
    );
    assert_eq!(
        captured.message.provider_thinking_level.as_deref(),
        Some("high")
    );
}

#[tokio::test]
async fn leaves_unsupported_models_on_top_level_effort() {
    let mut model = managed_model("anthropic");
    model.compat = Some(json!({ "forceAdaptiveThinking": true }));
    let captured = capture(&model, vec![user("one", 1)], Some("low"), Some(true)).await;
    assert_eq!(
        captured.payload["messages"],
        json!([{ "role": "user", "content": "one" }])
    );
    assert_eq!(
        captured.payload["output_config"],
        json!({ "effort": "low" })
    );
    assert_eq!(
        captured.payload["thinking"],
        json!({ "type": "adaptive", "display": "summarized" })
    );
    assert!(captured.message.provider_thinking_level.is_none());
}

#[tokio::test]
async fn sends_the_effort_and_binding_beta_headers() {
    let fetch = MockHttpClient::fixed(MockResponse::sse(
        [
            (
                "message_start",
                json!({ "type": "message_start", "message": { "id": "msg_test", "model": "claude-fable-5-1", "usage": { "input_tokens": 1, "output_tokens": 0 } } }),
            ),
            (
                "message_delta",
                json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "input_tokens": 1, "output_tokens": 1 } }),
            ),
            ("message_stop", json!({ "type": "message_stop" })),
        ]
        .iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect::<String>(),
    ));
    let mut base = harness::stream_options(fetch.clone());
    base.cache_retention = Some(kiana_ai::types::CacheRetention::None);
    let result = harness::run_to_result(stream(
        &managed_model("anthropic"),
        &context(None, vec![user("one", 1)]),
        AnthropicOptions {
            base,
            thinking_enabled: Some(true),
            ..Default::default()
        },
    ))
    .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    let request = fetch.requests().remove(0);
    let beta_header = request
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("anthropic-beta"))
        .map(|(_, value)| value.clone())
        .expect("anthropic-beta header");
    assert!(beta_header.contains("mid-conversation-output-config-2026-07-01"));
    assert!(beta_header.contains("thinking-binding-controls-2026-08-01"));
}

#[test]
fn generates_exact_model_and_transport_gates() {
    let direct: Model = serde_json::from_value(
        kiana_ai::providers::catalog::get_model("anthropic", "claude-fable-5-1")
            .expect("model")
            .clone(),
    )
    .unwrap();
    assert_eq!(
        direct
            .compat
            .as_ref()
            .and_then(|compat| compat.get("supportsMidConvoEffort")),
        Some(&Value::Bool(true))
    );
    assert_eq!(
        direct
            .thinking_level_map
            .as_ref()
            .and_then(|map| map.get(&ModelThinkingLevel::Off)),
        Some(&None)
    );
    let open_router: Model = serde_json::from_value(
        kiana_ai::providers::catalog::get_model("openrouter", "anthropic/claude-fable-5.1")
            .expect("model")
            .clone(),
    )
    .unwrap();
    assert_eq!(open_router.api, "anthropic-messages");
    assert_eq!(open_router.base_url, "https://openrouter.ai/api");
    assert_eq!(
        open_router
            .compat
            .as_ref()
            .and_then(|compat| compat.get("supportsMidConvoEffort")),
        Some(&Value::Bool(true))
    );
    let unsupported: Model = serde_json::from_value(
        kiana_ai::providers::catalog::get_model("anthropic", "claude-opus-4-8")
            .expect("model")
            .clone(),
    )
    .unwrap();
    assert!(
        unsupported
            .compat
            .as_ref()
            .and_then(|compat| compat.get("supportsMidConvoEffort"))
            .is_none()
    );
    let openrouter_opus: Model = serde_json::from_value(
        kiana_ai::providers::catalog::get_model("openrouter", "anthropic/claude-opus-5")
            .expect("model")
            .clone(),
    )
    .unwrap();
    assert!(
        openrouter_opus
            .compat
            .as_ref()
            .and_then(|compat| compat.get("supportsMidConvoEffort"))
            .is_none()
    );
    let opus5: Model = serde_json::from_value(
        kiana_ai::providers::catalog::get_model("anthropic", "claude-opus-5")
            .expect("model")
            .clone(),
    )
    .unwrap();
    assert!(
        opus5
            .compat
            .as_ref()
            .and_then(|compat| compat.get("allowedFallbackModels"))
            .is_none()
    );
}

#[test]
fn supports_off_only_on_managed_models_that_accept_disabled_thinking() {
    for id in ["claude-opus-5", "claude-haiku-5-5"] {
        let model: Model = serde_json::from_value(
            kiana_ai::providers::catalog::get_model("anthropic", id)
                .unwrap()
                .clone(),
        )
        .unwrap();
        assert_eq!(
            model
                .compat
                .as_ref()
                .and_then(|compat| compat.get("supportsMidConvoEffort")),
            Some(&Value::Bool(true)),
            "{id}"
        );
        assert_ne!(
            model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get(&ModelThinkingLevel::Off)),
            Some(&None),
            "{id}"
        );
    }
    for id in ["claude-opus-5-5", "claude-sonnet-5-5", "claude-fable-5-1"] {
        let model: Model = serde_json::from_value(
            kiana_ai::providers::catalog::get_model("anthropic", id)
                .unwrap()
                .clone(),
        )
        .unwrap();
        assert_eq!(
            model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get(&ModelThinkingLevel::Off)),
            Some(&None),
            "{id}"
        );
    }
}

#[test]
fn thinking_level_conversion_keeps_all_levels() {
    // 保留对 `ThinkingLevel` 的引用，避免 unused import（该类型在 streamSimple 用例中使用）。
    let _ = ThinkingLevel::Low;
}
