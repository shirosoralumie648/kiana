//! `compaction-serialization.test.ts` 的 Rust 移植。
//!
//! TS: `packages/coding-agent/test/compaction-serialization.test.ts`

use indexmap::IndexMap;
use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, ImageContent, ImageKind, JsonValue,
    Message, StopReason, SystemContent, SystemMessage, SystemRole, TextContent, TextKind,
    ThinkingContent, ThinkingKind, Tool, ToolCall, ToolCallKind, ToolResultMessage, ToolResultRole,
    UserContent, UserContentBlock, UserMessage, UserRole,
};
use kiana_compaction::{estimate_tokens, serialize_conversation};

fn tool_result(text: &str) -> Message {
    Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: "tc1".to_string(),
        tool_name: "read".to_string(),
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: text.to_string(),
            text_signature: None,
        })],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 0,
        duration_ms: None,
    })
}

fn assistant(content: Vec<AssistantContentBlock>, stop_reason: StopReason) -> Message {
    Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: "anthropic".to_string(),
        provider: "anthropic".to_string(),
        model: "test".to_string(),
        stop_reason,
        timestamp: 0,
        ..AssistantMessage::default()
    })
}

/// TS: it("should truncate long tool results")
#[test]
fn should_truncate_long_tool_results() {
    let long_content = "x".repeat(5000);
    let messages = vec![tool_result(&long_content)];

    let result = serialize_conversation(&messages);

    assert!(result.contains("[Tool result]:"));
    assert!(result.contains("[... 3000 more characters truncated]"));
    assert!(!result.contains(&"x".repeat(3000)));
    // First 2000 chars should be present
    assert!(result.contains(&"x".repeat(2000)));
}

/// TS: it("should not truncate short tool results")
#[test]
fn should_not_truncate_short_tool_results() {
    let short_content = "x".repeat(1500);
    let messages = vec![tool_result(&short_content)];

    let result = serialize_conversation(&messages);

    assert_eq!(result, format!("[Tool result]: {short_content}"));
    assert!(!result.contains("truncated"));
}

/// TS: it("should not truncate assistant or user messages")
#[test]
fn should_not_truncate_assistant_or_user_messages() {
    let long_text = "y".repeat(5000);
    let messages = vec![
        Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: long_text.clone(),
                text_signature: None,
            })]),
            timestamp: 0,
        }),
        assistant(
            vec![AssistantContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: long_text.clone(),
                text_signature: None,
            })],
            StopReason::Stop,
        ),
    ];

    let result = serialize_conversation(&messages);

    assert!(!result.contains("truncated"));
    assert!(result.contains(&long_text));
}

/// TS: it("should omit thinking")
#[test]
fn should_omit_thinking() {
    let messages = vec![assistant(
        vec![
            AssistantContentBlock::Thinking(ThinkingContent {
                kind_thinking: ThinkingKind::Thinking,
                thinking: "private reasoning".to_string(),
                thinking_signature: Some("sig".to_string()),
                redacted: None,
            }),
            AssistantContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "visible reply".to_string(),
                text_signature: None,
            }),
            AssistantContentBlock::ToolCall(ToolCall {
                kind_tool_call: ToolCallKind::ToolCall,
                id: "tc1".to_string(),
                name: "read".to_string(),
                arguments: indexmap::IndexMap::from([(
                    "path".to_string(),
                    serde_json::Value::String("a.ts".to_string()),
                )]),
                thought_signature: None,
                namespace: None,
                arguments_error: None,
            }),
        ],
        StopReason::ToolUse,
    )];

    let result = serialize_conversation(&messages);

    assert_eq!(
        result,
        "[Assistant]: visible reply\n\n[Assistant tool calls]: read(path=\"a.ts\")"
    );
}

// ============================================================================
// 黄金测试：由 TS 参考实现生成（estimateTokens / serializeConversation）
// ============================================================================

fn image_block(data: &str) -> UserContentBlock {
    UserContentBlock::Image(ImageContent {
        kind_image: ImageKind::Image,
        data: data.to_string(),
        mime_type: "image/png".to_string(),
    })
}

fn text_block(text: &str) -> UserContentBlock {
    UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.to_string(),
        text_signature: None,
    })
}

fn custom_object(fields: Vec<(&str, JsonValue)>) -> AgentMessage {
    let mut object: IndexMap<String, JsonValue> = IndexMap::new();
    for (key, value) in fields {
        object.insert(key.to_string(), value);
    }
    AgentMessage::Custom(Box::new(object))
}

fn golden_messages() -> Vec<AgentMessage> {
    let usage = kiana_ai::types::Usage {
        input: 1,
        output: 2,
        cache_read: 3,
        cache_write: 4,
        total_tokens: 10,
        ..kiana_ai::types::Usage::default()
    };
    let mut arguments: IndexMap<String, JsonValue> = IndexMap::new();
    arguments.insert("2".to_string(), JsonValue::String("two".to_string()));
    arguments.insert(
        "1".to_string(),
        JsonValue::Number(serde_json::Number::from_f64(1e21).expect("finite")),
    );
    arguments.insert(
        "a".to_string(),
        JsonValue::Number(serde_json::Number::from_f64(0.000001).expect("finite")),
    );
    arguments.insert(
        "10".to_string(),
        JsonValue::Number(serde_json::Number::from_f64(-0.0).expect("finite")),
    );
    arguments.insert(
        "b".to_string(),
        JsonValue::Number(serde_json::Number::from_f64(1.0).expect("finite")),
    );
    arguments.insert("01".to_string(), JsonValue::String("leading".to_string()));
    arguments.insert("z".to_string(), serde_json::json!([1, 2.5, "x"]));

    let assistant_with_call = |id: &str,
                               name: &str,
                               arguments: IndexMap<String, JsonValue>,
                               with_text_and_thinking: bool|
     -> AgentMessage {
        let mut content: Vec<AssistantContentBlock> = Vec::new();
        if with_text_and_thinking {
            content.push(AssistantContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "visible".to_string(),
                text_signature: None,
            }));
            content.push(AssistantContentBlock::Thinking(ThinkingContent {
                kind_thinking: ThinkingKind::Thinking,
                thinking: "hidden reasoning".to_string(),
                thinking_signature: None,
                redacted: None,
            }));
        }
        content.push(AssistantContentBlock::ToolCall(ToolCall {
            kind_tool_call: ToolCallKind::ToolCall,
            id: id.to_string(),
            name: name.to_string(),
            arguments,
            thought_signature: None,
            namespace: None,
            arguments_error: None,
        }));
        AgentMessage::Standard(Message::Assistant(AssistantMessage {
            role: AssistantRole::Assistant,
            content,
            api: "anthropic-messages".to_string(),
            provider: "anthropic".to_string(),
            model: "claude-sonnet-4-5".to_string(),
            usage: usage.clone(),
            stop_reason: StopReason::ToolUse,
            timestamp: 1,
            ..AssistantMessage::default()
        }))
    };

    let mut sections: IndexMap<String, Option<String>> = IndexMap::new();
    sections.insert("preamble".to_string(), Some("1234".to_string()));
    sections.insert("other".to_string(), Some(String::new()));

    let system = AgentMessage::Standard(Message::System(SystemMessage {
        role: SystemRole::System,
        content: SystemContent::Text("héllo😀".to_string()),
        sections: Some(sections),
        tools_added: Some(vec![Tool {
            name: "read".to_string(),
            description: "d".to_string(),
            parameters: serde_json::json!({ "type": "object" }),
            constrained_sampling: None,
        }]),
        tools_removed: None,
        timestamp: 1,
    }));

    vec![
        system,
        AgentMessage::Standard(Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("user text 😀".to_string()),
            timestamp: 1,
        })),
        AgentMessage::Standard(Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![text_block("block text"), image_block("AAA")]),
            timestamp: 1,
        })),
        assistant_with_call("c1", "read", arguments, true),
        AgentMessage::Standard(Message::ToolResult(ToolResultMessage {
            role: ToolResultRole::ToolResult,
            tool_call_id: "c1".to_string(),
            tool_name: "read".to_string(),
            content: vec![text_block("result text"), image_block("BBB")],
            details: None,
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp: 1,
            duration_ms: None,
        })),
        custom_object(vec![
            ("role", JsonValue::String("custom".to_string())),
            ("customType", JsonValue::String("x".to_string())),
            ("content", JsonValue::String("custom string".to_string())),
            ("display", JsonValue::Bool(true)),
            ("timestamp", JsonValue::Number(1.into())),
        ]),
        custom_object(vec![
            ("role", JsonValue::String("custom".to_string())),
            ("customType", JsonValue::String("x".to_string())),
            (
                "content",
                serde_json::json!([
                    { "type": "text", "text": "custom block" },
                    { "type": "image", "data": "CCC", "mimeType": "image/png" }
                ]),
            ),
            ("display", JsonValue::Bool(true)),
            ("timestamp", JsonValue::Number(1.into())),
        ]),
        custom_object(vec![
            ("role", JsonValue::String("bashExecution".to_string())),
            ("command", JsonValue::String("echo hi".to_string())),
            ("output", JsonValue::String("hi\n".to_string())),
            ("exitCode", JsonValue::Number(0.into())),
            ("cancelled", JsonValue::Bool(false)),
            ("truncated", JsonValue::Bool(false)),
            ("timestamp", JsonValue::Number(1.into())),
        ]),
        custom_object(vec![
            ("role", JsonValue::String("branchSummary".to_string())),
            ("summary", JsonValue::String("branch summary".to_string())),
            ("fromId", JsonValue::String("x".to_string())),
            ("timestamp", JsonValue::Number(1.into())),
        ]),
        custom_object(vec![
            ("role", JsonValue::String("compactionSummary".to_string())),
            (
                "summary",
                JsonValue::String("compaction summary".to_string()),
            ),
            ("tokensBefore", JsonValue::Number(5.into())),
            ("timestamp", JsonValue::Number(1.into())),
        ]),
        assistant_with_call("c2", "noargs", IndexMap::new(), false),
    ]
}

/// TS: `contentText` 的 separator——user/toolResult 用 `""`，assistant 用 `"\n"`。
#[test]
fn joins_multiple_text_blocks_with_ts_separators() {
    let messages = vec![
        Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![text_block("a"), text_block("b")]),
            timestamp: 0,
        }),
        assistant(
            vec![
                AssistantContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "c".to_string(),
                    text_signature: None,
                }),
                AssistantContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "d".to_string(),
                    text_signature: None,
                }),
            ],
            StopReason::Stop,
        ),
    ];

    assert_eq!(
        serialize_conversation(&messages),
        "[User]: ab\n\n[Assistant]: c\nd"
    );
}

#[test]
fn estimate_tokens_matches_ts_reference_golden() {
    let messages = golden_messages();
    let tokens: Vec<u64> = messages.iter().map(estimate_tokens).collect();
    assert_eq!(tokens, vec![20, 3, 1203, 27, 1203, 4, 1203, 3, 4, 5, 2]);
}

#[test]
fn serialize_conversation_matches_ts_reference_golden() {
    let messages = golden_messages();
    let typed: Vec<Message> = messages
        .into_iter()
        .filter_map(|message| match message {
            AgentMessage::Standard(message) => match message {
                Message::System(_) => None,
                other => Some(other),
            },
            AgentMessage::Custom(_) => None,
        })
        .collect();
    // TS 的输入还包含 custom/bash/summary 消息，但它们不产生任何输出片段；这里只保留标准
    // 消息即可得到同一结果。
    assert_eq!(
        serialize_conversation(&typed),
        concat!(
            "[User]: user text 😀\n\n",
            "[User]: block text\n\n",
            "[Assistant]: visible\n\n",
            "[Assistant tool calls]: read(1=1e+21, 2=\"two\", 10=0, a=0.000001, b=1, 01=\"leading\", z=[1,2.5,\"x\"])\n\n",
            "[Tool result]: result text\n\n",
            "[Assistant tool calls]: noargs()"
        )
    );
}
