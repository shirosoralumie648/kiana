//! 第 2 周：统一执行策略、relay 状态与后端不可用 fail-closed 的真实测试。
//!
//! 依赖宿主环境的测试（bubblewrap）在不可用时显式跳过并打印原因；策略/relay/路径
//! 门禁是纯逻辑，不依赖内核特性。

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use kiana_sandbox::{
    ExecutionPolicy, ExecutionStatus, SandboxError, SandboxManager, SandboxPolicy, SandboxType,
    SpawnOptions, StdioMode, ToolInput,
};

fn scratch_base() -> PathBuf {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(&base).expect("create target tmp");
    base
}

fn tempdir(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(scratch_base())
        .expect("tempdir")
}

fn workspace_policy(roots: Vec<PathBuf>) -> ExecutionPolicy {
    let cwd = roots.first().cloned().unwrap_or_else(scratch_base);
    ExecutionPolicy::new(SandboxPolicy::workspace_write_with_roots(roots), cwd)
}

fn write_paths(paths: &[PathBuf]) -> ToolInput<'_> {
    ToolInput::FileWrite { paths }
}

#[test]
fn workspace_write_allows_every_root_and_rejects_outside_paths() {
    let root_a = tempdir("kiana-policy-a-");
    let root_b = tempdir("kiana-policy-b-");
    let outside = tempdir("kiana-policy-out-");
    let policy = workspace_policy(vec![
        root_a.path().to_path_buf(),
        root_b.path().to_path_buf(),
    ]);

    let inside_a = root_a.path().join("a.txt");
    let inside_b = root_b.path().join("nested").join("b.txt");
    let outside_file = outside.path().join("evil.txt");
    for path in [&inside_a, &inside_b] {
        let decision = policy.resolve("write", &write_paths(std::slice::from_ref(path)));
        assert!(
            decision.is_allowed(),
            "{} should be writable: {:?}",
            path.display(),
            decision.denial()
        );
    }
    let decision = policy.resolve("write", &write_paths(&[outside_file]));
    let denial = decision.denial().expect("outside must be denied");
    assert_eq!(denial.code, "OUTSIDE_WORKSPACE");
}

#[test]
fn protected_git_and_kiana_paths_are_denied_under_every_root() {
    let root = tempdir("kiana-policy-protected-");
    let policy = workspace_policy(vec![root.path().to_path_buf()]);

    for relative in [
        ".git/config",
        ".git/hooks/post-checkout",
        ".kiana/settings.json",
    ] {
        let path = root.path().join(relative);
        let decision = policy.resolve("edit", &write_paths(&[path]));
        let denial = decision
            .denial()
            .unwrap_or_else(|| panic!("{relative} must be denied"));
        assert_eq!(denial.code, "PROTECTED_PATH", "{relative}");
    }

    // 受保护目录的兄弟路径仍然可写。
    let allowed = root.path().join(".gitignore");
    assert!(
        policy
            .resolve("write", &write_paths(&[allowed]))
            .is_allowed()
    );
}

#[test]
fn symlinks_are_resolved_before_the_write_decision() {
    #[cfg(unix)]
    {
        let root = tempdir("kiana-policy-root-");
        let outside = tempdir("kiana-policy-outside-");
        let policy = workspace_policy(vec![root.path().to_path_buf()]);

        // root/escape -> outside：穿过符号链接写到工作区外必须被拒绝。
        let escape = root.path().join("escape");
        std::os::unix::fs::symlink(outside.path(), &escape).expect("symlink outside");
        let via_escape = escape.join("via-symlink.txt");
        let denial = policy
            .resolve("write", &write_paths(&[via_escape]))
            .denial()
            .expect("symlink escape must be denied")
            .clone();
        assert_eq!(denial.code, "OUTSIDE_WORKSPACE");

        // root/friendly -> root/real：最终落在工作区内，允许。
        let real = root.path().join("real");
        std::fs::create_dir_all(&real).expect("mkdir real");
        let friendly = root.path().join("friendly");
        std::os::unix::fs::symlink(&real, &friendly).expect("symlink inside");
        assert!(
            policy
                .resolve("write", &write_paths(&[friendly.join("ok.txt")]))
                .is_allowed()
        );

        // 穿过符号链接指向 .git 也必须按受保护路径拒绝。
        let dot_git = root.path().join(".git");
        std::fs::create_dir_all(&dot_git).expect("mkdir .git");
        let git_alias = root.path().join("git-alias");
        std::os::unix::fs::symlink(&dot_git, &git_alias).expect("symlink git");
        let denial = policy
            .resolve("write", &write_paths(&[git_alias.join("config")]))
            .denial()
            .expect("symlink to .git must be denied")
            .clone();
        assert_eq!(denial.code, "PROTECTED_PATH");
    }
}

#[test]
fn parent_traversal_cannot_escape_a_writable_root() {
    let root = tempdir("kiana-policy-traversal-");
    let outside = tempdir("kiana-policy-traversal-out-");
    let policy = workspace_policy(vec![root.path().to_path_buf()]);
    let escaped = root
        .path()
        .join("..")
        .join(outside.path().file_name().expect("outside basename"))
        .join("evil.txt");
    let decision = policy.resolve("write", &write_paths(&[escaped]));
    let denial = decision.denial().expect("traversal must be denied");
    assert_eq!(denial.code, "OUTSIDE_WORKSPACE");
}

#[test]
fn read_only_and_danger_full_access_have_opposite_write_decisions() {
    let outside = tempdir("kiana-policy-ro-");
    let path = outside.path().join("x.txt");
    let read_only = ExecutionPolicy::new(SandboxPolicy::read_only(), outside.path());
    assert_eq!(
        read_only
            .resolve("write", &write_paths(std::slice::from_ref(&path)))
            .denial()
            .expect("read-only denies")
            .code,
        "READ_ONLY"
    );

    let danger = ExecutionPolicy::new(SandboxPolicy::danger_full_access(), outside.path());
    assert!(danger.resolve("write", &write_paths(&[path])).is_allowed());
}

#[test]
fn shell_resolution_uses_the_same_policy_without_path_checks() {
    let root = tempdir("kiana-policy-shell-");
    let policy = workspace_policy(vec![root.path().to_path_buf()]);
    let decision = policy.resolve(
        "bash",
        &ToolInput::Shell {
            command: "printf x > /etc/evil",
        },
    );
    // shell 的路径约束由沙箱在运行期施加；策略层只回答用哪一档。
    assert_eq!(
        decision.policy().map(SandboxPolicy::label),
        Some("workspace-write")
    );
}

fn relay_options() -> SpawnOptions {
    SpawnOptions {
        cwd: Some(scratch_base()),
        stdin: StdioMode::Ignore,
        stdout: StdioMode::Ignore,
        stderr: StdioMode::Ignore,
        detached: true,
        ..Default::default()
    }
}

#[tokio::test]
async fn relay_reports_a_confirmed_exit_code() {
    let manager = SandboxManager::new(SandboxPolicy::danger_full_access(), scratch_base());
    let mut relayed = manager
        .spawn_relayed(
            "/bin/sh",
            &["-c".to_string(), "exit 3".to_string()],
            &relay_options(),
        )
        .expect("spawn relay");
    let status = relayed.child_mut().wait().await.expect("wait relay");
    assert_eq!(status.code(), Some(3));
    assert_eq!(
        relayed.outcome(),
        ExecutionStatus::ConfirmedExit {
            code: 3,
            signal: None
        }
    );
}

#[cfg(unix)]
#[tokio::test]
async fn relay_killed_after_start_is_unconfirmed_and_the_command_is_not_retried() {
    let workspace = tempdir("kiana-relay-kill-");
    let marker = workspace.path().join("ran-once.txt");
    let manager = SandboxManager::new(SandboxPolicy::danger_full_access(), scratch_base());
    let mut relayed = manager
        .spawn_relayed(
            "/bin/sh",
            &[
                "-c".to_string(),
                format!("printf once >> '{}'; sleep 30", marker.display()),
            ],
            &relay_options(),
        )
        .expect("spawn relay");
    let pid = relayed.pid().expect("relay pid");

    // 等 relay 写下 started 且命令真的执行一次，再 SIGKILL 整个进程组
    // （模拟超时/取消的进程树清理）。
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let contents = std::fs::read_to_string(relayed.status_path()).unwrap_or_default();
        if contents.contains("started") && marker.exists() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "relay never wrote its started marker or the command never ran"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // SAFETY: pid 来自刚 spawn 的子进程，且以 detached 启动（进程组组长）。
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    let _ = relayed.child_mut().wait().await;
    let outcome = relayed.outcome();
    assert!(
        matches!(outcome, ExecutionStatus::Unconfirmed { .. }),
        "expected Unconfirmed, got {outcome:?}"
    );
    assert!(
        outcome
            .user_message()
            .contains("Do not retry automatically")
    );
    assert!(!outcome.allows_automatic_retry());

    // 没有任何自动重试：命令只执行过一次。
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        std::fs::read_to_string(&marker).expect("marker"),
        "once",
        "the command must not be retried after an unconfirmed result"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn relay_forwards_termination_to_the_command_and_confirms_the_exit() {
    let workspace = tempdir("kiana-relay-term-");
    let marker = workspace.path().join("started.txt");
    let manager = SandboxManager::new(SandboxPolicy::danger_full_access(), scratch_base());
    let mut relayed = manager
        .spawn_relayed(
            "/bin/sh",
            &[
                "-c".to_string(),
                format!("printf ran > '{}'; sleep 30", marker.display()),
            ],
            &relay_options(),
        )
        .expect("spawn relay");
    let pid = relayed.pid().expect("relay pid");

    // 等命令真正执行（relay 的 started 标记在 fork 之前就写了，不能用它判断）。
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() {
        assert!(Instant::now() < deadline, "the command never started");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // trap 在 `"$@" &` 之后才安装；给 relay 一点时间再发信号，避免命中默认动作。
    tokio::time::sleep(Duration::from_millis(200)).await;

    // SAFETY: 只向 relay 进程（进程组组长）发 SIGTERM；验证它把信号转发给命令，
    // 命令被信号终止后 relay 仍然写出确定的 exit 状态。
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    let status = relayed.child_mut().wait().await.expect("wait relay");
    let outcome = relayed.outcome();
    assert_eq!(status.code(), Some(143), "relay should exit 128 + SIGTERM");
    assert_eq!(
        outcome,
        ExecutionStatus::ConfirmedExit {
            code: 143,
            // 状态文件只有退出码，无法区分「命令自己 exit 143」与「被 SIGTERM 杀掉」。
            signal: None
        }
    );
}

/// `from_env` 读取进程级环境变量，测试之间必须串行。
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn unavailable_backend_fails_closed_without_running_the_command() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let workspace = tempdir("kiana-policy-unavailable-");
    let marker = workspace.path().join("marker.txt");

    let previous_policy = std::env::var_os("KIANA_SANDBOX_POLICY");
    let previous_sandbox = std::env::var_os("KIANA_SANDBOX");
    // SAFETY: 本测试持有 ENV_LOCK；本测试二进制里只有这里读写这两个变量。
    unsafe {
        std::env::set_var("KIANA_SANDBOX_POLICY", "this-is-not-a-policy");
        std::env::remove_var("KIANA_SANDBOX");
    }
    let manager = SandboxManager::from_env(workspace.path());
    let result = match &manager {
        Some(manager) => manager.spawn_relayed(
            "/bin/sh",
            &[
                "-c".to_string(),
                format!("printf ran > {}", marker.display()),
            ],
            &relay_options(),
        ),
        None => panic!("invalid policy must produce a fail-closed manager"),
    };
    // SAFETY: 恢复本测试修改的两个环境变量。
    unsafe {
        match previous_policy {
            Some(value) => std::env::set_var("KIANA_SANDBOX_POLICY", value),
            None => std::env::remove_var("KIANA_SANDBOX_POLICY"),
        }
        match previous_sandbox {
            Some(value) => std::env::set_var("KIANA_SANDBOX", value),
            None => std::env::remove_var("KIANA_SANDBOX"),
        }
    }

    let error = result.expect_err("fail-closed manager must reject spawn");
    assert!(matches!(error, SandboxError::Unavailable { .. }));
    assert!(!marker.exists(), "the command must not have run");
    assert!(matches!(
        ExecutionStatus::from_sandbox_error(&error),
        ExecutionStatus::BackendUnavailable { .. }
    ));
}

#[tokio::test]
async fn bubblewrap_backend_is_available_or_explicitly_skipped() {
    let workspace = tempdir("kiana-policy-bwrap-");
    let policy = SandboxPolicy::workspace_write(workspace.path()).with_network_access(true);
    let manager = SandboxManager::with_type(SandboxType::LinuxBubblewrap, policy, workspace.path());
    let Ok(manager) = manager else {
        eprintln!("skipping bubblewrap relay test: backend unavailable");
        return;
    };
    assert_eq!(manager.sandbox_type(), SandboxType::LinuxBubblewrap);
    let mut relayed = manager
        .spawn_relayed(
            "/bin/sh",
            &["-c".to_string(), "exit 0".to_string()],
            &relay_options(),
        )
        .expect("spawn in bwrap");
    let _ = relayed.child_mut().wait().await;
    assert_eq!(
        relayed.outcome(),
        ExecutionStatus::ConfirmedExit {
            code: 0,
            signal: None
        }
    );
}

/// 没有 bubblewrap 的机器上，显式选择 bwrap 必须在创建任何进程/临时文件之前失败，
/// 并映射成 `BackendUnavailable`（命令没有执行）；有 bubblewrap 的机器显式跳过。
#[tokio::test]
async fn unavailable_bubblewrap_is_backend_unavailable_or_explicitly_skipped() {
    let workspace = tempdir("kiana-policy-no-bwrap-");
    let policy = SandboxPolicy::workspace_write(workspace.path()).with_network_access(true);
    match SandboxManager::with_type(SandboxType::LinuxBubblewrap, policy, workspace.path()) {
        Ok(manager) => {
            assert_eq!(manager.sandbox_type(), SandboxType::LinuxBubblewrap);
            eprintln!("skipping bubblewrap-unavailable test: backend is available on this host");
        }
        Err(error) => {
            assert!(
                matches!(error, SandboxError::Unavailable { .. }),
                "{error:?}"
            );
            let status = ExecutionStatus::from_sandbox_error(&error);
            assert!(
                matches!(status, ExecutionStatus::BackendUnavailable { .. }),
                "{status:?}"
            );
            assert!(!status.may_have_executed());
            assert!(status.user_message().contains("was not executed"));
        }
    }
}
