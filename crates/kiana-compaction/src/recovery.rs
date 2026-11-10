//! 压缩后的上下文恢复：文件读取、已调用技能、plan/plan mode、工具/agent/MCP 增量。
//!
//! 设计来源（只借鉴行为，不复制代码）：
//! - `reference_project/ClaudeCode/src/utils/fileReadCache.ts`：压缩后恢复最近读过的
//!   文件，未恢复的路径由 read gate 拒绝编辑；
//! - `reference_project/qwen-code/packages/core/src/services/fileReadCache.integration.test.ts`：
//!   “恢复快照而不是伪造 tool result”的集成契约；
//! - `reference_project/oh-my-pi/packages/coding-agent/src/session/session-maintenance.ts`
//!   与 `tool-protection.ts`：skill 读取与 active plan 的保护；恢复内容按预算裁剪，
//!   缺失只 warning；
//! - 任务要求：最近 5 文件（总 50k、单文件 5k）、技能（总 25k、单技能 5k）、
//!   plan/plan mode、工具/agent/MCP 增量；hidden context/system attachment，不伪造
//!   tool result。
//!
//! # 输出形态
//!
//! [`RecoveryAttachment`] 只是一段带 `<post-compaction-recovery>` 包裹的文本；调用方
//! 把它作为 **system 消息 / hidden context** 追加到下一次请求，**不要**生成
//! `toolResult` 消息。文本明确写明“这不是新的工具结果”，避免模型据此认为自己重新读过
//! 文件。
//!
//! # 预算与缺失
//!
//! - 文件：按 `captured_at_ms` 取最近 5 个；单文件 excerpt 最多 5k 字符；合计 50k；
//! - 技能：按调用时间取最近；单技能 5k；合计 25k；
//! - 缺失内容（`content_excerpt` 为空、技能正文缺失、plan 内容缺失）只记 warning，
//!   不阻塞压缩；对应的 read state 仍按 hash/range 交给 kiana-tools 尝试恢复。

use std::collections::HashSet;

use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::{AssistantContentBlock, JsonValue, Message, ToolCall};
use kiana_core::file_read_cache::{FileReadSnapshot, ReadLineRange, truncate_chars};
use kiana_session_core::session_manager::SessionEntry;

use crate::pruning::{tool_result_message, tool_result_text};

/// 恢复预算。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryBudget {
    /// 最多恢复多少个文件。
    pub max_files: usize,
    /// 单文件 excerpt 字符上限。
    pub max_file_chars: usize,
    /// 全部文件 excerpt 字符上限。
    pub max_files_total_chars: usize,
    /// 全部技能正文上限。
    pub max_skills_total_chars: usize,
    /// 单技能正文上限。
    pub max_skill_chars: usize,
}

impl Default for RecoveryBudget {
    fn default() -> Self {
        Self {
            max_files: 5,
            max_file_chars: 5_000,
            max_files_total_chars: 50_000,
            max_skills_total_chars: 25_000,
            max_skill_chars: 5_000,
        }
    }
}

/// 一次已调用的技能。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalledSkill {
    /// 技能名。
    pub name: String,
    /// 技能正文（调用时加载的内容）。
    pub content: String,
    /// 调用时间（Unix 毫秒）。
    pub called_at_ms: i64,
    /// 可选路径（诊断用）。
    pub path: Option<String>,
}

/// plan / plan mode 状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanRecovery {
    /// plan 文件路径。
    pub path: Option<String>,
    /// plan 内容。
    pub content: Option<String>,
    /// 是否处于 plan mode。
    pub plan_mode: bool,
    /// plan 是否已获批准。
    pub approved: bool,
}

/// 工具 / agent / MCP 增量。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextDeltas {
    /// 新增（或本轮可用）的工具名。
    pub tools: Vec<String>,
    /// 新增/恢复的 agent 名。
    pub agents: Vec<String>,
    /// 新增/重连的 MCP server 名。
    pub mcp_servers: Vec<String>,
}

/// 恢复输入。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryInput {
    /// 压缩前的文件读取快照（通常来自 `ReadStateRegistry::snapshots`）。
    pub files: Vec<FileReadSnapshot>,
    /// 已调用技能（从旧到新）。
    pub skills: Vec<CalledSkill>,
    /// plan 状态。
    pub plan: PlanRecovery,
    /// 工具/agent/MCP 增量。
    pub deltas: ContextDeltas,
}

/// 恢复后仍应 re-arm read gate 的文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredFile {
    /// 路径。
    pub path: std::path::PathBuf,
    /// 内容指纹。
    pub content_hash: u64,
    /// 文件总行数。
    pub line_count: u64,
    /// 行范围。
    pub ranges: Vec<ReadLineRange>,
    /// 是否文本。
    pub text: bool,
    /// excerpt 是否已包含在 attachment 中。
    pub excerpt_included: bool,
}

/// 恢复的技能。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredSkill {
    /// 技能名。
    pub name: String,
    /// 是否包含正文。
    pub content_included: bool,
}

/// 恢复附件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryAttachment {
    /// 作为 hidden context / system attachment 的文本。
    pub text: String,
    /// 非阻塞 warning（缺失文件、预算裁剪等）。
    pub warnings: Vec<String>,
    /// 需要 re-arm read gate 的文件。
    pub restored_files: Vec<RestoredFile>,
    /// 恢复的技能。
    pub restored_skills: Vec<RestoredSkill>,
    /// plan 是否恢复。
    pub plan_restored: bool,
    /// 恒为 true；提醒调用方这是 hidden context，不是 tool result。
    pub hidden: bool,
}

impl RecoveryAttachment {
    /// 没有可恢复内容（调用方仍可忽略 warning）。
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
            || (self.restored_files.is_empty()
                && self.restored_skills.is_empty()
                && !self.plan_restored
                && self.text.contains("## Context changes\n- None"))
    }
}

/// 从会话条目中收集已调用技能。
///
/// 识别两类：
/// - 工具名 `skill` / `skill_read`，参数 `name` 或 `path`；
/// - `read` 的路径以 `skill://` 开头（技能文档读取）。
///
/// 技能正文取配对 tool result 的文本；同一技能保留最新一次调用。返回按调用时间从新
/// 到旧排序、最多 `limit` 条。没有正文的结果仍会返回（`content` 为空），恢复时只
/// warning。
pub fn collect_called_skills(entries: &[SessionEntry], limit: usize) -> Vec<CalledSkill> {
    let mut calls: std::collections::HashMap<String, ToolCall> = std::collections::HashMap::new();
    for entry in entries {
        let SessionEntry::Message(message_entry) = entry else {
            continue;
        };
        let AgentMessage::Standard(Message::Assistant(assistant)) = &message_entry.message else {
            continue;
        };
        for block in &assistant.content {
            if let AssistantContentBlock::ToolCall(call) = block {
                calls.insert(call.id.clone(), call.clone());
            }
        }
    }

    let mut skills: Vec<CalledSkill> = Vec::new();
    for entry in entries {
        let Some(result) = tool_result_message(entry) else {
            continue;
        };
        let Some(call) = calls.get(&result.tool_call_id) else {
            continue;
        };
        let is_skill_tool = matches!(call.name.as_str(), "skill" | "skill_read");
        let read_skill_path = call
            .arguments
            .get("path")
            .and_then(JsonValue::as_str)
            .is_some_and(|path| path.starts_with("skill://"));
        if !is_skill_tool && !read_skill_path {
            continue;
        }
        let name = call
            .arguments
            .get("name")
            .and_then(JsonValue::as_str)
            .or_else(|| call.arguments.get("path").and_then(JsonValue::as_str))
            .unwrap_or(&call.name)
            .to_string();
        let path = call
            .arguments
            .get("path")
            .and_then(JsonValue::as_str)
            .map(str::to_string);
        let content = tool_result_text(result);
        if let Some(existing) = skills.iter_mut().find(|skill| skill.name == name) {
            if result.timestamp >= existing.called_at_ms {
                *existing = CalledSkill {
                    name,
                    content,
                    called_at_ms: result.timestamp,
                    path,
                };
            }
        } else {
            skills.push(CalledSkill {
                name,
                content,
                called_at_ms: result.timestamp,
                path,
            });
        }
    }
    skills.sort_by(|left, right| right.called_at_ms.cmp(&left.called_at_ms));
    skills.truncate(limit);
    skills
}

/// 从会话条目收集工具 / agent / MCP 增量。
///
/// - `tools`：出现过的工具名（去重、保持首次出现顺序）；
/// - `agents`：`agent` / `task` / `subagent` 调用的参数 `name`/`subagent_type`；
/// - `mcp_servers`：工具名 `mcp__<server>__<tool>` 里的 server 段。
///
/// 这是压缩后附件的最小信息量；上层有更精确的运行时增量时可以覆盖。
pub fn collect_context_deltas(entries: &[SessionEntry]) -> ContextDeltas {
    let mut deltas = ContextDeltas::default();
    let mut seen_tools = HashSet::new();
    for entry in entries {
        let SessionEntry::Message(message_entry) = entry else {
            continue;
        };
        let AgentMessage::Standard(Message::Assistant(assistant)) = &message_entry.message else {
            continue;
        };
        for block in &assistant.content {
            let AssistantContentBlock::ToolCall(call) = block else {
                continue;
            };
            if seen_tools.insert(call.name.clone()) {
                deltas.tools.push(call.name.clone());
            }
            if matches!(call.name.as_str(), "agent" | "task" | "subagent") {
                let name = call
                    .arguments
                    .get("name")
                    .or_else(|| call.arguments.get("subagent_type"))
                    .and_then(JsonValue::as_str);
                if let Some(name) = name
                    && !deltas.agents.iter().any(|existing| existing == name)
                {
                    deltas.agents.push(name.to_string());
                }
            }
            if let Some(rest) = call.name.strip_prefix("mcp__")
                && let Some((server, _)) = rest.split_once("__")
                && !deltas.mcp_servers.iter().any(|existing| existing == server)
            {
                deltas.mcp_servers.push(server.to_string());
            }
        }
    }
    deltas
}

/// 构造恢复附件。
pub fn build_recovery_attachment(
    input: &RecoveryInput,
    budget: &RecoveryBudget,
) -> RecoveryAttachment {
    let mut warnings = Vec::new();
    let mut text = String::from(
        "<post-compaction-recovery>\n\
         Hidden context restored after compaction. This is NOT a new tool result; the files \
         below are excerpts from earlier reads, not fresh reads.\n\n",
    );

    // ---- 文件 ----
    let restored_files = select_files(&input.files, budget, &mut warnings);
    text.push_str("## Files (recent reads)\n");
    if restored_files.is_empty() {
        text.push_str("- None\n");
    } else {
        for restored in &restored_files {
            let source = input
                .files
                .iter()
                .find(|snapshot| snapshot.path == restored.path)
                .expect("selected file exists");
            text.push_str(&format!("### {}\n", restored.path.display()));
            if let Some(range) = source.ranges.first() {
                text.push_str(&format!(
                    "lines {}-{}{}\n",
                    range.start,
                    range.end,
                    if source.covers_all_lines() {
                        " (whole file)"
                    } else {
                        ""
                    }
                ));
            }
            if restored.excerpt_included {
                let excerpt = truncate_chars(&source.content_excerpt, budget.max_file_chars);
                text.push_str(&excerpt);
                if !excerpt.ends_with('\n') {
                    text.push('\n');
                }
            } else {
                text.push_str("(content excerpt unavailable; read state will be restored if the file is unchanged)\n");
            }
        }
    }

    // ---- 技能 ----
    text.push_str("\n## Skills (previously invoked)\n");
    let restored_skills = select_skills(&input.skills, budget, &mut warnings);
    if restored_skills.is_empty() {
        text.push_str("- None\n");
    } else {
        for skill in &restored_skills {
            text.push_str(&format!("### {}\n", skill.name));
            if let Some(source) = input
                .skills
                .iter()
                .find(|candidate| candidate.name == skill.name)
            {
                if skill.content_included {
                    let body = truncate_chars(&source.content, budget.max_skill_chars);
                    text.push_str(&body);
                    if !body.ends_with('\n') {
                        text.push('\n');
                    }
                } else {
                    text.push_str("(skill body unavailable)\n");
                }
            }
        }
    }

    // ---- plan ----
    text.push_str("\n## Plan\n");
    let mut plan_restored = false;
    if input.plan.plan_mode || input.plan.path.is_some() || input.plan.content.is_some() {
        text.push_str(&format!(
            "- Plan mode: {}\n- Approved: {}\n",
            if input.plan.plan_mode {
                "active"
            } else {
                "inactive"
            },
            if input.plan.approved { "yes" } else { "no" }
        ));
        if let Some(path) = &input.plan.path {
            text.push_str(&format!("- Plan file: {path}\n"));
        }
        match &input.plan.content {
            Some(content) if !content.trim().is_empty() => {
                plan_restored = true;
                text.push_str("\n");
                text.push_str(content);
                if !content.ends_with('\n') {
                    text.push('\n');
                }
            }
            _ => {
                if input.plan.path.is_some() {
                    warnings.push(
                        "plan content unavailable; plan file will need a fresh read".to_string(),
                    );
                }
                text.push_str("- Plan content: unavailable\n");
            }
        }
    } else {
        text.push_str("- None\n");
    }

    // ---- 工具/agent/MCP 增量 ----
    text.push_str("\n## Context changes\n");
    let mut any_delta = false;
    if !input.deltas.tools.is_empty() {
        any_delta = true;
        text.push_str(&format!("- Tools: {}\n", input.deltas.tools.join(", ")));
    }
    if !input.deltas.agents.is_empty() {
        any_delta = true;
        text.push_str(&format!("- Agents: {}\n", input.deltas.agents.join(", ")));
    }
    if !input.deltas.mcp_servers.is_empty() {
        any_delta = true;
        text.push_str(&format!(
            "- MCP servers: {}\n",
            input.deltas.mcp_servers.join(", ")
        ));
    }
    if !any_delta {
        text.push_str("- None\n");
    }
    text.push_str("</post-compaction-recovery>");

    RecoveryAttachment {
        text,
        warnings,
        restored_files,
        restored_skills,
        plan_restored,
        hidden: true,
    }
}

fn select_files(
    files: &[FileReadSnapshot],
    budget: &RecoveryBudget,
    warnings: &mut Vec<String>,
) -> Vec<RestoredFile> {
    // 去重（同路径取最新），按时间从新到旧取前 N 个。
    let mut deduped: Vec<&FileReadSnapshot> = Vec::new();
    for snapshot in files {
        if let Some(existing) = deduped
            .iter_mut()
            .find(|existing| existing.path == snapshot.path)
        {
            if snapshot.captured_at_ms >= existing.captured_at_ms {
                *existing = snapshot;
            }
        } else {
            deduped.push(snapshot);
        }
    }
    deduped.sort_by(|left, right| right.captured_at_ms.cmp(&left.captured_at_ms));

    let mut selected = Vec::new();
    let mut total_chars = 0usize;
    for snapshot in deduped {
        if selected.len() >= budget.max_files {
            warnings.push(format!(
                "recovery budget: only the most recent {} files were restored",
                budget.max_files
            ));
            break;
        }
        if !snapshot.text {
            warnings.push(format!(
                "non-text read not restored: {}",
                snapshot.path.display()
            ));
            continue;
        }
        let excerpt_chars = snapshot.content_excerpt.chars().count();
        let include_excerpt = excerpt_chars > 0
            && total_chars + excerpt_chars.min(budget.max_file_chars)
                <= budget.max_files_total_chars;
        if excerpt_chars == 0 {
            warnings.push(format!(
                "content excerpt unavailable for {}; read state will still be restored",
                snapshot.path.display()
            ));
        } else if !include_excerpt {
            warnings.push(format!(
                "file excerpt omitted due to total recovery budget: {}",
                snapshot.path.display()
            ));
        }
        let included_chars = if include_excerpt {
            excerpt_chars.min(budget.max_file_chars)
        } else {
            0
        };
        total_chars += included_chars;
        selected.push(RestoredFile {
            path: snapshot.path.clone(),
            content_hash: snapshot.content_hash,
            line_count: snapshot.line_count,
            ranges: snapshot.ranges.clone(),
            text: snapshot.text,
            excerpt_included: include_excerpt,
        });
    }
    selected
}

fn select_skills(
    skills: &[CalledSkill],
    budget: &RecoveryBudget,
    warnings: &mut Vec<String>,
) -> Vec<RestoredSkill> {
    let mut deduped: Vec<&CalledSkill> = Vec::new();
    for skill in skills {
        if let Some(existing) = deduped
            .iter_mut()
            .find(|existing| existing.name == skill.name)
        {
            if skill.called_at_ms >= existing.called_at_ms {
                *existing = skill;
            }
        } else {
            deduped.push(skill);
        }
    }
    deduped.sort_by(|left, right| right.called_at_ms.cmp(&left.called_at_ms));

    let mut selected = Vec::new();
    let mut total_chars = 0usize;
    let mut seen: HashSet<String> = HashSet::new();
    for skill in deduped {
        if !seen.insert(skill.name.clone()) {
            continue;
        }
        let body_chars = skill.content.chars().count();
        let per_skill = body_chars.min(budget.max_skill_chars);
        let include = per_skill > 0 && total_chars + per_skill <= budget.max_skills_total_chars;
        if body_chars == 0 {
            warnings.push(format!("skill body unavailable: {}", skill.name));
        } else if !include {
            warnings.push(format!(
                "skill body omitted due to recovery budget: {}",
                skill.name
            ));
        }
        if include {
            total_chars += per_skill;
        }
        selected.push(RestoredSkill {
            name: skill.name.clone(),
            content_included: include,
        });
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn snapshot(path: &str, at: i64, chars: usize, text: bool) -> FileReadSnapshot {
        FileReadSnapshot {
            path: PathBuf::from(path),
            content_hash: 7,
            line_count: 100,
            ranges: vec![ReadLineRange::new(1, 100)],
            text,
            captured_at_ms: at,
            content_excerpt: "x".repeat(chars),
        }
    }

    #[test]
    fn restores_only_five_most_recent_files_within_budget() {
        let files: Vec<_> = (0..8)
            .map(|index| snapshot(&format!("/f{index}"), index, 20_000, true))
            .collect();
        let attachment = build_recovery_attachment(
            &RecoveryInput {
                files,
                ..RecoveryInput::default()
            },
            &RecoveryBudget::default(),
        );
        assert_eq!(attachment.restored_files.len(), 5);
        let names: Vec<_> = attachment
            .restored_files
            .iter()
            .map(|file| file.path.to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["/f7", "/f6", "/f5", "/f4", "/f3"]);
        for file in &attachment.restored_files {
            let source = attachment
                .text
                .split("### ")
                .find(|section| section.starts_with(&file.path.to_string_lossy().to_string()));
            if let Some(section) = source {
                let body = section.split('\n').nth(2).unwrap_or("");
                assert!(body.chars().count() <= 5_000);
            }
        }
        let total_excerpt: usize = attachment
            .restored_files
            .iter()
            .filter(|file| file.excerpt_included)
            .count()
            * 5_000;
        assert!(total_excerpt <= 50_000);
    }

    #[test]
    fn missing_content_only_warns() {
        let attachment = build_recovery_attachment(
            &RecoveryInput {
                files: vec![snapshot("/a", 1, 0, true)],
                skills: vec![CalledSkill {
                    name: "deploy".to_string(),
                    content: String::new(),
                    called_at_ms: 5,
                    path: None,
                }],
                plan: PlanRecovery {
                    path: Some("PLAN.md".to_string()),
                    content: None,
                    plan_mode: true,
                    approved: false,
                },
                ..RecoveryInput::default()
            },
            &RecoveryBudget::default(),
        );
        assert!(attachment.warnings.len() >= 3);
        assert!(attachment.restored_files[0].excerpt_included == false);
        assert!(!attachment.plan_restored);
        assert!(attachment.hidden);
        assert!(attachment.text.contains("<post-compaction-recovery>"));
        assert!(attachment.text.contains("NOT a new tool result"));
    }

    #[test]
    fn skills_have_total_and_per_skill_budgets() {
        let skills: Vec<_> = (0..10)
            .map(|index| CalledSkill {
                name: format!("skill-{index}"),
                content: "s".repeat(10_000),
                called_at_ms: index,
                path: None,
            })
            .collect();
        let attachment = build_recovery_attachment(
            &RecoveryInput {
                skills,
                ..RecoveryInput::default()
            },
            &RecoveryBudget::default(),
        );
        let included = attachment
            .restored_skills
            .iter()
            .filter(|skill| skill.content_included)
            .count();
        assert_eq!(included, 5);
        let text_len = attachment.text.len();
        assert!(text_len < 25_000 + 10_000);
    }

    #[test]
    fn plan_and_deltas_are_included() {
        let attachment = build_recovery_attachment(
            &RecoveryInput {
                plan: PlanRecovery {
                    path: Some("PLAN.md".to_string()),
                    content: Some("- step 1\n- step 2\n".to_string()),
                    plan_mode: true,
                    approved: true,
                },
                deltas: ContextDeltas {
                    tools: vec!["read".to_string()],
                    agents: vec!["explore".to_string()],
                    mcp_servers: vec!["github".to_string()],
                },
                ..RecoveryInput::default()
            },
            &RecoveryBudget::default(),
        );
        assert!(attachment.plan_restored);
        assert!(attachment.text.contains("- step 1"));
        assert!(attachment.text.contains("Tools: read"));
        assert!(attachment.text.contains("Agents: explore"));
        assert!(attachment.text.contains("MCP servers: github"));
    }

    #[test]
    fn non_text_reads_are_skipped_with_warning() {
        let attachment = build_recovery_attachment(
            &RecoveryInput {
                files: vec![snapshot("/image.png", 1, 100, false)],
                ..RecoveryInput::default()
            },
            &RecoveryBudget::default(),
        );
        assert!(attachment.restored_files.is_empty());
        assert!(
            attachment
                .warnings
                .iter()
                .any(|warning| warning.contains("non-text"))
        );
    }

    #[test]
    fn collects_called_skills_from_entries() {
        use kiana_ai::types::{
            AssistantMessage, AssistantRole, TextContent, TextKind, ToolCall, ToolCallKind,
            ToolResultMessage, UserContentBlock,
        };
        use kiana_session_core::session_manager::{
            SessionEntry, SessionMessageEntry, SessionMessageType,
        };

        fn entry(id: &str, message: AgentMessage) -> SessionEntry {
            SessionEntry::Message(SessionMessageEntry {
                kind: SessionMessageType::Marker,
                id: id.to_string(),
                parent_id: None,
                timestamp: String::new(),
                message,
                raw_message: None,
            })
        }
        fn call(id: &str, name: &str, args: JsonValue) -> AgentMessage {
            let mut arguments = kiana_ai::types::JsonObject::default();
            for (key, value) in args.as_object().unwrap() {
                arguments.insert(key.clone(), value.clone());
            }
            AgentMessage::Standard(Message::Assistant(AssistantMessage {
                role: AssistantRole::Assistant,
                content: vec![AssistantContentBlock::ToolCall(ToolCall {
                    kind_tool_call: ToolCallKind::default(),
                    id: id.to_string(),
                    name: name.to_string(),
                    arguments,
                    thought_signature: None,
                    namespace: None,
                    arguments_error: None,
                })],
                usage: Default::default(),
                stop_reason: kiana_ai::types::StopReason::Stop,
                timestamp: 0,
                ..Default::default()
            }))
        }
        fn result(id: &str, name: &str, text: &str, timestamp: i64) -> AgentMessage {
            AgentMessage::Standard(Message::ToolResult(ToolResultMessage {
                role: Default::default(),
                tool_call_id: id.to_string(),
                tool_name: name.to_string(),
                content: vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: text.to_string(),
                    text_signature: None,
                })],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp,
                duration_ms: None,
            }))
        }

        let entries = vec![
            entry(
                "1",
                call("s1", "skill", serde_json::json!({"name": "deploy"})),
            ),
            entry("2", result("s1", "skill", "deploy body", 10)),
            entry(
                "3",
                call(
                    "s2",
                    "read",
                    serde_json::json!({"path": "skill://review/SKILL.md"}),
                ),
            ),
            entry("4", result("s2", "read", "review body", 20)),
            entry(
                "5",
                call("s3", "skill", serde_json::json!({"name": "deploy"})),
            ),
            entry("6", result("s3", "skill", "deploy body v2", 30)),
        ];
        let skills = collect_called_skills(&entries, 10);
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "deploy");
        assert_eq!(skills[0].content, "deploy body v2");
        assert_eq!(skills[0].called_at_ms, 30);
        assert_eq!(skills[1].name, "skill://review/SKILL.md");

        let entries = vec![
            entry(
                "1",
                call(
                    "a",
                    "agent",
                    serde_json::json!({"subagent_type": "explore"}),
                ),
            ),
            entry("2", call("b", "mcp__github__search", serde_json::json!({}))),
            entry("3", call("c", "read", serde_json::json!({"path": "x"}))),
        ];
        let deltas = collect_context_deltas(&entries);
        assert_eq!(deltas.agents, vec!["explore"]);
        assert_eq!(deltas.mcp_servers, vec!["github"]);
        assert!(deltas.tools.contains(&"read".to_string()));
    }
}
