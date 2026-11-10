//! core/tools/bash.ts。
//!
//! `bash` 工具：命令前缀、spawn hook、KIANA_* 会话环境、100ms 节流的流式更新、
//! [`OutputAccumulator`] 的行/字节双截断与临时文件，以及面向 codemode 的
//! `structuredContent`。
//!
//! # 与 TS 的边界
//!
//! - [`BashOperations`] 是 `kiana-ext-api` 的冻结契约（§4.9），本模块只再导出；
//! - 本地执行的 100ms「exit 后空闲」宽限语义由 [`kiana_core::child_process`] 的
//!   `wait_for_child_process_observed` 统一维护；这里使用它返回的原始 `ExitStatus` 取
//!   Node 的 `child.signalCode`（信号编号），把信号终止报告为 `128 + signal`；
//! - 子进程环境按 Node `spawn({env})` 语义整体替换（`kiana-core` 的 `SpawnOptions::env`）。
//!
//! # 忠实度要点
//!
//! - 首个更新恒为 `{content: [], details: undefined}`；
//! - 流式更新只在前一块之后 ≥100ms 时发出（首个数据块是 leading edge）；
//! - `wall_time_seconds = round(ms / 100) / 10`，序列化时必须写成整数（`0` 而非 `0.0`）；
//! - 退出码 `null` 视为失败；信号终止按 `128 + signal`；
//! - 超时/中止错误文本通过 `finishOutput` 的截断脚注拼接。
//!
//! # 在 crate 中的位置
//!
//! `bash` 与 `powershell` 共用本文件的 [`create_shell_tool_definition`] 与 `ShellExecute`：
//! `powershell.rs` 在共享逻辑之外只额外提供一份 [`ShellToolConfig`] 和一层“命令前追加
//! UTF-8 设置”的 [`BashOperations`] 包装。工具定义通常由 `index` 模块的
//! `create_coding_tool_definitions` / `create_all_tool_definitions` 组装，再经
//! `crate::definition::wrap_tool_definition` 投影成 [`AgentTool`]。
//!
//! # 一次执行的阶段
//!
//! 1. `ShellExecute::execute` 解析参数、拼接 `command_prefix`、用 `effective_cwd` 决定 cwd，
//!    并由 `resolve_spawn_context` 生成 spawn hook 可见的 `{command, cwd, env}`；
//! 2. 创建 [`OutputAccumulator`]（临时文件前缀来自 [`ShellToolConfig::temp_file_prefix`]）；
//!    若存在 `on_update` 回调，先发出一个**空内容**的初始更新；
//! 3. 调用 [`BashOperations::exec`]。本地实现 `LocalShellOperations` 负责超时、中止与退出码，
//!    并在数据到达时同步调用 `on_data`；`on_data` 把数据追加进累加器，
//!    `drive_with_updates` 按 100ms 节流驱动部分更新；
//! 4. exec 返回（或出错）后 `finish_output` 停止接收、flush、发出最后一块更新并关闭临时文件；
//! 5. `format_output` 生成给模型的文本与 `details`；成功时再读完整输出（上限
//!    [`STRUCTURED_OUTPUT_MAX_BYTES`] 字节）生成 `structuredContent`。
//!
//! # 并发与锁
//!
//! 本地实现会在等待子进程的 future 被 poll 时同步调用 `on_data`（与执行本体同一任务），
//! 因此回调必须快速返回。流式回调、`execute` 本体与节流循环共享一个
//! `Mutex<SharedOutput>`；锁只保护累加器和节流状态，`on_update` 回调总在释放锁之后调用，
//! 避免回调重入或长时间持锁。互斥锁中毒时 `lock_output` 用 `into_inner()` 取回内部值继续，
//! 不把 poison 传播给调用方。
//!
//! # 注意事项
//!
//! - `timeout` 以**秒**传入本模块，[`resolve_timeout_ms`] 校验后换算成毫秒；本地实现再换
//!   算成 `tokio::time::sleep` 的期限；
//! - 本地实现的错误消息是字符串协议：`ShellExecute` 只识别 `"aborted"` 与
//!   `"timeout:<原始秒数>"` 两种文本，其余错误原样透传给模型；
//! - 临时文件写入失败不会在流式回调里立即抛出（回调没有错误通道），而是记录首个错误，
//!   在 `finish_output` 时让整个工具调用失败；
//! - 修改 [`MAX_TIMEOUT_SECONDS`] / [`STRUCTURED_OUTPUT_MAX_BYTES`] 会改变错误文案与
//!   codemode 调用者看到的输出上限，属于对外契约。

use std::ffi::OsString;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use kiana_agent_core::{AgentTool, AgentToolError, AgentToolResult, AgentToolUpdateCallback};
use kiana_ai::types::{JsonValue, TextContent, TextKind, UserContentBlock};
use kiana_core::child_process::{SpawnOptions, StdioMode, spawn_process};
use kiana_core::shell::{
    CommandTransport, ShellConfig, ShellConfigError, get_shell_config, get_shell_env,
    kill_process_tree, track_detached_child_pid, untrack_detached_child_pid,
};
pub use kiana_ext_api::tool_result::{
    BashExecOptions, BashExecResult, BashExecutionStatus, BashOperations,
};
pub use kiana_ext_api::tool_types::{BashToolDetails, BashToolInput};
use kiana_ext_api::tool_types::{ToolDefinition, ToolDefinitionExecute, ToolRenderers};
use kiana_ext_api::types::ExtensionToolContextApi;
use kiana_sandbox::{
    ExecutionPolicy, ExecutionStatus, RelayedChild, SandboxDecision, SandboxManager, SandboxPolicy,
    SpawnOptions as SandboxSpawnOptions, StdioMode as SandboxStdioMode, ToolInput,
};
use tokio::io::AsyncWriteExt;
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;

use crate::definition::wrap_tool_definition;
use crate::output_accumulator::{
    OutputAccumulator, OutputAccumulatorError, OutputAccumulatorOptions, OutputSnapshot,
};
use crate::read::effective_cwd;
use crate::renderers::bash::{BASH_UPDATE_THROTTLE_MS, create_shell_renderers};
use crate::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, format_size};

/// `MAX_TIMEOUT_MS = 2_147_483_647`（2^31-1，Node 定时器上限）。
pub const MAX_TIMEOUT_MS: f64 = 2_147_483_647.0;
/// `MAX_TIMEOUT_SECONDS = MAX_TIMEOUT_MS / 1000`（`2147483.647`）。
pub const MAX_TIMEOUT_SECONDS: f64 = 2_147_483.647;
/// `STRUCTURED_OUTPUT_MAX_BYTES = 1024 * 1024`（程序化调用者读到的上限）。
pub const STRUCTURED_OUTPUT_MAX_BYTES: usize = 1024 * 1024;

/// relay 在命令启动后被 kill 时追加的安全提示。
///
/// 与 `ExecutionStatus::Unconfirmed::user_message` 的契约一致：明确「可能已经执行」与
/// 「不要自动重试」。工具不会自动重跑命令；这段文案是给模型/用户的唯一提示。
pub const UNCONFIRMED_COMMAND_NOTE: &str = "Unconfirmed: the command may have already executed, but its exit status could not be confirmed. Do not retry it automatically; inspect the workspace first.";

/// `bashToolSystemPromptContribution` 的 Rust 形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BashToolSystemPromptContribution {
    /// 系统提示 "Available tools" 段的一行摘要。
    pub snippet: &'static str,
    /// 追加到 Guidelines 段的条目。
    pub guidelines: &'static [&'static str],
}

/// `bashToolSystemPromptContribution`。
///
/// guidelines 只在 `expose_session_environment` 为真时贡献（见
/// [`create_shell_tool_definition`]）；关闭会话环境后系统提示里不应再出现 KIANA_* 提示。
pub fn bash_tool_system_prompt_contribution() -> BashToolSystemPromptContribution {
    BashToolSystemPromptContribution {
        snippet: "Execute bash commands (ls, grep, find, etc.)",
        guidelines: &[
            "You can inspect KIANA_* environment variables for current model and session details.",
        ],
    }
}

/// 本地 shell 操作抛出的错误（`Display` 逐字等于 TS 的 `Error.message`）。
///
/// 这些消息会经 `BashOperations` 的错误通道（`Box<dyn Error>`）上浮；`ShellExecute` 按
/// 文本匹配 `"aborted"` 与 `"timeout:"` 前缀，因此这两类消息的格式不能改动。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashOperationError {
    /// 错误消息。
    pub message: String,
}

impl BashOperationError {
    /// 新建错误。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for BashOperationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for BashOperationError {}

/// `resolveTimeoutMs(timeout)`：`None` → `None`；非有限/≤0 → 错误；超过 Node 定时器上限 → 错误。
///
/// TS: `bash.ts#resolveTimeoutMs`。输入是**秒**，返回**毫秒**。上限 [`MAX_TIMEOUT_MS`]
/// 对应 Node `setTimeout` 的 2^31-1；本地执行改用 `tokio::time::sleep`，该实现本身没有
/// 这一上限，但仍按同一上限校验，以保持 `Invalid timeout` 错误行为稳定（改动需同步测试）。
pub fn resolve_timeout_ms(timeout: Option<f64>) -> Result<Option<f64>, BashOperationError> {
    let Some(timeout) = timeout else {
        return Ok(None);
    };
    if !timeout.is_finite() || timeout <= 0.0 {
        return Err(BashOperationError::new(
            "Invalid timeout: must be a finite number of seconds",
        ));
    }
    let timeout_ms = timeout * 1000.0;
    if timeout_ms > MAX_TIMEOUT_MS {
        return Err(BashOperationError::new(format!(
            "Invalid timeout: maximum is {} seconds",
            js_number_text(MAX_TIMEOUT_SECONDS)
        )));
    }
    Ok(Some(timeout_ms))
}

/// JS `String(number)`（ECMAScript 数字格式化）。
///
/// `NaN` 与 `±Infinity` 在这里单独写成 JS 的 `String()` 文本；其余值交给
/// `kiana_json::js_num::format_js_number`——它写的是 JS `JSON.stringify` 的数字文本，
/// 与 Rust `Display` 的排版不同（大数与指数形式）。
fn js_number_text(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value == f64::INFINITY {
        return "Infinity".to_string();
    }
    if value == f64::NEG_INFINITY {
        return "-Infinity".to_string();
    }
    kiana_json::js_num::format_js_number(value)
}

/// JS 对象字面量的 `delete env.KEY`。
///
/// 用 `shift_remove` 而不是 `swap_remove`：环境变量的键序要保持与 `getShellEnv()` 一致，
/// 删除后不能打乱其余键的相对顺序（spawn 时按该顺序写入子进程环境）。
fn remove_env(env: &mut IndexMap<String, String>, key: &str) {
    env.shift_remove(key);
}

/// 把 `getShellEnv()` 的 `(OsString, OsString)` 列表转成 JS 对象键序的 `IndexMap`。
///
/// 非 UTF-8 的键/值经 `to_string_lossy` 转成文本（TS 的 `process.env` 本来就是字符串），
/// 因此这里可能发生有损转换；顺序保持不变。
fn shell_env_map() -> IndexMap<String, String> {
    let mut env: IndexMap<String, String> = IndexMap::new();
    for (key, value) in get_shell_env() {
        env.insert(
            key.to_string_lossy().into_owned(),
            value.to_string_lossy().into_owned(),
        );
    }
    env
}

/// `BashSpawnContext`：spawn hook 看到的命令、cwd 与环境变量。
///
/// TS: `bash.ts#BashSpawnContext`（`env: NodeJS.ProcessEnv`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashSpawnContext {
    /// 已经拼接 `commandPrefix` 的命令。
    pub command: String,
    /// 工作目录（`ctx.cwd || cwd`）。
    pub cwd: String,
    /// 替换子进程环境（键序与 `getShellEnv()` 一致）。
    pub env: IndexMap<String, String>,
}

/// W3C trace context 提供者：每次执行前返回 `(traceparent, tracestate)`。
///
/// 返回 `None` 表示当前没有活动 trace（不注入环境变量）。由会话把当前 agent run 的
/// `SessionTelemetry::current_trace_context()` 接进来；工具层不依赖遥测 crate。
pub type TraceContextProvider = Arc<dyn Fn() -> Option<(String, Option<String>)> + Send + Sync>;

/// `BashSpawnHook`：在 spawn 前调整命令、cwd 或 env。
///
/// 由 `resolve_spawn_context` 在 `execute` 的 future 里**同步**调用一次，返回值就是最终交给
/// [`BashOperations::exec`] 的三元组；hook 可以改写任意字段（包括增删环境变量）。因为它运行
/// 在异步执行路径上，hook 应快速返回，不能阻塞等待。
pub type BashSpawnHook = Arc<dyn Fn(BashSpawnContext) -> BashSpawnContext + Send + Sync>;

/// `BashToolOptions`。
///
/// 所有字段都可缺省；`Default` 等价于“本地 bash 操作、无命令前缀、暴露会话环境、无 hook”。
/// `shell_path` 只在 `operations` 为 `None` 时被读取（见 [`create_shell_tool_definition`]）；
/// `expose_session_environment` 同时决定 KIANA_* 是否注入以及系统提示 guidelines 是否贡献。
#[derive(Clone)]
pub struct BashToolOptions {
    /// 自定义执行操作。默认本地 shell。
    pub operations: Option<Arc<dyn BashOperations>>,
    /// 每条命令前追加的命令前缀（例如 shell 初始化语句）。
    pub command_prefix: Option<String>,
    /// 设置里的显式 shell 路径。
    pub shell_path: Option<String>,
    /// 是否把当前 Kiana 会话元数据暴露为 KIANA_* 环境变量。默认 `true`。
    pub expose_session_environment: bool,
    /// spawn 前调整命令/cwd/env 的 hook。
    pub spawn_hook: Option<BashSpawnHook>,
    /// 本地 shell 的沙箱。`None` 时由 `KIANA_SANDBOX_POLICY` 决定（见
    /// [`kiana_sandbox::SandboxManager::from_env`]，在第一次 exec 时才解析）；两者都
    /// 未设置时保持原有行为。
    pub sandbox: Option<Arc<SandboxManager>>,
    /// 当前 trace context 提供者（注入 `TRACEPARENT`/`TRACESTATE` 到子进程）。
    pub trace_context: Option<TraceContextProvider>,
}

impl Default for BashToolOptions {
    fn default() -> Self {
        Self {
            operations: None,
            command_prefix: None,
            shell_path: None,
            expose_session_environment: true,
            spawn_hook: None,
            sandbox: None,
            trace_context: None,
        }
    }
}

/// 手写 `Debug`：`operations` 与 `spawn_hook` 是 trait 对象，不能 `derive(Debug)`；两者只
/// 打印是否存在，避免把闭包或操作实现带进日志。
impl std::fmt::Debug for BashToolOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BashToolOptions")
            .field("operations", &self.operations.is_some())
            .field("command_prefix", &self.command_prefix)
            .field("shell_path", &self.shell_path)
            .field(
                "expose_session_environment",
                &self.expose_session_environment,
            )
            .field("spawn_hook", &self.spawn_hook.is_some())
            .field("sandbox", &self.sandbox.is_some())
            .field("trace_context", &self.trace_context.is_some())
            .finish()
    }
}

/// `BashRenderState`：TS 渲染器的共享状态（`startedAt`/`endedAt`/`interval`）。
///
/// Rust 的渲染状态是 JSON（`kiana_ext_api::tool_types::RenderState`），`interval` 用
/// `true` 表示每秒重绘计时器存活（见 `render_utils::tool_timer_text`）。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashRenderState {
    /// 执行开始时间（毫秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<f64>,
    /// 执行结束时间（毫秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<f64>,
    /// `setInterval` 存活标记（TS 的 `NodeJS.Timeout`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<bool>,
}

/// `ShellToolConfig`：bash 与 powershell 共享的 shell 工具配置。
///
/// 由 [`create_shell_tool_definition`] 消费：`name`/`label`/`prompt_snippet` 等字段进入
/// `ToolDefinition`，`temp_file_prefix` 传给 [`OutputAccumulator`]，`prompt` 交给
/// `create_shell_renderers` 决定调用行前缀。`prompt_guidelines` 只在
/// [`BashToolOptions::expose_session_environment`] 为真时贡献给系统提示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellToolConfig {
    /// 工具名。
    pub name: String,
    /// UI 标签。
    pub label: String,
    /// 描述里使用的 shell 名（`bash` / `PowerShell`）。
    pub shell_name: String,
    /// 调用行提示符（`$` / `PS>`）。
    pub prompt: String,
    /// 系统提示摘要。
    pub prompt_snippet: String,
    /// 系统提示 guidelines；`None` 时不贡献。
    pub prompt_guidelines: Option<Vec<String>>,
    /// 临时文件前缀（`kiana-bash` / `kiana-powershell`）。
    pub temp_file_prefix: String,
}

/// `BashToolOutput`：`structuredContent`（键顺序 output/truncated/full_output_path(可选)/exit_code/wall_time_seconds）。
///
/// 这是 codemode 等程序化调用者读取的 schema（与 `bash_output_schema` 的字段一一对应）。
/// `output` 来自 [`OutputAccumulator::read_full_output`]：读取上限是
/// [`STRUCTURED_OUTPUT_MAX_BYTES`] 字节；超出时保留首尾各一半，并在中间插入省略标记。
/// `full_output_path` 只在完整输出本身超过该上限（`truncated == true`）且临时文件存在时
/// 出现；仅触发 50 KiB 显示截断并不会让它出现。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BashToolOutput {
    /// 合并后的 stdout + stderr（最多 1 MiB，超出时保留首尾）。
    pub output: String,
    /// `output` 是否省略了部分内容。
    pub truncated: bool,
    /// 完整输出所在的临时文件（仅当 `truncated` 时出现）。
    #[serde(
        rename = "full_output_path",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub full_output_path: Option<String>,
    /// 退出码。
    pub exit_code: i32,
    /// 墙钟耗时（0.1 秒精度）。
    #[serde(serialize_with = "serialize_js_f64")]
    pub wall_time_seconds: f64,
}

impl BashToolOutput {
    /// 转成 `structuredContent` 的 JSON 值（键序由结构体字段顺序保证）。
    pub fn to_json_value(&self) -> JsonValue {
        serde_json::to_value(self).unwrap_or(JsonValue::Null)
    }
}

/// 把 `f64` 按 JS `JSON.stringify` 的数字文本写出（`0` 而不是 `0.0`）。
fn serialize_js_f64<S: serde::Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
    use serde::ser::Error as _;
    let raw =
        serde_json::value::RawValue::from_string(kiana_json::js_num::format_js_number(*value))
            .map_err(S::Error::custom)?;
    serde::Serialize::serialize(&raw, serializer)
}

/// `read` 工具导出面之外，bash schema 也必须与黄金 fixture 逐字节一致（`tests/tool_definitions_golden.rs`）。
fn bash_parameters_schema() -> JsonValue {
    serde_json::json!({
        "type": "object",
        "required": ["command"],
        "properties": {
            "command": { "type": "string", "description": "Shell command to execute" },
            "timeout": { "type": "number", "description": "Timeout in seconds (optional, no default timeout)" }
        }
    })
}

/// `bashOutputSchema`（TypeBox 序列化结果，逐字节）。
fn bash_output_schema() -> JsonValue {
    serde_json::json!({
        "type": "object",
        "required": ["output", "truncated", "exit_code", "wall_time_seconds"],
        "properties": {
            "output": { "type": "string", "description": "Combined stdout and stderr, possibly truncated" },
            "truncated": { "type": "boolean" },
            "full_output_path": { "type": "string", "description": "Full output, when truncated" },
            "exit_code": { "type": "number" },
            "wall_time_seconds": { "type": "number" }
        }
    })
}

/// `{ type: "json_schema", strict: "prefer" }`。
fn json_schema_prefer() -> kiana_ai::types::ConstrainedSampling {
    kiana_ai::types::ConstrainedSampling::Config(
        kiana_ai::types::ConstrainedSamplingConfig::JsonSchema {
            strict: kiana_ai::types::StrictMode::Prefer,
        },
    )
}

// ===========================================================================
// 本地 shell 操作
// ===========================================================================

/// `createLocalShellOperations(shellName, resolveShellConfig)` 的实现。
struct LocalShellOperations {
    shell_name: String,
    resolve_shell_config: Arc<dyn Fn() -> Result<ShellConfig, ShellConfigError> + Send + Sync>,
    sandbox: SandboxSource,
}

/// 沙箱来源。
///
/// [`SandboxSource::FromEnv`] 把 [`SandboxManager::from_env`]（可能触发最多 500ms 的
/// bubblewrap 探针）推迟到第一次 exec，并在 blocking 线程上执行。工具定义在会话启动
/// （`AgentSession::build_runtime`，current-thread 运行时）时构造，不能在那里阻塞。
enum SandboxSource {
    /// 显式传入的 manager（或明确没有沙箱）。
    Fixed(Option<Arc<SandboxManager>>),
    /// 由 `KIANA_SANDBOX_POLICY` 决定；首次 exec 时解析并缓存。
    FromEnv {
        cwd: String,
        cell: OnceCell<Result<Option<Arc<SandboxManager>>, String>>,
    },
}

impl SandboxSource {
    fn fixed(sandbox: Option<Arc<SandboxManager>>) -> Self {
        Self::Fixed(sandbox)
    }

    fn from_env(cwd: impl Into<String>) -> Self {
        Self::FromEnv {
            cwd: cwd.into(),
            cell: OnceCell::new(),
        }
    }

    /// 解析沙箱。`Err` 表示解析过程本身失败（探针 panic / blocking 线程池关闭），
    /// 调用方必须 fail-closed 地拒绝执行，**不能**退化成无沙箱启动。
    async fn resolve(&self) -> Result<Option<Arc<SandboxManager>>, String> {
        match self {
            Self::Fixed(sandbox) => Ok(sandbox.clone()),
            Self::FromEnv { cwd, cell } => cell
                .get_or_init(|| async {
                    let cwd = cwd.clone();
                    match tokio::task::spawn_blocking(move || {
                        SandboxManager::from_env(&cwd).map(Arc::new)
                    })
                    .await
                    {
                        Ok(sandbox) => Ok(sandbox),
                        Err(error) => Err(format!(
                            "sandbox backend probe failed: {error}; refusing to run unsandboxed"
                        )),
                    }
                })
                .await
                .clone(),
        }
    }

    /// Debug/日志用：是否配置了沙箱来源（`FromEnv` 尚未解析时也算）。
    fn is_configured(&self) -> bool {
        match self {
            Self::Fixed(sandbox) => sandbox.is_some(),
            Self::FromEnv { .. } => true,
        }
    }
}

impl std::fmt::Debug for LocalShellOperations {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalShellOperations")
            .field("shell_name", &self.shell_name)
            .field("sandbox", &self.sandbox.is_configured())
            .finish_non_exhaustive()
    }
}

/// 一次子进程等待的结果。
struct ChildExit {
    /// `None` 表示被信号终止（Node 的 `exitCode === null`）。
    exit_code: Option<i32>,
    /// Unix 信号编号（Node 的 `signalCode`）。
    signal: Option<i32>,
}

/// 实际运行的是直接子进程还是沙箱 relay。
///
/// relay 是 `/bin/sh` 监督进程：命令的 stdout/stderr 与退出码都经它转发，但“命令到底
/// 有没有启动、有没有被外层 kill”只有 relay 的状态文件知道。非沙箱路径保持原来的
/// 直接 spawn（没有 relay，也没有 `Unconfirmed` 概念）。
enum RunningChild {
    /// 无沙箱：直接等待 shell。
    Direct(tokio::process::Child),
    /// 沙箱：等待 relay，结束后读取状态文件。
    Relayed(RelayedChild),
}

impl RunningChild {
    fn child_mut(&mut self) -> &mut tokio::process::Child {
        match self {
            Self::Direct(child) => child,
            Self::Relayed(relayed) => relayed.child_mut(),
        }
    }

    /// relay 的最终状态；直接子进程返回 `None`。
    fn relay_outcome(&self) -> Option<ExecutionStatus> {
        match self {
            Self::Direct(_) => None,
            Self::Relayed(relayed) => Some(relayed.outcome()),
        }
    }

    fn pid(&self) -> Option<u32> {
        match self {
            Self::Direct(child) => child.id(),
            Self::Relayed(relayed) => relayed.pid(),
        }
    }
}

/// 从 `ExitStatus` 取信号编号。
fn exit_status_signal(status: &std::process::ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

/// 等待子进程并流式转发 stdout/stderr。
///
/// 等待状态机（退出后每个数据块重置 100ms 空闲宽限，两个流都 EOF 时立即返回）由
/// [`kiana_core::child_process::wait_for_child_process_observed`] 维护；这里只把两个
/// 流合并回放给 `on_data`，再从原始 `ExitStatus` 取信号编号（Node 的 `signalCode`）。
async fn wait_for_child_streaming(
    child: &mut tokio::process::Child,
    on_data: &(dyn Fn(&[u8]) + Send + Sync),
) -> std::io::Result<ChildExit> {
    let status =
        kiana_core::child_process::wait_for_child_process_observed(child, |_, data| on_data(data))
            .await?;
    Ok(ChildExit {
        exit_code: status.code(),
        signal: exit_status_signal(&status),
    })
}

impl LocalShellOperations {
    /// `ExecutionPolicy::resolve` 使用的工具名。
    ///
    /// `bash` 与 `execute_bash` 都是 shell 执行路径；`powershell` 单独命名，便于策略层
    /// 未来做 per-tool 区分。
    fn execution_tool_name(&self) -> &'static str {
        if self.shell_name.eq_ignore_ascii_case("powershell") {
            "powershell"
        } else {
            "bash"
        }
    }
}

/// 沙箱状态 → 工具错误文本（保留 CamelCase 状态名，便于模型/测试识别）。
fn sandbox_status_error(status: &ExecutionStatus) -> BashOperationError {
    BashOperationError::new(status.tool_message())
}

/// 超时/中止时 relay 已经启动过命令：在原有字符串协议后附加 `|unconfirmed`，
/// 让 `ShellExecute` 在保留旧文案的同时追加「不要自动重试」提示。
fn unconfirmed_suffix(relay_outcome: Option<&ExecutionStatus>) -> &'static str {
    match relay_outcome {
        Some(status) if status.may_have_executed() => "|unconfirmed",
        _ => "",
    }
}

impl BashOperations for LocalShellOperations {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a str,
        options: BashExecOptions<'a>,
    ) -> kiana_agent_core::types::BoxFuture<
        'a,
        Result<BashExecResult, Box<dyn std::error::Error + Send + Sync>>,
    > {
        Box::pin(async move {
            // 阶段 1：timeout 校验（秒 → 毫秒）；非法值在这里直接失败，尚未创建任何进程。
            // 顺序固定：timeout 校验先于 abort 与 cwd 检查（TS 的实现顺序）。
            let timeout_ms = resolve_timeout_ms(options.timeout)?;
            // 阶段 2：已取消的调用不再 spawn（复刻 TS 在 spawn 前的 signal.aborted 检查）。
            if options
                .signal
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            {
                return Err(boxed(BashOperationError::new("aborted")));
            }
            // 阶段 3：解析 shell。自定义路径不存在等错误由 resolve_shell_config 返回，
            // 原样装箱上浮（Display 即 TS 的错误消息）。
            let shell_config = (self.resolve_shell_config)()
                .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> { Box::new(error) })?;

            // 阶段 4：cwd 必须存在；错误文本是单个 \n 的固定格式（当前稳定契约，改动需同步测试）。
            if tokio::fs::metadata(cwd).await.is_err() {
                return Err(boxed(BashOperationError::new(format!(
                    "Working directory does not exist: {cwd}\nCannot execute {} commands.",
                    self.shell_name
                ))));
            }

            // 阶段 5：组装 argv/env。Stdin 传输时命令走 stdin，否则追加为 argv 的最后一项。
            let command_from_stdin =
                shell_config.command_transport == Some(CommandTransport::Stdin);
            let mut args = shell_config.args.clone();
            if !command_from_stdin {
                args.push(command.to_string());
            }
            // 调用方（ShellExecute）总是传 Some；trait 的其他调用者传 None 时沿用
            // getShellEnv()（含把 binDir 前置到 PATH 的处理）。
            let env: Vec<(OsString, OsString)> = match &options.env {
                Some(env) => env
                    .iter()
                    .map(|(key, value)| (OsString::from(key), OsString::from(value)))
                    .collect(),
                None => get_shell_env(),
            };
            // Unix 上 detached 让子进程成为新进程组的组长，kill_process_tree 的 -pid 才能
            // 覆盖整棵进程树；Windows 用 taskkill /T，不依赖这个标志，但仍隐藏窗口。
            let spawn_options = SpawnOptions {
                cwd: Some(std::path::PathBuf::from(cwd)),
                stdin: if command_from_stdin {
                    StdioMode::Pipe
                } else {
                    StdioMode::Ignore
                },
                stdout: StdioMode::Pipe,
                stderr: StdioMode::Pipe,
                detached: !cfg!(windows),
                windows_hide: true,
                env: Some(env),
            };
            // 环境驱动的沙箱在这里才解析（blocking 线程），不在工具构造时探测。
            // 解析失败必须 fail-closed：绝不能退化成无沙箱启动。
            let sandbox = self
                .sandbox
                .resolve()
                .await
                .map_err(|error| boxed(BashOperationError::new(error)))?;
            // 探针期间可能已经被中止：spawn 之前再检查一次，避免取消后仍产生副作用。
            if options
                .signal
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            {
                return Err(boxed(BashOperationError::new("aborted")));
            }
            // 所有 shell 执行路径先过统一策略：即使没有 manager（DangerFullAccess）也
            // 走同一个纯函数决策，保证 bash / PowerShell / execute_bash / MCP 不会各自
            // 解释策略。拒绝发生在创建任何进程之前。
            let execution_policy = match &sandbox {
                Some(sandbox) => sandbox.execution_policy(),
                None => ExecutionPolicy::new(
                    SandboxPolicy::danger_full_access(),
                    std::path::PathBuf::from(cwd),
                ),
            };
            if let SandboxDecision::Deny(denial) =
                execution_policy.resolve(self.execution_tool_name(), &ToolInput::Shell { command })
            {
                return Err(boxed(BashOperationError::new(denial.message())));
            }

            let mut child = if let Some(sandbox) = &sandbox {
                let sandbox_options = SandboxSpawnOptions {
                    cwd: spawn_options.cwd.clone(),
                    stdin: sandbox_stdio(spawn_options.stdin),
                    stdout: sandbox_stdio(spawn_options.stdout),
                    stderr: sandbox_stdio(spawn_options.stderr),
                    detached: spawn_options.detached,
                    windows_hide: spawn_options.windows_hide,
                    env: spawn_options.env.clone(),
                    traceparent: None,
                    tracestate: None,
                };
                // 沙箱路径必须走 relay：`BackendUnavailable`（命令没跑）与
                // `Unconfirmed`（relay 被 kill，命令可能已经执行）是两种安全结论，
                // 不能都折叠成退出码。
                let relayed = sandbox
                    .spawn_relayed(&shell_config.shell, &args, &sandbox_options)
                    .map_err(|error| {
                        let status = ExecutionStatus::from_sandbox_error(&error);
                        boxed(sandbox_status_error(&status))
                    })?;
                RunningChild::Relayed(relayed)
            } else {
                RunningChild::Direct(
                    spawn_process(&shell_config.shell, &args, &spawn_options).map_err(
                        |error| -> Box<dyn std::error::Error + Send + Sync> { Box::new(error) },
                    )?,
                )
            };

            if command_from_stdin && let Some(mut stdin) = child.child_mut().stdin.take() {
                let command = command.to_string();
                // TS 的 `stdin.end(command)` 不阻塞等待：放到后台任务里写，避免子进程
                // 在写端缓冲区满时与输出管道互相等待。写失败（例如子进程提前退出）被忽略，
                // 与 TS 的 stdin "error" 处理器一致；最终状态由 wait 决定。
                tokio::spawn(async move {
                    let _ = stdin.write_all(command.as_bytes()).await;
                    drop(stdin);
                });
            }

            // 先登记 PID：进程退出清理（`kill_tracked_detached_children`）据此找到它；
            // 等 wait 结束后无论成功或失败都配对 untrack。
            let pid = child.pid();
            if let Some(pid) = pid {
                track_detached_child_pid(pid);
            }

            // 阶段 6：三路竞速。超时/中止只置位并 kill 进程树，循环继续等 wait 返回，
            // 以便排空管道并回收 Child；标志位防止同一分支重复触发。
            // 注意 Child 没有 kill_on_drop：只有这两个分支会主动杀进程，若整个 future 被
            // 丢弃，子进程不会被自动清理。
            //
            // `pin!` 的 future 持有 `child` 的可变借用；把它放进独立块，块结束即释放，
            // 之后才能读取 relay 状态文件。
            let (exit, timed_out, _abort_observed) = {
                let mut timed_out = false;
                let mut abort_observed = false;
                let mut wait =
                    std::pin::pin!(wait_for_child_streaming(child.child_mut(), options.on_data));
                let timeout_sleep = async {
                    match timeout_ms {
                        Some(timeout_ms) => {
                            tokio::time::sleep(Duration::from_secs_f64(timeout_ms / 1000.0)).await
                        }
                        None => std::future::pending::<()>().await,
                    }
                };
                let mut timeout_sleep = std::pin::pin!(timeout_sleep);
                let abort = async {
                    match &options.signal {
                        Some(signal) => signal.cancelled().await,
                        None => std::future::pending::<()>().await,
                    }
                };
                let mut abort = std::pin::pin!(abort);

                let exit = loop {
                    tokio::select! {
                        result = wait.as_mut() => break result,
                        _ = timeout_sleep.as_mut(), if !timed_out => {
                            timed_out = true;
                            if let Some(pid) = pid {
                                kill_process_tree(pid);
                            }
                        }
                        _ = abort.as_mut(), if !abort_observed => {
                            abort_observed = true;
                            if let Some(pid) = pid {
                                kill_process_tree(pid);
                            }
                        }
                    }
                };
                (exit, timed_out, abort_observed)
            };

            if let Some(pid) = pid {
                untrack_detached_child_pid(pid);
            }
            let exit = exit
                .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> { Box::new(error) })?;

            // 先报告中止，再报告超时：两者同时发生时 abort 优先（调用方已放弃结果，取消状态应压过超时）。
            let relay_outcome = child.relay_outcome();
            if options
                .signal
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            {
                // relay 已经被启动过命令时，中止不是「什么都没发生」：在旧协议后附加
                // `|unconfirmed`，由 ShellExecute 追加「不要自动重试」提示。
                let message = format!("aborted{}", unconfirmed_suffix(relay_outcome.as_ref()));
                return Err(boxed(BashOperationError::new(message)));
            }
            if timed_out {
                // 错误文本里的秒数是原始参数（可能带小数），ShellExecute 用
                // `split(':').nth(1)` 取回它，因此格式化必须保持 `timeout:<数字>`。
                let timeout = options.timeout.unwrap_or(0.0);
                return Err(boxed(BashOperationError::new(format!(
                    "timeout:{}{}",
                    js_number_text(timeout),
                    unconfirmed_suffix(relay_outcome.as_ref())
                ))));
            }

            // 沙箱路径的权威结论在 relay 状态文件里：只有 `ConfirmedExit` 才能相信退出码。
            if let Some(status) = relay_outcome {
                match status {
                    ExecutionStatus::ConfirmedExit { code, .. } => {
                        return Ok(BashExecResult {
                            exit_code: Some(code),
                            execution_status: Some(BashExecutionStatus::ConfirmedExit { code }),
                        });
                    }
                    // `Unconfirmed`：命令可能已经执行，工具不得自动重试；
                    // `BackendUnavailable`：命令确定没执行；`Interrupted`：启动前中断。
                    other => return Err(boxed(sandbox_status_error(&other))),
                }
            }

            // 非沙箱路径（直接 spawn）：Node 的 shell 约定——信号终止按 128 + signal
            // 报告；没有退出码时 1。
            let exit_code = exit
                .exit_code
                .unwrap_or_else(|| exit.signal.map(|signal| 128 + signal).unwrap_or(1));
            Ok(BashExecResult {
                exit_code: Some(exit_code),
                // 无沙箱路径没有 relay 的权威结论：原样保留 wait status，信号终止时
                // `code` 为 `None`（外层 `exit_code` 仍是 128 + signal）。
                execution_status: Some(BashExecutionStatus::Unsandboxed {
                    code: exit.exit_code,
                    signal: exit.signal,
                }),
            })
        })
    }
}

/// 把具体错误装箱（`BashOperations` 的错误通道）。
fn boxed(
    error: impl std::error::Error + Send + Sync + 'static,
) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(error)
}

/// Kiana `StdioMode` → `kiana-sandbox` 的 stdio 模式。
fn sandbox_stdio(mode: StdioMode) -> SandboxStdioMode {
    match mode {
        StdioMode::Ignore => SandboxStdioMode::Ignore,
        StdioMode::Pipe => SandboxStdioMode::Pipe,
        StdioMode::Inherit => SandboxStdioMode::Inherit,
    }
}

/// `createLocalShellOperations(shellName, resolveShellConfig)`。
///
/// `shell_name` 只用于
/// `Working directory does not exist: <cwd>\nCannot execute <shell_name> commands.`
/// 这条错误文本；`resolve_shell_config` 在**每次 exec** 时调用一次（而不是构造
/// 时），因此 shell 路径配置的变化会在下一次调用生效。
pub fn create_local_shell_operations(
    shell_name: impl Into<String>,
    resolve_shell_config: Arc<dyn Fn() -> Result<ShellConfig, ShellConfigError> + Send + Sync>,
) -> Arc<dyn BashOperations> {
    create_local_shell_operations_with_sandbox(shell_name, resolve_shell_config, None)
}

/// [`create_local_shell_operations`] 的带沙箱版本。
///
/// `sandbox` 为 `None` 时行为与前者完全一致。`Some` 时每次 exec 都先调用
/// [`kiana_sandbox::ExecutionPolicy::resolve`] 再通过
/// [`SandboxManager::spawn_relayed`] 启动 shell；后端不可用返回
/// `BackendUnavailable`，relay 在命令启动后被 kill 返回 `Unconfirmed`（工具提示
/// 「可能已经执行、不要自动重试」），不会静默放行、也不会自动重跑。
pub fn create_local_shell_operations_with_sandbox(
    shell_name: impl Into<String>,
    resolve_shell_config: Arc<dyn Fn() -> Result<ShellConfig, ShellConfigError> + Send + Sync>,
    sandbox: Option<Arc<SandboxManager>>,
) -> Arc<dyn BashOperations> {
    Arc::new(LocalShellOperations {
        shell_name: shell_name.into(),
        resolve_shell_config,
        sandbox: SandboxSource::fixed(sandbox),
    })
}

/// [`create_local_shell_operations_with_sandbox`] 的环境驱动版本，但把
/// [`SandboxManager::from_env`]（可能触发 bubblewrap 探针）推迟到第一次 exec。
///
/// 供工具定义工厂使用：`create_shell_tool_definition` 在会话启动时、运行时线程上被调用，
/// 不能在那里同步探测后端。第一次 exec 时在 blocking 线程里解析一次并缓存。
pub fn create_local_shell_operations_from_env(
    shell_name: impl Into<String>,
    resolve_shell_config: Arc<dyn Fn() -> Result<ShellConfig, ShellConfigError> + Send + Sync>,
    cwd: impl Into<String>,
) -> Arc<dyn BashOperations> {
    Arc::new(LocalShellOperations {
        shell_name: shell_name.into(),
        resolve_shell_config,
        sandbox: SandboxSource::from_env(cwd),
    })
}

/// `createLocalBashOperations({ shellPath }?)`。
///
/// 返回的 [`BashOperations`] 每次 exec 都用 `get_shell_config(shell_path)` 解析 shell；
/// `shell_path` 为 `None` 时走平台默认搜索顺序。
pub fn create_local_bash_operations(shell_path: Option<&str>) -> Arc<dyn BashOperations> {
    let shell_path = shell_path.map(str::to_string);
    create_local_shell_operations(
        "bash",
        Arc::new(move || get_shell_config(shell_path.as_deref())),
    )
}

/// [`create_local_bash_operations`] 的环境驱动沙箱版本。
///
/// 工具表里的 bash 由 [`create_shell_tool_definition`] 自动接线；直接调用执行器的路径
/// （`AgentSession::execute_bash`：交互式 `!cmd`、RPC）也必须服从同一份
/// `KIANA_SANDBOX_POLICY`，否则同一会话里存在「工具调用被沙箱、直接执行不被沙箱」的
/// 不一致。未设置策略时与 [`create_local_bash_operations`] 完全一致。
pub fn create_local_bash_operations_from_env(
    shell_path: Option<&str>,
    cwd: impl AsRef<std::path::Path>,
) -> Arc<dyn BashOperations> {
    let shell_path = shell_path.map(str::to_string);
    match SandboxManager::from_env(cwd).map(Arc::new) {
        Some(sandbox) => create_local_shell_operations_with_sandbox(
            "bash",
            Arc::new(move || get_shell_config(shell_path.as_deref())),
            Some(sandbox),
        ),
        None => create_local_bash_operations(shell_path.as_deref()),
    }
}

/// 取原始参数里的 `timeout`：只有 JSON 数字能通过 `Number.isFinite`。
///
/// 缺失返回 `None`；存在但不是 JSON 数字时返回 `Some(NaN)`——`Number.isFinite` 对
/// `null`/字符串/布尔/对象都为 false，`resolve_timeout_ms` 会把它们统一报成 invalid
/// timeout。只有键缺失才表示“不设超时”。
fn input_timeout(params: &JsonValue) -> Option<f64> {
    match params.as_object().and_then(|object| object.get("timeout")) {
        None => None,
        Some(JsonValue::Number(number)) => number.as_f64(),
        // `Number.isFinite(null|string|bool|object)` 均为 false → 本地操作报 invalid timeout。
        Some(_) => Some(f64::NAN),
    }
}

/// 解析工具参数（TS 的 schema 在更早的管线里校验）。
///
/// 这里对缺失/类型错误一律回落到缺省值（`command` 为空串、`timeout` 缺失），不返回错误；
/// 入参是已擦除的 `JsonValue`，函数本身不假设调用方一定已经按 schema 校验过。
fn parse_tool_input(params: &JsonValue) -> BashToolInput {
    BashToolInput {
        command: params
            .as_object()
            .and_then(|object| object.get("command"))
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string(),
        timeout: input_timeout(params),
    }
}

/// `resolveSpawnContext(command, cwd, spawnHook, exposeSessionEnvironment, ctx)`。
///
/// 环境变量的构造顺序固定：
/// 1. 取 `getShellEnv()` 的完整环境快照；
/// 2. 先删除 5 个 `KIANA_*` 键——即使本次不注入，hook 也看不到宿主进程里残留的旧值；
/// 3. 只有当 `expose_session_environment` 为真且 ctx 不是占位上下文时才写入会话 ID、
///    会话文件（非空时）、provider/model（`ctx.model()` 存在时）与推理等级；
/// 4. 最后调用 `spawn_hook`，返回值即最终 spawn 上下文。
///
/// 占位判断用 `crate::definition::is_missing_tool_context` 复刻 TS 里 `ctx === undefined`；
/// 占位上下文没有真实会话（session id 为空、model/thinking level 为 `None`），直接注入会
/// 把虚假的 `KIANA_*` 值暴露给命令。
fn resolve_spawn_context(
    command: String,
    cwd: String,
    spawn_hook: Option<&BashSpawnHook>,
    expose_session_environment: bool,
    ctx: &Arc<dyn ExtensionToolContextApi>,
) -> BashSpawnContext {
    let mut env = shell_env_map();
    for key in [
        "KIANA_SESSION_ID",
        "KIANA_SESSION_FILE",
        "KIANA_PROVIDER",
        "KIANA_MODEL",
        "KIANA_REASONING_LEVEL",
    ] {
        remove_env(&mut env, key);
    }
    if expose_session_environment && !crate::definition::is_missing_tool_context(ctx) {
        let session_file = ctx.session_manager().get_session_file().map(str::to_string);
        env.insert(
            "KIANA_SESSION_ID".to_string(),
            ctx.session_manager().get_session_id().to_string(),
        );
        if let Some(session_file) = session_file
            && !session_file.is_empty()
        {
            env.insert("KIANA_SESSION_FILE".to_string(), session_file);
        }
        if let Some(model) = ctx.model() {
            env.insert("KIANA_PROVIDER".to_string(), model.provider);
            env.insert("KIANA_MODEL".to_string(), model.id);
        }
        if let Some(thinking_level) = ctx.thinking_level() {
            env.insert(
                "KIANA_REASONING_LEVEL".to_string(),
                thinking_level_text(thinking_level),
            );
        }
    }
    let context = BashSpawnContext { command, cwd, env };
    match spawn_hook {
        Some(hook) => hook(context),
        None => context,
    }
}

/// `ModelThinkingLevel` 的字符串形式（serde 与实际线格式一致）。
fn thinking_level_text(level: kiana_ai::types::ModelThinkingLevel) -> String {
    serde_json::to_value(level)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

// ===========================================================================
// 流式更新状态
// ===========================================================================

/// 执行期间的共享输出状态。
///
/// 由流式回调（写）与 `execute`/节流循环（读写）共享；所有字段都在同一个互斥锁下访问，
/// 因此 `accepting`/`dirty`/`last_update_at` 的检查与更新是原子的。
struct SharedOutput {
    /// 累加器；`finish_output` 会把它 `take()` 走（之后为 `None`）。
    accumulator: Option<OutputAccumulator>,
    /// `false` 后到达的数据块被丢弃（`finish_output` 置位）。
    accepting: bool,
    /// 有尚未发出的部分更新（节流合并用）。
    dirty: bool,
    /// 上一次发出更新的时刻；`None` 表示还没发过，第一个数据块走 leading edge。
    last_update_at: Option<Instant>,
    /// [`OutputAccumulator::append`] 的首个错误。
    ///
    /// TS 的 `handleData` 在 `output.append(data)` 抛错时直接抛出（临时文件创建/写入失败，
    /// 例如 TMPDIR 不存在或磁盘已满）。Rust 的流式回调没有错误通道，因此先记录，由
    /// [`finish_output`] 在调用结束时让整个工具调用失败（绝不能在错误状态下返回成功结果）。
    error: Option<OutputAccumulatorError>,
}

/// 取共享状态锁；互斥锁中毒时用 `into_inner()` 继续，而不是把 poison 传播给调用方。
fn lock_output(output: &Arc<Mutex<SharedOutput>>) -> MutexGuard<'_, SharedOutput> {
    output
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 把快照转成流式更新的 [`AgentToolResult`]。
///
/// `details` 是 `{truncation?, fullOutputPath?}`：`truncation` 只在已截断时出现，
/// `fullOutputPath` 只在临时文件已创建时出现（TS 的 `undefined` 键会被省略）。更新不是
/// 最终结果，`structured_content`/`usage`/`is_error`/`terminate` 都留空。
fn update_result(snapshot: &OutputSnapshot) -> AgentToolResult<JsonValue> {
    let mut details = serde_json::Map::new();
    if snapshot.truncation.truncated
        && let Ok(value) = serde_json::to_value(&snapshot.truncation)
    {
        details.insert("truncation".to_string(), value);
    }
    if let Some(full_output_path) = &snapshot.full_output_path {
        details.insert(
            "fullOutputPath".to_string(),
            JsonValue::String(full_output_path.clone()),
        );
    }
    AgentToolResult {
        content: vec![text_block(snapshot.content.clone())],
        details: JsonValue::Object(details),
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    }
}

/// `emitOutputUpdate()`：只在 `updateDirty` 时发出，不检查节流延迟。
///
/// 调用方（`drive_with_updates` 的计时器或 `finish_output`）已经决定该发。锁内只做
/// `dirty` 翻转与快照，`on_update` 在锁外执行。
fn emit_output_update(output: &Arc<Mutex<SharedOutput>>, on_update: &AgentToolUpdateCallback) {
    let snapshot = {
        let mut state = lock_output(output);
        if !state.dirty {
            return;
        }
        state.dirty = false;
        state.last_update_at = Some(Instant::now());
        let Some(accumulator) = state.accumulator.as_mut() else {
            return;
        };
        accumulator.snapshot(true)
    };
    on_update(update_result(&snapshot));
}

/// `finishOutput()`：停止接收、flush、发出最后一块更新、取快照并关闭临时文件。
///
/// 与 TS 相同，`finish()` / `closeTempFile()` 的 I/O 错误会让整个工具调用失败（不得返回
/// 一个指向永远不会存在的临时文件的「成功」结果）。`append` 期间记录的首个错误优先
/// （TS 在 `handleData` 里就是那一刻抛出的）。
///
/// 顺序：`accepting = false` → `finish()`（flush 解码器尾字节并确保临时文件）→ 若 dirty
/// 且有回调则发出最后一块更新 → `snapshot(true)` → `close_temp_file()`。错误优先级为
/// `append` 记录的错误 > `finish()` 错误 > `close_temp_file()` 错误。
///
/// 调用前提：共享状态里仍有 accumulator，且本函数只被调用一次。`execute` 的错误路径与
/// 成功路径互斥，各自最多调用一次；违反前提会命中 `.expect(...)` 而 panic。
async fn finish_output(
    output: &Arc<Mutex<SharedOutput>>,
    on_update: Option<&AgentToolUpdateCallback>,
) -> Result<(OutputSnapshot, OutputAccumulator), OutputAccumulatorError> {
    let (mut accumulator, error, pending_update) = {
        let mut state = lock_output(output);
        state.accepting = false;
        let mut accumulator = state
            .accumulator
            .take()
            .expect("finish_output is called once after the accumulator was created");
        let recorded_error = state.error.take();
        let finish_error = accumulator.finish().err();
        let pending_update = if on_update.is_some() && state.dirty {
            state.dirty = false;
            state.last_update_at = Some(Instant::now());
            Some(accumulator.snapshot(true))
        } else {
            None
        };
        (accumulator, recorded_error.or(finish_error), pending_update)
    };
    if let (Some(on_update), Some(snapshot)) = (on_update, pending_update) {
        on_update(update_result(&snapshot));
    }
    let snapshot = accumulator.snapshot(true);
    let close_error = accumulator.close_temp_file().await.err();
    if let Some(error) = error.or(close_error.map(OutputAccumulatorError::Io)) {
        return Err(error);
    }
    Ok((snapshot, accumulator))
}

/// 把累加器错误转成工具错误（Node 风格的消息，见 [`node_fs_error_message`]）。
fn accumulator_tool_error(error: OutputAccumulatorError) -> AgentToolError {
    match error {
        OutputAccumulatorError::Finished => tool_error(error.to_string()),
        OutputAccumulatorError::Io(error) => tool_error(node_fs_error_message(error)),
    }
}

/// 把临时文件的 `std::io::Error` 转成 Node `uvException()` 的文案
/// （`ENOENT: no such file or directory, open`）。
///
/// `kiana_core::output_files::create_output_file_stream` 失败时不回报生成的路径，因此消息里
/// 省略 Node 错误末尾的 ` '<path>'`（其余逐字一致）。
///
/// `std::io::Error` 不携带“失败发生在 open 还是 write”，这里按 errno 推断：ENOSPC、
/// EDQUOT、EFBIG、EPIPE 归为 `write`，其余归为 `open`（与测试
/// `accumulator_io_errors_use_node_fs_messages` 锁定的文案一致）。
fn node_fs_error_message(error: std::io::Error) -> String {
    // 打开失败与写入失败在 Node 里的 syscall 名不同；按 errno 区分常见情形。
    #[cfg(unix)]
    let operation = match error.raw_os_error() {
        Some(libc::ENOSPC | libc::EDQUOT | libc::EFBIG | libc::EPIPE) => "write",
        _ => "open",
    };
    #[cfg(not(unix))]
    let operation = "open";
    crate::read::ToolOperationError::from_io_parts(operation, None, error).message
}

/// 在 `ops.exec` 运行期间按 100ms 节流发出部分更新。
///
/// 每轮先看共享状态：`dirty` 为假时等待 `notify`（数据到达）或 exec 结束；`dirty` 为真时
/// 等到 `last_update_at + 100ms` 再 `emit_output_update`。`last_update_at == None` 的延迟
/// 是 0，使第一个数据块立即发出（leading edge）；之后的块被合并到下一个窗口。
///
/// `Notify` 只用来唤醒“当前没有待发更新”的分支，不携带数据；真正的数据在 `SharedOutput`
/// 里，所以通知合并（或错过一次通知）不会丢快照——下一轮循环会重新读取状态。返回 exec
/// future 的输出；更新循环本身不产生错误。
async fn drive_with_updates<F>(
    output: Arc<Mutex<SharedOutput>>,
    notify: Arc<tokio::sync::Notify>,
    on_update: AgentToolUpdateCallback,
    exec: F,
) -> F::Output
where
    F: std::future::Future,
{
    let mut exec = std::pin::pin!(exec);
    loop {
        let delay = {
            let state = lock_output(&output);
            if state.dirty {
                Some(match state.last_update_at {
                    None => Duration::ZERO,
                    Some(last) => (last + Duration::from_millis(BASH_UPDATE_THROTTLE_MS))
                        .saturating_duration_since(Instant::now()),
                })
            } else {
                None
            }
        };
        match delay {
            Some(delay) => {
                tokio::select! {
                    result = exec.as_mut() => break result,
                    _ = tokio::time::sleep(delay) => emit_output_update(&output, &on_update),
                }
            }
            None => {
                tokio::select! {
                    result = exec.as_mut() => break result,
                    _ = notify.notified() => {}
                }
            }
        }
    }
}

// ===========================================================================
// 结果格式化
// ===========================================================================

/// 一个 text 内容块。
fn text_block(text: String) -> UserContentBlock {
    UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text,
        text_signature: None,
    })
}

/// `formatOutput(snapshot, emptyText)`。
///
/// 返回（给模型的文本, 可选 details）。`content` 为空时用 `empty_text` 占位（主流程传
/// `"(no output)"`，错误路径传 `""`，因此错误只剩状态行）。发生截断时把
/// `BashToolDetails` 放进 details，并按三种情形追加脚注：
/// - `last_line_partial`：最后一行被字节上限截断，脚注给出行号与 `output_bytes`；
/// - `truncated_by == Lines`：只给出行区间；
/// - 其余（普通字节截断）：额外写出 `DEFAULT_MAX_BYTES` 的 `format_size` 文本。
///
/// `last_line_bytes` 来自 [`OutputAccumulator::get_last_line_bytes`]，是当前未结束行的
/// 字节数；只有 `last_line_partial` 分支会用到它。
fn format_output(
    snapshot: &OutputSnapshot,
    last_line_bytes: usize,
    empty_text: &str,
) -> (String, Option<BashToolDetails>) {
    let truncation = &snapshot.truncation;
    let mut text = if snapshot.content.is_empty() {
        empty_text.to_string()
    } else {
        snapshot.content.clone()
    };
    let mut details: Option<BashToolDetails> = None;
    if truncation.truncated {
        details = Some(BashToolDetails {
            truncation: Some(truncation.clone()),
            full_output_path: snapshot.full_output_path.clone(),
        });
        let start_line = truncation.total_lines - truncation.output_lines + 1;
        let end_line = truncation.total_lines;
        let full_output_path = snapshot.full_output_path.as_deref().unwrap_or("undefined");
        if truncation.last_line_partial {
            let last_line_size = format_size(last_line_bytes as u64);
            text.push_str(&format!(
                "\n\n[Showing last {} of line {end_line} (line is {last_line_size}). Full output: {full_output_path}]",
                format_size(truncation.output_bytes)
            ));
        } else if truncation.truncated_by == Some(kiana_ext_api::tool_types::TruncatedBy::Lines) {
            text.push_str(&format!(
                "\n\n[Showing lines {start_line}-{end_line} of {}. Full output: {full_output_path}]",
                truncation.total_lines
            ));
        } else {
            text.push_str(&format!(
                "\n\n[Showing lines {start_line}-{end_line} of {} ({} limit). Full output: {full_output_path}]",
                truncation.total_lines,
                format_size(DEFAULT_MAX_BYTES as u64)
            ));
        }
    }
    (text, details)
}

/// `appendStatus(text, status)`。
fn append_status(text: &str, status: &str) -> String {
    if text.is_empty() {
        status.to_string()
    } else {
        format!("{text}\n\n{status}")
    }
}

/// 详情 → `AgentToolResult.details`。
fn details_value(details: Option<BashToolDetails>) -> JsonValue {
    match details {
        Some(details) => serde_json::to_value(details).unwrap_or(JsonValue::Null),
        None => JsonValue::Null,
    }
}

// ===========================================================================
// 工具执行
// ===========================================================================

/// shell 工具的 `execute` 实现（bash 与 powershell 共用）。
///
/// 只持有不随单次调用变化的配置；`OutputAccumulator`、`SharedOutput`、`Notify` 等可变
/// 状态都在 `execute` 内新建，`&self` 上没有跨调用共享的可变字段。`cwd` 是构造时的缺省
/// 目录，实际执行目录由 `effective_cwd` 优先取上下文 cwd。
struct ShellExecute {
    cwd: String,
    operations: Arc<dyn BashOperations>,
    command_prefix: Option<String>,
    expose_session_environment: bool,
    spawn_hook: Option<BashSpawnHook>,
    trace_context: Option<TraceContextProvider>,
    temp_file_prefix: String,
}

impl ToolDefinitionExecute for ShellExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: JsonValue,
        signal: Option<&'a CancellationToken>,
        on_update: Option<AgentToolUpdateCallback>,
        ctx: Arc<dyn ExtensionToolContextApi>,
    ) -> kiana_agent_core::types::BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>>
    {
        Box::pin(async move {
            // 阶段 1：参数与 spawn 上下文。命令前缀、cwd 覆盖与 KIANA_* 注入都在这里定稿，
            // 之后的 hook 是最后一次改写机会。
            let input = parse_tool_input(&params);
            // `commandPrefix ? ... : command`：空字符串在 TS 里是假值，视为未配置。
            let resolved_command = match &self.command_prefix {
                Some(prefix) if !prefix.is_empty() => format!("{prefix}\n{}", input.command),
                _ => input.command.clone(),
            };
            let cwd = effective_cwd(&ctx, &self.cwd);
            let spawn_context = resolve_spawn_context(
                resolved_command,
                cwd,
                self.spawn_hook.as_ref(),
                self.expose_session_environment,
                &ctx,
            );

            // 阶段 2：每次调用独立的输出状态；临时文件只在截断时创建，前缀决定文件名。
            let output = Arc::new(Mutex::new(SharedOutput {
                accumulator: Some(OutputAccumulator::new(OutputAccumulatorOptions {
                    temp_file_prefix: Some(self.temp_file_prefix.clone()),
                    ..OutputAccumulatorOptions::default()
                })),
                accepting: true,
                dirty: false,
                last_update_at: None,
                error: None,
            }));
            // `Notify` 只负责把节流循环从“空闲等待”唤醒；数据本身在 `SharedOutput` 里。
            let notify = Arc::new(tokio::sync::Notify::new());
            // 数据回调：锁内追加并按需生成快照，锁外调用 `on_update`。
            let handle_data = {
                let output = Arc::clone(&output);
                let notify = Arc::clone(&notify);
                let on_update = on_update.clone();
                move |data: &[u8]| {
                    // TS 的 `scheduleOutputUpdate`：`delay = 100 - (Date.now() - lastUpdateAt)`，
                    // 而 `lastUpdateAt` 初始为 0，所以第一个数据块在 `handleData` 内同步发出
                    // （leading edge），即使 `ops.exec` 从不 await。其余数据块由
                    // `drive_with_updates` 的 100ms 计时器合并。
                    let snapshot = {
                        let mut state = lock_output(&output);
                        if !state.accepting {
                            return;
                        }
                        if state.error.is_none() {
                            let Some(accumulator) = state.accumulator.as_mut() else {
                                return;
                            };
                            if let Err(error) = accumulator.append(data) {
                                // TS 在这里同步抛出；流式回调没有错误通道，记录后由
                                // `finish_output` 让整个调用失败。
                                state.error = Some(error);
                                return;
                            }
                        }
                        if on_update.is_none() {
                            return;
                        }
                        state.dirty = true;
                        let delay = match state.last_update_at {
                            None => Duration::ZERO,
                            Some(last) => (last + Duration::from_millis(BASH_UPDATE_THROTTLE_MS))
                                .saturating_duration_since(Instant::now()),
                        };
                        if delay.is_zero() {
                            state.dirty = false;
                            state.last_update_at = Some(Instant::now());
                            state
                                .accumulator
                                .as_mut()
                                .map(|accumulator| accumulator.snapshot(true))
                        } else {
                            None
                        }
                    };
                    if let (Some(on_update), Some(snapshot)) = (&on_update, snapshot) {
                        on_update(update_result(&snapshot));
                    }
                    notify.notify_one();
                }
            };

            // 首个更新恒为空内容（`tool-execution-component.test.ts`）。
            if let Some(on_update) = &on_update {
                on_update(AgentToolResult {
                    content: Vec::new(),
                    details: JsonValue::Null,
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                });
            }

            // 墙钟计时从调用 ops.exec 之前开始，不包含参数解析、cwd/hook 与初始空更新。
            let started_at = Instant::now();
            let timeout = input.timeout;
            // 第 9 周：把当前（agent run/turn）trace context 注入子进程环境。
            // `TRACEPARENT`/`TRACESTATE` 在 env 替换之后仍是最终值，直接与 KIANA_* 同列。
            let mut exec_env = spawn_context.env.clone();
            if let Some(provider) = &self.trace_context
                && let Some((traceparent, tracestate)) = provider()
            {
                exec_env.insert("TRACEPARENT".to_string(), traceparent);
                match tracestate {
                    Some(tracestate) => {
                        exec_env.insert("TRACESTATE".to_string(), tracestate);
                    }
                    None => {
                        exec_env.shift_remove("TRACESTATE");
                    }
                }
            }
            let exec = self.operations.exec(
                &spawn_context.command,
                &spawn_context.cwd,
                BashExecOptions {
                    on_data: &handle_data,
                    signal: signal.cloned(),
                    timeout,
                    env: Some(exec_env),
                },
            );

            // 阶段 3：执行并驱动更新。有更新回调时用节流循环推进 exec；没有回调时直接
            // await，省掉计时器与通知。
            let exec_result = match &on_update {
                Some(on_update) => {
                    drive_with_updates(
                        Arc::clone(&output),
                        Arc::clone(&notify),
                        Arc::clone(on_update),
                        exec,
                    )
                    .await
                }
                None => exec.await,
            };

            let exit_code = match exec_result {
                Ok(result) => result.exit_code,
                // 阶段 4a：exec 失败。先收尾输出（把已产生的部分内容放进错误文本），
                // 再按 TS 的字符串协议区分 aborted / timeout / 其他错误。
                Err(error) => {
                    let message = error.to_string();
                    let (snapshot, accumulator) =
                        match finish_output(&output, on_update.as_ref()).await {
                            Ok(value) => value,
                            // TS：catch 里的 `await finishOutput()` 若抛错，会取代原始错误。
                            Err(error) => return Err(accumulator_tool_error(error)),
                        };
                    let (text, _) = format_output(&snapshot, accumulator.get_last_line_bytes(), "");
                    // 只识别本地实现的两种协议文本；自定义 operations 的其他消息原样透传。
                    // `|unconfirmed` 后缀表示 relay 在命令启动后被 kill：文案仍沿用旧格式，
                    // 但必须追加「可能已经执行、不要自动重试」的安全提示。
                    if message == "aborted" || message.starts_with("aborted|") {
                        let mut text = append_status(&text, "Command aborted");
                        if message.contains("|unconfirmed") {
                            text = append_status(&text, UNCONFIRMED_COMMAND_NOTE);
                        }
                        return Err(tool_error(text));
                    }
                    if let Some(rest) = message.strip_prefix("timeout:") {
                        let (timeout_secs, suffix) = rest.split_once('|').unwrap_or((rest, ""));
                        let mut text = append_status(
                            &text,
                            &format!("Command timed out after {timeout_secs} seconds"),
                        );
                        if suffix.contains("unconfirmed") {
                            text = append_status(&text, UNCONFIRMED_COMMAND_NOTE);
                        }
                        return Err(tool_error(text));
                    }
                    return Err(tool_error(message));
                }
            };

            // 阶段 4b：exec 正常返回。无论退出码是否为 0，都先收尾并格式化输出。
            let (snapshot, accumulator) = finish_output(&output, on_update.as_ref())
                .await
                .map_err(accumulator_tool_error)?;
            let (output_text, details) =
                format_output(&snapshot, accumulator.get_last_line_bytes(), "(no output)");
            let Some(exit_code) = exit_code else {
                return Err(tool_error(append_status(
                    &output_text,
                    "Command terminated without an exit code",
                )));
            };

            // 阶段 5：structuredContent。`read_full_output` 最多读 1 MiB，超出时保留首尾；
            // `full_output_path` 只在真的截断时暴露（即使临时文件存在，未截断也不给出）。
            let wall_time_seconds =
                (((started_at.elapsed().as_millis() as f64) / 100.0).round()) / 10.0;
            let full_output = accumulator
                .read_full_output(STRUCTURED_OUTPUT_MAX_BYTES)
                .await
                .map_err(|error| tool_error(error.to_string()))?;
            let structured_content = BashToolOutput {
                output: full_output.content,
                truncated: full_output.truncated,
                full_output_path: if full_output.truncated {
                    snapshot.full_output_path.clone()
                } else {
                    None
                },
                exit_code,
                wall_time_seconds,
            }
            .to_json_value();

            // 非零退出码不是 `Err`：返回 `is_error: Some(true)` 的结果，同时保留
            // structuredContent 与截断 details，让上层既能重试/展示又能读取机器可读输出。
            if exit_code != 0 {
                return Ok(AgentToolResult {
                    content: vec![text_block(append_status(
                        &output_text,
                        &format!("Command exited with code {exit_code}"),
                    ))],
                    details: details_value(details),
                    structured_content: Some(structured_content),
                    usage: None,
                    is_error: Some(true),
                    terminate: None,
                });
            }
            Ok(AgentToolResult {
                content: vec![text_block(output_text)],
                details: details_value(details),
                structured_content: Some(structured_content),
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

/// 工具错误（`Error.message` 逐字）。
fn tool_error(message: impl Into<String>) -> AgentToolError {
    AgentToolError {
        message: message.into(),
        name: None,
        stack: None,
    }
}

// ===========================================================================
// 定义与工厂
// ===========================================================================

/// `createShellToolDefinition(cwd, config, options?)`。
///
/// 组装 bash/powershell 的 `ToolDefinition`：
/// - `operations` 缺省时用 `create_local_bash_operations(options.shell_path)`（powershell 由
///   调用方传入自己的 operations）；
/// - `description` 由 `config.shell_name` 与 [`DEFAULT_MAX_LINES`]/[`DEFAULT_MAX_BYTES`] 代入；
/// - `parameters`/`output_schema` 是固定的 schema（`bash_parameters_schema` /
///   `bash_output_schema`），`constrained_sampling` 为 `{type:"json_schema", strict:"prefer"}`；
/// - `prompt_guidelines` 只在 `expose_session_environment` 为真时取自 config；
/// - 渲染器由 `create_shell_renderers(config.prompt)` 生成。
pub fn create_shell_tool_definition(
    cwd: impl Into<String>,
    config: ShellToolConfig,
    options: Option<BashToolOptions>,
) -> ToolDefinition {
    let cwd = cwd.into();
    let options = options.unwrap_or_default();
    // 显式传入的 manager 优先；否则由 KIANA_SANDBOX_POLICY 决定，但推迟到首次 exec
    // 才解析（构造发生在会话启动的运行时线程上，不能同步跑 bubblewrap 探针）。
    // 自定义 operations 已经接管执行，不需要沙箱。
    let operations = options.operations.clone().unwrap_or_else(|| {
        let shell_path = options.shell_path.clone();
        let resolve_shell_config = Arc::new(move || get_shell_config(shell_path.as_deref()))
            as Arc<dyn Fn() -> Result<ShellConfig, ShellConfigError> + Send + Sync>;
        match options.sandbox.clone() {
            Some(sandbox) => create_local_shell_operations_with_sandbox(
                config.shell_name.clone(),
                resolve_shell_config,
                Some(sandbox),
            ),
            None => create_local_shell_operations_from_env(
                config.shell_name.clone(),
                resolve_shell_config,
                cwd.clone(),
            ),
        }
    });
    let expose_session_environment = options.expose_session_environment;
    let prompt_guidelines = if expose_session_environment {
        config.prompt_guidelines.clone()
    } else {
        None
    };
    let renderers: ToolRenderers = create_shell_renderers(&config.prompt);
    ToolDefinition {
        name: config.name.clone(),
        label: config.label.clone(),
        description: format!(
            "Execute a {} command in the current working directory. Returns stdout and stderr. Output is truncated to last {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
            config.shell_name,
            DEFAULT_MAX_BYTES / 1024
        ),
        prompt_snippet: Some(config.prompt_snippet.clone()),
        prompt_guidelines,
        parameters: bash_parameters_schema(),
        constrained_sampling: Some(json_schema_prefer()),
        render_shell: renderers.render_shell,
        prepare_arguments: None,
        output_schema: Some(bash_output_schema()),
        exposure: None,
        namespace: None,
        reaches_tools: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(ShellExecute {
            cwd,
            operations,
            command_prefix: options.command_prefix,
            expose_session_environment,
            spawn_hook: options.spawn_hook,
            trace_context: options.trace_context,
            temp_file_prefix: config.temp_file_prefix,
        }),
        render_call: renderers.render_call,
        render_result: renderers.render_result,
    }
}

/// bash 的 [`ShellToolConfig`]。
fn bash_tool_config() -> ShellToolConfig {
    let contribution = bash_tool_system_prompt_contribution();
    ShellToolConfig {
        name: "bash".to_string(),
        label: "bash".to_string(),
        shell_name: "bash".to_string(),
        prompt: "$".to_string(),
        prompt_snippet: contribution.snippet.to_string(),
        prompt_guidelines: Some(
            contribution
                .guidelines
                .iter()
                .map(|guideline| (*guideline).to_string())
                .collect(),
        ),
        temp_file_prefix: "kiana-bash".to_string(),
    }
}

/// `createBashToolDefinition(cwd, options?)`。
///
/// 等价于 `create_shell_tool_definition(cwd, bash_tool_config(), options)`；配置里的 `prompt`
/// 是 `$`，临时文件前缀是 `kiana-bash`。
pub fn create_bash_tool_definition(
    cwd: impl Into<String>,
    options: Option<BashToolOptions>,
) -> ToolDefinition {
    create_shell_tool_definition(cwd, bash_tool_config(), options)
}

/// `createBashTool(cwd, options?)`。
///
/// TS 的 `Object.assign(tool, {promptSnippet, promptGuidelines})` 在 Rust 里没有对应字段
/// （`AgentTool` 是冻结的瘦视图）；系统提示构建在 Rust 侧读取 `ToolDefinition`，因此这里
/// 只做 `wrapToolDefinition` 投影，不丢任何可观察行为。
pub fn create_bash_tool(cwd: impl Into<String>, options: Option<BashToolOptions>) -> AgentTool {
    wrap_tool_definition(create_bash_tool_definition(cwd, options), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_validation_matches_ts() {
        assert_eq!(resolve_timeout_ms(None).unwrap(), None);
        assert_eq!(resolve_timeout_ms(Some(2.0)).unwrap(), Some(2000.0));
        assert_eq!(
            resolve_timeout_ms(Some(0.0)).unwrap_err().message,
            "Invalid timeout: must be a finite number of seconds"
        );
        assert_eq!(
            resolve_timeout_ms(Some(f64::NAN)).unwrap_err().message,
            "Invalid timeout: must be a finite number of seconds"
        );
        assert_eq!(
            resolve_timeout_ms(Some(f64::INFINITY)).unwrap_err().message,
            "Invalid timeout: must be a finite number of seconds"
        );
        assert_eq!(
            resolve_timeout_ms(Some(MAX_TIMEOUT_SECONDS + 1.0))
                .unwrap_err()
                .message,
            "Invalid timeout: maximum is 2147483.647 seconds"
        );
    }

    #[test]
    fn max_timeout_seconds_prints_with_js_number_formatting() {
        assert_eq!(js_number_text(MAX_TIMEOUT_SECONDS), "2147483.647");
    }

    #[test]
    fn structured_content_numbers_are_python_style_integers() {
        let output = BashToolOutput {
            output: "x".to_string(),
            truncated: false,
            full_output_path: None,
            exit_code: 0,
            wall_time_seconds: 0.0,
        };
        let json = serde_json::to_string(&output).expect("serialize");
        assert!(json.contains("\"wall_time_seconds\":0"));
        assert!(!json.contains("0.0"));
    }

    #[cfg(unix)]
    #[test]
    fn accumulator_io_errors_use_node_fs_messages() {
        // TS 的临时文件错误是 Node `uvException()` 文案；`createOutputFileStream` 不回报
        // 失败时的路径，因此省略末尾的 ` '<path>'`。
        let error = accumulator_tool_error(OutputAccumulatorError::Io(
            std::io::Error::from_raw_os_error(libc::ENOENT),
        ));
        assert_eq!(error.message, "ENOENT: no such file or directory, open");
        let error = accumulator_tool_error(OutputAccumulatorError::Io(
            std::io::Error::from_raw_os_error(libc::ENOSPC),
        ));
        assert_eq!(error.message, "ENOSPC: no space left on device, write");
    }

    struct CaptureEnv(Arc<std::sync::Mutex<Option<std::collections::BTreeMap<String, String>>>>);

    impl BashOperations for CaptureEnv {
        fn exec<'a>(
            &'a self,
            _command: &'a str,
            _cwd: &'a str,
            options: BashExecOptions<'a>,
        ) -> kiana_agent_core::types::BoxFuture<
            'a,
            Result<BashExecResult, Box<dyn std::error::Error + Send + Sync>>,
        > {
            let captured = options.env.as_ref().map(|env| {
                env.iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            });
            *self.0.lock().unwrap() = captured;
            Box::pin(async {
                Ok(BashExecResult {
                    exit_code: Some(0),
                    execution_status: None,
                })
            })
        }
    }

    type CapturedEnvironment =
        Arc<std::sync::Mutex<Option<std::collections::BTreeMap<String, String>>>>;

    fn trace_tool(trace_context: Option<TraceContextProvider>) -> (AgentTool, CapturedEnvironment) {
        let captured = Arc::new(std::sync::Mutex::new(None));
        let tool = create_bash_tool(
            ".",
            Some(BashToolOptions {
                operations: Some(Arc::new(CaptureEnv(Arc::clone(&captured)))),
                expose_session_environment: false,
                trace_context,
                ..Default::default()
            }),
        );
        (tool, captured)
    }

    /// 第 9 周 finding #13：`trace_context` provider 的值写入子进程的
    /// `TRACEPARENT`/`TRACESTATE`，且不会被后续 env 替换覆盖。
    #[tokio::test]
    async fn trace_context_is_injected_into_the_shell_environment() {
        let (tool, captured) = trace_tool(Some(Arc::new(|| {
            Some((
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(),
                Some("vendor=value".to_string()),
            ))
        })));
        tool.execute
            .execute(
                "call_1",
                serde_json::json!({ "command": "true" }),
                None,
                None,
            )
            .await
            .expect("execute");
        let env = captured.lock().unwrap().clone().expect("env captured");
        assert_eq!(
            env.get("TRACEPARENT").map(String::as_str),
            Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
        );
        assert_eq!(
            env.get("TRACESTATE").map(String::as_str),
            Some("vendor=value")
        );
    }

    /// 没有 trace provider 时不注入 `TRACEPARENT`（也不伪造 `TRACESTATE`）。
    #[tokio::test]
    async fn no_trace_context_leaves_the_environment_clean() {
        let (tool, captured) = trace_tool(None);
        tool.execute
            .execute(
                "call_1",
                serde_json::json!({ "command": "true" }),
                None,
                None,
            )
            .await
            .expect("execute");
        let env = captured.lock().unwrap().clone().expect("env captured");
        assert!(!env.contains_key("TRACEPARENT"), "{env:?}");
        assert!(!env.contains_key("TRACESTATE"), "{env:?}");
    }
}
