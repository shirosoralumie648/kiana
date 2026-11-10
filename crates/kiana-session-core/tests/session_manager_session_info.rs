//! 会话元数据（`session_info`）与 `newSession` 的边界行为。
//!
//! TS: `session-manager.ts#SessionManager.appendSessionInfo` / `#getSessionName` /
//! `#newSession`（验收清单 §2.2.10、§9.2.2、§9.2.3）。

mod common;

use common::*;
use kiana_session_core::session_manager::{
    FileEntry, NewSessionOptions, SessionEntry, SessionManager, entry_json_string,
    parse_session_entries,
};

fn name_entries(entries: &[SessionEntry]) -> Vec<Option<String>> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            SessionEntry::SessionInfo(info) => Some(info.name.clone()),
            _ => None,
        })
        .collect()
}

/// TS: `name.replace(/[\r\n]+/g, " ").trim()`（回归 5996 的 SessionManager 层行为）。
#[test]
fn append_session_info_collapses_newlines_and_trims() {
    let mut session = SessionManager::in_memory("/project", None, None).unwrap();

    session
        .append_session_info("hello\nworld\r\nagain")
        .unwrap();
    assert_eq!(
        session.get_session_name().as_deref(),
        Some("hello world again")
    );

    session.append_session_info("  padded  ").unwrap();
    assert_eq!(session.get_session_name().as_deref(), Some("padded"));

    // 条目落盘字段为 `type,id,parentId,timestamp,name`；空名显式清除标题。
    session.append_session_info("").unwrap();
    assert_eq!(session.get_session_name(), None);
    let names = name_entries(&session.get_entries());
    assert_eq!(
        names,
        [
            Some("hello world again".to_string()),
            Some("padded".to_string()),
            Some(String::new()),
        ]
    );

    let value = serde_json::to_value(session.get_entries().last().unwrap()).unwrap();
    let keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["type", "id", "parentId", "timestamp", "name"]);
    assert_eq!(
        value.get("name"),
        Some(&serde_json::Value::String(String::new()))
    );
}

/// `getSessionName()` 反向扫描 **fileEntries**：遇到第一条 `session_info` 就返回，
/// 并对其 `name` 做 `trim()`（加载的旧文件可能带空白，空串/纯空白 → undefined）。
#[test]
fn get_session_name_reads_the_latest_entry_and_trims_loaded_names() {
    let mut session = SessionManager::in_memory("/project", None, None).unwrap();
    let first = session.append_session_info("first").unwrap();
    session.append_session_info("second").unwrap();
    assert_eq!(session.get_session_name().as_deref(), Some("second"));

    // 加载的文件可以携带未 trim 的名字（appendSessionInfo 之外的来源）。
    let entries = parse_session_entries(&format!(
        "{}\n{}\n{}\n",
        entry_json_string(&FileEntry::Header(session_header(
            "sess",
            Some("/project"),
            None
        ))),
        entry_json_string(&FileEntry::Entry(
            session.get_entry(&first).unwrap().clone()
        )),
        r#"{"type":"session_info","id":"later","parentId":"root","timestamp":"2025-01-01T00:00:00Z","name":"  loaded  "}"#,
    ));
    let loaded = SessionManager::in_memory("/project", None, Some(entries)).unwrap();
    assert_eq!(loaded.get_session_name().as_deref(), Some("loaded"));

    let blank = parse_session_entries(&format!(
        "{}\n{}\n",
        entry_json_string(&FileEntry::Header(session_header(
            "sess",
            Some("/project"),
            None
        ))),
        r#"{"type":"session_info","id":"blank","parentId":null,"timestamp":"2025-01-01T00:00:00Z","name":"   "}"#,
    ));
    let loaded = SessionManager::in_memory("/project", None, Some(blank)).unwrap();
    assert_eq!(loaded.get_session_name(), None);
}

/// §9.2.2：`newSession()` 只重置内存状态与 `sessionFile`，**不删除**旧文件。
#[test]
fn new_session_does_not_delete_the_previous_file() {
    let temp = temp_dir("kiana-session-new-session-");
    let session_dir = temp.path().to_string_lossy().into_owned();

    let mut session = SessionManager::create("/project", Some(&session_dir), None).unwrap();
    let first_file = session.get_session_file().unwrap().to_string();
    session.append_message(user_message("hello", 1)).unwrap();
    assert!(std::path::Path::new(&first_file).exists());

    session.new_session(None).unwrap();
    assert!(
        std::path::Path::new(&first_file).exists(),
        "newSession must not delete the old session file"
    );
    let second_file = session.get_session_file().unwrap().to_string();
    assert_ne!(first_file, second_file);
    assert!(
        !std::path::Path::new(&second_file).exists(),
        "a fresh session file is created lazily on the first conversation entry"
    );

    // 显式 id 也走同一条路径，并且 header 使用新 id。
    session
        .new_session(Some(NewSessionOptions {
            id: Some("explicit-id".to_string()),
            parent_session: None,
        }))
        .unwrap();
    assert_eq!(session.get_session_id(), "explicit-id");
    assert!(std::path::Path::new(&first_file).exists());
}
