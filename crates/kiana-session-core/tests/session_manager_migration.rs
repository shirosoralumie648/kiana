//! TS: `test/session-manager/migration.test.ts`（2 例）。

mod common;

use kiana_session_core::session_manager::{
    FileEntry, SessionManager, migrate_session_entries, parse_session_entries,
};

fn parse(lines: &[serde_json::Value]) -> Vec<FileEntry> {
    let content = lines
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    parse_session_entries(&content)
}

fn json(entry: &FileEntry) -> serde_json::Value {
    serde_json::to_value(entry).unwrap()
}

/// TS: "should add id/parentId to v1 entries"
#[test]
fn should_add_id_and_parent_id_to_v1_entries() {
    let mut entries = parse(&[
        serde_json::json!({"type": "session", "id": "sess-1", "timestamp": "2025-01-01T00:00:00Z", "cwd": "/tmp"}),
        serde_json::json!({
            "type": "message",
            "timestamp": "2025-01-01T00:00:01Z",
            "message": {"role": "user", "content": "hi", "timestamp": 1},
        }),
        serde_json::json!({
            "type": "message",
            "timestamp": "2025-01-01T00:00:02Z",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": "hello"}],
                "api": "test",
                "provider": "test",
                "model": "test",
                "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0},
                "stopReason": "stop",
                "timestamp": 2,
            },
        }),
    ]);

    migrate_session_entries(&mut entries);

    // header 的 version 被设为 3（hookMessage→custom 迁移后的当前版本）。
    let header = json(&entries[0]);
    assert_eq!(
        header.get("version").and_then(serde_json::Value::as_f64),
        Some(3.0)
    );

    // 条目获得 id/parentId。
    let msg1 = json(&entries[1]);
    let msg2 = json(&entries[2]);

    let id1 = msg1
        .get("id")
        .and_then(serde_json::Value::as_str)
        .expect("msg1 id");
    assert_eq!(id1.len(), 8);
    assert_eq!(msg1.get("parentId"), Some(&serde_json::Value::Null));

    let id2 = msg2
        .get("id")
        .and_then(serde_json::Value::as_str)
        .expect("msg2 id");
    assert_eq!(id2.len(), 8);
    assert_eq!(
        msg2.get("parentId").and_then(serde_json::Value::as_str),
        Some(id1)
    );
}

/// TS: "should be idempotent (skip already migrated)"
#[test]
fn should_be_idempotent_skip_already_migrated() {
    let mut entries = parse(&[
        serde_json::json!({"type": "session", "id": "sess-1", "version": 2, "timestamp": "2025-01-01T00:00:00Z", "cwd": "/tmp"}),
        serde_json::json!({
            "type": "message",
            "id": "abc12345",
            "parentId": null,
            "timestamp": "2025-01-01T00:00:01Z",
            "message": {"role": "user", "content": "hi", "timestamp": 1},
        }),
        serde_json::json!({
            "type": "message",
            "id": "def67890",
            "parentId": "abc12345",
            "timestamp": "2025-01-01T00:00:02Z",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": "hello"}],
                "api": "test",
                "provider": "test",
                "model": "test",
                "usage": {"input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0},
                "stopReason": "stop",
                "timestamp": 2,
            },
        }),
    ]);

    migrate_session_entries(&mut entries);

    // id 保持不变。
    assert_eq!(
        json(&entries[1])
            .get("id")
            .and_then(serde_json::Value::as_str),
        Some("abc12345")
    );
    assert_eq!(
        json(&entries[2])
            .get("id")
            .and_then(serde_json::Value::as_str),
        Some("def67890")
    );
    assert_eq!(
        json(&entries[2])
            .get("parentId")
            .and_then(serde_json::Value::as_str),
        Some("abc12345")
    );
}

/// 回归（评审 #2）：迁移重写必须在 `JSON.parse` 得到的原始对象上原地修改——未知字段、
/// 未建模的条目类型、以及键位置都原样保留（TS 源：`_rewriteFile`）。
#[test]
fn rewrite_preserves_unknown_fields_and_key_order_like_typescript() {
    let temp = common::temp_dir("kiana-session-raw-");
    let path = temp.path().join("v2.jsonl");
    std::fs::write(
        &path,
        include_str!("fixtures/session_manager/v2_roundtrip.jsonl"),
    )
    .unwrap();

    let manager = SessionManager::open(path.to_str().unwrap(), None, None).unwrap();
    assert_eq!(manager.get_session_id(), "v2-session");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        include_str!("fixtures/session_manager/v2_rewritten.jsonl")
    );

    // forkFrom 逐字复制源条目（header 除外，顺序不变）。
    let forked =
        SessionManager::fork_from(path.to_str().unwrap(), "/tmp/fixture", None, None).unwrap();
    let fork_content = std::fs::read_to_string(forked.get_session_file().unwrap()).unwrap();
    let fork_entries: String = fork_content
        .split('\n')
        .skip(1)
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        fork_entries,
        include_str!("fixtures/session_manager/v2_fork_entries.jsonl")
    );
}

/// 回归（评审 #2）：v1 迁移在原始对象上追加 `id`/`parentId`（末尾），header 的 `version`
/// 也追加在末尾；`firstKeptEntryIndex` 被删除并换成追加的 `firstKeptEntryId`。
#[test]
fn v1_migration_rewrites_raw_objects_in_place() {
    let temp = common::temp_dir("kiana-session-v1-");
    let path = temp.path().join("v1.jsonl");
    std::fs::write(
        &path,
        include_str!("fixtures/session_manager/v1_migration.jsonl"),
    )
    .unwrap();

    let manager = SessionManager::open(path.to_str().unwrap(), None, None).unwrap();
    assert_eq!(manager.get_session_id(), "v1-session");

    let content = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.split('\n').collect();
    assert_eq!(
        lines[0],
        r#"{"type":"session","id":"v1-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp/fixture","version":3}"#
    );

    // 原有键位置不变；id/parentId 追加在末尾。
    assert!(
        lines[1].starts_with(
            r#"{"type":"message","timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"user","content":"hi","timestamp":1},"id":"#
        ),
        "{}",
        lines[1]
    );
    assert!(lines[1].ends_with(r#"","parentId":null}"#), "{}", lines[1]);
    let message: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    let message_id = message["id"].as_str().expect("message id");
    assert_eq!(message_id.len(), 8);

    // firstKeptEntryIndex 删除；firstKeptEntryId 追加在 parentId 之后。
    assert!(
        lines[2].starts_with(
            r#"{"type":"compaction","timestamp":"2026-01-01T00:00:02.000Z","summary":"s","tokensBefore":5,"id":"#
        ),
        "{}",
        lines[2]
    );
    let compaction: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
    assert!(compaction.get("firstKeptEntryIndex").is_none());
    assert_eq!(compaction["parentId"].as_str(), Some(message_id));
    assert_eq!(compaction["firstKeptEntryId"].as_str(), Some(message_id));
}

/// 回归：TS 只在 `typeof firstKeptEntryIndex === "number"` 时转换并删除该键；非数字值
/// （如字符串 `"1"`）原样保留。
#[test]
fn v1_migration_keeps_a_non_numeric_first_kept_entry_index() {
    let temp = common::temp_dir("kiana-session-v1-nonnumeric-");
    let path = temp.path().join("v1.jsonl");
    std::fs::write(
        &path,
        concat!(
            "{\"type\":\"session\",\"id\":\"v1-session\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":\"/tmp/fixture\"}\n",
            "{\"type\":\"compaction\",\"timestamp\":\"2026-01-01T00:00:02.000Z\",\"summary\":\"s\",\"firstKeptEntryIndex\":\"1\",\"tokensBefore\":5}\n",
        ),
    )
    .unwrap();

    SessionManager::open(path.to_str().unwrap(), None, None).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let compaction: serde_json::Value =
        serde_json::from_str(content.split('\n').nth(1).unwrap()).unwrap();
    assert_eq!(
        compaction
            .get("firstKeptEntryIndex")
            .and_then(serde_json::Value::as_str),
        Some("1")
    );
    assert!(compaction.get("firstKeptEntryId").is_none());
}

/// 回归（评审 round 2 #9）：`migrateToCurrentVersion` 用 JS 关系比较读取原始 `version`
/// （`header?.version ?? 1`）。`"abc"` 经 `Number()` 得到 `NaN`，所有 `<`/`>=` 比较都是
/// false：不执行任何字段迁移，但函数返回 true，文件仍被原样重写。
///
/// 期望值由 TS 参考实现生成（vitest probe，`SessionManager.open`）。
#[test]
fn non_numeric_version_runs_no_field_migration_like_typescript() {
    let temp = common::temp_dir("kiana-session-version-abc-");
    let path = temp.path().join("abc.jsonl");
    let original = concat!(
        r#"{"type":"session","id":"v","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp","version":"abc"}"#,
        "\n",
        r#"{"type":"message","timestamp":"2025-01-01T00:00:01.000Z","message":{"role":"hookMessage","content":"hi","timestamp":1}}"#,
        "\n",
    );
    std::fs::write(&path, original).unwrap();

    let manager = SessionManager::open(path.to_str().unwrap(), None, None).unwrap();
    assert_eq!(manager.get_session_id(), "v");
    // 文件被重写但内容逐字节不变：version 仍是 "abc"，没有 id/parentId，role 仍是
    // hookMessage。
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

/// 回归（评审 round 2 #9）：布尔 `version: true` 在 JS 里 `true < 2` 与 `true < 3` 都为真，
/// 因此 v1→v2 与 v2→v3 迁移都会执行（`id`/`parentId` 添加、`hookMessage` → `custom`、
/// header version 变成 3）。与 Rust 之前把非数字版本一律当成 1 的行为不同，但结果一致；
/// 这里锁定 TS 的行为。
#[test]
fn boolean_version_runs_both_migrations_like_typescript() {
    let temp = common::temp_dir("kiana-session-version-true-");
    let path = temp.path().join("true.jsonl");
    std::fs::write(
        &path,
        concat!(
            r#"{"type":"session","id":"v","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp","version":true}"#,
            "\n",
            r#"{"type":"message","timestamp":"2025-01-01T00:00:01.000Z","message":{"role":"hookMessage","content":"hi","timestamp":1}}"#,
            "\n",
        ),
    )
    .unwrap();

    let _manager = SessionManager::open(path.to_str().unwrap(), None, None).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.trim_end().split('\n').collect();
    let header: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(header["version"], 3);
    let message: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert!(message.get("id").and_then(|value| value.as_str()).is_some());
    assert!(message.get("parentId").is_some());
    assert_eq!(message["message"]["role"], "custom");
}
