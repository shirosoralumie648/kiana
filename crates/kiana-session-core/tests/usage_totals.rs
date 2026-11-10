//! TS: `test/usage-totals.test.ts`（1 例）。

mod common;

use common::*;
use kiana_ai::types::Usage;
use kiana_session_core::session_manager::{FileEntry, parse_session_entries};
use kiana_session_core::usage_totals::{combine_usage, get_usage_cost_breakdown};

fn usage(total: f64, total_from_provider: Option<bool>) -> Usage {
    let mut usage = usage_with(1, 1, 2, total);
    usage.cost.total_from_provider = total_from_provider;
    usage
}

/// TS: "marks the combined total as provider-reported only when both totals are"
#[test]
fn marks_the_combined_total_as_provider_reported_only_when_both_totals_are() {
    let combined = combine_usage(&usage(1.0, Some(true)), &usage(2.0, Some(true)));
    assert_eq!(combined.cost.total, 3.0);
    assert_eq!(combined.cost.total_from_provider, Some(true));

    let combined = combine_usage(&usage(1.0, Some(true)), &usage(2.0, None));
    assert_eq!(combined.cost.total_from_provider, Some(false));

    let combined = combine_usage(&usage(1.0, None), &usage(2.0, Some(true)));
    assert_eq!(combined.cost.total_from_provider, Some(false));

    let combined = combine_usage(&usage(1.0, None), &usage(2.0, None));
    assert_eq!(combined.cost.total_from_provider, Some(false));
}

/// 回归（评审 #5）：`getUsageCostBreakdown` 读取 `entry.message.role === "assistant"`；
/// 宽松解析成 `AgentMessage::Custom` 的 assistant 消息同样按 `{provider}/{responseModel ?? model}`
/// 归组。
///
/// 期望值由 TS 参考实现生成（vitest probe）：
/// `[{key:"p/r",cost:0.0021,tokens:2010},{key:"p/m",cost:0,tokens:100000}]`。
#[test]
fn breakdown_reads_lenient_custom_assistant_messages() {
    let lines = [
        r#"{"type":"session","version":3,"id":"s","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#,
        r#"{"type":"usage","id":"w1","parentId":null,"timestamp":"2025-01-01T00:00:00.500Z","kind":"cache_warm","provider":"p","model":"m","usage":{"input":0,"output":0,"cacheRead":100000,"cacheWrite":0,"totalTokens":100000,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}"#,
        r#"{"type":"message","id":"a1","parentId":"w1","timestamp":"2025-01-01T00:00:01.000Z","message":{"role":"assistant","content":null,"provider":"p","model":"m","responseModel":"r","timestamp":1000,"usage":{"input":2000,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":2010,"cost":{"input":0.002,"output":0.0001,"cacheRead":0,"cacheWrite":0,"total":0.0021}}}}"#,
    ];
    let entries: Vec<kiana_session_core::session_manager::SessionEntry> =
        parse_session_entries(&lines.join("\n"))
            .into_iter()
            .filter_map(|entry| match entry {
                FileEntry::Entry(entry) => Some(entry),
                FileEntry::Header(_) | FileEntry::Raw(_) => None,
            })
            .collect();

    let breakdown = get_usage_cost_breakdown(&entries);
    assert_eq!(breakdown.len(), 2);
    assert_eq!(breakdown[0].key, "p/r");
    assert_eq!(breakdown[0].cost, 0.0021);
    assert_eq!(breakdown[0].tokens, 2010.0);
    assert_eq!(breakdown[1].key, "p/m");
    assert_eq!(breakdown[1].cost, 0.0);
    assert_eq!(breakdown[1].tokens, 100_000.0);
}

/// 回归（评审 round 2 #12）：`getUsageCostBreakdown` 与 TS 一样直接读取原始 usage 字段。
/// 残缺的 usage 对象（缺 token 字段）会产生 `NaN` tokens 的桶，而不是被跳过；无法解析成
/// typed `Usage` 的 `usage` / `branch_summary` 条目同样参与统计。
///
/// 期望值由 TS 参考实现生成（vitest probe）：
/// - assistant `{input: 2000, cost: {total: 0.5}}` → `{key:"p/m", cost:0.5, tokens:null}`；
/// - `usage` 条目 `{input: 2, cost: {total: 0.3}}` → `{key:"pu/mu", cost:0.3, tokens:null}`；
/// - `branch_summary` `{output: 5, cost:{total:0.2}}` → `{key:"Tools/summaries", cost:0.2, tokens:null}`。
#[test]
fn breakdown_includes_partial_usage_objects_with_nan_tokens() {
    let parse = |lines: &[&str]| -> Vec<kiana_session_core::session_manager::SessionEntry> {
        parse_session_entries(&lines.join("\n"))
            .into_iter()
            .filter_map(|entry| match entry {
                FileEntry::Entry(entry) => Some(entry),
                FileEntry::Header(_) | FileEntry::Raw(_) => None,
            })
            .collect()
    };
    let header = r#"{"type":"session","version":3,"id":"s","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#;

    let entries = parse(&[
        header,
        r#"{"type":"message","id":"a","parentId":null,"timestamp":"t","message":{"role":"assistant","content":[],"provider":"p","model":"m","timestamp":1,"usage":{"input":2000,"cost":{"total":0.5}}}}"#,
    ]);
    let breakdown = get_usage_cost_breakdown(&entries);
    assert_eq!(breakdown.len(), 1);
    assert_eq!(breakdown[0].key, "p/m");
    assert_eq!(breakdown[0].cost, 0.5);
    assert!(breakdown[0].tokens.is_nan());
    // `JSON.stringify(NaN)` 与 serde 都写成 null。
    let value = serde_json::to_value(&breakdown[0]).unwrap();
    assert_eq!(value.get("tokens"), Some(&serde_json::Value::Null));

    let entries = parse(&[
        header,
        r#"{"type":"usage","id":"u","parentId":null,"timestamp":"t","kind":"k","provider":"pu","model":"mu","usage":{"input":2,"cost":{"total":0.3}}}"#,
    ]);
    let breakdown = get_usage_cost_breakdown(&entries);
    assert_eq!(breakdown.len(), 1);
    assert_eq!(breakdown[0].key, "pu/mu");
    assert_eq!(breakdown[0].cost, 0.3);
    assert!(breakdown[0].tokens.is_nan());

    let entries = parse(&[
        header,
        r#"{"type":"branch_summary","id":"b","parentId":null,"timestamp":"t","fromId":"z","summary":"s","usage":{"output":5,"cost":{"total":0.2}}}"#,
    ]);
    let breakdown = get_usage_cost_breakdown(&entries);
    assert_eq!(breakdown.len(), 1);
    assert_eq!(breakdown[0].key, "Tools/summaries");
    assert_eq!(breakdown[0].cost, 0.2);
    assert!(breakdown[0].tokens.is_nan());
}
