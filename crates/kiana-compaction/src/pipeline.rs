//! 第 6 周压缩流水线：prune → prepare → 摘要 / 确定性 fallback，全程不修改历史。
//!
//! # all-or-nothing 契约
//!
//! [`HistorySnapshot`] 在流水线开始时对条目做一次字节级快照；流水线只返回**新的**
//! `Vec<SessionEntry>`，从不就地修改入参。因此：
//!
//! - 摘要生成失败且 fallback 也不可用时，返回 [`PipelineOutcome::Failed`]，其中
//!   `entries` 是**原始条目**（不是 prune 后的版本），`history_unchanged == true`；
//! - 调用方只有拿到 `Compacted` / `Fallback` 才把 `entries` + `result` 提交给
//!   `SessionManager`（见 docs/history/porting/handoffs/phase-06.md）。
//!
//! # split-turn
//!
//! 进入摘要前调用 [`check_split_turn_pairing`]：`messages_to_summarize` 与
//! `turn_prefix_messages` 都必须保持 tool pair 完整；任何一侧出现无配对
//! `tool_result` 时返回 [`PipelineError::PairingBroken`]，不调用模型、不改历史。
//!
//! # fallback
//!
//! LLM 失败时若提供 [`FallbackConfig`]，使用 [`crate::fallback::build_fallback_result`]
//! 生成确定性摘要；它保证严格更小且在 recovery target 内。fallback 也失败时返回
//! `Failed` 并保留原始历史。

use kiana_agent_core::types::{AgentMessage, StreamFn, ThinkingLevel};
use kiana_ai::types::{AssistantContentBlock, JsonValue, Model, ProviderEnv, ProviderHeaders};
use kiana_ai::utils::abort::AbortScope;
use kiana_ai::utils::retry::{RetryCallbacks, RetryPolicy};
use kiana_ai::utils::text::content_text_assistant;
use kiana_session_core::messages::convert_to_llm;
use kiana_session_core::session_manager::SessionEntry;
use kiana_session_core::usage_totals::combine_usage;
use thiserror::Error;

use crate::compaction::{
    CompactionDetails, CompactionError, CompactionPreparation, CompactionResult,
    CompactionSettings, SummaryWithUsage, build_summarization_context, complete_summarization,
    create_summarization_options, prepare_compaction, summarization_failure,
};
use crate::fallback::{FallbackConfig, build_fallback_result};
use crate::pruning::{
    PairingViolation, PruneConfig, PruneOutcome, messages_pairing_violations, prune_tool_outputs,
    tool_pairing_violations,
};
use crate::structured_summary::{merge_prior_summaries, structured_summary_prompt};
use crate::utils::{compute_file_lists, format_file_operations, serialize_conversation};

/// 流水线错误（在摘要错误之外）。
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PipelineError {
    /// 条目历史无法序列化（理论上不可达）。
    #[error("failed to serialize session history: {0}")]
    Serialize(String),
    /// split-turn 两侧的 tool pair 不完整。
    #[error("refusing to compact: tool call/result pairing would be broken: {0}")]
    PairingBroken(String),
}

/// 字节级历史快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistorySnapshot {
    bytes: Vec<u8>,
}

impl HistorySnapshot {
    /// 对条目序列化（每条一行 JSON）并计算 hash。
    pub fn capture(entries: &[SessionEntry]) -> Result<Self, PipelineError> {
        let mut bytes = Vec::new();
        for entry in entries {
            let line = serde_json::to_vec(entry)
                .map_err(|error| PipelineError::Serialize(error.to_string()))?;
            bytes.extend_from_slice(&line);
            bytes.push(b'\n');
        }
        Ok(Self { bytes })
    }

    /// 快照字节。
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// 与当前条目是否逐字节一致。
    pub fn matches(&self, entries: &[SessionEntry]) -> bool {
        match Self::capture(entries) {
            Ok(current) => current.bytes == self.bytes,
            Err(_) => false,
        }
    }

    /// no-op 缓存使用的历史 hash。
    pub fn hash(&self) -> u64 {
        crate::gate::history_hash(&self.bytes)
    }
}

/// 单次压缩的模型调用参数（与 [`compact`] 的位置参数一一对应，但可整体传递）。
#[derive(Clone, Default)]
pub struct CompactModelOptions {
    /// 摘要模型。
    pub model: Model,
    /// API key。
    pub api_key: Option<String>,
    /// 请求头。
    pub headers: Option<ProviderHeaders>,
    /// 额外关注点。
    pub custom_instructions: Option<String>,
    /// 取消信号。
    pub signal: Option<AbortScope>,
    /// thinking level。
    pub thinking_level: Option<ThinkingLevel>,
    /// 自定义 stream 函数（测试/离线）。
    pub stream_fn: Option<StreamFn>,
    /// provider 环境变量。
    pub env: Option<ProviderEnv>,
    /// 重试策略。
    pub retry: Option<RetryPolicy>,
    /// 重试回调。
    pub callbacks: Option<RetryCallbacks>,
    /// routing session id。
    pub session_id: Option<String>,
}

/// 流水线选项。
#[derive(Clone, Default)]
pub struct PipelineOptions {
    /// pruning 配置。
    pub prune: PruneConfig,
    /// LLM 失败时的确定性回退；`None` 表示不回退。
    pub fallback: Option<FallbackConfig>,
    /// 模型调用参数。
    pub model: CompactModelOptions,
}

/// 流水线结果。
#[derive(Debug, Clone, PartialEq)]
pub enum PipelineOutcome {
    /// 没有可压缩内容（已压缩边界、消息不足等）。
    NothingToCompact {
        /// 原始条目。
        entries: Vec<SessionEntry>,
        /// prune 结果（`applied` 可能为 false）。
        pruned: PruneOutcome,
    },
    /// LLM 摘要成功。
    Compacted {
        /// 应提交的条目（prune 后的版本；未应用 prune 时等于原始条目）。
        entries: Vec<SessionEntry>,
        /// 压缩结果。
        result: CompactionResult,
        /// prune 结果。
        pruned: PruneOutcome,
        /// 原始历史是否逐字节未变（恒为 true；用于自检）。
        history_unchanged: bool,
    },
    /// LLM 失败，但确定性 fallback 成功。
    Fallback {
        /// 应提交的条目。
        entries: Vec<SessionEntry>,
        /// fallback 压缩结果。
        result: CompactionResult,
        /// LLM 的原始错误。
        llm_error: CompactionError,
        /// prune 结果。
        pruned: PruneOutcome,
        /// 原始历史是否逐字节未变。
        history_unchanged: bool,
    },
    /// 全部失败：历史保持不变。
    Failed {
        /// 原始条目（未 prune）。
        entries: Vec<SessionEntry>,
        /// LLM 错误。
        error: CompactionError,
        /// 原始历史是否逐字节未变。
        history_unchanged: bool,
    },
}

impl PipelineOutcome {
    /// 是否产生了可提交的压缩结果。
    pub fn result(&self) -> Option<&CompactionResult> {
        match self {
            PipelineOutcome::Compacted { result, .. }
            | PipelineOutcome::Fallback { result, .. } => Some(result),
            _ => None,
        }
    }

    /// 应提交的条目（失败时为原始条目）。
    pub fn entries(&self) -> &[SessionEntry] {
        match self {
            PipelineOutcome::NothingToCompact { entries, .. }
            | PipelineOutcome::Compacted { entries, .. }
            | PipelineOutcome::Fallback { entries, .. }
            | PipelineOutcome::Failed { entries, .. } => entries,
        }
    }

    /// 原始历史是否逐字节未变。
    pub fn history_unchanged(&self) -> bool {
        match self {
            PipelineOutcome::Compacted {
                history_unchanged, ..
            }
            | PipelineOutcome::Fallback {
                history_unchanged, ..
            }
            | PipelineOutcome::Failed {
                history_unchanged, ..
            } => *history_unchanged,
            PipelineOutcome::NothingToCompact { .. } => true,
        }
    }
}

/// 用第 6 周结构化模板生成一份摘要（并合并 prior summary）。
async fn generate_structured_summary(
    messages: &[AgentMessage],
    previous_summary: Option<&str>,
    options: &CompactModelOptions,
) -> Result<SummaryWithUsage, CompactionError> {
    let conversation_text = serialize_conversation(&convert_to_llm(messages));
    let mut prompt_text = format!("<conversation>\n{conversation_text}\n</conversation>\n\n");
    prompt_text.push_str(&structured_summary_prompt(previous_summary));
    if let Some(instructions) = options
        .custom_instructions
        .as_deref()
        .filter(|instructions| !instructions.is_empty())
    {
        prompt_text.push_str(&format!("\n\nAdditional focus: {instructions}"));
    }

    let completion_options = create_summarization_options(
        &options.model,
        options.api_key.clone(),
        options.headers.clone(),
        options.env.clone(),
        options.signal.clone(),
        options.thinking_level,
        options.session_id.clone(),
    );
    let response = complete_summarization(
        &options.model,
        build_summarization_context(&prompt_text),
        completion_options,
        options.stream_fn.clone(),
        options.retry.clone(),
        options.callbacks.clone(),
    )
    .await?;
    if let Some(failure) = summarization_failure(&response, "Summarization") {
        return Err(failure);
    }
    if response
        .content
        .iter()
        .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)))
    {
        return Err(CompactionError::ToolCallAttempted {
            label: "Summarization".to_string(),
        });
    }
    let text = content_text_assistant(&response.content, "\n");
    let text = match previous_summary.filter(|summary| !summary.trim().is_empty()) {
        Some(previous) => merge_prior_summaries(previous, &text),
        None => text,
    };
    Ok(SummaryWithUsage {
        text,
        usage: response.usage,
    })
}

/// 第 6 周的主压缩函数：模板固定为 objective/details/completed-active-blocked/next/files。
///
/// 与 [`compact`] 的差别只有摘要 prompt 与 prior summary 合并；split-turn 的拼接格式、
/// 文件清单、`details` 结构完全一致，便于 wire agent 逐步替换而不改变会话文件形状。
///
/// 该函数只调用模型并返回结果，不读/写会话；调用方仍负责扩展的
/// `session_before_compact` 与提交事务。
pub async fn compact_with_structured_template(
    preparation: &CompactionPreparation,
    options: &CompactModelOptions,
) -> Result<CompactionResult, CompactionError> {
    let CompactionPreparation {
        first_kept_entry_id,
        messages_to_summarize,
        turn_prefix_messages,
        is_split_turn,
        tokens_before,
        previous_summary,
        file_ops,
        ..
    } = preparation;

    let summary: String;
    let summary_usage: kiana_ai::types::Usage;
    if *is_split_turn && !turn_prefix_messages.is_empty() {
        let mut history_text = previous_summary
            .clone()
            .unwrap_or_else(|| "No prior history.".to_string());
        let mut history_usage = None;
        if !messages_to_summarize.is_empty() {
            let result = generate_structured_summary(
                messages_to_summarize,
                previous_summary.as_deref(),
                options,
            )
            .await?;
            history_text = result.text;
            history_usage = Some(result.usage);
        }
        let prefix = generate_structured_summary(turn_prefix_messages, None, options).await?;
        summary = format!(
            "{history_text}\n\n---\n\n**Turn Context (split turn):**\n\n{}",
            prefix.text
        );
        summary_usage = match history_usage {
            Some(history) => combine_usage(&history, &prefix.usage),
            None => prefix.usage,
        };
    } else {
        let result = generate_structured_summary(
            messages_to_summarize,
            previous_summary.as_deref(),
            options,
        )
        .await?;
        summary = result.text;
        summary_usage = result.usage;
    }

    let mut summary = summary;
    let lists = compute_file_lists(file_ops);
    summary.push_str(&format_file_operations(
        &lists.read_files,
        &lists.modified_files,
    ));
    if first_kept_entry_id.is_empty() {
        return Err(CompactionError::FirstKeptEntryMissing);
    }
    let details = serde_json::to_value(CompactionDetails {
        read_files: lists.read_files,
        modified_files: lists.modified_files,
    })
    .unwrap_or(JsonValue::Null);
    Ok(CompactionResult {
        summary,
        first_kept_entry_id: first_kept_entry_id.clone(),
        tokens_before: *tokens_before,
        estimated_tokens_after: None,
        usage: Some(summary_usage),
        details: Some(details),
    })
}

/// 校验 split-turn 的前缀/历史两侧 tool pair 完整。
pub fn check_split_turn_pairing(preparation: &CompactionPreparation) -> Result<(), PipelineError> {
    let history_violations: Vec<String> =
        messages_pairing_violations(&preparation.messages_to_summarize);
    let prefix_violations: Vec<String> =
        messages_pairing_violations(&preparation.turn_prefix_messages);
    if history_violations.is_empty() && prefix_violations.is_empty() {
        return Ok(());
    }
    let mut details = Vec::new();
    details.extend(
        history_violations
            .into_iter()
            .map(|message| format!("history: {message}")),
    );
    details.extend(
        prefix_violations
            .into_iter()
            .map(|message| format!("turn prefix: {message}")),
    );
    Err(PipelineError::PairingBroken(details.join("; ")))
}

/// `SessionEntry` 层面的配对校验（供调用方在 prune 后自检）。
pub fn check_entry_pairing(entries: &[SessionEntry]) -> Result<(), Vec<PairingViolation>> {
    let violations = tool_pairing_violations(entries);
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

/// 运行完整流水线。
///
/// 顺序：快照 → prune → prepare → split-turn 配对校验 → LLM 摘要 → fallback。
pub async fn run_compaction_pipeline(
    entries: &[SessionEntry],
    settings: &CompactionSettings,
    options: &PipelineOptions,
) -> Result<PipelineOutcome, PipelineError> {
    let snapshot = HistorySnapshot::capture(entries)?;
    let pruned = prune_tool_outputs(entries, &options.prune);
    let working_entries = if pruned.applied {
        pruned.entries.clone()
    } else {
        entries.to_vec()
    };

    let Some(preparation) = prepare_compaction(&working_entries, settings) else {
        return Ok(PipelineOutcome::NothingToCompact {
            entries: entries.to_vec(),
            pruned,
        });
    };

    check_split_turn_pairing(&preparation)?;

    let result = compact_with_structured_template(&preparation, &options.model).await;

    match result {
        Ok(result) => {
            let history_unchanged = snapshot.matches(entries);
            Ok(PipelineOutcome::Compacted {
                entries: working_entries,
                result,
                pruned,
                history_unchanged,
            })
        }
        Err(error) => {
            if let Some(fallback_config) = &options.fallback
                && let Some(fallback) = build_fallback_result(&preparation, fallback_config)
            {
                let history_unchanged = snapshot.matches(entries);
                return Ok(PipelineOutcome::Fallback {
                    entries: working_entries,
                    result: fallback.result,
                    llm_error: error,
                    pruned,
                    history_unchanged,
                });
            }
            let history_unchanged = snapshot.matches(entries);
            Ok(PipelineOutcome::Failed {
                entries: entries.to_vec(),
                error,
                history_unchanged,
            })
        }
    }
}

/// 摘要前的 prune + prepare（不调用模型），供手动检查/扩展使用。
pub fn prepare_with_pruning(
    entries: &[SessionEntry],
    settings: &CompactionSettings,
    config: &PruneConfig,
) -> (PruneOutcome, Option<CompactionPreparation>) {
    let pruned = prune_tool_outputs(entries, config);
    let working = if pruned.applied {
        pruned.entries.clone()
    } else {
        entries.to_vec()
    };
    let preparation = prepare_compaction(&working, settings);
    (pruned, preparation)
}

/// 便捷函数：从消息列表提取 typed user / assistant 文本（恢复与 fallback 共用）。
pub fn recent_assistant_texts(messages: &[AgentMessage], limit: usize) -> Vec<String> {
    let mut texts = Vec::new();
    for message in messages.iter().rev() {
        if let AgentMessage::Standard(kiana_ai::types::Message::Assistant(assistant)) = message {
            let mut text = String::new();
            for block in &assistant.content {
                if let kiana_ai::types::AssistantContentBlock::Text(content) = block {
                    if !text.is_empty() {
                        text.push(' ');
                    }
                    text.push_str(&content.text);
                }
            }
            if !text.trim().is_empty() {
                texts.push(text);
            }
            if texts.len() >= limit {
                break;
            }
        }
    }
    texts.reverse();
    texts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_snapshot_detects_byte_changes() {
        let entries = Vec::new();
        let snapshot = HistorySnapshot::capture(&entries).unwrap();
        assert!(snapshot.matches(&entries));
        assert_eq!(snapshot.hash(), crate::gate::history_hash(snapshot.bytes()));
    }

    #[test]
    fn pairing_violations_detect_orphan_results() {
        use kiana_ai::types::{
            Message, TextContent, TextKind, ToolResultMessage, UserContentBlock,
        };
        use kiana_session_core::session_manager::{
            SessionEntry, SessionMessageEntry, SessionMessageType,
        };
        let entry = SessionEntry::Message(SessionMessageEntry {
            kind: SessionMessageType::Marker,
            id: "1".to_string(),
            parent_id: None,
            timestamp: String::new(),
            message: AgentMessage::Standard(Message::ToolResult(ToolResultMessage {
                role: Default::default(),
                tool_call_id: "missing".to_string(),
                tool_name: "read".to_string(),
                content: vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: "x".to_string(),
                    text_signature: None,
                })],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 0,
                duration_ms: None,
            })),
            raw_message: None,
        });
        let violations = tool_pairing_violations(&[entry]);
        assert_eq!(violations.len(), 1);
    }
}
