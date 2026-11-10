//! `openai-responses-compat.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use std::sync::Arc;

use kiana_ai::api::openai_responses::OpenAIResponsesOptions;
use kiana_ai::types::{
    CacheRetention, Context, Model, ProviderHeaders, SimpleStreamOptions, StreamOptions, Tool,
};
use serde_json::{Value, json};

fn completed_sse() -> mock_http::MockResponse {
    mock_http::MockResponse::sse(&[json!({
        "type": "response.completed",
        "sequence_number": 0,
        "response": { "id": "resp_test", "status": "completed", "usage": { "input_tokens": 1, "output_tokens": 1, "total_tokens": 2 } },
    })])
}

fn capture_options(mock: &mock_http::MockHttp, api_key: &str) -> OpenAIResponsesOptions {
    OpenAIResponsesOptions {
        base: StreamOptions {
            api_key: Some(api_key.into()),
            fetch: Some(Arc::new(mock.clone())),
            ..Default::default()
        },
        ..Default::default()
    }
}

async fn capture(model: &Model, options: OpenAIResponsesOptions) -> (Value, mock_http::MockHttp) {
    let mock = mock_http::MockHttp::single(completed_sse());
    let options = OpenAIResponsesOptions {
        base: StreamOptions {
            fetch: Some(Arc::new(mock.clone())),
            ..options.base
        },
        ..options
    };
    let _ = mock_http::run_responses(model, &mock_http::text_context("hi"), options).await;
    (mock.last_body(), mock)
}

async fn capture_with_context(
    model: &Model,
    context: Context,
    api_key: &str,
    session_id: Option<&str>,
    cache_retention: Option<CacheRetention>,
    headers: Option<ProviderHeaders>,
) -> (Value, mock_http::MockHttp) {
    let mock = mock_http::MockHttp::single(completed_sse());
    let options = OpenAIResponsesOptions {
        base: StreamOptions {
            api_key: Some(api_key.into()),
            fetch: Some(Arc::new(mock.clone())),
            session_id: session_id.map(str::to_string),
            cache_retention,
            headers,
            ..Default::default()
        },
        ..Default::default()
    };
    let _ = mock_http::run_responses(model, &mock_http::context_with(&context), options).await;
    (mock.last_body(), mock)
}

fn ping_tool() -> Tool {
    Tool {
        name: "ping".into(),
        description: "Ping".into(),
        parameters: json!({ "type": "object", "properties": { "value": { "type": "string" } } }),
        constrained_sampling: None,
    }
}

#[tokio::test]
async fn omits_reasoning_when_no_reasoning_is_requested() {
    let model = mock_http::catalog_model("github-copilot", "gpt-6-sol");
    let (payload, _) = capture(
        &model,
        capture_options(&mock_http::MockHttp::default(), "sk-test-key"),
    )
    .await;
    assert!(payload.get("reasoning").is_none(), "{payload}");
}

#[tokio::test]
async fn forwards_required_tool_choice() {
    let model = mock_http::catalog_model("openai", "gpt-5.4");
    let context = Context {
        messages: vec![mock_http::user(
            "Do not call ping. Respond with text instead.",
        )],
        tools: Some(vec![ping_tool()]),
        ..Context::default()
    };
    let mock = mock_http::MockHttp::single(completed_sse());
    let options = OpenAIResponsesOptions {
        base: StreamOptions {
            api_key: Some("sk-test-key".into()),
            fetch: Some(Arc::new(mock.clone())),
            ..Default::default()
        },
        tool_choice: Some(json!("required")),
        ..Default::default()
    };
    let _ = mock_http::run_responses(&model, &mock_http::context_with(&context), options).await;
    let payload = mock.last_body();
    assert_eq!(payload["tool_choice"], json!("required"));
    assert_eq!(payload["input"][0]["type"], json!("additional_tools"));
    assert_eq!(payload["input"][0]["tools"][0]["name"], json!("ping"));
}

#[tokio::test]
async fn sets_strict_mode_explicitly_for_cloudflare_openai_responses_tools() {
    let model = mock_http::catalog_model("cloudflare-ai-gateway", "gpt-5.6-sol");
    assert_eq!(
        model
            .compat
            .as_ref()
            .and_then(|c| c.get("supportsStrictMode")),
        Some(&json!(true))
    );
    let mut constrained = ping_tool();
    constrained.name = "constrained".into();
    constrained.description = "A constrained tool".into();
    constrained.constrained_sampling =
        Some(serde_json::from_value(json!({ "type": "json_schema", "strict": "prefer" })).unwrap());
    let mut ordinary = ping_tool();
    ordinary.name = "ordinary".into();
    ordinary.description = "An ordinary tool".into();
    let context = Context {
        messages: vec![mock_http::user("Use a tool.")],
        tools: Some(vec![ordinary, constrained]),
        ..Context::default()
    };
    let (payload, _) = capture_with_context(&model, context, "sk-test-key", None, None, None).await;
    assert_eq!(payload["tools"][0]["name"], json!("ordinary"));
    assert_eq!(payload["tools"][0]["strict"], json!(false));
    assert_eq!(payload["tools"][1]["name"], json!("constrained"));
    assert_eq!(payload["tools"][1]["strict"], json!(true));
}

#[tokio::test]
async fn sends_none_reasoning_effort_for_openai_models_without_reasoning_requested() {
    for model_id in [
        "gpt-5.1",
        "gpt-5.2",
        "gpt-5.3-codex",
        "gpt-5.4",
        "gpt-5.4-mini",
        "gpt-5.4-nano",
        "gpt-5.5",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-6-sol",
        "gpt-6-luna",
    ] {
        let model = mock_http::catalog_model("openai", model_id);
        let (payload, _) = capture(
            &model,
            capture_options(&mock_http::MockHttp::default(), "sk-test-key"),
        )
        .await;
        assert_eq!(payload["reasoning"]["effort"], json!("none"), "{model_id}");
    }
}

#[tokio::test]
async fn omits_reasoning_effort_for_openai_models_where_off_is_unsupported() {
    for model_id in [
        "gpt-5",
        "gpt-5-mini",
        "gpt-5-nano",
        "gpt-5-pro",
        "gpt-5.2-pro",
        "gpt-5.4-pro",
        "gpt-5.5-pro",
    ] {
        let model = mock_http::catalog_model("openai", model_id);
        let (payload, _) = capture(
            &model,
            capture_options(&mock_http::MockHttp::default(), "sk-test-key"),
        )
        .await;
        assert!(payload.get("reasoning").is_none(), "{model_id}");
    }
}

#[tokio::test]
async fn sets_cache_affinity_headers_and_clamps_prompt_cache_key() {
    let model = mock_http::catalog_model("openai", "gpt-5.4");
    let (payload, mock) = capture_with_context(
        &model,
        Context::default(),
        "sk-test-key",
        Some("session-123"),
        None,
        None,
    )
    .await;
    assert_eq!(
        mock.last_header("session_id").as_deref(),
        Some("session-123")
    );
    assert_eq!(
        mock.last_header("x-client-request-id").as_deref(),
        Some("session-123")
    );

    let (payload2, _) = capture_with_context(
        &model,
        Context::default(),
        "sk-test-key",
        Some(&"x".repeat(67)),
        None,
        None,
    )
    .await;
    assert_eq!(payload2["prompt_cache_key"], json!("x".repeat(64)));
    let _ = payload;
}

#[tokio::test]
async fn sets_cache_affinity_headers_for_proxy_requests() {
    let mut model = mock_http::catalog_model("openai", "gpt-5.4");
    model.provider = "opencode".into();
    model.base_url = "https://proxy.example.com/v1".into();
    let (_, mock) = capture_with_context(
        &model,
        Context::default(),
        "sk-test-key",
        Some("session-123"),
        None,
        None,
    )
    .await;
    assert_eq!(
        mock.last_header("session_id").as_deref(),
        Some("session-123")
    );
    assert_eq!(
        mock.last_header("x-client-request-id").as_deref(),
        Some("session-123")
    );
}

#[tokio::test]
async fn uses_openrouter_session_affinity_when_configured_or_auto_detected() {
    let mut configured = mock_http::catalog_model("openai", "gpt-5.4");
    configured.provider = "proxy".into();
    configured.base_url = "https://proxy.example.com/v1".into();
    configured.compat = Some(json!({ "sessionAffinityFormat": "openrouter" }));
    let (payload, mock) = capture_with_context(
        &configured,
        Context::default(),
        "sk-test-key",
        Some("session-proxy"),
        None,
        None,
    )
    .await;
    assert!(mock.last_header("session_id").is_none());
    assert!(mock.last_header("x-client-request-id").is_none());
    assert_eq!(
        mock.last_header("x-session-id").as_deref(),
        Some("session-proxy")
    );
    assert!(payload.get("session_id").is_none());
    assert_eq!(payload["prompt_cache_key"], json!("session-proxy"));

    let mut detected = mock_http::catalog_model("openai", "gpt-5.4");
    detected.provider = "openrouter".into();
    detected.base_url = "https://openrouter.ai/api/v1".into();
    let (payload, mock) = capture_with_context(
        &detected,
        Context::default(),
        "sk-test-key",
        Some("session-openrouter"),
        None,
        None,
    )
    .await;
    assert!(mock.last_header("session_id").is_none());
    assert!(mock.last_header("x-client-request-id").is_none());
    assert_eq!(
        mock.last_header("x-session-id").as_deref(),
        Some("session-openrouter")
    );
    assert_eq!(payload["prompt_cache_key"], json!("session-openrouter"));
}

#[tokio::test]
async fn uses_openai_no_session_format_when_configured() {
    let mut model = mock_http::catalog_model("openai", "gpt-5.4");
    model.provider = "proxy".into();
    model.base_url = "https://proxy.example.com/v1".into();
    model.compat = Some(json!({ "sessionAffinityFormat": "openai-nosession" }));
    let (payload, mock) = capture_with_context(
        &model,
        Context::default(),
        "sk-test-key",
        Some("session-proxy"),
        None,
        None,
    )
    .await;
    assert!(mock.last_header("session_id").is_none());
    assert_eq!(
        mock.last_header("x-client-request-id").as_deref(),
        Some("session-proxy")
    );
    assert!(mock.last_header("x-session-id").is_none());
    assert_eq!(payload["prompt_cache_key"], json!("session-proxy"));

    let opencode = mock_http::catalog_model("opencode", "gpt-5.4");
    assert_eq!(
        opencode
            .compat
            .as_ref()
            .and_then(|c| c.get("sessionAffinityFormat")),
        Some(&json!("openai-nosession"))
    );
    let (payload, mock) = capture_with_context(
        &opencode,
        Context::default(),
        "sk-test-key",
        Some("session-opencode"),
        None,
        None,
    )
    .await;
    assert!(mock.last_header("session_id").is_none());
    assert_eq!(
        mock.last_header("x-client-request-id").as_deref(),
        Some("session-opencode")
    );
    assert_eq!(payload["prompt_cache_key"], json!("session-opencode"));
}

#[tokio::test]
async fn lets_explicit_headers_override_and_omits_affinity_when_cache_retention_is_none() {
    let mut headers = ProviderHeaders::new();
    headers.insert("session_id".into(), Some("override-session".into()));
    headers.insert(
        "x-client-request-id".into(),
        Some("override-request".into()),
    );
    let model = mock_http::catalog_model("openai", "gpt-5.4");
    let (_, mock) = capture_with_context(
        &model,
        Context::default(),
        "sk-test-key",
        Some("session-123"),
        None,
        Some(headers),
    )
    .await;
    assert_eq!(
        mock.last_header("session_id").as_deref(),
        Some("override-session")
    );
    assert_eq!(
        mock.last_header("x-client-request-id").as_deref(),
        Some("override-request")
    );

    let (_, mock) = capture_with_context(
        &model,
        Context::default(),
        "sk-test-key",
        Some("session-123"),
        Some(CacheRetention::None),
        None,
    )
    .await;
    assert!(mock.last_header("session_id").is_none());
    assert!(mock.last_header("x-client-request-id").is_none());
}

#[tokio::test]
async fn applies_service_tier_cost_multipliers() {
    for (model_id, requested, returned, multiplier) in [
        ("gpt-5.4", "priority", "priority", 2.0),
        ("gpt-5.5", "priority", "priority", 2.5),
        ("gpt-5.5", "flex", "flex", 0.5),
        ("gpt-6-luna", "priority", "fast", 2.0),
        ("gpt-6-luna", "fast", "fast", 2.0),
    ] {
        let model = mock_http::catalog_model("openai", model_id);
        let token_count = 100_000.0_f64;
        let token_scale = token_count / 1_000_000.0;
        let event = json!({
            "type": "response.completed",
            "response": {
                "status": "completed",
                "service_tier": returned,
                "usage": {
                    "input_tokens": token_count,
                    "output_tokens": token_count,
                    "total_tokens": token_count * 2.0,
                    "input_tokens_details": { "cached_tokens": 0 },
                },
            },
        });
        let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[event]));
        let options = OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some("sk-test-key".into()),
                fetch: Some(Arc::new(mock)),
                ..Default::default()
            },
            service_tier: Some(requested.into()),
            ..Default::default()
        };
        let result =
            mock_http::run_responses(&model, &mock_http::text_context("hi"), options).await;
        assert!(
            (result.usage.cost.input - model.cost.input * multiplier * token_scale).abs() < 1e-9,
            "{model_id} {requested}"
        );
        assert!(
            (result.usage.cost.output - model.cost.output * multiplier * token_scale).abs() < 1e-9,
            "{model_id} {requested}"
        );
        assert!(
            (result.usage.cost.total
                - (model.cost.input + model.cost.output) * multiplier * token_scale)
                .abs()
                < 1e-9,
            "{model_id} {requested}"
        );
    }
}

#[tokio::test]
async fn sends_max_output_tokens_by_default_and_omits_it_when_unsupported() {
    let model = mock_http::catalog_model("openai", "gpt-5.4");
    let mut options = capture_options(&mock_http::MockHttp::default(), "sk-test-key");
    options.base.max_tokens = Some(1024);
    let (payload, _) = capture(&model, options).await;
    assert_eq!(payload["max_output_tokens"], json!(1024));

    let mut unsupported = model.clone();
    unsupported.compat = Some(json!({ "supportsMaxOutputTokens": false }));
    let mut options = capture_options(&mock_http::MockHttp::default(), "sk-test-key");
    options.base.max_tokens = Some(1024);
    let (payload, _) = capture(&unsupported, options).await;
    assert!(payload.get("max_output_tokens").is_none());
}

/// 保持 `SimpleStreamOptions` 在导入面里（与 TS 的兼容入口一致）。
#[allow(dead_code)]
fn simple_options_marker(options: SimpleStreamOptions) -> SimpleStreamOptions {
    options
}

#[tokio::test]
async fn sends_explicit_prompt_cache_mode_when_retention_is_none() {
    let mut model = mock_http::catalog_model("openai", "gpt-5.6-sol");
    model.compat = Some(json!({ "supportsExplicitPromptCacheMode": true }));
    let (payload, _) = capture_with_context(
        &model,
        Context::default(),
        "sk-test-key",
        Some("session-123"),
        Some(CacheRetention::None),
        None,
    )
    .await;
    assert!(payload.get("prompt_cache_key").is_none());
    assert_eq!(
        payload["prompt_cache_options"],
        json!({ "mode": "explicit" })
    );
}
