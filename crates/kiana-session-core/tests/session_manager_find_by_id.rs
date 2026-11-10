//! TS: `test/session-id-readonly.test.ts` 中的 `findById` 用例（2 例）。
//!
//! 该文件的其余用例经由 CLI / `createSessionManager`（其它 unit），不在本 crate 范围。

mod common;

use common::*;
use kiana_session_core::session_manager::SessionManager;

fn persist_session(session: &mut SessionManager, content: &str) {
    session.append_message(user_message(content, 0)).unwrap();
    session
        .append_message(assistant_message("persisted", 0))
        .unwrap();
}

/// TS: "filters exact IDs by cwd in a custom session directory"
#[test]
fn filters_exact_ids_by_cwd_in_a_custom_session_directory() {
    let temp = temp_dir("kiana-session-id-readonly-");
    let root = temp.path().to_string_lossy().into_owned();
    let project_a = format!("{root}/project-a");
    let project_b = format!("{root}/project-b");
    let session_dir = format!("{root}/sessions");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();

    let mut foreign = SessionManager::create(
        &project_b,
        Some(&session_dir),
        Some(kiana_session_core::session_manager::NewSessionOptions {
            id: Some("foreign-id".to_string()),
            parent_session: None,
        }),
    )
    .unwrap();
    persist_session(&mut foreign, "foreign session");
    let foreign_file = foreign.get_session_file().unwrap().to_string();

    assert!(
        SessionManager::find_by_id(&project_a, "foreign-id", Some(&session_dir))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        SessionManager::find_by_id(&project_b, "foreign-id", Some(&session_dir)).unwrap(),
        Some(foreign_file)
    );
}

/// TS: "reopens an exact ID from a renamed session file"（`findById` 只读 header，不看文件名）。
#[test]
fn reopens_an_exact_id_from_a_renamed_session_file() {
    let temp = temp_dir("kiana-session-id-renamed-");
    let root = temp.path().to_string_lossy().into_owned();
    let project = format!("{root}/project");
    let session_dir = format!("{root}/sessions");
    std::fs::create_dir_all(&project).unwrap();

    let mut original = SessionManager::create(
        &project,
        Some(&session_dir),
        Some(kiana_session_core::session_manager::NewSessionOptions {
            id: Some("renamed-id".to_string()),
            parent_session: None,
        }),
    )
    .unwrap();
    persist_session(&mut original, "persist me");
    let original_file = original.get_session_file().unwrap().to_string();
    let renamed = format!("{session_dir}/imported-session.jsonl");
    std::fs::rename(&original_file, &renamed).unwrap();

    assert_eq!(
        SessionManager::find_by_id(&project, "renamed-id", Some(&session_dir)).unwrap(),
        Some(renamed)
    );
}
