//! `core/exec.ts` 的移植测试（`test/exec.test.ts`）。
//!
//! TS 用 `node -e` 制造静默退出；Rust 用 `/bin/sh -c 'exit 1'` 替代（Windows 上跳过）。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kiana_core::exec::{ExecOptions, ExecResult, exec_command};
use tokio_util::sync::CancellationToken;

fn cwd() -> PathBuf {
    std::env::current_dir().expect("cwd")
}

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

// Regression test for https://github.com/earendil-works/kiana/issues/11085
#[test]
fn reports_spawn_errors_in_stderr() {
    let result = exec_command("kiana-command-that-does-not-exist-11085", &[], &cwd(), None);
    assert_eq!(result.code, 1);
    assert!(!result.killed);
    assert_eq!(result.stdout, "");
    assert!(
        result.stderr.contains("ENOENT"),
        "stderr was {:?}",
        result.stderr
    );
}

#[cfg(unix)]
#[test]
fn keeps_stderr_empty_when_a_command_exits_silently() {
    let result = exec_command("/bin/sh", &args(&["-c", "exit 1"]), &cwd(), None);
    assert_eq!(
        result,
        ExecResult {
            stdout: String::new(),
            stderr: String::new(),
            code: 1,
            killed: false,
        }
    );
}

#[cfg(unix)]
#[test]
fn captures_stdout_and_stderr() {
    let result = exec_command(
        "/bin/sh",
        &args(&["-c", "printf hello; printf world >&2"]),
        &cwd(),
        None,
    );
    assert_eq!(result.stdout, "hello");
    assert_eq!(result.stderr, "world");
    assert_eq!(result.code, 0);
    assert!(!result.killed);
}

#[cfg(unix)]
#[test]
fn decodes_multibyte_output_split_across_chunks() {
    // 逐字节写出一个多字节字符，确保增量解码器不会把块边界处的字符弄坏。
    let script = "printf '\\342'; sleep 0.05; printf '\\202'; sleep 0.05; printf '\\254'";
    let result = exec_command("/bin/sh", &args(&["-c", script]), &cwd(), None);
    assert_eq!(result.stdout, "€");
}

#[cfg(unix)]
#[test]
fn runs_in_the_given_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let result = exec_command("/bin/sh", &args(&["-c", "pwd"]), dir.path(), None);
    assert_eq!(
        result.stdout.trim(),
        std::fs::canonicalize(dir.path()).unwrap().to_string_lossy()
    );
}

#[cfg(unix)]
#[test]
fn options_cwd_field_is_ignored() {
    // TS 的 `ExecOptions.cwd` 从不被读取：真正生效的是位置参数 `cwd`。
    let actual_dir = tempfile::tempdir().unwrap();
    let other_dir = tempfile::tempdir().unwrap();
    let options = ExecOptions {
        cwd: Some(other_dir.path().to_path_buf()),
        ..ExecOptions::default()
    };
    let result = exec_command(
        "/bin/sh",
        &args(&["-c", "pwd"]),
        actual_dir.path(),
        Some(&options),
    );
    assert_eq!(
        result.stdout.trim(),
        std::fs::canonicalize(actual_dir.path())
            .unwrap()
            .to_string_lossy()
    );
}

#[cfg(unix)]
#[test]
fn timeout_kills_the_process() {
    let start = Instant::now();
    let options = ExecOptions {
        timeout: Some(100),
        ..ExecOptions::default()
    };
    let result = exec_command("/bin/sh", &args(&["-c", "sleep 5"]), &cwd(), Some(&options));
    assert!(
        start.elapsed() < Duration::from_secs(4),
        "timeout was not honored"
    );
    assert!(result.killed);
    // 被 SIGTERM 杀死时 Node 的 exit code 是 null → 0。
    assert_eq!(result.code, 0);
}

#[cfg(unix)]
#[test]
fn timeout_zero_means_no_timeout() {
    let options = ExecOptions {
        timeout: Some(0),
        ..ExecOptions::default()
    };
    let result = exec_command("/bin/sh", &args(&["-c", "exit 0"]), &cwd(), Some(&options));
    assert_eq!(result.code, 0);
    assert!(!result.killed);
}

#[cfg(unix)]
#[test]
fn already_aborted_signal_kills_immediately() {
    let token = CancellationToken::new();
    token.cancel();
    let options = ExecOptions {
        signal: Some(token),
        ..ExecOptions::default()
    };
    let start = Instant::now();
    let result = exec_command("/bin/sh", &args(&["-c", "sleep 5"]), &cwd(), Some(&options));
    assert!(start.elapsed() < Duration::from_secs(4));
    assert!(result.killed);
}

#[cfg(unix)]
#[test]
fn later_abort_kills_the_process() {
    let token = CancellationToken::new();
    let options = ExecOptions {
        signal: Some(token.clone()),
        ..ExecOptions::default()
    };
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        token.cancel();
    });
    let start = Instant::now();
    let result = exec_command("/bin/sh", &args(&["-c", "sleep 5"]), &cwd(), Some(&options));
    handle.join().unwrap();
    assert!(start.elapsed() < Duration::from_secs(4));
    assert!(result.killed);
}

#[test]
fn nonexistent_cwd_reports_spawn_error() {
    let result = exec_command(
        "/bin/sh",
        &args(&["-c", "true"]),
        Path::new("/kiana/no/such/directory"),
        None,
    );
    assert_eq!(result.code, 1);
    assert!(!result.killed);
    assert!(result.stdout.is_empty());
    assert!(
        result.stderr.contains("ENOENT"),
        "stderr was {:?}",
        result.stderr
    );
}

// Node 的异步 spawn 把 `cwd: ""` 当作未设置（在进程 cwd 里运行）；`spawnSync` 才报 ENOENT。
#[cfg(unix)]
#[test]
fn empty_cwd_runs_in_the_process_cwd() {
    let result = exec_command("/bin/sh", &args(&["-c", "pwd"]), Path::new(""), None);
    assert_eq!(result.code, 0, "stderr was {:?}", result.stderr);
    assert_eq!(
        result.stdout.trim(),
        std::env::current_dir().unwrap().to_string_lossy()
    );
}

// Node 的 `spawn("")` 同步抛 TypeError [ERR_INVALID_ARG_VALUE]；冻结签名不能抛错，
// Rust 侧把同样的 message 放进 stderr（与 spawn 错误路径同形状）。
#[test]
fn empty_command_reports_the_node_type_error() {
    let result = exec_command("", &[], &cwd(), None);
    assert_eq!(result.code, 1);
    assert!(!result.killed);
    assert_eq!(
        result.stderr,
        "The argument 'file' cannot be empty. Received ''"
    );
    assert_eq!(result.stdout, "");
}

// TS 的 SIGTERM 后 5s SIGKILL 分支永远不会执行（Node 在 kill() 成功时即置
// proc.killed=true）。因此忽略 SIGTERM 的子进程会按自己的退出码结束，而不是被强杀成
// code 0。这里用 `trap '' TERM; sleep 0.3; exit 7` 对拍。
// 注意：Rust 侧在 SIGKILL_GRACE（5s）后仍会升级为 SIGKILL，见下一个测试。
#[cfg(unix)]
#[test]
fn timeout_does_not_force_kill_a_process_that_ignores_sigterm() {
    let start = Instant::now();
    let options = ExecOptions {
        timeout: Some(100),
        ..ExecOptions::default()
    };
    let result = exec_command(
        "/bin/sh",
        &args(&["-c", "trap '' TERM; sleep 0.3; exit 7"]),
        &cwd(),
        Some(&options),
    );
    assert!(
        start.elapsed() < Duration::from_secs(4),
        "must not wait for the 5s SIGKILL timer"
    );
    assert_eq!(
        result.code, 7,
        "the process's own exit code must be reported"
    );
    assert!(result.killed);
}

// 子进程忽略 SIGTERM 且长时间不退出时，必须在 SIGKILL_GRACE 后升级为 SIGKILL，否则
// `exec_command`（扩展的 kiana.exec）会永久挂起，agent turn 也无法结束。
#[cfg(unix)]
#[test]
fn timeout_escalates_to_sigkill_when_sigterm_is_ignored() {
    let start = Instant::now();
    let options = ExecOptions {
        timeout: Some(100),
        ..ExecOptions::default()
    };
    let result = exec_command(
        "/bin/sh",
        &args(&["-c", "trap '' TERM; sleep 30"]),
        &cwd(),
        Some(&options),
    );
    // SIGKILL 在 SIGTERM 后 5s 发出；给调度与构建负载留出余量。
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "must escalate to SIGKILL instead of waiting for the process to finish"
    );
    assert!(result.killed);
    // 被信号杀死时 `status.code()` 为 None，映射为 0。
    assert_eq!(result.code, 0, "killed child reports code 0");
}

// finalize 关闭管道（TS 的 `stdout.destroy()`）：退出后仍持有继承管道的孙进程在第一次
// 写入时收到 EPIPE/SIGPIPE，不能继续运行。用“等 go 文件再写”把写入时刻精确放到
// exec_command 返回之后。
#[cfg(unix)]
#[test]
fn descendants_die_on_their_first_write_after_finalize() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker");
    let go = dir.path().join("go");
    let script = format!(
        "( while [ ! -e '{}' ]; do sleep 0.01; done; printf x; touch '{}' ) & exit 0",
        go.display(),
        marker.display()
    );
    let result = exec_command("/bin/sh", &args(&["-c", &script]), &cwd(), None);
    assert_eq!(result.code, 0);
    std::fs::write(&go, "go").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !marker.exists(),
        "a descendant holding the pipe must get EPIPE/SIGPIPE after finalize"
    );
}

// Node 的 error.message 带真实 errno 名（ENOTDIR），不是 "UNKNOWN"。
#[cfg(unix)]
#[test]
fn spawn_errors_report_the_real_errno_name() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-directory");
    std::fs::write(&file, "x").unwrap();
    let result = exec_command("/bin/sh", &args(&["-c", "true"]), &file.join("child"), None);
    assert_eq!(result.code, 1);
    assert_eq!(result.stderr, "spawn /bin/sh ENOTDIR");
}

// Regression test for https://github.com/earendil-works/kiana/issues/5303
// 子进程在退出前已经静默超过 100ms：TS 在 exit 时重新武装空闲计时器，孙进程在 exit 后
// 写入 stdout 的尾部输出必须被捕获，而不是立刻返回。
#[cfg(unix)]
#[test]
fn captures_descendant_output_written_after_a_silent_pre_exit_period() {
    let result = exec_command(
        "/bin/sh",
        &args(&["-c", "sleep 0.2; ( sleep 0.02; printf LATE ) & exit 0"]),
        &cwd(),
        None,
    );
    assert_eq!(result.stdout, "LATE");
    assert_eq!(result.code, 0);

    let result = exec_command(
        "/bin/sh",
        &args(&[
            "-c",
            "printf A; sleep 0.2; ( sleep 0.02; printf LATE ) & exit 0",
        ]),
        &cwd(),
        None,
    );
    assert_eq!(result.stdout, "ALATE");
    assert_eq!(result.code, 0);
}

// TS 的 timeout 定时器直到 waitForChildProcess resolve 才清理：在 exit 后的 100ms 宽限期内
// 触发时 `killed` 仍要变成 true（exec.ts:57-84,96-105）。
#[cfg(unix)]
#[test]
fn timeout_that_fires_during_the_post_exit_grace_marks_killed() {
    let options = ExecOptions {
        timeout: Some(100),
        ..ExecOptions::default()
    };
    let result = exec_command(
        "/bin/sh",
        &args(&["-c", "sleep 0.02; ( sleep 0.3; printf X ) & exit 0"]),
        &cwd(),
        Some(&options),
    );
    assert_eq!(result.code, 0);
    assert!(
        result.killed,
        "timeout during the post-exit grace must set killed"
    );
    // 宽限在 exit 后 100ms 到点，孙进程 300ms 后的输出被丢弃。
    assert_eq!(result.stdout, "");
}
