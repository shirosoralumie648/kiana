//! `appendCompaction` / `appendUsage` 的条目形状（spec §2.2.10、§9.2.11、§9.2.18）。
//!
//! 期望值由 TS 参考实现生成（node probe：`SessionManager.appendCompaction` /
//! `appendUsage` 的对象字面量）。

mod common;

use common::*;
use kiana_session_core::session_manager::{SessionEntry, SessionManager, entry_json_string};

/// 追加压缩摘要时捕获当前 system 消息：内容用 `"\n\n"` 连接，`timestamp` 被替换为
/// **条目** ISO 时间的毫秒数；retain-none 的 `firstKeptEntryId` 回退为自身 id。
#[test]
fn append_compaction_captures_the_current_system_message() {
    let mut session = SessionManager::in_memory("/project", None, None).unwrap();
    session
        .append_message(system_message("base prompt", 123))
        .unwrap();
    let second_system_id = session
        .append_message(system_message("tool block", 456))
        .unwrap();

    let id = session
        .append_compaction(
            "summary",
            None,
            10.0,
            Some(serde_json::json!({"k": 1})),
            Some(false),
            None,
        )
        .unwrap();
    let SessionEntry::Compaction(entry) = session.get_entry(&id).unwrap().clone() else {
        panic!("expected a compaction entry");
    };

    assert_eq!(entry.first_kept_entry_id, id);
    assert_eq!(entry.parent_id.as_deref(), Some(second_system_id.as_str()));
    assert_eq!(entry.details, Some(serde_json::json!({"k": 1})));
    assert_eq!(entry.from_hook, Some(false));
    assert_eq!(entry.usage, None);

    let system_message = entry.system_message.as_ref().expect("system message");
    assert_eq!(
        system_message.content,
        kiana_ai::types::SystemContent::Text("base prompt\n\ntool block".to_string())
    );
    // `new Date(entry.timestamp).getTime()`，不是原 system 消息的 123。
    let expected = kiana_ai::model_catalog::js_date_parse(&entry.timestamp).unwrap() as i64;
    assert_eq!(system_message.timestamp, expected);
    assert_ne!(system_message.timestamp, 123);

    // 键序与 TS 字面量一致；未设置的 `usage` 省略。
    let serialized = entry_json_string(&SessionEntry::Compaction(entry));
    assert!(
        serialized.starts_with("{\"type\":\"compaction\",\"id\":\""),
        "{serialized}"
    );
    assert!(
        serialized.contains("\"details\":{\"k\":1},\"fromHook\":false,\"systemMessage\":{\"role\":\"system\",\"content\":\"base prompt\\n\\ntool block\",\"timestamp\":"),
        "{serialized}"
    );
    assert!(
        serialized.ends_with(&format!("\"timestamp\":{expected}}}}}")),
        "{serialized}"
    );
    assert!(!serialized.contains("\"usage\""));
}

#[test]
fn compaction_system_message_preserves_tools_added_before_timestamp() {
    // TS source: session-manager.ts#appendCompaction spreads systemMessage then
    // assigns timestamp, retaining the tool-change fields' original positions.
    // Golden provenance: reproduce with Node 26 using SessionManager with an
    // initial system message carrying sections/toolsAdded, appendCompaction(),
    // then JSON.stringify(getEntries().at(-1)); fixed ids/timestamps are those
    // passed to that isolated construction. Regenerate only from the read-only
    // reference tree; never replace this with Rust output.
    let expected = include_str!("golden/compaction_system_message.json");
    let entry: SessionEntry = serde_json::from_str(expected).unwrap();
    assert_eq!(entry_json_string(&entry), expected.trim());

    let mut session = SessionManager::in_memory("/project", None, None).unwrap();
    let mut message = match system_message("prompt", 1) {
        kiana_agent_core::AgentMessage::Standard(kiana_ai::types::Message::System(message)) => {
            message
        }
        _ => unreachable!(),
    };
    message.tools_added = Some(vec![kiana_ai::types::Tool {
        name: "read".into(),
        description: "Read".into(),
        parameters: serde_json::json!({"type":"object"}),
        constrained_sampling: None,
    }]);
    session
        .append_message(kiana_agent_core::AgentMessage::Standard(
            kiana_ai::types::Message::System(message),
        ))
        .unwrap();
    let id = session
        .append_compaction("summary", None, 42.0, None, None, None)
        .unwrap();
    let text = entry_json_string(session.get_entry(&id).unwrap());
    let system_start = text.find("\"systemMessage\":").unwrap();
    let system = &text[system_start..];
    let tools = system.find("\"toolsAdded\"").unwrap();
    let timestamp = system[tools..].find("\"timestamp\"").unwrap() + tools;
    assert!(tools < timestamp, "{text}");
    assert_eq!(
        system[tools..].matches("\"toolsAdded\"").count(),
        1,
        "{text}"
    );
}

/// 没有 system 消息时压缩条目不写 `systemMessage` 键；`details`/`fromHook` 也按需省略。
#[test]
fn append_compaction_omits_the_system_message_when_there_is_none() {
    let mut session = SessionManager::in_memory("/project", None, None).unwrap();
    let user_id = session.append_message(user_message("hi", 1)).unwrap();
    let id = session
        .append_compaction("plain", None, 5.0, None, None, None)
        .unwrap();
    let entry = session.get_entry(&id).unwrap();
    let serialized = entry_json_string(entry);
    assert_eq!(
        serialized,
        format!(
            "{{\"type\":\"compaction\",\"id\":\"{id}\",\"parentId\":\"{user_id}\",\"timestamp\":\"{}\",\"summary\":\"plain\",\"firstKeptEntryId\":\"{id}\",\"tokensBefore\":5}}",
            entry.timestamp().unwrap()
        )
    );
}

/// `appendUsage` 返回条目对象（不是 id）；`note` 用 truthiness 条件展开，空串不写键。
#[test]
fn append_usage_returns_the_entry_and_omits_falsy_notes() {
    use kiana_session_core::session_manager::{SessionEntry as Entry, SessionManager as Manager};

    let mut session = Manager::in_memory("/project", None, None).unwrap();
    let usage = usage_with(1, 2, 3, 0.25);

    let entry = session
        .append_usage("cache_warm", "p", "m", usage.clone(), None)
        .unwrap();
    assert!(matches!(
        session.get_entry(&entry.id),
        Some(Entry::Usage(_))
    ));
    let serialized = entry_json_string(&Entry::Usage(entry.clone()));
    assert!(!serialized.contains("\"note\""), "{serialized}");

    let empty_note = session
        .append_usage("cache_warm", "p", "m", usage.clone(), Some(""))
        .unwrap();
    let serialized = entry_json_string(&Entry::Usage(empty_note));
    assert!(!serialized.contains("\"note\""), "{serialized}");

    let noted = session
        .append_usage("cache_warm", "p", "m", usage, Some("manual"))
        .unwrap();
    let serialized = entry_json_string(&Entry::Usage(noted));
    assert!(serialized.ends_with("\"note\":\"manual\"}"), "{serialized}");
}
