//! `packages/ai/test/context-estimate.test.ts`。
//!
//! TS 的第二个断言用 `buildBaseOptions(model, context).maxTokens`，它依赖
//! `api/simple-options.ts` 的 `clampMaxTokensToContext`（不在本单元的所有权范围内）。
//! 该函数只做 `min(maxTokens, max(1, contextWindow - tokens - 4096))`，本文件内联同一算式，
//! 以便对 `estimateContextTokens` 的可观察结果（`tokens`）保持逐值等价。

use kiana_ai::types::{
    Api, AssistantContentBlock, AssistantMessage, AssistantRole, Context, Message, Model,
    ProviderId, StopReason, TextContent, TextKind, Usage, UsageCost, UserContent, UserMessage,
    UserRole,
};
use kiana_ai::utils::estimate::{ContextUsageEstimate, estimate_context_tokens};
use kiana_ai::utils::transcript::normalize_context;

fn create_usage(total_tokens: u64) -> Usage {
    Usage {
        input: total_tokens,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens,
        cost: UsageCost::default(),
    }
}

fn create_assistant(timestamp: i64, total_tokens: u64) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "kept".into(),
            text_signature: None,
        })],
        api: Api::from("openai-responses"),
        provider: ProviderId::from("openai"),
        model: "test-model".into(),
        usage: create_usage(total_tokens),
        stop_reason: StopReason::Stop,
        timestamp,
        ..Default::default()
    }
}

fn user_message(content: &str, timestamp: i64) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(content.into()),
        timestamp,
    })
}

fn model() -> Model {
    Model {
        id: "test-model".into(),
        name: "Test Model".into(),
        api: Api::from("openai-responses"),
        provider: ProviderId::from("openai"),
        base_url: "https://api.openai.com/v1".into(),
        reasoning: Some(false),
        input: vec![kiana_ai::types::ModelInputModality::Text],
        cost: Default::default(),
        context_window: Some(10_000),
        max_tokens: Some(8_000),
        ..Default::default()
    }
}

/// `api/simple-options.ts#clampMaxTokensToContext`（本单元外，内联同一算式）。
fn clamp_max_tokens_to_context(
    model: &Model,
    context: &kiana_ai::types::TranscriptContext,
    max_tokens: u64,
) -> u64 {
    const CONTEXT_SAFETY_TOKENS: u64 = 4096;
    const MIN_MAX_TOKENS: u64 = 1;
    let context_window = model.context_window.unwrap_or(0);
    if context_window == 0 {
        return MIN_MAX_TOKENS.max(max_tokens);
    }
    let available = context_window as i64
        - estimate_context_tokens(context).tokens as i64
        - CONTEXT_SAFETY_TOKENS as i64;
    max_tokens.min(available.max(MIN_MAX_TOKENS as i64) as u64)
}

fn base_options_max_tokens(model: &Model, context: &kiana_ai::types::TranscriptContext) -> u64 {
    clamp_max_tokens_to_context(model, context, model.max_tokens.unwrap_or(0))
}

// describe("context token estimation")

#[test]
fn reserves_3_5_characters_per_token_for_new_text_when_limiting_output() {
    // Regression for #10497: large new inputs need more room than chars/4 allows.
    let context = Context {
        system_prompt: None,
        messages: vec![
            Message::Assistant(create_assistant(100, 2_000)),
            user_message(&"x".repeat(3_500), 200),
        ],
        tools: None,
    };
    let normalized = normalize_context(&context);

    assert_eq!(
        estimate_context_tokens(&normalized),
        ContextUsageEstimate {
            tokens: 3_000,
            usage_tokens: 2_000,
            trailing_tokens: 1_000,
            last_usage_index: Some(0),
        }
    );
    assert_eq!(base_options_max_tokens(&model(), &normalized), 2_904);
}

#[test]
fn ignores_stale_assistant_usage_after_a_newer_message_is_inserted_before_it() {
    let context = Context {
        system_prompt: Some("system".into()),
        messages: vec![
            user_message("summary", 200),
            Message::Assistant(create_assistant(100, 9_500)),
            user_message(&"x".repeat(4_000), 300),
        ],
        tools: None,
    };
    let normalized = normalize_context(&context);

    assert_eq!(
        estimate_context_tokens(&normalized),
        ContextUsageEstimate {
            tokens: 1_149,
            usage_tokens: 0,
            trailing_tokens: 1_149,
            last_usage_index: None,
        }
    );
    assert_eq!(base_options_max_tokens(&model(), &normalized), 4_755);
}

#[test]
fn uses_assistant_usage_again_after_a_response_to_the_inserted_context() {
    let context = Context {
        system_prompt: None,
        messages: vec![
            user_message("summary", 200),
            Message::Assistant(create_assistant(100, 9_500)),
            user_message("new prompt", 300),
            Message::Assistant(create_assistant(400, 2_000)),
            user_message("tail", 500),
        ],
        tools: None,
    };
    let normalized = normalize_context(&context);

    assert_eq!(
        estimate_context_tokens(&normalized),
        ContextUsageEstimate {
            tokens: 2_002,
            usage_tokens: 2_000,
            trailing_tokens: 2,
            last_usage_index: Some(3),
        }
    );
}

#[test]
fn estimate_and_context_helpers_agree() {
    // TS 的 `estimateContextTokens` 同时接受 `TranscriptContext` 与 `Message[]`；Rust 侧两个
    // 入口对同一份归一化消息必须给出同一结果。
    let context = Context {
        system_prompt: Some("system".into()),
        messages: vec![user_message("hello", 1)],
        tools: None,
    };
    let normalized = normalize_context(&context);
    assert_eq!(
        estimate_context_tokens(&normalized),
        kiana_ai::utils::estimate::estimate_context_tokens_for_messages(
            normalized.as_context().messages.as_slice()
        )
    );
}
