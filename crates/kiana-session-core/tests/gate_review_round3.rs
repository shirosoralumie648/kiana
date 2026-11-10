//! Gate 评审（第三轮）回归：`gate__kiana-session-core` 报告的偏差。
//!
//! 每个测试对应一条评审 finding；期望值来自 TS 参考实现的探针
//! （`/home/ctyun/kiana-work/upstream-reference/packages/coding-agent/src/core/session-manager.ts` /
//! `usage-totals.ts` / `cache-stats.ts`，node 26 直接运行 .ts）。
//!
//! finding 1/2（非对象 `message` / `systemMessage` 原样进入投影）与 finding 5 的
//! `thinkingLevel`/`model`/`getLabel` 原始值部分受 §4.9 冻结的
//! `AgentMessage = Standard | Custom(JsonObject)` 与公开 typed 字段限制，无法逐值表达；
//! 见 `session_entry_to_context_messages` 的表示能力边界文档。

mod common;

use common::*;
use kiana_ai::types::{AssistantMessage, Usage, UsageCost};
use kiana_session_core::cache_stats::{ModelPriceInfo, ModelPriceSource, detect_cache_miss};
use kiana_session_core::session_manager::{
    FileEntry, LeafId, SessionEntry, build_session_projection, get_latest_compaction_entry,
    parse_session_entries,
};
use kiana_session_core::usage_totals::get_usage_cost_breakdown;

fn session_entries(content: &str) -> Vec<SessionEntry> {
    parse_session_entries(content)
        .into_iter()
        .filter_map(|entry| match entry {
            FileEntry::Entry(entry) => Some(entry),
            FileEntry::Header(_) | FileEntry::Raw(_) => None,
        })
        .collect()
}

fn projection_messages(content: &str) -> Vec<serde_json::Value> {
    build_session_projection(&session_entries(content), LeafId::Unspecified, None)
        .messages
        .iter()
        .map(message_json)
        .collect()
}

/// Finding 3（major）：`custom_message` 的 `customType` / `display` 类型不合法时 TS 仍用原始
/// 值构造消息；Rust 之前因 typed 反序列化失败而整条丢弃。
///
/// TS 输出（projection-fixture7.jsonl）：三条消息，`customType: 5` 与 `display: "yes"` 原样保留。
#[test]
fn custom_message_projects_loose_custom_type_and_display() {
    let content = [
        r#"{"type":"session","version":3,"id":"s7","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#,
        r#"{"type":"custom_message","id":"a","parentId":null,"timestamp":"2025-01-01T00:00:01.000Z","customType":5,"content":"x","display":true}"#,
        r#"{"type":"custom_message","id":"b","parentId":"a","timestamp":"2025-01-01T00:00:02.000Z","customType":"t","content":"y","display":"yes"}"#,
        r#"{"type":"custom_message","id":"c","parentId":"b","timestamp":"2025-01-01T00:00:03.000Z","customType":"t","content":"z","display":true}"#,
    ]
    .join("\n");

    assert_eq!(
        projection_messages(&content),
        vec![
            serde_json::json!({"role": "custom", "customType": 5, "content": "x", "display": true, "timestamp": 1735689601000i64}),
            serde_json::json!({"role": "custom", "customType": "t", "content": "y", "display": "yes", "timestamp": 1735689602000i64}),
            serde_json::json!({"role": "custom", "customType": "t", "content": "z", "display": true, "timestamp": 1735689603000i64}),
        ]
    );
}

/// Finding 4b（minor）：`customMessage` 缺省 `display`/`customType` 时 TS 的 JSON.stringify
/// 省略这两个键；Rust 之前物化成 `display:false` / `customType:""`。
///
/// TS 输出（projection-fixture6.jsonl）。
#[test]
fn custom_message_omits_absent_display_and_custom_type() {
    let content = [
        r#"{"type":"session","version":3,"id":"s6","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#,
        r#"{"type":"custom_message","id":"cm1","parentId":null,"timestamp":"2025-01-01T00:00:01.000Z","customType":"t","content":"x"}"#,
        r#"{"type":"custom_message","id":"cm2","parentId":"cm1","timestamp":"2025-01-01T00:00:02.000Z","content":"y","display":true}"#,
    ]
    .join("\n");

    assert_eq!(
        projection_messages(&content),
        vec![
            serde_json::json!({"role": "custom", "customType": "t", "content": "x", "timestamp": 1735689601000i64}),
            serde_json::json!({"role": "custom", "content": "y", "display": true, "timestamp": 1735689602000i64}),
        ]
    );
}

/// Finding 4a（minor）+ finding 3：`compaction.systemMessage` 对象上的额外键按原始对象保留；
/// 同一份文件里类型宽松的 `custom_message` 也要进入投影。
///
/// TS 输出（projection-fixture5.jsonl）。
#[test]
fn compaction_system_message_keeps_extra_keys_from_raw_entry() {
    let content = [
        r#"{"type":"session","version":3,"id":"s5","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#,
        r#"{"type":"message","id":"u0","parentId":null,"timestamp":"2025-01-01T00:00:01.000Z","message":{"role":"user","content":"hi","timestamp":1}}"#,
        r#"{"type":"compaction","id":"c1","parentId":"u0","timestamp":"2025-01-01T00:00:02.000Z","summary":"s","firstKeptEntryId":"u0","tokensBefore":1,"systemMessage":{"role":"system","content":"sys","timestamp":7,"extraSys":1}}"#,
        r#"{"type":"custom_message","id":"cm1","parentId":"c1","timestamp":"2025-01-01T00:00:03.000Z","customType":5,"content":"x","display":"yes"}"#,
        r#"{"type":"custom_message","id":"cm2","parentId":"cm1","timestamp":"2025-01-01T00:00:04.000Z","customType":"t","content":"y","display":true,"details":"d"}"#,
        r#"{"type":"message","id":"u1","parentId":"cm2","timestamp":"2025-01-01T00:00:05.000Z","message":{"role":"user","content":"after","timestamp":2}}"#,
    ]
    .join("\n");

    assert_eq!(
        projection_messages(&content),
        vec![
            serde_json::json!({"role": "system", "content": "sys", "timestamp": 7, "extraSys": 1}),
            serde_json::json!({"role": "compactionSummary", "summary": "s", "tokensBefore": 1, "timestamp": 1735689602000i64}),
            serde_json::json!({"role": "user", "content": "hi", "timestamp": 1}),
            serde_json::json!({"role": "custom", "customType": 5, "content": "x", "display": "yes", "timestamp": 1735689603000i64}),
            serde_json::json!({"role": "custom", "customType": "t", "content": "y", "display": true, "details": "d", "timestamp": 1735689604000i64}),
            serde_json::json!({"role": "user", "content": "after", "timestamp": 2}),
        ]
    );
}

/// Finding 6（minor）：TS 的 `getLatestCompactionEntry` 只判断 `type === "compaction"`，
/// 不校验字段；`systemMessage` 为字符串、`summary` 为数字、`tokensBefore` 为字符串的条目
/// 仍然是最新 compaction。
#[test]
fn get_latest_compaction_entry_accepts_malformed_fields() {
    let content = [
        r#"{"type":"session","version":3,"id":"s4","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#,
        r#"{"type":"message","id":"u0","parentId":null,"timestamp":"2025-01-01T00:00:01.000Z","message":{"role":"user","content":"hi","timestamp":1}}"#,
        r#"{"type":"compaction","id":"c1","parentId":"u0","timestamp":"2025-01-01T00:00:02.000Z","summary":"s","firstKeptEntryId":"u0","tokensBefore":1,"systemMessage":"weird"}"#,
    ]
    .join("\n");
    let entries = session_entries(&content);
    let latest = get_latest_compaction_entry(&entries).expect("latest compaction");
    assert_eq!(latest.id, "c1");
    assert_eq!(latest.first_kept_entry_id, "u0");
    assert_eq!(latest.timestamp, "2025-01-01T00:00:02.000Z");

    let content = [
        r#"{"type":"session","version":3,"id":"s","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#,
        r#"{"type":"compaction","id":"c1","parentId":null,"timestamp":"2025-01-01T00:00:07.000Z","summary":5,"firstKeptEntryId":"u1","tokensBefore":"x","systemMessage":{"role":"system","content":"sys","timestamp":7,"extraSys":1}}"#,
    ]
    .join("\n");
    let entries = session_entries(&content);
    let latest = get_latest_compaction_entry(&entries).expect("latest compaction");
    assert_eq!(latest.id, "c1");
    assert_eq!(latest.first_kept_entry_id, "u1");
    // TS 的 `entry.summary` 是数字 5；typed 字段用 JS `String(5)` 承载（投影仍读原始对象）。
    assert_eq!(latest.summary, "5");
}

/// Finding 6 关联：`buildContextEntries` 的保留区起点用 TS 的严格相等比较原始
/// `firstKeptEntryId`；数字 `5` 不会匹配字符串 id `"5"`（typed 字段的 `String(5)` 会误匹配）。
///
/// TS 输出（first-kept-num.jsonl）：`[compactionSummary, user("after")]`。
#[test]
fn non_string_first_kept_entry_id_does_not_match_string_entry_id() {
    let content = [
        r#"{"type":"session","version":3,"id":"fk","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#,
        r#"{"type":"message","id":"5","parentId":null,"timestamp":"2025-01-01T00:00:01.000Z","message":{"role":"user","content":"keep?","timestamp":1}}"#,
        r#"{"type":"message","id":"u0","parentId":"5","timestamp":"2025-01-01T00:00:02.000Z","message":{"role":"user","content":"u0","timestamp":2}}"#,
        r#"{"type":"compaction","id":"c1","parentId":"u0","timestamp":"2025-01-01T00:00:03.000Z","summary":"s","firstKeptEntryId":5,"tokensBefore":1}"#,
        r#"{"type":"message","id":"u1","parentId":"c1","timestamp":"2025-01-01T00:00:04.000Z","message":{"role":"user","content":"after","timestamp":3}}"#,
    ]
    .join("\n");

    assert_eq!(
        projection_messages(&content),
        vec![
            serde_json::json!({"role": "compactionSummary", "summary": "s", "tokensBefore": 1, "timestamp": 1735689603000i64}),
            serde_json::json!({"role": "user", "content": "after", "timestamp": 3}),
        ]
    );
}

/// Finding 5（minor，usage 部分）：`${entry.provider}/${entry.model}` 的缺省字段按 JS 模板
/// 插值为 `"undefined"`；typed `String` 默认值会得到空串。
///
/// TS 输出（usage-fixture.jsonl）：
/// `[{"key":"undefined/m",...},{"key":"p/undefined",...},{"key":"p/m",...}]`。
#[test]
fn usage_breakdown_interpolates_missing_provider_and_model_as_undefined() {
    let content = [
        r#"{"type":"session","version":3,"id":"u","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#,
        r#"{"type":"usage","id":"u1","parentId":null,"timestamp":"2025-01-01T00:00:01.000Z","kind":"cache_warm","model":"m","usage":{"input":10,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":10,"cost":{"input":1,"output":0,"cacheRead":0,"cacheWrite":0,"total":1}}}"#,
        r#"{"type":"usage","id":"u2","parentId":"u1","timestamp":"2025-01-01T00:00:02.000Z","kind":"other","provider":"p","usage":{"input":10,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":10,"cost":{"input":1,"output":0,"cacheRead":0,"cacheWrite":0,"total":1}}}"#,
        r#"{"type":"usage","id":"u3","parentId":"u2","timestamp":"2025-01-01T00:00:03.000Z","kind":"other","model":"m","usage":{"input":10,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":10,"cost":{"input":1,"output":0,"cacheRead":0,"cacheWrite":0,"total":1}}}"#,
        r#"{"type":"usage","id":"u4","parentId":"u3","timestamp":"2025-01-01T00:00:04.000Z","kind":"other","provider":"p","model":"m","usage":{"input":10,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":10,"cost":{"input":1,"output":0,"cacheRead":0,"cacheWrite":0,"total":1}}}"#,
    ]
    .join("\n");
    let entries = session_entries(&content);
    let breakdown: Vec<(String, f64, f64)> = get_usage_cost_breakdown(&entries)
        .into_iter()
        .map(|entry| (entry.key, entry.cost, entry.tokens))
        .collect();
    assert_eq!(
        breakdown,
        vec![
            ("undefined/m".to_string(), 2.0, 20.0),
            ("p/undefined".to_string(), 1.0, 10.0),
            ("p/m".to_string(), 1.0, 10.0),
        ]
    );
}

struct Models;

impl ModelPriceSource for Models {
    fn get_model(&self, _provider: &str, _model_id: &str) -> Option<ModelPriceInfo> {
        Some(ModelPriceInfo { cache_read: 0.3 })
    }
}

fn zero_cost() -> UsageCost {
    UsageCost {
        input: 0.0,
        output: 0.0,
        cache_read: 0.0,
        cache_write: 0.0,
        total: 0.0,
        total_from_provider: None,
    }
}

/// Finding 5（minor，cache-stats 部分）：`cache_warm` usage 条目的 `modelKey` 同样按原始字段
/// 插值（缺省 provider → `"undefined/<model>"`），而不是 typed 默认值的 `"/<model>"`。
///
/// 构造：上一轮是缺 provider 的 cache_warm（TS key `undefined/m`），新 assistant 的
/// provider 是空串（key `/m`）→ TS 报告 modelChanged=true；typed 默认值会让两者相等而漏报。
#[test]
fn cache_warm_usage_entry_model_key_uses_raw_provider_and_model() {
    let content = [
        r#"{"type":"session","version":3,"id":"u","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#,
        r#"{"type":"usage","id":"u1","parentId":null,"timestamp":"2025-01-01T00:00:01.000Z","kind":"cache_warm","model":"m","usage":{"input":100000,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":100000,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}"#,
    ]
    .join("\n");
    let entries = session_entries(&content);

    let message = AssistantMessage {
        provider: String::new(),
        model: "m".to_string(),
        timestamp: 600_000,
        usage: Usage {
            input: 100_000,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write1h: None,
            reasoning: None,
            total_tokens: 100_000,
            cost: zero_cost(),
        },
        ..Default::default()
    };
    let miss = detect_cache_miss(&entries, &message, &Models).expect("cache miss");
    assert!(
        miss.model_changed,
        "TS: \"undefined/m\" !== \"/m\" (modelChanged=true), got {miss:?}"
    );
}
