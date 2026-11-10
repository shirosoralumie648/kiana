//! `test/args.test.ts` 的 CLI 冒烟面 + `spec/ca-cli.md` §3.1/§3.2 的进程级行为。
//!
//! TS 没有同名测试文件（`--version`/`--help`/未知选项由 `args.test.ts` 与 `stdout-cleanliness`
//! 覆盖）；Rust 侧按 §7「CLI 端到端」把二进制的退出码与逐字 stdout/stderr 固定下来。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 一次子进程运行的结果。
struct ChildResult {
    code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl ChildResult {
    fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/package_root")
}

fn create_workspace() -> PathBuf {
    let root = tempfile::tempdir().expect("temp dir").keep();
    fs::create_dir_all(root.join("agent")).expect("agent dir");
    fs::create_dir_all(root.join("project")).expect("project dir");
    root
}

fn run_cli(
    binary: &str,
    args: &[&str],
    root: &Path,
    envs: &[(&str, &str)],
    name: &str,
) -> ChildResult {
    let stdout_path = root.join(format!("{name}.out"));
    let stderr_path = root.join(format!("{name}.err"));
    let stdout = fs::File::create(&stdout_path).expect("stdout file");
    let stderr = fs::File::create(&stderr_path).expect("stderr file");
    let mut command = Command::new(binary);
    command
        .args(args)
        .current_dir(root.join("project"))
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", root)
        .env("KIANA_CODING_AGENT_DIR", root.join("agent"))
        .env("KIANA_PACKAGE_DIR", package_root())
        .env("KIANA_SKIP_VERSION_CHECK", "1")
        .env("KIANA_OFFLINE", "1")
        .env("FORCE_COLOR", "0")
        .env("TERM", "dumb")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    for (key, value) in envs {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("spawn kiana");
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
    ChildResult {
        code: status.code().unwrap_or(-1),
        stdout: fs::read(&stdout_path).unwrap_or_default(),
        stderr: fs::read(&stderr_path).unwrap_or_default(),
    }
}

#[test]
fn version_prints_the_release_and_exits_zero() {
    let root = create_workspace();
    let result = run_cli(
        env!("CARGO_BIN_EXE_kiana"),
        &["--version"],
        &root,
        &[],
        "version",
    );
    assert_eq!(result.code, 0);
    assert_eq!(result.stdout_text(), "1.4.0\n");
    assert_eq!(result.stderr_text(), "");
}

#[test]
fn help_matches_the_checked_in_golden_on_stdout() {
    let root = create_workspace();
    let result = run_cli(env!("CARGO_BIN_EXE_kiana"), &["--help"], &root, &[], "help");
    assert_eq!(result.code, 0);
    assert_eq!(
        result.stdout_text(),
        include_str!("golden/help.txt"),
        "help text differs from the TS golden"
    );
    // TS 的 `--help` 在干净环境下 stderr 为空；这也覆盖内置扩展不得在加载阶段报错
    // （回归：`builtin:codemode` 曾在工厂里调用 `kiana.getSettings()` 触发 panic）。
    assert_eq!(
        result.stderr_text(),
        "",
        "help must not write anything to stderr"
    );
}

#[test]
fn unknown_option_is_reported_on_stderr_with_exit_one() {
    let root = create_workspace();
    let result = run_cli(
        env!("CARGO_BIN_EXE_kiana"),
        &["--bogus"],
        &root,
        &[],
        "unknown",
    );
    assert_eq!(result.code, 1);
    assert!(result.stdout.is_empty(), "stdout must stay empty");
    // TS 的 stderr 恰好只有这一行；逐字比较可以暴露启动期多出的 panic/扩展错误。
    assert_eq!(
        result.stderr_text(),
        "Error: Unknown option: --bogus\n",
        "stderr must match TS exactly"
    );
}

/// 回归：迁移阶段的 Node fs 异常在 kiana-rpc 里同样必须映射为 `Error: <message>` + 退出码 1。
///
/// TS 的 `rpc-entry.ts` 不调用 `setupCli()`，但仍然没有 catch `main()` 的错误：迁移抛出的
/// 同步 fs 异常会作为 Node 未捕获异常打印并以 1 退出。Rust 侧若忘记安装
/// `install_fatal_startup_panic_hook`，同样的场景会以 101 退出并打印 `Box<dyn Any>`。
#[cfg(unix)]
#[test]
fn rpc_binary_maps_fatal_startup_errors_to_node_style_exit() {
    use std::os::unix::fs::PermissionsExt;

    let root = create_workspace();
    let agent = root.join("agent");
    fs::write(
        agent.join("settings.json"),
        r#"{"apiKeys":{"anthropic":"sk-test"}}"#,
    )
    .expect("legacy settings");

    let original_permissions = fs::metadata(&agent).expect("stat agent dir").permissions();
    let mut read_only = original_permissions.clone();
    read_only.set_mode(0o555);
    fs::set_permissions(&agent, read_only).expect("chmod agent dir");

    // root 无视目录权限（测试也可能以 root 运行）：此时该回归无法复现，跳过断言。
    let probe = agent.join(".write-probe");
    if fs::write(&probe, b"x").is_ok() {
        let _ = fs::remove_file(&probe);
        fs::set_permissions(&agent, original_permissions).expect("restore agent dir");
        return;
    }

    let result = run_cli(
        env!("CARGO_BIN_EXE_kiana-rpc"),
        &[],
        &root,
        &[],
        "rpc-fatal",
    );
    fs::set_permissions(&agent, original_permissions).expect("restore agent dir");

    assert_eq!(result.code, 1, "exit code must be Node's 1, not Rust's 101");
    assert!(result.stdout.is_empty());
    let stderr = result.stderr_text();
    assert!(
        !stderr.contains("panicked"),
        "panic leaked to stderr: {stderr}"
    );
    let auth_json = agent.join("auth.json");
    assert_eq!(
        stderr,
        format!(
            "Error: EACCES: permission denied, open '{}'\n",
            auth_json.display()
        )
    );
}

#[test]
fn rpc_mode_rejects_file_arguments() {
    let root = create_workspace();
    let file = root.join("prompt.txt");
    fs::write(&file, "hello").expect("write file");
    let file_arg = format!("@{}", file.to_string_lossy());
    let result = run_cli(
        env!("CARGO_BIN_EXE_kiana"),
        &["--mode", "rpc", &file_arg],
        &root,
        &[],
        "rpc-file",
    );
    assert_eq!(result.code, 1);
    assert!(result.stdout.is_empty());
    assert_eq!(
        result.stderr_text(),
        "Error: @file arguments are not supported in RPC mode\n"
    );
}

#[test]
fn json_mode_help_keeps_stdout_empty_and_prints_usage_on_stderr() {
    let root = create_workspace();
    let result = run_cli(
        env!("CARGO_BIN_EXE_kiana"),
        &["--mode", "json", "--help"],
        &root,
        &[],
        "json-help",
    );
    assert_eq!(result.code, 0);
    assert!(result.stdout.is_empty(), "stdout must stay empty");
    assert!(result.stderr_text().contains("Usage:"));
    assert!(
        !result.stderr_text().contains("panicked at"),
        "stderr was: {}",
        result.stderr_text()
    );
}

#[test]
fn rpc_binary_prints_the_same_version() {
    let root = create_workspace();
    let result = run_cli(
        env!("CARGO_BIN_EXE_kiana-rpc"),
        &["--version"],
        &root,
        &[],
        "rpc-version",
    );
    assert_eq!(result.code, 0);
    assert_eq!(result.stdout_text(), "1.4.0\n");
    assert_eq!(result.stderr_text(), "");
}

/// Node 的 `process.stdin` `end` 事件之后事件循环耗尽，进程执行 `exit` 处理器（写 fd 1 的
/// 终端恢复序列）后以 0 退出。回归：启动期 TUI 驱动循环曾经永远等不到事件，`--resume`
/// 配合关闭的 stdin 会永久挂起。
///
/// 同时覆盖 stdout 接管：`--mode json` 接管 stdout 后 TUI 字节必须进 stderr（TS 的
/// `takeOverStdout` 替换 `process.stdout.write`），协议通道只剩 18 字节的终端恢复序列。
#[test]
fn resume_with_closed_stdin_exits_zero_and_keeps_protocol_stdout_clean() {
    let root = create_workspace();
    let result = run_cli(
        env!("CARGO_BIN_EXE_kiana"),
        &["--mode", "json", "--resume"],
        &root,
        &[],
        "json-resume-eof",
    );
    assert_eq!(result.code, 0, "stdin EOF must exit 0 like Node");
    assert_eq!(
        result.stdout, b"\x1b[?25h\x1b[?2004l\x1b[<u",
        "stdout must carry only the exit-restore sequence"
    );
    let stderr = result.stderr_text();
    assert!(
        stderr.contains("\x1b[?2026h"),
        "the TUI frame must be written to stderr while stdout is taken over"
    );
    assert!(
        !stderr.contains("panicked"),
        "unexpected panic on stderr: {stderr}"
    );
}

/// `--mode rpc` 与 json 同样接管 stdout：`--resume` 的启动 TUI 也必须进 stderr。
#[test]
fn rpc_resume_with_closed_stdin_exits_zero_and_keeps_protocol_stdout_clean() {
    let root = create_workspace();
    let result = run_cli(
        env!("CARGO_BIN_EXE_kiana"),
        &["--mode", "rpc", "--resume"],
        &root,
        &[],
        "rpc-resume-eof",
    );
    assert_eq!(result.code, 0, "stdin EOF must exit 0 like Node");
    assert_eq!(result.stdout, b"\x1b[?25h\x1b[?2004l\x1b[<u");
    assert!(result.stderr_text().contains("\x1b[?2026h"));
}

/// `kiana config` 的启动选择器（`cli/config-selector.ts`）同样在 stdin EOF 后退出 0。
///
/// 该命令在 `takeOverStdout` 之前处理，stdout 未被接管：TUI 帧与终端恢复序列都写 stdout，
/// stderr 保持为空。
#[test]
fn config_with_closed_stdin_exits_zero_with_tui_on_stdout() {
    let root = create_workspace();
    let result = run_cli(
        env!("CARGO_BIN_EXE_kiana"),
        &["config"],
        &root,
        &[],
        "config-eof",
    );
    assert_eq!(result.code, 0, "stdin EOF must exit 0 like Node");
    assert_eq!(result.stderr_text(), "", "config writes nothing to stderr");
    assert!(
        result.stdout.windows(8).any(|w| w == b"\x1b[?2026h"),
        "the TUI frame must be rendered on stdout (stdout is not taken over)"
    );
    assert!(
        result.stdout.ends_with(b"\x1b[?25h\x1b[?2004l\x1b[<u"),
        "the process exit handler must restore the terminal last"
    );
}

/// 回归（`main.ts:299-336`）：空串 `--session` 在 TS 里是假值，等价于未提供；它不能触发
/// `--fork` / `--session-id` 的冲突错误。
/// 回归：会话目录创建失败时，`Error: <message>` 必须是 Node/libuv 的 fs 文案
/// （`ENOTDIR: not a directory, mkdir '<dir>'`），而不是 Rust 的
/// `Not a directory (os error 20)`。TS 把 `mkdirSync` 的异常原样抛出，CLI 打印它。
#[test]
fn session_directory_creation_errors_use_node_fs_messages() {
    let root = create_workspace();
    let blocker = root.join("blocker");
    fs::write(&blocker, "not a directory").expect("blocker file");
    let session_dir = format!("{}/sessions", blocker.to_string_lossy());
    let result = run_cli(
        env!("CARGO_BIN_EXE_kiana"),
        &["--session-dir", &session_dir, "-p"],
        &root,
        &[],
        "session-dir-error",
    );
    assert_eq!(result.code, 1);
    assert!(result.stdout.is_empty(), "stdout must stay empty");
    assert_eq!(
        result.stderr_text(),
        format!("Error: ENOTDIR: not a directory, mkdir '{session_dir}'\n")
    );
}

#[test]
fn empty_session_value_is_falsy_for_conflict_validation() {
    let root = create_workspace();

    // `--fork <path> --session ''`：不报冲突，直接走 fork 路径（文件无效 → 退出 1）。
    let fork = run_cli(
        env!("CARGO_BIN_EXE_kiana"),
        &["--fork", "/nope/missing.jsonl", "--session", "", "-p"],
        &root,
        &[],
        "empty-session-fork",
    );
    assert_eq!(fork.code, 1);
    assert_eq!(
        fork.stderr_text(),
        "Error: Cannot fork: source session file is empty or invalid: /nope/missing.jsonl\n"
    );

    // `--session-id abc --session ''`：不报冲突，进入“创建新会话”的警告路径。
    let session_id = run_cli(
        env!("CARGO_BIN_EXE_kiana"),
        &["--session-id", "abc", "--session", "", "--help"],
        &root,
        &[],
        "empty-session-id",
    );
    assert_eq!(session_id.code, 0);
    assert!(
        !session_id.stderr_text().contains("cannot be combined"),
        "empty --session must not conflict: {}",
        session_id.stderr_text()
    );
}
