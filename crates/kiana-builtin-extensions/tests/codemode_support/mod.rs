//! codemode 集成测试的共享辅助。

use kiana_agent_core::AgentToolResult;
use kiana_ai::types::{JsonValue, UserContentBlock};

/// 结果里的文本块。
pub fn text_blocks(result: &AgentToolResult<JsonValue>) -> Vec<&str> {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect()
}

/// 是否有文本块包含 `needle`。
pub fn has_text_containing(result: &AgentToolResult<JsonValue>, needle: &str) -> bool {
    text_blocks(result)
        .iter()
        .any(|block| block.contains(needle))
}
