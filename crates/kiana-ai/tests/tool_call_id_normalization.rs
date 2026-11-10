//! `tool-call-id-normalization.test.ts` 的移植（真实 provider 的跨 provider handoff）。
//!
//! OpenAI Responses 生成的 tool call id 形如 `{call_id}|{id}`；切到其他 provider 时必须归一化，
//! 不能出现 "call_id too long" 或 ID 校验错误。`it.skipIf(!key)` 由测试开头的凭据检查复刻。

mod common;

#[path = "fixtures/api-openai/mock_http.rs"]
mod mock_http;

use indexmap::IndexMap;
use kiana_ai::compat::{complete_simple, get_env_api_key, get_model};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Context, Message, ProviderId,
    SimpleStreamOptions, StopReason, TextContent, TextKind, Tool, ToolCall, ToolCallKind,
    ToolResultMessage, ToolResultRole, Usage, UserContent, UserContentBlock, UserMessage, UserRole,
};
use serde_json::json;

fn echo_tool() -> Tool {
    Tool {
        name: "echo".into(),
        description: "Echoes the message back".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "message": { "type": "string", "description": "Message to echo back" }
            },
            "required": ["message"],
        }),
        constrained_sampling: None,
    }
}

fn user(text: &str, timestamp: i64) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.into()),
        timestamp,
    })
}

fn tool_call_of(message: &AssistantMessage) -> &ToolCall {
    message
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::ToolCall(call) => Some(call),
            _ => None,
        })
        .expect("tool call")
}

fn tool_result(call: &ToolCall, text: &str, timestamp: i64) -> Message {
    Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: call.id.clone(),
        tool_name: "echo".into(),
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: text.into(),
            text_signature: None,
        })],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp,
        duration_ms: None,
    })
}

fn simple_options(api_key: &str) -> SimpleStreamOptions {
    SimpleStreamOptions {
        base: kiana_ai::types::StreamOptions {
            api_key: Some(api_key.to_string()),
            ..Default::default()
        },
        ..Default::default()
    }
}

async fn complete_handoff(
    model: &kiana_ai::types::Model,
    messages: Vec<Message>,
    api_key: &str,
) -> AssistantMessage {
    let context = Context {
        system_prompt: Some("You are a helpful assistant.".into()),
        messages,
        tools: Some(vec![echo_tool()]),
    };
    complete_simple(model, &context, simple_options(api_key)).await
}

#[tokio::test]
#[ignore = "needs live github-copilot + openrouter credentials"]
async fn github_copilot_to_openrouter_should_normalize_pipe_separated_ids() {
    let Some(copilot_token) = common::oauth::resolve_api_key("github-copilot") else {
        return;
    };
    let Some(openrouter_key) = get_env_api_key("openrouter", None) else {
        return;
    };
    let copilot_model = get_model("github-copilot", "gpt-6-luna").expect("copilot model");
    let openrouter_model = get_model("openrouter", "openai/gpt-5.5").expect("openrouter model");

    let user_message = user("Use the echo tool to echo 'hello world'", 0);
    let assistant =
        complete_handoff(&copilot_model, vec![user_message.clone()], &copilot_token).await;
    assert_eq!(
        assistant.stop_reason,
        StopReason::ToolUse,
        "Copilot error: {:?}",
        assistant.error_message
    );
    let call = tool_call_of(&assistant);
    assert!(call.id.contains('|'), "expected a pipe-separated id");
    let result = tool_result(call, "hello world", 0);

    let response = complete_handoff(
        &openrouter_model,
        vec![
            user_message,
            Message::Assistant(assistant),
            result,
            user("Say hi", 0),
        ],
        &openrouter_key,
    )
    .await;
    assert_ne!(
        response.stop_reason,
        StopReason::Error,
        "OpenRouter error: {:?}",
        response.error_message
    );
    assert!(response.error_message.is_none());
}

#[tokio::test]
#[ignore = "needs live github-copilot + openai-codex credentials"]
async fn openrouter_to_openai_codex_should_normalize_pipe_separated_ids() {
    let Some(copilot_token) = common::oauth::resolve_api_key("github-copilot") else {
        return;
    };
    let Some(codex_token) = common::oauth::resolve_api_key("openai-codex") else {
        return;
    };
    let copilot_model = get_model("github-copilot", "gpt-6-luna").expect("copilot model");
    let codex_model = get_model("openai-codex", "gpt-6.1-sol").expect("codex model");

    let user_message = user("Use the echo tool to echo 'test message'", 0);
    let assistant =
        complete_handoff(&copilot_model, vec![user_message.clone()], &copilot_token).await;
    assert_eq!(
        assistant.stop_reason,
        StopReason::ToolUse,
        "Copilot error: {:?}",
        assistant.error_message
    );
    let call = tool_call_of(&assistant);
    let result = tool_result(call, "test message", 0);

    let response = complete_handoff(
        &codex_model,
        vec![
            user_message,
            Message::Assistant(assistant),
            result,
            user("Say hi", 0),
        ],
        &codex_token,
    )
    .await;
    assert_ne!(
        response.stop_reason,
        StopReason::Error,
        "Codex error: {:?}",
        response.error_message
    );
    assert!(response.error_message.is_none());
}

/// issue #1022 的原始失败 id（`call_xxx|very_long_base64_with_special_chars+/=`）。
const FAILING_TOOL_CALL_ID: &str = "call_pAYbIr76hXIjncD9UE4eGfnS|t5nnb2qYMFWGSsr13fhCd1CaCu3t3qONEPuOudu4HSVEtA8YJSL6FAZUxvoOoD792VIJWl91g87EdqsCWp9krVsdBysQoDaf9lMCLb8BS4EYi4gQd5kBQBYLlgD71PYwvf+TbMD9J9/5OMD42oxSRj8H+vRf78/l2Xla33LWz4nOgsddBlbvabICRs8GHt5C9PK5keFtzyi3lsyVKNlfduK3iphsZqs4MLv4zyGJnvZo/+QzShyk5xnMSQX/f98+aEoNflEApCdEOXipipgeiNWnpFSHbcwmMkZoJhURNu+JEz3xCh1mrXeYoN5o+trLL3IXJacSsLYXDrYTipZZbJFRPAucgbnjYBC+/ZzJOfkwCs+Gkw7EoZR7ZQgJ8ma+9586n4tT4cI8DEhBSZsWMjrCt8dxKg==";

fn build_prefilled_messages() -> Vec<Message> {
    let user_message = user("Use the echo tool to echo 'hello'", -2000);
    let assistant = Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::ToolCall(ToolCall {
            kind_tool_call: ToolCallKind::ToolCall,
            id: FAILING_TOOL_CALL_ID.into(),
            name: "echo".into(),
            arguments: IndexMap::from([("message".to_string(), json!("hello"))]),
            thought_signature: None,
            namespace: None,
            arguments_error: None,
        })],
        api: "openai-responses".into(),
        provider: ProviderId::from("github-copilot"),
        model: "gpt-5.2-codex".into(),
        usage: Usage {
            input: 100,
            output: 50,
            cache_read: 0,
            cache_write: 0,
            total_tokens: 150,
            ..Default::default()
        },
        stop_reason: StopReason::ToolUse,
        timestamp: -1500,
        ..Default::default()
    });
    let result = Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: FAILING_TOOL_CALL_ID.into(),
        tool_name: "echo".into(),
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "hello".into(),
            text_signature: None,
        })],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: -1000,
        duration_ms: None,
    });
    vec![user_message, assistant, result, user("Say hi", 0)]
}

async fn complete_prefilled(model: &kiana_ai::types::Model, api_key: &str) -> AssistantMessage {
    let context = Context {
        system_prompt: Some("You are a helpful assistant.".into()),
        messages: build_prefilled_messages(),
        tools: Some(vec![echo_tool()]),
    };
    complete_simple(model, &context, simple_options(api_key)).await
}

#[tokio::test]
#[ignore = "needs live openrouter credentials"]
async fn prefill_context_with_foreign_tool_call_ids_normalizes_before_dispatch() {
    let Some(key) = get_env_api_key("openrouter", None) else {
        return;
    };
    let model = get_model("openrouter", "openai/gpt-5.5").expect("openrouter model");
    let response = complete_prefilled(&model, &key).await;
    assert_ne!(
        response.stop_reason,
        StopReason::Error,
        "OpenRouter error: {:?}",
        response.error_message
    );
    if let Some(message) = response.error_message.as_deref() {
        assert!(!message.contains("call_id"), "unexpected error {message:?}");
        assert!(
            !message.contains("too long"),
            "unexpected error {message:?}"
        );
    }
}

#[tokio::test]
#[ignore = "needs live openai-codex credentials"]
async fn codex_prefill_context_with_foreign_tool_call_ids_normalizes_before_dispatch() {
    let Some(key) = common::oauth::resolve_api_key("openai-codex") else {
        return;
    };
    let model = get_model("openai-codex", "gpt-6.1-sol").expect("codex model");
    let response = complete_prefilled(&model, &key).await;
    assert_ne!(
        response.stop_reason,
        StopReason::Error,
        "Codex error: {:?}",
        response.error_message
    );
    if let Some(message) = response.error_message.as_deref() {
        assert!(!message.contains("id"), "unexpected error {message:?}");
        assert!(
            !message.contains("additional characters"),
            "unexpected error {message:?}"
        );
    }
}
