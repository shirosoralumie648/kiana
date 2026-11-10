//! `test/stdout-cleanliness.test.ts`：非交互模式下 stdout 只承载协议/命令输出。
//!
//! 关键断言（逐条对应 TS）：
//! - 普通 `--help`（stdout 未接管）把帮助写 stdout，项目未被信任时不安装项目包，
//!   因此 npm 的输出完全不可见；
//! - `--mode json --help --approve` 接管 stdout：协议通道必须保持为空，帮助与「可信启动
//!   杂讯」（npm 安装输出）都走 stderr。
//!
//! 夹具：`packages` 与 `npmCommand` 都写在项目的 `.kiana/settings.json` 里
//! （`npmCommand` 只在项目被信任时生效）；假 npm 是 shell 脚本。两个用例都不设置
//! `KIANA_OFFLINE`：普通 `--help` 依赖“项目未被信任”来证明 npm 根本没被调用（离线模式会
//! 跳过安装，使该断言失去意义），`--mode json --help --approve` 则让假 npm 真正运行。
//!
//! 额外的回归断言：启动过程不得出现扩展加载失败（内置 `codemode` 曾在工厂里过早调用
//! `kiana.getSettings()`，导致所有非交互启动多出 panic 与 `Failed to load extension` 行）。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct ChildResult {
    code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/package_root")
}

fn run_cli(root: &Path, args: &[&str], offline: bool, name: &str) -> ChildResult {
    let stdout_path = root.join(format!("{name}.out"));
    let stderr_path = root.join(format!("{name}.err"));
    let stdout = fs::File::create(&stdout_path).expect("stdout file");
    let stderr = fs::File::create(&stderr_path).expect("stderr file");
    let mut command = Command::new(env!("CARGO_BIN_EXE_kiana"));
    command
        .args(args)
        .current_dir(root.join("project"))
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", root)
        .env("KIANA_CODING_AGENT_DIR", root.join("agent"))
        .env("KIANA_PACKAGE_DIR", package_root())
        .env("KIANA_SKIP_VERSION_CHECK", "1")
        .env("FORCE_COLOR", "0")
        .env("TERM", "dumb")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    if offline {
        command.env("KIANA_OFFLINE", "1");
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

/// 扩展加载失败时 stderr 必须为空（对应 TS 的干净启动）。
fn assert_no_extension_load_failures(stderr: &str) {
    assert!(
        !stderr.contains("panicked at"),
        "unexpected panic during startup: {stderr}"
    );
    assert!(
        !stderr.contains("Failed to load extension"),
        "extension load failure on stderr: {stderr}"
    );
    assert!(
        !stderr.contains("Hint: Start without extensions"),
        "extension failure hint on stderr: {stderr}"
    );
}

fn create_project() -> PathBuf {
    let root = tempfile::tempdir().expect("temp dir").keep();
    fs::create_dir_all(root.join("agent")).expect("agent dir");
    fs::create_dir_all(root.join("project/.kiana")).expect("project config dir");

    let fake_npm = root.join("fake-npm.sh");
    fs::write(
        &fake_npm,
        "#!/bin/sh\necho \"changed 1 package in 471ms\"\necho \"found 0 vulnerabilities\"\nexit 0\n",
    )
    .expect("fake npm");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&fake_npm).expect("stat").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_npm, permissions).expect("chmod");
    }

    // 项目设置（与 TS 夹具一致）：包源 + npmCommand，只有项目被信任时才加载/生效。
    fs::write(
        root.join("project/.kiana/settings.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "packages": ["npm:fake-package"],
            "npmCommand": ["/bin/sh", fake_npm.to_string_lossy()],
        }))
        .expect("json"),
    )
    .expect("project settings");
    root
}

#[test]
fn prints_plain_help_to_stdout_when_stdout_is_redirected() {
    let root = create_project();
    // 与 TS 一样不设 `KIANA_OFFLINE`：项目未被信任，因此 `packages` 不会被安装，
    // `npmCommand` 也不生效——这才真正证明 npm 没有被调用。
    let result = run_cli(&root, &["--help"], false, "plain-help");
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);

    assert_eq!(result.code, 0);
    assert!(stdout.contains("Usage:"));
    assert!(!stderr.contains("Usage:"));
    assert!(!stderr.contains("changed 1 package in 471ms"));
    assert!(!stderr.contains("found 0 vulnerabilities"));
    assert_no_extension_load_failures(&stderr);
}

#[test]
fn keeps_stdout_empty_for_json_help_while_routing_trusted_startup_chatter_to_stderr() {
    let root = create_project();
    // 离线模式会跳过 `npm:` 包安装（`installMissingSource` 直接 Skipped）；本用例必须让
    // 假 npm 真正运行，因此不设置 `KIANA_OFFLINE`（网络请求仍然被 mock 的设置挡住）。
    let result = run_cli(
        &root,
        &["--mode", "json", "--help", "--approve"],
        false,
        "json-help",
    );
    let stderr = String::from_utf8_lossy(&result.stderr);

    assert_eq!(result.code, 0);
    assert!(result.stdout.is_empty(), "stdout must stay empty");
    assert!(stderr.contains("changed 1 package in 471ms"));
    assert!(stderr.contains("found 0 vulnerabilities"));
    assert!(stderr.contains("Usage:"));
    assert_no_extension_load_failures(&stderr);
}
