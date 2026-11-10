//! `xai-responses.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use std::sync::Arc;

use kiana_ai::api::openai_responses::OpenAIResponsesOptions;
use kiana_ai::types::{AssistantMessage, Context, Model, StreamOptions};
use serde_json::{Value, json};

const KIANA_USER_AGENT: fn() -> String = || kiana_ai::utils::identity::format_user_agent(None);

fn completed_sse() -> mock_http::MockResponse {
    mock_http::MockResponse::sse(&[json!({
        "type": "response.completed",
        "sequence_number": 0,
        "response": {
            "id": "resp_xai_test",
            "status": "completed",
            "output": [],
            "usage": {
                "input_tokens": 1,
                "output_tokens": 1,
                "total_tokens": 2,
                "input_tokens_details": { "cached_tokens": 0 },
            },
        },
    })])
}

fn xai_model(id: &str) -> Model {
    mock_http::catalog_model("xai", id)
}

struct Captured {
    url: String,
    headers: mock_http::MockHttp,
    body: Value,
}

async fn capture_request(
    model: &Model,
    context: Context,
    options: OpenAIResponsesOptions,
) -> Captured {
    let mock = mock_http::MockHttp::single(completed_sse());
    let options = OpenAIResponsesOptions {
        base: StreamOptions {
            fetch: Some(Arc::new(mock.clone())),
            ..options.base
        },
        ..options
    };
    let message: AssistantMessage =
        mock_http::run_responses(model, &mock_http::context_with(&context), options).await;
    assert_eq!(
        message.stop_reason,
        kiana_ai::types::StopReason::Stop,
        "{:?}",
        message.error_message
    );
    Captured {
        url: mock.last_request().url,
        body: mock.last_body(),
        headers: mock,
    }
}

#[test]
fn excludes_retired_and_redundant_models_from_the_builtin_catalog() {
    for model_id in [
        "grok-3",
        "grok-3-fast",
        "grok-4.20-0309-non-reasoning",
        "grok-4.20-0309-reasoning",
        "grok-build-0.1",
        "grok-code-fast-1",
    ] {
        assert!(
            kiana_ai::providers::catalog::get_model("xai", model_id).is_none(),
            "{model_id}"
        );
    }
}

#[test]
fn routes_every_builtin_xai_model_through_responses() {
    for value in kiana_ai::providers::catalog::get_models("xai") {
        let model: Model = serde_json::from_value(value.clone()).expect("model");
        assert_eq!(model.api, "openai-responses", "{}", model.id);
    }
    let levels = |id: &str| {
        kiana_ai::models::get_supported_thinking_levels(&xai_model(id))
            .iter()
            .map(|level| format!("{level:?}").to_lowercase())
            .collect::<Vec<_>>()
    };
    assert_eq!(levels("grok-4.5"), ["low", "medium", "high", "xhigh"]);
    assert_eq!(levels("grok-4.6"), ["low", "medium", "high", "xhigh"]);
    assert_eq!(levels("grok-4.7"), ["low", "medium", "high", "xhigh"]);
    assert_eq!(
        levels("grok-4.3"),
        ["off", "low", "medium", "high", "xhigh"]
    );
}

#[test]
fn includes_grok_4_7_capabilities_and_long_context_pricing() {
    let model = xai_model("grok-4.7");
    assert_eq!(model.api, "openai-responses");
    assert_eq!(model.reasoning, Some(true));
    assert_eq!(
        model.input,
        vec![
            kiana_ai::types::ModelInputModality::Text,
            kiana_ai::types::ModelInputModality::Image
        ]
    );
    assert_eq!(model.context_window, Some(500_000));
    assert_eq!(model.max_tokens, Some(500_000));
    assert_eq!(model.cost.input, 2.0);
    assert_eq!(model.cost.output, 6.0);
    assert_eq!(model.cost.cache_read, 0.5);
    let tier = model.cost.tiers.as_ref().unwrap().first().unwrap();
    assert_eq!(tier.input_tokens_above, 200_000.0);
    assert_eq!(tier.input, 4.0);
    assert_eq!(tier.output, 12.0);
    assert_eq!(tier.cache_read, 1.0);
}

#[tokio::test]
async fn uses_responses_with_bearer_auth_and_xai_compatible_request_fields() {
    let captured = capture_request(
        &xai_model("grok-4.5"),
        Context {
            system_prompt: Some("You are a careful coding assistant.".into()),
            messages: vec![mock_http::user("hello")],
            tools: None,
        },
        OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("xai-test-token".into()),
                session_id: Some("kiana-session-123".into()),
                cache_retention: Some(kiana_ai::types::CacheRetention::Long),
                ..Default::default()
            },
            reasoning_effort: Some(kiana_ai::types::ThinkingLevel::Medium),
            ..Default::default()
        },
    )
    .await;

    assert_eq!(captured.url, "https://api.x.ai/v1/responses");
    assert_eq!(
        captured.headers.last_header("authorization").as_deref(),
        Some("Bearer xai-test-token")
    );
    assert_eq!(
        captured.headers.last_header("user-agent").as_deref(),
        Some(KIANA_USER_AGENT().as_str())
    );
    assert_eq!(
        captured.headers.last_header("session_id").as_deref(),
        Some("kiana-session-123")
    );
    assert_eq!(captured.body["model"], json!("grok-4.5"));
    assert_eq!(captured.body["store"], json!(false));
    assert_eq!(captured.body["stream"], json!(true));
    assert_eq!(
        captured.body["prompt_cache_key"],
        json!("kiana-session-123")
    );
    assert_eq!(captured.body["reasoning"]["effort"], json!("medium"));
    assert_eq!(
        captured.body["include"],
        json!(["reasoning.encrypted_content"])
    );
    assert!(captured.body.get("prompt_cache_retention").is_none());
    assert_eq!(captured.body["input"][0]["role"], json!("developer"));
    assert_eq!(
        captured.body["input"][0]["content"],
        json!("You are a careful coding assistant.")
    );
}

#[tokio::test]
async fn requests_encrypted_reasoning_without_an_effort_override() {
    let captured = capture_request(
        &xai_model("grok-4.5"),
        Context {
            messages: vec![mock_http::user("hello")],
            ..Context::default()
        },
        OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("xai-test-token".into()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert_eq!(captured.body["model"], json!("grok-4.5"));
    assert_eq!(captured.body["store"], json!(false));
    assert_eq!(
        captured.body["include"],
        json!(["reasoning.encrypted_content"])
    );
    assert!(captured.body.get("reasoning").is_none());
}

#[tokio::test]
async fn uses_responses_for_grok_4_7_xhigh_and_grok_4_3_low() {
    let captured = capture_request(
        &xai_model("grok-4.7"),
        Context {
            messages: vec![mock_http::user("hello")],
            ..Context::default()
        },
        OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("xai-test-token".into()),
                ..Default::default()
            },
            reasoning_effort: Some(kiana_ai::types::ThinkingLevel::Xhigh),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(captured.url, "https://api.x.ai/v1/responses");
    assert_eq!(captured.body["reasoning"]["effort"], json!("xhigh"));
    assert_eq!(
        captured.body["include"],
        json!(["reasoning.encrypted_content"])
    );

    let captured = capture_request(
        &xai_model("grok-4.3"),
        Context {
            messages: vec![mock_http::user("hello")],
            ..Context::default()
        },
        OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("xai-test-token".into()),
                ..Default::default()
            },
            reasoning_effort: Some(kiana_ai::types::ThinkingLevel::Low),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(captured.url, "https://api.x.ai/v1/responses");
    assert_eq!(captured.body["store"], json!(false));
    assert_eq!(
        captured.body["include"],
        json!(["reasoning.encrypted_content"])
    );
    assert_eq!(captured.body["reasoning"]["effort"], json!("low"));
}

#[tokio::test]
async fn user_agent_defaults_and_overrides_for_responses_and_completions() {
    let mut model = xai_model("grok-4.5");
    model.provider = "openai".into();
    model.base_url = "https://api.openai.com/v1".into();
    let mock = mock_http::MockHttp::single(completed_sse());
    let message = mock_http::run_responses(
        &model,
        &mock_http::text_context("hello"),
        OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("test-token".into()),
                fetch: Some(Arc::new(mock.clone())),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert_eq!(message.stop_reason, kiana_ai::types::StopReason::Stop);
    assert_eq!(
        mock.last_header("user-agent").as_deref(),
        Some(KIANA_USER_AGENT().as_str())
    );

    let mut headers = kiana_ai::types::ProviderHeaders::new();
    headers.insert("User-Agent".into(), Some("custom-agent".into()));
    let captured = capture_request(
        &xai_model("grok-4.5"),
        Context {
            messages: vec![mock_http::user("hello")],
            ..Context::default()
        },
        OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("xai-test-token".into()),
                headers: Some(headers.clone()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        captured.headers.last_header("user-agent").as_deref(),
        Some("custom-agent")
    );

    // Completions 侧。
    let mut completions_model = Model {
        id: "grok-custom".into(),
        name: "Grok Custom".into(),
        api: "openai-completions".into(),
        provider: "xai".into(),
        base_url: "https://api.x.ai/v1".into(),
        reasoning: Some(false),
        input: vec![kiana_ai::types::ModelInputModality::Text],
        context_window: Some(128_000),
        max_tokens: Some(16_384),
        ..Model::default()
    };
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[
        json!({ "id": "chatcmpl-ua", "choices": [{ "delta": { "content": "ok" }, "finish_reason": null, "index": 0 }] }),
        json!({
            "id": "chatcmpl-ua",
            "choices": [{ "delta": {}, "finish_reason": "stop", "index": 0 }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 0 },
            },
        }),
    ]));
    let result = mock_http::run_completions(
        &completions_model,
        &mock_http::text_context("hello"),
        kiana_ai::api::openai_completions::OpenAICompletionsOptions {
            base: StreamOptions {
                api_key: Some("xai-test-token".into()),
                fetch: Some(Arc::new(mock.clone())),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert_eq!(result.stop_reason, kiana_ai::types::StopReason::Stop);
    assert_eq!(
        mock.last_header("user-agent").as_deref(),
        Some(KIANA_USER_AGENT().as_str())
    );

    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[json!({
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
    })]));
    let mut headers = kiana_ai::types::ProviderHeaders::new();
    headers.insert("User-Agent".into(), Some("custom-agent".into()));
    completions_model.compat = None;
    let _ = mock_http::run_completions(
        &completions_model,
        &mock_http::text_context("hello"),
        kiana_ai::api::openai_completions::OpenAICompletionsOptions {
            base: StreamOptions {
                api_key: Some("xai-test-token".into()),
                fetch: Some(Arc::new(mock.clone())),
                headers: Some(headers),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        mock.last_header("user-agent").as_deref(),
        Some("custom-agent")
    );
}

#[tokio::test]
async fn routes_every_builtin_xai_model_through_the_responses_api_via_the_provider_factory() {
    let provider = kiana_ai::providers::xai::xai_provider();
    let models = provider.get_models();
    assert!(!models.is_empty());
    for model in &models {
        assert_eq!(model.api, "openai-responses", "{}", model.id);
    }

    // provider 工厂的 api map 把 responses 模型分派到 responses 适配器。
    let mock = mock_http::MockHttp::single(completed_sse());
    let model = xai_model("grok-4.5");
    let message = provider
        .stream(
            &model,
            &mock_http::text_context("hello"),
            StreamOptions {
                api_key: Some("xai-test-token".into()),
                fetch: Some(Arc::new(mock.clone())),
                ..Default::default()
            },
        )
        .result()
        .await
        .expect("stream result");
    assert_eq!(
        message.stop_reason,
        kiana_ai::types::StopReason::Stop,
        "{:?}",
        message.error_message
    );
    assert!(
        mock.last_request().url.ends_with("/responses"),
        "{}",
        mock.last_request().url
    );
}
