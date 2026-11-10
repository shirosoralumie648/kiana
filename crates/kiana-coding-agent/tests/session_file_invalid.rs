//! `test/session-file-invalid.test.ts`：`--session <非会话文件>` 的友好错误与文件保全。
//!
//! 断言逐字对齐 TS：stderr 只包含 `Error: Session file is not a valid kiana session: <path>`，
//! 不泄漏 `SessionManager.open` 或堆栈；文件内容原样保留。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/package_root")
}

fn create_temp_dir() -> PathBuf {
    let root = tempfile::tempdir().expect("temp dir").keep();
    fs::canonicalize(&root).expect("canonicalize")
}

#[test]
fn prints_a_friendly_error_and_preserves_non_session_file_content() {
    let root = create_temp_dir();
    let agent = root.join("agent");
    let project = root.join("project");
    let session_file = root.join("not-a-session.log");
    let original_content = "{\"type\":\"event\",\"data\":\"not a session\"}\n";
    fs::create_dir_all(&agent).expect("agent");
    fs::create_dir_all(&project).expect("project");
    fs::write(&session_file, original_content).expect("session file");

    let stdout_path = root.join("out");
    let stderr_path = root.join("err");
    let stdout = fs::File::create(&stdout_path).expect("out");
    let stderr = fs::File::create(&stderr_path).expect("err");
    let mut child = Command::new(env!("CARGO_BIN_EXE_kiana"))
        .args(["--session", &session_file.to_string_lossy(), "-p", "hi"])
        .current_dir(&project)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", &root)
        .env("KIANA_CODING_AGENT_DIR", &agent)
        .env("KIANA_PACKAGE_DIR", package_root())
        .env("KIANA_OFFLINE", "1")
        .env("KIANA_SKIP_VERSION_CHECK", "1")
        .env("FORCE_COLOR", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .expect("spawn kiana");
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if started.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("kiana did not exit within 10s");
        }
        std::thread::sleep(Duration::from_millis(10));
    };

    let stderr_text = fs::read_to_string(&stderr_path).unwrap_or_default();
    assert_eq!(status.code(), Some(1));
    assert!(
        stderr_text.contains(&format!(
            "Error: Session file is not a valid kiana session: {}",
            session_file.to_string_lossy()
        )),
        "stderr was: {stderr_text}"
    );
    assert!(!stderr_text.contains("SessionManager.open"));
    assert!(!stderr_text.contains("at "));
    assert_eq!(
        fs::read_to_string(&session_file).expect("read session file"),
        original_content
    );
}
