//! `packages/ai/test/baseten-models.test.ts` 的 Rust 移植。
//!
//! TS 的 `streamSimple` + `onPayload` 抛错用于在发出请求前捕获 payload；Rust 的
//! `onPayload` 不能抛错，因此用 mock HTTP 客户端让请求立即失败，payload 仍被同步捕获。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use std::sync::{Arc, Mutex};

use harness::{MockHttpClient, MockResponse, context};
use kiana_ai::api::openai_completions::stream_simple;
use kiana_ai::env_api_keys::{find_env_keys, get_env_api_key};
use kiana_ai::providers::all::get_builtin_model;
use kiana_ai::types::{Message, Model, SimpleStreamOptions, ThinkingLevel, UserMessage, UserRole};
use serde_json::Value;

fn catalog_model(id: &str) -> Model {
    get_builtin_model("baseten", id).unwrap_or_else(|| panic!("missing baseten/{id}"))
}

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
fn keeps_both_glm_5_2_endpoints_text_only() {
    assert_eq!(
        catalog_model("zai-org/GLM-5.2").input,
        vec![kiana_ai::types::ModelInputModality::Text]
    );
    assert_eq!(
        catalog_model("zai-org/GLM-5.2-Fast").input,
        vec![kiana_ai::types::ModelInputModality::Text]
    );
}

#[tokio::test]
async fn sends_baseten_chat_template_args_with_reasoning_effort() {
    let payload =
        capture_payload(&catalog_model("zai-org/GLM-5.2"), Some(ThinkingLevel::High)).await;
    assert_eq!(
        payload["chat_template_args"],
        serde_json::json!({ "enable_thinking": true })
    );
    assert_eq!(payload["reasoning_effort"], serde_json::json!("high"));
}

#[tokio::test]
async fn disables_baseten_opt_in_reasoning_when_thinking_is_off() {
    let payload = capture_payload(&catalog_model("zai-org/GLM-5.2"), None).await;
    assert_eq!(
        payload["chat_template_args"],
        serde_json::json!({ "enable_thinking": false })
    );
    assert_eq!(payload["reasoning_effort"], serde_json::json!("none"));
}

#[test]
fn resolves_baseten_api_key_from_the_environment() {
    const VARIABLE: &str = "BASETEN_API_KEY";
    let original = std::env::var(VARIABLE).ok();
    unsafe { std::env::set_var(VARIABLE, "test-baseten-key") };

    assert_eq!(find_env_keys("baseten", None), Some(vec![VARIABLE]));
    assert_eq!(
        get_env_api_key("baseten", None),
        Some("test-baseten-key".to_string())
    );

    match original {
        Some(value) => unsafe { std::env::set_var(VARIABLE, value) },
        None => unsafe { std::env::remove_var(VARIABLE) },
    }
}
