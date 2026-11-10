//! `packages/ai/test/xiaomi-models.test.ts` 的 Rust 移植。
//!
//! `getModels(provider)` → `providers::all::get_builtin_models`；
//! `streamSimple` + `onPayload` 的 payload 捕获同 `baseten-models.test.ts`（mock HTTP）。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use std::sync::{Arc, Mutex};

use harness::{MockHttpClient, MockResponse, context};
use kiana_ai::api::openai_completions::stream_simple;
use kiana_ai::models::get_supported_thinking_levels;
use kiana_ai::providers::all::get_builtin_models;
use kiana_ai::types::{
    Message, Model, ModelThinkingLevel, SimpleStreamOptions, ThinkingLevel, UserMessage, UserRole,
};
use serde_json::Value;

const XIAOMI_PROVIDERS: &[&str] = &[
    "xiaomi",
    "xiaomi-token-plan-cn",
    "xiaomi-token-plan-ams",
    "xiaomi-token-plan-sgp",
];
const DEPRECATED_MODEL_IDS: &[&str] = &["mimo-v2-flash", "mimo-v2-omni", "mimo-v2-pro"];
const REPLACEMENT_MODEL_IDS: &[&str] = &["mimo-v2.6-flash", "mimo-v2.6-pro"];

fn user_context() -> kiana_ai::types::TranscriptContext {
    context(
        None,
        vec![Message::User(UserMessage {
            role: UserRole::User,
            content: kiana_ai::types::UserContent::Text("test".into()),
            timestamp: 0,
        })],
    )
}

async fn capture_payload(model: &Model, reasoning: Option<ThinkingLevel>) -> Value {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_callback = captured.clone();
    let fetch = MockHttpClient::fixed(MockResponse::json(400, "{}"));
    let mut options = SimpleStreamOptions {
        base: harness::stream_options(fetch),
        reasoning,
        ..Default::default()
    };
    options.base.on_payload = Some(Arc::new(move |payload: Value, _model| {
        *captured_for_callback.lock().unwrap() = Some(payload);
        None
    }));
    if let Ok(stream) = stream_simple(model, &user_context(), options) {
        let _ = stream.result().await;
    }
    captured
        .lock()
        .unwrap()
        .clone()
        .expect("payload captured before the request")
}

#[test]
fn omits_deprecated_models() {
    for provider in XIAOMI_PROVIDERS {
        let ids: Vec<String> = get_builtin_models(provider)
            .into_iter()
            .map(|model| model.id)
            .collect();
        for deprecated in DEPRECATED_MODEL_IDS {
            assert!(
                !ids.iter().any(|id| id == deprecated),
                "{provider}/{deprecated}"
            );
        }
    }
}

#[test]
fn keeps_replacement_models() {
    for provider in XIAOMI_PROVIDERS {
        let ids: Vec<String> = get_builtin_models(provider)
            .into_iter()
            .map(|model| model.id)
            .collect();
        for replacement in REPLACEMENT_MODEL_IDS {
            assert!(
                ids.iter().any(|id| id == replacement),
                "{provider}/{replacement}"
            );
        }
    }
}

// #10818: MiMo Chat Completions only supports a thinking on/off toggle.
#[tokio::test]
async fn models_reasoning_as_an_off_on_toggle() {
    for provider in XIAOMI_PROVIDERS {
        let models: Vec<Model> = get_builtin_models(provider)
            .into_iter()
            .filter(|model| model.reasoning == Some(true))
            .collect();
        assert!(!models.is_empty(), "{provider} has no reasoning models");

        for model in models {
            assert_eq!(
                get_supported_thinking_levels(&model),
                vec![ModelThinkingLevel::Off, ModelThinkingLevel::High],
                "{provider}/{}",
                model.id
            );

            let enabled = capture_payload(&model, Some(ThinkingLevel::High)).await;
            assert_eq!(
                enabled["thinking"],
                serde_json::json!({ "type": "enabled" }),
                "{provider}/{}",
                model.id
            );
            assert!(
                enabled.get("reasoning_effort").is_none(),
                "{provider}/{}",
                model.id
            );

            let disabled = capture_payload(&model, None).await;
            assert_eq!(
                disabled["thinking"],
                serde_json::json!({ "type": "disabled" }),
                "{provider}/{}",
                model.id
            );
            assert!(
                disabled.get("reasoning_effort").is_none(),
                "{provider}/{}",
                model.id
            );
        }
    }
}
