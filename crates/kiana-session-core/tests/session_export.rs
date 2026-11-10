//! `core/session-export.ts` 的契约测试（spec `ca-session.md` §2.13/§9.2）。
//!
//! TS 仓库没有直接对应测试文件；这里的断言来自导出格式的冻结契约。

mod common;

use std::sync::Arc;

use common::*;
use kiana_session_core::session_export::{
    TrailingEntries, export_session_to_jsonl, json_line, serialize_session_branch,
};
use kiana_session_core::session_manager::SessionManager;

fn parse_lines(content: &str) -> Vec<serde_json::Value> {
    content
        .trim_end()
        .split('\n')
        .map(|line| serde_json::from_str(line).expect("json line"))
        .collect()
}

/// header 不含 `parentSession`，分支按当前 leaf 重新线性化，末尾保留换行。
#[test]
fn serialize_session_branch_writes_a_linearized_branch() {
    let mut session = SessionManager::in_memory("/project", None, None).unwrap();
    let first = session.append_message(user_message("first", 0)).unwrap();
    let second = session
        .append_message(assistant_message("second", 0))
        .unwrap();
    // 制造一个不在当前分支上的条目，确保只导出当前分支。
    session.branch(&first).unwrap();
    let branched = session.append_message(user_message("branched", 0)).unwrap();
    session.branch(&second).unwrap();

    let content = serialize_session_branch(&session, None);
    assert!(content.ends_with('\n'));
    let lines = parse_lines(&content);
    assert_eq!(lines.len(), 3);

    let header = &lines[0];
    assert_eq!(
        header.get("type").and_then(serde_json::Value::as_str),
        Some("session")
    );
    assert_eq!(
        header.get("version").and_then(serde_json::Value::as_f64),
        Some(3.0)
    );
    assert_eq!(
        header.get("id").and_then(serde_json::Value::as_str),
        Some(session.get_session_id())
    );
    assert_eq!(
        header.get("cwd").and_then(serde_json::Value::as_str),
        Some("/project")
    );
    assert!(header.get("parentSession").is_none());

    // 分支被重新线性化：首条 parentId null，其后每条指向导出序列里的前一条。
    assert_eq!(
        lines[1].get("id").and_then(serde_json::Value::as_str),
        Some(first.as_str())
    );
    assert_eq!(lines[1].get("parentId"), Some(&serde_json::Value::Null));
    assert_eq!(
        lines[2].get("id").and_then(serde_json::Value::as_str),
        Some(second.as_str())
    );
    assert_eq!(
        lines[2].get("parentId").and_then(serde_json::Value::as_str),
        Some(first.as_str())
    );
    // branched 条目不在当前分支上。
    assert!(!content.contains(&branched));
}

/// 追加导出条目回调收到最后一个导出的 id（空分支为 `null`）与 header 时间戳。
#[test]
fn serialize_session_branch_appends_trailing_entries() {
    let mut session = SessionManager::in_memory("/project", None, None).unwrap();
    let first = session.append_message(user_message("first", 0)).unwrap();
    let second = session.append_message(user_message("second", 0)).unwrap();

    let trailing: TrailingEntries = Arc::new(|parent_id: Option<&str>, timestamp: &str| {
        vec![serde_json::json!({
            "type": "export_info",
            "id": "export-1",
            "parentId": parent_id,
            "timestamp": timestamp,
        })]
    });
    let content = serialize_session_branch(&session, Some(&trailing));
    let lines = parse_lines(&content);
    assert_eq!(lines.len(), 4);
    let extra = &lines[3];
    assert_eq!(
        extra.get("type").and_then(serde_json::Value::as_str),
        Some("export_info")
    );
    assert_eq!(
        extra.get("parentId").and_then(serde_json::Value::as_str),
        Some(second.as_str())
    );
    assert!(
        extra
            .get("timestamp")
            .and_then(serde_json::Value::as_str)
            .is_some()
    );
    let _ = first;

    // 空分支：parentId 为 null。
    let empty = SessionManager::in_memory("/project", None, None).unwrap();
    let content = serialize_session_branch(&empty, Some(&trailing));
    let lines = parse_lines(&content);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1].get("parentId"), Some(&serde_json::Value::Null));
}

/// 自定义输出路径：目录递归创建，内容与序列化结果一致。
#[test]
fn export_session_to_jsonl_writes_the_serialized_branch() {
    let temp = temp_dir("kiana-session-export-");
    let dir = temp.path().to_string_lossy().into_owned();
    let output_path = format!("{dir}/nested/dir/session.jsonl");

    let mut session = SessionManager::in_memory("/project", None, None).unwrap();
    session.append_message(user_message("hi", 0)).unwrap();

    let written = export_session_to_jsonl(&session, Some(&output_path), None).unwrap();
    assert_eq!(written, output_path);
    let content = std::fs::read_to_string(&output_path).unwrap();
    assert!(content.ends_with('\n'));
    let lines = parse_lines(&content);
    assert_eq!(
        lines[0].get("type").and_then(serde_json::Value::as_str),
        Some("session")
    );
    assert_eq!(
        lines[1].get("type").and_then(serde_json::Value::as_str),
        Some("message")
    );

    // 覆盖已有文件。
    let before = std::fs::read_to_string(&output_path).unwrap();
    session
        .append_message(assistant_message("done", 0))
        .unwrap();
    export_session_to_jsonl(&session, Some(&output_path), None).unwrap();
    let after = std::fs::read_to_string(&output_path).unwrap();
    assert_ne!(before, after);
    assert_eq!(parse_lines(&after).len(), 3);
}

/// `json_line` 按 `JSON.stringify` 语义写出单行（无换行）。
#[test]
fn json_line_serializes_like_json_stringify() {
    let value = serde_json::json!({"type": "session", "version": 3.0, "id": "abc"});
    assert_eq!(
        json_line(&value),
        "{\"type\":\"session\",\"version\":3,\"id\":\"abc\"}"
    );
}

/// 回归（评审 #2）：导出与 `createBranchedSession` 在 `JSON.parse` 得到的原始对象上展开
/// `{ ...entry, parentId }`——未知字段、未建模条目类型与键位置逐字保留。
///
/// 期望字节由 TS 参考实现生成（`serializeSessionBranch` / `createBranchedSession`）。
#[test]
fn export_and_branched_session_preserve_raw_entries() {
    let temp = temp_dir("kiana-session-export-raw-");
    let source = temp.path().join("raw.jsonl");
    std::fs::write(
        &source,
        include_str!("fixtures/session_manager/raw_roundtrip.jsonl"),
    )
    .unwrap();

    let manager = SessionManager::open(source.to_str().unwrap(), None, None).unwrap();
    let exported = serialize_session_branch(&manager, None);
    let exported_entries: String = exported.split('\n').skip(1).collect::<Vec<_>>().join("\n");
    assert_eq!(
        exported_entries,
        include_str!("fixtures/session_manager/raw_roundtrip_entries.jsonl")
    );

    let mut manager = manager;
    let branched = manager.create_branched_session("k1").unwrap().unwrap();
    let branched_content = std::fs::read_to_string(&branched).unwrap();
    let branched_entries: String = branched_content
        .split('\n')
        .skip(1)
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        branched_entries,
        include_str!("fixtures/session_manager/raw_roundtrip_entries.jsonl")
    );
}
