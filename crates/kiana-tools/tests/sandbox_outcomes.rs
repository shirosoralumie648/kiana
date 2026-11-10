//! 第 2 周：沙箱结果状态在工具层的表现。
//!
//! - 后端不可用（非法策略 / fail-closed manager）时命令完全不执行，错误文本是
//!   `BackendUnavailable`，绝不静默回退到宿主；
//! - relay 在命令启动后被 kill 对应 `Unconfirmed`，工具提示「可能已经执行、不要自动
//!   重试」，并且不会自动重跑；
//! - 超时/中止沿用旧字符串协议，但在 relay 已启动命令时追加 unconfirmed 提示。
//!
//! relay 级的真实 kill 测试在 `kiana-sandbox/tests/execution_policy.rs`
//! （`relay_killed_after_start_is_unconfirmed`）；这里验证工具层如何消费这些状态。

mod file_tools_test_support;

use std::path::Path;
use std::sync::{Arc, Mutex};

use file_tools_test_support::{TestContext, execute};
use indexmap::IndexMap;
use kiana_agent_core::types::BoxFuture;
use kiana_ext_api::tool_result::{BashExecOptions, BashExecResult, BashOperations};
use kiana_sandbox::ExecutionStatus;
use kiana_tools::bash::{
    BashToolOptions, UNCONFIRMED_COMMAND_NOTE, create_bash_tool_definition,
    create_local_bash_operations_from_env,
};

fn tempdir(prefix: &str) -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(base).expect("target tmp");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(base)
        .expect("tempdir")
}

fn shell_env() -> IndexMap<String, String> {
    let mut env = IndexMap::new();
    env.insert(
        "PATH".to_string(),
        std::env::var("PATH").unwrap_or_default(),
    );
    env
}

/// 直接复刻本地执行器的错误字符串协议。
struct ProtocolOperations(&'static str);

impl BashOperations for ProtocolOperations {
    fn exec<'a>(
        &'a self,
        _command: &'a str,
        _cwd: &'a str,
        _options: BashExecOptions<'a>,
    ) -> BoxFuture<'a, Result<BashExecResult, Box<dyn std::error::Error + Send + Sync>>> {
        let message = self.0;
        Box::pin(async move {
            Err(Box::new(std::io::Error::other(message))
                as Box<dyn std::error::Error + Send + Sync>)
        })
    }
}

async fn run_bash_protocol(message: &'static str) -> String {
    let workspace = tempdir("kiana-status-protocol-");
    let definition = create_bash_tool_definition(
        workspace.path().to_string_lossy(),
        Some(BashToolOptions {
            operations: Some(Arc::new(ProtocolOperations(message))),
            ..BashToolOptions::default()
        }),
    );
    let ctx = TestContext::new(workspace.path());
    execute(&definition, serde_json::json!({ "command": "noop" }), &ctx)
        .await
        .expect_err("protocol error")
        .message
}

#[test]
fn unconfirmed_status_is_never_retried_automatically() {
    let unconfirmed = ExecutionStatus::Unconfirmed {
        reason: "relay killed".to_string(),
    };
    assert!(unconfirmed.may_have_executed());
    assert!(!unconfirmed.allows_automatic_retry());
    let message = unconfirmed.tool_message();
    assert!(message.contains("Unconfirmed"), "{message}");
    assert!(message.contains("may have already executed"), "{message}");
    assert!(message.contains("Do not retry automatically"), "{message}");

    let unavailable = ExecutionStatus::BackendUnavailable {
        reason: "no bwrap".to_string(),
    };
    assert!(!unavailable.may_have_executed());
    assert!(unavailable.allows_automatic_retry());
    assert!(unavailable.tool_message().contains("was not executed"));
}

#[test]
fn the_tool_note_matches_the_sandbox_contract() {
    assert!(UNCONFIRMED_COMMAND_NOTE.contains("may have already executed"));
    assert!(UNCONFIRMED_COMMAND_NOTE.contains("Do not retry it automatically"));
}

#[tokio::test]
async fn timeout_after_the_command_started_warns_about_unconfirmed_execution() {
    let message = run_bash_protocol("timeout:5|unconfirmed").await;
    assert!(
        message.contains("Command timed out after 5 seconds"),
        "{message}"
    );
    assert!(
        message.contains("Do not retry it automatically"),
        "{message}"
    );
    assert!(message.contains("may have already executed"), "{message}");
}

#[tokio::test]
async fn abort_after_the_command_started_warns_about_unconfirmed_execution() {
    let message = run_bash_protocol("aborted|unconfirmed").await;
    assert!(message.contains("Command aborted"), "{message}");
    assert!(
        message.contains("Do not retry it automatically"),
        "{message}"
    );
}

#[tokio::test]
async fn plain_timeout_and_abort_messages_are_unchanged() {
    let timeout = run_bash_protocol("timeout:2").await;
    assert_eq!(timeout, "Command timed out after 2 seconds");
    let aborted = run_bash_protocol("aborted").await;
    assert_eq!(aborted, "Command aborted");
}

/// `KIANA_SANDBOX_POLICY` 是进程级全局状态；本文件内的环境变量测试必须串行。
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[tokio::test]
async fn invalid_sandbox_policy_fails_closed_without_running_the_command() {
    let workspace = tempdir("kiana-status-unavailable-");
    let marker = workspace.path().join("must-not-exist.txt");

    // 环境变量与构造在同一个临界区内完成；exec 不再读取环境，锁可以在 await 前释放。
    let operations = {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_policy = std::env::var_os("KIANA_SANDBOX_POLICY");
        let previous_sandbox = std::env::var_os("KIANA_SANDBOX");
        // SAFETY: 本测试持有 ENV_LOCK；本测试二进制里只有这里读写这两个变量。
        unsafe {
            std::env::set_var("KIANA_SANDBOX_POLICY", "this-is-not-a-policy");
            std::env::remove_var("KIANA_SANDBOX");
        }
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

    let output = Mutex::new(Vec::<u8>::new());
    let on_data = |chunk: &[u8]| {
        output.lock().expect("output").extend_from_slice(chunk);
    };
    let result = operations
        .exec(
            &format!("printf ran > {}", marker.display()),
            workspace.path().to_str().expect("utf8"),
            BashExecOptions {
                on_data: &on_data,
                signal: None,
                timeout: None,
                env: Some(shell_env()),
            },
        )
        .await;

    let error = result.expect_err("fail-closed backend must reject");
    let message = error.to_string();
    assert!(message.contains("BackendUnavailable"), "{message}");
    assert!(message.contains("was not executed"), "{message}");
    assert!(!marker.exists(), "the command must not have run");
}
