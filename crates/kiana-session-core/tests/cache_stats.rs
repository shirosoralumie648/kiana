//! TS: `test/cache-stats.test.ts`（11 例）。

mod common;

use common::*;
use kiana_agent_core::AgentMessage;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, StopReason, Usage, UsageCost,
};
use kiana_session_core::cache_stats::{
    CacheMiss, ModelPriceInfo, ModelPriceSource, collect_cache_misses, compute_cache_waste,
    detect_cache_miss,
};
use kiana_session_core::session_manager::{
    FileEntry, SessionEntry, SessionMessageEntry, SessionMessageType, UsageEntry, UsageEntryType,
    parse_session_entries,
};

const ZERO_COST: UsageCost = UsageCost {
    input: 0.0,
    output: 0.0,
    cache_read: 0.0,
    cache_write: 0.0,
    total: 0.0,
    total_from_provider: None,
};

/// `getModel: () => ({ cost: { cacheRead: 0.3 } })`。
struct Models;

impl ModelPriceSource for Models {
    fn get_model(&self, _provider: &str, _model_id: &str) -> Option<ModelPriceInfo> {
        Some(ModelPriceInfo { cache_read: 0.3 })
    }
}

struct Options {
    input: u64,
    cache_read: u64,
    cache_write: u64,
    cost: UsageCost,
    model: &'static str,
    timestamp: i64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            input: 0,
            cache_read: 0,
            cache_write: 0,
            cost: ZERO_COST,
            model: "test-model",
            timestamp: 0,
        }
    }
}

fn assistant(options: Options) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::<AssistantContentBlock>::new(),
        api: "anthropic-messages".to_string(),
        provider: "test".to_string(),
        model: options.model.to_string(),
        usage: Usage {
            input: options.input,
            output: 10,
            cache_read: options.cache_read,
            cache_write: options.cache_write,
            cache_write1h: None,
            reasoning: None,
            total_tokens: 0,
            cost: options.cost,
        },
        stop_reason: StopReason::Stop,
        timestamp: options.timestamp,
        ..Default::default()
    }
}

fn assistant_entry(message: &AssistantMessage) -> SessionEntry {
    SessionEntry::Message(SessionMessageEntry {
        kind: SessionMessageType::Marker,
        id: "x".to_string(),
        parent_id: None,
        timestamp: String::new(),
        message: AgentMessage::Standard(kiana_ai::types::Message::Assistant(message.clone())),
        raw_message: None,
    })
}

fn usage_entry(kind: &str, timestamp: i64) -> SessionEntry {
    SessionEntry::Usage(UsageEntry {
        kind: UsageEntryType::Marker,
        id: format!("usage-{kind}"),
        parent_id: None,
        timestamp: kiana_ai::model_catalog::js_date_to_iso_string(timestamp),
        kind_name: kind.to_string(),
        provider: "test".to_string(),
        model: "test-model".to_string(),
        usage: Usage {
            input: 0,
            output: 0,
            cache_read: 100_000,
            cache_write: 0,
            cache_write1h: None,
            reasoning: None,
            total_tokens: 100_000,
            cost: ZERO_COST,
        },
        note: None,
        raw_entry: None,
    })
}

fn cost_with(cache_read: f64, cache_write: f64) -> UsageCost {
    UsageCost {
        cache_read,
        cache_write,
        ..ZERO_COST
    }
}

/// 第一轮：全新写入 100k 缓存，$3.75/M。
fn turn1() -> AssistantMessage {
    assistant(Options {
        cache_write: 100_000,
        cost: cost_with(0.0, 0.375),
        timestamp: 0,
        ..Default::default()
    })
}

/// 第二轮：健康，全部按 $0.30/M 读回。
fn turn2() -> AssistantMessage {
    assistant(Options {
        cache_read: 100_000,
        cache_write: 5_000,
        cost: cost_with(0.03, 0.019),
        timestamp: 60_000,
        ..Default::default()
    })
}

fn close_to(actual: f64, expected: f64, digits: i32) {
    let tolerance = 0.5 * 10f64.powi(-digits);
    assert!(
        (actual - expected).abs() < tolerance,
        "expected {actual} to be close to {expected} (tolerance {tolerance})"
    );
}

mod compute_cache_waste {
    use super::*;

    /// TS: "accumulates missed tokens and cost across turns"
    #[test]
    fn accumulates_missed_tokens_and_cost_across_turns() {
        // 第三轮：全 miss，上一轮 105k prompt 按 $3.75/M 重新计费。
        let turn3 = assistant(Options {
            cache_write: 110_000,
            cost: cost_with(0.0, 0.4125),
            timestamp: 120_000,
            ..Default::default()
        });
        let totals = compute_cache_waste(
            &[
                assistant_entry(&turn1()),
                assistant_entry(&turn2()),
                assistant_entry(&turn3),
            ],
            &Models,
        );
        assert_eq!(totals.missed_tokens, 105_000.0);
        // 105k 按 ($3.75 - $0.30)/M。
        close_to(totals.missed_cost, 0.36225, 5);
    }

    /// TS: "counts nothing for healthy sessions"
    #[test]
    fn counts_nothing_for_healthy_sessions() {
        let totals = compute_cache_waste(
            &[assistant_entry(&turn1()), assistant_entry(&turn2())],
            &Models,
        );
        assert_eq!(totals.missed_tokens, 0.0);
        assert_eq!(totals.missed_cost, 0.0);
    }

    /// TS: "skips the turn after a compaction reset"
    #[test]
    fn skips_the_turn_after_a_compaction_reset() {
        let reset = compaction_entry("c", None, "summary", "x");
        let after_reset = assistant(Options {
            cache_write: 20_000,
            cost: cost_with(0.0, 0.075),
            ..Default::default()
        });
        let totals = compute_cache_waste(
            &[
                assistant_entry(&turn1()),
                reset,
                assistant_entry(&after_reset),
            ],
            &Models,
        );
        assert_eq!(totals.missed_tokens, 0.0);
    }

    /// TS: "counts misses caused by model switches"
    #[test]
    fn counts_misses_caused_by_model_switches() {
        let other_model = assistant(Options {
            cache_write: 100_000,
            cost: cost_with(0.0, 0.375),
            model: "other-model",
            ..Default::default()
        });
        let totals = compute_cache_waste(
            &[assistant_entry(&turn1()), assistant_entry(&other_model)],
            &Models,
        );
        assert_eq!(totals.missed_tokens, 100_000.0);
        assert_eq!(totals.miss_count, 1);
    }

    /// TS: "skips providers that report no cache activity"
    #[test]
    fn skips_providers_that_report_no_cache_activity() {
        let a = assistant(Options {
            input: 100_000,
            ..Default::default()
        });
        let b = assistant(Options {
            input: 110_000,
            ..Default::default()
        });
        let totals = compute_cache_waste(&[assistant_entry(&a), assistant_entry(&b)], &Models);
        assert_eq!(totals.missed_tokens, 0.0);
    }
}

mod collect_cache_misses {
    use super::*;

    /// TS: "maps counted misses to their assistant messages by reference"
    #[test]
    fn maps_counted_misses_to_their_assistant_messages_by_reference() {
        let miss_turn = assistant(Options {
            cache_write: 110_000,
            cost: cost_with(0.0, 0.4125),
            timestamp: 120_000,
            ..Default::default()
        });
        let misses = collect_cache_misses(
            &[
                assistant_entry(&turn1()),
                assistant_entry(&turn2()),
                assistant_entry(&miss_turn),
            ],
            &Models,
        );
        assert_eq!(misses.len(), 1);
        assert_eq!(misses[0].0, 2);
        assert_eq!(misses[0].1.missed_tokens, 105_000.0);
    }
}

mod detect_cache_miss_tests {
    use super::*;

    /// TS: "detects a miss on a just-completed message with idle time"
    #[test]
    fn detects_a_miss_on_a_just_completed_message_with_idle_time() {
        let miss_message = assistant(Options {
            cache_write: 110_000,
            cost: cost_with(0.0, 0.4125),
            timestamp: 600_000,
            ..Default::default()
        });
        let miss: Option<CacheMiss> = detect_cache_miss(
            &[assistant_entry(&turn1()), assistant_entry(&turn2())],
            &miss_message,
            &Models,
        );
        let miss = miss.expect("expected a cache miss");
        assert_eq!(miss.missed_tokens, 105_000.0);
        close_to(miss.missed_cost, 0.36225, 5);
        // 600s - 60s，距上次请求。
        assert_eq!(miss.idle_ms, 540_000.0);
        assert!(!miss.model_changed);
    }

    /// TS: "flags model switches on detected misses"
    #[test]
    fn flags_model_switches_on_detected_misses() {
        let other_model = assistant(Options {
            cache_write: 110_000,
            cost: cost_with(0.0, 0.4125),
            model: "other-model",
            timestamp: 120_000,
            ..Default::default()
        });
        let miss = detect_cache_miss(
            &[assistant_entry(&turn1()), assistant_entry(&turn2())],
            &other_model,
            &Models,
        )
        .expect("expected a miss");
        assert_eq!(miss.missed_tokens, 105_000.0);
        assert!(miss.model_changed);
    }

    /// TS: "uses only cache-warm usage entries as cache refreshes"
    #[test]
    fn uses_only_cache_warm_usage_entries_as_cache_refreshes() {
        let miss_message = assistant(Options {
            cache_write: 110_000,
            cost: cost_with(0.0, 0.4125),
            timestamp: 600_000,
            ..Default::default()
        });

        let after_cache_warm = detect_cache_miss(
            &[
                assistant_entry(&turn1()),
                usage_entry("cache_warm", 500_000),
            ],
            &miss_message,
            &Models,
        )
        .expect("miss after cache warm");
        let after_other_usage = detect_cache_miss(
            &[
                assistant_entry(&turn1()),
                usage_entry("custom_operation", 500_000),
            ],
            &miss_message,
            &Models,
        )
        .expect("miss after other usage");

        assert_eq!(after_cache_warm.idle_ms, 100_000.0);
        assert_eq!(after_other_usage.idle_ms, 600_000.0);
    }

    /// TS: "returns undefined for healthy turns"
    #[test]
    fn returns_undefined_for_healthy_turns() {
        let healthy = assistant(Options {
            cache_read: 105_000,
            cache_write: 2_000,
            cost: cost_with(0.0315, 0.0075),
            timestamp: 120_000,
            ..Default::default()
        });
        assert!(
            detect_cache_miss(
                &[assistant_entry(&turn1()), assistant_entry(&turn2())],
                &healthy,
                &Models
            )
            .is_none()
        );
    }

    /// TS: "returns undefined for the first turn of a session"
    #[test]
    fn returns_undefined_for_the_first_turn_of_a_session() {
        assert!(detect_cache_miss(&[], &turn1(), &Models).is_none());
    }
}

/// 回归（评审 #5）：TS 的 scan 判断 `entry.message.role === "assistant"`；宽松解析成
/// `AgentMessage::Custom` 的 assistant 消息（如 `content: null`）同样计入 cache 统计。
///
/// 期望值由 TS 参考实现生成（vitest probe）：
/// `{missedTokens: 2000, missedCost: 0.002, missCount: 1}`。
#[test]
fn counts_cache_misses_on_lenient_custom_assistant_messages() {
    struct NoModels;
    impl ModelPriceSource for NoModels {
        fn get_model(&self, _provider: &str, _model_id: &str) -> Option<ModelPriceInfo> {
            None
        }
    }

    let lines = [
        r#"{"type":"session","version":3,"id":"s","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#,
        r#"{"type":"usage","id":"w1","parentId":null,"timestamp":"2025-01-01T00:00:00.500Z","kind":"cache_warm","provider":"p","model":"m","usage":{"input":0,"output":0,"cacheRead":100000,"cacheWrite":0,"totalTokens":100000,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}"#,
        r#"{"type":"message","id":"a1","parentId":"w1","timestamp":"2025-01-01T00:00:01.000Z","message":{"role":"assistant","content":null,"provider":"p","model":"m","responseModel":"r","timestamp":1000,"usage":{"input":2000,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":2010,"cost":{"input":0.002,"output":0.0001,"cacheRead":0,"cacheWrite":0,"total":0.0021}}}}"#,
    ];
    let entries: Vec<SessionEntry> = parse_session_entries(&lines.join("\n"))
        .into_iter()
        .filter_map(|entry| match entry {
            FileEntry::Entry(entry) => Some(entry),
            FileEntry::Header(_) | FileEntry::Raw(_) => None,
        })
        .collect();

    let totals = compute_cache_waste(&entries, &NoModels);
    assert_eq!(totals.missed_tokens, 2000.0);
    assert_eq!(totals.missed_cost, 0.002);
    assert_eq!(totals.miss_count, 1);
    let misses = collect_cache_misses(&entries, &NoModels);
    assert_eq!(misses.len(), 1);
    assert_eq!(misses[0].0, 1);
    assert_eq!(misses[0].1.missed_tokens, 2000.0);
}

/// 没有目录定价的 `ModelPriceSource`（`getModel` 返回 undefined）。
struct NoModels;

impl ModelPriceSource for NoModels {
    fn get_model(&self, _provider: &str, _model_id: &str) -> Option<ModelPriceInfo> {
        None
    }
}

/// 回归（评审 round 2 #11）：`detectMiss` 的 `Math.min`/`Math.max` 在任一操作数为 `NaN`
/// 时返回 `NaN`（Rust 的 `f64::min`/`f64::max` 会忽略 `NaN`）。畸形 usage/timestamp 的
/// 宽松 assistant 消息因此得到 `NaN` 的 miss 数值，而不是被当成 0。
///
/// 期望值由 TS 参考实现生成（vitest probe：`computeCacheWaste` → `missedTokens: null,
/// missedCost: null, missCount: 2`，JSON 里的 `null` 即 `NaN`）。
#[test]
fn cache_miss_math_propagates_nan_like_javascript() {
    let lines = [
        r#"{"type":"session","version":3,"id":"s","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#,
        r#"{"type":"usage","id":"w","parentId":null,"timestamp":"2025-01-01T00:00:01.000Z","kind":"cache_warm","provider":"p","model":"m","usage":{"input":0,"output":0,"cacheRead":100000,"cacheWrite":0,"totalTokens":100000,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}"#,
        r#"{"type":"message","id":"a","parentId":"w","timestamp":"2025-01-01T00:00:02.000Z","message":{"role":"assistant","content":null,"provider":"p","model":"m","usage":{"input":2000,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":2010,"cost":{"input":0.002,"output":0.0001,"cacheRead":0,"cacheWrite":0,"total":0.0021}}}}"#,
        r#"{"type":"message","id":"b","parentId":"a","timestamp":"2025-01-01T00:00:03.000Z","message":{"role":"assistant","content":null,"provider":"p","model":"m","timestamp":2000,"usage":{"cacheRead":50000,"cost":{"cacheRead":0}}}}"#,
    ];
    let entries: Vec<SessionEntry> = parse_session_entries(&lines.join("\n"))
        .into_iter()
        .filter_map(|entry| match entry {
            FileEntry::Entry(entry) => Some(entry),
            FileEntry::Header(_) | FileEntry::Raw(_) => None,
        })
        .collect();

    let totals = compute_cache_waste(&entries, &NoModels);
    assert!(totals.missed_tokens.is_nan(), "{totals:?}");
    assert!(totals.missed_cost.is_nan(), "{totals:?}");
    assert_eq!(totals.miss_count, 2);

    let misses = collect_cache_misses(&entries, &NoModels);
    assert_eq!(misses.len(), 2);
    // 第一条：usage 完整但 timestamp 缺失 → `Math.max(0, NaN)` = NaN。
    assert_eq!(misses[0].1.missed_tokens, 2000.0);
    assert!(misses[0].1.idle_ms.is_nan());
    // 第二条：cacheRead 有值、input/cacheWrite 缺失 → `Math.min(finite, NaN)` = NaN。
    assert!(misses[1].1.missed_tokens.is_nan());
    assert!(misses[1].1.missed_cost.is_nan());
    assert!(misses[1].1.idle_ms.is_nan());
}

/// TS 的 `scan` 只判断 `entry.type === "compaction" | "branch_summary"`；字段类型畸形、
/// 无法解析成类型化条目（[`SessionEntry::Unknown`]）的条目同样重置扫描，下一轮 prompt
/// 不会被当成重复计费。
#[test]
fn malformed_compaction_and_branch_summary_entries_reset_the_scan() {
    let header = r#"{"type":"session","version":3,"id":"s","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#;
    let warm = r#"{"type":"usage","id":"w","parentId":null,"timestamp":"2025-01-01T00:00:01.000Z","kind":"cache_warm","provider":"p","model":"m","usage":{"input":0,"output":0,"cacheRead":100000,"cacheWrite":0,"totalTokens":100000,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}"#;
    let miss = |id: &str, parent: &str| {
        format!(
            r#"{{"type":"message","id":"{id}","parentId":"{parent}","timestamp":"2025-01-01T00:00:02.000Z","message":{{"role":"assistant","content":[],"api":"a","provider":"p","model":"m","usage":{{"input":5000,"output":1,"cacheRead":0,"cacheWrite":0,"totalTokens":5001,"cost":{{"input":0.01,"output":0,"cacheRead":0,"cacheWrite":0,"total":0.01}}}},"stopReason":"stop","timestamp":1000}}}}"#
        )
    };
    let parse = |lines: Vec<String>| -> Vec<SessionEntry> {
        parse_session_entries(&lines.join("\n"))
            .into_iter()
            .filter_map(|entry| match entry {
                FileEntry::Entry(entry) => Some(entry),
                FileEntry::Header(_) | FileEntry::Raw(_) => None,
            })
            .collect()
    };

    // 对照：没有重置条目时，两条 assistant 都是超过噪声阈值的 cache miss。
    let without = parse(vec![
        header.to_string(),
        warm.to_string(),
        miss("a", "w"),
        miss("b", "a"),
    ]);
    let totals = compute_cache_waste(&without, &NoModels);
    assert_eq!(totals.miss_count, 2);
    assert_eq!(totals.missed_tokens, 10_000.0);

    // summary 是数字 → 字段按 JS `String(value)` 宽松承载，条目仍是 Compaction
    // （TS 只判断 `type === "compaction"`；投影读取原始对象）。
    let with_compaction = parse(vec![
        header.to_string(),
        warm.to_string(),
        miss("a", "w"),
        r#"{"type":"compaction","id":"c","parentId":"a","timestamp":"2025-01-01T00:00:03.000Z","summary":5,"firstKeptEntryId":"a","tokensBefore":1}"#.to_string(),
        miss("b", "c"),
    ]);
    assert!(matches!(with_compaction[2], SessionEntry::Compaction(_)));
    let totals = compute_cache_waste(&with_compaction, &NoModels);
    assert_eq!(totals.miss_count, 1, "{totals:?}");
    assert_eq!(totals.missed_tokens, 5000.0);
    assert_eq!(collect_cache_misses(&with_compaction, &NoModels).len(), 1);

    // fromId 是数字 → 类型化 BranchSummaryEntry 解析失败（TS 仍是 branch_summary）。
    let with_branch = parse(vec![
        header.to_string(),
        warm.to_string(),
        miss("a", "w"),
        r#"{"type":"branch_summary","id":"s","parentId":"a","timestamp":"2025-01-01T00:00:03.000Z","fromId":5,"summary":"s"}"#.to_string(),
        miss("b", "s"),
    ]);
    assert!(matches!(with_branch[2], SessionEntry::Unknown(_)));
    let totals = compute_cache_waste(&with_branch, &NoModels);
    assert_eq!(totals.miss_count, 1, "{totals:?}");
    assert_eq!(totals.missed_tokens, 5000.0);
}
