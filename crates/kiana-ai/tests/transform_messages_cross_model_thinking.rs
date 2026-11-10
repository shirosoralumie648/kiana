//! `transform-messages-cross-model-thinking.test.ts` 的 Rust 移植。
//!
//! https://github.com/earendil-works/kiana/issues/10995：另一个模型的可见 thinking 不得作为
//! 纯文本发给新模型；只有目标模型自己的 thinking 保留。

use kiana_ai::api::transform_messages::transform_messages;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Message, Model, ModelInputModality,
    StopReason, TextContent, TextKind, ThinkingContent, ThinkingKind, ToolCall, ToolCallKind,
    ToolResultMessage, ToolResultRole, Usage, UserContent, UserMessage, UserRole,
};

fn kimi() -> Model {
    Model {
        id: "moonshotai/Kimi-K3".into(),
        name: "Kimi K3".into(),
        api: "openai-completions".into(),
        provider: "sference".into(),
        base_url: "https://api.sference.com/v1".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        context_window: Some(1_048_576),
        max_tokens: Some(32_768),
        ..Model::default()
    }
}

fn assistant(model: &str, content: Vec<AssistantContentBlock>) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: "openai-completions".into(),
        provider: "sference".into(),
        model: model.into(),
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
        timestamp: 1_700_000_000_000,
        ..AssistantMessage::default()
    }
}

fn user(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.into()),
        timestamp: 1_700_000_000_000,
    })
}

fn thinking(text: &str) -> AssistantContentBlock {
    AssistantContentBlock::Thinking(ThinkingContent {
        kind_thinking: ThinkingKind::Thinking,
        thinking: text.into(),
        thinking_signature: Some("reasoning_content".into()),
        redacted: None,
    })
}

fn text(value: &str) -> AssistantContentBlock {
    AssistantContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: value.into(),
        text_signature: None,
    })
}

fn assistant_texts_and_thinking(message: &AssistantMessage) -> Vec<(String, String)> {
    message
        .content
        .iter()
        .map(|block| match block {
            AssistantContentBlock::Text(text) => ("text".to_string(), text.text.clone()),
            AssistantContentBlock::Thinking(thinking) => {
                ("thinking".to_string(), thinking.thinking.clone())
            }
            AssistantContentBlock::ToolCall(call) => ("toolCall".to_string(), call.id.clone()),
        })
        .collect()
}

#[test]
fn does_not_send_another_models_readable_thinking_as_text() {
    let messages = vec![
        user("Why does curl work but node fails?"),
        Message::Assistant(assistant(
            "zai-org/GLM-5.3",
            vec![
                thinking("The user asks about ECONNRESET. Candidates: proxy, IPv6, TLS."),
                text("Let me check the TLS versions."),
                AssistantContentBlock::ToolCall(ToolCall {
                    kind_tool_call: ToolCallKind::ToolCall,
                    id: "call_1".into(),
                    name: "bash".into(),
                    arguments: [("command".to_string(), serde_json::json!("curl -v"))]
                        .into_iter()
                        .collect(),
                    thought_signature: None,
                    namespace: None,
                    arguments_error: None,
                }),
            ],
        )),
        Message::ToolResult(ToolResultMessage {
            role: ToolResultRole::ToolResult,
            tool_call_id: "call_1".into(),
            tool_name: "bash".into(),
            content: vec![kiana_ai::types::UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "TLSv1.2".into(),
                text_signature: None,
            })],
            details: None,
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp: 1_700_000_000_000,
            duration_ms: None,
        }),
    ];

    let result = transform_messages(&messages, &kimi());
    let assistant_message = result
        .iter()
        .find_map(|message| match message {
            Message::Assistant(assistant) => Some(assistant),
            _ => None,
        })
        .expect("assistant message");

    assert_eq!(
        assistant_texts_and_thinking(assistant_message),
        vec![
            (
                "text".to_string(),
                "Let me check the TLS versions.".to_string()
            ),
            ("toolCall".to_string(), "call_1".to_string()),
        ]
    );
}

#[test]
fn keeps_thinking_only_from_the_target_model_when_models_alternate() {
    let make = |model: &str, thinking_text: &str, text_value: &str| {
        let mut message = assistant(model, vec![thinking(thinking_text), text(text_value)]);
        message.stop_reason = StopReason::Stop;
        Message::Assistant(message)
    };
    let messages = vec![
        user("1"),
        make("moonshotai/Kimi-K3", "kimi 1", "a"),
        user("2"),
        make("zai-org/GLM-5.3", "glm 1", "b"),
        user("3"),
        make("moonshotai/Kimi-K3", "kimi 2", "c"),
        user("4"),
        make("zai-org/GLM-5.3", "glm 2", "d"),
        user("hi"),
    ];

    let result = transform_messages(&messages, &kimi());
    let contents: Vec<Vec<(String, String)>> = result
        .iter()
        .filter_map(|message| match message {
            Message::Assistant(assistant) => Some(assistant_texts_and_thinking(assistant)),
            _ => None,
        })
        .collect();

    assert_eq!(
        contents,
        vec![
            vec![
                ("thinking".to_string(), "kimi 1".to_string()),
                ("text".to_string(), "a".to_string()),
            ],
            vec![("text".to_string(), "b".to_string())],
            vec![
                ("thinking".to_string(), "kimi 2".to_string()),
                ("text".to_string(), "c".to_string()),
            ],
            vec![("text".to_string(), "d".to_string())],
        ]
    );
}
