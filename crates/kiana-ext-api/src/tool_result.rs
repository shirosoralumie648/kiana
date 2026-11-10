//! `BashResult` / `BashOperations` / `ExecOptions` / `ExecResult` 的跨 crate 契约。
//!
//! 另有 [`crate::ui::ReadonlyFooterDataProvider`]：TS 的 `types.ts` 从 `footer-data-provider.ts`
//! 导入该接口，而 `footer-data-provider.ts` 属于 `kiana-resources`（L10），`kiana-ext-api` 是 L7。
//! 因此把**只读接口**上移为本 crate 的 trait，`kiana-resources::FooterDataProvider` 实现它
//! （`ARCHITECTURE.md` §3.5 的“契约类型上移”）。
//!
//! # ExecOptions / ExecResult 的归属
//!
//! `ARCHITECTURE.md` §3.5 与 §5 把 `ExecOptions`/`ExecResult` 指派给本模块，它们是扩展
//! `kiana.exec()` 面向 JS 的契约；这里给出**唯一权威定义**（`kiana-tools`/`kiana-session` 只再导出）。
//!
//! `kiana-core`（L3）不能依赖 `kiana-ext-api`（L7），因此 `kiana-core::exec` 另有一套**低层**同名
//! 类型供 `exec_command` 的实现使用（`timeout: Option<u64>`、`cwd: Option<PathBuf>`）。
//! 两者不是同一个类型：跨边界时由调用方显式转换，本 crate 不实现行为。这是对
//! “`kiana-core` 应暴露不同名字的低层 API”这一 planner 说明的偏离（`kiana-core` 已在 M1 合并、
//! 不在本单元所有权内），已记录在交付报告里。
//!
//! `ExecOptions.timeout` 是**毫秒**（`core/exec.ts`）；[`BashExecOptions::timeout`] 是**秒**
//! （`core/tools/bash.ts` 的 `resolveTimeoutMs`）。两者**不得**统一。
//!
//! # 谁在用
//!
//! - `kiana-tools::bash`/`bash_executor`/`powershell` 再导出这些类型，并实现本地
//!   [`BashOperations`]；
//! - `kiana-session` 与 `kiana-tools` 的 bash 选项持有 `Option<Arc<dyn BashOperations>>`，
//!   允许把命令执行委托给扩展或远端；
//! - `kiana-extensions` 实现扩展的 `kiana.exec()`（把 [`ExecOptions`] 转成
//!   `kiana-core::exec` 的低层同名类型），JS 扩展注册的 bash 操作则实现
//!   [`BashOperations`]（`user_bash` 结果里的 `Operations` 分支）。
//!
//! # 三个同族结果类型的区别
//!
//! - [`BashExecResult`]：`exitCode` 加可选的 [`BashExecutionStatus`]，是
//!   [`BashOperations::exec`] 的原始返回值；
//! - [`BashResult`]：`execute_bash_with_operations` 汇总后的结果（合并输出、是否截断、
//!   临时文件路径、取消标志、可选的结构化执行状态）；
//! - [`ExecResult`]：`kiana.exec()`（`execCommand`）的结果，stdout/stderr 分开、带 `killed`。

use std::sync::Arc;

use indexmap::IndexMap;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::BoxFuture;

/// 执行 shell 命令的选项。TS: `core/exec.ts#ExecOptions`。
///
/// 字段顺序与 TS 声明一致：`signal`、`timeout`、`cwd`。
///
/// **`timeout` 是 `number`（毫秒）**，这里用 [`f64`] 保留完整的 JS 数值域：
/// `execCommand` 只在 `options.timeout && options.timeout > 0` 时才启动计时器，
/// 因此负数、`0`、`NaN` 都表示“不设超时”，而 `1.5` 是合法的亚毫秒超时。
/// 用整数类型（例如 `u64`）会拒绝这些 TS 可接受的输入，故不使用。
/// Rust 的扩展桥把 `> 0` 且有限的值向上取整为整毫秒（至少 1ms）再交给
/// `kiana_core::exec_command`，所以 `Infinity` 在这里同样等于“不设超时”。
///
/// `cwd` 是**字符串**；Rust 的调用链会真正读取它：`kiana-extensions` 里
/// `ExtensionApi::exec` 的 JS 桥会把本字段作为位置参数 cwd（缺省回落到扩展自身的 cwd），
/// 因此从 JS 传入 `cwd` 会改变子进程目录。这是当前的实现行为，不是纯签名兼容字段。
///
/// `signal` 是 TS 的 `AbortSignal` → [`CancellationToken`]（`ARCHITECTURE.md` §4.2）。
#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    /// 取消信号（TS 的 `AbortSignal`）。
    pub signal: Option<CancellationToken>,
    /// 超时，**毫秒**（TS `number`）；负数/`0`/`NaN` 表示不设超时。
    pub timeout: Option<f64>,
    /// TS 保留该字段而 `execCommand` 不读取；Rust 扩展桥会把它当作子进程 cwd（见类型说明）。
    pub cwd: Option<String>,
}

/// 执行 shell 命令的结果。TS: `core/exec.ts#ExecResult`。
///
/// 字段顺序即 JSON 键顺序：`stdout`、`stderr`、`code`、`killed`。
/// TS 的 `code` 是 `number`；Node 的退出码总是整数，这里用 [`i32`] 并作为 JSON 数字
/// 序列化（`kiana.exec()` 的返回值会原样交给 JS）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecResult {
    /// 标准输出（已按 UTF-8 增量解码）。
    pub stdout: String,
    /// 标准错误（已按 UTF-8 增量解码）。
    pub stderr: String,
    /// 退出码；spawn 失败时为 `1`（与 TS 的 catch 分支一致）。
    pub code: i32,
    /// 是否由 signal/超时杀死。
    pub killed: bool,
}

/// bash 执行结果的**中立**结构化状态。
///
/// 不依赖 `kiana-sandbox`：沙箱执行器与直接 spawn 各自把自己的权威结论映射到本枚举，
/// 下游（会话、RPC、扩展）只消费这里的中立形状。
///
/// JSON 使用外部标签 + camelCase 变体名，例如 `{"confirmedExit":{"code":0}}`。
/// `BashExecResult` / `BashResult` 通过可选的 `executionStatus` 字段携带它；
/// 旧 JSON 缺少该键时按 `None` 处理（向后兼容）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BashExecutionStatus {
    /// 拿到确定的退出码（沙箱 relay 的 `ConfirmedExit`）。
    ConfirmedExit {
        /// 退出码。
        code: i32,
    },
    /// 未经沙箱直接执行；进程可能被信号终止。
    Unsandboxed {
        /// 进程退出码；被信号终止时为 `None`。
        code: Option<i32>,
        /// Unix 信号编号（Node 的 `signalCode`）。
        signal: Option<i32>,
    },
    /// 命令已经启动，但最终状态不可知（relay 被外部杀死）。
    Unconfirmed,
    /// 沙箱后端不可用或启动失败；命令**没有**执行。
    BackendUnavailable {
        /// 具体原因。
        reason: String,
    },
    /// 在命令确认启动之前被中断；命令**没有**执行。
    Interrupted {
        /// 中断原因。
        reason: String,
    },
}

/// bash 命令执行结果。
///
/// TS: `core/bash-executor.ts#BashResult`。字段顺序即 JSON 键顺序：
/// `output`、`exitCode`、`cancelled`、`truncated`、`fullOutputPath`、`executionStatus`。
///
/// `exitCode` 是 `number | undefined`：缺省时省略键（`serde(default)` +
/// `skip_serializing_if`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashResult {
    /// 合并后的 stdout + stderr（已清理、可能已截断）。
    pub output: String,
    /// 进程退出码；被取消时为 `None`，`BashOperations::exec` 返回 `None`（失败）时也是
    /// `None`（TS 的 `undefined`，省略键）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// 是否由 signal 取消。
    pub cancelled: bool,
    /// 输出是否被截断。
    pub truncated: bool,
    /// 完整输出所在的临时文件（输出超过截断阈值时才有）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
    /// 结构化执行状态；旧 JSON 缺少该键时为 `None`（省略键，旧输出不变）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_status: Option<BashExecutionStatus>,
}

/// [`BashOperations::exec`] 的执行选项（Rust-only helper）。
///
/// TS: `core/tools/bash.ts#BashOperations.exec` 第 3 个参数的内联对象
/// `{ onData: (data: Buffer) => void; signal?: AbortSignal; timeout?: number; env?: NodeJS.ProcessEnv }`。
///
/// 该对象在 TS 里是匿名类型，因此这里起名 [`BashExecOptions`]（`ExecOptions` 已被
/// `kiana.exec()` 的选项占用）。`timeout` 的单位是**秒**（`resolveTimeoutMs(timeout)` 再乘 1000）。
/// `on_data` 用 `&dyn Fn(&[u8])` 而不是回调 `Arc`：调用期间一定有效，避免每次执行额外分配。
/// 签名把 `self`、`command`、`cwd` 与整个 `BashExecOptions` 绑在同一个 `'a` 上，因此
/// 回调（以及 `options` 里的引用）必须在 `exec` 返回的 future 完成前保持存活；
/// 实现不能把 `on_data` 的引用存到 future 结束之后。
pub struct BashExecOptions<'a> {
    /// 流式输出回调（TS `onData(data: Buffer)`）。已经过 `BashOperations` 的实现原样透传。
    ///
    /// 回调类型要求 `Send + Sync`（实现可以把调用放到其它线程），因此回调内部共享的状态
    /// 必须自行同步。
    pub on_data: &'a (dyn Fn(&[u8]) + Send + Sync),
    /// 取消信号（TS 的 `AbortSignal`）。实现应在启动前检查 `is_cancelled()`，
    /// 并在等待期间响应取消；本契约不规定取消后返回什么错误。
    pub signal: Option<CancellationToken>,
    /// 超时，**秒**（`resolveTimeoutMs` 的输入）。`None` 表示不设超时。
    pub timeout: Option<f64>,
    /// 覆盖进程环境变量（TS `NodeJS.ProcessEnv`）；`None` 表示沿用宿主环境。
    pub env: Option<IndexMap<String, String>>,
}

// 手写 Debug：`on_data` 是 `&dyn Fn`，无法 Debug；其余字段照常打印，
// `finish_non_exhaustive` 保留“还有未打印字段”的信号。
impl std::fmt::Debug for BashExecOptions<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BashExecOptions")
            .field("signal", &self.signal)
            .field("timeout", &self.timeout)
            .field("env", &self.env)
            .finish_non_exhaustive()
    }
}

/// [`BashOperations::exec`] 的返回值。
///
/// TS: `Promise<{ exitCode: number | null }>`。`exit_code` 是**必填键**，`null` 表示失败；
/// 序列化时永远写出（`Option` 但**不加** `skip_serializing_if`）。
///
/// `execution_status` 是 Rust 侧新增的结构化状态：缺省时省略键，因此旧输出逐字不变；
/// 旧 JSON 缺少该键时反序列化为 `None`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashExecResult {
    /// 退出码；`None` 对应 TS 的 `null`（按键写出 `null`）。
    pub exit_code: Option<i32>,
    /// 结构化执行状态（可选，向后兼容）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_status: Option<BashExecutionStatus>,
}

/// 可插拔的 bash 执行操作。
///
/// TS: `core/tools/bash.ts#BashOperations`。覆盖它可把命令执行委托给远端系统（例如 SSH）。
///
/// 实现者只负责启动/等待进程、调用 `BashExecOptions::on_data` 流式输出并返回退出码；
/// 合并输出、截断、临时文件等由调用方（`kiana-tools::bash_executor`）完成。
///
/// 错误传播：`Err` 会由 `execute_bash_with_operations` 在**未取消**时原样上抛；如果 signal
/// 已取消，则改为返回已产生的输出并把 `exit_code` 置为 `None`。
///
/// 对象安全：`Box<dyn BashOperations>`/`Arc<dyn BashOperations>` 可以使用
/// （`ARCHITECTURE.md` §5.2 要求 `Box<dyn BashOperations>`）。
pub trait BashOperations: Send + Sync {
    /// 执行命令并流式输出。
    ///
    /// `cwd` 是调用方解析好的工作目录（本地实现会先检查其是否存在，不存在则返回错误）；
    /// 实现负责在进程运行期间把输出块交给 `options.on_data`。
    ///
    /// TS 的 `exec` 返回 `Promise<{ exitCode: number | null }>`；信号终止按 `128 + signal`
    /// 报告，`null` 退出码被视为失败命令。实现抛出的任意值以
    /// `Box<dyn Error + Send + Sync>` 透传（消息逐字保留，避免丢失实现抛出的原始错误上下文；
    /// `ARCHITECTURE.md` §4.1）。
    ///
    /// 返回 [`BoxFuture`]（`Send`）：工具执行会跨进 `kiana-tools`/`kiana-session` 的异步管线。
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a str,
        options: BashExecOptions<'a>,
    ) -> BoxFuture<'a, Result<BashExecResult, Box<dyn std::error::Error + Send + Sync>>>;
}

/// 让 `Arc<dyn BashOperations>` 也满足 [`BashOperations`]，便于下游拷贝共享。
///
/// 转发只解引用内层实现，不复制任何状态；因此 `Arc<dyn BashOperations>` 可以直接当作
/// `Box<dyn BashOperations>` 或 `&dyn BashOperations` 使用（`tests/tool_types.rs` 覆盖）。
impl<T: BashOperations + ?Sized> BashOperations for Arc<T> {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a str,
        options: BashExecOptions<'a>,
    ) -> BoxFuture<'a, Result<BashExecResult, Box<dyn std::error::Error + Send + Sync>>> {
        (**self).exec(command, cwd, options)
    }
}
