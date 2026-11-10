//! TS: `test/session-manager/load-entries.test.ts`（13 例）。

mod common;

use common::*;
use kiana_agent_core::AgentMessage;
use kiana_session_core::session_manager::{
    FileEntry, NewSessionOptions, SessionEntry, SessionManager, parse_session_entries,
};

fn user_message(text: &str) -> AgentMessage {
    user_blocks_message(text, 0)
}

fn stored_entries(build: impl FnOnce(&mut SessionManager)) -> Vec<SessionEntry> {
    let mut source = SessionManager::in_memory("/project", None, None).unwrap();
    build(&mut source);
    source.get_entries()
}

fn body(entries: Vec<SessionEntry>) -> Vec<FileEntry> {
    entries.into_iter().map(FileEntry::Entry).collect()
}

fn options(id: Option<&str>) -> NewSessionOptions {
    NewSessionOptions {
        id: id.map(str::to_string),
        parent_session: None,
    }
}

mod session_manager_in_memory_with_preloaded_entries {
    use super::*;

    /// TS: "adopts entries verbatim"
    #[test]
    fn adopts_entries_verbatim() {
        let entries = stored_entries(|source| {
            source.append_message(user_message("hello")).unwrap();
            source
                .append_model_change("anthropic", "claude-opus-4-5")
                .unwrap();
            source.append_message(user_message("again")).unwrap();
        });

        let session =
            SessionManager::in_memory("/project", None, Some(body(entries.clone()))).unwrap();

        assert_eq!(session.get_entries(), entries);
    }

    /// TS: "keeps the loaded leaf so appends continue the conversation"
    #[test]
    fn keeps_the_loaded_leaf_so_appends_continue_the_conversation() {
        let entries = stored_entries(|source| {
            source.append_message(user_message("hello")).unwrap();
            source.append_message(user_message("again")).unwrap();
        });
        let last_id = entries.last().unwrap().id().to_string();

        let mut session = SessionManager::in_memory("/project", None, Some(body(entries))).unwrap();
        let appended_id = session.append_message(user_message("continued")).unwrap();

        assert_eq!(session.get_leaf_id(), Some(appended_id.as_str()));
        assert_eq!(
            session.get_entry(&appended_id).unwrap().parent_id(),
            Some(last_id.as_str())
        );
    }

    /// TS: "never mints an id that collides with a loaded entry"
    #[test]
    fn never_mints_an_id_that_collides_with_a_loaded_entry() {
        let entries = stored_entries(|source| {
            for index in 0..50 {
                source
                    .append_message(user_message(&format!("message {index}")))
                    .unwrap();
            }
        });

        let mut session =
            SessionManager::in_memory("/project", None, Some(body(entries.clone()))).unwrap();
        let appended_id = session.append_message(user_message("continued")).unwrap();

        assert!(!entries.iter().any(|entry| entry.id() == appended_id));
    }

    /// TS: "rebuilds the branch structure rather than a flat chain"
    #[test]
    fn rebuilds_the_branch_structure_rather_than_a_flat_chain() {
        let entries = stored_entries(|source| {
            let first_id = source.append_message(user_message("hello")).unwrap();
            source.append_message(user_message("abandoned")).unwrap();
            source.branch(&first_id).unwrap();
            source.append_message(user_message("kept")).unwrap();
        });

        let session = SessionManager::in_memory("/project", None, Some(body(entries))).unwrap();
        let roots = session.get_tree();

        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].children.len(), 2);
    }

    /// TS: "rebuilds labels"
    #[test]
    fn rebuilds_labels() {
        let mut labelled_id = String::new();
        let entries = stored_entries(|source| {
            labelled_id = source.append_message(user_message("hello")).unwrap();
            source
                .append_label_change(&labelled_id, Some("checkpoint"))
                .unwrap();
        });

        let session = SessionManager::in_memory("/project", None, Some(body(entries))).unwrap();

        assert_eq!(session.get_label(&labelled_id), Some("checkpoint"));
    }

    /// TS: "resolves a compaction against the entry it was written against"
    #[test]
    fn resolves_a_compaction_against_the_entry_it_was_written_against() {
        let mut kept_id = String::new();
        let entries = stored_entries(|source| {
            source.append_message(user_message("dropped")).unwrap();
            kept_id = source.append_message(user_message("kept")).unwrap();
            source
                .append_compaction("summary so far", Some(&kept_id), 1000.0, None, None, None)
                .unwrap();
        });

        let session = SessionManager::in_memory("/project", None, Some(body(entries))).unwrap();
        let context = session.build_context_entries();

        assert!(context.iter().any(|entry| entry.id() == kept_id));
    }

    /// TS: "creates a header from the options when the entries carry none"
    #[test]
    fn creates_a_header_from_the_options_when_the_entries_carry_none() {
        let entries = stored_entries(|source| {
            source.append_message(user_message("hello")).unwrap();
        });

        let session = SessionManager::in_memory(
            "/project",
            Some(options(Some("restored-session"))),
            Some(body(entries)),
        )
        .unwrap();

        assert_eq!(session.get_session_id(), "restored-session");
        assert_eq!(session.get_header().unwrap().id, "restored-session");
        assert_eq!(
            session.get_header().unwrap().cwd.as_deref(),
            Some("/project")
        );
    }

    /// TS: "generates a session id when the options carry none"
    #[test]
    fn generates_a_session_id_when_the_options_carry_none() {
        let entries = stored_entries(|source| {
            source.append_message(user_message("hello")).unwrap();
        });

        let session = SessionManager::in_memory("/project", None, Some(body(entries))).unwrap();

        assert!(is_uuid_v7(session.get_session_id()));
        assert_eq!(session.get_header().unwrap().id, session.get_session_id());
    }

    /// TS: "stays off the filesystem"
    #[test]
    fn stays_off_the_filesystem() {
        let entries = stored_entries(|source| {
            source.append_message(user_message("hello")).unwrap();
        });

        let mut session = SessionManager::in_memory("/project", None, Some(body(entries))).unwrap();
        session.append_message(user_message("continued")).unwrap();

        assert!(session.get_session_file().is_none());
        assert!(!session.is_persisted());
    }

    /// TS: "starts an empty session when the entries are empty"
    #[test]
    fn starts_an_empty_session_when_the_entries_are_empty() {
        let session =
            SessionManager::in_memory("/project", Some(options(Some("empty-session"))), None)
                .unwrap();

        assert_eq!(session.get_session_id(), "empty-session");
        assert!(session.get_entries().is_empty());
        assert!(session.get_leaf_id().is_none());
    }

    /// TS: "takes the session identity from a header among the entries"
    #[test]
    fn takes_the_session_identity_from_a_header_among_the_entries() {
        let body_entries = stored_entries(|source| {
            source.append_message(user_message("hello")).unwrap();
        });
        let mut entries = vec![FileEntry::Header(session_header(
            "stored-session",
            Some("/stored"),
            None,
        ))];
        entries.extend(body(body_entries));

        let session =
            SessionManager::in_memory("/project", Some(options(Some("ignored"))), Some(entries))
                .unwrap();

        assert_eq!(session.get_session_id(), "stored-session");
        assert_eq!(
            session.get_header().unwrap().cwd.as_deref(),
            Some("/stored")
        );
    }

    /// TS: "migrates entries restored with an older header"
    #[test]
    fn migrates_entries_restored_with_an_older_header() {
        let hook_message: AgentMessage = serde_json::from_value(
            serde_json::json!({"role": "hookMessage", "content": "from a hook", "timestamp": 1}),
        )
        .unwrap();
        let mut header = session_header("v2-session", Some("/project"), None);
        header.version = Some(2.0);
        let entries = vec![
            FileEntry::Header(header),
            FileEntry::Entry(message_entry(
                "abc12345",
                None,
                "2026-01-01T00:00:01Z",
                hook_message,
            )),
        ];

        let session = SessionManager::in_memory("/project", None, Some(entries)).unwrap();
        let restored = match &session.get_entries()[0] {
            SessionEntry::Message(entry) => entry.clone(),
            other => panic!("expected message entry, got {other:?}"),
        };

        assert_eq!(session.get_header().unwrap().version, Some(3.0));
        assert_eq!(restored.message.role(), "custom");
        assert_eq!(restored.id, "abc12345");
    }

    /// TS: "adopts headerless entries as current-version without migrating them"
    #[test]
    fn adopts_headerless_entries_as_current_version_without_migrating_them() {
        let hook_message: AgentMessage = serde_json::from_value(
            serde_json::json!({"role": "hookMessage", "content": "from a hook", "timestamp": 1}),
        )
        .unwrap();
        let entries = body(vec![message_entry(
            "abc12345",
            None,
            "2026-01-01T00:00:01Z",
            hook_message,
        )]);

        let session = SessionManager::in_memory("/project", None, Some(entries)).unwrap();
        let restored = match &session.get_entries()[0] {
            SessionEntry::Message(entry) => entry.clone(),
            other => panic!("expected message entry, got {other:?}"),
        };

        assert_eq!(restored.message.role(), "hookMessage");
    }
}

/// 回归（评审 #7）：TS 的 `parseSessionEntries` 保留任何解析成功的值（含 `null`/数组/
/// 数字）；`loadEntriesFromFile` 对首行不是 header 对象的文件返回空数组且不修改文件。
#[test]
fn parse_session_entries_keeps_non_object_values() {
    let entries = parse_session_entries("null\n[]\n42\n\"text\"\ntrue");
    assert_eq!(entries.len(), 5);
    assert!(matches!(
        &entries[0],
        FileEntry::Raw(serde_json::Value::Null)
    ));
    assert!(matches!(
        &entries[1],
        FileEntry::Raw(serde_json::Value::Array(_))
    ));
    assert!(matches!(
        &entries[2],
        FileEntry::Raw(serde_json::Value::Number(_))
    ));
    assert!(matches!(
        &entries[3],
        FileEntry::Raw(serde_json::Value::String(_))
    ));
    assert!(matches!(
        &entries[4],
        FileEntry::Raw(serde_json::Value::Bool(true))
    ));
}

/// 回归（评审 #7）：`[]` 作为首个可解析行时整个文件不是 kiana 会话。
#[test]
fn load_entries_from_file_rejects_a_non_object_first_line() {
    let temp = temp_dir("kiana-session-nonobject-");
    let path = format!("{}/session.jsonl", temp.path().to_string_lossy());
    let content = "[]\n{\"type\":\"session\",\"id\":\"x\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":\"/tmp\"}\n";
    std::fs::write(&path, content).unwrap();

    let entries = kiana_session_core::session_manager::load_entries_from_file(&path).unwrap();
    assert!(entries.is_empty());
    // 不补写末尾换行，也不改动内容。
    assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
}
