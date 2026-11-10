//! `test/startup-session-name.test.ts`：`--name` 在运行期模型校验之前写入所选会话。
//!
//! 用 `--model missing-model` 让启动在模型解析阶段以 1 退出；会话文件里必须已经出现
//! 去掉首尾空白的 `session_info` 条目。

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

fn create_session_file(project_dir: &Path, session_file: &Path) {
    let timestamp = "2026-01-01T00:00:00.000Z";
    let header = serde_json::json!({
        "type": "session",
        "version": 3,
        "id": "existing-session",
        "timestamp": timestamp,
        "cwd": project_dir.to_string_lossy(),
    });
    let message = serde_json::json!({
        "type": "message",
        "id": "assistant-1",
        "parentId": null,
        "timestamp": timestamp,
        "message": {
            "role": "assistant",
            "content": [{ "type": "text", "text": "hello" }],
            "provider": "anthropic",
            "model": "claude-sonnet-4-5",
            "timestamp": 0,
        },
    });
    fs::write(session_file, format!("{header}\n{message}\n")).expect("session file");
}

fn read_session_info_names(session_file: &Path) -> Vec<String> {
    fs::read_to_string(session_file)
        .expect("read session")
        .trim()
        .split('\n')
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|entry| entry.get("type").and_then(|value| value.as_str()) == Some("session_info"))
        .map(|entry| {
            entry
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

#[test]
fn sets_name_on_the_selected_session_before_runtime_model_validation() {
    let root = create_temp_dir();
    let agent = root.join("agent");
    let project = root.join("project");
    let session_file = root.join("session.jsonl");
    fs::create_dir_all(&agent).expect("agent");
    fs::create_dir_all(&project).expect("project");
    create_session_file(&project, &session_file);

    let stdout_path = root.join("out");
    let stderr_path = root.join("err");
    let stdout = fs::File::create(&stdout_path).expect("out");
    let stderr = fs::File::create(&stderr_path).expect("err");
    let mut child = Command::new(env!("CARGO_BIN_EXE_kiana"))
        .args([
            "--session",
            &session_file.to_string_lossy(),
            "--name",
            "  CLI Named Session  ",
            "--model",
            "missing-model",
            "-p",
            "hi",
        ])
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

    assert_eq!(status.code(), Some(1));
    assert_eq!(
        read_session_info_names(&session_file),
        vec!["CLI Named Session".to_string()]
    );
}
