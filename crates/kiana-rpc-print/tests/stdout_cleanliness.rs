//! `packages/coding-agent/test/stdout-cleanliness.test.ts` 的 Rust 移植。
//!
//! TS 用 `node --import source-resolver src/cli.ts` 起真实 CLI 子进程，断言非交互模式下
//! stdout 只承载协议/帮助文本：
//!
//! 1. `--help`（stdout 重定向）：exit 0，stdout 含 `Usage:`，stderr 不含；
//! 2. `--mode json --help --approve`：exit 0，stdout **为空**，stderr 含 `Usage:`
//!    （`takeOverStdout` 把普通 stdout 输出重定向到 stderr）。
//!
//! Rust 侧等价的被测对象是 stage-8 的 `kiana` 二进制（ca-cli），因此这些用例 `#[ignore]`，
//! 设置 `KIANA_BIN=/path/to/kiana` 后可用
//! `cargo test -p kiana-rpc-print --test stdout_cleanliness -- --ignored` 验证。
//!
//! **有意省略的断言**：TS 通过临时 `settings.json` 的 `packages` + `npmCommand` 伪造
//! `changed 1 package in 471ms` / `found 0 vulnerabilities` 两行 npm chatter，用来证明
//! 它们被赶到 stderr。Rust 二进制没有 npm 安装器，不存在该输出，因此不构造该装置、
//! 也不断言这两行（断言 `Usage:` 的落点已经覆盖 stdout 清洁性本身）。

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// 测试用临时目录（Drop 时递归删除）。
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = format!(
            "{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        );
        let path = std::env::temp_dir().join(format!("kiana-stdout-clean-{tag}-{unique}"));
        std::fs::create_dir_all(path.join("agent")).expect("create agent dir");
        std::fs::create_dir_all(path.join("project")).expect("create project dir");
        Self { path }
    }

    fn project_dir(&self) -> PathBuf {
        self.path.join("project")
    }

    fn agent_dir(&self) -> PathBuf {
        self.path.join("agent")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// `kiana` 可执行文件：优先 `KIANA_TEST_BIN`，其次 `KIANA_BIN`。
fn kiana_binary() -> PathBuf {
    std::env::var_os("KIANA_TEST_BIN")
        .or_else(|| std::env::var_os("KIANA_BIN"))
        .map(PathBuf::from)
        .expect(
            "set KIANA_BIN to the `kiana` executable (built by the kiana-coding-agent stage-8 unit) \
             to run these ignored stdout-cleanliness tests",
        )
}

/// 与 TS `runCli` 同构：独立 cwd / agent dir，stdin 关闭，stdout/stderr 捕获。
fn run_cli(args: &[&str]) -> Output {
    let dir = TempDir::new("run");
    Command::new(kiana_binary())
        .args(args)
        .current_dir(dir.project_dir())
        .env("KIANA_CODING_AGENT_DIR", dir.agent_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn the kiana binary")
}

/// TS: "prints plain --help to stdout when stdout is redirected"。
#[test]
#[ignore = "needs the `kiana` binary from kiana-coding-agent stage 8 (set KIANA_BIN)"]
fn plain_help_goes_to_stdout_when_stdout_is_redirected() {
    let output = run_cli(&["--help"]);
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("Usage:"),
        "stdout must carry Usage: {stdout:?}"
    );
    assert!(
        !stderr.contains("Usage:"),
        "stderr must not carry Usage: {stderr:?}"
    );
    // npm chatter 断言见文件头“有意省略的断言”。
    assert!(!stderr.contains("changed 1 package in 471ms"));
    assert!(!stderr.contains("found 0 vulnerabilities"));
}

/// TS: "keeps stdout empty for --mode json --help while routing trusted startup chatter to stderr"。
#[test]
#[ignore = "needs the `kiana` binary from kiana-coding-agent stage 8 (set KIANA_BIN)"]
fn json_mode_help_keeps_stdout_empty() {
    let output = run_cli(&["--mode", "json", "--help", "--approve"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "",
        "json mode must keep protocol stdout empty"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Usage:"),
        "help must be routed to stderr in json mode: {stderr:?}"
    );
}
