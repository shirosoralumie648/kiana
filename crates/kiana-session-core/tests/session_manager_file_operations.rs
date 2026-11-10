//! TS: `test/session-manager/file-operations.test.ts`（35 例，含 `it.each` 的 3 个参数化用例）。

mod common;

use std::io::{Seek, SeekFrom, Write};
use std::sync::Arc;
use std::time::Duration;

use common::*;
use kiana_ai::utils::abort::AbortScope;
use kiana_session_core::session_manager::{
    FileEntry, SessionInfo, SessionListError, SessionManager, entry_json_string,
    find_most_recent_session, load_entries_from_file,
};

const HEADER_SCAN_LIMIT_BYTES: usize = 1024 * 1024;
/// Node 的 `buffer.constants.MAX_STRING_LENGTH`（64 位 V8：2^29 - 24）。
const NODE_MAX_STRING_LENGTH: u64 = 536_870_888;

fn json_line<T: serde::Serialize>(value: &T) -> String {
    format!("{}\n", entry_json_string(value))
}

fn write_session_header(file: &str, cwd: &str, id: &str, prefix: &str) {
    let header = session_header(id, Some(cwd), None);
    std::fs::write(
        file,
        format!("{prefix}{}", json_line(&FileEntry::Header(header))),
    )
    .unwrap();
}

fn temp_path(prefix: &str) -> (tempfile::TempDir, String) {
    let temp = temp_dir(prefix);
    let dir = temp.path().to_string_lossy().into_owned();
    std::fs::create_dir_all(&dir).unwrap();
    (temp, dir)
}

fn wait_for_distinct_mtime() {
    std::thread::sleep(Duration::from_millis(10));
}

fn paths(infos: &[SessionInfo]) -> Vec<String> {
    infos.iter().map(|info| info.path.clone()).collect()
}

mod load_entries_from_file {
    use super::*;

    fn file_with(dir: &str, name: &str, content: &str) -> String {
        let file = format!("{dir}/{name}");
        std::fs::write(&file, content).unwrap();
        file
    }

    /// TS: "returns empty array for non-existent file"
    #[test]
    fn returns_empty_array_for_non_existent_file() {
        let (_temp, dir) = temp_path("session-test-");
        let entries = load_entries_from_file(&format!("{dir}/nonexistent.jsonl")).unwrap();
        assert!(entries.is_empty());
    }

    /// TS: "returns empty array for empty file"
    #[test]
    fn returns_empty_array_for_empty_file() {
        let (_temp, dir) = temp_path("session-test-");
        let file = file_with(&dir, "empty.jsonl", "");
        assert!(load_entries_from_file(&file).unwrap().is_empty());
    }

    /// TS: "returns empty array for file without valid session header"
    #[test]
    fn returns_empty_array_for_file_without_valid_session_header() {
        let (_temp, dir) = temp_path("session-test-");
        let file = file_with(
            &dir,
            "no-header.jsonl",
            "{\"type\":\"message\",\"id\":\"1\"}\n",
        );
        assert!(load_entries_from_file(&file).unwrap().is_empty());
    }

    /// TS: "returns empty array for malformed JSON"
    #[test]
    fn returns_empty_array_for_malformed_json() {
        let (_temp, dir) = temp_path("session-test-");
        let file = file_with(&dir, "malformed.jsonl", "not json\n");
        assert!(load_entries_from_file(&file).unwrap().is_empty());
    }

    /// TS: "loads valid session file"
    #[test]
    fn loads_valid_session_file() {
        let (_temp, dir) = temp_path("session-test-");
        let file = file_with(
            &dir,
            "valid.jsonl",
            "{\"type\":\"session\",\"id\":\"abc\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n\
             {\"type\":\"message\",\"id\":\"1\",\"parentId\":null,\"timestamp\":\"2025-01-01T00:00:01Z\",\"message\":{\"role\":\"user\",\"content\":\"hi\",\"timestamp\":1}}\n",
        );
        let entries = load_entries_from_file(&file).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].kind(), "session");
        assert_eq!(entries[1].kind(), "message");
    }

    /// TS: "skips malformed lines but keeps valid ones"
    #[test]
    fn skips_malformed_lines_but_keeps_valid_ones() {
        let (_temp, dir) = temp_path("session-test-");
        let file = file_with(
            &dir,
            "mixed.jsonl",
            "{\"type\":\"session\",\"id\":\"abc\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n\
             not valid json\n\
             {\"type\":\"message\",\"id\":\"1\",\"parentId\":null,\"timestamp\":\"2025-01-01T00:00:01Z\",\"message\":{\"role\":\"user\",\"content\":\"hi\",\"timestamp\":1}}\n",
        );
        assert_eq!(load_entries_from_file(&file).unwrap().len(), 2);
    }

    /// TS: "adds a newline after an unterminated valid record"
    #[test]
    fn adds_a_newline_after_an_unterminated_valid_record() {
        let (_temp, dir) = temp_path("session-test-");
        let content = "{\"type\":\"session\",\"id\":\"abc\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n\
                       {\"type\":\"message\",\"id\":\"1\",\"parentId\":null,\"timestamp\":\"2025-01-01T00:00:01Z\",\"message\":{\"role\":\"user\",\"content\":\"hi\",\"timestamp\":1}}";
        let file = file_with(&dir, "unterminated.jsonl", content);

        assert_eq!(load_entries_from_file(&file).unwrap().len(), 2);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            format!("{content}\n")
        );
    }

    /// TS: "adds a newline after an unterminated malformed final fragment"
    #[test]
    fn adds_a_newline_after_an_unterminated_malformed_final_fragment() {
        let (_temp, dir) = temp_path("session-test-");
        let content = "{\"type\":\"session\",\"id\":\"abc\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n\
                       {\"type\":\"message\"";
        let file = file_with(&dir, "malformed-tail.jsonl", content);

        assert_eq!(load_entries_from_file(&file).unwrap().len(), 1);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            format!("{content}\n")
        );
    }

    /// TS: "does not modify an unterminated non-session file"
    #[test]
    fn does_not_modify_an_unterminated_non_session_file() {
        let (_temp, dir) = temp_path("session-test-");
        let content = "{\"type\":\"message\",\"id\":\"1\"}";
        let file = file_with(&dir, "invalid.jsonl", content);

        assert!(load_entries_from_file(&file).unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), content);
    }

    // TS: `it.each([...])("reads cwd from a session with %s", ...)`。
    /// TS: "reads cwd from a session with leading blank lines"
    #[test]
    fn reads_cwd_from_a_session_with_leading_blank_lines() {
        let case = ("leading-blank", "\n  \n");
        let (_temp, dir) = temp_path("session-test-");
        let stored_cwd = format!("{dir}/stored-project");
        let file = format!("{dir}/header.jsonl");
        write_session_header(&file, &stored_cwd, case.0, case.1);

        let session = SessionManager::open(&file, Some(&dir), None).unwrap();
        assert_eq!(session.get_session_id(), case.0);
        assert_eq!(session.get_cwd(), stored_cwd);
    }

    /// TS: "reads cwd from a session with leading malformed lines"
    #[test]
    fn reads_cwd_from_a_session_with_leading_malformed_lines() {
        let case = ("leading-malformed", "not json\n{broken json\n");
        let (_temp, dir) = temp_path("session-test-");
        let stored_cwd = format!("{dir}/stored-project");
        let file = format!("{dir}/header.jsonl");
        write_session_header(&file, &stored_cwd, case.0, case.1);

        let session = SessionManager::open(&file, Some(&dir), None).unwrap();
        assert_eq!(session.get_session_id(), case.0);
        assert_eq!(session.get_cwd(), stored_cwd);
    }

    /// TS: "reads cwd from a session with a multi-buffer header"
    #[test]
    fn reads_cwd_from_a_session_with_a_multi_buffer_header() {
        let session_id = "a".repeat(8192);
        let (_temp, dir) = temp_path("session-test-");
        let stored_cwd = format!("{dir}/stored-project");
        let file = format!("{dir}/header.jsonl");
        write_session_header(&file, &stored_cwd, &session_id, "");

        let session = SessionManager::open(&file, Some(&dir), None).unwrap();
        assert_eq!(session.get_session_id(), session_id);
        assert_eq!(session.get_cwd(), stored_cwd);
    }

    /// TS: "opens compatible sessions beyond the discovery scan limit"
    #[test]
    fn opens_compatible_sessions_beyond_the_discovery_scan_limit() {
        let (_temp, dir) = temp_path("session-test-");
        let stored_cwd = format!("{dir}/stored-project");
        let override_cwd = format!("{dir}/override-project");
        let cases = [
            (
                "large-header",
                "a".repeat(HEADER_SCAN_LIMIT_BYTES + 1),
                String::new(),
            ),
            (
                "large-prefix",
                "large-prefix".to_string(),
                format!("{}\n", "x".repeat(HEADER_SCAN_LIMIT_BYTES + 1)),
            ),
        ];

        for (name, id, prefix) in cases {
            let file = format!("{dir}/{name}.jsonl");
            write_session_header(&file, &stored_cwd, &id, &prefix);
            for cwd_override in [None, Some(override_cwd.as_str())] {
                let session = SessionManager::open(&file, Some(&dir), cwd_override).unwrap();
                assert_eq!(session.get_session_id(), id);
                assert_eq!(session.get_cwd(), cwd_override.unwrap_or(&stored_cwd));
            }
        }
    }

    /// TS: "opens session files larger than Node's max string length"
    #[test]
    fn opens_session_files_larger_than_nodes_max_string_length() {
        let (_temp, dir) = temp_path("session-test-");
        let file = format!("{dir}/large.jsonl");
        std::fs::write(
            &file,
            "{\"type\":\"session\",\"version\":3,\"id\":\"abc\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n",
        )
        .unwrap();

        {
            let mut handle = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&file)
                .unwrap();
            let stride = 16u64 * 1024 * 1024;
            let mut offset = stride;
            while offset <= NODE_MAX_STRING_LENGTH + stride {
                handle.seek(SeekFrom::Start(offset)).unwrap();
                handle.write_all(b"\n").unwrap();
                offset += stride;
            }
        }

        {
            let mut handle = std::fs::OpenOptions::new()
                .append(true)
                .open(&file)
                .unwrap();
            handle
                .write_all(
                    b"{\"type\":\"message\",\"id\":\"1\",\"parentId\":null,\"timestamp\":\"2025-01-01T00:00:01Z\",\"message\":{\"role\":\"user\",\"content\":\"hi\",\"timestamp\":1}}\n",
                )
                .unwrap();
        }

        let session = SessionManager::open(&file, Some(&dir), None).unwrap();
        assert_eq!(session.get_session_id(), "abc");
        assert_eq!(session.get_entries().len(), 1);
        let context = session.build_session_context();
        assert_eq!(context.messages.len(), 1);
        assert_eq!(context.messages[0].role(), "user");
        let value = message_json(&context.messages[0]);
        assert_eq!(
            value.get("content").and_then(serde_json::Value::as_str),
            Some("hi")
        );
        assert_eq!(
            value.get("timestamp").and_then(serde_json::Value::as_i64),
            Some(1)
        );
    }
}

mod find_most_recent_session {
    use super::*;

    /// TS: "returns null for empty directory"
    #[test]
    fn returns_null_for_empty_directory() {
        let (_temp, dir) = temp_path("session-test-");
        assert!(find_most_recent_session(&dir, None).is_none());
    }

    /// TS: "returns null for non-existent directory"
    #[test]
    fn returns_null_for_non_existent_directory() {
        let (_temp, dir) = temp_path("session-test-");
        assert!(find_most_recent_session(&format!("{dir}/nonexistent"), None).is_none());
    }

    /// TS: "ignores non-jsonl files"
    #[test]
    fn ignores_non_jsonl_files() {
        let (_temp, dir) = temp_path("session-test-");
        std::fs::write(format!("{dir}/file.txt"), "hello").unwrap();
        std::fs::write(format!("{dir}/file.json"), "{}").unwrap();
        assert!(find_most_recent_session(&dir, None).is_none());
    }

    /// TS: "ignores jsonl files without valid session header"
    #[test]
    fn ignores_jsonl_files_without_valid_session_header() {
        let (_temp, dir) = temp_path("session-test-");
        std::fs::write(format!("{dir}/invalid.jsonl"), "{\"type\":\"message\"}\n").unwrap();
        assert!(find_most_recent_session(&dir, None).is_none());
    }

    /// TS: "returns single valid session file"
    #[test]
    fn returns_single_valid_session_file() {
        let (_temp, dir) = temp_path("session-test-");
        let file = format!("{dir}/session.jsonl");
        write_session_header(&file, "/tmp", "abc", "");
        assert_eq!(find_most_recent_session(&dir, None), Some(file));
    }

    /// TS: "returns most recently modified session"
    #[test]
    fn returns_most_recently_modified_session() {
        let (_temp, dir) = temp_path("session-test-");
        let file1 = format!("{dir}/older.jsonl");
        let file2 = format!("{dir}/newer.jsonl");

        write_session_header(&file1, "/tmp", "old", "");
        wait_for_distinct_mtime();
        write_session_header(&file2, "/tmp", "new", "");

        assert_eq!(find_most_recent_session(&dir, None), Some(file2));
    }

    /// TS: "skips invalid files and returns valid one"
    #[test]
    fn skips_invalid_files_and_returns_valid_one() {
        let (_temp, dir) = temp_path("session-test-");
        let invalid = format!("{dir}/invalid.jsonl");
        let valid = format!("{dir}/valid.jsonl");

        std::fs::write(&invalid, "{\"type\":\"not-session\"}\n").unwrap();
        wait_for_distinct_mtime();
        write_session_header(&valid, "/tmp", "abc", "");

        assert_eq!(find_most_recent_session(&dir, None), Some(valid));
    }

    /// TS: "skips oversized corrupt files and returns a valid session"
    #[test]
    fn skips_oversized_corrupt_files_and_returns_a_valid_session() {
        let (_temp, dir) = temp_path("session-test-");
        let invalid = format!("{dir}/oversized.jsonl");
        let valid = format!("{dir}/valid.jsonl");
        std::fs::write(&invalid, "x".repeat(HEADER_SCAN_LIMIT_BYTES + 1)).unwrap();
        write_session_header(&valid, "/tmp", "abc", "");

        assert_eq!(find_most_recent_session(&dir, None), Some(valid));
    }

    /// TS: "filters most recent session by cwd"
    #[test]
    fn filters_most_recent_session_by_cwd() {
        let (_temp, dir) = temp_path("session-test-");
        let project_a = format!("{dir}/project-a");
        let project_b = format!("{dir}/project-b");
        let file_a = format!("{dir}/a.jsonl");
        let file_b = format!("{dir}/b.jsonl");

        write_session_header(&file_a, &project_a, "a", "");
        wait_for_distinct_mtime();
        write_session_header(&file_b, &project_b, "b", "");

        assert_eq!(
            find_most_recent_session(&dir, Some(&project_a)),
            Some(file_a)
        );
        assert_eq!(
            find_most_recent_session(&dir, Some(&project_b)),
            Some(file_b)
        );
    }
}

mod session_manager_custom_flat_session_directory {
    use super::*;

    struct FlatFixture {
        _temp: tempfile::TempDir,
        dir: String,
        project_a: String,
        project_b: String,
    }

    fn setup() -> FlatFixture {
        let (_temp, dir) = temp_path("session-test-");
        let project_a = format!("{dir}/project-a");
        let project_b = format!("{dir}/project-b");
        std::fs::create_dir_all(&project_a).unwrap();
        std::fs::create_dir_all(&project_b).unwrap();
        FlatFixture {
            _temp,
            dir,
            project_a,
            project_b,
        }
    }

    fn create_persisted_session(dir: &str, cwd: &str, label: &str) -> String {
        let mut session = SessionManager::create(cwd, Some(dir), None).unwrap();
        session.append_message(user_message(label, 0)).unwrap();
        session
            .append_message(assistant_message(&format!("reply to {label}"), 0))
            .unwrap();
        session
            .get_session_file()
            .expect("expected persisted session file")
            .to_string()
    }

    /// TS: "scopes current-folder APIs by cwd while listing all flat sessions"
    #[tokio::test]
    async fn scopes_current_folder_apis_by_cwd_while_listing_all_flat_sessions() {
        let fixture = setup();
        let session_a = create_persisted_session(&fixture.dir, &fixture.project_a, "from A");
        wait_for_distinct_mtime();
        let session_b = create_persisted_session(&fixture.dir, &fixture.project_b, "from B");

        let current_a = SessionManager::list(&fixture.project_a, Some(&fixture.dir), None, None)
            .await
            .unwrap();
        assert_eq!(paths(&current_a), [session_a.as_str()]);

        let all = SessionManager::list_all_in(&fixture.dir, None, None)
            .await
            .unwrap();
        let mut all_paths = paths(&all);
        all_paths.sort();
        let mut expected = vec![session_a.clone(), session_b];
        expected.sort();
        assert_eq!(all_paths, expected);

        let continued_a =
            SessionManager::continue_recent(&fixture.project_a, Some(&fixture.dir)).unwrap();
        assert_eq!(continued_a.get_session_file(), Some(session_a.as_str()));
    }

    /// TS: "rejects a cancelled session listing"
    #[tokio::test]
    async fn rejects_a_cancelled_session_listing() {
        let fixture = setup();
        create_persisted_session(&fixture.dir, &fixture.project_a, "from A");
        create_persisted_session(&fixture.dir, &fixture.project_b, "from B");

        let controller = Arc::new(AbortScope::new());
        let abort_controller = Arc::clone(&controller);
        let progress: kiana_session_core::session_manager::SessionListProgress =
            Arc::new(move |_loaded, _total, partial_sessions| {
                if partial_sessions.is_some() {
                    abort_controller.abort();
                }
            });
        let listing =
            SessionManager::list_all_in(&fixture.dir, Some(progress), Some(&controller)).await;
        let error: SessionListError = listing.expect_err("cancelled listing must fail");
        assert_eq!(error.name, "AbortError");

        let second: Result<Vec<SessionInfo>, SessionListError> =
            SessionManager::list_all(None, Some(&controller)).await;
        assert_eq!(
            second.expect_err("aborted listing must fail").name,
            "AbortError"
        );
    }
}

mod session_manager_set_session_file_with_corrupted_files {
    use super::*;

    /// TS: "truncates and rewrites empty file with valid header"
    #[test]
    fn truncates_and_rewrites_empty_file_with_valid_header() {
        let (_temp, dir) = temp_path("session-test-");
        let empty_file = format!("{dir}/empty.jsonl");
        std::fs::write(&empty_file, "").unwrap();

        let session = SessionManager::open(&empty_file, Some(&dir), None).unwrap();

        // 应创建带有效 header 的新会话。
        assert!(!session.get_session_id().is_empty());
        let header = session.get_header().expect("header");
        assert_eq!(
            header.kind,
            kiana_session_core::session_manager::SessionTypeMarker::Marker
        );

        // 文件现在应只含一个有效 header。
        let content = std::fs::read_to_string(&empty_file).unwrap();
        let lines: Vec<&str> = content
            .trim()
            .split('\n')
            .filter(|line| !line.is_empty())
            .collect();
        assert_eq!(lines.len(), 1);
        let header: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(
            header.get("type").and_then(serde_json::Value::as_str),
            Some("session")
        );
        assert_eq!(
            header.get("id").and_then(serde_json::Value::as_str),
            Some(session.get_session_id())
        );
    }

    /// TS: "throws and preserves non-empty file without valid header"
    #[test]
    fn throws_and_preserves_non_empty_file_without_valid_header() {
        let (_temp, dir) = temp_path("session-test-");
        let no_header_file = format!("{dir}/no-header.jsonl");
        let original_content = "{\"type\":\"message\",\"id\":\"abc\",\"parentId\":\"orphaned\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"message\":{\"role\":\"assistant\",\"content\":\"test\"}}\n";
        std::fs::write(&no_header_file, original_content).unwrap();

        let error = error_text(SessionManager::open(&no_header_file, Some(&dir), None));
        assert_eq!(
            error,
            format!("Session file is not a valid kiana session: {no_header_file}")
        );
        assert_eq!(
            std::fs::read_to_string(&no_header_file).unwrap(),
            original_content
        );
    }

    /// TS: "throws and preserves non-session JSONL files"
    #[test]
    fn throws_and_preserves_non_session_jsonl_files() {
        let (_temp, dir) = temp_path("session-test-");
        let non_session_file = format!("{dir}/not-a-session.log");
        let original_content = "{\"type\":\"event\",\"data\":\"not a session\"}\n";
        std::fs::write(&non_session_file, original_content).unwrap();

        let error = error_text(SessionManager::open(&non_session_file, Some(&dir), None));
        assert_eq!(
            error,
            format!("Session file is not a valid kiana session: {non_session_file}")
        );
        assert_eq!(
            std::fs::read_to_string(&non_session_file).unwrap(),
            original_content
        );
    }

    /// TS: "preserves explicit session file path when recovering from corrupted file"
    #[test]
    fn preserves_explicit_session_file_path_when_recovering_from_corrupted_file() {
        let (_temp, dir) = temp_path("session-test-");
        let explicit_path = format!("{dir}/my-session.jsonl");
        std::fs::write(&explicit_path, "").unwrap();

        let session = SessionManager::open(&explicit_path, Some(&dir), None).unwrap();

        assert_eq!(session.get_session_file(), Some(explicit_path.as_str()));
    }

    /// TS: "subsequent loads of initialized empty file work correctly"
    #[test]
    fn subsequent_loads_of_initialized_empty_file_work_correctly() {
        let (_temp, dir) = temp_path("session-test-");
        let empty_file = format!("{dir}/empty.jsonl");
        std::fs::write(&empty_file, "").unwrap();

        let session1 = SessionManager::open(&empty_file, Some(&dir), None).unwrap();
        let session_id = session1.get_session_id().to_string();

        let session2 = SessionManager::open(&empty_file, Some(&dir), None).unwrap();
        assert_eq!(session2.get_session_id(), session_id);
        assert!(session2.get_header().is_some());
    }
}

mod session_manager_session_file_creation {
    use super::*;

    /// TS: "does not create a file for a session with only setup entries"
    #[test]
    fn does_not_create_a_file_for_a_session_with_only_setup_entries() {
        let (temp, dir) = temp_path("kiana-session-persist-");
        let _keep = temp;
        let mut session = SessionManager::create(&dir, Some(&dir), None).unwrap();
        session
            .append_model_change("anthropic", "claude-sonnet-4-5")
            .unwrap();
        session.append_thinking_level_change("off").unwrap();

        assert!(!std::path::Path::new(session.get_session_file().unwrap()).exists());
    }

    // TS #10000：首个 prompt 必须在首轮没有 assistant 消息时也能存活。
    /// TS: "creates the file when the first user message is appended"
    #[test]
    fn creates_the_file_when_the_first_user_message_is_appended() {
        let (_temp, dir) = temp_path("kiana-session-persist-");
        let mut session = SessionManager::create(&dir, Some(&dir), None).unwrap();
        session
            .append_model_change("anthropic", "claude-sonnet-4-5")
            .unwrap();
        session
            .append_message(user_message("first question", 0))
            .unwrap();

        let file = session.get_session_file().unwrap().to_string();
        assert_eq!(
            read_session_file_roles(&file),
            ["session", "model_change", "user"]
        );
        let reopened = SessionManager::open(&file, Some(&dir), None).unwrap();
        assert_eq!(reopened.build_session_context().messages.len(), 1);
    }

    /// TS: "appends later entries to the file without rewriting earlier ones"
    #[test]
    fn appends_later_entries_to_the_file_without_rewriting_earlier_ones() {
        let (_temp, dir) = temp_path("kiana-session-persist-");
        let mut session = SessionManager::create(&dir, Some(&dir), None).unwrap();
        session
            .append_message(user_message("first question", 0))
            .unwrap();
        session
            .append_custom_entry("preset-state", Some(serde_json::json!({"name": "plan"})))
            .unwrap();
        session
            .append_message(assistant_message("first answer", 0))
            .unwrap();

        assert_eq!(
            read_session_file_roles(session.get_session_file().unwrap()),
            ["session", "user", "custom", "assistant"]
        );
    }
}

mod session_manager_list_directory_errors {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// `KIANA_CODING_AGENT_DIR` 是进程级全局状态；本二进制的用例并行执行，切换时串行化。
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    struct AgentDirGuard(String);

    impl AgentDirGuard {
        fn set(path: &str) -> Self {
            let previous = std::env::var(kiana_core::config::ENV_AGENT_DIR).unwrap_or_default();
            // SAFETY: 由 `env_lock` 串行化，且运行期没有其它线程读取该变量。
            unsafe {
                std::env::set_var(kiana_core::config::ENV_AGENT_DIR, path);
            }
            AgentDirGuard(previous)
        }
    }

    impl Drop for AgentDirGuard {
        fn drop(&mut self) {
            // SAFETY: 与 `set` 相同。
            unsafe {
                if self.0.is_empty() {
                    std::env::remove_var(kiana_core::config::ENV_AGENT_DIR);
                } else {
                    std::env::set_var(kiana_core::config::ENV_AGENT_DIR, &self.0);
                }
            }
        }
    }

    /// 回归（评审 #8）：TS 在 try/catch 之外解析/创建 `sessionDir`；默认会话目录的
    /// `mkdirSync` 失败会让 `list()` 的 promise 拒绝，而不是返回空列表。
    // 理由：`KIANA_CODING_AGENT_DIR` 必须在整个 `list().await` 期间保持设置，而串行化用的
    // `std::sync::Mutex` 守卫因此跨 await；`#[tokio::test]` 的 current-thread 运行时下没有
    // 其它任务会尝试拿这把锁（不会死锁）。
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn list_propagates_default_session_directory_creation_errors() {
        let _guard = env_lock();
        let temp = temp_dir("kiana-session-list-error-");
        let blocker = format!("{}/blocker", temp.path().to_string_lossy());
        std::fs::write(&blocker, "not a directory").unwrap();
        let _agent_dir = AgentDirGuard::set(&blocker);

        let result = SessionManager::list("/tmp", None, None, None).await;
        let error = result.expect_err("list must reject when the session dir cannot be created");
        // Node 的 fs 异常文案（TS 把 `mkdirSync` 的异常原样抛给调用方）。
        assert!(
            error
                .message
                .starts_with("ENOTDIR: not a directory, mkdir '"),
            "unexpected error: {}",
            error.message
        );
    }
}
