//! 由 TS 参考实现生成的差分黄金测试。
//!
//! 输入是 24 组随机但确定性的会话条目（user/assistant/toolResult/custom_message/
//! model_change/thinking_level_change/label/session_info/branch_summary/compaction），
//! 每组分别用 7 个 `keepRecentTokens` 运行 TS 的 `estimateProjectedContextTokens`、
//! `findCutPoint` 与 `prepareCompaction`。断言 Rust 侧在这些输入上产出完全相同的：
//! 估算（tokens/usageTokens/trailingTokens/lastUsageIndex）、切点（下标/turnStart/isSplitTurn）、
//! 压缩准备（firstKeptEntryId/tokensBefore/isSplitTurn/previousSummary/消息 role 序列/
//! fileOps）。
//!
//! 期望值生成方式（`/home/ctyun/kiana-work/scratch/gate-compaction/probe/differential.test.ts`）：
//! `npx vitest run --dir <probe>`，导入 `upstream-reference` 的 `core/compaction/index.ts` 与
//! `core/session-manager.ts`。
//!
//! TS: `packages/coding-agent/src/core/compaction/{compaction,utils}.ts`

use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::JsonValue;
use kiana_compaction::{
    CompactionSettings, DEFAULT_COMPACTION_SETTINGS, FileValue, estimate_projected_context_tokens,
    find_cut_point, prepare_branch_entries, prepare_compaction,
};
use kiana_session_core::session_manager::{LeafId, SessionEntry, build_session_projection};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Scenario {
    seed: u32,
    entries: Vec<SessionEntry>,
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    keep_recent_tokens: u64,
    branch_cases: Vec<BranchCase>,
    estimate: Estimate,
    cut: Cut,
    preparation: Option<Preparation>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct BranchCase {
    budget: i64,
    total_tokens: f64,
    messages: Vec<String>,
    read: Vec<String>,
    written: Vec<String>,
    edited: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct Estimate {
    tokens: u64,
    usage_tokens: u64,
    trailing_tokens: u64,
    last_usage_index: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Cut {
    first_kept_entry_index: usize,
    turn_start_index: i64,
    is_split_turn: bool,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct Preparation {
    first_kept_entry_id: String,
    tokens_before: u64,
    is_split_turn: bool,
    previous_summary: Option<String>,
    messages_to_summarize: Vec<String>,
    turn_prefix_messages: Vec<String>,
    read: Vec<String>,
    written: Vec<String>,
    edited: Vec<String>,
}

/// TS 的 `message.role`（custom 消息渲染成 `custom:<customType>`）。
fn message_role(message: &AgentMessage) -> String {
    let role = message.role();
    if role != "custom" {
        return role.to_string();
    }
    let custom_type = match message {
        AgentMessage::Custom(object) => object
            .get("customType")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    };
    format!("custom:{custom_type}")
}

fn sorted<I>(items: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let mut items: Vec<String> = items.into_iter().collect();
    items.sort();
    items
}

/// 差分夹具里的文件操作元素都是字符串（TS 的 `.sort()` 对字符串等价于 `String()`）。
fn file_value_text(value: &FileValue) -> String {
    match value.as_json() {
        JsonValue::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[test]
fn matches_ts_reference_for_random_sessions() {
    let scenarios: Vec<Scenario> =
        serde_json::from_str(include_str!("fixtures/differential-ts-reference.json"))
            .expect("differential fixture");

    let mut checked = 0usize;
    for scenario in &scenarios {
        let entries = &scenario.entries;
        let projection = build_session_projection(entries, LeafId::Unspecified, None);

        for case in &scenario.cases {
            let context = format!("seed={} keep={}", scenario.seed, case.keep_recent_tokens);

            let estimate = estimate_projected_context_tokens(&projection, entries);
            assert_eq!(
                Estimate {
                    tokens: estimate.tokens,
                    usage_tokens: estimate.usage_tokens,
                    trailing_tokens: estimate.trailing_tokens,
                    last_usage_index: estimate.last_usage_index,
                },
                case.estimate,
                "estimate mismatch ({context})"
            );

            let cut = find_cut_point(entries, 0, entries.len(), case.keep_recent_tokens);
            assert_eq!(
                cut.first_kept_entry_index, case.cut.first_kept_entry_index,
                "cut index mismatch ({context})"
            );
            assert_eq!(
                cut.turn_start_index.map_or(-1, |index| index as i64),
                case.cut.turn_start_index,
                "cut turn start mismatch ({context})"
            );
            assert_eq!(
                cut.is_split_turn, case.cut.is_split_turn,
                "cut split mismatch ({context})"
            );

            for branch_case in &case.branch_cases {
                let preparation = prepare_branch_entries(entries, branch_case.budget);
                let actual = BranchCase {
                    budget: branch_case.budget,
                    total_tokens: preparation.total_tokens,
                    messages: preparation.messages.iter().map(message_role).collect(),
                    read: sorted(preparation.file_ops.read.iter().map(file_value_text)),
                    written: sorted(preparation.file_ops.written.iter().map(file_value_text)),
                    edited: sorted(preparation.file_ops.edited.iter().map(file_value_text)),
                };
                assert_eq!(
                    actual, *branch_case,
                    "branch preparation mismatch ({context}, budget={})",
                    branch_case.budget
                );
            }

            let settings = CompactionSettings {
                keep_recent_tokens: case.keep_recent_tokens,
                ..DEFAULT_COMPACTION_SETTINGS
            };
            let preparation = prepare_compaction(entries, &settings);
            match (&preparation, &case.preparation) {
                (None, None) => {}
                (Some(preparation), Some(expected)) => {
                    let actual = Preparation {
                        first_kept_entry_id: preparation.first_kept_entry_id.clone(),
                        tokens_before: preparation.tokens_before,
                        is_split_turn: preparation.is_split_turn,
                        previous_summary: preparation.previous_summary.clone(),
                        messages_to_summarize: preparation
                            .messages_to_summarize
                            .iter()
                            .map(message_role)
                            .collect(),
                        turn_prefix_messages: preparation
                            .turn_prefix_messages
                            .iter()
                            .map(message_role)
                            .collect(),
                        read: sorted(preparation.file_ops.read.iter().map(file_value_text)),
                        written: sorted(preparation.file_ops.written.iter().map(file_value_text)),
                        edited: sorted(preparation.file_ops.edited.iter().map(file_value_text)),
                    };
                    assert_eq!(actual, *expected, "preparation mismatch ({context})");
                }
                (Some(_), None) => panic!("expected no preparation, got one ({context})"),
                (None, Some(_)) => panic!("expected a preparation, got none ({context})"),
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 280);
}
