//! `packages/ai/test/qwen-token-plan-models.test.ts` 的 Rust 移植。
//!
//! TS mock 了 `openai` SDK；Rust 用 [`harness`] 的 mock HTTP 客户端（请求立即 400），
//! `onPayload` 在发请求前同步捕获 payload。逐模型遍历用例合并为循环（断言不变）。

#[path = "support/api_anthropic_codex.rs"]
mod harness;

use std::sync::{Arc, Mutex};

use harness::{MockHttpClient, MockResponse, context};
use kiana_ai::api::openai_completions::stream_simple;
use kiana_ai::env_api_keys::find_env_keys;
use kiana_ai::providers::all::get_builtin_models;
use kiana_ai::types::{
    Message, Model, ModelThinkingLevel, ProviderEnv, SimpleStreamOptions, ThinkingLevel,
    UserMessage, UserRole,
};
use serde_json::Value;

const TEXT_MODELS: &[&str] = &[
    "MiniMax-M2.5",
    "deepseek-v3.2",
    "deepseek-v4-flash",
    "deepseek-v4-pro",
    "glm-5",
    "glm-5.1",
    "glm-5.2",
    "kimi-k2.5",
    "kimi-k2.6",
    "kimi-k2.7-code",
    "qwen3.6-flash",
    "qwen3.6-plus",
    "qwen3.7-max",
    "qwen3.7-plus",
    "qwen3.8-flash",
    "qwen3.8-max",
];

const INDIVIDUAL_TEXT_MODELS: &[&str] = &[
    "deepseek-v4-flash-0731",
    "deepseek-v4-pro",
    "deepseek-v4-pro-0813",
    "glm-5.2",
    "qwen3.6-flash",
    "qwen3.7-max",
    "qwen3.7-plus",
    "qwen3.8-flash",
    "qwen3.8-max",
];

const IMAGE_MODELS: &[&str] = &[
    "qwen-image-2.0",
    "qwen-image-2.0-pro",
    "wan2.7-image",
    "wan2.7-image-pro",
];

const QWEN_THINKING_MODELS: &[&str] = &[
    "deepseek-v3.2",
    "deepseek-v4-flash",
    "deepseek-v4-pro",
    "glm-5",
    "glm-5.1",
    "glm-5.2",
    "kimi-k2.5",
    "kimi-k2.6",
    "kimi-k2.7-code",
    "qwen3.6-flash",
    "qwen3.6-plus",
    "qwen3.7-max",
    "qwen3.7-plus",
    "qwen3.8-flash",
    "qwen3.8-max",
];

const QWEN_REASONING_EFFORT_MODELS: &[&str] = &[
    "deepseek-v4-flash",
    "deepseek-v4-pro",
    "glm-5",
    "glm-5.1",
    "glm-5.2",
];
const QWEN38_MODELS: &[&str] = &["qwen3.8-flash", "qwen3.8-max"];

fn find_model(provider: &str, id: &str) -> Model {
    get_builtin_models(provider)
        .into_iter()
        .find(|model| model.id == id)
        .unwrap_or_else(|| panic!("missing model: {provider}/{id}"))
}

fn model_ids(provider: &str) -> Vec<String> {
    get_builtin_models(provider)
        .into_iter()
        .map(|model| model.id)
        .collect()
}

fn user_context() -> kiana_ai::types::TranscriptContext {
    context(
        None,
        vec![Message::User(UserMessage {
            role: UserRole::User,
            content: kiana_ai::types::UserContent::Text("Hi".into()),
            timestamp: 0,
        })],
    )
}

async fn capture_payload(model: &Model, reasoning: ThinkingLevel) -> Value {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_callback = captured.clone();
    let fetch = MockHttpClient::fixed(MockResponse::json(400, "{}"));
    let mut options = SimpleStreamOptions {
        base: harness::stream_options(fetch),
        reasoning: Some(reasoning),
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
fn exposes_exactly_the_documented_individual_text_models() {
    let mut ids = model_ids("qwen-token-plan-individual");
    ids.sort();
    let mut expected: Vec<String> = INDIVIDUAL_TEXT_MODELS
        .iter()
        .map(|id| (*id).into())
        .collect();
    expected.sort();
    assert_eq!(ids, expected);
}

#[test]
fn reuses_the_international_token_plan_environment_variable() {
    let env: ProviderEnv = [("QWEN_TOKEN_PLAN_API_KEY".to_string(), "test".to_string())]
        .into_iter()
        .collect();
    assert_eq!(
        find_env_keys("qwen-token-plan-individual", Some(&env)),
        Some(vec!["QWEN_TOKEN_PLAN_API_KEY"])
    );
}

#[test]
fn exposes_all_text_models() {
    for provider in ["qwen-token-plan", "qwen-token-plan-cn"] {
        let ids = model_ids(provider);
        for expected in TEXT_MODELS {
            assert!(
                ids.iter().any(|id| id == expected),
                "{provider} should include {expected}"
            );
        }
    }
}

#[test]
fn omits_image_models() {
    for provider in ["qwen-token-plan", "qwen-token-plan-cn"] {
        let ids = model_ids(provider);
        for excluded in IMAGE_MODELS {
            assert!(
                !ids.iter().any(|id| id == excluded),
                "{provider} should not include {excluded}"
            );
        }
    }
}

#[tokio::test]
async fn sends_qwen_thinking_fields() {
    let mut cases: Vec<(&str, &str)> = Vec::new();
    for provider in ["qwen-token-plan", "qwen-token-plan-cn"] {
        for id in QWEN_THINKING_MODELS {
            cases.push((provider, id));
        }
    }
    for id in INDIVIDUAL_TEXT_MODELS {
        cases.push(("qwen-token-plan-individual", id));
    }

    for (provider, model_id) in cases {
        let model = find_model(provider, model_id);
        let payload = capture_payload(&model, ThinkingLevel::High).await;
        assert_eq!(
            payload["enable_thinking"],
            Value::Bool(true),
            "{provider}/{model_id}"
        );
        assert!(
            payload.get("thinking").is_none(),
            "{provider}/{model_id} must not send thinking"
        );
    }
}

#[test]
fn exposes_qwen_reasoning_effort_levels() {
    let check = |model: &Model| {
        let map = model
            .thinking_level_map
            .as_ref()
            .expect("thinking level map");
        assert_eq!(map.get(&ModelThinkingLevel::Minimal), Some(&None));
        assert_eq!(map.get(&ModelThinkingLevel::Low), Some(&None));
        assert_eq!(map.get(&ModelThinkingLevel::Medium), Some(&None));
        assert_eq!(
            map.get(&ModelThinkingLevel::High),
            Some(&Some("high".to_string()))
        );
        assert_eq!(map.get(&ModelThinkingLevel::Xhigh), Some(&None));
        assert_eq!(
            map.get(&ModelThinkingLevel::Max),
            Some(&Some("max".to_string()))
        );
    };
    for provider in ["qwen-token-plan", "qwen-token-plan-cn"] {
        for id in QWEN_REASONING_EFFORT_MODELS {
            check(&find_model(provider, id));
        }
    }
    for id in [
        "deepseek-v4-flash-0731",
        "deepseek-v4-pro",
        "deepseek-v4-pro-0813",
        "glm-5.2",
    ] {
        check(&find_model("qwen-token-plan-individual", id));
    }
}

#[test]
fn exposes_qwen3_8_reasoning_effort_levels() {
    for provider in [
        "qwen-token-plan",
        "qwen-token-plan-cn",
        "qwen-token-plan-individual",
    ] {
        for id in QWEN38_MODELS {
            let model = find_model(provider, id);
            let map = model
                .thinking_level_map
                .as_ref()
                .expect("thinking level map");
            assert_eq!(map.get(&ModelThinkingLevel::Minimal), Some(&None));
            assert_eq!(
                map.get(&ModelThinkingLevel::Low),
                Some(&Some("low".to_string()))
            );
            assert_eq!(
                map.get(&ModelThinkingLevel::Medium),
                Some(&Some("medium".to_string()))
            );
            assert_eq!(map.get(&ModelThinkingLevel::High), Some(&None));
            assert_eq!(
                map.get(&ModelThinkingLevel::Xhigh),
                Some(&Some("xhigh".to_string()))
            );
            assert_eq!(map.get(&ModelThinkingLevel::Max), Some(&None));
        }
    }
}

#[test]
fn omits_retired_qwen3_8_max_preview() {
    for provider in [
        "qwen-token-plan",
        "qwen-token-plan-cn",
        "qwen-token-plan-individual",
    ] {
        let ids = model_ids(provider);
        assert!(
            !ids.iter().any(|id| id == "qwen3.8-max-preview"),
            "{provider}"
        );
    }
}

#[tokio::test]
async fn sends_qwen_reasoning_effort() {
    let mut cases: Vec<(&str, &str)> = Vec::new();
    for provider in ["qwen-token-plan", "qwen-token-plan-cn"] {
        for id in QWEN_REASONING_EFFORT_MODELS {
            cases.push((provider, id));
        }
    }
    for id in [
        "deepseek-v4-flash-0731",
        "deepseek-v4-pro",
        "deepseek-v4-pro-0813",
        "glm-5.2",
    ] {
        cases.push(("qwen-token-plan-individual", id));
    }

    for (provider, model_id) in cases {
        let model = find_model(provider, model_id);
        let payload = capture_payload(&model, ThinkingLevel::High).await;
        assert_eq!(
            payload["reasoning_effort"],
            Value::String("high".into()),
            "{provider}/{model_id}"
        );
    }
}

#[tokio::test]
async fn sends_qwen3_8_xhigh_reasoning_effort() {
    for provider in [
        "qwen-token-plan",
        "qwen-token-plan-cn",
        "qwen-token-plan-individual",
    ] {
        for id in QWEN38_MODELS {
            let model = find_model(provider, id);
            let payload = capture_payload(&model, ThinkingLevel::Xhigh).await;
            assert_eq!(
                payload["enable_thinking"],
                Value::Bool(true),
                "{provider}/{id}"
            );
            assert_eq!(
                payload["reasoning_effort"],
                Value::String("xhigh".into()),
                "{provider}/{id}"
            );
            assert!(
                payload.get("thinking").is_none(),
                "{provider}/{id} must not send thinking"
            );
        }
    }
}
