//! W3C `TRACEPARENT` / `TRACESTATE` 传入子进程的测试。
//!
//! 使用 `SandboxPolicy::danger_full_access()`（不需要 bwrap/Landlock 后端）执行一个真实
//! shell，断言环境变量在 `SpawnOptions::env` 的 `env_clear` 之后仍然存在。

use std::ffi::OsString;
use std::path::PathBuf;

use kiana_sandbox::{SandboxManager, SandboxPolicy, SpawnOptions, StdioMode};

fn manager() -> SandboxManager {
    let cwd = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(&cwd).expect("create target tmp");
    SandboxManager::new(SandboxPolicy::danger_full_access(), cwd)
}

fn options() -> SpawnOptions {
    SpawnOptions {
        stdin: StdioMode::Ignore,
        stdout: StdioMode::Pipe,
        stderr: StdioMode::Pipe,
        env: Some(vec![(
            OsString::from("PATH"),
            OsString::from(std::env::var("PATH").unwrap_or_default()),
        )]),
        ..SpawnOptions::default()
    }
}

#[tokio::test]
async fn traceparent_is_injected_after_env_clear() {
    let child = manager()
        .spawn(
            "/bin/sh",
            &[
                "-c".to_string(),
                "printf %s \"$TRACEPARENT|$TRACESTATE\"".to_string(),
            ],
            &options()
                .with_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
                .with_tracestate("vendor=value"),
        )
        .expect("spawn");
    let output = child.wait_with_output().await.expect("wait");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01|vendor=value"
    );
}

#[tokio::test]
async fn no_trace_variables_when_not_requested() {
    let child = manager()
        .spawn(
            "/bin/sh",
            &[
                "-c".to_string(),
                "printf %s \"${TRACEPARENT-unset}\"".to_string(),
            ],
            &options(),
        )
        .expect("spawn");
    let output = child.wait_with_output().await.expect("wait");
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "unset");
}

#[tokio::test]
async fn relayed_spawn_also_receives_traceparent() {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("tempdir");
    let path = dir.path().join("trace.txt");
    let script = format!("printf %s \"$TRACEPARENT\" > {}", path.display());
    let mut relayed = manager()
        .spawn_relayed(
            "/bin/sh",
            &["-c".to_string(), script],
            &options().with_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
        )
        .expect("spawn relayed");
    let _status = relayed.child_mut().wait().await.expect("wait relay");
    let written = std::fs::read_to_string(&path).expect("trace file");
    assert_eq!(
        written,
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
    );
}
