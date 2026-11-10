//! `core/tools/bash.ts` 的移植测试。
//!
//! 覆盖 `tools.test.ts` 的 bash 段、`tool-execution-component.test.ts` 的首个空更新、
//! `agent-session-dynamic-tools.test.ts` 的 KIANA_* 注入，以及
//! `5208-late-bash-output.test.ts` 能安全复现的部分。
//!
//! 需要真实 shell 的用例用 `/bin/sh`/`/bin/bash`；其余用脚本化的 [`BashOperations`]，
//! 这样节流、脚注、structuredContent 都是确定性的。黄金文件由 TS 参考生成
//! （`tests/fixtures/search-shell/behavior.golden.json`）。

mod search_shell_test_support;

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use indexmap::IndexMap;
use kiana_agent_core::{AgentToolError, AgentToolUpdateCallback, ThinkingLevel};
use kiana_ai::types::JsonValue;
use kiana_core::shell::{CommandTransport, ShellConfig};
use kiana_ext_api::BoxFuture;
use kiana_ext_api::tool_result::{BashExecOptions, BashExecResult, BashOperations};
use kiana_ext_api::tool_types::ToolDefinition;
use kiana_session_core::session_manager::SessionManager;
use kiana_tools::bash::{
    BashSpawnContext, BashToolOptions, ShellToolConfig, create_bash_tool_definition,
    create_local_bash_operations, create_local_shell_operations, create_shell_tool_definition,
};
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use search_shell_test_support::{
    TestContext, execute, execute_with_update, golden, test_model, text_output,
};

// ===========================================================================
// 脚本化操作
// ===========================================================================

/// 脚本化操作抛出的错误。
#[derive(Debug)]
struct ScriptError(String);

impl std::fmt::Display for ScriptError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ScriptError {}

type ScriptFn =
    dyn Fn(&str, &str, &(dyn Fn(&[u8]) + Send + Sync)) -> Result<Option<i32>, String> + Send + Sync;

struct ScriptedOps {
    script: Arc<ScriptFn>,
}

impl BashOperations for ScriptedOps {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a str,
        options: BashExecOptions<'a>,
    ) -> BoxFuture<'a, Result<BashExecResult, Box<dyn std::error::Error + Send + Sync>>> {
        Box::pin(async move {
            (self.script)(command, cwd, options.on_data)
                .map(|exit_code| BashExecResult {
                    exit_code,
                    execution_status: None,
                })
                .map_err(|message| -> Box<dyn std::error::Error + Send + Sync> {
                    Box::new(ScriptError(message))
                })
        })
    }
}

fn scripted<F>(script: F) -> Arc<dyn BashOperations>
where
    F: Fn(&str, &str, &(dyn Fn(&[u8]) + Send + Sync)) -> Result<Option<i32>, String>
        + Send
        + Sync
        + 'static,
{
    Arc::new(ScriptedOps {
        script: Arc::new(script),
    })
}

fn bash_definition(cwd: &std::path::Path, operations: Arc<dyn BashOperations>) -> ToolDefinition {
    create_bash_tool_definition(
        cwd.to_string_lossy().into_owned(),
        Some(BashToolOptions {
            operations: Some(operations),
            expose_session_environment: false,
            ..BashToolOptions::default()
        }),
    )
}

fn real_bash_definition(cwd: &std::path::Path) -> ToolDefinition {
    create_bash_tool_definition(
        cwd.to_string_lossy().into_owned(),
        Some(BashToolOptions {
            expose_session_environment: false,
            ..BashToolOptions::default()
        }),
    )
}

/// 把临时文件名归一为 `kiana-bash-<hex>.log`（黄金文件里的占位符）。
fn normalize_temp_paths(text: &str) -> String {
    let mut output = String::new();
    let mut rest = text;
    while let Some(position) = rest.find("kiana-bash-") {
        output.push_str(&rest[..position]);
        let after = &rest[position + "kiana-bash-".len()..];
        if let Some(dot) = after.find(".log") {
            let candidate = &after[..dot];
            if candidate.len() == 16
                && candidate
                    .chars()
                    .all(|character| character.is_ascii_hexdigit())
            {
                output.push_str("kiana-bash-<hex>.log");
                rest = &after[dot + ".log".len()..];
                continue;
            }
        }
        output.push_str("kiana-bash-");
        rest = after;
    }
    output.push_str(rest);
    output
}

/// 把黄金文件里被裁剪过的 details 还原成可比形状（内容/路径换成占位符）。
fn normalize_details(value: &JsonValue) -> JsonValue {
    let mut value = value.clone();
    if let Some(object) = value.as_object_mut() {
        if object.contains_key("fullOutputPath") {
            object.insert(
                "fullOutputPath".to_string(),
                JsonValue::String("<full-output-path>".to_string()),
            );
        }
        if let Some(truncation) = object
            .get_mut("truncation")
            .and_then(JsonValue::as_object_mut)
        {
            truncation.insert(
                "content".to_string(),
                JsonValue::String("<content omitted>".to_string()),
            );
        }
    }
    value
}

fn error_message(error: AgentToolError) -> String {
    error.message
}

// ===========================================================================
// 基础行为
// ===========================================================================

#[tokio::test]
async fn should_execute_simple_commands() {
    let temp = TempDir::new().expect("temp");
    let definition = real_bash_definition(temp.path());
    let result = execute(
        &definition,
        json!({ "command": "echo 'test output'" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("execute");

    assert!(text_output(&result).contains("test output"));
    assert_eq!(result.details, JsonValue::Null);
}

#[tokio::test]
async fn should_report_non_zero_exit_codes_as_error_results_with_structured_content() {
    let temp = TempDir::new().expect("temp");
    let ctx = TestContext::new(temp.path());
    let golden = golden();

    let operations = scripted(|_command, _cwd, on_data| {
        on_data(b"out\n");
        Ok(Some(3))
    });
    let definition = bash_definition(temp.path(), operations);
    let result = execute(&definition, json!({ "command": "x" }), &ctx)
        .await
        .expect("execute");

    assert_eq!(
        text_output(&result),
        golden["bash"]["nonzero"]["text"].as_str().unwrap()
    );
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.details, JsonValue::Null);
    let mut structured = result.structured_content.clone().expect("structured");
    structured["wall_time_seconds"] = JsonValue::String("<number>".to_string());
    assert_eq!(structured, golden["bash"]["nonzero"]["structuredContent"]);
    // 实测耗时只断言是数字；`0.0` 必须序列化成 `0` 的格式由 bash 模块的单元测试用
    // 构造值确定性覆盖（这里依赖实测值会在负载下跨越 50 ms 取整边界时偶发失败）。
    assert!(
        result.structured_content.as_ref().unwrap()["wall_time_seconds"].is_number(),
        "wall_time_seconds must be a number"
    );

    let ok_definition = bash_definition(
        temp.path(),
        scripted(|_command, _cwd, on_data| {
            on_data(b"fine\n");
            Ok(Some(0))
        }),
    );
    let ok = execute(&ok_definition, json!({ "command": "x" }), &ctx)
        .await
        .expect("execute");
    assert_eq!(ok.is_error, None);
    assert_eq!(ok.structured_content.as_ref().unwrap()["output"], "fine\n");
    assert_eq!(ok.structured_content.as_ref().unwrap()["exit_code"], 0);

    let empty_definition = bash_definition(
        temp.path(),
        scripted(|_command, _cwd, _on_data| Ok(Some(0))),
    );
    let empty = execute(&empty_definition, json!({ "command": "x" }), &ctx)
        .await
        .expect("execute");
    assert_eq!(text_output(&empty), "(no output)");
    assert_eq!(empty.structured_content.as_ref().unwrap()["output"], "");
    assert_eq!(
        empty.structured_content.as_ref().unwrap()["truncated"],
        false
    );
}

#[tokio::test]
async fn should_return_up_to_1_mib_of_output_in_structured_content() {
    let temp = TempDir::new().expect("temp");
    let ctx = TestContext::new(temp.path());
    let definition = real_bash_definition(temp.path());

    let medium = execute(&definition, json!({ "command": "seq 1 3000" }), &ctx)
        .await
        .expect("execute");
    assert!(!text_output(&medium).contains("\n1\n2\n"));
    assert_eq!(
        medium.details["truncation"]["truncated"],
        JsonValue::Bool(true)
    );
    let medium_structured = medium.structured_content.as_ref().unwrap();
    assert_eq!(medium_structured["truncated"], false);
    let expected: String = (1..=3000)
        .map(|value| format!("{value}\n"))
        .collect::<Vec<_>>()
        .join("");
    assert_eq!(medium_structured["output"], expected);

    let large = execute(&definition, json!({ "command": "seq 1 300000" }), &ctx)
        .await
        .expect("execute");
    let large_structured = large.structured_content.as_ref().unwrap();
    assert_eq!(large_structured["truncated"], true);
    let output = large_structured["output"].as_str().unwrap();
    assert!(output.starts_with("1\n2\n3\n"));
    assert!(output.ends_with("299999\n300000\n"));
    assert!(output.contains("\n\n[... "));
    assert!(output.contains(" bytes omitted ...]\n\n"));
    assert!(output.len() < 1024 * 1024 + 100);
    let full_output_path = large_structured["full_output_path"].as_str().expect("path");
    assert_eq!(
        large.details["fullOutputPath"].as_str(),
        Some(full_output_path)
    );
    let full = std::fs::read_to_string(full_output_path).expect("read full output");
    assert!(full.ends_with("300000\n"));
}

#[cfg(unix)]
#[tokio::test]
async fn should_map_signal_killed_commands_to_128_plus_the_signal_number() {
    let temp = TempDir::new().expect("temp");
    let operations = create_local_bash_operations(None);
    for (signal, exit_code) in [("KILL", 137), ("TERM", 143)] {
        let on_data = |_: &[u8]| {};
        let result = operations
            .exec(
                &format!("kill -{signal} $$"),
                &temp.path().to_string_lossy(),
                BashExecOptions {
                    on_data: &on_data,
                    signal: None,
                    timeout: None,
                    env: None,
                },
            )
            .await
            .expect("exec");
        assert_eq!(result.exit_code, Some(exit_code));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn should_report_signal_killed_commands_as_errors_while_preserving_partial_output() {
    let temp = TempDir::new().expect("temp");
    let ctx = TestContext::new(temp.path());
    let definition = real_bash_definition(temp.path());
    for (signal, exit_code) in [("KILL", 137), ("TERM", 143)] {
        let result = execute(
            &definition,
            json!({ "command": format!("printf 'before-kill\\n'; kill -{signal} $$") }),
            &ctx,
        )
        .await
        .expect("execute");
        assert_eq!(result.is_error, Some(true));
        let output = text_output(&result);
        assert!(output.starts_with("before-kill"));
        assert!(output.ends_with(&format!("Command exited with code {exit_code}")));
    }
}

#[tokio::test]
async fn should_reject_a_null_exit_code_from_custom_operations() {
    let temp = TempDir::new().expect("temp");
    let definition = bash_definition(temp.path(), scripted(|_command, _cwd, _on_data| Ok(None)));
    let error = execute(
        &definition,
        json!({ "command": "remote" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("must fail");
    assert_eq!(
        error_message(error),
        golden()["bash"]["null_exit"]["message"].as_str().unwrap()
    );

    // 有输出时输出保留在状态行之前。
    let definition = bash_definition(
        temp.path(),
        scripted(|_command, _cwd, on_data| {
            on_data(b"partial\n");
            Ok(None)
        }),
    );
    let error = execute(
        &definition,
        json!({ "command": "remote" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("must fail");
    assert_eq!(
        error_message(error),
        "partial\n\n\nCommand terminated without an exit code"
    );
}

#[tokio::test]
async fn should_respect_timeout() {
    let temp = TempDir::new().expect("temp");
    let ctx = TestContext::new(temp.path());
    let definition = real_bash_definition(temp.path());
    let error = execute(
        &definition,
        json!({ "command": "sleep 5", "timeout": 0.05 }),
        &ctx,
    )
    .await
    .expect_err("must time out");
    assert!(error_message(error).contains("Command timed out after 0.05 seconds"));
}

#[tokio::test]
async fn should_include_full_output_path_for_truncated_timeout_and_abort_errors() {
    let temp = TempDir::new().expect("temp");
    for (script_error, expected) in [
        ("timeout:5", "Command timed out after 5 seconds"),
        ("aborted", "Command aborted"),
    ] {
        let error = script_error.to_string();
        let operations = scripted(move |_command, _cwd, on_data| {
            for index in 1..=3000 {
                on_data(format!("{index}\n").as_bytes());
            }
            Err(error.clone())
        });
        let definition = bash_definition(temp.path(), operations);
        let error = execute(
            &definition,
            json!({ "command": "chatty-fail" }),
            &TestContext::new(temp.path()),
        )
        .await
        .expect_err("must fail");
        let message = error_message(error);
        assert!(message.contains(expected));
        assert!(message.contains("[Showing lines "));
        assert!(!message.contains("Full output: undefined"));
        let path = message
            .split("Full output: ")
            .last()
            .and_then(|rest| rest.split(']').next())
            .expect("full output path");
        assert!(std::path::Path::new(path).exists());
        let full = std::fs::read_to_string(path).expect("read full output");
        assert!(full.contains("1\n2\n3"));
        assert!(full.contains("2998\n2999\n3000"));
    }
}

#[tokio::test]
async fn should_throw_error_when_cwd_does_not_exist() {
    let nonexistent = "/this/directory/definitely/does/not/exist/12345";
    let definition = create_bash_tool_definition(
        nonexistent,
        Some(BashToolOptions {
            expose_session_environment: false,
            ..BashToolOptions::default()
        }),
    );
    let error = execute(
        &definition,
        json!({ "command": "echo test" }),
        &TestContext::new(nonexistent),
    )
    .await
    .expect_err("must fail");
    assert_eq!(
        error_message(error),
        format!("Working directory does not exist: {nonexistent}\nCannot execute bash commands.")
    );
}

#[tokio::test]
async fn should_handle_process_spawn_errors() {
    let temp = TempDir::new().expect("temp");
    let operations = create_local_shell_operations(
        "bash",
        Arc::new(|| {
            Ok(ShellConfig {
                shell: "/nonexistent-shell-path-xyz123".to_string(),
                args: vec!["-c".to_string()],
                command_transport: None,
            })
        }),
    );
    let definition = bash_definition(temp.path(), operations);
    let error = execute(
        &definition,
        json!({ "command": "echo test" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("must fail");
    assert!(error_message(error).contains("ENOENT"));
}

#[tokio::test]
async fn should_pass_shell_path_through_to_shell_resolution() {
    let temp = TempDir::new().expect("temp");
    let operations = create_local_bash_operations(Some("/custom/bash"));
    let on_data = |_: &[u8]| {};
    let error = operations
        .exec(
            "echo test",
            &temp.path().to_string_lossy(),
            BashExecOptions {
                on_data: &on_data,
                signal: None,
                timeout: None,
                env: None,
            },
        )
        .await
        .expect_err("must fail");
    assert_eq!(
        error.to_string(),
        "Custom shell path not found: /custom/bash"
    );

    // 自定义 operations 优先，不会走到 shellPath 解析。
    let definition = create_bash_tool_definition(
        temp.path().to_string_lossy().into_owned(),
        Some(BashToolOptions {
            shell_path: Some("/custom/bash".to_string()),
            operations: Some(scripted(|_command, _cwd, _on_data| Ok(Some(0)))),
            expose_session_environment: false,
            ..BashToolOptions::default()
        }),
    );
    execute(
        &definition,
        json!({ "command": "echo test" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("custom operations win");
}

#[cfg(unix)]
#[tokio::test]
async fn should_send_commands_over_stdin_when_shell_resolution_requires_it() {
    let temp = TempDir::new().expect("temp");
    let operations = create_local_shell_operations(
        "bash",
        Arc::new(|| {
            Ok(ShellConfig {
                shell: "/bin/cat".to_string(),
                args: Vec::new(),
                command_transport: Some(CommandTransport::Stdin),
            })
        }),
    );
    let chunks: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&chunks);
    let on_data = move |data: &[u8]| {
        sink.lock().unwrap().extend_from_slice(data);
    };
    let command = "Hello, World!";
    let result = operations
        .exec(
            command,
            &temp.path().to_string_lossy(),
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
    assert_eq!(String::from_utf8_lossy(&chunks.lock().unwrap()), command);
}

#[tokio::test]
async fn should_prepend_command_prefix_when_configured() {
    let temp = TempDir::new().expect("temp");
    let ctx = TestContext::new(temp.path());
    let definition = create_bash_tool_definition(
        temp.path().to_string_lossy().into_owned(),
        Some(BashToolOptions {
            command_prefix: Some("export TEST_VAR=hello".to_string()),
            expose_session_environment: false,
            ..BashToolOptions::default()
        }),
    );
    let result = execute(&definition, json!({ "command": "echo $TEST_VAR" }), &ctx)
        .await
        .expect("execute");
    assert_eq!(text_output(&result).trim(), "hello");

    let definition = create_bash_tool_definition(
        temp.path().to_string_lossy().into_owned(),
        Some(BashToolOptions {
            command_prefix: Some("echo prefix-output".to_string()),
            expose_session_environment: false,
            ..BashToolOptions::default()
        }),
    );
    let result = execute(
        &definition,
        json!({ "command": "echo command-output" }),
        &ctx,
    )
    .await
    .expect("execute");
    assert_eq!(text_output(&result).trim(), "prefix-output\ncommand-output");

    let definition = create_bash_tool_definition(
        temp.path().to_string_lossy().into_owned(),
        Some(BashToolOptions {
            expose_session_environment: false,
            ..BashToolOptions::default()
        }),
    );
    let result = execute(&definition, json!({ "command": "echo no-prefix" }), &ctx)
        .await
        .expect("execute");
    assert_eq!(text_output(&result).trim(), "no-prefix");
}

#[tokio::test]
async fn empty_command_prefix_is_treated_as_unconfigured() {
    // TS 是 `commandPrefix ? ... : command`：空字符串是假值，不应产生前导 "\n"。
    let temp = TempDir::new().expect("temp");
    let ctx = TestContext::new(temp.path());
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&seen);
    let operations = scripted(move |command, _cwd, _on_data| {
        sink.lock().unwrap().push(command.to_string());
        Ok(Some(0))
    });
    let definition = create_bash_tool_definition(
        temp.path().to_string_lossy().into_owned(),
        Some(BashToolOptions {
            operations: Some(operations),
            command_prefix: Some(String::new()),
            expose_session_environment: false,
            ..BashToolOptions::default()
        }),
    );
    execute(&definition, json!({ "command": "echo hi" }), &ctx)
        .await
        .expect("execute");
    assert_eq!(*seen.lock().unwrap(), vec!["echo hi".to_string()]);
}

// ===========================================================================
// 流式更新与截断
// ===========================================================================

/// `executeBash` 每块都 `stripAnsi`；bash 工具**不 strip**（spec §8.4 的 ANSI 差异验收）。
///
/// ANSI 一直保留到 `getTextOutput`/渲染器与 `ansiToHtml`，因此工具结果文本里必须还能
/// 看到原始转义序列。
#[tokio::test]
async fn bash_tool_keeps_ansi_escape_sequences_that_execute_bash_strips() {
    let temp = TempDir::new().expect("temp");
    let ctx = TestContext::new(temp.path());
    let chunks: [&[u8]; 2] = [b"\x1b[31mred", b"\x1b[0m plain\n"];

    let definition = bash_definition(
        temp.path(),
        scripted(move |_command, _cwd, on_data| {
            for chunk in chunks {
                on_data(chunk);
            }
            Ok(Some(0))
        }),
    );
    let result = execute(&definition, json!({ "command": "color" }), &ctx)
        .await
        .expect("execute");
    let text = text_output(&result);
    assert!(
        text.contains("\u{1b}[31mred\u{1b}[0m plain"),
        "bash tool must keep ANSI: {text:?}"
    );

    // 同一份字节走 bash-executor：每块 stripAnsi 后只剩纯文本。
    let streamed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&streamed);
    let on_chunk: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |chunk: &str| {
        sink.lock().expect("lock").push(chunk.to_string());
    });
    let operations = scripted(move |_command, _cwd, on_data| {
        for chunk in chunks {
            on_data(chunk);
        }
        Ok(Some(0))
    });
    let executed = kiana_tools::bash_executor::execute_bash_with_operations(
        "color",
        &temp.path().to_string_lossy(),
        operations.as_ref(),
        Some(kiana_tools::bash_executor::BashExecutorOptions {
            on_chunk: Some(on_chunk),
            signal: None,
        }),
    )
    .await
    .expect("execute bash");
    assert_eq!(executed.output, "red plain\n");
    assert_eq!(streamed.lock().expect("lock").concat(), "red plain\n");
}

#[tokio::test]
async fn should_coalesce_streaming_updates_for_chatty_output() {
    let temp = TempDir::new().expect("temp");
    let operations = scripted(|_command, _cwd, on_data| {
        for index in 0..5000 {
            on_data(format!("line {index}\n").as_bytes());
        }
        Ok(Some(0))
    });
    let definition = bash_definition(temp.path(), operations);
    let updates: Arc<Mutex<Vec<kiana_agent_core::AgentToolResult<JsonValue>>>> =
        Arc::new(Mutex::new(Vec::new()));
    let callback: AgentToolUpdateCallback = {
        let updates = Arc::clone(&updates);
        Arc::new(move |update| updates.lock().unwrap().push(update))
    };
    let result = execute_with_update(
        &definition,
        json!({ "command": "chatty" }),
        Some(callback),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("execute");

    assert!(updates.lock().unwrap().len() < 25);
    assert!(text_output(&result).contains("line 4999"));
}

#[tokio::test]
async fn bash_execute_emits_an_initial_empty_partial_update_before_output_arrives() {
    let temp = TempDir::new().expect("temp");
    let operations = scripted(|_command, _cwd, _on_data| Ok(Some(0)));
    let definition = bash_definition(temp.path(), operations);
    let updates: Arc<Mutex<Vec<kiana_agent_core::AgentToolResult<JsonValue>>>> =
        Arc::new(Mutex::new(Vec::new()));
    let callback: AgentToolUpdateCallback = {
        let updates = Arc::clone(&updates);
        Arc::new(move |update| updates.lock().unwrap().push(update))
    };
    let ctx = TestContext::new(temp.path());
    let mut future = definition.execute.execute(
        "tool-bash-1",
        json!({ "command": "sleep 10" }),
        None,
        Some(callback),
        Arc::clone(&ctx),
    );
    // 首次 poll 已经发出空更新；操作在当前实现里立刻完成，所以这里只检查顺序。
    let result = (&mut future).await.expect("execute");
    let updates = updates.lock().unwrap();
    assert!(!updates.is_empty());
    assert!(updates[0].content.is_empty());
    assert_eq!(updates[0].details, JsonValue::Null);
    assert_eq!(text_output(&result), "(no output)");
}

/// 回归：TS 的 `scheduleOutputUpdate` 里 `lastUpdateAt` 初始为 0，所以第一个数据块在
/// `handleData` 内同步发出（leading edge），即便 `ops.exec` 从不 await。同步突发 5 个
/// 数据块应得到 3 次更新：初始空更新、首块快照、最终快照。
#[tokio::test]
async fn synchronous_output_burst_emits_the_leading_edge_update() {
    let temp = TempDir::new().expect("temp");
    let operations = scripted(|_command, _cwd, on_data| {
        for index in 0..5 {
            on_data(format!("line{index}\n").as_bytes());
        }
        Ok(Some(0))
    });
    let definition = bash_definition(temp.path(), operations);
    let updates: Arc<Mutex<Vec<kiana_agent_core::AgentToolResult<JsonValue>>>> =
        Arc::new(Mutex::new(Vec::new()));
    let callback: AgentToolUpdateCallback = {
        let updates = Arc::clone(&updates);
        Arc::new(move |update| updates.lock().unwrap().push(update))
    };
    let result = execute_with_update(
        &definition,
        json!({ "command": "burst" }),
        Some(callback),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("execute");

    let updates = updates.lock().unwrap();
    let texts: Vec<String> = updates.iter().map(text_output).collect();
    assert_eq!(
        texts,
        vec![
            String::new(),
            "line0\n".to_string(),
            "line0\nline1\nline2\nline3\nline4\n".to_string(),
        ]
    );
    assert!(updates[0].content.is_empty());
    assert_eq!(updates[0].details, JsonValue::Null);
    assert_eq!(
        updates[1].details,
        JsonValue::Object(serde_json::Map::new())
    );
    assert_eq!(
        updates[2].details,
        JsonValue::Object(serde_json::Map::new())
    );
    assert_eq!(text_output(&result), "line0\nline1\nline2\nline3\nline4\n");
}

#[tokio::test]
async fn should_not_count_a_trailing_newline_as_an_extra_truncated_bash_output_line() {
    let temp = TempDir::new().expect("temp");
    let operations = scripted(|_command, _cwd, on_data| {
        for index in 1..=4000 {
            on_data(format!("line-{index:04}\n").as_bytes());
        }
        Ok(Some(0))
    });
    let definition = bash_definition(temp.path(), operations);
    let result = execute(
        &definition,
        json!({ "command": "many-lines" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("execute");
    let output = text_output(&result);
    assert_eq!(result.details["truncation"]["totalLines"], 4000);
    assert_eq!(result.details["truncation"]["outputLines"], 2000);
    assert!(output.contains("line-2001"));
    assert!(output.contains("line-4000"));
    assert!(output.contains("[Showing lines 2001-4000 of 4000. Full output: "));
    assert!(!output.contains("4001"));
}

#[tokio::test]
async fn should_decode_utf8_characters_split_across_output_chunks() {
    let temp = TempDir::new().expect("temp");
    let euro = "\u{20ac}\n".as_bytes();
    let (first, second) = euro.split_at(1);
    let first = first.to_vec();
    let second = second.to_vec();
    let operations = scripted(move |_command, _cwd, on_data| {
        on_data(&first);
        on_data(&second);
        Ok(Some(0))
    });
    let definition = bash_definition(temp.path(), operations);
    let result = execute(
        &definition,
        json!({ "command": "split-utf8" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("execute");
    assert_eq!(text_output(&result).trim(), "\u{20ac}");
}

#[tokio::test]
async fn golden_utf8_split_matches_ts() {
    let temp = TempDir::new().expect("temp");
    let euro = "\u{20ac}\n".as_bytes();
    let (first, second) = euro.split_at(1);
    let first = first.to_vec();
    let second = second.to_vec();
    let operations = scripted(move |_command, _cwd, on_data| {
        on_data(&first);
        on_data(&second);
        Ok(Some(0))
    });
    let definition = bash_definition(temp.path(), operations);
    let result = execute(
        &definition,
        json!({ "command": "utf8" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("execute");
    assert_eq!(
        text_output(&result),
        golden()["bash"]["utf8_split"]["text"].as_str().unwrap()
    );
}

#[tokio::test]
async fn golden_truncated_lines_match_ts() {
    let temp = TempDir::new().expect("temp");
    let operations = scripted(|_command, _cwd, on_data| {
        for index in 1..=4000 {
            on_data(format!("line-{index:04}\n").as_bytes());
        }
        Ok(Some(0))
    });
    let definition = bash_definition(temp.path(), operations);
    let result = execute(
        &definition,
        json!({ "command": "many" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("execute");
    let golden = golden();
    let expected_text = golden["bash"]["truncated_lines"]["text"].as_str().unwrap();
    assert_eq!(normalize_temp_paths(&text_output(&result)), expected_text);
    assert_eq!(
        normalize_details(&result.details),
        golden["bash"]["truncated_lines"]["details"]
    );
}

#[tokio::test]
async fn golden_byte_truncation_matches_ts() {
    let temp = TempDir::new().expect("temp");
    let operations = scripted(|_command, _cwd, on_data| {
        for index in 1..=3000 {
            on_data(format!("{index:04}-{}\n", "y".repeat(20)).as_bytes());
        }
        Ok(Some(0))
    });
    let definition = bash_definition(temp.path(), operations);
    let result = execute(
        &definition,
        json!({ "command": "bytes" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("execute");
    let golden = golden();
    assert_eq!(
        normalize_temp_paths(&text_output(&result)),
        golden["bash"]["bytes_truncation"]["text"].as_str().unwrap()
    );
    assert_eq!(
        normalize_details(&result.details),
        golden["bash"]["bytes_truncation"]["details"]
    );
}

#[tokio::test]
async fn should_persist_full_output_when_truncation_happens_by_line_count_only() {
    let temp = TempDir::new().expect("temp");
    let operations = scripted(|_command, _cwd, on_data| {
        for index in 1..=3000 {
            on_data(format!("{index}\n").as_bytes());
        }
        Ok(Some(0))
    });
    let definition = bash_definition(temp.path(), operations);
    let result = execute(
        &definition,
        json!({ "command": "seq 3000" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("execute");
    let output = text_output(&result);
    let full_output_path = result.details["fullOutputPath"].as_str().expect("path");
    assert_eq!(result.details["truncation"]["truncated"], true);
    assert_eq!(result.details["truncation"]["truncatedBy"], "lines");
    assert!(output.contains("[Showing lines "));
    assert!(!output.contains("Full output: undefined"));
    let full = std::fs::read_to_string(full_output_path).expect("read");
    assert!(full.contains("1\n2\n3"));
    assert!(full.contains("2998\n2999\n3000"));
}

// ===========================================================================
// 本地操作 / 环境
// ===========================================================================

#[cfg(unix)]
#[tokio::test]
async fn should_expose_local_bash_operations_for_extension_reuse() {
    let temp = TempDir::new().expect("temp");
    let operations = create_local_bash_operations(None);
    let chunks: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&chunks);
    let on_data = move |data: &[u8]| sink.lock().unwrap().extend_from_slice(data);
    let env = kiana_core::shell::get_shell_env()
        .into_iter()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .chain(std::iter::once((
            "TEST_LOCAL_BASH_OPS".to_string(),
            "from-local-ops".to_string(),
        )))
        .collect::<IndexMap<_, _>>();
    let result = operations
        .exec(
            "echo $TEST_LOCAL_BASH_OPS",
            &temp.path().to_string_lossy(),
            BashExecOptions {
                on_data: &on_data,
                signal: None,
                timeout: None,
                env: Some(env),
            },
        )
        .await
        .expect("exec");
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(
        String::from_utf8_lossy(&chunks.lock().unwrap()).trim(),
        "from-local-ops"
    );
}

#[tokio::test]
async fn bash_uses_ctx_cwd_when_provided() {
    let temp = TempDir::new().expect("temp");
    let definition = real_bash_definition(std::path::Path::new("/"));
    let result = execute(
        &definition,
        json!({ "command": "pwd" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect("execute");
    let canonical = temp.path().canonicalize().expect("canonical");
    assert!(text_output(&result).contains(&canonical.to_string_lossy().to_string()));
}

#[tokio::test]
async fn exposes_session_state_before_custom_bash_spawn_hooks_and_supports_opting_out() {
    let temp = TempDir::new().expect("temp");
    let manager = SessionManager::in_memory(&temp.path().to_string_lossy(), None, None)
        .expect("session manager");
    let session_id = manager.get_session_id().to_string();
    let model = test_model("anthropic", "claude-sonnet-4-5");
    let ctx = TestContext::with_session(
        temp.path(),
        Some(model.clone()),
        manager,
        Some(ThinkingLevel::High),
    );

    let captured: Arc<Mutex<Option<BashSpawnContext>>> = Arc::new(Mutex::new(None));
    let capture_hook = {
        let captured = Arc::clone(&captured);
        Arc::new(move |context: BashSpawnContext| {
            *captured.lock().unwrap() = Some(context.clone());
            context
        })
    };
    let definition = create_bash_tool_definition(
        temp.path().to_string_lossy().into_owned(),
        Some(BashToolOptions {
            operations: Some(scripted(|_command, _cwd, _on_data| Ok(Some(0)))),
            spawn_hook: Some(capture_hook),
            ..BashToolOptions::default()
        }),
    );
    execute(&definition, json!({ "command": "printf ok" }), &ctx)
        .await
        .expect("execute");
    let environment = captured.lock().unwrap().clone().expect("hook ran").env;
    assert_eq!(environment.get("KIANA_SESSION_ID"), Some(&session_id));
    assert_eq!(environment.get("KIANA_SESSION_FILE"), None);
    assert_eq!(
        environment.get("KIANA_PROVIDER"),
        Some(&"anthropic".to_string())
    );
    assert_eq!(
        environment.get("KIANA_MODEL"),
        Some(&"claude-sonnet-4-5".to_string())
    );
    assert_eq!(
        environment.get("KIANA_REASONING_LEVEL"),
        Some(&"high".to_string())
    );
    let contribution = kiana_tools::bash::bash_tool_system_prompt_contribution();
    let expected_guidelines = contribution
        .guidelines
        .iter()
        .map(|guideline| (*guideline).to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        definition.prompt_guidelines.as_deref(),
        Some(expected_guidelines.as_slice())
    );

    let opted_out: Arc<Mutex<Option<BashSpawnContext>>> = Arc::new(Mutex::new(None));
    let opt_out_hook = {
        let opted_out = Arc::clone(&opted_out);
        Arc::new(move |context: BashSpawnContext| {
            *opted_out.lock().unwrap() = Some(context.clone());
            context
        })
    };
    let definition = create_bash_tool_definition(
        temp.path().to_string_lossy().into_owned(),
        Some(BashToolOptions {
            operations: Some(scripted(|_command, _cwd, _on_data| Ok(Some(0)))),
            spawn_hook: Some(opt_out_hook),
            expose_session_environment: false,
            ..BashToolOptions::default()
        }),
    );
    execute(&definition, json!({ "command": "printf ok" }), &ctx)
        .await
        .expect("execute");
    let environment = opted_out.lock().unwrap().clone().expect("hook ran").env;
    for key in [
        "KIANA_SESSION_ID",
        "KIANA_SESSION_FILE",
        "KIANA_PROVIDER",
        "KIANA_MODEL",
        "KIANA_REASONING_LEVEL",
    ] {
        assert!(!environment.contains_key(key), "{key} must be absent");
    }
    assert_eq!(definition.prompt_guidelines, None);
}

#[tokio::test]
async fn aborted_signal_is_reported_as_command_aborted() {
    let temp = TempDir::new().expect("temp");
    let definition = bash_definition(
        temp.path(),
        scripted(|_command, _cwd, on_data| {
            on_data(b"before-kill\n");
            Err("aborted".to_string())
        }),
    );
    let error = execute(
        &definition,
        json!({ "command": "abort" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("must fail");
    assert_eq!(error_message(error), "before-kill\n\n\nCommand aborted");
}

#[tokio::test]
async fn golden_timeout_and_abort_messages_match_ts() {
    let temp = TempDir::new().expect("temp");
    let golden = golden();
    let script_error = "timeout:5".to_string();
    let operations = scripted(move |_command, _cwd, on_data| {
        for index in 1..=3000 {
            on_data(format!("{index}\n").as_bytes());
        }
        Err(script_error.clone())
    });
    let definition = bash_definition(temp.path(), operations);
    let error = execute(
        &definition,
        json!({ "command": "chatty-fail" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("must fail");
    let message = normalize_temp_paths(&error_message(error));
    let expected = golden["bash"]["timeout_full"]["message"].as_str().unwrap();
    assert_eq!(message, expected);

    let operations = scripted(|_command, _cwd, on_data| {
        on_data(b"before-kill\n");
        Err("aborted".to_string())
    });
    let definition = bash_definition(temp.path(), operations);
    let error = execute(
        &definition,
        json!({ "command": "abort" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("must fail");
    assert_eq!(
        error_message(error),
        golden["bash"]["aborted"]["message"].as_str().unwrap()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn local_operations_report_aborted_before_spawn() {
    let temp = TempDir::new().expect("temp");
    let operations = create_local_bash_operations(None);
    let token = CancellationToken::new();
    token.cancel();
    let on_data = |_: &[u8]| {};
    let error = operations
        .exec(
            "echo hi",
            &temp.path().to_string_lossy(),
            BashExecOptions {
                on_data: &on_data,
                signal: Some(token),
                timeout: None,
                env: None,
            },
        )
        .await
        .expect_err("must fail");
    assert_eq!(error.to_string(), "aborted");
}

#[tokio::test]
async fn local_operations_validate_timeout_before_any_other_check() {
    let operations = create_local_bash_operations(None);
    let on_data = |_: &[u8]| {};
    let error = operations
        .exec(
            "echo hi",
            "/this/directory/does/not/exist",
            BashExecOptions {
                on_data: &on_data,
                signal: None,
                timeout: Some(0.0),
                env: None,
            },
        )
        .await
        .expect_err("must fail");
    assert_eq!(
        error.to_string(),
        "Invalid timeout: must be a finite number of seconds"
    );
    let error = operations
        .exec(
            "echo hi",
            "/this/directory/does/not/exist",
            BashExecOptions {
                on_data: &on_data,
                signal: None,
                timeout: Some(2_147_483.648),
                env: None,
            },
        )
        .await
        .expect_err("must fail");
    assert_eq!(
        error.to_string(),
        "Invalid timeout: maximum is 2147483.647 seconds"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn bash_tool_resolves_after_the_shell_exits_even_if_inherited_stdio_handles_stay_open() {
    // Windows-only 回归（#5303 的 bash 工具侧）；Linux 上由 kiana-core 的单元测试覆盖状态机。
    let temp = TempDir::new().expect("temp");
    let definition = real_bash_definition(temp.path());
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        execute(
            &definition,
            json!({ "command": "echo child-exiting" }),
            &TestContext::new(temp.path()),
        ),
    )
    .await
    .expect("must resolve")
    .expect("execute");
    assert!(text_output(&result).contains("child-exiting"));
}

#[tokio::test]
async fn temp_file_failures_fail_the_call_instead_of_returning_a_bogus_footer() {
    // 回归（TS `OutputAccumulator.append` / `finish` / `closeTempFile`）：临时文件创建/写入
    // 失败（TMPDIR 不存在、磁盘已满等）必须让 bash 调用失败，不能返回一个成功结果并在
    // 脚注里写 `Full output: undefined`。
    //
    // 前缀里放一个不存在的子目录：`createOutputFileStream` 对
    // `<tmpdir>/<prefix>-<hex>.log` 的 open 稳定得到 ENOENT，且不影响进程级 TMPDIR。
    let temp = TempDir::new().expect("temp");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let config = ShellToolConfig {
        name: "bash".to_string(),
        label: "bash".to_string(),
        shell_name: "bash".to_string(),
        prompt: "$".to_string(),
        prompt_snippet: String::new(),
        prompt_guidelines: None,
        temp_file_prefix: format!(
            "kiana-bash-missing-{}-{nonce}/kiana-bash",
            std::process::id()
        ),
    };
    let operations = scripted(|_command, _cwd, on_data| {
        // > DEFAULT_MAX_BYTES：append 自身就会尝试创建临时文件。
        on_data(&vec![b'x'; 60 * 1024]);
        Ok(Some(0))
    });
    let definition = create_shell_tool_definition(
        temp.path().to_string_lossy().into_owned(),
        config,
        Some(BashToolOptions {
            operations: Some(operations),
            expose_session_environment: false,
            ..BashToolOptions::default()
        }),
    );

    let error = execute(
        &definition,
        json!({ "command": "x" }),
        &TestContext::new(temp.path()),
    )
    .await
    .expect_err("a temp file failure must fail the call");
    assert!(
        error
            .message
            .starts_with("ENOENT: no such file or directory, open"),
        "{}",
        error.message
    );
}
