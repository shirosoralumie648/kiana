//! `openai-completions-thinking-token-budget.test.ts`。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use kiana_ai::types::{
    Context, Model, ModelInputModality, SimpleStreamOptions, StreamOptions, ThinkingBudgets,
    ThinkingLevel, TranscriptContext, UserContent, UserMessage, UserRole,
};
use serde_json::{Value, json};

fn vllm_model(compat: Value) -> Model {
    Model {
        id: "zai-org/glm-5.2".into(),
        name: "GLM 5.2 (local vLLM)".into(),
        api: "openai-completions".into(),
        provider: "local-vllm".into(),
        base_url: "http://localhost:8000/v1".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        context_window: Some(262_144),
        max_tokens: Some(16_384),
        compat: Some(compat),
        ..Model::default()
    }
}

fn default_compat() -> Value {
    json!({ "thinkingFormat": "zai", "supportsThinkingTokenBudget": true })
}

fn context() -> TranscriptContext {
    TranscriptContext::from_normalized(Context {
        messages: vec![kiana_ai::types::Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("Hi".into()),
            timestamp: 1,
        })],
        ..Context::default()
    })
}

async fn capture(
    model: Model,
    reasoning: Option<ThinkingLevel>,
    thinking_budgets: Option<ThinkingBudgets>,
    max_tokens: Option<u64>,
) -> Value {
    let mock = mock_http::MockHttp::single(mock_http::MockResponse::sse(&[json!({
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        "usage": {
            "prompt_tokens": 1,
            "completion_tokens": 1,
            "prompt_tokens_details": { "cached_tokens": 0 },
            "completion_tokens_details": { "reasoning_tokens": 0 },
        },
    })]));
    let options = SimpleStreamOptions {
        base: StreamOptions {
            api_key: Some("test".into()),
            fetch: Some(std::sync::Arc::new(mock.clone())),
            max_tokens,
            ..StreamOptions::default()
        },
        reasoning,
        thinking_budgets,
        ..SimpleStreamOptions::default()
    };
    let _ = mock_http::run_completions_simple(&model, &context(), options).await;
    mock.last_body()
}

#[tokio::test]
async fn sends_the_configured_budget_for_the_requested_level() {
    let params = capture(
        vllm_model(default_compat()),
        Some(ThinkingLevel::Medium),
        Some(ThinkingBudgets {
            medium: Some(4096),
            ..Default::default()
        }),
        None,
    )
    .await;
    assert_eq!(params.get("thinking_token_budget"), Some(&json!(4096)));
}

#[tokio::test]
async fn omits_the_budget_when_neither_the_field_nor_the_alias_is_set() {
    let params = capture(
        vllm_model(json!({ "thinkingFormat": "zai" })),
        Some(ThinkingLevel::Medium),
        Some(ThinkingBudgets {
            medium: Some(4096),
            ..Default::default()
        }),
        None,
    )
    .await;
    for field in [
        "thinking_token_budget",
        "thinking_budget",
        "thinking_budget_tokens",
    ] {
        assert!(params.get(field).is_none(), "{field}");
    }
}

#[tokio::test]
async fn omits_the_budget_when_thinking_is_off() {
    let params = capture(
        vllm_model(default_compat()),
        None,
        Some(ThinkingBudgets {
            high: Some(8192),
            ..Default::default()
        }),
        None,
    )
    .await;
    assert!(params.get("thinking_token_budget").is_none());
}

#[tokio::test]
async fn clamps_xhigh_and_max_to_the_high_budget() {
    for level in [ThinkingLevel::Xhigh, ThinkingLevel::Max] {
        let params = capture(
            vllm_model(default_compat()),
            Some(level),
            Some(ThinkingBudgets {
                high: Some(8192),
                ..Default::default()
            }),
            None,
        )
        .await;
        assert_eq!(
            params.get("thinking_token_budget"),
            Some(&json!(8192)),
            "{level:?}"
        );
    }
}

#[tokio::test]
async fn leaves_room_for_the_answer_when_the_budget_meets_the_response_ceiling() {
    let params = capture(
        vllm_model(default_compat()),
        Some(ThinkingLevel::High),
        None,
        None,
    )
    .await;
    assert_eq!(
        params.get("thinking_token_budget"),
        Some(&json!(16_384 - 1024))
    );
}

#[tokio::test]
async fn uses_the_caller_max_tokens_as_the_ceiling_when_it_is_lower_than_the_model_cap() {
    let params = capture(
        vllm_model(default_compat()),
        Some(ThinkingLevel::High),
        Some(ThinkingBudgets {
            high: Some(8192),
            ..Default::default()
        }),
        Some(4096),
    )
    .await;
    assert_eq!(
        params.get("thinking_token_budget"),
        Some(&json!(4096 - 1024))
    );
}

#[tokio::test]
async fn sends_thinking_budget_fields_when_thinking_token_budget_field_is_set() {
    for field in ["thinking_budget", "thinking_budget_tokens"] {
        let params = capture(
            vllm_model(json!({ "thinkingFormat": "qwen", "thinkingTokenBudgetField": field })),
            Some(ThinkingLevel::Medium),
            Some(ThinkingBudgets {
                medium: Some(4096),
                ..Default::default()
            }),
            None,
        )
        .await;
        assert_eq!(params.get(field), Some(&json!(4096)), "{field}");
        assert!(params.get("thinking_token_budget").is_none());
    }
}

#[tokio::test]
async fn lets_thinking_token_budget_field_win_over_the_boolean_alias() {
    let params = capture(
        vllm_model(json!({
            "thinkingFormat": "zai",
            "supportsThinkingTokenBudget": true,
            "thinkingTokenBudgetField": "thinking_budget",
        })),
        Some(ThinkingLevel::Medium),
        Some(ThinkingBudgets {
            medium: Some(4096),
            ..Default::default()
        }),
        None,
    )
    .await;
    assert_eq!(params.get("thinking_budget"), Some(&json!(4096)));
    assert!(params.get("thinking_token_budget").is_none());
}

#[tokio::test]
async fn puts_the_clamped_budget_in_chat_template_kwargs_when_var_is_thinking_budget() {
    let params = capture(
        vllm_model(json!({
            "thinkingFormat": "chat-template",
            "chatTemplateKwargs": {
                "enable_thinking": { "$var": "thinking.enabled" },
                "thinking_budget": { "$var": "thinking.budget" },
            },
        })),
        Some(ThinkingLevel::High),
        None,
        None,
    )
    .await;
    assert_eq!(
        params.get("chat_template_kwargs"),
        Some(&json!({ "enable_thinking": true, "thinking_budget": 16_384 - 1024 }))
    );
    assert!(params.get("thinking_token_budget").is_none());
}

#[tokio::test]
async fn omits_thinking_budget_from_chat_template_kwargs_when_thinking_is_off() {
    let params = capture(
        vllm_model(json!({
            "thinkingFormat": "chat-template",
            "chatTemplateKwargs": {
                "enable_thinking": { "$var": "thinking.enabled" },
                "thinking_budget": { "$var": "thinking.budget" },
            },
        })),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(
        params.get("chat_template_kwargs"),
        Some(&json!({ "enable_thinking": false }))
    );
}
