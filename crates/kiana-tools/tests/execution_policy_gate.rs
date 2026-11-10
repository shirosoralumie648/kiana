//! 第 2 周：文件工具与 shell 共用同一份执行策略的集成测试。
//!
//! 覆盖：
//! - workspace-write 下 write/edit 对工作区外路径、`.git`/`.kiana` 的拒绝；
//! - 符号链接（含悬空链接）不能逃逸可写根；
//! - read-only 拒绝所有写入；
//! - bwrap 可用时，shell 对同一路径得出与文件工具一致的结论；
//! - 多根 workspace-write。
//!
//! bwrap / Landlock 不可用时 shell 部分显式跳过并打印原因，不假装通过。

mod file_tools_test_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

use file_tools_test_support::{TestContext, execute};
use indexmap::IndexMap;
use kiana_ext_api::tool_result::BashExecOptions;
use kiana_ext_api::tool_types::ToolDefinition;
use kiana_sandbox::{ExecutionPolicy, SandboxManager, SandboxPolicy, SandboxType};
use kiana_tools::bash::{
    BashOperations, create_local_bash_operations_from_env,
    create_local_shell_operations_with_sandbox,
};
use kiana_tools::edit::{EditToolOptions, create_edit_tool_definition};
use kiana_tools::read::{ReadStateRegistry, ReadToolOptions, create_read_tool_definition};
use kiana_tools::write::{WriteToolOptions, create_write_tool_definition};
use serde_json::json;
use tempfile::TempDir;

fn tempdir(prefix: &str) -> TempDir {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
    fs::create_dir_all(base).expect("target tmp");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(base)
        .expect("tempdir")
}

struct Env {
    registry: Arc<ReadStateRegistry>,
    policy: Arc<ExecutionPolicy>,
    cwd: PathBuf,
}

impl Env {
    fn new(policy: SandboxPolicy, cwd: impl Into<PathBuf>) -> Self {
        let cwd = cwd.into();
        Self {
            registry: Arc::new(ReadStateRegistry::new()),
            policy: Arc::new(ExecutionPolicy::new(policy, cwd.clone())),
            cwd,
        }
    }

    fn write_definition(&self) -> ToolDefinition {
        create_write_tool_definition(
            self.cwd.to_string_lossy(),
            Some(WriteToolOptions {
                execution_policy: Some(Arc::clone(&self.policy)),
                read_state: Some(Arc::clone(&self.registry)),
                ..WriteToolOptions::default()
            }),
        )
    }

    fn edit_definition(&self) -> ToolDefinition {
        create_edit_tool_definition(
            self.cwd.to_string_lossy(),
            Some(EditToolOptions {
                execution_policy: Some(Arc::clone(&self.policy)),
                read_state: Some(Arc::clone(&self.registry)),
                ..EditToolOptions::default()
            }),
        )
    }

    fn read_definition(&self) -> ToolDefinition {
        create_read_tool_definition(
            self.cwd.to_string_lossy(),
            Some(ReadToolOptions {
                read_state: Some(Arc::clone(&self.registry)),
                ..ReadToolOptions::default()
            }),
        )
    }
}

fn write_params(path: &Path, content: &str) -> serde_json::Value {
    json!({ "path": path.to_string_lossy(), "content": content })
}

fn edit_params(path: &Path, old: &str, new: &str) -> serde_json::Value {
    json!({
        "path": path.to_string_lossy(),
        "edits": [{ "oldText": old, "newText": new }],
    })
}

#[tokio::test]
async fn workspace_write_denies_writes_outside_the_workspace_for_write_and_edit() {
    let workspace = tempdir("kiana-gate-ws-");
    let outside = tempdir("kiana-gate-out-");
    let env = Env::new(
        SandboxPolicy::workspace_write(workspace.path()),
        workspace.path(),
    );
    let ctx = TestContext::new(workspace.path());

    let outside_file = outside.path().join("evil.txt");
    let write_error = execute(
        &env.write_definition(),
        write_params(&outside_file, "x"),
        &ctx,
    )
    .await
    .expect_err("write outside workspace");
    assert!(
        write_error.message.contains("OUTSIDE_WORKSPACE"),
        "{}",
        write_error.message
    );
    assert!(!outside_file.exists());

    fs::write(&outside_file, "outside\n").expect("fixture");
    let edit_error = execute(
        &env.edit_definition(),
        edit_params(&outside_file, "outside", "mine"),
        &ctx,
    )
    .await
    .expect_err("edit outside workspace");
    assert!(
        edit_error.message.contains("OUTSIDE_WORKSPACE"),
        "{}",
        edit_error.message
    );
    assert_eq!(
        fs::read_to_string(&outside_file).expect("read"),
        "outside\n"
    );

    // 工作区内的新文件不需要读记录。
    let inside = workspace.path().join("nested").join("ok.txt");
    execute(&env.write_definition(), write_params(&inside, "ok"), &ctx)
        .await
        .expect("write inside workspace");
    assert_eq!(fs::read_to_string(&inside).expect("read"), "ok");
}

#[tokio::test]
async fn git_and_kiana_are_protected_under_workspace_write() {
    let workspace = tempdir("kiana-gate-protected-");
    let env = Env::new(
        SandboxPolicy::workspace_write(workspace.path()),
        workspace.path(),
    );
    let ctx = TestContext::new(workspace.path());
    fs::create_dir_all(workspace.path().join(".git")).expect("mkdir .git");
    fs::create_dir_all(workspace.path().join(".kiana")).expect("mkdir .kiana");

    for relative in [
        ".git/config",
        ".git/hooks/post-checkout",
        ".kiana/settings.json",
    ] {
        let path = workspace.path().join(relative);
        let error = execute(&env.write_definition(), write_params(&path, "x"), &ctx)
            .await
            .expect_err("protected path");
        assert!(
            error.message.contains("PROTECTED_PATH"),
            "{relative}: {}",
            error.message
        );
        assert!(!path.exists(), "{relative} must not be created");
    }

    // 受保护目录的兄弟路径仍然可写。
    let sibling = workspace.path().join(".gitignore");
    execute(
        &env.write_definition(),
        write_params(&sibling, "target\n"),
        &ctx,
    )
    .await
    .expect("sibling path");
    assert!(sibling.exists());

    // edit 与 write 共用同一层门禁：受保护路径无论文件是否存在都必须拒绝。
    for relative in [".git/config", ".kiana/settings.json"] {
        let path = workspace.path().join(relative);
        fs::write(&path, "original\n").expect("seed protected file");
        let error = execute(
            &env.edit_definition(),
            edit_params(&path, "original", "mine"),
            &ctx,
        )
        .await
        .expect_err("edit on a protected path");
        assert!(
            error.message.contains("PROTECTED_PATH"),
            "{relative}: {}",
            error.message
        );
        assert_eq!(
            fs::read_to_string(&path).expect("read"),
            "original\n",
            "{relative} must stay untouched"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn dangling_symlinks_cannot_escape_the_workspace() {
    use std::os::unix::fs::symlink;

    let workspace = tempdir("kiana-gate-link-ws-");
    let outside = tempdir("kiana-gate-link-out-");
    let env = Env::new(
        SandboxPolicy::workspace_write(workspace.path()),
        workspace.path(),
    );
    let ctx = TestContext::new(workspace.path());

    let link = workspace.path().join("escape");
    symlink(outside.path().join("new.txt"), &link).expect("symlink");
    let error = execute(&env.write_definition(), write_params(&link, "x"), &ctx)
        .await
        .expect_err("dangling symlink escape");
    assert!(
        error.message.contains("OUTSIDE_WORKSPACE"),
        "{}",
        error.message
    );
    assert!(!outside.path().join("new.txt").exists());
}

#[tokio::test]
async fn read_only_rejects_every_file_write() {
    let workspace = tempdir("kiana-gate-ro-");
    let env = Env::new(SandboxPolicy::read_only(), workspace.path());
    let ctx = TestContext::new(workspace.path());
    let path = workspace.path().join("blocked.txt");
    let error = execute(&env.write_definition(), write_params(&path, "x"), &ctx)
        .await
        .expect_err("read-only");
    assert!(error.message.contains("READ_ONLY"), "{}", error.message);
    assert!(!path.exists());
}

#[tokio::test]
async fn multiple_writable_roots_are_all_honored() {
    let root_a = tempdir("kiana-gate-root-a-");
    let root_b = tempdir("kiana-gate-root-b-");
    let outside = tempdir("kiana-gate-root-out-");
    let policy = SandboxPolicy::workspace_write_with_roots(vec![
        root_a.path().to_path_buf(),
        root_b.path().to_path_buf(),
    ]);
    let env = Env::new(policy, root_a.path());
    let ctx = TestContext::new(root_a.path());

    for path in [
        root_a.path().join("a.txt"),
        root_b.path().join("nested").join("b.txt"),
    ] {
        execute(&env.write_definition(), write_params(&path, "ok"), &ctx)
            .await
            .unwrap_or_else(|error| {
                panic!("{} should be writable: {}", path.display(), error.message)
            });
        assert!(path.exists());
    }

    let outside_file = outside.path().join("no.txt");
    let error = execute(
        &env.write_definition(),
        write_params(&outside_file, "x"),
        &ctx,
    )
    .await
    .expect_err("outside all roots");
    assert!(
        error.message.contains("OUTSIDE_WORKSPACE"),
        "{}",
        error.message
    );
}

fn available_bwrap(policy: SandboxPolicy, cwd: &Path) -> Option<Arc<SandboxManager>> {
    SandboxManager::with_type(SandboxType::LinuxBubblewrap, policy, cwd)
        .ok()
        .map(Arc::new)
}

async fn run_shell(
    operations: &Arc<dyn BashOperations>,
    command: &str,
    cwd: &Path,
) -> (Option<i32>, String) {
    let output = Mutex::new(Vec::<u8>::new());
    let on_data = |chunk: &[u8]| {
        output.lock().expect("output").extend_from_slice(chunk);
    };
    let mut env = IndexMap::new();
    env.insert(
        "PATH".to_string(),
        std::env::var("PATH").unwrap_or_default(),
    );
    let result = operations
        .exec(
            command,
            cwd.to_str().expect("utf8 cwd"),
            BashExecOptions {
                on_data: &on_data,
                signal: None,
                timeout: None,
                env: Some(env),
            },
        )
        .await;
    let text = String::from_utf8_lossy(&output.lock().expect("output")).into_owned();
    match result {
        Ok(result) => (result.exit_code, text),
        Err(error) => (None, format!("{text}\nERROR: {error}")),
    }
}

#[tokio::test]
async fn shell_and_file_tools_agree_under_workspace_write() {
    let workspace = tempdir("kiana-gate-shell-ws-");
    let second_root = tempdir("kiana-gate-shell-root-b-");
    let outside = tempdir("kiana-gate-shell-out-");
    let roots = vec![
        workspace.path().to_path_buf(),
        second_root.path().to_path_buf(),
    ];
    let policy = SandboxPolicy::workspace_write_with_roots(roots.clone()).with_network_access(true);
    let Some(manager) = available_bwrap(policy, workspace.path()) else {
        eprintln!("skipping shell/file consistency test: bubblewrap backend unavailable");
        return;
    };
    fs::create_dir_all(workspace.path().join(".git")).expect("mkdir .git");

    let operations = create_local_shell_operations_with_sandbox(
        "bash",
        Arc::new(|| kiana_core::shell::get_shell_config(None)),
        Some(manager),
    );
    let outside_file = outside.path().join("shell-outside.txt");
    let protected = workspace.path().join(".git").join("config");
    let inside = workspace.path().join("shell-inside.txt");
    let second = second_root.path().join("shell-second-root.txt");
    let command = format!(
        "printf inside > '{inside}'; printf second > '{second}'; \
         printf outside > '{outside}'; printf git > '{git}'",
        inside = inside.display(),
        second = second.display(),
        outside = outside_file.display(),
        git = protected.display(),
    );
    let (exit_code, output) = run_shell(&operations, &command, workspace.path()).await;

    let exit_code = exit_code
        .unwrap_or_else(|| panic!("sandboxed shell failed to produce an exit status: {output}"));
    assert_ne!(exit_code, 0, "outside/.git writes must fail: {output}");
    assert!(inside.exists(), "workspace write should succeed");
    assert!(second.exists(), "second writable root should succeed");
    assert!(!outside_file.exists(), "outside write must be blocked");
    assert!(!protected.exists(), ".git write must be blocked");

    // 文件工具对同一批路径给出相同结论（拒绝），且拒绝码稳定。
    let env = Env::new(
        SandboxPolicy::workspace_write_with_roots(roots),
        workspace.path(),
    );
    let ctx = TestContext::new(workspace.path());
    for path in [&outside_file, &protected] {
        let error = execute(&env.write_definition(), write_params(path, "x"), &ctx)
            .await
            .expect_err("file tool must deny the same path");
        assert!(
            error.message.contains("OUTSIDE_WORKSPACE") || error.message.contains("PROTECTED_PATH"),
            "{}",
            error.message
        );
    }
}

#[tokio::test]
async fn powershell_named_shell_obeys_the_same_workspace_policy() {
    // PowerShell 与 bash 共用同一个执行器；在 Unix 上用 bash 的 shell 配置驱动它，
    // 只为验证 `execution_tool_name` 把 powershell 也映射到同一条执行策略。
    let workspace = tempdir("kiana-gate-ps-ws-");
    let outside = tempdir("kiana-gate-ps-out-");
    let policy = SandboxPolicy::workspace_write(workspace.path()).with_network_access(true);
    let Some(manager) = available_bwrap(policy, workspace.path()) else {
        eprintln!("skipping powershell consistency test: bubblewrap backend unavailable");
        return;
    };

    let operations = create_local_shell_operations_with_sandbox(
        "PowerShell",
        Arc::new(|| kiana_core::shell::get_shell_config(None)),
        Some(manager),
    );
    let inside = workspace.path().join("powershell-inside.txt");
    let outside_file = outside.path().join("powershell-outside.txt");
    let command = format!(
        "printf inside > '{}'; printf outside > '{}'",
        inside.display(),
        outside_file.display()
    );
    let (exit_code, output) = run_shell(&operations, &command, workspace.path()).await;

    let exit_code = exit_code
        .unwrap_or_else(|| panic!("powershell-named shell failed to produce a status: {output}"));
    assert_ne!(exit_code, 0, "outside write must fail: {output}");
    assert!(inside.exists(), "workspace write should succeed");
    assert!(!outside_file.exists(), "outside write must be blocked");
}

/// `execute_bash`（`AgentSession` 的直接执行路径）通过
/// [`create_local_bash_operations_from_env`] 进入同一份 `KIANA_SANDBOX_POLICY`；
/// 环境变量是进程级状态，本测试与同二进制内的其它环境测试必须串行。
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[tokio::test]
async fn execute_bash_entry_point_uses_the_same_workspace_write_policy() {
    let workspace = tempdir("kiana-gate-execute-bash-ws-");
    let outside = tempdir("kiana-gate-execute-bash-out-");

    // 环境变量与构造必须在同一个临界区内完成；拿到 operations 后就可以释放锁，
    // 因为 exec 不再读取环境（避免 MutexGuard 跨 await）。
    let operations = {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // 只有真实后端可用时才断言隔离结论。后端不可用时 `from_env` 会构造 fail-closed
        // manager（结果同样安全，但测不到「工作区可写/外面不可写」），显式跳过。
        let probe = SandboxManager::with_type(
            SandboxType::LinuxBubblewrap,
            SandboxPolicy::workspace_write(workspace.path()),
            workspace.path(),
        );
        if probe.is_err() {
            eprintln!("skipping execute_bash env test: bubblewrap backend unavailable");
            return;
        }

        let previous_policy = std::env::var_os("KIANA_SANDBOX_POLICY");
        let previous_sandbox = std::env::var_os("KIANA_SANDBOX");
        // SAFETY: 本测试持有 ENV_LOCK；本测试二进制里只有环境测试读写这两个变量。
        unsafe {
            std::env::set_var("KIANA_SANDBOX_POLICY", "workspace-write");
            std::env::remove_var("KIANA_SANDBOX");
        }
        // execute_bash 的入口在构造时就解析策略（与 bash 工具的延迟解析不同），
        // 因此必须在恢复环境变量之前构造。
        let operations = create_local_bash_operations_from_env(None, workspace.path());
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
        operations
    };

    let inside = workspace.path().join("execute-bash-inside.txt");
    let outside_file = outside.path().join("execute-bash-outside.txt");
    let command = format!(
        "printf inside > '{}'; printf outside > '{}'",
        inside.display(),
        outside_file.display()
    );
    let (exit_code, output) = run_shell(&operations, &command, workspace.path()).await;

    let exit_code = exit_code
        .unwrap_or_else(|| panic!("execute_bash path failed to produce a status: {output}"));
    assert_ne!(exit_code, 0, "outside write must fail: {output}");
    assert!(inside.exists(), "workspace write should succeed");
    assert!(!outside_file.exists(), "outside write must be blocked");
}

#[tokio::test]
async fn read_tool_sees_the_same_realpath_that_the_gate_checks() {
    // 符号链接指向工作区内文件：read 记录（按真实路径）应能解锁通过链接路径的 edit。
    let workspace = tempdir("kiana-gate-read-link-");
    let env = Env::new(
        SandboxPolicy::workspace_write(workspace.path()),
        workspace.path(),
    );
    let ctx = TestContext::new(workspace.path());
    let real = workspace.path().join("real.txt");
    fs::write(&real, "hello\n").expect("fixture");

    execute(
        &env.read_definition(),
        json!({ "path": real.to_string_lossy() }),
        &ctx,
    )
    .await
    .expect("read");

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let link = workspace.path().join("alias.txt");
        symlink(&real, &link).expect("symlink");
        execute(
            &env.edit_definition(),
            edit_params(&link, "hello", "bye"),
            &ctx,
        )
        .await
        .expect("edit through an in-workspace symlink");
        assert_eq!(fs::read_to_string(&real).expect("read"), "bye\n");
    }
}
