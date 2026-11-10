//! TS: `test/session-manager/labels.test.ts`（9 例）。

mod common;

use common::*;
use kiana_session_core::session_manager::{
    LabelEntry, SessionEntry, SessionManager, SessionTreeNode,
};

fn session() -> SessionManager {
    SessionManager::in_memory(&current_cwd(), None, None).unwrap()
}

fn user(text: &str, timestamp: i64) -> kiana_agent_core::AgentMessage {
    user_message(text, timestamp)
}

fn find_label(entries: &[SessionEntry], id: &str) -> LabelEntry {
    entries
        .iter()
        .find_map(|entry| match entry {
            SessionEntry::Label(label) if label.id == id => Some(label.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("label entry {id} not found"))
}

fn find_node<'a>(nodes: &'a [SessionTreeNode], id: &str) -> Option<&'a SessionTreeNode> {
    for node in nodes {
        if node.entry.id() == id {
            return Some(node);
        }
        if let Some(found) = find_node(&node.children, id) {
            return Some(found);
        }
    }
    None
}

/// TS: "sets and gets labels"
#[test]
fn sets_and_gets_labels() {
    let mut session = session();

    let msg_id = session.append_message(user("hello", 1)).unwrap();

    // 初始无标签。
    assert!(session.get_label(&msg_id).is_none());

    // 设置标签。
    let label_id = session
        .append_label_change(&msg_id, Some("checkpoint"))
        .unwrap();
    assert_eq!(session.get_label(&msg_id), Some("checkpoint"));

    // 标签条目出现在 entries 里。
    let entries = session.get_entries();
    let label_entry = find_label(&entries, &label_id);
    assert_eq!(label_entry.id, label_id);
    assert_eq!(label_entry.target_id, msg_id);
    assert_eq!(label_entry.label.as_deref(), Some("checkpoint"));
}

/// TS: "clears labels with undefined"
#[test]
fn clears_labels_with_undefined() {
    let mut session = session();

    let msg_id = session.append_message(user("hello", 1)).unwrap();

    session
        .append_label_change(&msg_id, Some("checkpoint"))
        .unwrap();
    assert_eq!(session.get_label(&msg_id), Some("checkpoint"));

    // 清除标签。
    session.append_label_change(&msg_id, None).unwrap();
    assert!(session.get_label(&msg_id).is_none());
}

/// TS: "last label wins"
#[test]
fn last_label_wins() {
    let mut session = session();

    let msg_id = session.append_message(user("hello", 1)).unwrap();

    session.append_label_change(&msg_id, Some("first")).unwrap();
    session
        .append_label_change(&msg_id, Some("second"))
        .unwrap();
    let last_label_id = session.append_label_change(&msg_id, Some("third")).unwrap();

    assert_eq!(session.get_label(&msg_id), Some("third"));

    let entries = session.get_entries();
    let last_label_entry = find_label(&entries, &last_label_id);
    let tree = session.get_tree();
    let msg_node = find_node(&tree, &msg_id).expect("message node");
    assert_eq!(
        msg_node.label_timestamp.as_deref(),
        Some(last_label_entry.timestamp.as_str())
    );
}

/// TS: "labels are included in tree nodes"
#[test]
fn labels_are_included_in_tree_nodes() {
    let mut session = session();

    let msg1_id = session.append_message(user("hello", 1)).unwrap();
    let msg2_id = session.append_message(assistant_message("hi", 2)).unwrap();

    let msg1_label_id = session
        .append_label_change(&msg1_id, Some("start"))
        .unwrap();
    let msg2_label_id = session
        .append_label_change(&msg2_id, Some("response"))
        .unwrap();

    let entries = session.get_entries();
    let msg1_label_entry = find_label(&entries, &msg1_label_id);
    let msg2_label_entry = find_label(&entries, &msg2_label_id);
    let tree = session.get_tree();

    // 找到消息节点（跳过 label 条目）。
    let msg1_node = find_node(&tree, &msg1_id).expect("msg1 node");
    assert_eq!(msg1_node.label.as_deref(), Some("start"));
    assert_eq!(
        msg1_node.label_timestamp.as_deref(),
        Some(msg1_label_entry.timestamp.as_str())
    );

    // msg2 是 msg1 的子节点。
    let msg2_node = find_node(&msg1_node.children, &msg2_id).expect("msg2 node");
    assert_eq!(msg2_node.label.as_deref(), Some("response"));
    assert_eq!(
        msg2_node.label_timestamp.as_deref(),
        Some(msg2_label_entry.timestamp.as_str())
    );
}

/// TS: "labels are preserved in createBranchedSession"
#[test]
fn labels_are_preserved_in_create_branched_session() {
    let mut session = session();

    let msg1_id = session.append_message(user("hello", 1)).unwrap();
    let msg2_id = session.append_message(assistant_message("hi", 2)).unwrap();

    let msg1_label_id = session
        .append_label_change(&msg1_id, Some("important"))
        .unwrap();
    let msg2_label_id = session
        .append_label_change(&msg2_id, Some("also-important"))
        .unwrap();
    let original_entries = session.get_entries();
    let msg1_label_entry = find_label(&original_entries, &msg1_label_id);
    let msg2_label_entry = find_label(&original_entries, &msg2_label_id);

    // 从 msg2 分支（内存态返回 None，但会更新内部状态）。
    session.create_branched_session(&msg2_id).unwrap();

    // 标签保留。
    assert_eq!(session.get_label(&msg1_id), Some("important"));
    assert_eq!(session.get_label(&msg2_id), Some("also-important"));

    // 新的 label 条目存在。
    let entries = session.get_entries();
    let label_entries: Vec<&LabelEntry> = entries
        .iter()
        .filter_map(|entry| match entry {
            SessionEntry::Label(label) => Some(label),
            _ => None,
        })
        .collect();
    assert_eq!(label_entries.len(), 2);

    let tree = session.get_tree();
    let msg1_node = find_node(&tree, &msg1_id).expect("msg1 node");
    let msg2_node = find_node(&msg1_node.children, &msg2_id).expect("msg2 node");
    assert_eq!(
        msg1_node.label_timestamp.as_deref(),
        Some(msg1_label_entry.timestamp.as_str())
    );
    assert_eq!(
        msg2_node.label_timestamp.as_deref(),
        Some(msg2_label_entry.timestamp.as_str())
    );
}

/// TS: "rewires children of removed labels when forking"
#[test]
fn rewires_children_of_removed_labels_when_forking() {
    let mut session = session();

    let msg1_id = session.append_message(user("hello", 1)).unwrap();
    session
        .append_label_change(&msg1_id, Some("checkpoint"))
        .unwrap();
    let model_change_id = session
        .append_model_change("anthropic", "claude-test")
        .unwrap();
    let msg2_id = session.append_message(user("followup", 2)).unwrap();

    session.create_branched_session(&msg2_id).unwrap();

    assert_eq!(
        session.get_entry(&model_change_id).unwrap().parent_id(),
        Some(msg1_id.as_str())
    );
}

/// TS: "labels not on path are not preserved in createBranchedSession"
#[test]
fn labels_not_on_path_are_not_preserved_in_create_branched_session() {
    let mut session = session();

    let msg1_id = session.append_message(user("hello", 1)).unwrap();
    let msg2_id = session.append_message(assistant_message("hi", 2)).unwrap();
    let msg3_id = session.append_message(user("followup", 3)).unwrap();

    // 给所有消息加标签。
    session
        .append_label_change(&msg1_id, Some("first"))
        .unwrap();
    session
        .append_label_change(&msg2_id, Some("second"))
        .unwrap();
    session
        .append_label_change(&msg3_id, Some("third"))
        .unwrap();

    // 从 msg2 分支（排除 msg3）。
    session.create_branched_session(&msg2_id).unwrap();

    // 只保留 msg1 与 msg2 的标签。
    assert_eq!(session.get_label(&msg1_id), Some("first"));
    assert_eq!(session.get_label(&msg2_id), Some("second"));
    assert!(session.get_label(&msg3_id).is_none());
}

/// TS: "labels are not included in buildSessionContext"
#[test]
fn labels_are_not_included_in_build_session_context() {
    let mut session = session();

    let msg_id = session.append_message(user("hello", 1)).unwrap();
    session
        .append_label_change(&msg_id, Some("checkpoint"))
        .unwrap();

    let ctx = session.build_session_context();
    assert_eq!(ctx.messages.len(), 1);
    assert_eq!(ctx.messages[0].role(), "user");
}

/// TS: "throws when labeling non-existent entry"
#[test]
fn throws_when_labeling_non_existent_entry() {
    let mut session = session();
    let error = error_text(session.append_label_change("non-existent", Some("label")));
    assert!(
        error.contains("Entry non-existent not found"),
        "unexpected error: {error}"
    );
}

/// 回归（评审 round 2 #10）：`_buildIndex` 直接读取原始 `label` 条目字段——
/// 真值标签原样生效（非字符串按 JS `String(value)` 承载）、假值（`0`/`""`/`null`/缺失）
/// 清除之前的标签；非字符串 `timestamp` 同样保留。
///
/// 期望语义由 TS 参考实现生成（vitest probe：`getLabel` 返回原始值；`getTree` 的
/// `labelTimestamp` 为原始 timestamp）。
#[test]
fn labels_read_unvalidated_values_and_falsy_clears() {
    let temp = temp_dir("kiana-session-labels-raw-");
    let dir = temp.path();
    let write = |name: &str, extra: &str, prior: Option<&str>| {
        let mut lines = vec![
            r#"{"type":"session","version":3,"id":"s","timestamp":"2025-01-01T00:00:00.000Z","cwd":"/tmp"}"#.to_string(),
            r#"{"type":"message","id":"t1","parentId":null,"timestamp":"2025-01-01T00:00:01.000Z","message":{"role":"user","content":"hi","timestamp":1}}"#.to_string(),
        ];
        if let Some(prior) = prior {
            lines.push(format!(
                r#"{{"type":"label","id":"l0","parentId":"t1","timestamp":"2025-01-01T00:00:01.000Z","targetId":"t1","label":"{prior}"}}"#
            ));
        }
        lines.push(format!(
            r#"{{"type":"label","id":"l1","parentId":"{}","targetId":"t1",{extra}}}"#,
            if prior.is_some() { "l0" } else { "t1" }
        ));
        std::fs::write(dir.join(format!("{name}.jsonl")), lines.join("\n") + "\n").unwrap();
        SessionManager::open(
            dir.join(format!("{name}.jsonl")).to_str().unwrap(),
            Some(dir.to_str().unwrap()),
            None,
        )
        .unwrap()
    };

    // 数字标签：TS 的 getLabel 返回 5（Rust 的 String 映射为 "5"）。
    let manager = write(
        "num",
        r#""timestamp":"2025-01-01T00:00:01.000Z","label":5"#,
        None,
    );
    assert_eq!(manager.get_label("t1"), Some("5"));
    // 数字 timestamp 保留（TS 返回 123）。
    let manager = write("stamp", r#""timestamp":123,"label":"x""#, None);
    assert_eq!(manager.get_label("t1"), Some("x"));
    assert_eq!(
        manager
            .get_tree()
            .iter()
            .find(|node| node.entry.id() == "t1")
            .and_then(|node| node.label_timestamp.clone()),
        Some("123".to_string())
    );
    // 假值标签清除之前的标签（此前该条目会被当作 Unknown 而忽略）。
    let manager = write("zero", r#""label":0"#, Some("prior"));
    assert_eq!(manager.get_label("t1"), None);
    assert!(
        manager
            .get_tree()
            .iter()
            .find(|node| node.entry.id() == "t1")
            .and_then(|node| node.label_timestamp.clone())
            .is_none()
    );
    // 空串同样清除。
    let manager = write("empty", r#""label":"""#, Some("prior"));
    assert_eq!(manager.get_label("t1"), None);
}
