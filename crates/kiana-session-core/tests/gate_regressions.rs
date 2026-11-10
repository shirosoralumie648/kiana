//! Gate 评审回归：`gate__kiana-session-core` 报告的偏差。
//!
//! 每个测试对应一条评审 finding；期望值来自 TS 参考实现的探针（`/home/ctyun/kiana-work/upstream-reference`
//! 下的 `session-manager.ts` / `cache-stats.ts` / `usage-totals.ts` / `nested-tool-calls.ts`）。

mod common;

use std::time::Duration;

use common::*;
use kiana_agent_core::types::AgentToolCall;
use kiana_ai::types::{JsonValue, ToolCallKind};
use kiana_session_core::cache_stats::{
    CacheWasteTotals, ModelPriceInfo, ModelPriceSource, compute_cache_waste,
};
use kiana_session_core::messages::{
    COMPACTION_SUMMARY_PREFIX, COMPACTION_SUMMARY_SUFFIX, convert_to_llm,
};
use kiana_session_core::nested_tool_calls::NestedCallRecorder;
use kiana_session_core::session_manager::{
    FileEntry, SessionEntry, SessionManager, find_most_recent_session, parse_session_entries,
};
use kiana_session_core::usage_totals::{UsageCostBreakdownEntry, get_usage_cost_breakdown};

fn header(id: &str) -> String {
    format!(
        "{{\"type\":\"session\",\"version\":3,\"id\":\"{id}\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":\"/tmp\"}}"
    )
}

fn write_file(dir: &std::path::Path, name: &str, lines: &[String]) -> String {
    let path = dir.join(name).to_string_lossy().into_owned();
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    path
}

fn entries_of(content: &str) -> Vec<SessionEntry> {
    parse_session_entries(content)
        .into_iter()
        .filter_map(|entry| match entry {
            FileEntry::Entry(entry) => Some(entry),
            FileEntry::Header(_) | FileEntry::Raw(_) => None,
        })
        .collect()
}

fn message_line(id: &str, parent: &str, text: &str) -> String {
    format!(
        "{{\"type\":\"message\",\"id\":\"{id}\",\"parentId\":{parent},\"timestamp\":\"2026-01-01T00:00:01.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"{text}\",\"timestamp\":1}}}}"
    )
}

fn last_json_line(path: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(path).unwrap();
    let line = text.lines().last().unwrap();
    serde_json::from_str(line).unwrap()
}

fn keys(value: &serde_json::Value) -> Vec<String> {
    value.as_object().expect("object").keys().cloned().collect()
}

/// Finding 1（major）：fork 时来源 label 没有 `timestamp`，重建的 label 条目必须省略该键。
#[test]
fn branched_session_omits_missing_label_timestamp() {
    let temp = temp_dir("gate-branch-label-missing-");
    let dir = temp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let path = write_file(
        &dir,
        "source.jsonl",
        &[
            header("sess1"),
            message_line("m1", "null", "hi"),
            "{\"type\":\"label\",\"id\":\"l1\",\"parentId\":\"m1\",\"targetId\":\"m1\",\"label\":\"tag\"}"
                .to_string(),
        ],
    );

    let mut session = SessionManager::open(&path, Some(dir.to_str().unwrap()), None).unwrap();
    assert_eq!(session.get_label("m1"), Some("tag"));
    let node = session
        .get_tree()
        .into_iter()
        .find(|node| node.entry.id() == "m1")
        .expect("m1 node");
    assert_eq!(node.label_timestamp, None);

    let out = session.create_branched_session("m1").unwrap().unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    let line = text
        .lines()
        .find(|line| line.contains("\"type\":\"label\""))
        .expect("label line");
    let value: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(
        keys(&value),
        ["type", "id", "parentId", "targetId", "label"],
        "timestamp must be omitted: {line}"
    );
}

/// Finding 1：非字符串 label timestamp 保留原始 JSON 类型（TS 的 `JSON.stringify`）。
#[test]
fn branched_session_preserves_non_string_label_timestamp() {
    let temp = temp_dir("gate-branch-label-number-");
    let dir = temp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let path = write_file(
        &dir,
        "source.jsonl",
        &[
            header("sess1"),
            message_line("m1", "null", "hi"),
            "{\"type\":\"label\",\"id\":\"l1\",\"parentId\":\"m1\",\"timestamp\":5,\"targetId\":\"m1\",\"label\":\"tag\"}"
                .to_string(),
        ],
    );

    let mut session = SessionManager::open(&path, Some(dir.to_str().unwrap()), None).unwrap();
    let out = session.create_branched_session("m1").unwrap().unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    let line = text
        .lines()
        .find(|line| line.contains("\"type\":\"label\""))
        .expect("label line");
    let value: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(value.get("timestamp"), Some(&serde_json::json!(5)));
    assert_eq!(
        keys(&value),
        ["type", "id", "parentId", "timestamp", "targetId", "label"]
    );
}

/// Finding 2：末尾是解析成功但非对象的行（`123`）时，`leafId` 变成 `undefined`。
///
/// 推论（与 TS 探针一致）：`getEntryCount()` 计入 `undefined` 键；下一次 append 的
/// `parentId` 键被省略；`getBranch()` 只含新条目。
#[test]
fn raw_non_object_tail_makes_leaf_undefined_and_omits_parent_id() {
    let temp = temp_dir("gate-raw-tail-");
    let dir = temp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let path = write_file(
        &dir,
        "source.jsonl",
        &[
            header("sess2"),
            message_line("m1", "null", "hi"),
            "123".to_string(),
        ],
    );

    let mut session = SessionManager::open(&path, Some(dir.to_str().unwrap()), None).unwrap();
    assert_eq!(session.get_leaf_id(), None);
    // TS byId.size = 2（m1 + undefined 键）。
    assert_eq!(session.get_entry_count(), 2);
    // 表示能力边界：`SessionEntry` 无法表示非对象值，typed getEntries 只有 1 条。
    assert_eq!(session.get_entries().len(), 1);

    let new_id = session.append_message(user_message("next", 2)).unwrap();
    let ids: Vec<String> = session
        .get_branch(None)
        .iter()
        .map(|entry| entry.id().to_string())
        .collect();
    assert_eq!(ids, vec![new_id.clone()]);
    let value = last_json_line(&path);
    assert!(
        value.get("parentId").is_none(),
        "parentId must be omitted: {value}"
    );
}

/// Finding 3：`{}` 行（对象但无字符串 id）同样让 leaf 变成 `undefined`，且不写入空串 parentId。
#[test]
fn id_less_object_tail_makes_leaf_undefined_and_omits_parent_id() {
    let temp = temp_dir("gate-idless-tail-");
    let dir = temp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let path = write_file(
        &dir,
        "source.jsonl",
        &[
            header("sess3"),
            message_line("m1", "null", "hi"),
            "{}".to_string(),
        ],
    );

    let mut session = SessionManager::open(&path, Some(dir.to_str().unwrap()), None).unwrap();
    assert_eq!(session.get_leaf_id(), None);
    assert_eq!(session.get_entry_count(), 2);

    let new_id = session.append_message(user_message("next", 2)).unwrap();
    let value = last_json_line(&path);
    assert!(
        value.get("parentId").is_none(),
        "parentId must be omitted: {value}"
    );
    let ids: Vec<String> = session
        .get_branch(None)
        .iter()
        .map(|entry| entry.id().to_string())
        .collect();
    assert_eq!(ids, vec![new_id.clone()]);

    // TS: roots = [m1, {} 节点]，新条目挂在 `{}`（undefined 键）节点下。
    let tree = session.get_tree();
    assert_eq!(tree.len(), 2);
    let id_less = tree
        .iter()
        .find(|node| node.entry.id().is_empty())
        .expect("id-less node");
    assert_eq!(id_less.children.len(), 1);
    assert_eq!(id_less.children[0].entry.id(), new_id);
}

/// Finding 3：`id: null` 的条目让 leaf 成为 `null`（不是 `undefined`）：
/// 追加条目写 `"parentId":null`，树里两者都是根。
#[test]
fn null_id_entry_keeps_null_leaf() {
    let temp = temp_dir("gate-null-id-");
    let dir = temp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let path = write_file(
        &dir,
        "source.jsonl",
        &[
            header("sess3n"),
            "{\"type\":\"message\",\"id\":null,\"parentId\":null,\"timestamp\":\"t\",\"message\":{\"role\":\"user\",\"content\":\"x\",\"timestamp\":1}}"
                .to_string(),
        ],
    );

    let mut session = SessionManager::open(&path, Some(dir.to_str().unwrap()), None).unwrap();
    assert_eq!(session.get_entry_count(), 1);
    assert_eq!(session.get_leaf_id(), None);
    session.append_message(user_message("next", 2)).unwrap();
    let value = last_json_line(&path);
    assert_eq!(value.get("parentId"), Some(&serde_json::Value::Null));
    assert_eq!(session.get_tree().len(), 2);
}

/// Finding 3：`id` 是非字符串原始值（`7`）时，`getEntryCount` 计入该 Map 键、
/// `getEntries()` 原样保留 `id`。表示能力边界：typed `parentId` 无法写原始数字，
/// 追加条目省略 `parentId`（评审期望）。
#[test]
fn non_string_id_entry_is_counted_and_raw_id_preserved() {
    let temp = temp_dir("gate-number-id-");
    let dir = temp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let path = write_file(
        &dir,
        "source.jsonl",
        &[
            header("sess3b"),
            "{\"type\":\"message\",\"id\":7,\"parentId\":null,\"timestamp\":\"t\",\"message\":{\"role\":\"user\",\"content\":\"x\",\"timestamp\":1}}"
                .to_string(),
        ],
    );

    let mut session = SessionManager::open(&path, Some(dir.to_str().unwrap()), None).unwrap();
    assert_eq!(session.get_entry_count(), 1);
    assert_eq!(session.get_leaf_id(), None);
    let entries = session.get_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        serde_json::to_value(&entries[0]).unwrap().get("id"),
        Some(&serde_json::json!(7))
    );
    session.append_message(user_message("next", 2)).unwrap();
    let value = last_json_line(&path);
    assert!(value.get("parentId").is_none(), "{value}");
}

/// Finding 4(a)：缺少 `summary` 的 compaction 投影必须省略 `summary` 键（TS 里是 undefined），
/// 而不是空串；`convertToLlm` 的文本因此是 `PREFIX + "undefined" + SUFFIX`。
#[test]
fn compaction_missing_summary_omits_key_and_renders_undefined() {
    let content = [
        header("sess4"),
        "{\"type\":\"compaction\",\"id\":\"c1\",\"parentId\":null,\"timestamp\":\"not-a-date\",\"tokensBefore\":5}"
            .to_string(),
    ];
    let session = SessionManager::in_memory(
        &current_cwd(),
        None,
        Some(parse_session_entries(&(content.join("\n") + "\n"))),
    )
    .unwrap();

    let messages = session.build_session_context().messages;
    assert_eq!(messages.len(), 1);
    let value = serde_json::to_value(&messages[0]).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"role": "compactionSummary", "tokensBefore": 5, "timestamp": null})
    );

    let llm = convert_to_llm(&messages);
    let llm_value = serde_json::to_value(&llm[0]).unwrap();
    assert_eq!(
        llm_value["content"][0]["text"],
        serde_json::json!(format!(
            "{COMPACTION_SUMMARY_PREFIX}undefined{COMPACTION_SUMMARY_SUFFIX}"
        ))
    );
}

/// Finding 4(c)：非字符串 summary（`5`）按原始值进入上下文消息。
#[test]
fn non_string_summary_uses_raw_value() {
    // compaction：TS 输出 {role, summary: 5, tokensBefore: 3, timestamp: null}，
    // 且路径上更早的 user 消息被压缩裁剪掉。
    let content = [
        header("sess4c"),
        message_line("m1", "null", "hi"),
        "{\"type\":\"compaction\",\"id\":\"c1\",\"parentId\":\"m1\",\"timestamp\":\"t\",\"summary\":5,\"tokensBefore\":3}"
            .to_string(),
    ];
    let session = SessionManager::in_memory(
        &current_cwd(),
        None,
        Some(parse_session_entries(&(content.join("\n") + "\n"))),
    )
    .unwrap();
    let messages = session.build_session_context().messages;
    assert_eq!(messages.len(), 1, "user message must be trimmed");
    assert_eq!(
        serde_json::to_value(&messages[0]).unwrap(),
        serde_json::json!({"role": "compactionSummary", "summary": 5, "tokensBefore": 3, "timestamp": null})
    );

    // branch_summary：summary 为 5，fromId 原样。
    let content = [
        header("sess4d"),
        "{\"type\":\"branch_summary\",\"id\":\"b1\",\"parentId\":null,\"timestamp\":\"t\",\"fromId\":\"m1\",\"summary\":5}"
            .to_string(),
    ];
    let session = SessionManager::in_memory(
        &current_cwd(),
        None,
        Some(parse_session_entries(&(content.join("\n") + "\n"))),
    )
    .unwrap();
    let messages = session.build_session_context().messages;
    assert_eq!(messages.len(), 1);
    assert_eq!(
        serde_json::to_value(&messages[0]).unwrap(),
        serde_json::json!({"role": "branchSummary", "summary": 5, "fromId": "m1", "timestamp": null})
    );
}

struct NoModels;

impl ModelPriceSource for NoModels {
    fn get_model(&self, _provider: &str, _model_id: &str) -> Option<ModelPriceInfo> {
        None
    }
}

/// Finding 6：`cache_warm` 条目 usage 字段残缺（typed 解析失败）时，TS 仍用原始
/// `input + cacheRead + cacheWrite` 建立基线，后续 miss 必须被计数。
#[test]
fn partial_cache_warm_usage_sets_previous_request_baseline() {
    let content = [
        header("sess6"),
        "{\"type\":\"usage\",\"id\":\"u1\",\"parentId\":null,\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"kind\":\"cache_warm\",\"provider\":\"p\",\"model\":\"m\",\"usage\":{\"input\":5000,\"cacheRead\":5000,\"cacheWrite\":0}}"
            .to_string(),
        "{\"type\":\"message\",\"id\":\"a1\",\"parentId\":\"u1\",\"timestamp\":\"2026-01-01T00:01:00.000Z\",\"message\":{\"role\":\"assistant\",\"content\":[],\"api\":\"x\",\"provider\":\"p\",\"model\":\"m\",\"usage\":{\"input\":6000,\"output\":0,\"cacheRead\":0,\"cacheWrite\":0,\"totalTokens\":6000,\"cost\":{\"input\":0,\"output\":0,\"cacheRead\":0,\"cacheWrite\":0,\"total\":0}},\"stopReason\":\"stop\",\"timestamp\":1}}"
            .to_string(),
    ];
    let entries = entries_of(&(content.join("\n") + "\n"));
    assert!(matches!(entries[0], SessionEntry::Unknown(_)));
    assert_eq!(
        compute_cache_waste(&entries, &NoModels),
        CacheWasteTotals {
            missed_tokens: 6000.0,
            missed_cost: 0.0,
            miss_count: 1,
        }
    );
}

fn tool_call(id: &str) -> AgentToolCall {
    AgentToolCall {
        kind_tool_call: ToolCallKind::ToolCall,
        id: id.to_string(),
        name: "t".to_string(),
        arguments: kiana_ai::types::JsonObject::new(),
        thought_signature: None,
        namespace: None,
        arguments_error: None,
    }
}

/// Finding 7：嵌套调用的非对象 arguments 按 `JSON.stringify(value)` 记录（不是 `{}`），
/// `null` 按 `?? {}` 处理。
#[test]
fn nested_call_recorder_keeps_raw_arguments() {
    let mut recorder = NestedCallRecorder::new();
    let scalar = recorder.start_with_arguments(&tool_call("c/1"), &serde_json::json!(5));
    recorder.finish(scalar, false, "");
    let array = recorder.start_with_arguments(&tool_call("c/2"), &serde_json::json!([1, 2]));
    recorder.finish(array, false, "");
    let null = recorder.start_with_arguments(&tool_call("c/3"), &JsonValue::Null);
    recorder.finish(null, false, "");

    let snapshot = recorder.snapshot().expect("snapshot");
    assert_eq!(snapshot.calls[0]["arguments"], serde_json::json!(5));
    assert_eq!(snapshot.calls[1]["arguments"], serde_json::json!([1, 2]));
    assert_eq!(snapshot.calls[2]["arguments"], serde_json::json!({}));
    assert!(snapshot.complete);
}

/// Finding 7：超限的 arguments 记录真实 UTF-8 字节数（含引号的 JSON 文本长度）。
#[test]
fn nested_call_recorder_counts_raw_argument_bytes() {
    let mut recorder = NestedCallRecorder::new();
    let big = JsonValue::String("x".repeat(9000));
    recorder.start_with_arguments(&tool_call("c/4"), &big);
    let snapshot = recorder.snapshot().expect("snapshot");
    // JSON.stringify("x" * 9000) 的字节数 = 9000 + 2 个引号。
    assert_eq!(snapshot.calls[0]["argumentsBytes"], serde_json::json!(9002));
    assert!(!snapshot.complete);
}

/// Finding 8：`message` 条目因其它字段类型错误退化为 Unknown 时，TS 仍按原始
/// `entry.message.role/usage` 统计。
#[test]
fn usage_breakdown_reads_raw_message_of_unknown_entry() {
    let content = [
        header("sess8"),
        "{\"type\":\"message\",\"id\":\"m1\",\"parentId\":5,\"timestamp\":\"t\",\"message\":{\"role\":\"assistant\",\"provider\":\"p\",\"model\":\"m\",\"usage\":{\"input\":10,\"output\":2,\"cacheRead\":3,\"cacheWrite\":0,\"totalTokens\":15,\"cost\":{\"input\":1,\"output\":1,\"cacheRead\":0,\"cacheWrite\":0,\"total\":2}}}}"
            .to_string(),
    ];
    let entries = entries_of(&(content.join("\n") + "\n"));
    assert!(matches!(entries[0], SessionEntry::Unknown(_)));
    assert_eq!(
        get_usage_cost_breakdown(&entries),
        vec![UsageCostBreakdownEntry {
            key: "p/m".to_string(),
            cost: 2.0,
            tokens: 15.0,
        }]
    );
}

/// Finding 9：`findMostRecentSession(dir, "")` 的空串 cwd 是假值 → 不做 cwd 过滤。
#[test]
fn find_most_recent_session_empty_cwd_does_not_filter() {
    let temp = temp_dir("gate-recent-cwd-");
    let dir = temp.path().to_path_buf();
    let old_path = dir.join("old.jsonl").to_string_lossy().into_owned();
    let new_path = dir.join("new.jsonl").to_string_lossy().into_owned();
    std::fs::write(
        &old_path,
        format!(
            "{}\n",
            "{\"type\":\"session\",\"version\":3,\"id\":\"old\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":\"/some/other\"}"
        ),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(
        &new_path,
        format!(
            "{}\n",
            "{\"type\":\"session\",\"version\":3,\"id\":\"new\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":\"/another\"}"
        ),
    )
    .unwrap();

    let dir_string = dir.to_string_lossy().into_owned();
    // 空串：不过滤，返回最新的文件（即使 header cwd 不同）。
    assert_eq!(
        find_most_recent_session(&dir_string, Some("")),
        Some(new_path.clone())
    );
    assert_eq!(find_most_recent_session(&dir_string, None), Some(new_path));
    // 非空 cwd 没有匹配的 header → None。
    assert_eq!(find_most_recent_session(&dir_string, Some("/nope")), None);
}
