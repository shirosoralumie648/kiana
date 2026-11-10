//! api/github-copilot-headers.ts。
//!
//! Copilot 期望 `X-Initiator` 说明请求是用户发起还是 agent 发起（例如 assistant/tool
//! 消息后的追问）；发送图片时还需要 `Copilot-Vision-Request`。
//!
//! TS: `packages/ai/src/api/github-copilot-headers.ts`。
//!
//! # 被谁调用
//!
//! `api/anthropic_messages.rs`、`api/openai_completions.rs`、`api/openai_responses.rs`
//! 只在 `model.provider == "github-copilot"` 时调用这里的函数，把结果并进请求头；
//! 其它 provider 走的是不调用这些函数的分支。
//!
//! # 约束
//!
//! - [`infer_copilot_initiator`] 只看最后一条消息的 role，不追溯整条工具调用链；
//!   空列表算 `user`。`system` 结尾与 `assistant` / `toolResult` 结尾一样算 `agent`。
//! - [`has_copilot_vision_input`] 只统计 `user` 与 `toolResult` 的内容块；`assistant`
//!   与 `system` 里的图片不计（它们不属于本次请求的视觉输入）。
//! - [`build_copilot_dynamic_headers`] 的 `has_images` 由调用方传入（当前调用方都传
//!   [`has_copilot_vision_input`] 的结果）。头顺序与键名逐字重要，测试
//!   `builds_headers_in_fixed_order` 已锁定。

use indexmap::IndexMap;

use crate::types::{Message, UserContent, UserContentBlock};

/// `inferCopilotInitiator()` 的返回值（TS 的 `"user" | "agent"`）。
///
/// 用枚举而不是字符串，避免调用方拼错字面量；上线值由 [`CopilotInitiator::as_str`]
/// 固定，构造时无法再传入其它值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CopilotInitiator {
    /// 请求由用户发起。
    User,
    /// 请求由 agent 发起（assistant / toolResult / system 结尾）。
    Agent,
}

impl CopilotInitiator {
    /// 头字段的字面量。
    ///
    /// 返回值会被逐字写进 `X-Initiator`，不能改成大写或缩写。
    pub fn as_str(self) -> &'static str {
        match self {
            CopilotInitiator::User => "user",
            CopilotInitiator::Agent => "agent",
        }
    }
}

/// `inferCopilotInitiator(messages)`。
///
/// 空数组 → `User`；最后一条是 `user` → `User`；否则（assistant / toolResult / system）→ `Agent`。
///
/// 判断只看最后一条消息的 role（`Message::role()`），不检查这一轮是否真的由工具结果
/// 触发；因此「用户消息后紧跟 assistant」也算 `Agent`。
///
/// TS: `src/api/github-copilot-headers.ts#inferCopilotInitiator`
pub fn infer_copilot_initiator(messages: &[Message]) -> CopilotInitiator {
    match messages.last() {
        Some(message) if message.role() != "user" => CopilotInitiator::Agent,
        _ => CopilotInitiator::User,
    }
}

fn is_image_block(block: &UserContentBlock) -> bool {
    matches!(block, UserContentBlock::Image(_))
}

/// `hasCopilotVisionInput(messages)`。
///
/// 只有 `user` / `toolResult` 消息的内容块里的图片算数；`system` / `assistant` 里的图片不计。
///
/// `user` 的纯文本形态（[`UserContent::Text`]）在数据结构上就不可能含图片，直接返回
/// `false`；`toolResult` 的 `content` 总是块列表，因此工具结果里的图片会被计入——TS
/// 同样如此，Copilot 需要据此决定是否带 vision 头。
///
/// TS: `src/api/github-copilot-headers.ts#hasCopilotVisionInput`
pub fn has_copilot_vision_input(messages: &[Message]) -> bool {
    messages.iter().any(|message| match message {
        Message::User(user) => match &user.content {
            UserContent::Blocks(blocks) => blocks.iter().any(is_image_block),
            UserContent::Text(_) => false,
        },
        Message::ToolResult(result) => result.content.iter().any(is_image_block),
        Message::System(_) | Message::Assistant(_) => false,
    })
}

/// `buildCopilotDynamicHeaders(params)`。
///
/// 返回顺序固定：`X-Initiator`、`Openai-Intent`，`hasImages` 为真时追加
/// `Copilot-Vision-Request`。键名逐字（`Openai` 全小写 o、`Intent` 首字母大写）。
///
/// `has_images` 与 `messages` 分开传：本函数不重新扫描消息，调用方传入自己算好的
/// 布尔值（当前调用方都传 [`has_copilot_vision_input`] 的结果）。
/// `Openai-Intent` 的值恒为 `"conversation-edits"`，不随消息变化。
///
/// TS: `src/api/github-copilot-headers.ts#buildCopilotDynamicHeaders`
pub fn build_copilot_dynamic_headers(
    messages: &[Message],
    has_images: bool,
) -> IndexMap<String, String> {
    let mut headers = IndexMap::new();
    headers.insert(
        "X-Initiator".to_string(),
        infer_copilot_initiator(messages).as_str().to_string(),
    );
    headers.insert(
        "Openai-Intent".to_string(),
        "conversation-edits".to_string(),
    );
    if has_images {
        headers.insert("Copilot-Vision-Request".to_string(), "true".to_string());
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AssistantMessage, ImageContent, ImageKind, TextContent, TextKind, ToolResultMessage,
        ToolResultRole, UserRole,
    };

    fn user_text(text: &str) -> Message {
        Message::User(crate::types::UserMessage {
            role: UserRole::User,
            content: UserContent::Text(text.into()),
            timestamp: 0,
        })
    }

    fn user_blocks(blocks: Vec<UserContentBlock>) -> Message {
        Message::User(crate::types::UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(blocks),
            timestamp: 0,
        })
    }

    fn tool_result(content: Vec<UserContentBlock>) -> Message {
        Message::ToolResult(ToolResultMessage {
            role: ToolResultRole::ToolResult,
            tool_call_id: "call_1".into(),
            tool_name: "read".into(),
            content,
            details: None,
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp: 0,
            duration_ms: None,
        })
    }

    fn image_block() -> UserContentBlock {
        UserContentBlock::Image(ImageContent {
            kind_image: ImageKind::Image,
            data: "AAAA".into(),
            mime_type: "image/png".into(),
        })
    }

    fn text_block(text: &str) -> UserContentBlock {
        UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: text.into(),
            text_signature: None,
        })
    }

    #[test]
    fn empty_messages_are_user_initiated() {
        assert_eq!(infer_copilot_initiator(&[]), CopilotInitiator::User);
    }

    #[test]
    fn trailing_user_message_is_user_initiated() {
        assert_eq!(
            infer_copilot_initiator(&[user_text("hi")]),
            CopilotInitiator::User
        );
    }

    #[test]
    fn trailing_assistant_or_tool_result_is_agent_initiated() {
        let assistant = Message::Assistant(AssistantMessage::default());
        let tool_result = tool_result(vec![]);
        assert_eq!(
            infer_copilot_initiator(&[user_text("hi"), assistant]),
            CopilotInitiator::Agent
        );
        assert_eq!(
            infer_copilot_initiator(&[user_text("hi"), tool_result]),
            CopilotInitiator::Agent
        );
    }

    #[test]
    fn detects_images_in_user_and_tool_results_only() {
        assert!(!has_copilot_vision_input(&[user_text("hi")]));
        assert!(has_copilot_vision_input(&[user_blocks(vec![
            text_block("look"),
            image_block()
        ])]));

        assert!(has_copilot_vision_input(&[tool_result(
            vec![image_block()]
        )]));

        // assistant 里的图片不计。
        let assistant = AssistantMessage {
            content: vec![crate::types::AssistantContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: "x".into(),
                text_signature: None,
            })],
            ..AssistantMessage::default()
        };
        assert!(!has_copilot_vision_input(&[Message::Assistant(assistant)]));
    }

    #[test]
    fn builds_headers_in_fixed_order() {
        let headers = build_copilot_dynamic_headers(&[user_text("hi")], false);
        assert_eq!(
            headers.keys().cloned().collect::<Vec<_>>(),
            vec!["X-Initiator".to_string(), "Openai-Intent".to_string()]
        );
        assert_eq!(headers["X-Initiator"], "user");
        assert_eq!(headers["Openai-Intent"], "conversation-edits");

        let headers = build_copilot_dynamic_headers(&[user_text("hi")], true);
        assert_eq!(
            headers.keys().cloned().collect::<Vec<_>>(),
            vec![
                "X-Initiator".to_string(),
                "Openai-Intent".to_string(),
                "Copilot-Vision-Request".to_string()
            ]
        );
        assert_eq!(headers["Copilot-Vision-Request"], "true");
    }
}
