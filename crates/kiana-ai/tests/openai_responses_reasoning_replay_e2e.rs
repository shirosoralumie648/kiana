//! `openai-responses-reasoning-replay-e2e.test.ts`（真实 OpenAI/Anthropic API）。

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use std::sync::{Arc, Mutex};

use kiana_ai::api::openai_responses::OpenAIResponsesOptions;
use kiana_ai::compat::{complete, get_model};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, Context, Message, StreamOptions, Tool,
    ToolResultMessage, UserContentBlock,
};
use serde_json::{Value, json};

fn double_tool() -> Tool {
    Tool {
        name: "double_number".into(),
        description: "Doubles a number and returns the result".into(),
        parameters: json!({
            "type": "object",
            "properties": { "value": { "type": "number", "description": "A number to double" } },
            "required": ["value"],
        }),
        constrained_sampling: None,
    }
}

#[tokio::test]
#[ignore = "needs live OpenAI + Anthropic API keys"]
async fn skips_reasoning_only_history_after_an_aborted_turn() {
    let Ok(api_key) = std::env::var("OPENAI_API_KEY") else {
        return;
    };
    if api_key.is_empty() {
        return;
    }
    let model = mock_http::catalog_model("openai", "gpt-5-mini");
    let context = Context {
        system_prompt: Some("You are a helpful assistant. Use the tool.".into()),
        messages: vec![mock_http::user("Use the double_number tool to double 21.")],
        tools: Some(vec![double_tool()]),
    };
    let assistant = mock_http::run_responses(
        &model,
        &mock_http::context_with(&context),
        OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some(api_key),
                ..Default::default()
            },
            reasoning_effort: Some(kiana_ai::types::ThinkingLevel::High),
            ..Default::default()
        },
    )
    .await;
    let thinking = assistant
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::Thinking(thinking)
                if thinking
                    .thinking_signature
                    .as_deref()
                    .is_some_and(|s| !s.is_empty()) =>
            {
                Some(thinking.clone())
            }
            _ => None,
        })
        .expect("thinking signature");
    let corrupted = AssistantMessage {
        content: vec![AssistantContentBlock::Thinking(thinking)],
        stop_reason: kiana_ai::types::StopReason::Aborted,
        ..assistant
    };
    let follow_up = Context {
        system_prompt: Some("You are a helpful assistant. Use the tool.".into()),
        messages: vec![
            mock_http::user("Use the double_number tool to double 21."),
            Message::Assistant(corrupted),
            mock_http::user("Continue."),
        ],
        tools: Some(vec![double_tool()]),
    };
    let response = mock_http::run_responses(
        &model,
        &mock_http::context_with(&follow_up),
        OpenAIResponsesOptions {
            base: StreamOptions {
                api_key: Some(std::env::var("OPENAI_API_KEY").unwrap()),
                ..Default::default()
            },
            reasoning_effort: Some(kiana_ai::types::ThinkingLevel::High),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        response.stop_reason,
        kiana_ai::types::StopReason::Stop,
        "{:?}",
        response.error_message
    );
}

fn user(text: &str) -> Message {
    Message::User(kiana_ai::types::UserMessage {
        role: Default::default(),
        content: kiana_ai::types::UserContent::Text(text.into()),
        timestamp: 0,
    })
}

fn tool_result(call: &kiana_ai::types::ToolCall, text: &str) -> Message {
    Message::ToolResult(ToolResultMessage {
        role: Default::default(),
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        content: vec![UserContentBlock::Text(kiana_ai::types::TextContent {
            text: text.into(),
            ..Default::default()
        })],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 0,
        duration_ms: None,
    })
}

fn first_tool_call(message: &AssistantMessage) -> Option<kiana_ai::types::ToolCall> {
    message.content.iter().find_map(|block| match block {
        AssistantContentBlock::ToolCall(call) => Some(call.clone()),
        _ => None,
    })
}

fn response_text(message: &AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn options(api_key: &str, extras: &[(&str, Value)]) -> StreamOptions {
    let mut options = StreamOptions {
        api_key: Some(api_key.to_string()),
        ..Default::default()
    };
    for (key, value) in extras {
        options.extra.insert((*key).to_string(), value.clone());
    }
    options
}

#[tokio::test]
#[ignore = "needs live OpenAI API key (same-provider different-model handoff)"]
async fn handles_same_provider_different_model_handoff_with_tool_calls() {
    let Ok(api_key) = std::env::var("OPENAI_API_KEY") else {
        return;
    };
    if api_key.is_empty() {
        return;
    }
    let model_a = get_model("openai", "gpt-5-mini").expect("gpt-5-mini");
    let model_b = get_model("openai", "gpt-5.5").expect("gpt-5.5");
    let user_message = user("Use the double_number tool to double 21.");

    let first_context = Context {
        system_prompt: Some("You are a helpful assistant. Always use the tool when asked.".into()),
        messages: vec![user_message.clone()],
        tools: Some(vec![double_tool()]),
    };
    let assistant = complete(
        &model_a,
        &first_context,
        options(&api_key, &[("reasoningEffort", json!("high"))]),
    )
    .await;
    let call = first_tool_call(&assistant)
        .expect("Missing tool call from OpenAI Responses - model did not use the tool");
    let result = tool_result(&call, "42");

    let context = Context {
        system_prompt: Some("You are a helpful assistant. Answer concisely.".into()),
        messages: vec![
            user_message,
            Message::Assistant(assistant),
            result,
            user("What was the result? Answer with just the number."),
        ],
        tools: Some(vec![double_tool()]),
    };
    let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let capture = Arc::clone(&captured);
    let mut options = options(&api_key, &[("reasoningEffort", json!("high"))]);
    options.on_payload = Some(Arc::new(
        move |payload: Value, _model: &kiana_ai::types::Model| {
            *capture.lock().unwrap_or_else(|error| error.into_inner()) = Some(payload);
            None
        },
    ));
    let response = complete(&model_b, &context, options).await;

    assert_ne!(
        response.stop_reason,
        kiana_ai::types::StopReason::Error,
        "Error: {:?}",
        response.error_message
    );
    assert!(response.error_message.is_none());
    assert!(!response.content.is_empty());
    assert!(response_text(&response).contains("42"));
    let payload = captured
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .expect("captured payload");
    assert!(payload["input"].as_array().is_some());
}

#[tokio::test]
#[ignore = "needs live OpenAI + Anthropic API keys (cross-provider handoff)"]
async fn handles_cross_provider_handoff_from_anthropic_to_openai_codex() {
    let Ok(openai_api_key) = std::env::var("OPENAI_API_KEY") else {
        return;
    };
    if openai_api_key.is_empty() {
        return;
    }
    let Ok(anthropic_api_key) = std::env::var("ANTHROPIC_API_KEY") else {
        return;
    };
    if anthropic_api_key.is_empty() {
        return;
    }
    let anthropic_model = get_model("anthropic", "claude-sonnet-4-5").expect("claude-sonnet-4-5");
    let openai_model = get_model("openai", "gpt-5.5").expect("gpt-5.5");
    let user_message = user("Use the double_number tool to double 21.");

    let first_context = Context {
        system_prompt: Some("You are a helpful assistant. Always use the tool when asked.".into()),
        messages: vec![user_message.clone()],
        tools: Some(vec![double_tool()]),
    };
    let assistant = complete(
        &anthropic_model,
        &first_context,
        options(
            &anthropic_api_key,
            &[
                ("thinkingEnabled", json!(true)),
                ("thinkingBudgetTokens", json!(5000)),
            ],
        ),
    )
    .await;
    let call = first_tool_call(&assistant)
        .expect("Missing tool call from Anthropic - model did not use the tool");
    let result = tool_result(&call, "42");

    let context = Context {
        system_prompt: Some("You are a helpful assistant. Answer concisely.".into()),
        messages: vec![
            user_message,
            Message::Assistant(assistant),
            result,
            user("What was the result? Answer with just the number."),
        ],
        tools: Some(vec![double_tool()]),
    };
    let response = complete(
        &openai_model,
        &context,
        options(&openai_api_key, &[("reasoningEffort", json!("high"))]),
    )
    .await;

    assert_ne!(
        response.stop_reason,
        kiana_ai::types::StopReason::Error,
        "Error: {:?}",
        response.error_message
    );
    assert!(response.error_message.is_none());
    assert!(!response.content.is_empty());
    assert!(response_text(&response).contains("42"));
}
