//! core/tools/powershell.ts。
//!
//! `powershell` 工具：与 bash 共用 [`create_shell_tool_definition`]（不复制 shell 逻辑），
//! 只在本地操作前追加 UTF-8 输出设置前缀，并在非 Windows 平台由
//! [`kiana_core::shell::get_power_shell_config`] 报
//! `The powershell tool is only available on Windows.`。
//!
//! # 为什么复用 bash 的执行器
//!
//! 本文件需要处理的差异有三处：可执行文件与启动参数（由
//! [`kiana_core::shell::get_power_shell_config`] 提供）、进程内输出编码、以及系统提示与
//! 渲染前缀（`PS>`）。其余能力（参数 schema、超时/中止、100ms 流式更新、输出截断与临时
//! 文件、`structuredContent`）全部来自 [`create_shell_tool_definition`]，因此本文件只组装
//! [`ShellToolConfig`]，并在默认操作外包一层 UTF-8 前缀。
//!
//! # 执行流程
//!
//! 1. [`create_power_shell_tool_definition`] 把 `PowerShellToolOptions` 投影为
//!    `BashToolOptions`（`command_prefix`/`shell_path` 固定为 `None`），调用共享构造函数；
//! 2. 默认操作是 `create_local_shell_operations("PowerShell", get_power_shell_config)` 外包
//!    `Utf8PrefixOperations`：每次 exec 在用户命令前拼上 [`UTF8_OUTPUT_PREFIX`]；
//! 3. 非 Windows 平台 `get_power_shell_config` 返回
//!    `The powershell tool is only available on Windows.`，工具调用以该文本失败。
//!
//! # 注意事项
//!
//! - 调用方自带的 `PowerShellToolOptions::operations` **不会**被包上 UTF-8 前缀，也
//!   不会被沙箱包装（自定义执行器自行负责隔离）；默认操作则与 bash 工具一致地服从
//!   `KIANA_SANDBOX_POLICY`；
//! - `powershell` 不在 coding / read-only 工具集里，需要按名创建（见 `index` 模块）；
//! - `PowerShellToolDetails` 是 `BashToolDetails` 的别名，`details` 的形状与 bash 相同。

use std::sync::Arc;

use kiana_agent_core::AgentTool;
use kiana_core::shell::get_power_shell_config;
use kiana_ext_api::tool_result::{BashExecOptions, BashExecResult};
use kiana_ext_api::tool_types::ToolDefinition;

pub use crate::bash::BashOperations as PowerShellOperations;
use crate::bash::{
    BashSpawnContext, BashSpawnHook, BashToolOptions, ShellToolConfig,
    create_local_shell_operations, create_local_shell_operations_from_env,
    create_local_shell_operations_with_sandbox, create_shell_tool_definition,
};
use crate::definition::wrap_tool_definition;
pub use kiana_ext_api::tool_types::{BashToolDetails, BashToolInput};

/// `UTF8_OUTPUT_PREFIX`：PowerShell 进程内把输出编码设为 UTF-8（结尾有换行）。
///
/// 用 `try`/空 `catch` 包住设置语句：设置失败时静默跳过，不能因此中止用户命令。结尾的
/// 换行是拼接前提——调用方把 `format!("{前缀}{命令}")` 拼成一条脚本，没有换行前缀会与
/// 用户命令连在同一行。
pub const UTF8_OUTPUT_PREFIX: &str =
    "try { [Console]::OutputEncoding=[System.Text.Encoding]::UTF8 } catch {}\n";

/// `powershellToolSystemPromptContribution`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerShellToolSystemPromptContribution {
    /// 系统提示 "Available tools" 段的一行摘要。
    pub snippet: &'static str,
    /// 追加到 Guidelines 段的条目。
    pub guidelines: &'static [&'static str],
}

/// `powershellToolSystemPromptContribution`。
///
/// `snippet` 进入系统提示的 "Available tools" 段；`guidelines` 只有在
/// `PowerShellToolOptions::expose_session_environment` 为真时才会进入 `ToolDefinition`
/// （过滤发生在 [`create_shell_tool_definition`]）。测试
/// `definition_matches_the_ts_metadata` 锁定两种配置下的结果。
pub fn powershell_tool_system_prompt_contribution() -> PowerShellToolSystemPromptContribution {
    PowerShellToolSystemPromptContribution {
        snippet: "Execute PowerShell commands",
        guidelines: &[
            "You can inspect KIANA_* environment variables for current model and session details.",
        ],
    }
}

/// `PowerShellSpawnContext = BashSpawnContext`。
pub type PowerShellSpawnContext = BashSpawnContext;
/// `PowerShellSpawnHook = BashSpawnHook`。
pub type PowerShellSpawnHook = BashSpawnHook;
/// `PowerShellToolDetails = BashToolDetails`。
pub type PowerShellToolDetails = BashToolDetails;
/// `PowerShellToolInput = BashToolInput`。
pub type PowerShellToolInput = BashToolInput;

/// `PowerShellToolOptions`：`Pick<BashToolOptions, "operations" | "exposeSessionEnvironment" | "spawnHook">`。
///
/// 默认值：本地 PowerShell 操作、`expose_session_environment = true`、无 spawn hook。
/// `command_prefix` 与 `shell_path` 不在这里暴露——PowerShell 的启动参数完全由
/// `get_power_shell_config` 决定。
#[derive(Clone)]
pub struct PowerShellToolOptions {
    /// 自定义执行操作。默认本地 PowerShell；自带操作不会获得 UTF-8 前缀（见模块文档）。
    pub operations: Option<Arc<dyn PowerShellOperations>>,
    /// 是否把当前 Kiana 会话元数据暴露为 KIANA_* 环境变量。默认 `true`。
    pub expose_session_environment: bool,
    /// spawn 前调整命令/cwd/env 的 hook。
    pub spawn_hook: Option<PowerShellSpawnHook>,
    /// 本地 PowerShell 的沙箱；语义与 [`BashToolOptions::sandbox`] 一致。
    ///
    /// `None` 时由 `KIANA_SANDBOX_POLICY` 决定；显式传入会话的 `SandboxManager` 后，
    /// PowerShell 与会话里的 bash 使用同一份策略（包括 `--add-dir` 追加的可写根）。
    pub sandbox: Option<Arc<kiana_sandbox::SandboxManager>>,
}

impl Default for PowerShellToolOptions {
    fn default() -> Self {
        Self {
            operations: None,
            expose_session_environment: true,
            spawn_hook: None,
            sandbox: None,
        }
    }
}

// `operations` 与 `spawn_hook` 是 trait 对象，不能 `derive(Debug)`；只打印是否存在，
// 避免把闭包或实现细节写进日志（与 `BashToolOptions` 的手写 Debug 一致）。
impl std::fmt::Debug for PowerShellToolOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PowerShellToolOptions")
            .field("operations", &self.operations.is_some())
            .field(
                "expose_session_environment",
                &self.expose_session_environment,
            )
            .field("spawn_hook", &self.spawn_hook.is_some())
            .field("sandbox", &self.sandbox.is_some())
            .finish()
    }
}

/// 在命令前追加 [`UTF8_OUTPUT_PREFIX`] 的操作包装。
///
/// 前缀以换行结尾，直接与用户命令拼接后交给内层操作；包装只发生在默认本地操作上，调用方
/// 替换 `operations` 时不会经过这里。
struct Utf8PrefixOperations {
    inner: Arc<dyn PowerShellOperations>,
}

impl PowerShellOperations for Utf8PrefixOperations {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a str,
        options: BashExecOptions<'a>,
    ) -> kiana_agent_core::types::BoxFuture<
        'a,
        Result<BashExecResult, Box<dyn std::error::Error + Send + Sync>>,
    > {
        let command = format!("{UTF8_OUTPUT_PREFIX}{command}");
        Box::pin(async move { self.inner.exec(&command, cwd, options).await })
    }
}

/// `createLocalPowerShellOperations()`：本地执行 + UTF-8 输出前缀。
///
/// shell 配置在**每次 exec** 时通过 `get_power_shell_config` 解析（复用
/// `create_local_shell_operations` 的语义），因此非 Windows 的失败发生在调用 exec 时，
/// 而不是创建 operations 时（测试 `local_operations_parse_to_the_execution_policy_args`）。
pub fn create_local_power_shell_operations() -> Arc<dyn PowerShellOperations> {
    Arc::new(Utf8PrefixOperations {
        inner: create_local_shell_operations("PowerShell", Arc::new(get_power_shell_config)),
    })
}

/// [`create_local_power_shell_operations`] 的 `KIANA_SANDBOX_POLICY` 版本。
///
/// 与 bash 工具保持一致：设置策略后 PowerShell 命令同样经过沙箱；后端不可用时
/// fail-closed（`spawn` 返回错误），而不是静默地在宿主上执行。沙箱在第一次 exec 时
/// 才解析（工具定义在会话启动的运行时线程上构造，不能同步探测后端）。
pub fn create_local_power_shell_operations_for_cwd(
    cwd: impl AsRef<std::path::Path>,
) -> Arc<dyn PowerShellOperations> {
    Arc::new(Utf8PrefixOperations {
        inner: create_local_shell_operations_from_env(
            "PowerShell",
            Arc::new(get_power_shell_config),
            cwd.as_ref().to_string_lossy().into_owned(),
        ),
    })
}

/// powershell 的 [`ShellToolConfig`]。
///
/// 与 `bash_tool_config` 同形状：`name`/`label` 是工具标识，`shell_name` 进入工具描述，
/// `prompt` 交给 `create_shell_renderers` 决定调用行前缀（`PS>`），`temp_file_prefix`
/// 成为完整输出临时文件名的一部分。
fn powershell_tool_config() -> ShellToolConfig {
    let contribution = powershell_tool_system_prompt_contribution();
    ShellToolConfig {
        name: "powershell".to_string(),
        label: "powershell".to_string(),
        shell_name: "PowerShell".to_string(),
        prompt: "PS>".to_string(),
        prompt_snippet: contribution.snippet.to_string(),
        prompt_guidelines: Some(
            contribution
                .guidelines
                .iter()
                .map(|guideline| (*guideline).to_string())
                .collect(),
        ),
        temp_file_prefix: "kiana-powershell".to_string(),
    }
}

/// `createPowerShellToolDefinition(cwd, options?)`。
///
/// 把选项投影为 `BashToolOptions`：`operations` 缺省时用
/// [`create_local_power_shell_operations`]，`command_prefix`/`shell_path` 固定为 `None`，
/// 再交给 [`create_shell_tool_definition`]。因此参数 schema、输出 schema、
/// `temp_file_prefix = "kiana-powershell"` 与渲染器都由共享层决定。
pub fn create_power_shell_tool_definition(
    cwd: impl Into<String>,
    options: Option<PowerShellToolOptions>,
) -> ToolDefinition {
    let options = options.unwrap_or_default();
    let cwd = cwd.into();
    // 自定义 operations 自己接管执行；否则显式 manager 优先于 `KIANA_SANDBOX_POLICY`。
    // 只把 manager 传给 `BashToolOptions` 不行：这里的 operations 已经是我们构造的
    // 本地执行器，`create_shell_tool_definition` 不会再用 sandbox 重新包装。
    let operations = match (options.operations, options.sandbox) {
        (Some(operations), _) => operations,
        (None, Some(sandbox)) => create_local_shell_operations_with_sandbox(
            "PowerShell",
            Arc::new(get_power_shell_config),
            Some(sandbox),
        ),
        (None, None) => create_local_power_shell_operations_for_cwd(&cwd),
    };
    let bash_options = BashToolOptions {
        operations: Some(operations),
        command_prefix: None,
        shell_path: None,
        expose_session_environment: options.expose_session_environment,
        spawn_hook: options.spawn_hook,
        sandbox: None,
        trace_context: None,
    };
    create_shell_tool_definition(cwd, powershell_tool_config(), Some(bash_options))
}

/// `createPowerShellTool(cwd, options?)`：把定义包装成 [`AgentTool`]。
pub fn create_power_shell_tool(
    cwd: impl Into<String>,
    options: Option<PowerShellToolOptions>,
) -> AgentTool {
    wrap_tool_definition(create_power_shell_tool_definition(cwd, options), None)
}
