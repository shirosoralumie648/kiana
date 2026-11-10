//! TS: `test/session-cwd.test.ts`（3 例）。
//!
//! 第三个 TS 用例经由 `agent-session-runtime`（另一 unit）验证“运行时创建前抛错”；本 crate
//! 无法依赖该模块，这里改为直接验证同一路径上的 `assertSessionCwdExists` 抛出
//! `MissingSessionCwdError`（session-cwd.ts 自身的契约）。

mod common;

use common::*;
use kiana_session_core::session_cwd::{
    MissingSessionCwdError, assert_session_cwd_exists, get_missing_session_cwd_issue,
};
use kiana_session_core::session_manager::{FileEntry, SessionManager, entry_json_string};

fn write_session_file(path: &str, cwd: &str) {
    let header = session_header("session-id", Some(cwd), None);
    std::fs::write(
        path,
        format!("{}\n", entry_json_string(&FileEntry::Header(header))),
    )
    .unwrap();
}

/// TS: "detects missing session cwd from persisted sessions"
#[test]
fn detects_missing_session_cwd_from_persisted_sessions() {
    let fallback = temp_dir("kiana-session-cwd-fallback-");
    let fallback_cwd = fallback.path().to_string_lossy().into_owned();
    let missing_cwd = format!("{fallback_cwd}/does-not-exist");
    let session_dir = temp_dir("kiana-session-cwd-session-dir-");
    let session_dir_path = session_dir.path().to_string_lossy().into_owned();
    let session_file = format!("{session_dir_path}/session.jsonl");
    write_session_file(&session_file, &missing_cwd);

    let session = SessionManager::open(&session_file, None, None).unwrap();
    let issue = get_missing_session_cwd_issue(&session, &fallback_cwd).expect("missing cwd issue");
    assert_eq!(issue.session_file.as_deref(), session.get_session_file());
    assert_eq!(issue.session_cwd, missing_cwd);
    assert_eq!(issue.fallback_cwd, fallback_cwd);
}

/// TS: "supports overriding the effective cwd when opening a session"
#[test]
fn supports_overriding_the_effective_cwd_when_opening_a_session() {
    let fallback = temp_dir("kiana-session-cwd-override-");
    let fallback_cwd = fallback.path().to_string_lossy().into_owned();
    let missing_cwd = format!("{fallback_cwd}/does-not-exist");
    let session_dir = temp_dir("kiana-session-cwd-override-session-dir-");
    let session_dir_path = session_dir.path().to_string_lossy().into_owned();
    let session_file = format!("{session_dir_path}/session.jsonl");
    write_session_file(&session_file, &missing_cwd);

    let session = SessionManager::open(&session_file, None, Some(&fallback_cwd)).unwrap();
    assert_eq!(session.get_cwd(), fallback_cwd);
    assert!(get_missing_session_cwd_issue(&session, &fallback_cwd).is_none());
}

/// TS: "throws a controlled error before runtime creation when the stored cwd is missing"
#[test]
fn throws_a_controlled_error_before_runtime_creation_when_the_stored_cwd_is_missing() {
    let fallback = temp_dir("kiana-session-cwd-runtime-");
    let fallback_cwd = fallback.path().to_string_lossy().into_owned();
    let missing_cwd = format!("{fallback_cwd}/does-not-exist");
    let session_dir = temp_dir("kiana-session-cwd-runtime-session-dir-");
    let session_dir_path = session_dir.path().to_string_lossy().into_owned();
    let session_file = format!("{session_dir_path}/session.jsonl");
    write_session_file(&session_file, &missing_cwd);

    let session = SessionManager::open(&session_file, None, None).unwrap();
    let error: MissingSessionCwdError =
        assert_session_cwd_exists(&session, &fallback_cwd).expect_err("missing cwd must fail");
    assert_eq!(error.name(), "MissingSessionCwdError");
    assert_eq!(error.issue.session_cwd, missing_cwd);
}
