//! 压缩前 tool-result pruning 的集成测试。
//!
//! 覆盖 DoD：
//! - 回收 >= 20k token，最新 40k 完整，tool call/result 仍然合法；
//! - superseded read 被 elide，未被覆盖的保留；
//! - skill 读取与 active plan 受保护；
//! - 零结果 grep / 等待使用精确 placeholder；<50 token 不 blank；
//! - 原地 blank 不改变 entry id / parent / 顺序 / toolCallId。

use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, JsonValue, Message, StopReason,
    TextContent, TextKind, ToolCall, ToolCallKind, ToolResultMessage, UserContentBlock,
};
use kiana_compaction::pruning::{
    ProtectedToolMatcher, PruneConfig, SUPERSEDED_NOTICE, USELESS_NOTICE, prune_tool_outputs,
    tool_pairing_violations, tool_result_message,
};
use kiana_session_core::session_manager::{SessionEntry, SessionMessageEntry, SessionMessageType};

fn assistant_call(id: &str, name: &str, args: JsonValue) -> AgentMessage {
    let arguments = args
        .as_object()
        .map(|object| object.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
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
        stop_reason: StopReason::Stop,
        timestamp: 0,
        ..Default::default()
    }))
}

fn tool_result(name: &str, id: &str, text: &str, details: Option<JsonValue>) -> AgentMessage {
    AgentMessage::Standard(Message::ToolResult(ToolResultMessage {
        role: Default::default(),
        tool_call_id: id.to_string(),
        tool_name: name.to_string(),
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: text.to_string(),
            text_signature: None,
        })],
        details,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 0,
        duration_ms: None,
    }))
}

struct Factory {
    next: usize,
    last: Option<String>,
}

impl Factory {
    fn new() -> Self {
        Self {
            next: 0,
            last: None,
        }
    }

    fn entry(&mut self, message: AgentMessage) -> SessionEntry {
        let id = format!("id-{}", self.next);
        self.next += 1;
        let entry = SessionEntry::Message(SessionMessageEntry {
            kind: SessionMessageType::Marker,
            id: id.clone(),
            parent_id: self.last.clone(),
            timestamp: String::new(),
            message,
            raw_message: None,
        });
        self.last = Some(id);
        entry
    }
}

fn read_pair(
    factory: &mut Factory,
    call_id: &str,
    path: &str,
    content: &str,
    with_call: bool,
) -> Vec<SessionEntry> {
    let mut entries = Vec::new();
    if with_call {
        entries.push(factory.entry(assistant_call(
            call_id,
            "read",
            serde_json::json!({"path": path}),
        )));
    }
    entries.push(factory.entry(tool_result("read", call_id, content, None)));
    entries
}

#[test]
fn pruning_reclaims_at_least_20k_and_keeps_latest_40k() {
    let mut factory = Factory::new();
    let mut entries = Vec::new();
    let payload = "x".repeat(30_000);
    for index in 0..20 {
        entries.extend(read_pair(
            &mut factory,
            &format!("call-{index}"),
            &format!("/file{index}"),
            &payload,
            true,
        ));
    }
    let before_ids: Vec<String> = entries.iter().map(|entry| entry.id().to_string()).collect();
    let outcome = prune_tool_outputs(&entries, &PruneConfig::default());
    assert!(outcome.applied, "expected pruning to apply");
    assert!(
        outcome.tokens_saved >= 20_000,
        "saved {} tokens",
        outcome.tokens_saved
    );

    // 最新 40k tool-output token 完整。
    let mut accumulated = 0u64;
    let mut protected = Vec::new();
    for entry in entries.iter().rev() {
        let Some(result) = tool_result_message(entry) else {
            continue;
        };
        protected.push(entry.id().to_string());
        accumulated += kiana_compaction::estimate_tokens(&AgentMessage::Standard(
            Message::ToolResult(result.clone()),
        ));
        if accumulated >= 40_000 {
            break;
        }
    }
    for id in &protected {
        let index = before_ids.iter().position(|before| before == id).unwrap();
        let message = tool_result_message(&outcome.entries[index]).unwrap();
        let text = kiana_compaction::pruning::tool_result_text(message);
        assert!(
            !text.starts_with("[Output truncated"),
            "protected result {id} was pruned"
        );
    }

    // entry 顺序/id/toolCallId 不变，pair 合法。
    let after_ids: Vec<String> = outcome
        .entries
        .iter()
        .map(|entry| entry.id().to_string())
        .collect();
    assert_eq!(before_ids, after_ids);
    assert!(tool_pairing_violations(&outcome.entries).is_empty());
    for (before, after) in entries.iter().zip(outcome.entries.iter()) {
        if let (Some(before_result), Some(after_result)) =
            (tool_result_message(before), tool_result_message(after))
        {
            assert_eq!(before_result.tool_call_id, after_result.tool_call_id);
            assert_eq!(before_result.tool_name, after_result.tool_name);
        }
    }
}

#[test]
fn superseded_read_is_elided_and_newer_read_kept() {
    let mut factory = Factory::new();
    let mut entries = Vec::new();
    entries.extend(read_pair(
        &mut factory,
        "old-read",
        "/src/a.rs",
        &"a".repeat(30_000),
        true,
    ));
    entries.extend(read_pair(
        &mut factory,
        "new-read",
        "/src/a.rs",
        &"b".repeat(30_000),
        true,
    ));
    entries.extend(read_pair(
        &mut factory,
        "other-read",
        "/src/other.rs",
        &"c".repeat(30_000),
        true,
    ));
    let config = PruneConfig {
        minimum_savings: 1,
        ..PruneConfig::default()
    };
    let outcome = prune_tool_outputs(&entries, &config);
    assert!(outcome.applied);
    let old = outcome
        .pruned
        .iter()
        .find(|record| record.entry_id == "id-1");
    assert!(
        old.is_some(),
        "older read should be pruned: {:?}",
        outcome.pruned
    );
    assert_eq!(old.unwrap().notice, SUPERSEDED_NOTICE);
    // 新读取与其他文件未被 supersede。
    assert!(
        !outcome
            .pruned
            .iter()
            .any(|record| record.entry_id == "id-3")
    );
    assert!(
        !outcome
            .pruned
            .iter()
            .any(|record| record.entry_id == "id-5")
    );
}

#[test]
fn skill_and_active_plan_reads_are_protected() {
    let mut factory = Factory::new();
    let mut entries = Vec::new();
    entries.extend(read_pair(
        &mut factory,
        "skill",
        "skill://deploy/SKILL.md",
        &"s".repeat(30_000),
        true,
    ));
    entries.extend(read_pair(
        &mut factory,
        "plan",
        "PLAN.md",
        &"p".repeat(30_000),
        true,
    ));
    for index in 0..8 {
        entries.extend(read_pair(
            &mut factory,
            &format!("normal-{index}"),
            &format!("/normal{index}"),
            &"n".repeat(30_000),
            true,
        ));
    }
    let config = PruneConfig {
        minimum_savings: 1,
        protected_tools: vec![
            ProtectedToolMatcher::SkillRead,
            ProtectedToolMatcher::PlanRead {
                paths: vec!["PLAN.md".to_string()],
            },
        ],
        ..PruneConfig::default()
    };
    let outcome = prune_tool_outputs(&entries, &config);
    assert!(outcome.applied);
    assert!(
        outcome
            .pruned
            .iter()
            .any(|record| record.entry_id.starts_with("id-"))
    );
    assert!(
        !outcome
            .pruned
            .iter()
            .any(|record| record.entry_id == "id-1")
    );
    assert!(
        !outcome
            .pruned
            .iter()
            .any(|record| record.entry_id == "id-3")
    );
    let skill = tool_result_message(&outcome.entries[1]).unwrap();
    assert_eq!(skill.tool_name, "read");
    assert_eq!(
        kiana_compaction::pruning::tool_result_text(skill)
            .chars()
            .count(),
        30_000
    );
}

#[test]
fn zero_result_grep_and_wait_use_exact_placeholders() {
    let mut factory = Factory::new();
    let mut entries = Vec::new();
    // 一个大的普通结果提供最小回收量。
    entries.extend(read_pair(
        &mut factory,
        "big",
        "/big",
        &"b".repeat(100_000),
        true,
    ));
    entries.push(factory.entry(assistant_call(
        "grep-call",
        "grep",
        serde_json::json!({"pattern": "needle", "path": "."}),
    )));
    entries.push(factory.entry(tool_result("grep", "grep-call", "No matches found", None)));
    entries.push(factory.entry(assistant_call(
        "wait-call",
        "wait",
        serde_json::json!({"seconds": 1}),
    )));
    entries.push(factory.entry(tool_result(
        "wait",
        "wait-call",
        "Waited 1s, process finished.",
        None,
    )));
    // 很小的普通结果：不足 50 token，不能 blank。
    entries.push(factory.entry(assistant_call(
        "tiny-call",
        "read",
        serde_json::json!({"path": "/tiny"}),
    )));
    entries.push(factory.entry(tool_result("read", "tiny-call", "tiny", None)));

    let config = PruneConfig {
        minimum_savings: 1,
        protect_tokens: 0,
        ..PruneConfig::default()
    };
    let outcome = prune_tool_outputs(&entries, &config);
    assert!(outcome.applied);
    // 第 6 周审计：MIN_PRUNE_TOKENS 对所有清除分支生效。`No matches found` /
    // `Waited 1s…` 都远小于 50 token，替换成占位符反而更长，必须原样保留。
    assert!(
        !outcome
            .pruned
            .iter()
            .any(|record| record.tool_name == "grep" || record.tool_name == "wait"),
        "small useless results must not be blanked: {:?}",
        outcome.pruned
    );
    let grep = tool_result_message(&outcome.entries[3]).unwrap();
    assert_eq!(
        kiana_compaction::pruning::tool_result_text(grep),
        "No matches found"
    );
    let wait = tool_result_message(&outcome.entries[5]).unwrap();
    assert_eq!(
        kiana_compaction::pruning::tool_result_text(wait),
        "Waited 1s, process finished."
    );
    let tiny = tool_result_message(outcome.entries.last().unwrap()).unwrap();
    assert_eq!(
        kiana_compaction::pruning::tool_result_text(tiny),
        "tiny",
        "results below MIN_PRUNE_TOKENS must not be blanked"
    );
}

#[test]
fn large_flagged_useless_result_uses_the_exact_placeholder() {
    let mut factory = Factory::new();
    let mut entries = Vec::new();
    // `details.useless = true` 让任意大小的结果被判定为 useless；这里用足够大的正文
    // 越过 MIN_PRUNE_TOKENS，从而走 `USELESS_NOTICE` 分支（而非保留原样）。
    entries.push(factory.entry(assistant_call(
        "grep-call",
        "grep",
        serde_json::json!({"pattern": "needle", "path": "."}),
    )));
    entries.push(factory.entry(tool_result(
        "grep",
        "grep-call",
        &"noise ".repeat(4_000),
        Some(serde_json::json!({"useless": true})),
    )));
    let config = PruneConfig {
        minimum_savings: 1,
        protect_tokens: 0,
        ..PruneConfig::default()
    };
    let outcome = prune_tool_outputs(&entries, &config);
    assert!(outcome.applied);
    assert_eq!(outcome.pruned.len(), 1);
    assert_eq!(outcome.pruned[0].notice, USELESS_NOTICE);
    let grep = tool_result_message(outcome.entries.last().unwrap()).unwrap();
    assert_eq!(
        kiana_compaction::pruning::tool_result_text(grep),
        USELESS_NOTICE
    );
}

#[test]
fn minimum_savings_gate_returns_untouched_history() {
    let mut factory = Factory::new();
    let mut entries = Vec::new();
    entries.extend(read_pair(
        &mut factory,
        "small",
        "/small",
        &"s".repeat(4_000),
        true,
    ));
    let outcome = prune_tool_outputs(&entries, &PruneConfig::default());
    assert!(!outcome.applied);
    assert_eq!(outcome.entries, entries);
}

#[test]
fn aggregate_budget_clears_oldest_but_artifact_survives_compaction() {
    let temp = tempfile::TempDir::new().unwrap();
    let store = kiana_core::artifact_store::ArtifactStore::new(temp.path(), "session").unwrap();
    let mut factory = Factory::new();
    let mut entries = Vec::new();
    let payloads = ["a".repeat(30_000), "b".repeat(30_000), "c".repeat(30_000)];
    for (index, payload) in payloads.iter().enumerate() {
        entries.extend(read_pair(
            &mut factory,
            &format!("call-{index}"),
            &format!("/f{index}"),
            payload,
            true,
        ));
    }

    let outcome = kiana_compaction::pruning::enforce_aggregate_tool_result_budget(
        &entries,
        10_000,
        Some(&store),
        &PruneConfig::default(),
    );
    assert!(!outcome.cleared.is_empty());
    assert_eq!(
        outcome.cleared[0].entry_id, "id-1",
        "oldest result must be cleared first"
    );
    let cleared = tool_result_message(&outcome.entries[1]).unwrap();
    let text = kiana_compaction::pruning::tool_result_text(cleared);
    assert!(text.starts_with("[Old tool result content cleared; artifact: "));
    let path = text
        .trim_start_matches("[Old tool result content cleared; artifact: ")
        .trim_end_matches(']');
    let artifact_path = std::path::PathBuf::from(path);
    assert!(artifact_path.exists());
    assert_eq!(
        std::fs::read_to_string(&artifact_path).unwrap(),
        payloads[0]
    );

    // 模拟压缩：prune 之后 handle/占位符仍在，解析到同一个 artifact。
    let pruned = prune_tool_outputs(
        &outcome.entries,
        &PruneConfig {
            minimum_savings: 1,
            ..PruneConfig::default()
        },
    );
    let still_there = tool_result_message(&pruned.entries[1]).unwrap();
    assert_eq!(
        kiana_compaction::pruning::tool_result_text(still_there),
        text
    );
    assert_eq!(
        std::fs::read_to_string(&artifact_path).unwrap(),
        payloads[0]
    );
}
