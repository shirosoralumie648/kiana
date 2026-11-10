//! `transform-messages-copilot-openai-to-anthropic.test.ts` 的 Rust 移植。
//!
//! Copilot 上从 OpenAI Responses 迁移到 Anthropic 的会话：跨模型时移除 `thoughtSignature`、
//! 归一化 tool call id，并为结尾的孤儿调用合成 `"No result provided"` 结果。

use kiana_ai::api::transform_messages::transform_messages_with;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Message, Model, ModelInputModality,
    StopReason, TextContent, TextKind, ToolCall, ToolCallKind, ToolResultMessage, ToolResultRole,
    Usage, UserContent, UserContentBlock, UserMessage, UserRole,
};

/// TS 测试里模拟 anthropic 适配器的归一化函数：非 `[a-zA-Z0-9_-]` → `_`，截断 64。
fn anthropic_normalize_tool_call_id(
    id: &str,
    _model: &Model,
    _source: &AssistantMessage,
) -> String {
    id.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

fn copilot_claude_model() -> Model {
    Model {
        id: "claude-sonnet-4.6".into(),
        name: "Claude Sonnet 4.6".into(),
        api: "anthropic-messages".into(),
        provider: "github-copilot".into(),
        base_url: "https://api.individual.githubcopilot.com".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        context_window: Some(128_000),
        max_tokens: Some(16_000),
        ..Model::default()
    }
}

fn make_assistant_message(content: Vec<AssistantContentBlock>) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: "openai-responses".into(),
        provider: "github-copilot".into(),
        model: "gpt-5".into(),
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
        timestamp: 1_700_000_000_000,
        ..AssistantMessage::default()
    }
}

fn tool_call(id: &str, name: &str, arguments: kiana_ai::types::JsonObject) -> ToolCall {
    ToolCall {
        kind_tool_call: ToolCallKind::ToolCall,
        id: id.into(),
        name: name.into(),
        arguments,
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    }
}

fn args(value: serde_json::Value) -> kiana_ai::types::JsonObject {
    serde_json::from_value(value).expect("object")
}

fn user(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.into()),
        timestamp: 1_700_000_000_000,
    })
}

fn tool_result(id: &str, name: &str, text: &str) -> Message {
    Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: id.into(),
        tool_name: name.into(),
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: text.into(),
            text_signature: None,
        })],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 1_700_000_000_000,
        duration_ms: None,
    })
}

#[test]
fn removes_thought_signature_from_tool_calls_when_migrating_between_models() {
    let model = copilot_claude_model();
    let mut call = tool_call(
        "call_123",
        "bash",
        args(serde_json::json!({ "command": "ls" })),
    );
    call.thought_signature = Some(
        serde_json::json!({ "type": "reasoning.encrypted", "id": "call_123", "data": "encrypted" })
            .to_string(),
    );
    let messages = vec![
        user("run a command"),
        Message::Assistant(make_assistant_message(vec![
            AssistantContentBlock::ToolCall(call),
        ])),
        tool_result("call_123", "bash", "output"),
    ];

    let result =
        transform_messages_with(&messages, &model, Some(&anthropic_normalize_tool_call_id));
    let assistant_message = result
        .iter()
        .find_map(|message| match message {
            Message::Assistant(assistant) => Some(assistant),
            _ => None,
        })
        .expect("assistant message");
    let call = assistant_message
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::ToolCall(call) => Some(call),
            _ => None,
        })
        .expect("tool call");
    assert_eq!(call.thought_signature, None);
}

#[test]
fn adds_synthetic_tool_results_for_trailing_orphaned_tool_calls() {
    let model = copilot_claude_model();
    let messages = vec![
        user("read the file"),
        Message::Assistant(make_assistant_message(vec![
            AssistantContentBlock::ToolCall(tool_call(
                "call_123|fc_123",
                "read",
                args(serde_json::json!({ "path": "README.md" })),
            )),
        ])),
    ];

    let result =
        transform_messages_with(&messages, &model, Some(&anthropic_normalize_tool_call_id));
    let last = result.last().expect("last message");
    let Message::ToolResult(synthetic) = last else {
        panic!("last message is a synthetic tool result");
    };
    assert_eq!(synthetic.tool_call_id, "call_123_fc_123");
    assert_eq!(synthetic.tool_name, "read");
    assert!(synthetic.is_error);
    assert_eq!(
        synthetic.content,
        vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "No result provided".into(),
            text_signature: None,
        })]
    );
}

#[test]
fn adds_synthetic_results_only_for_trailing_tool_calls_that_are_still_missing_results() {
    let model = copilot_claude_model();
    let messages = vec![
        user("run commands"),
        Message::Assistant(make_assistant_message(vec![
            AssistantContentBlock::ToolCall(tool_call(
                "call_1|fc_1",
                "read",
                args(serde_json::json!({ "path": "README.md" })),
            )),
            AssistantContentBlock::ToolCall(tool_call(
                "call_2|fc_2",
                "bash",
                args(serde_json::json!({ "command": "pwd" })),
            )),
        ])),
        tool_result("call_1|fc_1", "read", "done"),
    ];

    let result =
        transform_messages_with(&messages, &model, Some(&anthropic_normalize_tool_call_id));
    let synthetic_results: Vec<&ToolResultMessage> = result
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult(result) if result.is_error => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(synthetic_results.len(), 1);
    assert_eq!(synthetic_results[0].tool_call_id, "call_2_fc_2");
    assert_eq!(synthetic_results[0].tool_name, "bash");
    assert_eq!(
        synthetic_results[0].content,
        vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: "No result provided".into(),
            text_signature: None,
        })]
    );
}
