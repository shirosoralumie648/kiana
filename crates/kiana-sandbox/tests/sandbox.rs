//! 真实执行测试：bubblewrap / Landlock 必须挡住「工作区之外」的写入。
//!
//! 这些测试依赖宿主环境（bwrap、内核 Landlock）；后端不可用时测试会打印原因并跳过，
//! 不会把「环境没有沙箱」误判成代码失败。

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Output;

use kiana_sandbox::{SandboxManager, SandboxPolicy, SandboxType, SpawnOptions, StdioMode};

fn scratch_base() -> PathBuf {
    // 用 cargo 的 target/tmp，而不是 /tmp：bwrap 策略会把 /tmp 换成私有 tmpfs，
    // 放在 /tmp 里的工作区在沙箱内会消失，测不出「越界写入被拒绝」。
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

async fn run_program(
    manager: &SandboxManager,
    program: &str,
    script: &str,
    cwd: &Path,
    env: Vec<(&str, OsString)>,
) -> Output {
    // 测试脚本会调用 `cat` 等外部命令；显式给 PATH，避免 shell 的默认 PATH
    // 在不同发行版上不一致。
    let mut environment = Vec::with_capacity(env.len() + 1);
    environment.push((
        OsString::from("PATH"),
        OsString::from(std::env::var("PATH").unwrap_or_default()),
    ));
    environment.extend(
        env.into_iter()
            .map(|(key, value)| (OsString::from(key), value)),
    );
    let options = SpawnOptions {
        cwd: Some(cwd.to_path_buf()),
        stdin: StdioMode::Ignore,
        stdout: StdioMode::Pipe,
        stderr: StdioMode::Pipe,
        env: Some(environment),
        ..Default::default()
    };
    let child = manager
        .spawn(program, &["-c".to_string(), script.to_string()], &options)
        .expect("spawn sandboxed shell");
    child.wait_with_output().await.expect("wait")
}

async fn run(
    manager: &SandboxManager,
    script: &str,
    cwd: &Path,
    env: Vec<(&str, OsString)>,
) -> Output {
    run_program(manager, "/bin/sh", script, cwd, env).await
}

/// 工作区可写、工作区外只读、工作区外可读——三条断言一次跑完。
async fn assert_write_is_blocked(manager: &SandboxManager, workspace: &Path, outside: &Path) {
    std::fs::write(outside.join("readable.txt"), "hello").expect("seed outside file");

    let inside = workspace.join("inside.txt");
    let outside_file = outside.join("outside.txt");
    let script = r#"
        printf inside > "$INSIDE"
        cat "$OUTSIDE/readable.txt" > /dev/null || exit 42
        printf read-ok
        printf outside > "$OUTSIDE/outside.txt"
    "#;
    let output = run(
        manager,
        script,
        workspace,
        vec![
            ("INSIDE", inside.clone().into_os_string()),
            ("OUTSIDE", outside.as_os_str().to_owned()),
        ],
    )
    .await;

    assert!(
        inside.exists(),
        "工作区内写入应成功；stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("read-ok"),
        "工作区外读取应成功；stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.status.success() && output.status.code() != Some(42),
        "越界写入应失败（42 表示读取失败）；stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!outside_file.exists(), "宿主上的越界文件不应被创建");
}

#[tokio::test]
async fn bubblewrap_allows_workspace_writes_and_blocks_outside_writes() {
    let workspace = tempdir("kiana-sandbox-bwrap-ws-");
    let outside = tempdir("kiana-sandbox-bwrap-out-");
    let policy = SandboxPolicy::workspace_write(workspace.path()).with_network_access(true);
    let manager = SandboxManager::with_type(SandboxType::LinuxBubblewrap, policy, workspace.path());
    let Ok(manager) = manager else {
        eprintln!("skipping bubblewrap test: backend unavailable");
        return;
    };
    assert_write_is_blocked(&manager, workspace.path(), outside.path()).await;
}

#[tokio::test]
async fn landlock_allows_workspace_writes_and_blocks_outside_writes() {
    let workspace = tempdir("kiana-sandbox-landlock-ws-");
    let outside = tempdir("kiana-sandbox-landlock-out-");
    let policy = SandboxPolicy::workspace_write(workspace.path()).with_network_access(true);
    let manager = SandboxManager::with_type(SandboxType::LinuxLandlock, policy, workspace.path());
    let Ok(manager) = manager else {
        eprintln!("skipping landlock test: backend unavailable");
        return;
    };
    assert_write_is_blocked(&manager, workspace.path(), outside.path()).await;
}

#[tokio::test]
async fn bubblewrap_read_only_blocks_workspace_writes_but_allows_private_tmp() {
    let workspace = tempdir("kiana-sandbox-readonly-");
    let policy = SandboxPolicy::read_only().with_network_access(true);
    let manager = SandboxManager::with_type(SandboxType::LinuxBubblewrap, policy, workspace.path());
    let Ok(manager) = manager else {
        eprintln!("skipping bubblewrap read-only test: backend unavailable");
        return;
    };

    let blocked = workspace.path().join("blocked.txt");
    let script = r#"
        printf t > /tmp/kiana-sandbox-ro-probe && cat /tmp/kiana-sandbox-ro-probe > /dev/null || exit 41
        printf tmp-ok
        printf x > "$BLOCKED"
    "#;
    let output = run(
        &manager,
        script,
        workspace.path(),
        vec![("BLOCKED", blocked.clone().into_os_string())],
    )
    .await;

    assert!(
        String::from_utf8_lossy(&output.stdout).contains("tmp-ok"),
        "私有 /tmp 应可写；stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.status.success() && output.status.code() != Some(41),
        "只读工作区写入应失败（41 表示 /tmp 写入失败）；stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!blocked.exists(), "宿主工作区不应出现文件");
}

#[tokio::test]
async fn landlock_blocks_tcp_connect_when_network_is_disabled() {
    if !Path::new("/bin/bash").exists() {
        eprintln!("skipping landlock network test: /bin/bash not found");
        return;
    }
    let workspace = tempdir("kiana-sandbox-landlock-net-");
    // 默认 workspace_write 的 network_access = false。
    let policy = SandboxPolicy::workspace_write(workspace.path());
    let manager = SandboxManager::with_type(SandboxType::LinuxLandlock, policy, workspace.path());
    let Ok(manager) = manager else {
        eprintln!("skipping landlock network test: ABI < 4 or backend unavailable");
        return;
    };

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let port = listener.local_addr().expect("local addr").port();
    let script = format!("exec 3<>/dev/tcp/127.0.0.1/{port}");
    let output = run_program(&manager, "/bin/bash", &script, workspace.path(), Vec::new()).await;
    assert!(
        !output.status.success(),
        "Landlock 应拒绝 TCP connect；stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn danger_full_access_is_a_passthrough() {
    let workspace = tempdir("kiana-sandbox-danger-ws-");
    let outside = tempdir("kiana-sandbox-danger-out-");
    let manager = SandboxManager::new(
        SandboxPolicy::danger_full_access(),
        workspace.path().to_path_buf(),
    );
    assert_eq!(manager.sandbox_type(), SandboxType::None);

    let outside_file = outside.path().join("outside.txt");
    let output = run(
        &manager,
        r#"printf outside > "$OUTSIDE/outside.txt""#,
        workspace.path(),
        vec![("OUTSIDE", outside.path().as_os_str().to_owned())],
    )
    .await;
    assert!(
        output.status.success(),
        "DangerFullAccess 应放行；stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(outside_file.exists());
}

#[tokio::test]
async fn spawn_uses_the_per_call_cwd_for_chdir() {
    // manager 构造时绑定一个目录，命令却要求在另一个目录执行（bash 的 spawn hook /
    // 会话 cwd 变化会走这条路径）。
    let construction_dir = tempdir("kiana-sandbox-cwd-manager-");
    let command_dir = tempdir("kiana-sandbox-cwd-command-");
    let policy = SandboxPolicy::workspace_write(construction_dir.path()).with_network_access(true);
    let manager = SandboxManager::with_type(
        SandboxType::LinuxBubblewrap,
        policy,
        construction_dir.path(),
    );
    let Ok(manager) = manager else {
        eprintln!("skipping per-call cwd test: bubblewrap unavailable");
        return;
    };

    let output = run(&manager, "pwd", command_dir.path(), Vec::new()).await;
    assert!(
        output.status.success(),
        "pwd 应成功；stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let printed = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        printed.trim(),
        command_dir.path().to_string_lossy(),
        "命令应在 SpawnOptions::cwd 指定的目录执行"
    );
}

#[test]
fn auto_detection_picks_a_backend_when_available() {
    // `KIANA_SANDBOX=none` 是文档推荐的显式关闭方式；此时 auto 行为就是「不沙箱」，
    // 不该拿它去断言必然选中后端。
    if std::env::var("KIANA_SANDBOX").is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "none" | "off" | "disabled" | "false"
        )
    }) {
        eprintln!("skipping auto-detection test: KIANA_SANDBOX disables sandboxing");
        return;
    }
    let workspace = tempdir("kiana-sandbox-auto-");
    let policy = SandboxPolicy::workspace_write(workspace.path());
    let manager = SandboxManager::new(policy, workspace.path());
    if !manager.is_available() {
        eprintln!("no sandbox backend on this host");
        return;
    }
    assert!(
        manager.sandbox_type().is_sandboxed(),
        "available manager should report a sandboxed backend"
    );
}
