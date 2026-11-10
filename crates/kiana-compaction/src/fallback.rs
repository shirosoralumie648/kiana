//! 确定性 no-LLM 压缩回退。
//!
//! 设计来源（只借鉴行为，不复制代码）：
//! - 任务要求：“摘要失败时历史逐字节不变；确定性 no-LLM fallback（折叠旧 tool 活动，
//!   保留 typed user 和最近 assistant，严格更小且在 recovery target 内）”；
//! - `reference_project/qwen-code/packages/core/src/services/microcompaction/microcompact.ts`
//!   的“清除旧 tool 结果、保留最近消息”原则；
//! - `reference_project/hermes-agent/agent/context_compressor_prune.py` 的确定性
//!   prune（不调用模型）。
//!
//! # 何时使用
//!
//! LLM 摘要失败（网络、超时、token cap、tool-call 响应）或测试/离线环境没有模型时，
//! 调用 [`build_fallback_result`]。它只读取 [`CompactionPreparation`] 的输入消息，
//! 不读会话文件、不修改任何历史，因此天然满足 all-or-nothing。
//!
//! # 确定性
//!
//! 不读取墙钟、不生成 uuid、不使用 HashMap 迭代顺序；同一输入永远得到同一输出。
//! 这使它可以安全地作为“重试同一请求”的结果。
//!
//! # 更小的保证
//!
//! 只有当 `summary_tokens < 被折叠消息的估算 token` 时才返回 `Some`；否则返回 `None`
//! （调用方应保留原历史）。`summary_tokens` 同时被压到
//! [`FallbackConfig::recovery_target_tokens`] 以内。

use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::{AssistantContentBlock, JsonValue, Message};
use serde_json::json;

use crate::compaction::{
    CompactionDetails, CompactionPreparation, CompactionResult, estimate_tokens,
};
use crate::pruning::{is_tool_result, tool_result_text, typed_user_text};
use crate::utils::{compute_file_lists, format_file_operations};

/// 回退摘要配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackConfig {
    /// 摘要文本必须适配的 token 上限（recovery target）。
    pub recovery_target_tokens: u64,
    /// 保留多少条最近 assistant 文本。
    pub keep_recent_assistants: usize,
    /// 最多保留多少条 user 文本。
    pub max_user_lines: usize,
    /// 单条摘录的最大字符数。
    pub max_line_chars: usize,
}

impl Default for FallbackConfig {
    fn default() -> Self {
        Self {
            recovery_target_tokens: 20_000,
            keep_recent_assistants: 3,
            max_user_lines: 20,
            max_line_chars: 500,
        }
    }
}

/// 回退结果。
#[derive(Debug, Clone, PartialEq)]
pub struct FallbackSummary {
    /// 可直接提交的压缩结果（`usage == None`）。
    pub result: CompactionResult,
    /// 摘要文本的估算 token。
    pub summary_tokens: u64,
    /// 被折叠消息的估算 token。
    pub input_tokens: u64,
}

/// 文本的保守 token 估算：`ceil(chars / 4)`。
pub fn estimate_text_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

#[derive(Default)]
struct FoldedActivity {
    user_lines: Vec<String>,
    assistant_lines: Vec<String>,
    tool_counts: Vec<(String, u64)>,
    tool_paths: Vec<String>,
    tool_results: u64,
    tool_errors: u64,
    assistant_count: u64,
}

impl FoldedActivity {
    fn record_tool(&mut self, name: &str) {
        if let Some(entry) = self
            .tool_counts
            .iter_mut()
            .find(|(existing, _)| existing == name)
        {
            entry.1 += 1;
        } else {
            self.tool_counts.push((name.to_string(), 1));
        }
    }

    fn record_path(&mut self, path: &str) {
        if !path.is_empty() && !self.tool_paths.iter().any(|existing| existing == path) {
            self.tool_paths.push(path.to_string());
        }
    }
}

fn truncate_line(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    let mut out: String = trimmed.chars().take(max_chars).collect();
    if trimmed.chars().count() > max_chars {
        out.push('…');
    }
    out
}

fn fold(messages: &[AgentMessage], config: &FallbackConfig) -> FoldedActivity {
    let mut activity = FoldedActivity::default();
    for message in messages {
        if let Some(text) = typed_user_text(message) {
            activity
                .user_lines
                .push(truncate_line(&text, config.max_line_chars));
            continue;
        }
        match message {
            AgentMessage::Standard(Message::Assistant(assistant)) => {
                activity.assistant_count += 1;
                let mut text = String::new();
                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Text(content) => {
                            if !text.is_empty() {
                                text.push(' ');
                            }
                            text.push_str(&content.text);
                        }
                        AssistantContentBlock::ToolCall(call) => {
                            activity.record_tool(&call.name);
                            if let Some(path) = call
                                .arguments
                                .get("path")
                                .or_else(|| call.arguments.get("file_path"))
                                .and_then(JsonValue::as_str)
                            {
                                activity.record_path(path);
                            }
                        }
                        AssistantContentBlock::Thinking(_) => {}
                    }
                }
                if !text.trim().is_empty() {
                    activity
                        .assistant_lines
                        .push(truncate_line(&text, config.max_line_chars));
                }
            }
            AgentMessage::Standard(Message::ToolResult(result)) => {
                activity.tool_results += 1;
                if result.is_error {
                    activity.tool_errors += 1;
                }
                let text = tool_result_text(result);
                if !text.trim().is_empty() && activity.tool_paths.len() < 8 {
                    // 从常见的 `path:line:` 输出里提取第一段路径。
                    if let Some(candidate) = text.split(':').next()
                        && (candidate.contains('/') || candidate.contains('.'))
                    {
                        activity.record_path(candidate.trim());
                    }
                }
            }
            AgentMessage::Standard(_) => {}
            AgentMessage::Custom(_) => {}
        }
    }
    activity
}

fn render(activity: &FoldedActivity, config: &FallbackConfig) -> String {
    let objective = activity
        .user_lines
        .last()
        .cloned()
        .unwrap_or_else(|| "Continue the current task.".to_string());
    let mut out = String::new();
    out.push_str("## Objective\n- ");
    out.push_str(&objective);
    out.push_str("\n\n## Details\n- Deterministic no-LLM fallback summary (the summarizer did not produce a result).\n");
    out.push_str(&format!(
        "- Folded {} assistant message(s), {} tool call(s), {} tool result(s) ({} error(s)).\n",
        activity.assistant_count,
        activity
            .tool_counts
            .iter()
            .map(|(_, count)| count)
            .sum::<u64>(),
        activity.tool_results,
        activity.tool_errors
    ));
    out.push_str("\n## Completed\n");
    if activity.tool_counts.is_empty() {
        out.push_str("- None\n");
    } else {
        let tools: Vec<String> = activity
            .tool_counts
            .iter()
            .map(|(name, count)| format!("{name} x{count}"))
            .collect();
        out.push_str(&format!("- Tool activity: {}\n", tools.join(", ")));
    }
    out.push_str("\n## Active\n");
    let assistants: Vec<&String> = activity
        .assistant_lines
        .iter()
        .rev()
        .take(config.keep_recent_assistants)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if assistants.is_empty() {
        out.push_str("- None\n");
    } else {
        for line in assistants {
            out.push_str(&format!("- {line}\n"));
        }
    }
    out.push_str("\n## Blocked\n");
    if activity.tool_errors == 0 {
        out.push_str("- None\n");
    } else {
        out.push_str(&format!(
            "- {} tool call(s) failed; see the retained recent results.\n",
            activity.tool_errors
        ));
    }
    out.push_str("\n## Next\n- Re-read any file whose full contents are needed.\n- Continue from the most recent assistant message.\n");
    out.push_str("\n## Files\n");
    if activity.tool_paths.is_empty() {
        out.push_str("- None\n");
    } else {
        for path in &activity.tool_paths {
            out.push_str(&format!("- {path}\n"));
        }
    }
    out
}

/// 把文本压到 `max_tokens` 以内（按字符截断并补充省略标记）。
fn enforce_target(text: &str, max_tokens: u64) -> String {
    let max_chars = (max_tokens.saturating_mul(4)) as usize;
    if text.chars().count() <= max_chars || max_chars == 0 {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(32);
    let mut truncated: String = text.chars().take(keep).collect();
    truncated.push_str("\n… [fallback summary truncated]");
    truncated
}

/// 构造确定性回退压缩结果。
///
/// 返回 `None` 表示无法保证严格更小（例如输入本来就只有一条短消息），调用方应保留
/// 原历史并报告失败。
pub fn build_fallback_result(
    preparation: &CompactionPreparation,
    config: &FallbackConfig,
) -> Option<FallbackSummary> {
    let mut messages: Vec<AgentMessage> = preparation.messages_to_summarize.clone();
    messages.extend(preparation.turn_prefix_messages.iter().cloned());
    if messages.is_empty() {
        return None;
    }
    let input_tokens: u64 = messages.iter().map(estimate_tokens).sum();
    let activity = fold(&messages, config);

    // 先按用户可见上限构造，再逐步收紧。
    let mut text = render(&activity, config);
    let mut summary_tokens = estimate_text_tokens(&text);
    if summary_tokens >= input_tokens {
        // 尝试只保留 objective + files 的最小形式。
        let minimal = format!(
            "## Objective\n- {}\n\n## Details\n- Deterministic fallback.\n\n## Files\n{}\n",
            activity
                .user_lines
                .last()
                .cloned()
                .unwrap_or_else(|| "Continue the current task.".to_string()),
            if activity.tool_paths.is_empty() {
                "- None".to_string()
            } else {
                activity
                    .tool_paths
                    .iter()
                    .map(|path| format!("- {path}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        );
        text = minimal;
        summary_tokens = estimate_text_tokens(&text);
    }
    if summary_tokens >= input_tokens {
        return None;
    }
    if summary_tokens > config.recovery_target_tokens {
        text = enforce_target(&text, config.recovery_target_tokens);
        summary_tokens = estimate_text_tokens(&text);
    }
    if summary_tokens >= input_tokens {
        return None;
    }

    let lists = compute_file_lists(&preparation.file_ops);
    let mut summary = text;
    summary.push_str(&format_file_operations(
        &lists.read_files,
        &lists.modified_files,
    ));
    // 文件清单可能把摘要重新推过 recovery target，甚至超过原历史（大量 tool_ops 时）。
    // 追加之后必须再 enforce 一次；仍不满足严格更小/目标上限就放弃 fallback，交给调用方
    // 保留原历史，绝不能提交比输入更大的“压缩”结果。
    let mut summary_tokens = estimate_text_tokens(&summary);
    if summary_tokens > config.recovery_target_tokens {
        summary = enforce_target(&summary, config.recovery_target_tokens);
        summary_tokens = estimate_text_tokens(&summary);
    }
    if summary_tokens >= input_tokens {
        return None;
    }
    let details = serde_json::to_value(CompactionDetails {
        read_files: lists.read_files,
        modified_files: lists.modified_files,
    })
    .unwrap_or(JsonValue::Null);

    Some(FallbackSummary {
        result: CompactionResult {
            summary,
            first_kept_entry_id: preparation.first_kept_entry_id.clone(),
            tokens_before: preparation.tokens_before,
            estimated_tokens_after: None,
            usage: None,
            details: Some(details),
        },
        summary_tokens,
        input_tokens,
    })
}

/// 仅用于测试/诊断：回退结果是否严格小于输入。
pub fn is_strictly_smaller(result: &FallbackSummary) -> bool {
    result.summary_tokens < result.input_tokens
}

/// 结构化 details 里的回退标记。
pub fn fallback_details_marker() -> JsonValue {
    json!({"fallback": "no-llm", "version": 1})
}

/// 判断消息列表里是否全是 tool 活动（没有 typed user）。
pub fn has_typed_user(messages: &[AgentMessage]) -> bool {
    messages
        .iter()
        .any(|message| typed_user_text(message).is_some())
}

/// 判断是否包含 tool result（供调用方决定是否值得折叠）。
pub fn has_tool_activity(messages: &[AgentMessage]) -> bool {
    messages.iter().any(is_tool_result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message_entry(
        id: &str,
        message: AgentMessage,
    ) -> kiana_session_core::session_manager::SessionEntry {
        kiana_session_core::session_manager::SessionEntry::Message(
            kiana_session_core::session_manager::SessionMessageEntry {
                kind: kiana_session_core::session_manager::SessionMessageType::Marker,
                id: id.to_string(),
                parent_id: None,
                timestamp: String::new(),
                message,
                raw_message: None,
            },
        )
    }

    fn preparation(messages: Vec<AgentMessage>) -> CompactionPreparation {
        let tokens: u64 = messages.iter().map(estimate_tokens).sum();
        CompactionPreparation {
            first_kept_entry_id: "kept".to_string(),
            messages_to_summarize: messages,
            turn_prefix_messages: Vec::new(),
            is_split_turn: false,
            tokens_before: tokens,
            previous_summary: None,
            file_ops: Default::default(),
            settings: Default::default(),
        }
    }

    fn user(text: &str) -> AgentMessage {
        AgentMessage::Standard(Message::User(kiana_ai::types::UserMessage {
            role: kiana_ai::types::UserRole::User,
            content: kiana_ai::types::UserContent::Text(text.to_string()),
            timestamp: 0,
        }))
    }

    fn assistant(text: &str) -> AgentMessage {
        AgentMessage::Standard(Message::Assistant(kiana_ai::types::AssistantMessage {
            role: kiana_ai::types::AssistantRole::Assistant,
            content: vec![AssistantContentBlock::Text(kiana_ai::types::TextContent {
                kind_text: kiana_ai::types::TextKind::Text,
                text: text.to_string(),
                text_signature: None,
            })],
            usage: Default::default(),
            stop_reason: kiana_ai::types::StopReason::Stop,
            timestamp: 0,
            ..Default::default()
        }))
    }

    fn tool_result(tool: &str, text: &str, id: &str) -> AgentMessage {
        AgentMessage::Standard(Message::ToolResult(kiana_ai::types::ToolResultMessage {
            role: Default::default(),
            tool_call_id: id.to_string(),
            tool_name: tool.to_string(),
            content: vec![kiana_ai::types::UserContentBlock::Text(
                kiana_ai::types::TextContent {
                    kind_text: kiana_ai::types::TextKind::Text,
                    text: text.to_string(),
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
    fn fallback_is_deterministic_and_strictly_smaller() {
        let mut messages = Vec::new();
        for index in 0..30 {
            messages.push(user(&format!("user message {index} with some detail")));
            messages.push(assistant(&format!(
                "assistant answer {index} with a fairly long explanation of what happened"
            )));
            messages.push(tool_result(
                "read",
                &format!("src/file{index}.rs:1:line content repeated repeated repeated"),
                &format!("call-{index}"),
            ));
        }
        let preparation = preparation(messages);
        let config = FallbackConfig::default();
        let first = build_fallback_result(&preparation, &config).expect("fallback");
        let second = build_fallback_result(&preparation, &config).expect("fallback");
        assert_eq!(first.result.summary, second.result.summary);
        assert!(is_strictly_smaller(&first));
        assert!(first.summary_tokens <= config.recovery_target_tokens);
        assert!(first.result.summary.contains("## Objective"));
        assert!(first.result.summary.contains("recent"));
    }

    #[test]
    fn fallback_returns_none_for_tiny_input() {
        let preparation = preparation(vec![user("hi")]);
        assert!(build_fallback_result(&preparation, &FallbackConfig::default()).is_none());
    }

    #[test]
    fn fallback_respects_tight_target() {
        let mut messages = Vec::new();
        for index in 0..50 {
            messages.push(user(&format!("user {index} {}", "detail ".repeat(100))));
            messages.push(assistant(&format!(
                "assistant {index} {}",
                "answer ".repeat(100)
            )));
        }
        let preparation = preparation(messages);
        let config = FallbackConfig {
            recovery_target_tokens: 300,
            ..FallbackConfig::default()
        };
        let fallback = build_fallback_result(&preparation, &config).expect("fallback");
        assert!(fallback.summary_tokens <= 300 + 40);
        assert!(is_strictly_smaller(&fallback));
    }

    #[test]
    fn fallback_enforces_target_after_appending_a_large_file_list() {
        let mut messages = Vec::new();
        for index in 0..50 {
            messages.push(user(&format!("user {index} {}", "detail ".repeat(100))));
            messages.push(assistant(&format!(
                "assistant {index} {}",
                "answer ".repeat(100)
            )));
        }
        let mut preparation = preparation(messages);
        for index in 0..500 {
            preparation
                .file_ops
                .read
                .insert(format!("src/generated/very/long/path/file_{index}.rs").into());
        }
        let config = FallbackConfig {
            recovery_target_tokens: 300,
            ..FallbackConfig::default()
        };
        let fallback = build_fallback_result(&preparation, &config).expect("fallback");
        // 追加文件清单后必须重新收紧，不能被清单推过 recovery target。
        assert!(
            fallback.summary_tokens <= config.recovery_target_tokens + 40,
            "summary_tokens={} target={}",
            fallback.summary_tokens,
            config.recovery_target_tokens
        );
        assert!(is_strictly_smaller(&fallback));
    }

    #[test]
    fn fallback_rejects_a_file_list_that_cannot_be_made_smaller() {
        // 输入本身只有很短的一条消息，文件清单却很大：无论怎么截断都不可能严格更小，
        // 必须返回 None，让调用方保留原历史。
        let mut preparation = preparation(vec![user(&"hi ".repeat(100))]);
        for index in 0..2000 {
            preparation
                .file_ops
                .read
                .insert(format!("src/generated/file_{index}.rs").into());
        }
        assert!(build_fallback_result(&preparation, &FallbackConfig::default()).is_none());
    }

    #[test]
    fn helper_entry_is_unused_but_compiles() {
        let entry = message_entry("x", user("x"));
        assert_eq!(entry.id(), "x");
    }
}
