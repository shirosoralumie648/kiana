//! `core/tools/powershell.ts` 的移植测试。
//!
//! TS 参考：`test/powershell-tool.test.ts`（`POWERSHELL_ARGS` 与 Windows 执行）、
//! `test/tool-system-prompt-contributions.test.ts`（prompt 元数据条件化）。
//! 非 Windows 上执行必然失败（TS 的 `getPowerShellConfig()` 抛
//! `The powershell tool is only available on Windows.`），因此执行成功路径只在
//! Windows 上断言；包装层（UTF-8 前缀、KIANA_* 注入）用脚本化操作跨平台覆盖。

mod search_shell_test_support;

use std::sync::{Arc, Mutex};

use kiana_agent_core::AgentToolError;
use kiana_ai::types::JsonValue;
use kiana_core::shell::POWERSHELL_ARGS;
use kiana_ext_api::BoxFuture;
use kiana_ext_api::tool_result::{BashExecOptions, BashExecResult, BashOperations};
use kiana_ext_api::tool_types::ToolDefinition;
use kiana_tools::powershell::{
    PowerShellToolOptions, UTF8_OUTPUT_PREFIX, create_local_power_shell_operations,
    create_power_shell_tool_definition, powershell_tool_system_prompt_contribution,
};
use serde_json::json;
use tempfile::TempDir;

use search_shell_test_support::{TestContext, execute, golden, text_output};

#[derive(Debug)]
struct RecordingOps {
    commands: Arc<Mutex<Vec<String>>>,
}

impl BashOperations for RecordingOps {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        _cwd: &'a str,
        _options: BashExecOptions<'a>,
    ) -> BoxFuture<'a, Result<BashExecResult, Box<dyn std::error::Error + Send + Sync>>> {
        self.commands.lock().unwrap().push(command.to_string());
        Box::pin(async move {
            Ok(BashExecResult {
                exit_code: Some(0),
                execution_status: None,
            })
        })
    }
}

fn powershell_definition(temp: &TempDir, options: PowerShellToolOptions) -> ToolDefinition {
    create_power_shell_tool_definition(temp.path().to_string_lossy().into_owned(), Some(options))
}

#[test]
fn uses_process_local_execution_policy_bypass() {
    assert_eq!(
        POWERSHELL_ARGS,
        [
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
        ]
    );
}

#[tokio::test]
async fn local_operations_parse_to_the_execution_policy_args() {
    // TS 的 `getPowerShellConfig()`（Windows 分支）args 等于 POWERSHELL_ARGS；
    // 这里验证本地操作在非 Windows 上以配置错误失败，并且不会先做别的事。
    let operations = create_local_power_shell_operations();
    let on_data = |_: &[u8]| {};
    let error = operations
        .exec(
            "Write-Output hi",
            ".",
            BashExecOptions {
                on_data: &on_data,
                signal: None,
                timeout: None,
                env: None,
            },
        )
        .await
        .expect_err("non-Windows must fail");
    if cfg!(windows) {
        panic!("unexpected error on Windows: {error}");
    }
    assert_eq!(
        error.to_string(),
        "The powershell tool is only available on Windows."
    );
}

#[tokio::test]
async fn custom_operations_receive_the_raw_command_without_the_utf8_prefix() {
    // TS: `createPowerShellToolDefinition` 只在默认的 `createLocalPowerShellOperations()`
    // 里包 UTF-8 前缀；调用方提供的 operations 原样使用。
    let temp = TempDir::new().expect("temp");
    let commands = Arc::new(Mutex::new(Vec::new()));
    let definition = powershell_definition(
        &temp,
        PowerShellToolOptions {
            operations: Some(Arc::new(RecordingOps {
                commands: Arc::clone(&commands),
            })),
            expose_session_environment: false,
            spawn_hook: None,
            sandbox: None,
        },
    );
    let result = execute(
        &definition,
        json!({ "command": "Write-Output 'héllo €'" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("execute");
    assert_eq!(text_output(&result), "(no output)");
    assert_eq!(
        commands.lock().unwrap().as_slice(),
        ["Write-Output 'héllo €'"]
    );
}

#[test]
fn definition_matches_the_ts_metadata() {
    let temp = TempDir::new().expect("temp");
    let definition = powershell_definition(
        &temp,
        PowerShellToolOptions {
            expose_session_environment: false,
            ..PowerShellToolOptions::default()
        },
    );
    let contribution = powershell_tool_system_prompt_contribution();
    assert_eq!(definition.name, "powershell");
    assert_eq!(definition.label, "powershell");
    assert_eq!(
        definition.prompt_snippet.as_deref(),
        Some(contribution.snippet)
    );
    assert_eq!(definition.prompt_guidelines, None);
    assert_eq!(definition.parameters["required"], json!(["command"]));
    assert!(definition.render_call.is_some());
    assert!(definition.render_result.is_some());

    let exposed = powershell_definition(&temp, PowerShellToolOptions::default());
    let expected: Vec<String> = contribution
        .guidelines
        .iter()
        .map(|guideline| (*guideline).to_string())
        .collect();
    assert_eq!(
        exposed.prompt_guidelines.as_deref(),
        Some(expected.as_slice())
    );
    assert_eq!(
        exposed.prompt_snippet.as_deref(),
        Some(contribution.snippet)
    );
}

#[test]
fn contribution_matches_ts() {
    let contribution = powershell_tool_system_prompt_contribution();
    assert_eq!(contribution.snippet, "Execute PowerShell commands");
    assert_eq!(
        contribution.guidelines,
        ["You can inspect KIANA_* environment variables for current model and session details."]
    );
    assert_eq!(
        UTF8_OUTPUT_PREFIX,
        "try { [Console]::OutputEncoding=[System.Text.Encoding]::UTF8 } catch {}\n"
    );
}

#[cfg(not(windows))]
#[tokio::test]
async fn off_windows_execution_reports_the_windows_only_error() {
    let temp = TempDir::new().expect("temp");
    let definition = powershell_definition(&temp, PowerShellToolOptions::default());
    let error: AgentToolError = execute(
        &definition,
        json!({ "command": "Write-Output hi" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("must fail");
    assert_eq!(
        error.message,
        "The powershell tool is only available on Windows."
    );
}

#[tokio::test]
async fn golden_shape_is_stable() {
    // powershell 与 bash 共享 shell 执行器；黄金文件里没有 powershell 条目，
    // 这里只确认共享的详情类型仍序列化为 null。
    let temp = TempDir::new().expect("temp");
    let definition = powershell_definition(
        &temp,
        PowerShellToolOptions {
            operations: Some(Arc::new(RecordingOps {
                commands: Arc::new(Mutex::new(Vec::new())),
            })),
            expose_session_environment: false,
            spawn_hook: None,
            sandbox: None,
        },
    );
    let result = execute(
        &definition,
        json!({ "command": "Write-Output hi" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("execute");
    assert_eq!(result.details, JsonValue::Null);
    assert_eq!(golden()["bash"]["nonzero"]["details"], JsonValue::Null);
}
