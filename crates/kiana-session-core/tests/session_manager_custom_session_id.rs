//! TS: `test/session-manager/custom-session-id.test.ts`（13 例）。

mod common;

use common::*;
use kiana_agent_core::AgentMessage;
use kiana_session_core::session_manager::{
    FileEntry, NewSessionOptions, SessionManager, entry_json_string,
};

fn options(id: Option<&str>, parent_session: Option<&str>) -> NewSessionOptions {
    NewSessionOptions {
        id: id.map(str::to_string),
        parent_session: parent_session.map(str::to_string),
    }
}

mod session_manager_new_session_with_custom_id {
    use super::*;

    /// TS: "uses the provided id instead of generating one"
    #[test]
    fn uses_the_provided_id_instead_of_generating_one() {
        let mut session = SessionManager::in_memory(&current_cwd(), None, None).unwrap();
        session
            .new_session(Some(options(Some("my-custom-id"), None)))
            .unwrap();
        assert_eq!(session.get_session_id(), "my-custom-id");
    }

    /// TS: "uses the provided id when creating an in-memory session"
    #[test]
    fn uses_the_provided_id_when_creating_an_in_memory_session() {
        let session = SessionManager::in_memory(
            &current_cwd(),
            Some(options(Some("memory-session-id"), None)),
            None,
        )
        .unwrap();
        assert_eq!(session.get_session_id(), "memory-session-id");
        assert_eq!(
            session.get_header().unwrap().id.as_str(),
            "memory-session-id"
        );
        assert!(session.get_session_file().is_none());
    }

    /// TS: "allows alphanumeric session ids with interior punctuation"
    #[test]
    fn allows_alphanumeric_session_ids_with_interior_punctuation() {
        let mut session = SessionManager::in_memory(&current_cwd(), None, None).unwrap();
        session
            .new_session(Some(options(Some("abc-123_def.456"), None)))
            .unwrap();
        assert_eq!(session.get_session_id(), "abc-123_def.456");
    }

    /// TS: "rejects invalid custom session ids"
    #[test]
    fn rejects_invalid_custom_session_ids() {
        let invalid_ids = [
            "", "-abc", "abc-", "_abc", "abc_", ".abc", "abc.", "abc/def", "abc\\def", "abc def",
        ];
        for id in invalid_ids {
            let mut session = SessionManager::in_memory(&current_cwd(), None, None).unwrap();
            let error = error_text(session.new_session(Some(options(Some(id), None))));
            assert!(
                error.starts_with(
                    "Session id must be non-empty, contain only alphanumeric characters"
                ),
                "unexpected error for {id:?}: {error}"
            );
        }
    }

    /// TS: "generates a UUIDv7 id when no id is provided"
    #[test]
    fn generates_a_uuidv7_id_when_no_id_is_provided() {
        let mut session = SessionManager::in_memory(&current_cwd(), None, None).unwrap();
        session.new_session(None).unwrap();
        let id = session.get_session_id().to_string();
        assert!(!id.is_empty());
        assert!(is_uuid_v7(&id), "not UUIDv7: {id}");
    }

    /// TS: "generates a UUIDv7 id when options is provided without id"
    #[test]
    fn generates_a_uuidv7_id_when_options_is_provided_without_id() {
        let mut session = SessionManager::in_memory(&current_cwd(), None, None).unwrap();
        session
            .new_session(Some(options(None, Some("parent.jsonl"))))
            .unwrap();
        let id = session.get_session_id().to_string();
        assert!(!id.is_empty());
        assert!(is_uuid_v7(&id), "not UUIDv7: {id}");
    }

    /// TS: "includes the custom id in the session header"
    #[test]
    fn includes_the_custom_id_in_the_session_header() {
        let mut session = SessionManager::in_memory(&current_cwd(), None, None).unwrap();
        session
            .new_session(Some(options(Some("header-test-id"), None)))
            .unwrap();
        let header = session.get_header().expect("header");
        assert_eq!(header.id, "header-test-id");
    }

    /// TS: "generates a UUIDv7 id when constructed without an explicit id"
    #[test]
    fn generates_a_uuidv7_id_when_constructed_without_an_explicit_id() {
        let session = SessionManager::in_memory(&current_cwd(), None, None).unwrap();
        assert!(is_uuid_v7(session.get_session_id()));
        assert_eq!(session.get_header().unwrap().id, session.get_session_id());
    }

    /// TS: "uses the provided id when creating a persisted session"
    #[test]
    fn uses_the_provided_id_when_creating_a_persisted_session() {
        let temp = temp_dir("kiana-session-manager-");
        let dir = temp.path().to_string_lossy().into_owned();
        let session = SessionManager::create(
            &dir,
            Some(&dir),
            Some(options(Some("created-session-id"), None)),
        )
        .unwrap();

        assert_eq!(session.get_session_id(), "created-session-id");
        assert_eq!(session.get_header().unwrap().id, "created-session-id");
        let session_file = session
            .get_session_file()
            .expect("session file")
            .to_string();
        assert!(session_file.contains("created-session-id"));
        assert!(is_session_file_name(&session_file, "created-session-id"));
        assert!(!std::path::Path::new(&session_file).exists());
    }

    /// TS: "generates a UUIDv7 id when creating a branched session"
    #[test]
    fn generates_a_uuidv7_id_when_creating_a_branched_session() {
        let mut session = SessionManager::in_memory(&current_cwd(), None, None).unwrap();
        let first_id = session
            .append_message(user_blocks_message("hello", 0))
            .expect("append message");

        session.create_branched_session(&first_id).unwrap();

        assert!(is_uuid_v7(session.get_session_id()));
        assert_eq!(session.get_header().unwrap().id, session.get_session_id());
    }

    /// TS: "generates a UUIDv7 id when forking from another session file"
    #[test]
    fn generates_a_uuidv7_id_when_forking_from_another_session_file() {
        let temp = temp_dir("kiana-session-manager-");
        let dir = temp.path().to_string_lossy().into_owned();
        let source_path = format!("{dir}/source.jsonl");
        let lines = [
            entry_json_string(&FileEntry::Header(session_header(
                "legacy-session-id",
                Some(&dir),
                None,
            ))),
            entry_json_string(&FileEntry::Entry(message_entry(
                "entry-1",
                None,
                "2025-01-01T00:00:00Z",
                assistant_message("hello", 0),
            ))),
        ];
        std::fs::write(&source_path, format!("{}\n", lines.join("\n"))).unwrap();

        let forked = SessionManager::fork_from(&source_path, &dir, Some(&dir), None).unwrap();
        let header = forked.get_header().expect("header");
        assert!(is_uuid_v7(&header.id), "not UUIDv7: {}", header.id);
        assert_eq!(header.parent_session.as_deref(), Some(source_path.as_str()));
    }

    /// TS: "uses the provided id when forking from another session file"
    #[test]
    fn uses_the_provided_id_when_forking_from_another_session_file() {
        let temp = temp_dir("kiana-session-manager-");
        let dir = temp.path().to_string_lossy().into_owned();
        let source_path = format!("{dir}/source.jsonl");
        let header = session_header("source-session-id", Some(&dir), None);
        std::fs::write(
            &source_path,
            format!("{}\n", entry_json_string(&FileEntry::Header(header))),
        )
        .unwrap();

        let forked = SessionManager::fork_from(
            &source_path,
            &dir,
            Some(&dir),
            Some(options(Some("forked-session-id"), None)),
        )
        .unwrap();
        let header = forked.get_header().expect("header");
        assert_eq!(header.id, "forked-session-id");
        assert_eq!(header.parent_session.as_deref(), Some(source_path.as_str()));
        let session_file = forked.get_session_file().expect("session file").to_string();
        assert!(session_file.contains("forked-session-id"));
        assert!(is_session_file_name(&session_file, "forked-session-id"));
    }
}

mod session_manager_fork_from {
    use super::*;

    /// TS: "copies all entries unchanged into the forked session"
    #[test]
    fn copies_all_entries_unchanged_into_the_forked_session() {
        let temp = temp_dir("kiana-session-manager-");
        let dir = temp.path().to_string_lossy().into_owned();
        let source_path = format!("{dir}/source.jsonl");
        let mut lines: Vec<String> = vec![entry_json_string(&FileEntry::Header(session_header(
            "source-id",
            Some(&dir),
            None,
        )))];
        let mut parent_id: Option<String> = None;
        for index in 0..50 {
            let id = format!("e{index}");
            let message: AgentMessage = user_message(&format!("message {index}"), 0);
            lines.push(entry_json_string(&FileEntry::Entry(message_entry(
                &id,
                parent_id.as_deref(),
                "2025-01-01T00:00:00Z",
                message,
            ))));
            parent_id = Some(id);
        }
        std::fs::write(&source_path, format!("{}\n", lines.join("\n"))).unwrap();

        let forked =
            SessionManager::fork_from(&source_path, &dir, Some(&format!("{dir}/sessions")), None)
                .unwrap();

        let written = std::fs::read_to_string(forked.get_session_file().unwrap()).unwrap();
        let written_lines: Vec<&str> = written.trim_end().split('\n').collect();
        assert_eq!(written_lines.len(), 51);
        let header: serde_json::Value = serde_json::from_str(written_lines[0]).unwrap();
        assert_eq!(
            header.get("parentSession").and_then(|v| v.as_str()),
            Some(source_path.as_str())
        );
        assert_eq!(written_lines[1..].join("\n"), lines[1..].join("\n"));
        assert_eq!(forked.get_entries().len(), 50);
    }
}
