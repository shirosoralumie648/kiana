//! `tools/bash.ts` / `tools/powershell.ts`：通过环境 shell 执行命令，输出走保留窗口与 spill。
//!
//! # 职责与边界
//! 模型只能通过工具执行命令。本模块把一次工具调用翻译成 `ExecutionEnv::exec`：校验超时、
//! 给命令加前缀、允许调用方用 `prepare` 改写命令/cwd/环境，然后把环境解码出的输出块转发
//! 给 [`ToolExecutionApi::output`]，最后把结果整理成工具结果。真正起进程、解码输出、执行
//! 超时以及把超限输出写到 spill 文件，都由 `crate::env` 的实现负责；stdout/stderr 的块按
//! 到达顺序交给回调，合并发生在 harness 的保留窗口里。
//!
//! # 一次调用的执行流程
//! 1. `serde_json::from_value` 解析 [`BashToolInput`]：`command` 必填，`timeout`（秒）可选。
//! 2. `validate_timeout` 拒绝非有限、非正数或超过 `MAX_TIMEOUT_SECONDS` 的值。
//! 3. `prepare_execution` 拼上 `command_prefix`（空串按没有处理），读取环境的 cwd，构造
//!    `inherit_env = true`、空 `env` 的 [`BashExecution`]，再交给 `prepare` 回调改写。
//! 4. `run_command` 逐种命令形式调用 `env.exec`，每个输出块立即进入 harness 的保留窗口。
//! 5. 成功时用 `api.retained_output()`（而不是本模块自己的缓冲）组装结构化输出；非零退出码
//!    仍表示命令跑完了，但结果标记 `isError` 并附一条 `exit_code` 诊断。
//!
//! # 失败模式
//! - 环境返回 `SpawnError` 时尝试下一种命令形式；`powershell` 依赖这一点从 `pwsh` 回退到
//!   `powershell`，全部形式都启动失败时抛出最后一条错误。
//! - `Timeout` 被改写成 `Command timed out after <timeout> seconds`（`timeout` 为 `None`
//!   时写成 `undefined`，与 TS 的 `${options?.timeout}` 相同）；`Aborted` 且上下文确实
//!   已取消时原样返回底层 `ExecutionError`，否则变成 `Command aborted`。
//! - spill 路径在错误分支之前读取：即使命令超时/被取消，只要环境已经写出 spill 文件，
//!   调用方仍会先收到 `full_output` 信息诊断。
//!
//! 上述行为由 `tests/tools.rs` 的 powershell/bash 用例锁定（含回退、超时、spill、
//! 非零退出码与保留窗口转发）。
//!
//! # 注意事项
//! - `prepare` 只能改命令、cwd、环境变量与 `inherit_env`；`timeout` 是工具输入，不放在
//!   [`BashExecution`] 里。
//! - 工具声明的 `output_limits` 只指定"保留尾部"；字节/行上限由 harness 在调用开始时用
//!   `DEFAULT_MAX_BYTES`/`DEFAULT_MAX_LINES` 补足（见 `harness/tool.rs` 的 `output_limits`）。
//! - `powershell` 会在用户命令前加一行强制控制台 UTF-8 输出的脚本。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use indexmap::IndexMap;
use kiana_chord::context::Context;
use kiana_json::to_radix_string;
use serde_json::{Map as JsonMap, Value as JsonValue, json};

use crate::env::{
    ExecutionErrorCode, OutputCallback, ShellCommand, ShellExecOptions, ShellSpillOptions,
};
use crate::harness::define::define_tool;
use crate::harness::types::{
    DiagnosticSeverity, ToolDiagnostic, ToolExecuteFn, ToolExecutionApi, ToolExecutionResult,
    ToolOutput, ToolRegistration,
};
use crate::tools::env::require_env;
use crate::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};
use crate::types::BoxError;

/// `MAX_TIMEOUT_SECONDS = 2_147_483_647 / 1000`。
const MAX_TIMEOUT_SECONDS: f64 = 2_147_483_647.0 / 1000.0;

/// 命令即将运行时，`prepare` 可以改写的执行参数：脚本、工作目录与环境。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashExecution {
    /// 命令（含前缀）。
    pub command: String,
    /// 工作目录。
    pub cwd: String,
    /// 额外环境变量。
    pub env: IndexMap<String, String>,
    /// 是否继承环境。
    pub inherit_env: bool,
}

/// `BashPrepare`：在命令运行前调整 [`BashExecution`]。
///
/// 回调在 `command_prefix` 已拼接、cwd 已从环境读取之后调用；它可以修改命令、cwd、环境
/// 变量与 `inherit_env`，也可以读取传入的 `api`/`context` 做额外检查。回调返回 `Err` 时
/// 本次工具调用直接失败，`env.exec` 不会执行。
///
/// 用 `for<'a> Fn(...) -> Pin<Box<dyn Future + 'a>>` 而不是 `async fn` 的 trait 对象：闭包
/// 需要同时支持捕获状态（`Fn`）与借用本次调用的参数（高阶生命周期），并且可跨任务共享
/// （`Send + Sync`），所以 future 被显式装箱且带 `'a`。
pub type BashPrepare = Arc<
    dyn for<'a> Fn(
            &'a mut BashExecution,
            Arc<dyn ToolExecutionApi>,
            Arc<dyn Context>,
        ) -> Pin<Box<dyn Future<Output = Result<(), BoxError>> + Send + 'a>>
        + Send
        + Sync,
>;

/// `BashToolOptions`。
#[derive(Clone, Default)]
pub struct BashToolOptions {
    /// 每条命令前追加的行。
    pub command_prefix: Option<String>,
    /// 运行前调整执行参数。
    pub prepare: Option<BashPrepare>,
}

/// `PowerShellToolOptions`。
#[derive(Clone, Default)]
pub struct PowerShellToolOptions {
    /// 每条命令前追加的行。
    pub command_prefix: Option<String>,
    /// 运行前调整执行参数。
    pub prepare: Option<BashPrepare>,
    /// 依次尝试的 PowerShell 程序；默认 `pwsh`，然后 `powershell`。无法启动的会被跳过。
    pub programs: Option<Vec<String>>,
}

/// `BashToolInput` / `PowerShellToolInput` 的共同形状。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashToolInput {
    /// 要执行的命令。
    pub command: String,
    /// 超时（秒，可选，无默认超时）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
}

/// `PowerShellToolInput`。
pub type PowerShellToolInput = BashToolInput;

/// `ShellStructuredOutput`。
///
/// 与 `SHELL_STRUCTURED_OUTPUT_SCHEMA` 字段一致的 Rust 视图；`run_command` 直接构造 JSON
/// 对象（不经过这个类型），因此字段与 JSON 键（如 `full_output_path` ↔ `fullOutputPath`）
/// 的对应关系需要与该 schema 手工保持同步。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellStructuredOutput {
    /// 合并后的 stdout 与 stderr：保留的尾部，即模型看到的内容。
    pub output: String,
    /// 是否丢弃了更早的输出。
    pub truncated: bool,
    /// 完整输出所在的文件（发生 spill 时）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
    /// 退出码。
    pub exit_code: i32,
}

// 按 JS `String(value)` 的规则格式化数字（`kiana_json::to_radix_string` 的契约），
// 这样超时文案采用 JS `String(value)` 的格式，而不是 Rust 默认的浮点格式。
fn number_text(value: f64) -> String {
    to_radix_string(value, 10)
}

// 校验工具输入里的秒数。上限与 TS 的 `MAX_TIMEOUT_SECONDS = 2_147_483_647 / 1000`
// 相同；`None` 表示不设超时（没有默认值）。错误文本也是 TS 的逐字文案。
fn validate_timeout(timeout: Option<f64>) -> Result<(), BoxError> {
    let Some(timeout) = timeout else {
        return Ok(());
    };
    if !timeout.is_finite() || timeout <= 0.0 {
        return Err(
            std::io::Error::other("Invalid timeout: must be a finite number of seconds").into(),
        );
    }
    if timeout > MAX_TIMEOUT_SECONDS {
        return Err(std::io::Error::other(format!(
            "Invalid timeout: maximum is {} seconds",
            number_text(MAX_TIMEOUT_SECONDS)
        ))
        .into());
    }
    Ok(())
}

/// 一次调用的执行参数：命令加前缀，在环境的工作目录里，然后交给 `prepare`。
struct PrepareOptions<'a> {
    command_prefix: Option<&'a str>,
    prepare: Option<&'a BashPrepare>,
}

// 组装一次执行的初始参数，再把改写权交给 `prepare`。
//
// 前缀是空串时不插入换行（TS 用 truthiness 判断 `commandPrefix`）；cwd 直接取自环境，
// 没有执行环境时由 `require_env` 报错。`prepare` 收到 `api`/`context` 的 `Arc` 克隆，
// 因此它看到的是本次调用同一套 API 与取消信号。
async fn prepare_execution(
    command: &str,
    options: PrepareOptions<'_>,
    api: &Arc<dyn ToolExecutionApi>,
    context: &Arc<dyn Context>,
) -> Result<BashExecution, BoxError> {
    let command = match options.command_prefix {
        Some(prefix) if !prefix.is_empty() => format!("{prefix}\n{command}"),
        _ => command.to_owned(),
    };
    let cwd = require_env(api.as_ref())?.cwd().to_owned();
    let mut execution = BashExecution {
        command,
        cwd,
        env: IndexMap::new(),
        inherit_env: true,
    };
    if let Some(prepare) = options.prepare {
        (prepare)(&mut execution, Arc::clone(api), Arc::clone(context)).await?;
    }
    Ok(execution)
}

fn diagnostic(severity: DiagnosticSeverity, code: &str, message: String) -> ToolDiagnostic {
    ToolDiagnostic {
        severity,
        code: Some(code.to_owned()),
        message,
    }
}

/// 依次尝试每种命令形式直到有一种能启动，把输出流式写进 `api.output()` 的保留窗口，
/// 再把结果变成工具结果：spill 诊断、非零退出的错误结果、超时等失败抛错。
///
/// # 参数
/// - `commands`：按顺序尝试的命令形式；`Shell` 走环境 shell，`Argv` 直接以第一项为程序。
/// - `execution`：已经被 `prepare` 改写过的命令、cwd 与 env。
/// - `timeout`：秒；既传给环境，也用于超时错误文案。
///
/// # 返回与失败
/// - 成功（包括非零退出码）返回带 `structuredOutput` 的结果，不设置 `output`：模型看到的
///   内容由 harness 从保留输出里取。
/// - 所有形式都以 `SpawnError` 失败时返回最后一条错误。
/// - 超时被转成 `std::io::Error`；上下文已取消的 `Aborted` 保留原始 `ExecutionError`，
///   让 harness 按取消处理。
///
/// # 顺序与副作用
/// 输出块在命令结束前就逐个追加到 `api`，因此失败或超时的命令也能留下已产生的输出。
/// `spill_path` 的诊断在错误分支之前发出，避免提前 `return` 把它丢掉。
async fn run_command(
    commands: Vec<ShellCommand>,
    execution: &BashExecution,
    timeout: Option<f64>,
    api: &Arc<dyn ToolExecutionApi>,
    context: &Arc<dyn Context>,
) -> Result<ToolExecutionResult, BoxError> {
    let env = require_env(api.as_ref())?;
    let mut result = None;
    // 逐个形式尝试：只有 SpawnError 才继续，成功或其它错误都结束循环。
    for command in commands {
        let output_api = Arc::clone(api);
        let on_output: OutputCallback = Arc::new(move |text, _context, info| {
            output_api.output(ToolOutput::Text(text.to_owned()), info.skipped);
        });
        // 保留窗口由 harness 提供（`None` 表示保留头部、不接受 skip）；spill 阈值固定用
        // truncate 的默认上限，与 TS 的 `{ afterBytes: 51200, afterLines: 2000 }` 一致。
        let options = ShellExecOptions {
            cwd: Some(execution.cwd.clone()),
            env: Some(execution.env.clone()),
            inherit_env: Some(execution.inherit_env),
            timeout,
            on_output: Some(on_output),
            spill: Some(ShellSpillOptions {
                after_bytes: DEFAULT_MAX_BYTES,
                after_lines: DEFAULT_MAX_LINES,
            }),
            // 环境可以据此省掉保留尾部之外的输出，并报告省略量。
            window: api.output_window(),
        };
        let current = env.exec(command, Some(&options), context.as_ref()).await;
        // 无法启动的程序不产生输出；下一种形式仍可能启动。
        let matched = match &current {
            Ok(_) => true,
            Err(error) => error.code != ExecutionErrorCode::SpawnError,
        };
        // 保留本次结果作为"最后一次尝试"；若继续尝试下一种形式，它会被覆盖。
        result = Some(current);
        if matched {
            break;
        }
    }
    let result = result.ok_or_else(|| std::io::Error::other("No command to run"))?;
    let spill_path = match &result {
        Ok(value) => value.spill_path.clone(),
        Err(error) => error.spill_path.clone(),
    };
    // 先报告 spill 再处理错误：超时/取消分支会提前 return，晚发就丢掉这条诊断。
    if let Some(spill_path) = &spill_path {
        api.diagnostic(diagnostic(
            DiagnosticSeverity::Info,
            "full_output",
            format!("Full output: {spill_path}"),
        ));
    }
    // 失败分支把环境错误码翻译成工具层文案；上下文已取消的情况例外，保留原始错误。
    let value = match result {
        Ok(value) => value,
        Err(error) => {
            if error.code == ExecutionErrorCode::Aborted
                && context
                    .abort_signal()
                    .is_some_and(|signal| signal.is_aborted())
            {
                return Err(Box::new(error));
            }
            if error.code == ExecutionErrorCode::Timeout {
                let timeout_text = match timeout {
                    Some(timeout) => number_text(timeout),
                    None => "undefined".to_owned(),
                };
                return Err(std::io::Error::other(format!(
                    "Command timed out after {timeout_text} seconds"
                ))
                .into());
            }
            if error.code == ExecutionErrorCode::Aborted {
                return Err(std::io::Error::other("Command aborted").into());
            }
            return Err(Box::new(error));
        }
    };
    // 模型看到的是 harness 保留窗口里的文本；结构化输出复用它，不在这里重新截断。
    let retained = api.retained_output();
    // 键的插入顺序就是 JSON 顺序：output、truncated、fullOutputPath、exitCode。
    let mut structured = JsonMap::new();
    structured.insert("output".to_owned(), json!(retained.text));
    structured.insert("truncated".to_owned(), json!(retained.truncated));
    if let Some(spill_path) = &spill_path {
        structured.insert("fullOutputPath".to_owned(), json!(spill_path));
    }
    structured.insert("exitCode".to_owned(), json!(value.exit_code));
    // 非零退出码不是执行失败：结果仍带完整 structuredOutput，只是标记 isError 并加诊断。
    if value.exit_code == 0 {
        return Ok(ToolExecutionResult {
            structured_output: Some(JsonValue::Object(structured)),
            ..ToolExecutionResult::default()
        });
    }
    Ok(ToolExecutionResult {
        structured_output: Some(JsonValue::Object(structured)),
        is_error: Some(true),
        diagnostics: Some(vec![diagnostic(
            DiagnosticSeverity::Error,
            "exit_code",
            format!("Command exited with code {}", value.exit_code),
        )]),
        ..ToolExecutionResult::default()
    })
}

const SHELL_STRUCTURED_OUTPUT_SCHEMA: &str = r#"{
  "type": "object",
  "required": ["output", "truncated", "exitCode"],
  "properties": {
    "output": {
      "type": "string",
      "description": "Combined stdout and stderr: the retained tail, as the model sees it"
    },
    "truncated": {
      "type": "boolean",
      "description": "Whether earlier output was dropped"
    },
    "fullOutputPath": {
      "type": "string",
      "description": "File with the complete output, when it was spilled"
    },
    "exitCode": {
      "type": "number"
    }
  }
}"#;

const TIMEOUT_PROPERTY: &str = "Timeout in seconds (optional, no default timeout)";

fn shell_structured_output_schema() -> JsonValue {
    serde_json::from_str(SHELL_STRUCTURED_OUTPUT_SCHEMA)
        .expect("the structured output schema is valid JSON")
}

/// `createBashTool(options)`。
///
/// `None` 等价于 `BashToolOptions::default()`（无命令前缀、无 `prepare`）。返回的注册项包含：
/// - `parameters`：`{ command: string, timeout?: number }`，只有 `command` 必填；
/// - `description`：用 `DEFAULT_MAX_LINES` 与 `DEFAULT_MAX_BYTES / 1024` 拼出的截断说明；
/// - `execute`：解析输入 → 校验超时 → `prepare_execution` → 以一条 `ShellCommand::Shell`
///   调用 `run_command`；
/// - `structured_output_schema`：本文件的 `SHELL_STRUCTURED_OUTPUT_SCHEMA`；
/// - `output_limits`：只声明 `retain: Tail`，具体上限由 harness 的缺省值补足。
///
/// `options` 被移动进 `execute` 闭包；每次调用只做 `Clone`（`prepare` 本身是 `Arc`）。
pub fn create_bash_tool(options: Option<BashToolOptions>) -> ToolRegistration {
    let options = options.unwrap_or_default();
    let parameters = json!({
        "type": "object",
        "required": ["command"],
        "properties": {
            "command": {
                "type": "string",
                "description": "Bash command to execute"
            },
            "timeout": {
                "type": "number",
                "description": TIMEOUT_PROPERTY
            }
        }
    });
    let description = format!(
        "Execute a bash command in the current working directory. Returns combined stdout and stderr. Output is truncated to last {} lines or {}KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
        DEFAULT_MAX_LINES,
        DEFAULT_MAX_BYTES / 1024
    );
    let execute: ToolExecuteFn = Arc::new(move |args, api, context| {
        let options = options.clone();
        Box::pin(async move {
            let input: BashToolInput = serde_json::from_value(args)?;
            validate_timeout(input.timeout)?;
            let execution = prepare_execution(
                &input.command,
                PrepareOptions {
                    command_prefix: options.command_prefix.as_deref(),
                    prepare: options.prepare.as_ref(),
                },
                &api,
                &context,
            )
            .await?;
            run_command(
                vec![ShellCommand::Shell(execution.command.clone())],
                &execution,
                input.timeout,
                &api,
                &context,
            )
            .await
        })
    });
    define_tool(ToolRegistration {
        tool: kiana_ai::Tool {
            name: "bash".to_owned(),
            description,
            parameters,
            constrained_sampling: None,
        },
        replay: None,
        execution_mode: None,
        prepare_arguments: None,
        structured_output_schema: Some(shell_structured_output_schema()),
        callers: None,
        deferred: false,
        output_limits: Some(crate::harness::types::OutputLimits {
            max_bytes: None,
            max_lines: None,
            retain: Some(crate::harness::types::OutputRetain::Tail),
        }),
        execute: Some(execute),
    })
}

/// 输出在控制台的代码页下也按 UTF-8 编码，与编码 agent 的 `powershell` 工具一致。
const UTF8_OUTPUT: &str = "try { [Console]::OutputEncoding=[System.Text.Encoding]::UTF8 } catch {}";
const POWERSHELL_ARGS: [&str; 5] = [
    "-NoProfile",
    "-NonInteractive",
    "-ExecutionPolicy",
    "Bypass",
    "-Command",
];

/// `createPowerShellTool(options)`。
///
/// 与 `bash` 共用 `prepare_execution`/`run_command`，差别只在命令形式：
/// - 程序列表默认 `["pwsh", "powershell"]`；`programs` 给出时按给出的顺序尝试。
/// - 每种形式都是一个 `ShellCommand::Argv`：`[program, -NoProfile, -NonInteractive,
///   -ExecutionPolicy, Bypass, -Command, script]`，脚本第一行是强制 UTF-8 输出的
///   `try { [Console]::OutputEncoding=... } catch {}`，随后换行拼接用户命令。
/// - 某个程序 `SpawnError`（无法启动）时继续试下一个，因此可以先试 `pwsh` 再试
///   `powershell`；错误与结构化输出形状与 `bash` 相同。
///
/// `None` 等价于 `PowerShellToolOptions::default()`；`programs` 被复制进 `execute` 闭包，
/// 每次调用都会为每种程序生成一份脚本。`programs` 是空列表时没有任何命令可尝试，
/// `run_command` 返回 `No command to run` 错误。
pub fn create_powershell_tool(options: Option<PowerShellToolOptions>) -> ToolRegistration {
    let options = options.unwrap_or_default();
    let programs = options
        .programs
        .clone()
        .unwrap_or_else(|| vec!["pwsh".to_owned(), "powershell".to_owned()]);
    let parameters = json!({
        "type": "object",
        "required": ["command"],
        "properties": {
            "command": {
                "type": "string",
                "description": "PowerShell command to execute"
            },
            "timeout": {
                "type": "number",
                "description": TIMEOUT_PROPERTY
            }
        }
    });
    let description = format!(
        "Execute a PowerShell command in the current working directory. Returns combined stdout and stderr. Output is truncated to last {} lines or {}KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
        DEFAULT_MAX_LINES,
        DEFAULT_MAX_BYTES / 1024
    );
    let execute: ToolExecuteFn = Arc::new(move |args, api, context| {
        let options = options.clone();
        let programs = programs.clone();
        Box::pin(async move {
            let input: PowerShellToolInput = serde_json::from_value(args)?;
            validate_timeout(input.timeout)?;
            let execution = prepare_execution(
                &input.command,
                PrepareOptions {
                    command_prefix: options.command_prefix.as_deref(),
                    prepare: options.prepare.as_ref(),
                },
                &api,
                &context,
            )
            .await?;
            let script = format!("{UTF8_OUTPUT}\n{}", execution.command);
            let commands: Vec<ShellCommand> = programs
                .iter()
                .map(|program| {
                    let mut argv = Vec::with_capacity(POWERSHELL_ARGS.len() + 2);
                    argv.push(program.clone());
                    argv.extend(
                        POWERSHELL_ARGS
                            .iter()
                            .map(|argument| (*argument).to_owned()),
                    );
                    argv.push(script.clone());
                    ShellCommand::Argv(argv)
                })
                .collect();
            run_command(commands, &execution, input.timeout, &api, &context).await
        })
    });
    define_tool(ToolRegistration {
        tool: kiana_ai::Tool {
            name: "powershell".to_owned(),
            description,
            parameters,
            constrained_sampling: None,
        },
        replay: None,
        execution_mode: None,
        prepare_arguments: None,
        structured_output_schema: Some(shell_structured_output_schema()),
        callers: None,
        deferred: false,
        output_limits: Some(crate::harness::types::OutputLimits {
            max_bytes: None,
            max_lines: None,
            retain: Some(crate::harness::types::OutputRetain::Tail),
        }),
        execute: Some(execute),
    })
}
