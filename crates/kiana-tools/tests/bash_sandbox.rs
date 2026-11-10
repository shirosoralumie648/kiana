//! bash 执行器与 `kiana-sandbox` 的接线测试。
//!
//! 验证 `BashToolOptions.sandbox` 这条路径：命令真的经过沙箱启动，工作区可写、
//! 工作区外不可写。后端不可用时跳过。

use std::path::Path;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use kiana_ext_api::tool_result::{BashExecOptions, BashExecutionStatus};
use kiana_sandbox::{SandboxManager, SandboxPolicy, SandboxType};
use kiana_tools::bash::{BashOperations, create_local_shell_operations_with_sandbox};

fn tempdir(prefix: &str) -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(base).expect("create target tmp");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(base)
        .expect("tempdir")
}

fn available_manager(policy: SandboxPolicy, cwd: &Path) -> Option<Arc<SandboxManager>> {
    SandboxManager::with_type(SandboxType::LinuxBubblewrap, policy.clone(), cwd)
        .or_else(|_| SandboxManager::with_type(SandboxType::LinuxLandlock, policy, cwd))
        .ok()
        .map(Arc::new)
}

#[tokio::test]
async fn local_shell_operations_use_the_sandbox() {
    let workspace = tempdir("kiana-tools-sandbox-ws-");
    let outside = tempdir("kiana-tools-sandbox-out-");
    let policy = SandboxPolicy::workspace_write(workspace.path()).with_network_access(true);
    let Some(manager) = available_manager(policy, workspace.path()) else {
        eprintln!("skipping bash sandbox test: backend unavailable");
        return;
    };

    let operations = create_local_shell_operations_with_sandbox(
        "bash",
        Arc::new(|| kiana_core::shell::get_shell_config(None)),
        Some(manager),
    );

    let inside = workspace.path().join("inside.txt");
    let outside_file = outside.path().join("outside.txt");
    let mut env = IndexMap::new();
    env.insert(
        "PATH".to_string(),
        std::env::var("PATH").unwrap_or_default(),
    );
    env.insert("INSIDE".to_string(), inside.display().to_string());
    env.insert("OUTSIDE".to_string(), outside.path().display().to_string());

    let output = Mutex::new(Vec::<u8>::new());
    let on_data = |chunk: &[u8]| {
        output.lock().expect("output lock").extend_from_slice(chunk);
    };
    let result = operations
        .exec(
            r#"printf inside > "$INSIDE"; printf outside > "$OUTSIDE/outside.txt""#,
            workspace.path().to_str().expect("utf8 workspace"),
            BashExecOptions {
                on_data: &on_data,
                signal: None,
                timeout: None,
                env: Some(env),
            },
        )
        .await
        .expect("exec");

    assert_ne!(
        result.exit_code,
        Some(0),
        "越界写入应让 shell 失败；output={}",
        String::from_utf8_lossy(&output.lock().expect("output lock"))
    );
    assert!(inside.exists(), "工作区内写入应成功");
    assert!(!outside_file.exists(), "工作区外写入不应落盘");
}

/// 配置了 manager 就一律走 relay 分支；显式关闭隔离（`SandboxType::None`）时 relay
/// 仍然会写 `started` / `exit <code>`，因此这里不依赖 bubblewrap/Landlock。
#[cfg(unix)]
#[tokio::test]
async fn sandbox_relay_reports_confirmed_exit_status() {
    let workspace = tempdir("kiana-tools-confirmed-");
    let manager = Arc::new(
        SandboxManager::with_type(
            SandboxType::None,
            SandboxPolicy::danger_full_access(),
            workspace.path(),
        )
        .expect("explicitly disabled manager"),
    );
    let operations = create_local_shell_operations_with_sandbox(
        "bash",
        Arc::new(|| kiana_core::shell::get_shell_config(None)),
        Some(manager),
    );

    let on_data = |_: &[u8]| {};
    let result = operations
        .exec(
            "exit 7",
            workspace.path().to_str().expect("utf8 workspace"),
            BashExecOptions {
                on_data: &on_data,
                signal: None,
                timeout: None,
                env: None,
            },
        )
        .await
        .expect("exec");

    assert_eq!(result.exit_code, Some(7));
    assert_eq!(
        result.execution_status,
        Some(BashExecutionStatus::ConfirmedExit { code: 7 })
    );
}

#[tokio::test]
async fn unsandboxed_execution_reports_unsandboxed_status() {
    let workspace = tempdir("kiana-tools-unsandboxed-");
    let operations = create_local_shell_operations_with_sandbox(
        "bash",
        Arc::new(|| kiana_core::shell::get_shell_config(None)),
        None,
    );

    let on_data = |_: &[u8]| {};
    let result = operations
        .exec(
            "exit 0",
            workspace.path().to_str().expect("utf8 workspace"),
            BashExecOptions {
                on_data: &on_data,
                signal: None,
                timeout: None,
                env: None,
            },
        )
        .await
        .expect("exec");

    assert_eq!(result.exit_code, Some(0));
    assert_eq!(
        result.execution_status,
        Some(BashExecutionStatus::Unsandboxed {
            code: Some(0),
            signal: None,
        })
    );
}
