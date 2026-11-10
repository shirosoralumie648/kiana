//! `packages/ai/test/text.test.ts`。
//!
//! TS 的 `contentText(content, separator?)` 是 `string | TextContent[]` 的联合重载；
//! Rust 侧按内容类型拆成 `content_text_*` 系列（见 [`kiana_ai::utils::text`]），断言逐项对应。

use kiana_ai::types::{
    AssistantContentBlock, ImageContent, ImageKind, TextContent, TextKind, ThinkingContent,
    ThinkingKind, ToolCall, ToolCallKind, UserContent, UserContentBlock,
};
use kiana_ai::utils::text::{content_text_assistant, content_text_user, content_text_user_content};

fn text_block(text: &str) -> TextContent {
    TextContent {
        kind_text: TextKind::Text,
        text: text.into(),
        text_signature: None,
    }
}

fn content() -> Vec<AssistantContentBlock> {
    vec![
        AssistantContentBlock::Thinking(ThinkingContent {
            kind_thinking: ThinkingKind::Thinking,
            thinking: "reasoning".into(),
            thinking_signature: None,
            redacted: None,
        }),
        AssistantContentBlock::Text(text_block("first")),
        AssistantContentBlock::ToolCall(ToolCall {
            kind_tool_call: ToolCallKind::ToolCall,
            id: "1".into(),
            name: "read".into(),
            arguments: Default::default(),
            thought_signature: None,
            namespace: None,
            arguments_error: None,
        }),
        AssistantContentBlock::Text(text_block("second")),
    ]
}

// describe("contentText")

#[test]
fn extracts_assistant_text_blocks() {
    assert_eq!(content_text_assistant(&content(), "\n"), "first\nsecond");
}

#[test]
fn supports_custom_separators() {
    assert_eq!(content_text_assistant(&content(), ""), "firstsecond");
}

#[test]
fn passes_string_content_through() {
    assert_eq!(
        content_text_user_content(&UserContent::Text("hello".into()), "\n"),
        "hello"
    );
}

#[test]
fn extracts_text_from_tool_result_content() {
    let tool_result_content = vec![
        UserContentBlock::Text(text_block("first")),
        UserContentBlock::Image(ImageContent {
            kind_image: ImageKind::Image,
            data: "...".into(),
            mime_type: "image/png".into(),
        }),
        UserContentBlock::Text(text_block("second")),
    ];

    assert_eq!(content_text_user(&tool_result_content, ""), "firstsecond");
}
