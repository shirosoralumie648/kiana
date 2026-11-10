//! TS: `test/session-manager/save-entry.test.ts`（1 例），外加 spec §4.2 的
//! 逐字节 JSONL 黄金测试（`tests/fixtures/session_manager/entries.jsonl` 由 TS
//! `JSON.stringify(entry)` 生成）。
//!
//! 每个已知条目类型都必须 `parse(serialize(x)) == x` 且写出字节保持稳定（当前稳定契约，
//! 改动需同步测试）。

mod common;

use kiana_session_core::session_manager::{
    FileEntry, SessionEntry, SessionManager, entry_json_string, parse_session_entries,
};

/// 黄金夹具逐行回环：`JSON.stringify(entry)` 的字节是当前稳定契约。
#[test]
fn entry_jsonl_fixture_round_trips_byte_exactly() {
    let fixture = include_str!("fixtures/session_manager/entries.jsonl");
    let lines: Vec<&str> = fixture
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let entries = parse_session_entries(fixture);
    assert_eq!(entries.len(), lines.len(), "every fixture line must parse");

    for (index, (line, entry)) in lines.iter().zip(entries.iter()).enumerate() {
        let serialized = entry_json_string(entry);
        assert_eq!(
            &serialized, line,
            "line {index} must serialize byte-exactly"
        );
        // 再解析一次，确保类型视图稳定。
        let reparsed = parse_session_entries(&serialized);
        assert_eq!(reparsed.len(), 1);
        assert_eq!(entry_json_string(&reparsed[0]), serialized);
    }

    // header 与关键条目的类型视图。
    assert!(matches!(&entries[0], FileEntry::Header(header) if header.version == Some(3.0)));
    assert!(matches!(
        &entries[1],
        FileEntry::Entry(SessionEntry::Message(_))
    ));
    let compaction = entries
        .iter()
        .find_map(|entry| match entry {
            FileEntry::Entry(SessionEntry::Compaction(compaction)) => Some(compaction),
            _ => None,
        })
        .expect("fixture contains a compaction entry");
    assert_eq!(compaction.details, Some(serde_json::Value::Null));
    assert_eq!(compaction.from_hook, Some(false));
    assert!(compaction.system_message.is_some());

    let labels: Vec<&Option<String>> = entries
        .iter()
        .filter_map(|entry| match entry {
            FileEntry::Entry(SessionEntry::Label(label)) => Some(&label.label),
            _ => None,
        })
        .collect();
    assert_eq!(
        labels,
        vec![
            &Some("checkpoint-1".to_string()),
            &None,
            &Some(String::new())
        ]
    );
}

mod save_custom_entry {
    use super::common::*;
    use kiana_agent_core::AgentMessage;
    use kiana_session_core::session_manager::SessionManager;

    /// TS: "saves custom entries and includes them in tree traversal"
    #[test]
    fn saves_custom_entries_and_includes_them_in_tree_traversal() {
        let mut session = SessionManager::in_memory("/project", None, None).unwrap();

        let msg_id = session
            .append_message(user_message("hello", 1))
            .expect("append message");
        let custom_id = session
            .append_custom_entry("my_data", Some(serde_json::json!({"foo": "bar"})))
            .expect("append custom entry");
        let msg2_id = session
            .append_message(assistant_message("hi", 2))
            .expect("append message");

        let entries = session.get_entries();
        assert_eq!(entries.len(), 3);

        let custom_entry = entries
            .iter()
            .find_map(|entry| match entry {
                kiana_session_core::session_manager::SessionEntry::Custom(custom) => Some(custom),
                _ => None,
            })
            .expect("custom entry");
        assert_eq!(custom_entry.custom_type, "my_data");
        assert_eq!(custom_entry.data, Some(serde_json::json!({"foo": "bar"})));
        assert_eq!(custom_entry.id, custom_id);
        assert_eq!(custom_entry.parent_id.as_deref(), Some(msg_id.as_str()));

        let path = session.get_branch(None);
        assert_eq!(path.len(), 3);
        assert_eq!(path[0].id(), msg_id);
        assert_eq!(path[1].id(), custom_id);
        assert_eq!(path[2].id(), msg2_id);

        let context = session.build_session_context();
        assert_eq!(context.messages.len(), 2);
        assert!(matches!(context.messages[1], AgentMessage::Standard(_)));
    }
}

/// 回归（评审 #1）：`appendCustomEntry` 的对象字面量顺序是
/// `type, customType, data?, id, parentId, timestamp`（**不是** base 前置）；
/// `appendCustomMessageEntry` 同样是 `type, customType, content, display, details, id, ...`。
///
/// 期望字节由 TS 参考实现生成（vitest probe）。
#[test]
fn custom_entry_uses_the_typescript_key_order() {
    let mut session = SessionManager::in_memory("/project", None, None).unwrap();
    session
        .append_custom_entry("my-ext", Some(serde_json::json!({"a": 1})))
        .unwrap();
    let entry = session.get_entries().pop().expect("custom entry");
    let serialized = entry_json_string(&entry);
    assert!(
        serialized.starts_with(
            "{\"type\":\"custom\",\"customType\":\"my-ext\",\"data\":{\"a\":1},\"id\":\""
        ),
        "{serialized}"
    );
    let position = |needle: &str| serialized.find(needle).unwrap();
    assert!(position("\"customType\"") < position("\"data\""));
    assert!(position("\"data\"") < position("\"id\""));
    assert!(position("\"id\"") < position("\"parentId\""));
    assert!(position("\"parentId\"") < position("\"timestamp\""));

    session.append_custom_entry("no-data", None).unwrap();
    let entry = session.get_entries().pop().expect("custom entry");
    let serialized = entry_json_string(&entry);
    assert!(
        serialized.starts_with("{\"type\":\"custom\",\"customType\":\"no-data\",\"id\":\""),
        "{serialized}"
    );

    // 新会话 header 的字段顺序也是 TS 字面量顺序。
    let header = session.get_header().expect("header");
    let serialized = entry_json_string(&FileEntry::Header(header.clone()));
    assert!(
        serialized.starts_with("{\"type\":\"session\",\"version\":3,\"id\":\""),
        "{serialized}"
    );
}
