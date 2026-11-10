//! TS: `test/session-file-invalid.test.ts`（1 例，CLI 层）。
//!
//! CLI 的进程/退出码部分属于 CLI unit；本 crate 验证同一失败路径的
//! `session-manager` 契约：友好错误文本，且绝不改动非会话文件内容。

mod common;

use common::*;
use kiana_session_core::session_manager::SessionManager;

/// TS: "prints a friendly error and preserves non-session file content"
#[test]
fn prints_a_friendly_error_and_preserves_non_session_file_content() {
    let temp = temp_dir("kiana-session-file-invalid-");
    let root = temp.path().to_string_lossy().into_owned();
    let project_dir = format!("{root}/project");
    let session_file = format!("{root}/not-a-session.log");
    let original_content = "{\"type\":\"event\",\"data\":\"not a session\"}\n";
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(&session_file, original_content).unwrap();

    let error = error_text(SessionManager::open(
        &session_file,
        Some(&project_dir),
        None,
    ));
    assert_eq!(
        error,
        format!("Session file is not a valid kiana session: {session_file}")
    );
    // 不含堆栈/内部 API 名。
    assert!(!error.contains("SessionManager.open"));
    assert!(!error.contains("at "));
    assert_eq!(
        std::fs::read_to_string(&session_file).unwrap(),
        original_content
    );
}

/// 回归（评审 #7）：首行 `[]`（JSON.parse 成功但不是对象）让文件整体无效；TS 抛
/// `Session file is not a valid ... session`，且内容保持不变。
#[test]
fn rejects_a_file_whose_first_parsed_line_is_not_a_session_header() {
    let temp = temp_dir("kiana-session-file-nonobject-");
    let root = temp.path().to_string_lossy().into_owned();
    let project_dir = format!("{root}/project");
    let session_file = format!("{root}/session.jsonl");
    let original_content = "[]\n{\"type\":\"session\",\"id\":\"x\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":\"/tmp\"}\n";
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(&session_file, original_content).unwrap();

    let error = error_text(SessionManager::open(
        &session_file,
        Some(&project_dir),
        None,
    ));
    assert_eq!(
        error,
        format!("Session file is not a valid kiana session: {session_file}")
    );
    assert_eq!(
        std::fs::read_to_string(&session_file).unwrap(),
        original_content
    );
}
