//! `openai-completions-empty-tools.test.ts`（adapter 层用例）。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::auth::types::AuthContext;
use kiana_ai::models::{
    CreateModelsOptions, ModelsSimpleStreamOptions, MutableModels, create_models,
};
use kiana_ai::providers::cloudflare_ai_gateway::cloudflare_ai_gateway_provider;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, Context, Message, ProviderHeaders,
    SimpleStreamOptions, StopReason, ThinkingLevel, ToolCall, TranscriptContext, UserContentBlock,
};
use serde_json::{Value, json};

fn model() -> kiana_ai::types::Model {
    let mut model = mock_http::catalog_model("openai", "gpt-4o-mini");
    model.api = "openai-completions".into();
    model.compat = None;
    model
}

fn sse() -> mock_http::MockResponse {
    mock_http::MockResponse::sse(&[json!({
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        "usage": {
            "prompt_tokens": 1,
            "completion_tokens": 1,
            "prompt_tokens_details": { "cached_tokens": 0 },
            "completion_tokens_details": { "reasoning_tokens": 0 },
        },
    })])
}

async fn capture(
    model: &kiana_ai::types::Model,
    context: &TranscriptContext,
    options: SimpleStreamOptions,
) -> Value {
    let mock = mock_http::MockHttp::single(sse());
    let mut options = options;
    options.base.fetch = Some(std::sync::Arc::new(mock.clone()));
    options.base.api_key = Some("test".into());
    let _ = mock_http::run_completions_simple(model, context, options).await;
    mock.last_body()
}

#[tokio::test]
async fn omits_tools_field_when_context_tools_is_an_empty_array() {
    let context = mock_http::context_with(&Context {
        messages: vec![mock_http::user("hi")],
        tools: Some(Vec::new()),
        ..Context::default()
    });
    let params = capture(&model(), &context, SimpleStreamOptions::default()).await;
    assert!(params.get("tools").is_none());
}

#[tokio::test]
async fn omits_tools_field_when_context_tools_is_undefined() {
    let params = capture(
        &model(),
        &mock_http::text_context("hi"),
        SimpleStreamOptions::default(),
    )
    .await;
    assert!(params.get("tools").is_none());
}

#[tokio::test]
async fn sends_default_and_explicit_max_tokens() {
    let model = model();
    let params = capture(
        &model,
        &mock_http::text_context("hi"),
        SimpleStreamOptions::default(),
    )
    .await;
    assert!(params.get("max_tokens").is_none());
    assert_eq!(
        params.get("max_completion_tokens"),
        Some(&json!(model.max_tokens.unwrap()))
    );

    let params = capture(
        &model,
        &mock_http::text_context("hi"),
        SimpleStreamOptions {
            base: kiana_ai::types::StreamOptions {
                max_tokens: Some(1234),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert!(params.get("max_tokens").is_none());
    assert_eq!(params.get("max_completion_tokens"), Some(&json!(1234)));
}

#[tokio::test]
async fn clamps_default_and_explicit_max_tokens_to_remaining_context() {
    let mut model = model();
    model.context_window = Some(10_000);
    model.max_tokens = Some(8000);
    let context = mock_http::text_context(&"x".repeat(8000));

    let params = capture(&model, &context, SimpleStreamOptions::default()).await;
    assert_eq!(params.get("max_completion_tokens"), Some(&json!(3618)));

    let params = capture(
        &model,
        &context,
        SimpleStreamOptions {
            base: kiana_ai::types::StreamOptions {
                max_tokens: Some(7000),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert_eq!(params.get("max_completion_tokens"), Some(&json!(3618)));
}

#[tokio::test]
async fn still_emits_empty_tools_for_anthropic_litellm_proxy_when_conversation_has_tool_history() {
    let assistant = AssistantMessage {
        role: Default::default(),
        content: vec![AssistantContentBlock::ToolCall(ToolCall {
            id: "t1".into(),
            name: "noop".into(),
            arguments: Default::default(),
            ..Default::default()
        })],
        api: "openai-completions".into(),
        provider: "openai".into(),
        model: "gpt-4o-mini".into(),
        usage: Default::default(),
        stop_reason: StopReason::ToolUse,
        timestamp: 2,
        ..Default::default()
    };
    let tool_result = kiana_ai::types::ToolResultMessage {
        role: Default::default(),
        tool_call_id: "t1".into(),
        tool_name: "noop".into(),
        content: vec![UserContentBlock::Text(kiana_ai::types::TextContent {
            text: "done".into(),
            ..Default::default()
        })],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 3,
        duration_ms: None,
    };
    let context = mock_http::context_with(&Context {
        messages: vec![
            mock_http::user("use the tool"),
            Message::Assistant(assistant),
            Message::ToolResult(tool_result),
        ],
        tools: Some(Vec::new()),
        ..Context::default()
    });
    let params = capture(&model(), &context, SimpleStreamOptions::default()).await;
    assert_eq!(params.get("tools"), Some(&json!([])));
}

/// `AuthContext` 的进程内假实现（TS 测试用 `process.env`；Rust 侧 `Models` 通过
/// `AuthContext` 读取 provider env，避免进程级环境变量竞态）。
struct EnvAuthContext {
    env: IndexMap<String, String>,
}

#[async_trait]
impl AuthContext for EnvAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }

    async fn file_exists(&self, _path: &str) -> bool {
        false
    }
}

fn cloudflare_models(env_pairs: &[(&str, &str)]) -> Arc<dyn MutableModels> {
    let models = create_models(CreateModelsOptions {
        auth_context: Some(Arc::new(EnvAuthContext {
            env: env_pairs
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
        })),
        ..Default::default()
    });
    models.set_provider(cloudflare_ai_gateway_provider());
    models
}

/// `getModel("cloudflare-ai-gateway", id)` + `streamSimple(...)`，返回记录的请求。
fn responses_sse() -> mock_http::MockResponse {
    mock_http::MockResponse::sse(&[json!({
        "type": "response.completed",
        "sequence_number": 0,
        "response": {
            "id": "resp_cloudflare_test",
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

async fn run_cloudflare(
    model_id: &str,
    context: &Context,
    options: SimpleStreamOptions,
) -> mock_http::MockHttp {
    let model = mock_http::catalog_model("cloudflare-ai-gateway", model_id);
    let response = if model.api == "openai-responses" {
        responses_sse()
    } else {
        sse()
    };
    let mock = mock_http::MockHttp::single(response);
    let mut options = options;
    options.base.fetch = Some(Arc::new(mock.clone()));
    let models = cloudflare_models(&[
        ("CLOUDFLARE_API_KEY", "cf-token"),
        ("CLOUDFLARE_ACCOUNT_ID", "account-id"),
        ("CLOUDFLARE_GATEWAY_ID", "gateway-id"),
    ]);
    let message = models
        .stream_simple(&model, context, ModelsSimpleStreamOptions::from(options))
        .result()
        .await
        .expect("stream result");
    assert_eq!(
        message.stop_reason,
        StopReason::Stop,
        "{:?}",
        message.error_message
    );
    mock
}

#[tokio::test]
async fn uses_conservative_openai_compatible_fields_for_cloudflare_ai_gateway_compat_models() {
    let context = Context {
        system_prompt: Some("You are helpful.".into()),
        messages: vec![mock_http::user("hi")],
        tools: None,
    };
    let mock = run_cloudflare(
        "workers-ai/@cf/moonshotai/kimi-k2.6",
        &context,
        SimpleStreamOptions {
            base: kiana_ai::types::StreamOptions {
                max_tokens: Some(1234),
                ..Default::default()
            },
            reasoning: Some(ThinkingLevel::High),
            ..Default::default()
        },
    )
    .await;

    let params = mock.last_body();
    assert_eq!(params["messages"][0]["role"], json!("system"));
    assert_eq!(params["max_tokens"], json!(1234));
    assert!(params.get("max_completion_tokens").is_none());
    assert!(params.get("reasoning_effort").is_none());
    assert!(params.get("store").is_none());

    assert_eq!(
        mock.last_request().url,
        "https://gateway.ai.cloudflare.com/v1/account-id/gateway-id/compat/chat/completions"
    );
    assert!(
        mock.last_header("authorization").is_none(),
        "{:?}",
        mock.last_request().headers
    );
    assert_eq!(
        mock.last_header("cf-aig-authorization").as_deref(),
        Some("Bearer cf-token")
    );
}

#[tokio::test]
async fn resolves_cloudflare_ai_gateway_base_url_through_provider_auth() {
    let context = Context {
        messages: vec![mock_http::user("hi")],
        ..Context::default()
    };
    let mock = run_cloudflare(
        "workers-ai/@cf/moonshotai/kimi-k2.6",
        &context,
        SimpleStreamOptions::default(),
    )
    .await;

    assert_eq!(
        mock.last_request().url,
        "https://gateway.ai.cloudflare.com/v1/account-id/gateway-id/compat/chat/completions"
    );
}

#[tokio::test]
async fn preserves_inline_upstream_authorization_for_cloudflare_ai_gateway_byok_requests() {
    let context = Context {
        messages: vec![mock_http::user("hi")],
        ..Context::default()
    };
    let mut headers = ProviderHeaders::new();
    headers.insert("Authorization".into(), Some("Bearer upstream-token".into()));
    let mock = run_cloudflare(
        "gpt-5.1",
        &context,
        SimpleStreamOptions {
            base: kiana_ai::types::StreamOptions {
                headers: Some(headers),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;

    assert_eq!(
        mock.last_header("authorization").as_deref(),
        Some("Bearer upstream-token")
    );
    assert_eq!(
        mock.last_header("cf-aig-authorization").as_deref(),
        Some("Bearer cf-token")
    );
}

#[tokio::test]
async fn sends_session_affinity_headers_for_workers_ai_through_cloudflare_ai_gateway() {
    let context = Context {
        messages: vec![mock_http::user("hi")],
        ..Context::default()
    };
    let mock = run_cloudflare(
        "workers-ai/@cf/moonshotai/kimi-k2.6",
        &context,
        SimpleStreamOptions {
            base: kiana_ai::types::StreamOptions {
                session_id: Some("session-1".into()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;

    assert_eq!(mock.last_header("session_id").as_deref(), Some("session-1"));
    assert_eq!(
        mock.last_header("x-client-request-id").as_deref(),
        Some("session-1")
    );
    assert_eq!(
        mock.last_header("x-session-affinity").as_deref(),
        Some("session-1")
    );
}

#[tokio::test]
async fn omits_stream_options_store_and_max_tokens_field_overrides_when_compat_disables_them() {
    let mut model = model();
    model.compat = Some(json!({
        "supportsUsageInStreaming": false,
        "supportsStore": false,
        "maxTokensField": "max_tokens",
    }));
    let params = capture(
        &model,
        &mock_http::text_context("hi"),
        SimpleStreamOptions::default(),
    )
    .await;
    assert!(params.get("stream_options").is_none());
    assert!(params.get("store").is_none());
    assert_eq!(params["max_tokens"], json!(model.max_tokens.unwrap()));
    assert!(params.get("max_completion_tokens").is_none());

    let params = capture(
        &model,
        &mock_http::text_context("hi"),
        SimpleStreamOptions {
            base: kiana_ai::types::StreamOptions {
                max_tokens: Some(77),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert_eq!(params["max_tokens"], json!(77));
    assert!(params.get("max_completion_tokens").is_none());
}
