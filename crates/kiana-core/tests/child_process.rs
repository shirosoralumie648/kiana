//! `utils/child-process.ts` 的移植测试。
//!
//! TS 没有直接的单测（行为由 `5303-bash-output-truncation` 与调用方覆盖）；这里补充
//! `spawn_process` / `spawn_process_sync` / `wait_for_child_process` 的基础契约。

use kiana_core::child_process::{
    SpawnOptions, SpawnSyncOptions, StdioMode, spawn_process, spawn_process_sync,
    wait_for_child_process,
};

#[tokio::test]
async fn spawn_process_runs_and_waits() {
    let mut child = spawn_process(
        "/bin/sh",
        &[
            "-c".to_string(),
            "printf hi; printf err >&2; exit 3".to_string(),
        ],
        &SpawnOptions {
            stdout: StdioMode::Pipe,
            stderr: StdioMode::Pipe,
            ..SpawnOptions::default()
        },
    )
    .expect("spawn");
    let mut stdout = child.stdout.take().expect("stdout");
    let mut stderr = child.stderr.take().expect("stderr");
    let mut stdout_text = String::new();
    let mut stderr_text = String::new();
    let (status, _, _) = tokio::join!(
        wait_for_child_process(&mut child),
        tokio::io::AsyncReadExt::read_to_string(&mut stdout, &mut stdout_text),
        tokio::io::AsyncReadExt::read_to_string(&mut stderr, &mut stderr_text),
    );
    assert_eq!(status.expect("wait"), Some(3));
    assert_eq!(stdout_text, "hi");
    assert_eq!(stderr_text, "err");
}

#[tokio::test]
async fn spawn_process_reports_node_style_spawn_errors() {
    let error = spawn_process(
        "kiana-command-that-does-not-exist-11085",
        &[],
        &SpawnOptions::default(),
    )
    .expect_err("must fail");
    assert_eq!(
        error.to_string(),
        "spawn kiana-command-that-does-not-exist-11085 ENOENT"
    );
}

#[tokio::test]
async fn spawn_process_respects_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn_process(
        "/bin/sh",
        &["-c".to_string(), "pwd".to_string()],
        &SpawnOptions {
            cwd: Some(dir.path().to_path_buf()),
            stdout: StdioMode::Pipe,
            stderr: StdioMode::Pipe,
            ..SpawnOptions::default()
        },
    )
    .expect("spawn");
    let mut stdout = child.stdout.take().expect("stdout");
    let mut text = String::new();
    tokio::io::AsyncReadExt::read_to_string(&mut stdout, &mut text)
        .await
        .unwrap();
    wait_for_child_process(&mut child).await.unwrap();
    let expected = std::fs::canonicalize(dir.path()).unwrap();
    assert_eq!(
        text.trim(),
        expected.to_string_lossy(),
        "pwd should report the configured cwd"
    );
}

#[test]
fn spawn_process_sync_captures_output() {
    let result = spawn_process_sync(
        "/bin/sh",
        &[
            "-c".to_string(),
            "printf out; printf err >&2; exit 4".to_string(),
        ],
        &SpawnSyncOptions::default(),
    );
    assert_eq!(result.status, Some(4));
    assert_eq!(result.stdout, "out");
    assert_eq!(result.stderr, "err");
    assert!(result.error.is_none());
}

#[test]
fn spawn_process_sync_never_returns_err() {
    let result = spawn_process_sync(
        "kiana-command-that-does-not-exist-11085",
        &[],
        &SpawnSyncOptions::default(),
    );
    assert_eq!(result.status, None);
    assert!(result.stdout.is_empty());
    let error = result.error.expect("error is recorded instead of returned");
    assert_eq!(
        error.to_string(),
        "spawnSync kiana-command-that-does-not-exist-11085 ENOENT"
    );
}

#[test]
fn spawn_process_sync_ignores_stdio() {
    let result = spawn_process_sync(
        "/bin/sh",
        &["-c".to_string(), "cat; printf done".to_string()],
        &SpawnSyncOptions::ignored(),
    );
    assert_eq!(result.status, Some(0));
    assert!(result.stdout.is_empty());
}

#[test]
fn spawn_process_sync_times_out() {
    let start = std::time::Instant::now();
    let result = spawn_process_sync(
        "/bin/sh",
        &["-c".to_string(), "sleep 5".to_string()],
        &SpawnSyncOptions {
            timeout_ms: Some(100),
            ..SpawnSyncOptions::default()
        },
    );
    assert!(start.elapsed() < std::time::Duration::from_secs(4));
    assert_eq!(result.status, None);
    assert_eq!(
        result.error.map(|error| error.to_string()),
        Some("spawnSync /bin/sh ETIMEDOUT".to_string())
    );
}

// Node 的 spawnSync 超时发送 killSignal（默认 SIGTERM），不是 SIGKILL；这里用 trap 验证。
#[cfg(unix)]
#[test]
fn spawn_process_sync_timeout_sends_sigterm_instead_of_sigkill() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("term-handler-ran");
    let script = format!(
        "trap 'printf term > \"{}\"; exit 0' TERM; while :; do sleep 0.01; done",
        marker.display()
    );
    let start = std::time::Instant::now();
    let result = spawn_process_sync(
        "/bin/sh",
        &["-c".to_string(), script],
        &SpawnSyncOptions {
            timeout_ms: Some(100),
            ..SpawnSyncOptions::default()
        },
    );
    assert!(start.elapsed() < std::time::Duration::from_secs(4));
    assert!(
        marker.exists(),
        "timeout must deliver SIGTERM so the trap can run"
    );
    assert_eq!(
        result.error.map(|error| error.to_string()),
        Some("spawnSync /bin/sh ETIMEDOUT".to_string())
    );
}

// Node 的 error.message 以真实 errno 名结尾（ENOENT/EACCES/ENOTDIR…），不是 "UNKNOWN"。
#[cfg(unix)]
#[test]
fn spawn_process_sync_reports_the_real_errno_name() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-directory");
    std::fs::write(&file, "x").unwrap();
    let result = spawn_process_sync(
        "/bin/sh",
        &["-c".to_string(), "true".to_string()],
        &SpawnSyncOptions {
            cwd: Some(file.join("child")),
            ..SpawnSyncOptions::default()
        },
    );
    let error = result.error.expect("spawn error");
    assert_eq!(error.to_string(), "spawnSync /bin/sh ENOTDIR");
}

// Node 的 `detached: true` 在 Unix 上创建新会话（setsid），不是只建进程组。
#[cfg(target_os = "linux")]
#[tokio::test]
async fn detached_spawn_creates_a_new_session() {
    if !std::path::Path::new("/proc/self/stat").exists() {
        return;
    }
    let mut child = spawn_process(
        "/bin/sh",
        &["-c".to_string(), "sleep 1".to_string()],
        &SpawnOptions {
            detached: true,
            ..SpawnOptions::default()
        },
    )
    .expect("spawn");
    let pid = child.id().expect("child pid");
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("/proc stat");
    let after_comm = stat
        .rsplit_once(')')
        .map(|(_, rest)| rest)
        .expect("comm field");
    let session: u32 = after_comm
        .split_whitespace()
        .nth(3)
        .expect("session field")
        .parse()
        .expect("numeric session id");
    assert_eq!(
        session, pid,
        "detached child must be its own session leader (setsid)"
    );

    let _ = child.start_kill();
    let _ = child.wait().await;
}

#[test]
fn spawn_process_sync_closes_piped_stdin_like_node() {
    // Node 的 spawnSync 没有 `input` 时会立刻关闭 stdin：`read` 得到 EOF，不会挂住。
    let result = spawn_process_sync(
        "/bin/sh",
        &[
            "-c".to_string(),
            "read line; printf '%s' \"$line\"".to_string(),
        ],
        &SpawnSyncOptions {
            stdin: StdioMode::Pipe,
            ..SpawnSyncOptions::default()
        },
    );
    assert_eq!(result.status, Some(0));
    assert_eq!(result.stdout, "");
}

// Node 的 `spawn("")`/`spawnSync("")` 同步抛 TypeError [ERR_INVALID_ARG_VALUE]；
// message 是稳定契约（冻结签名不能抛，放进 Result::Err / error 字段）。
#[tokio::test]
async fn spawn_process_rejects_an_empty_command_like_node() {
    let error = spawn_process("", &[], &SpawnOptions::default()).expect_err("must fail");
    assert_eq!(
        error.to_string(),
        "The argument 'file' cannot be empty. Received ''"
    );
}

#[test]
fn spawn_process_sync_reports_an_empty_command_like_node() {
    let result = spawn_process_sync("", &[], &SpawnSyncOptions::default());
    assert_eq!(result.status, None);
    assert!(result.stdout.is_empty());
    assert!(result.stderr.is_empty());
    assert_eq!(
        result.error.expect("error").to_string(),
        "The argument 'file' cannot be empty. Received ''"
    );
}

// Node 的异步 spawn 把 `cwd: ""` 当作未设置；spawnSync 则报 ENOENT。
#[cfg(unix)]
#[tokio::test]
async fn spawn_process_runs_in_the_process_cwd_for_an_empty_cwd() {
    let mut child = spawn_process(
        "/bin/sh",
        &["-c".to_string(), "pwd".to_string()],
        &SpawnOptions {
            cwd: Some(std::path::PathBuf::new()),
            stdout: StdioMode::Pipe,
            stderr: StdioMode::Pipe,
            ..SpawnOptions::default()
        },
    )
    .expect("spawn");
    let mut stdout = child.stdout.take().expect("stdout");
    let mut text = String::new();
    tokio::io::AsyncReadExt::read_to_string(&mut stdout, &mut text)
        .await
        .unwrap();
    wait_for_child_process(&mut child).await.unwrap();
    assert_eq!(
        text.trim(),
        std::env::current_dir().unwrap().to_string_lossy()
    );
}

#[cfg(unix)]
#[test]
fn spawn_process_sync_fails_for_an_empty_cwd_like_node() {
    let result = spawn_process_sync(
        "/bin/sh",
        &["-c".to_string(), "pwd".to_string()],
        &SpawnSyncOptions {
            cwd: Some(std::path::PathBuf::new()),
            ..SpawnSyncOptions::default()
        },
    );
    assert_eq!(result.status, None);
    assert_eq!(
        result.error.map(|error| error.to_string()),
        Some("spawnSync /bin/sh ENOENT".to_string())
    );
}

// Node 的 spawnSync 在超时瞬间销毁 stdio：子进程已退出但孙进程仍持有管道时，
// 一旦到点就停止读取并报 ETIMEDOUT，`status` 保留**超时前**观察到的退出码。
#[cfg(unix)]
#[test]
fn spawn_process_sync_timeout_destroys_stdio_after_the_child_exited() {
    let start = std::time::Instant::now();
    let result = spawn_process_sync(
        "/bin/sh",
        &[
            "-c".to_string(),
            "(sleep 0.5; printf LATE) & exit 0".to_string(),
        ],
        &SpawnSyncOptions {
            timeout_ms: Some(100),
            ..SpawnSyncOptions::default()
        },
    );
    assert!(start.elapsed() < std::time::Duration::from_secs(4));
    assert_eq!(
        result.status,
        Some(0),
        "exit code observed before the timeout"
    );
    assert_eq!(
        result.stdout, "",
        "output written after the timeout is discarded"
    );
    assert_eq!(
        result.error.map(|error| error.to_string()),
        Some("spawnSync /bin/sh ETIMEDOUT".to_string())
    );
}

// 超时后才退出的进程：Node 报 status:null（即使它后来自行退出），且超时后的输出被丢弃。
#[cfg(unix)]
#[test]
fn spawn_process_sync_timeout_reports_null_status_for_a_late_exit() {
    let start = std::time::Instant::now();
    let result = spawn_process_sync(
        "/bin/sh",
        &[
            "-c".to_string(),
            "trap '' TERM; sleep 0.4; printf b; exit 3".to_string(),
        ],
        &SpawnSyncOptions {
            timeout_ms: Some(100),
            ..SpawnSyncOptions::default()
        },
    );
    assert!(start.elapsed() >= std::time::Duration::from_millis(300));
    assert_eq!(result.status, None, "must not report the late exit code");
    assert!(
        !result.stdout.contains('b'),
        "stdout was {:?}",
        result.stdout
    );
    assert_eq!(
        result.error.map(|error| error.to_string()),
        Some("spawnSync /bin/sh ETIMEDOUT".to_string())
    );
}
