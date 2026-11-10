//! Prompt-too-long (PTL) 重试与 provider overflow 恢复状态机。
//!
//! 设计来源（只借鉴行为，不复制代码）：
//! - 任务要求：“PTL 重试头部丢最旧消息最多 3 次、至少留一组”；
//!   “provider overflow 只 compact+retry 一次，第二次明确报错”。
//! - `reference_project/Codex` / `reference_project/qwen-code` 的 context-overflow
//!   恢复路径：请求被 provider 拒绝时先从**请求副本**丢弃最旧历史，不修改持久化历史。
//!
//! # 为什么在请求副本上丢消息
//!
//! PTL 是 provider 对“这一次请求”的拒绝；持久化历史仍然完整，下一次请求可能需要它。
//! 因此 [`drop_oldest_groups`] 返回新的消息列表，调用方只把结果用于重试请求。
//!
//! # “组”的定义
//!
//! 一个 group 是一个 user 消息及其后的 assistant/toolResult 消息，直到下一个 user。
//! 开头的 system 消息不属于任何 group，永远保留。丢弃 group 不会拆开
//! `assistant(tool_calls)` 与其 `tool_result`。
//!
//! # 至少留一组
//!
//! 最大丢弃次数为 `min(max_retries, groups.len() - 1)`；即使配置更大，也不会把历史
//! 丢空。

use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::Message;

/// PTL 头部丢弃的最大次数。
pub const MAX_PTL_RETRIES: usize = 3;

/// provider overflow 的恢复动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverflowAction {
    /// 第一次 overflow：带着压缩后的上下文重试一次。
    CompactAndRetry,
    /// 第二次 overflow：明确失败，不再无限重试。
    Fail {
        /// 用户可见错误文本。
        message: String,
    },
}

/// provider overflow 恢复状态机：每次请求生命周期内只允许一次 compact+retry。
#[derive(Debug, Clone, Default)]
pub struct OverflowRecovery {
    attempts: usize,
}

impl OverflowRecovery {
    /// 新建（0 次尝试）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 已尝试次数。
    pub fn attempts(&self) -> usize {
        self.attempts
    }

    /// provider 报告 context overflow 时调用。
    pub fn on_overflow(&mut self) -> OverflowAction {
        self.attempts += 1;
        if self.attempts == 1 {
            OverflowAction::CompactAndRetry
        } else {
            OverflowAction::Fail {
                message: "Context overflow recovery failed after one compact-and-retry attempt. \
                          Try reducing context or switching to a larger-context model."
                    .to_string(),
            }
        }
    }

    /// 成功恢复后重置（下一次独立请求仍允许一次恢复）。
    pub fn reset(&mut self) {
        self.attempts = 0;
    }
}

fn is_system(message: &AgentMessage) -> bool {
    message.role() == "system"
}

/// 把消息切成 group：开头 system 消息单独保留，其余每个 user 开启一组。
///
/// 没有 user 的尾部消息归入最后一组（或自成一组）。
pub fn group_messages(messages: &[AgentMessage]) -> (Vec<AgentMessage>, Vec<Vec<AgentMessage>>) {
    let mut prefix = Vec::new();
    let mut groups: Vec<Vec<AgentMessage>> = Vec::new();
    let mut index = 0;
    while index < messages.len() && is_system(&messages[index]) {
        prefix.push(messages[index].clone());
        index += 1;
    }
    for message in &messages[index..] {
        let starts_group = message.role() == "user" || groups.is_empty();
        if starts_group {
            groups.push(Vec::new());
        }
        groups
            .last_mut()
            .expect("group exists")
            .push(message.clone());
    }
    (prefix, groups)
}

/// 丢弃最旧的 `drop_groups` 组，保留至少一组。
///
/// 返回 `(messages, dropped)`；`dropped` 为实际丢弃的组数（可能小于请求值）。
pub fn drop_oldest_groups(
    messages: &[AgentMessage],
    drop_groups: usize,
) -> (Vec<AgentMessage>, usize) {
    let (prefix, groups) = group_messages(messages);
    if groups.len() <= 1 {
        return (messages.to_vec(), 0);
    }
    let dropped = drop_groups.min(groups.len() - 1);
    let mut out = prefix;
    for group in groups.into_iter().skip(dropped) {
        out.extend(group);
    }
    (out, dropped)
}

/// 生成最多 [`MAX_PTL_RETRIES`] 次 PTL 重试计划。
///
/// 第 N 个计划丢弃最旧的 N 个 group；计划长度受“至少留一组”限制。
pub fn plan_ptl_retries(messages: &[AgentMessage]) -> Vec<Vec<AgentMessage>> {
    let (_, groups) = group_messages(messages);
    if groups.len() <= 1 {
        return Vec::new();
    }
    let max_drop = MAX_PTL_RETRIES.min(groups.len() - 1);
    (1..=max_drop)
        .map(|drop| drop_oldest_groups(messages, drop).0)
        .collect()
}

/// 配对完整性检查：丢弃后不允许出现“tool_result 在 tool_call 之前”的孤立结果。
pub fn retry_plan_is_paired(original: &[AgentMessage], plan: &[AgentMessage]) -> bool {
    let original_calls: std::collections::HashSet<String> = original
        .iter()
        .filter_map(|message| match message {
            AgentMessage::Standard(Message::Assistant(assistant)) => Some(assistant),
            _ => None,
        })
        .flat_map(|assistant| assistant.content.iter())
        .filter_map(|block| match block {
            kiana_ai::types::AssistantContentBlock::ToolCall(call) => Some(call.id.clone()),
            _ => None,
        })
        .collect();
    let plan_calls: std::collections::HashSet<String> = plan
        .iter()
        .filter_map(|message| match message {
            AgentMessage::Standard(Message::Assistant(assistant)) => Some(assistant),
            _ => None,
        })
        .flat_map(|assistant| assistant.content.iter())
        .filter_map(|block| match block {
            kiana_ai::types::AssistantContentBlock::ToolCall(call) => Some(call.id.clone()),
            _ => None,
        })
        .collect();
    for message in plan {
        if let AgentMessage::Standard(Message::ToolResult(result)) = message
            && !plan_calls.contains(&result.tool_call_id)
            && original_calls.contains(&result.tool_call_id)
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> AgentMessage {
        AgentMessage::Standard(Message::User(kiana_ai::types::UserMessage {
            role: kiana_ai::types::UserRole::User,
            content: kiana_ai::types::UserContent::Text(text.to_string()),
            timestamp: 0,
        }))
    }

    fn assistant_with_call(id: &str) -> AgentMessage {
        AgentMessage::Standard(Message::Assistant(kiana_ai::types::AssistantMessage {
            role: kiana_ai::types::AssistantRole::Assistant,
            content: vec![kiana_ai::types::AssistantContentBlock::ToolCall(
                kiana_ai::types::ToolCall {
                    kind_tool_call: Default::default(),
                    id: id.to_string(),
                    name: "read".to_string(),
                    arguments: Default::default(),
                    thought_signature: None,
                    namespace: None,
                    arguments_error: None,
                },
            )],
            usage: Default::default(),
            stop_reason: kiana_ai::types::StopReason::Stop,
            timestamp: 0,
            ..Default::default()
        }))
    }

    fn tool_result(id: &str) -> AgentMessage {
        AgentMessage::Standard(Message::ToolResult(kiana_ai::types::ToolResultMessage {
            role: Default::default(),
            tool_call_id: id.to_string(),
            tool_name: "read".to_string(),
            content: vec![kiana_ai::types::UserContentBlock::Text(
                kiana_ai::types::TextContent {
                    kind_text: kiana_ai::types::TextKind::Text,
                    text: "ok".to_string(),
                    text_signature: None,
                },
            )],
            details: None,
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp: 0,
            duration_ms: None,
        }))
    }

    #[test]
    fn groups_start_at_user_and_keep_tool_pairs() {
        let messages = vec![
            user("one"),
            assistant_with_call("a"),
            tool_result("a"),
            user("two"),
            assistant_with_call("b"),
            tool_result("b"),
        ];
        let (_, groups) = group_messages(&messages);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 3);
        assert_eq!(groups[1].len(), 3);
    }

    #[test]
    fn drop_keeps_at_least_one_group_and_pairs() {
        let messages = vec![
            user("one"),
            assistant_with_call("a"),
            tool_result("a"),
            user("two"),
            assistant_with_call("b"),
            tool_result("b"),
        ];
        let (plan, dropped) = drop_oldest_groups(&messages, 99);
        assert_eq!(dropped, 1);
        assert_eq!(plan.len(), 3);
        assert!(retry_plan_is_paired(&messages, &plan));
        let (unchanged, dropped) = drop_oldest_groups(&[user("only")], 1);
        assert_eq!(dropped, 0);
        assert_eq!(unchanged.len(), 1);
    }

    #[test]
    fn plans_are_capped_at_three() {
        let mut messages = Vec::new();
        for index in 0..6 {
            messages.push(user(&format!("u{index}")));
            messages.push(assistant_with_call(&format!("c{index}")));
            messages.push(tool_result(&format!("c{index}")));
        }
        let plans = plan_ptl_retries(&messages);
        assert_eq!(plans.len(), 3);
        assert!(retry_plan_is_paired(&messages, &plans[0]));
        assert!(retry_plan_is_paired(&messages, &plans[2]));
    }

    #[test]
    fn overflow_recovers_only_once() {
        let mut recovery = OverflowRecovery::new();
        assert_eq!(recovery.on_overflow(), OverflowAction::CompactAndRetry);
        let second = recovery.on_overflow();
        assert!(matches!(second, OverflowAction::Fail { .. }));
        if let OverflowAction::Fail { message } = second {
            assert!(message.contains("one compact-and-retry attempt"));
        }
        recovery.reset();
        assert_eq!(recovery.on_overflow(), OverflowAction::CompactAndRetry);
    }
}
