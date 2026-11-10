//! modes/rpc/rpc-client.ts：RPC 客户端（子进程生命周期 + 33 个命令的类型化 API）。
//!
//! TS: `packages/coding-agent/src/modes/rpc/rpc-client.ts`（617 行）。
//! Spec: `spec/ca-rpc-print.md` §2.7.1-§2.7.13、§2.4、§3.4、§4.4.1、§6.1/§6.2/§6.3、§7.1、
//! §7.4 第 7 条、§8.4 第 10 条、§8.5、附录 A.8 第 5/8/9 条。
//!
//! ## 唯一允许的行为差异（`ARCHITECTURE.md` §6 / §8 第 8 条）
//!
//! TS 默认 `spawn("node", [dist/cli.js, ...])`；Rust 侧默认用
//! [`std::env::current_exe`] 作为可执行文件并**直接** spawn（不经过 `node`）。
//! [`RpcClientOptions::cli_path`] 显式给出时用法与 TS 相同（仍是直接 spawn，Rust 无解释器）。
//! `current_exe()` 失败时返回 [`RpcClientError`]。
//!
//! ## 子进程语义（审计修复 10）
//!
//! 一个 supervisor 任务**独占**持有 `tokio::process::Child`：它同时负责 `wait()` 与
//! SIGTERM/SIGKILL 控制消息，避免 “wait 任务 + stop 各自 wait” 的双重 `wait`。stdout 由
//! `pump_jsonl`（[`crate::jsonl`]）读取，stderr 每块做 `String::from_utf8_lossy` 追加并镜像
//! 到父进程 stderr。退出后 supervisor 记录 `(code, signal)`、缓存退出错误、拒绝全部 pending
//! 请求并唤醒 `stop()`。
//!
//! ## 与 TS 的被迫差异
//!
//! 1. 监听器 panic：Rust 用 `catch_unwind(AssertUnwindSafe(..))` **逐个**兜住监听器 panic，
//!    使一个监听器的 panic 不影响其它监听器（Rust-only 安全性；TS 的 `try/catch` 会中断同一行
//!    的后续监听器，但 TS 监听器不会“panic”）。
//! 2. `options.env` 的**新键**顺序：TS 的对象展开按 `options.env` 插入顺序追加新键，Rust 的
//!    公开字段是 [`HashMap`]，新键相对顺序不固定（已存在键仍原地替换值）。子进程 environ 的
//!    顺序不可观察（所有查找按名字）。
//! 3. stdin 写失败的 errno 文本：常见 errno 映射为 Node 的 `write EPIPE` 形式；未知 errno 保留
//!    Rust/C 库文本（spec §8.5 同类可接受差异）。
//!
//! 另外 `start()` 的 100ms 检查按 **TS 源**实现为 `exitCode !== null`（即只检查
//! `code.is_some()`）：子进程在 100ms 内被信号杀死时 TS 的 `start()` 不会抛错（`exitCode` 为
//! `null`），随后的 `send()` 才用缓存的退出错误失败。
//!
//! ## 类型化返回的 Rust 适配
//!
//! TS 的 `getData` 之后直接读 `data.disposition` / `data.models` 等；字段缺失时得到
//! `undefined`（调用方可能在稍后抛 `TypeError`）。Rust 无法返回“undefined”，因此这些缺失/
//! 类型不符的响应会返回 [`RpcClientError`]，文本形如
//! `Invalid response for <command>: missing <field>`。正常服务端不会产生这些响应。
//!
//! ## 在系统中的位置与一次典型调用
//!
//! 本模块与同 crate 的 [`crate::rpc_mode`]（服务端）是对偶两端：服务端从 stdin 读命令、往
//! stdout 写响应与事件；本模块反过来，把命令写进子进程 stdin，从子进程 stdout 读回响应与
//! 事件。对外它是 SDK 公开面的一部分：`kiana-coding-agent` 的 `sdk_exports` 模块把
//! [`RpcClient`]、[`RpcClientOptions`]、[`ModelInfo`] 等再导出（对应 TS `modes/index.ts`
//! 的 `export { RpcClient, ... } from "./rpc/rpc-client.ts"`）。
//!
//! 一次典型调用：
//!
//! ```text
//! let client = RpcClient::new(options);                   // 仅构造，不启动进程
//! client.start().await?;                                  // spawn 子进程 + 100ms 初始化检查
//! let events = client.prompt_and_wait("hi", None).await?; // 发命令并收集事件直到 agent_settled
//! client.stop().await?;                                   // SIGTERM，1s 宽限后 SIGKILL
//! ```
//!
//! `start()` 成功后有三个后台任务在跑（stderr 收集、stdout 行泵、supervisor），都用
//! `tokio::task::spawn_local` 启动；因此 [`RpcClient`] 的异步方法必须在
//! `tokio::task::LocalSet`（或单线程 runtime）上下文里 await。阅读顺序建议：先读类型区
//! （§2.7.1），再读 [`RpcClient`] 的 `start` / `stop` / `send`，最后读 Helpers 区的
//! 响应解码函数。

use std::collections::HashMap;
use std::ffi::OsString;
use std::future::Future;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::Serialize;
use serde::de::{Deserialize, DeserializeOwned};
use serde_json::{Map, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::ChildStdin;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;

use kiana_agent_core::{AgentMessage, QueueMode, ThinkingLevel};
use kiana_ai::types::{ImageContent, Model};
use kiana_core::child_process::{SpawnOptions, StdioMode, spawn_process};
use kiana_core::event_bus::Unsubscribe;
use kiana_core::slash_commands::SlashCommandInfo;
use kiana_ext_api::events::StreamingBehavior;
use kiana_ext_api::tool_result::BashResult;
use kiana_json::js_parse;
use kiana_json::js_string;
use kiana_session::agent_session::{PromptDisposition, QueuedInputDisposition, SessionStats};
use kiana_session_core::session_manager::SessionEntry;

use crate::json_event::JsonAgentSessionEvent;
use crate::jsonl::{pump_jsonl, serialize_json_line};
use crate::rpc_types::{RpcResponse, RpcResponseType, RpcSessionState};

/// 请求响应的固定超时（TS: `send()` 里的 `setTimeout(..., 30000)`）。
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// `waitForIdle` / `collectEvents` 的默认超时（TS 参数默认值 `60000`）。
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// `start()` 里等待子进程初始化的固定 100ms（TS: `setTimeout(resolve, 100)`）。
const START_READY_DELAY: Duration = Duration::from_millis(100);

/// `stop()` 里 SIGTERM 之后等待退出的宽限（TS: 1000ms 后 SIGKILL）。
const STOP_KILL_TIMEOUT: Duration = Duration::from_millis(1000);

/// stderr 每次读取的块大小。
///
/// TS 的 stderr 是 Node 流：`data.toString()` 每次处理 libuv 交付的一块（管道为 64 KiB
/// 读缓冲）。用 8192 会在多字节 UTF-8 字符跨块时产生 Node 不会产生的 U+FFFD，永久污染
/// `get_stderr()` 与所有内嵌 stderr 的错误文本，因此与 libuv/`jsonl` 的 64 KiB 对齐。
const STDERR_READ_CHUNK: usize = 64 * 1024;

/// 取锁；锁中毒时取出内部值（本模块持锁期间不执行可能 panic 的代码）。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ============================================================================
// Types（§2.7.1）
// ============================================================================

/// [`RpcClient::new`] 的选项。
///
/// TS: `RpcClientOptions`。与 TS 的差异只有 [`cli_path`](Self::cli_path) 的缺省值
/// （`dist/cli.js` -> [`std::env::current_exe`]，见模块文档）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RpcClientOptions {
    /// CLI 可执行文件路径；`None` 时使用当前进程的可执行文件（TS 默认 `dist/cli.js`）。
    pub cli_path: Option<PathBuf>,
    /// 子进程工作目录；`None` 时继承当前目录。
    pub cwd: Option<PathBuf>,
    /// 环境变量覆盖。`Some` 覆盖同名变量，`None` **删除**该变量（Node `spawn` 的 env 语义）。
    pub env: HashMap<String, Option<String>>,
    /// 追加 `--provider <p>`。
    pub provider: Option<String>,
    /// 追加 `--model <m>`。
    pub model: Option<String>,
    /// 追加的其它 CLI 参数。
    pub args: Vec<String>,
}

/// `get_available_models` 返回的模型摘要。
///
/// TS: `ModelInfo`。只反序列化四个字段，服务端模型对象的其余字段被忽略。
/// `reasoning` 在 TS 运行时**不做类型校验**（客户端原样返回服务端对象；调用方通常按
/// 真值使用），这里按 JS 真值映射成 `bool`，非布尔值不会让整个列表反序列化失败。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    /// provider id。
    pub provider: String,
    /// 模型 id。
    pub id: String,
    /// 上下文窗口；缺省为 0（TS 类型上必填，这里容忍缺省）。
    #[serde(default)]
    pub context_window: u64,
    /// 是否支持 reasoning；缺省 `false`（TS 的 `undefined` 在真值判断下为假）。
    #[serde(default, deserialize_with = "deserialize_js_truthy")]
    pub reasoning: bool,
}

/// 任意 JSON -> JS 真值（TS 的 `if (value)`）。
fn deserialize_js_truthy<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(js_string::is_truthy(Some(&value)))
}

/// 事件监听器（TS: `RpcEventListener`）。
///
/// 用 [`Arc`] 持有，便于 [`RpcClient::on_event`] 在退订时按指针比较“第一个注册项”。
pub type RpcEventListener = Arc<dyn Fn(&JsonAgentSessionEvent) + Send + Sync>;

/// 客户端错误；[`std::fmt::Display`] 就是 TS 的 `Error.message`。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct RpcClientError {
    /// 逐字错误文本。
    pub message: String,
}

impl RpcClientError {
    /// 构造错误。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// `clear_queue` 的返回值。
///
/// TS: `{ steering: string[]; followUp: string[] }`（键序 `steering`、`followUp`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearQueueResult {
    /// 被清除的 steering 文本。
    pub steering: Vec<String>,
    /// 被清除的 follow-up 文本。
    pub follow_up: Vec<String>,
}

/// `new_session` / `switch_session` / `clone` 的返回值。
///
/// TS: `{ cancelled: boolean }`（扩展取消时为 `true`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct SessionChangeResult {
    /// 是否被扩展取消。
    pub cancelled: bool,
}

/// `fork` 的返回值。
///
/// TS: `{ text: string; cancelled: boolean }`；`selectedText` 为 `undefined` 时 `text` 键缺失。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkResult {
    /// 被 fork 消息的文本；缺失时为 `None`。
    #[serde(default)]
    pub text: Option<String>,
    /// 是否被扩展取消。
    pub cancelled: bool,
}

/// `cycle_model` 的返回值。
///
/// TS: `ModelCycleResult`（`kiana-session` 的同名类型没有 `Serialize`/`Deserialize`，
/// 因此这里定义客户端本地结构）。
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CycleModelResult {
    /// 新模型。
    pub model: Model,
    /// 新的 thinking level。
    pub thinking_level: ThinkingLevel,
    /// 是否在 scoped 模型之间循环。
    pub is_scoped: bool,
}

/// `cycle_thinking_level` 的返回值。
///
/// TS: `{ level: ThinkingLevel }`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct CycleThinkingLevelResult {
    /// 新的 thinking level。
    pub level: ThinkingLevel,
}

/// `export_html` 的返回值。
///
/// TS: `{ path: string }`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct ExportHtmlResult {
    /// 写出的 HTML 路径。
    pub path: String,
}

/// `get_fork_messages` 返回的一项。
///
/// TS: `{ entryId: string; text: string }`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkMessage {
    /// 条目 id。
    pub entry_id: String,
    /// 消息文本。
    pub text: String,
}

/// `get_entries` 的返回值。
///
/// TS: `{ entries: SessionEntry[]; leafId: string | null }`。
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntriesResult {
    /// 追加顺序的条目。
    pub entries: Vec<SessionEntry>,
    /// 当前叶子 id（无叶子时为 `null`）。
    #[serde(default)]
    pub leaf_id: Option<String>,
}

/// `get_tree` 的树节点。
///
/// TS: `SessionTreeNode`（`kiana-session-core` 的同名类型没有 `Serialize`/`Deserialize`，
/// 因此这里定义客户端本地结构；键序 `entry`、`children`、`label`、`labelTimestamp`）。
///
/// 注意：`children` 是递归结构，反序列化与 `Drop` 都按调用栈递归。正常会话树是单链且深度
/// 远小于栈限制；若需要与 `kiana-session-core::SessionTreeNode` 一样支持任意深度，需要另写
/// 显式栈的反序列化/析构（本单元按规划者的冻结形状实现）。
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSessionTreeNode {
    /// 条目。
    pub entry: SessionEntry,
    /// 子节点。
    pub children: Vec<RpcSessionTreeNode>,
    /// 解析后的标签（无标签时省略）。
    #[serde(default)]
    pub label: Option<String>,
    /// 最近一次标签变更的时间戳（无标签时省略）。
    #[serde(default)]
    pub label_timestamp: Option<String>,
}

/// `get_tree` 的返回值。
///
/// TS: `{ tree: SessionTreeNode[]; leafId: string | null }`。
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTreeResult {
    /// 根节点列表。
    pub tree: Vec<RpcSessionTreeNode>,
    /// 当前叶子 id（无叶子时为 `null`）。
    #[serde(default)]
    pub leaf_id: Option<String>,
}

// ============================================================================
// Internal state
// ============================================================================

/// 进程退出信息（Node `exit` 事件的 `(code, signal)`）。
#[derive(Debug, Clone, Copy)]
struct ExitInfo {
    code: Option<i32>,
    signal: Option<i32>,
}

/// supervisor 的控制消息。
enum SupervisorCommand {
    /// 向子进程发送指定信号（Unix: `kill(2)`）。
    Signal(i32),
    /// 强杀（TS: `kill("SIGKILL")`；非 Unix 为 `Child::kill`）。
    Kill,
}

/// 未完成请求的发送端。
///
/// 错误用 [`Arc`] 包裹：supervisor 退出时用**同一个**错误拒绝所有 pending（TS 的
/// `rejectPendingRequests(error)` 把同一个 `Error` 传给每个 reject），`Arc` 让多个
/// oneshot 共享一次分配。
type PendingSender = oneshot::Sender<Result<RpcResponse, Arc<RpcClientError>>>;

/// 未完成请求表（请求 id -> oneshot 发送端）。
type PendingMap = Mutex<HashMap<String, PendingSender>>;

/// 每个子进程实例独占的状态；`send()` 与 supervisor 共享。
struct ProcessState {
    /// 退出信息；`Some` 表示子进程已退出。
    exit_info: Mutex<Option<ExitInfo>>,
    /// 缓存的退出错误（TS 的 `exitError`）。
    exit_error: Mutex<Option<RpcClientError>>,
    /// 未完成的请求。
    pending: PendingMap,
    /// `stop()` 清空 pending 后仍存活的 sender。
    ///
    /// TS 的 `pendingRequests.clear()` 只丢弃 resolve/reject 引用，Promise 会一直挂到
    /// `send()` 的 30s 定时器再用超时文本 reject。Rust 若直接丢弃 oneshot sender，
    /// `receiver.await` 会立即返回（文本相同但时机不同），因此把 sender 移到这里保活，
    /// 直到 [`ProcessState`]（`send()` 的 future 持有）被释放。
    stopped_pending: Mutex<Vec<PendingSender>>,
    /// 子进程 stdin；`None` 表示不可写。
    stdin: AsyncMutex<Option<ChildStdin>>,
    /// 退出通知（`stop()` 用；`notify_one` 会保存一个 permit，避免丢失唤醒）。
    exit_notify: Notify,
    /// 请求超时（默认 30s；测试可用 [`RpcClient::with_request_timeout_for_test`] 覆盖）。
    request_timeout: Duration,
    /// 与客户端共享的 stderr 累积缓冲。
    stderr: Arc<Mutex<String>>,
}

/// 一个已启动子进程的句柄（TS 的 `ChildProcess` 字段 + `stopReadingStdout`）。
///
/// 存在即表示 [`RpcClient::start`] 已经跑过一次（含 spawn 失败的替身句柄，见
/// [`RpcClient::start`] 里的说明）；`Inner::process` 是 `Mutex<Option<Self>>`，
/// `stop()` 把它置回 `None`。
struct ProcessHandle {
    /// 与 supervisor 共享的每进程状态。
    state: Arc<ProcessState>,
    /// 向 supervisor 发送信号/强杀命令的通道。
    control_tx: mpsc::UnboundedSender<SupervisorCommand>,
    /// stdout 行泵任务；`stop()` 里 abort（对应 TS 的 `stopReadingStdout()`）。
    stdout_task: JoinHandle<()>,
}

/// `RpcClient` 的共享内部状态。
struct Inner {
    /// `new()` 时冻结的选项；`request_timeout` 独立存放（可被测试覆盖）。
    options: RpcClientOptions,
    /// 当前子进程句柄；`None` 表示未启动，或 `stop()` 已置空。
    process: Mutex<Option<Arc<ProcessHandle>>>,
    /// 事件监听器；`handle_line` 广播前取快照（克隆整个 Vec），与 TS 的
    /// `[...this.eventListeners]` 一致，监听器在广播中退订不影响本次遍历。
    listeners: Mutex<Vec<RpcEventListener>>,
    /// 请求 id 计数器（`req_1`、`req_2`…，进程内单调递增，不补零）。
    request_id: AtomicU64,
    /// 累积的子进程 stderr；与每个 `ProcessState::stderr` 指向同一份缓冲。
    stderr: Arc<Mutex<String>>,
    /// 请求超时；`new()` 后可被 `with_request_timeout_for_test` 覆盖，随后由每个
    /// 新启动的 `ProcessState` 拍快照。
    request_timeout: Mutex<Duration>,
}

// ============================================================================
// RPC Client（§2.7.2）
// ============================================================================

/// RPC 客户端：启动 `kiana --mode rpc` 子进程并提供 33 个命令的类型化 API。
///
/// TS: `rpc-client.ts#RpcClient`。
///
/// 与 TS 的唯一允许差异见模块文档：默认 `cliPath` 是 [`std::env::current_exe`]。所有任务
/// （stderr 收集、stdout JSONL 泵、supervisor）用 `tokio::task::spawn_local` 启动，因此
/// 客户端的方法必须在 `tokio::task::LocalSet`（或单线程 runtime）上下文里 await。
pub struct RpcClient {
    inner: Arc<Inner>,
}

impl RpcClient {
    /// 构造客户端（不启动子进程）。
    pub fn new(options: RpcClientOptions) -> Self {
        Self {
            inner: Arc::new(Inner {
                options,
                process: Mutex::new(None),
                listeners: Mutex::new(Vec::new()),
                request_id: AtomicU64::new(0),
                stderr: Arc::new(Mutex::new(String::new())),
                request_timeout: Mutex::new(DEFAULT_REQUEST_TIMEOUT),
            }),
        }
    }

    /// 覆盖请求超时（默认 30s）。
    ///
    /// 仅供测试：TS 的 30000ms 不可配置，这里是为了让
    /// `Timeout waiting for response to ...` 的错误文本能被快速断言。
    #[doc(hidden)]
    pub fn with_request_timeout_for_test(self, timeout: Duration) -> Self {
        *lock(&self.inner.request_timeout) = timeout;
        self
    }

    /// 累积的 stderr 文本（TS: `getStderr()`）。
    pub fn get_stderr(&self) -> String {
        lock(&self.inner.stderr).clone()
    }

    /// 订阅 agent 事件；返回退订闭包（Ts: `onEvent`）。
    ///
    /// 退订移除**第一个**指针相同的注册项；重复调用无副作用（与 TS 的
    /// `indexOf` + `splice` 一致）。
    pub fn on_event(&self, listener: RpcEventListener) -> Unsubscribe {
        lock(&self.inner.listeners).push(Arc::clone(&listener));
        let inner = Arc::clone(&self.inner);
        Box::new(move || {
            let mut listeners = lock(&inner.listeners);
            if let Some(index) = listeners
                .iter()
                .position(|existing| Arc::ptr_eq(existing, &listener))
            {
                listeners.remove(index);
            }
        })
    }

    /// 启动 RPC 子进程（TS: `start()`）。
    ///
    /// 参数：`--mode rpc`，按序追加 `--provider`、`--model` 与 [`RpcClientOptions::args`]。
    /// 启动后等待固定 100ms；若子进程在这段时间内以退出码退出，返回逐字的退出错误。
    pub async fn start(&self) -> Result<(), RpcClientError> {
        if lock(&self.inner.process).is_some() {
            return Err(RpcClientError::new("Client already started"));
        }

        let cli_path = match &self.inner.options.cli_path {
            Some(path) => path.clone(),
            None => std::env::current_exe().map_err(|error| {
                RpcClientError::new(format!("Failed to resolve the current executable: {error}"))
            })?,
        };

        let mut args: Vec<String> = vec!["--mode".to_string(), "rpc".to_string()];
        if let Some(provider) = &self.inner.options.provider {
            args.push("--provider".to_string());
            args.push(provider.clone());
        }
        if let Some(model) = &self.inner.options.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        args.extend(self.inner.options.args.iter().cloned());

        let spawn_options = SpawnOptions {
            cwd: self.inner.options.cwd.clone(),
            stdin: StdioMode::Pipe,
            stdout: StdioMode::Pipe,
            stderr: StdioMode::Pipe,
            detached: false,
            windows_hide: false,
            env: Some(merged_env(&self.inner.options.env)),
        };
        let command = cli_path.to_string_lossy().into_owned();
        let mut child = match spawn_process(&command, &args, &spawn_options) {
            Ok(child) => child,
            Err(error) => {
                // Node 的 `spawn` 不抛错：它返回 ChildProcess，并在随后触发 `error`
                // （`exitCode` 变为 -2）。这里用“已失败的进程句柄”复刻同样的可观察状态：
                // `start()` 抛 `Agent process error: ...`，之后的 `send()` 复用缓存错误，
                // 第二次 `start()` 仍报 `Client already started`。
                let error = RpcClientError::new(format!(
                    "Agent process error: {error}. Stderr: {}",
                    self.get_stderr()
                ));
                let state = Arc::new(ProcessState {
                    exit_info: Mutex::new(Some(ExitInfo {
                        code: Some(-2),
                        signal: None,
                    })),
                    exit_error: Mutex::new(Some(error.clone())),
                    pending: Mutex::new(HashMap::new()),
                    stopped_pending: Mutex::new(Vec::new()),
                    stdin: AsyncMutex::new(None),
                    exit_notify: Notify::new(),
                    request_timeout: *lock(&self.inner.request_timeout),
                    stderr: Arc::clone(&self.inner.stderr),
                });
                let (control_tx, control_rx) = mpsc::unbounded_channel();
                drop(control_rx);
                let stdout_task = tokio::task::spawn_local(async {});
                *lock(&self.inner.process) = Some(Arc::new(ProcessHandle {
                    state,
                    control_tx,
                    stdout_task,
                }));
                return Err(error);
            }
        };

        let pid = child.id();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        // stdio 句柄在构造 `ProcessState` 前全部 take 完：stdin 进状态锁、stdout/stderr
        // 交给下面的泵任务，`Child` 本体（含 wait 能力）整体移交给 supervisor。

        let state = Arc::new(ProcessState {
            exit_info: Mutex::new(None),
            exit_error: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            stopped_pending: Mutex::new(Vec::new()),
            stdin: AsyncMutex::new(stdin),
            exit_notify: Notify::new(),
            request_timeout: *lock(&self.inner.request_timeout),
            stderr: Arc::clone(&self.inner.stderr),
        });

        // stderr：每块 `String::from_utf8_lossy` 追加（TS 的 `data.toString()`），并把原始
        // 字节镜像到父进程 stderr。
        if let Some(mut stderr_pipe) = stderr_pipe {
            let stderr_buffer = Arc::clone(&self.inner.stderr);
            tokio::task::spawn_local(async move {
                let mut buffer = [0u8; STDERR_READ_CHUNK];
                loop {
                    match stderr_pipe.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => {
                            let chunk = &buffer[..read];
                            lock(&stderr_buffer).push_str(&String::from_utf8_lossy(chunk));
                            let mut target = std::io::stderr();
                            let _ = target.write_all(chunk);
                            let _ = target.flush();
                        }
                    }
                }
            });
        }

        // stdout：严格 JSONL 读取（仅在 LF 处切分、剥离一个 CR、EOF 冲刷尾行）。
        let stdout_task = match stdout {
            Some(mut stdout) => {
                let inner = Arc::clone(&self.inner);
                let line_state = Arc::clone(&state);
                tokio::task::spawn_local(async move {
                    let _ = pump_jsonl(&mut stdout, &mut |line| {
                        handle_line(&inner, &line_state, &line);
                    })
                    .await;
                })
            }
            None => tokio::task::spawn_local(async {}),
        };

        // supervisor：独占 Child，同时处理 wait 与控制消息。
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        {
            let supervisor_state = Arc::clone(&state);
            tokio::task::spawn_local(async move {
                supervise(child, pid, control_rx, supervisor_state).await;
            });
        }

        *lock(&self.inner.process) = Some(Arc::new(ProcessHandle {
            state: Arc::clone(&state),
            control_tx,
            stdout_task,
        }));

        tokio::time::sleep(START_READY_DELAY).await;

        // TS: `if (this.process.exitCode !== null)`。Node 的 `exitCode` 在“被信号杀死”时仍是
        // `null`（`signalCode` 才有值），因此这里只检查 `code`。
        let exited = *lock(&state.exit_info);
        if let Some(info) = exited.filter(|info| info.code.is_some()) {
            let error = lock(&state.exit_error).clone().unwrap_or_else(|| {
                RpcClientError::new(exit_error_message(&info, &self.get_stderr()))
            });
            *lock(&state.exit_error) = Some(error.clone());
            return Err(error);
        }

        Ok(())
    }

    /// 停止子进程（TS: `stop()`）。
    ///
    /// Unix 先发 SIGTERM，最多等 1s，然后发 SIGKILL 并**不**继续等待。清空 pending 请求但
    /// 不主动 reject（supervisor 已在退出时拒绝过；若进程从未退出，调用方会一直挂起，这是刻意行为）。
    pub async fn stop(&self) -> Result<(), RpcClientError> {
        let handle = lock(&self.inner.process).clone();
        let Some(handle) = handle else {
            return Ok(());
        };

        // TS 的 `stopReadingStdout()` 是 attachJsonlLineReader 返回的 detach 闭包（仅取消
        // 行监听）；Rust 侧的对应物就是 abort 这个泵任务。此后 `handle_line` 不再被调用，
        // 未读的 stdout 行留在管道里、已读未分发的行随任务丢弃。
        handle.stdout_task.abort();
        #[cfg(unix)]
        let _ = handle
            .control_tx
            .send(SupervisorCommand::Signal(libc::SIGTERM));
        #[cfg(not(unix))]
        let _ = handle.control_tx.send(SupervisorCommand::Kill);

        // TS 在 `kill("SIGTERM")` **之后**才 `process.on("exit", ...)`。若子进程在 stop()
        // 之前已经退出，`exit` 事件不会再次触发，定时器会等满 1000ms 再 SIGKILL 并 resolve。
        if lock(&handle.state.exit_info).is_some() {
            tokio::time::sleep(STOP_KILL_TIMEOUT).await;
            let _ = handle.control_tx.send(SupervisorCommand::Kill);
        } else {
            let waited = async {
                loop {
                    if lock(&handle.state.exit_info).is_some() {
                        return;
                    }
                    handle.state.exit_notify.notified().await;
                }
            };
            if tokio::time::timeout(STOP_KILL_TIMEOUT, waited)
                .await
                .is_err()
            {
                let _ = handle.control_tx.send(SupervisorCommand::Kill);
            }
        }

        // TS 的 `pendingRequests.clear()` 只是丢弃 map；Promise 会一直挂到 `send()` 的
        // 30s 定时器再用超时文本 reject。这里把 sender 移进 keepalive，避免 oneshot 在
        // 清空时立刻解析（否则调用方看到的文本相同、时机不同）。
        keep_pending_alive(&handle.state);
        *lock(&self.inner.process) = None;
        Ok(())
    }

    // =========================================================================
    // Command methods（§2.7.7）
    // =========================================================================

    /// 发送 prompt；返回接受后的 disposition。
    ///
    /// TS: `prompt(message, images?, streamingBehavior?)` -> `data.disposition`。
    pub async fn prompt(
        &self,
        message: impl Into<String>,
        images: Option<Vec<ImageContent>>,
        streaming_behavior: Option<StreamingBehavior>,
    ) -> Result<PromptDisposition, RpcClientError> {
        let mut body = command_body("prompt");
        body.insert("message".to_string(), Value::String(message.into()));
        insert_optional(&mut body, "images", images.as_ref(), "prompt")?;
        insert_optional(
            &mut body,
            "streamingBehavior",
            streaming_behavior.as_ref(),
            "prompt",
        )?;
        let response = self.send(body).await?;
        let data = data_object(&response, "prompt")?;
        prompt_disposition(data, "prompt")
    }

    /// 队列化一条 steering 消息；返回 disposition。
    ///
    /// TS: `steer(message, images?)`。
    pub async fn steer(
        &self,
        message: impl Into<String>,
        images: Option<Vec<ImageContent>>,
    ) -> Result<QueuedInputDisposition, RpcClientError> {
        let mut body = command_body("steer");
        body.insert("message".to_string(), Value::String(message.into()));
        insert_optional(&mut body, "images", images.as_ref(), "steer")?;
        let response = self.send(body).await?;
        let data = data_object(&response, "steer")?;
        queued_disposition(data, "steer")
    }

    /// 队列化一条 follow-up 消息；返回 disposition。
    ///
    /// TS: `followUp(message, images?)`。
    pub async fn follow_up(
        &self,
        message: impl Into<String>,
        images: Option<Vec<ImageContent>>,
    ) -> Result<QueuedInputDisposition, RpcClientError> {
        let mut body = command_body("follow_up");
        body.insert("message".to_string(), Value::String(message.into()));
        insert_optional(&mut body, "images", images.as_ref(), "follow_up")?;
        let response = self.send(body).await?;
        let data = data_object(&response, "follow_up")?;
        queued_disposition(data, "follow_up")
    }

    /// 中止当前操作（TS: `abort()`）。
    pub async fn abort(&self) -> Result<(), RpcClientError> {
        self.send(command_body("abort")).await.map(|_| ())
    }

    /// 清空 steering / follow-up 队列并返回其文本（TS: `clearQueue()`）。
    pub async fn clear_queue(&self) -> Result<ClearQueueResult, RpcClientError> {
        let response = self.send(command_body("clear_queue")).await?;
        deserialize_data(&response, "clear_queue")
    }

    /// 新建会话；`cancelled` 表示扩展取消了操作（TS: `newSession(parentSession?)`）。
    pub async fn new_session(
        &self,
        parent_session: Option<String>,
    ) -> Result<SessionChangeResult, RpcClientError> {
        let mut body = command_body("new_session");
        if let Some(parent_session) = parent_session {
            body.insert("parentSession".to_string(), Value::String(parent_session));
        }
        let response = self.send(body).await?;
        deserialize_data(&response, "new_session")
    }

    /// 当前会话状态（TS: `getState()`）。
    pub async fn get_state(&self) -> Result<RpcSessionState, RpcClientError> {
        let response = self.send(command_body("get_state")).await?;
        deserialize_data(&response, "get_state")
    }

    /// 按 provider / id 设置模型（TS: `setModel(provider, modelId)`）。
    pub async fn set_model(
        &self,
        provider: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Result<Model, RpcClientError> {
        let mut body = command_body("set_model");
        body.insert("provider".to_string(), Value::String(provider.into()));
        body.insert("modelId".to_string(), Value::String(model_id.into()));
        let response = self.send(body).await?;
        deserialize_data(&response, "set_model")
    }

    /// 循环到下一个模型；服务端返回 `data: null` 时为 `None`（TS: `cycleModel()`）。
    pub async fn cycle_model(&self) -> Result<Option<CycleModelResult>, RpcClientError> {
        let response = self.send(command_body("cycle_model")).await?;
        optional_data(&response, "cycle_model")
    }

    /// 可用模型列表（TS: `getAvailableModels()` -> `data.models`）。
    pub async fn get_available_models(&self) -> Result<Vec<ModelInfo>, RpcClientError> {
        let response = self.send(command_body("get_available_models")).await?;
        let data = data_object(&response, "get_available_models")?;
        field(data, "models", "get_available_models")
    }

    /// 设置 thinking level（TS: `setThinkingLevel(level)`）。
    pub async fn set_thinking_level(&self, level: ThinkingLevel) -> Result<(), RpcClientError> {
        let mut body = command_body("set_thinking_level");
        body.insert("level".to_string(), to_value(&level, "set_thinking_level")?);
        self.send(body).await.map(|_| ())
    }

    /// 循环 thinking level；`data: null` 时为 `None`（TS: `cycleThinkingLevel()`）。
    pub async fn cycle_thinking_level(
        &self,
    ) -> Result<Option<CycleThinkingLevelResult>, RpcClientError> {
        let response = self.send(command_body("cycle_thinking_level")).await?;
        optional_data(&response, "cycle_thinking_level")
    }

    /// 当前模型的可用 thinking level（TS: `getAvailableThinkingLevels()` -> `data.levels`）。
    pub async fn get_available_thinking_levels(
        &self,
    ) -> Result<Vec<ThinkingLevel>, RpcClientError> {
        let response = self
            .send(command_body("get_available_thinking_levels"))
            .await?;
        let data = data_object(&response, "get_available_thinking_levels")?;
        field(data, "levels", "get_available_thinking_levels")
    }

    /// 设置 steering 模式（TS: `setSteeringMode(mode)`）。
    pub async fn set_steering_mode(&self, mode: QueueMode) -> Result<(), RpcClientError> {
        let mut body = command_body("set_steering_mode");
        body.insert("mode".to_string(), to_value(&mode, "set_steering_mode")?);
        self.send(body).await.map(|_| ())
    }

    /// 设置 follow-up 模式（TS: `setFollowUpMode(mode)`）。
    pub async fn set_follow_up_mode(&self, mode: QueueMode) -> Result<(), RpcClientError> {
        let mut body = command_body("set_follow_up_mode");
        body.insert("mode".to_string(), to_value(&mode, "set_follow_up_mode")?);
        self.send(body).await.map(|_| ())
    }

    /// 压缩会话上下文（TS: `compact(customInstructions?)`）。
    ///
    /// `CompactionResult` 不是本 crate 的直接依赖，因此返回原始 [`Value`]；`data` 缺失时为
    /// [`Value::Null`]（TS 返回 `undefined`）。
    pub async fn compact(
        &self,
        custom_instructions: Option<String>,
    ) -> Result<Value, RpcClientError> {
        let mut body = command_body("compact");
        if let Some(instructions) = custom_instructions {
            body.insert(
                "customInstructions".to_string(),
                Value::String(instructions),
            );
        }
        let response = self.send(body).await?;
        Ok(get_data(&response)?.cloned().unwrap_or(Value::Null))
    }

    /// 开关自动压缩（TS: `setAutoCompaction(enabled)`）。
    pub async fn set_auto_compaction(&self, enabled: bool) -> Result<(), RpcClientError> {
        let mut body = command_body("set_auto_compaction");
        body.insert("enabled".to_string(), Value::Bool(enabled));
        self.send(body).await.map(|_| ())
    }

    /// 开关自动重试（TS: `setAutoRetry(enabled)`）。
    pub async fn set_auto_retry(&self, enabled: bool) -> Result<(), RpcClientError> {
        let mut body = command_body("set_auto_retry");
        body.insert("enabled".to_string(), Value::Bool(enabled));
        self.send(body).await.map(|_| ())
    }

    /// 中止进行中的重试（TS: `abortRetry()`）。
    pub async fn abort_retry(&self) -> Result<(), RpcClientError> {
        self.send(command_body("abort_retry")).await.map(|_| ())
    }

    /// 执行 bash 命令（TS: `bash(command)`；**不**带 `excludeFromContext`）。
    pub async fn bash(&self, command: impl Into<String>) -> Result<BashResult, RpcClientError> {
        let mut body = command_body("bash");
        body.insert("command".to_string(), Value::String(command.into()));
        let response = self.send(body).await?;
        deserialize_data(&response, "bash")
    }

    /// 中止运行中的 bash 命令（TS: `abortBash()`）。
    pub async fn abort_bash(&self) -> Result<(), RpcClientError> {
        self.send(command_body("abort_bash")).await.map(|_| ())
    }

    /// 会话统计（TS: `getSessionStats()`）。
    pub async fn get_session_stats(&self) -> Result<SessionStats, RpcClientError> {
        let response = self.send(command_body("get_session_stats")).await?;
        deserialize_data(&response, "get_session_stats")
    }

    /// 导出会话为 HTML（TS: `exportHtml(outputPath?)`）。
    pub async fn export_html(
        &self,
        output_path: Option<String>,
    ) -> Result<ExportHtmlResult, RpcClientError> {
        let mut body = command_body("export_html");
        if let Some(path) = output_path {
            body.insert("outputPath".to_string(), Value::String(path));
        }
        let response = self.send(body).await?;
        deserialize_data(&response, "export_html")
    }

    /// 切换到另一个会话文件（TS: `switchSession(sessionPath)`）。
    pub async fn switch_session(
        &self,
        session_path: impl Into<String>,
    ) -> Result<SessionChangeResult, RpcClientError> {
        let mut body = command_body("switch_session");
        body.insert(
            "sessionPath".to_string(),
            Value::String(session_path.into()),
        );
        let response = self.send(body).await?;
        deserialize_data(&response, "switch_session")
    }

    /// 从指定消息 fork（TS: `fork(entryId)`）。
    pub async fn fork(&self, entry_id: impl Into<String>) -> Result<ForkResult, RpcClientError> {
        let mut body = command_body("fork");
        body.insert("entryId".to_string(), Value::String(entry_id.into()));
        let response = self.send(body).await?;
        deserialize_data(&response, "fork")
    }

    /// 克隆当前活动分支到新会话（TS: `clone()`）。
    pub async fn clone(&self) -> Result<SessionChangeResult, RpcClientError> {
        let response = self.send(command_body("clone")).await?;
        deserialize_data(&response, "clone")
    }

    /// 可供 fork 的消息（TS: `getForkMessages()` -> `data.messages`）。
    pub async fn get_fork_messages(&self) -> Result<Vec<ForkMessage>, RpcClientError> {
        let response = self.send(command_body("get_fork_messages")).await?;
        let data = data_object(&response, "get_fork_messages")?;
        field(data, "messages", "get_fork_messages")
    }

    /// 追加顺序的会话条目（`since` 之后；TS: `getEntries(since?)`）。
    pub async fn get_entries(
        &self,
        since: Option<String>,
    ) -> Result<EntriesResult, RpcClientError> {
        let mut body = command_body("get_entries");
        if let Some(since) = since {
            body.insert("since".to_string(), Value::String(since));
        }
        let response = self.send(body).await?;
        deserialize_data(&response, "get_entries")
    }

    /// 会话条目树（TS: `getTree()`）。
    pub async fn get_tree(&self) -> Result<SessionTreeResult, RpcClientError> {
        let response = self.send(command_body("get_tree")).await?;
        deserialize_data(&response, "get_tree")
    }

    /// 最后一条 assistant 消息的文本（TS: `getLastAssistantText()` -> `data.text`）。
    pub async fn get_last_assistant_text(&self) -> Result<Option<String>, RpcClientError> {
        let response = self.send(command_body("get_last_assistant_text")).await?;
        let data = data_object(&response, "get_last_assistant_text")?;
        optional_field(data, "text", "get_last_assistant_text")
    }

    /// 设置会话显示名（TS: `setSessionName(name)`）。
    pub async fn set_session_name(&self, name: impl Into<String>) -> Result<(), RpcClientError> {
        let mut body = command_body("set_session_name");
        body.insert("name".to_string(), Value::String(name.into()));
        self.send(body).await.map(|_| ())
    }

    /// 会话中的全部消息（TS: `getMessages()` -> `data.messages`）。
    pub async fn get_messages(&self) -> Result<Vec<AgentMessage>, RpcClientError> {
        let response = self.send(command_body("get_messages")).await?;
        let data = data_object(&response, "get_messages")?;
        field(data, "messages", "get_messages")
    }

    /// 可用命令（扩展命令、prompt 模板、skills；TS: `getCommands()` -> `data.commands`）。
    pub async fn get_commands(&self) -> Result<Vec<SlashCommandInfo>, RpcClientError> {
        let response = self.send(command_body("get_commands")).await?;
        let data = data_object(&response, "get_commands")?;
        field(data, "commands", "get_commands")
    }

    // =========================================================================
    // Waits（§2.7.8-§2.7.10）
    // =========================================================================

    /// 等待 agent 空闲（收到 `agent_settled`；TS: `waitForIdle()`，默认 60s）。
    ///
    /// 返回的 future 在**调用时**同步注册监听器并启动计时（TS 的 `new Promise` 执行器同步
    /// 运行），因此“先创建等待、再发 prompt、最后 await”不会错过中间到达的事件。
    pub fn wait_for_idle(&self) -> impl Future<Output = Result<(), RpcClientError>> + '_ {
        self.wait_for_idle_with_timeout(DEFAULT_IDLE_TIMEOUT)
    }

    /// [`Self::wait_for_idle`] 的显式超时版本（计时同样从调用时开始）。
    pub fn wait_for_idle_with_timeout(
        &self,
        timeout: Duration,
    ) -> impl Future<Output = Result<(), RpcClientError>> + '_ {
        let (sender, receiver) = oneshot::channel::<()>();
        let sender = Arc::new(Mutex::new(Some(sender)));
        let listener_sender = Arc::clone(&sender);
        let unsubscribe = self.on_event(Arc::new(move |event: &JsonAgentSessionEvent| {
            if event.get("type").and_then(Value::as_str) == Some("agent_settled")
                && let Some(sender) = lock(&listener_sender).take()
            {
                let _ = sender.send(());
            }
        }));
        let unsubscribe = UnsubscribeOnDrop::new(unsubscribe);
        let deadline = tokio::time::Instant::now() + timeout;
        async move {
            let result = tokio::time::timeout_at(deadline, receiver).await;
            // TS 在 resolve/timeout 回调里立即 unsubscribe；守卫同时兜住 future 被提前丢弃。
            drop(unsubscribe);
            match result {
                Ok(Ok(())) => Ok(()),
                _ => Err(RpcClientError::new(format!(
                    "Timeout waiting for agent to become idle. Stderr: {}",
                    self.get_stderr()
                ))),
            }
        }
    }

    /// 收集事件直到 `agent_settled`（含该事件；TS: `collectEvents()`，默认 60s）。
    ///
    /// 监听器与计时器在**调用时**建立，超时从调用时刻起算。
    pub fn collect_events(
        &self,
    ) -> impl Future<Output = Result<Vec<JsonAgentSessionEvent>, RpcClientError>> + '_ {
        self.collect_events_with_timeout(DEFAULT_IDLE_TIMEOUT)
    }

    /// [`Self::collect_events`] 的显式超时版本（计时从调用时开始）。
    pub fn collect_events_with_timeout(
        &self,
        timeout: Duration,
    ) -> impl Future<Output = Result<Vec<JsonAgentSessionEvent>, RpcClientError>> + '_ {
        let waiter = self.begin_collect(timeout);
        self.await_collect(waiter)
    }

    /// 发送 prompt 并等待完成，返回全部事件（TS: `promptAndWait()`）。
    ///
    /// 收集器在**调用时**同步注册（TS 的 async 函数体在第一个 await 之前同步执行），因此
    /// 即使事件先于 prompt 响应到达也不会丢失。
    pub fn prompt_and_wait(
        &self,
        message: impl Into<String>,
        images: Option<Vec<ImageContent>>,
    ) -> impl Future<Output = Result<Vec<JsonAgentSessionEvent>, RpcClientError>> + '_ {
        self.prompt_and_wait_with_timeout(message, images, DEFAULT_IDLE_TIMEOUT)
    }

    /// [`Self::prompt_and_wait`] 的显式超时版本（计时从调用时开始，prompt 的耗时计入其中）。
    pub fn prompt_and_wait_with_timeout(
        &self,
        message: impl Into<String>,
        images: Option<Vec<ImageContent>>,
        timeout: Duration,
    ) -> impl Future<Output = Result<Vec<JsonAgentSessionEvent>, RpcClientError>> + '_ {
        let waiter = self.begin_collect(timeout);
        let message = message.into();
        async move {
            if let Err(error) = self.prompt(message, images, None).await {
                // TS 把 collectEvents 的 Promise 留在后台（最终产生 unhandled rejection）；
                // Rust 侧丢弃 waiter 即退订，避免泄漏监听器。
                drop(waiter);
                return Err(error);
            }
            self.await_collect(waiter).await
        }
    }

    /// 同步注册一个事件收集器（`prompt_and_wait` 的关键：TS 的 Promise 执行器同步运行）。
    ///
    /// `deadline` 在这里（调用时）确定，复刻 TS 在 `collectEvents()` 调用时启动的
    /// `setTimeout`。
    fn begin_collect(&self, timeout: Duration) -> CollectWaiter {
        let (sender, receiver) = oneshot::channel::<Vec<JsonAgentSessionEvent>>();
        let sender = Arc::new(Mutex::new(Some(sender)));
        let events: Arc<Mutex<Vec<JsonAgentSessionEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let listener_events = Arc::clone(&events);
        let listener_sender = Arc::clone(&sender);
        let unsubscribe = self.on_event(Arc::new(move |event: &JsonAgentSessionEvent| {
            let mut collected = lock(&listener_events);
            collected.push(event.clone());
            if event.get("type").and_then(Value::as_str) == Some("agent_settled")
                && let Some(sender) = lock(&listener_sender).take()
            {
                let _ = sender.send(collected.clone());
            }
        }));
        CollectWaiter {
            receiver,
            unsubscribe: UnsubscribeOnDrop::new(unsubscribe),
            deadline: tokio::time::Instant::now() + timeout,
        }
    }

    /// 等待收集器完成或超时。
    async fn await_collect(
        &self,
        waiter: CollectWaiter,
    ) -> Result<Vec<JsonAgentSessionEvent>, RpcClientError> {
        let CollectWaiter {
            receiver,
            unsubscribe,
            deadline,
        } = waiter;
        let result = tokio::time::timeout_at(deadline, receiver).await;
        drop(unsubscribe);
        match result {
            Ok(Ok(events)) => Ok(events),
            _ => Err(RpcClientError::new(format!(
                "Timeout collecting events. Stderr: {}",
                self.get_stderr()
            ))),
        }
    }

    // =========================================================================
    // Internal（§2.7.11-§2.7.13）
    // =========================================================================

    /// 发送一条命令并等待响应（TS: `send()`）。
    async fn send(&self, mut body: Map<String, Value>) -> Result<RpcResponse, RpcClientError> {
        let handle = lock(&self.inner.process).clone();
        let Some(handle) = handle else {
            return Err(RpcClientError::new("Client not started"));
        };
        let state = &handle.state;

        // 检查顺序固定：未启动 -> exitError -> exitCode -> stdin 可写（先返回已记录的退出错误，保持错误优先级稳定）。
        if let Some(error) = lock(&state.exit_error).clone() {
            return Err(error);
        }
        if let Some(info) = *lock(&state.exit_info) {
            let error = lock(&state.exit_error).clone().unwrap_or_else(|| {
                RpcClientError::new(exit_error_message(&info, &self.get_stderr()))
            });
            *lock(&state.exit_error) = Some(error.clone());
            return Err(error);
        }

        let command_type = body
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        let mut stdin_guard = state.stdin.lock().await;
        let Some(stdin) = stdin_guard.as_mut() else {
            let error = RpcClientError::new(format!(
                "Agent process stdin is not writable. Stderr: {}",
                self.get_stderr()
            ));
            drop(stdin_guard);
            *lock(&state.exit_error) = Some(error.clone());
            return Err(error);
        };

        let id = format!(
            "req_{}",
            self.inner.request_id.fetch_add(1, Ordering::SeqCst) + 1
        );
        // `{ ...command, id }`：id 必须是**最后**一个键。
        body.insert("id".to_string(), Value::String(id.clone()));
        let line = serialize_json_line(&Value::Object(body));

        let (sender, receiver) = oneshot::channel();
        lock(&state.pending).insert(id.clone(), sender);
        let deadline = tokio::time::Instant::now() + state.request_timeout;

        let write_result = tokio::time::timeout_at(deadline, stdin.write_all(line.as_bytes()))
            .await
            .map(|result| result.map_err(|error| node_stdin_error_message(&error)));
        drop(stdin_guard);

        match write_result {
            Ok(Ok(())) => {}
            Ok(Err(message)) => {
                // TS 的 stdin `error` 事件处理器：
                // `const stdinError = this.exitError ?? new Error("Agent process stdin error: ...")`
                // 然后 `rejectPendingRequests(stdinError)`（**所有** pending，包括本条）。
                // 子进程刚退出时复用已有的 `exitError`，不要用 stdin 文本覆盖它。
                // 先在独立语句里取快照：match 的 scrutinee 临时值会活到整个 match 结束，
                // 直接写 `match lock(..).clone()` 会在 None 分支重新加锁时自锁。
                let existing_exit_error = lock(&state.exit_error).clone();
                let error = match existing_exit_error {
                    Some(existing) => existing,
                    None => {
                        let error = RpcClientError::new(format!(
                            "Agent process stdin error: {message}. Stderr: {}",
                            self.get_stderr()
                        ));
                        *lock(&state.exit_error) = Some(error.clone());
                        *state.stdin.lock().await = None;
                        error
                    }
                };
                reject_pending_requests(state, &error);
                return Err(error);
            }
            Err(_) => {
                lock(&state.pending).remove(&id);
                return Err(timeout_error(&command_type, &self.get_stderr()));
            }
        }

        match tokio::time::timeout_at(deadline, receiver).await {
            Ok(Ok(Ok(response))) => Ok(response),
            Ok(Ok(Err(error))) => Err((*error).clone()),
            // sender 被丢弃（进程状态整体释放）：TS 会挂到 30s 定时器再用同一文本 reject；
            // 这里立即返回相同文本。`stop()` 的 clear 走 keepalive，不会触发这一分支。
            Ok(Err(_)) => Err(timeout_error(&command_type, &self.get_stderr())),
            Err(_) => {
                lock(&state.pending).remove(&id);
                Err(timeout_error(&command_type, &self.get_stderr()))
            }
        }
    }
}

/// 收集器的等待端。
///
/// `deadline` 在 [`RpcClient::begin_collect`] 调用时确定（不是第一次 poll 时），`unsubscribe`
/// 用守卫持有，future 被提前丢弃也会退订。
struct CollectWaiter {
    receiver: oneshot::Receiver<Vec<JsonAgentSessionEvent>>,
    unsubscribe: UnsubscribeOnDrop,
    /// TS `collectEvents()` 里 `setTimeout` 的到期时刻。
    deadline: tokio::time::Instant,
}

/// 退订守卫：持有 [`Unsubscribe`]，在 Drop 时（且仅一次）移除监听器。
///
/// TS 的 Promise 一定会挂到 resolve/timeout 才退订；Rust future 可能在 poll 之前被丢弃，
/// 此时守卫保证不泄漏监听器。
struct UnsubscribeOnDrop(Option<Unsubscribe>);

impl UnsubscribeOnDrop {
    fn new(unsubscribe: Unsubscribe) -> Self {
        Self(Some(unsubscribe))
    }
}

impl Drop for UnsubscribeOnDrop {
    fn drop(&mut self) {
        if let Some(unsubscribe) = self.0.take() {
            unsubscribe();
        }
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// 构造 `{ "type": <kind> }`，后续字段按 TS 对象字面量的顺序插入。
fn command_body(kind: &str) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("type".to_string(), Value::String(kind.to_string()));
    body
}

/// `serde_json::to_value`，错误转成 [`RpcClientError`]。
fn to_value<T: Serialize>(value: &T, command: &str) -> Result<Value, RpcClientError> {
    serde_json::to_value(value)
        .map_err(|error| RpcClientError::new(format!("Failed to encode {command}: {error}")))
}

/// 仅当 `Some` 时插入键（TS 的 `undefined` 在 `JSON.stringify` 时被省略）。
fn insert_optional<T: Serialize>(
    body: &mut Map<String, Value>,
    key: &str,
    value: Option<&T>,
    command: &str,
) -> Result<(), RpcClientError> {
    if let Some(value) = value {
        body.insert(key.to_string(), to_value(value, command)?);
    }
    Ok(())
}

/// 合并环境变量：`overrides` 的值覆盖 `std::env::vars_os()`，`None` 删除该键。
///
/// TS 是 `{ ...process.env, ...this.options.env }`：已存在的键**在原位置**替换值，新键追加在
/// 末尾（按 `options.env` 的插入顺序）。Rust 的 `options.env` 是 [`HashMap`]（冻结的公开
/// 类型），无法保留新键的插入顺序；已存在键的位置仍按 JS 语义原地替换。所有查找都按名字，
/// 子进程的 environ 顺序不可观察。
fn merged_env(overrides: &HashMap<String, Option<String>>) -> Vec<(OsString, OsString)> {
    let mut merged: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    for (key, value) in overrides {
        let key = OsString::from(key);
        match merged.iter().position(|(existing, _)| existing == &key) {
            Some(index) => match value {
                Some(value) => merged[index].1 = OsString::from(value),
                None => {
                    merged.remove(index);
                }
            },
            None => {
                if let Some(value) = value {
                    merged.push((key, OsString::from(value)));
                }
            }
        }
    }
    merged
}

/// `getData`：失败响应转成错误；成功响应返回 `data`（键缺失 -> `None`，显式 `null` -> `Some(Null)`）。
///
/// TS 是 `throw new Error(errorResponse.error)`；`error` 为 `undefined` 时
/// `new Error(undefined).message === ""`，因此缺失 `error` 的失败响应应映射为空消息，
/// 而不是字面量 `"undefined"`。
fn get_data(response: &RpcResponse) -> Result<Option<&Value>, RpcClientError> {
    if !response.success {
        return Err(RpcClientError::new(
            response.error.clone().unwrap_or_default(),
        ));
    }
    Ok(response.data.as_ref())
}

/// 成功响应的 `data` 必须是对象。
fn data_object<'a>(
    response: &'a RpcResponse,
    command: &str,
) -> Result<&'a Map<String, Value>, RpcClientError> {
    match get_data(response)? {
        Some(Value::Object(object)) => Ok(object),
        Some(_) => Err(RpcClientError::new(format!(
            "Invalid response for {command}: expected an object"
        ))),
        None => Err(RpcClientError::new(format!(
            "Invalid response for {command}: missing data"
        ))),
    }
}

/// 从 `data` 对象取字段并反序列化。
fn field<T: DeserializeOwned>(
    data: &Map<String, Value>,
    key: &str,
    command: &str,
) -> Result<T, RpcClientError> {
    let value = data.get(key).ok_or_else(|| {
        RpcClientError::new(format!("Invalid response for {command}: missing {key}"))
    })?;
    serde_json::from_value(value.clone())
        .map_err(|error| RpcClientError::new(format!("Invalid response for {command}: {error}")))
}

/// 从 `data` 对象取可选字段；键缺失或 `null` -> `None`。
fn optional_field<T: DeserializeOwned>(
    data: &Map<String, Value>,
    key: &str,
    command: &str,
) -> Result<Option<T>, RpcClientError> {
    match data.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|error| {
                RpcClientError::new(format!("Invalid response for {command}: {error}"))
            }),
    }
}

/// 把整个 `data` 反序列化为 `T`。
fn deserialize_data<T: DeserializeOwned>(
    response: &RpcResponse,
    command: &str,
) -> Result<T, RpcClientError> {
    let data = get_data(response)?.ok_or_else(|| {
        RpcClientError::new(format!("Invalid response for {command}: missing data"))
    })?;
    serde_json::from_value(data.clone())
        .map_err(|error| RpcClientError::new(format!("Invalid response for {command}: {error}")))
}

/// `data: null` -> `None`；否则反序列化。
fn optional_data<T: DeserializeOwned>(
    response: &RpcResponse,
    command: &str,
) -> Result<Option<T>, RpcClientError> {
    match get_data(response)? {
        None | Some(Value::Null) => Ok(None),
        Some(data) => serde_json::from_value(data.clone())
            .map(Some)
            .map_err(|error| {
                RpcClientError::new(format!("Invalid response for {command}: {error}"))
            }),
    }
}

/// `data.disposition` -> [`PromptDisposition`]。
fn prompt_disposition(
    data: &Map<String, Value>,
    command: &str,
) -> Result<PromptDisposition, RpcClientError> {
    match data.get("disposition").and_then(Value::as_str) {
        Some("handled") => Ok(PromptDisposition::Handled),
        Some("queued") => Ok(PromptDisposition::Queued),
        Some("started") => Ok(PromptDisposition::Started),
        other => Err(RpcClientError::new(format!(
            "Invalid response for {command}: unknown disposition {other:?}"
        ))),
    }
}

/// `data.disposition` -> [`QueuedInputDisposition`]。
fn queued_disposition(
    data: &Map<String, Value>,
    command: &str,
) -> Result<QueuedInputDisposition, RpcClientError> {
    match data.get("disposition").and_then(Value::as_str) {
        Some("handled") => Ok(QueuedInputDisposition::Handled),
        Some("queued") => Ok(QueuedInputDisposition::Queued),
        other => Err(RpcClientError::new(format!(
            "Invalid response for {command}: unknown disposition {other:?}"
        ))),
    }
}

/// stdin 写失败 -> Node 的 `error.message`。
///
/// Node 的 stdin `error` 事件文本形如 `write EPIPE`（libuv 的 `<syscall> <errno name>`）；
/// Rust 的 [`std::io::Error::to_string`] 是 C 库文本（`Broken pipe (os error 32)`）。这里把
/// 常见 errno 映射成 Node 的符号名，未知 errno 保留 Rust 文本（spec §8.5 允许的错误文本差异）。
fn node_stdin_error_message(error: &std::io::Error) -> String {
    #[cfg(unix)]
    if let Some(code) = error.raw_os_error()
        && let Some(name) = errno_name(code)
    {
        return format!("write {name}");
    }
    error.to_string()
}

/// `write(2)` 可能返回的 errno -> libuv/Node 的符号名。
#[cfg(unix)]
fn errno_name(code: i32) -> Option<&'static str> {
    Some(match code {
        libc::EACCES => "EACCES",
        libc::EBADF => "EBADF",
        libc::ECONNABORTED => "ECONNABORTED",
        libc::ECONNRESET => "ECONNRESET",
        libc::EDQUOT => "EDQUOT",
        libc::EFAULT => "EFAULT",
        libc::EFBIG => "EFBIG",
        libc::EINTR => "EINTR",
        libc::EINVAL => "EINVAL",
        libc::EIO => "EIO",
        libc::EISDIR => "EISDIR",
        libc::ENETDOWN => "ENETDOWN",
        libc::ENETUNREACH => "ENETUNREACH",
        libc::ENOENT => "ENOENT",
        libc::ENOMEM => "ENOMEM",
        libc::ENOSPC => "ENOSPC",
        libc::ENOTCONN => "ENOTCONN",
        libc::ENXIO => "ENXIO",
        libc::EPIPE => "EPIPE",
        libc::EPROTOTYPE => "EPROTOTYPE",
        libc::EROFS => "EROFS",
        libc::ESPIPE => "ESPIPE",
        libc::ETIMEDOUT => "ETIMEDOUT",
        _ => return None,
    })
}

/// 请求超时错误文本（TS 逐字）。
fn timeout_error(command_type: &str, stderr: &str) -> RpcClientError {
    RpcClientError::new(format!(
        "Timeout waiting for response to {command_type}. Stderr: {stderr}"
    ))
}

/// `Agent process exited (code=<code> signal=<signal>). Stderr: <stderr>`（TS 逐字）。
fn exit_error_message(info: &ExitInfo, stderr: &str) -> String {
    let code = info
        .code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "null".to_string());
    let signal = info
        .signal
        .map(signal_name)
        .unwrap_or_else(|| "null".to_string());
    format!("Agent process exited (code={code} signal={signal}). Stderr: {stderr}")
}

/// 信号编号 -> Node 的信号名（未知时 `SIG<n>`）。
#[cfg(unix)]
fn signal_name(signal: i32) -> String {
    let name = if signal == libc::SIGHUP {
        "SIGHUP"
    } else if signal == libc::SIGINT {
        "SIGINT"
    } else if signal == libc::SIGQUIT {
        "SIGQUIT"
    } else if signal == libc::SIGILL {
        "SIGILL"
    } else if signal == libc::SIGTRAP {
        "SIGTRAP"
    } else if signal == libc::SIGABRT {
        "SIGABRT"
    } else if signal == libc::SIGBUS {
        "SIGBUS"
    } else if signal == libc::SIGFPE {
        "SIGFPE"
    } else if signal == libc::SIGKILL {
        "SIGKILL"
    } else if signal == libc::SIGUSR1 {
        "SIGUSR1"
    } else if signal == libc::SIGSEGV {
        "SIGSEGV"
    } else if signal == libc::SIGUSR2 {
        "SIGUSR2"
    } else if signal == libc::SIGPIPE {
        "SIGPIPE"
    } else if signal == libc::SIGALRM {
        "SIGALRM"
    } else if signal == libc::SIGTERM {
        "SIGTERM"
    } else if signal == libc::SIGCHLD {
        "SIGCHLD"
    } else if signal == libc::SIGCONT {
        "SIGCONT"
    } else if signal == libc::SIGSTOP {
        "SIGSTOP"
    } else if signal == libc::SIGTSTP {
        "SIGTSTP"
    } else if signal == libc::SIGTTIN {
        "SIGTTIN"
    } else if signal == libc::SIGTTOU {
        "SIGTTOU"
    } else if signal == libc::SIGURG {
        "SIGURG"
    } else if signal == libc::SIGXCPU {
        "SIGXCPU"
    } else if signal == libc::SIGXFSZ {
        "SIGXFSZ"
    } else if signal == libc::SIGVTALRM {
        "SIGVTALRM"
    } else if signal == libc::SIGPROF {
        "SIGPROF"
    } else if signal == libc::SIGWINCH {
        "SIGWINCH"
    } else if signal == libc::SIGIO {
        "SIGIO"
    } else if signal == libc::SIGSYS {
        "SIGSYS"
    } else {
        return format!("SIG{signal}");
    };
    name.to_string()
}

/// 非 Unix 平台 `ExitStatus` 不携带信号名（Node 的 win32 也只有 code）。
#[cfg(not(unix))]
fn signal_name(signal: i32) -> String {
    format!("SIG{signal}")
}

/// `std::process::ExitStatus` -> [`ExitInfo`]。
fn exit_info_from_status(status: std::io::Result<std::process::ExitStatus>) -> ExitInfo {
    match status {
        Ok(status) => ExitInfo {
            code: status.code(),
            signal: exit_status_signal(&status),
        },
        Err(_) => ExitInfo {
            code: None,
            signal: None,
        },
    }
}

/// Unix 上取终止信号。
#[cfg(unix)]
fn exit_status_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

/// 非 Unix 平台恒为 `None`（Node 的 win32 `signal` 也为 `null`）。
#[cfg(not(unix))]
fn exit_status_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// `stop()` 的 `pendingRequests.clear()`：把 sender 移进 keepalive，让等待方挂到自己的
/// 30s 定时器（TS 的 Promise 语义）。
fn keep_pending_alive(state: &ProcessState) {
    let drained: Vec<_> = lock(&state.pending)
        .drain()
        .map(|(_, sender)| sender)
        .collect();
    lock(&state.stopped_pending).extend(drained);
}

/// TS 的 `rejectPendingRequests(error)`：拒绝**当前**全部未完成请求并清空 map。
///
/// `stop()` 已经挪进 keepalive 的 sender 不在其中（与 TS 的清空语义一致）。
fn reject_pending_requests(state: &ProcessState, error: &RpcClientError) {
    let pending: Vec<_> = lock(&state.pending)
        .drain()
        .map(|(_, sender)| sender)
        .collect();
    let error = Arc::new(error.clone());
    for sender in pending {
        let _ = sender.send(Err(Arc::clone(&error)));
    }
}

/// 记录退出：缓存退出错误、拒绝全部 pending、唤醒 `stop()`。
///
/// 这是 supervisor 的 `wait()` 分支收尾时唯一的落点，对应 TS `start()` 里注册的
/// `once("exit")` 处理器。所有 pending 请求拿到的是**同一个** [`Arc`] 错误（与 TS 把同一
/// 个 `Error` 传给每个 reject 一致）。
fn finish_process(state: &ProcessState, info: ExitInfo) {
    let stderr = lock(&state.stderr).clone();
    let error = Arc::new(RpcClientError::new(exit_error_message(&info, &stderr)));
    *lock(&state.exit_info) = Some(info);
    *lock(&state.exit_error) = Some((*error).clone());
    let pending: Vec<_> = lock(&state.pending)
        .drain()
        .map(|(_, sender)| sender)
        .collect();
    for sender in pending {
        let _ = sender.send(Err(Arc::clone(&error)));
    }
    state.exit_notify.notify_one();
}

/// supervisor：独占子进程，处理退出与 stop 的控制消息（审计修复 10）。
///
/// 循环体是 `tokio::select!` 的两个分支，语义如下：
///
/// - **`child.wait()` 完成**（唯一出口）：把 `(code, signal)` 记进
///   [`ProcessState::exit_info`]，缓存退出错误，用同一个 [`Arc`] 错误拒绝全部 pending
///   请求，然后 `notify_one` 唤醒可能正在 [`RpcClient::stop`] 里等待的调用方，循环结束。
/// - **控制消息**：[`SupervisorCommand::Signal`] 走 [`signal_child`]（Unix 用 `kill(2)`
///   发送 SIGTERM 等指定信号），[`SupervisorCommand::Kill`] 用 `Child::kill()` 强杀；
///   两个分支都不结束循环，只有子进程真正退出才收尾。
/// - **通道关闭**（`stop()` 之后句柄整体被丢弃）：`control_open` 置 `false`，select
///   不再监听该分支，supervisor 只等 `child.wait()`。
async fn supervise(
    mut child: tokio::process::Child,
    pid: Option<u32>,
    mut control_rx: mpsc::UnboundedReceiver<SupervisorCommand>,
    state: Arc<ProcessState>,
) {
    let mut control_open = true;
    loop {
        tokio::select! {
            status = child.wait() => {
                finish_process(&state, exit_info_from_status(status));
                break;
            }
            command = control_rx.recv(), if control_open => {
                match command {
                    Some(SupervisorCommand::Signal(signal)) => {
                        signal_child(&mut child, pid, signal).await;
                    }
                    Some(SupervisorCommand::Kill) => {
                        let _ = child.kill().await;
                    }
                    None => control_open = false,
                }
            }
        }
    }
}

/// 向子进程发送信号。
///
/// 不走 `Child::kill()`（只能 SIGKILL），而是对原始 pid 调 `kill(2)`，这样 `stop()` 的
/// 宽限阶段可以发 SIGTERM。`pid` 为 `None` 时不发送；`kill(2)` 的失败（如目标进程已死）
/// 同样被忽略——退出错误随后会由 supervisor 的 `wait()` 分支统一记录。
#[cfg(unix)]
async fn signal_child(child: &mut tokio::process::Child, pid: Option<u32>, signal: i32) {
    let _ = child;
    if let Some(pid) = pid {
        // SAFETY: `kill(2)` 只接收 pid 与信号编号，无内存安全前提。
        unsafe {
            libc::kill(pid as libc::pid_t, signal);
        }
    }
}

/// 非 Unix：没有可移植的“发送指定信号”，退化为强杀。
#[cfg(not(unix))]
async fn signal_child(child: &mut tokio::process::Child, _pid: Option<u32>, _signal: i32) {
    let _ = child.kill().await;
}

/// 处理 stdout 的一行（TS: `handleLine`）。
///
/// - 仅当 `type === "response"` 且 `id` 是非空字符串且 pending 中存在时才解析为响应；
/// - 否则（包括超时之后才到达的响应）作为事件广播给监听器快照；
/// - `JSON.parse` 失败与顶层 `null` 都被忽略（TS 里 `null.type` 抛 TypeError 后被 catch 吞掉）。
fn handle_line(inner: &Inner, state: &ProcessState, line: &str) {
    let Ok(data) = js_parse::parse(line) else {
        return;
    };
    if data.is_null() {
        return;
    }
    if let Value::Object(object) = &data
        && object.get("type").and_then(Value::as_str) == Some("response")
    {
        let id = match object.get("id") {
            Some(Value::String(id)) if !id.is_empty() => Some(id.clone()),
            _ => None,
        };
        if let Some(id) = id {
            let sender = lock(&state.pending).remove(&id);
            if let Some(sender) = sender {
                let _ = sender.send(Ok(response_from_value(&data)));
                return;
            }
        }
    }

    let listeners: Vec<RpcEventListener> = lock(&inner.listeners).clone();
    for listener in listeners {
        // Rust-only 安全性：监听器 panic 不会中断同一行的其它监听器（TS 的异常会）。
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(&data)));
        drop(result);
    }
}

/// 把 wire 上的响应对象转成 [`RpcResponse`]（字段缺失/类型异常都容忍）。
///
/// TS 的 `getData` 是 `if (!response.success) throw new Error(response.error)`：
/// `success` 按 **JS 真值**判断（`"false"`、`{}`、`1` 都算成功），`error` 经
/// `Error(...)` 的 `ToString` 转成消息（`null` -> `"null"`、数字 -> `"5"`、
/// 对象 -> `"[object Object]"`；缺失（`undefined`）-> 空串，由 [`get_data`] 处理）。
fn response_from_value(data: &Value) -> RpcResponse {
    let object = data.as_object();
    let get = |key: &str| object.and_then(|object| object.get(key)).cloned();
    let success = js_string::is_truthy(object.and_then(|object| object.get("success")));
    let error = object
        .and_then(|object| object.get("error"))
        .map(|value| js_string::js_string(Some(value)));
    RpcResponse {
        id: get("id"),
        kind: RpcResponseType::Response,
        command: get("command"),
        success,
        data: get("data"),
        error,
        raw_line: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_inner() -> Inner {
        Inner {
            options: RpcClientOptions::default(),
            process: Mutex::new(None),
            listeners: Mutex::new(Vec::new()),
            request_id: AtomicU64::new(0),
            stderr: Arc::new(Mutex::new(String::new())),
            request_timeout: Mutex::new(DEFAULT_REQUEST_TIMEOUT),
        }
    }

    fn test_state(inner: &Inner) -> Arc<ProcessState> {
        Arc::new(ProcessState {
            exit_info: Mutex::new(None),
            exit_error: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            stopped_pending: Mutex::new(Vec::new()),
            stdin: AsyncMutex::new(None),
            exit_notify: Notify::new(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            stderr: Arc::clone(&inner.stderr),
        })
    }

    #[test]
    fn exit_error_message_renders_null_and_signal_names() {
        let info = ExitInfo {
            code: Some(43),
            signal: None,
        };
        assert_eq!(
            exit_error_message(&info, ""),
            "Agent process exited (code=43 signal=null). Stderr: "
        );

        #[cfg(unix)]
        {
            let info = ExitInfo {
                code: None,
                signal: Some(libc::SIGTERM),
            };
            assert_eq!(
                exit_error_message(&info, "boom"),
                "Agent process exited (code=null signal=SIGTERM). Stderr: boom"
            );
            assert_eq!(signal_name(libc::SIGKILL), "SIGKILL");
            assert_eq!(signal_name(libc::SIGHUP), "SIGHUP");
            assert_eq!(signal_name(9999), "SIG9999");
        }
    }

    #[test]
    fn merged_env_applies_overrides_and_deletions() {
        let mut overrides = HashMap::new();
        overrides.insert(
            "KIANA_RPC_CLIENT_TEST_VAR".to_string(),
            Some("value".to_string()),
        );
        let merged = merged_env(&overrides);
        assert!(merged.iter().any(|(key, value)| {
            key == "KIANA_RPC_CLIENT_TEST_VAR" && value == std::ffi::OsStr::new("value")
        }));

        // `None` 删除同名环境变量（Node `spawn` 的 env 语义）。
        overrides.clear();
        overrides.insert("PATH".to_string(), None);
        let merged = merged_env(&overrides);
        assert!(!merged.iter().any(|(key, _)| key == "PATH"));
    }

    #[test]
    fn merged_env_replaces_existing_variables_in_place() {
        let (existing_key, _) = std::env::vars_os()
            .find(|(name, _)| name.to_str().is_some())
            .expect("at least one UTF-8 environment variable");
        let before_index = std::env::vars_os()
            .position(|(name, _)| name == existing_key)
            .expect("existing key position");
        let mut overrides = HashMap::new();
        overrides.insert(
            existing_key.to_string_lossy().into_owned(),
            Some("replacement".to_string()),
        );
        let merged = merged_env(&overrides);
        let after_index = merged
            .iter()
            .position(|(name, _)| name == &existing_key)
            .expect("still present");
        // JS 对象展开对已存在的键原地替换值，不改变位置。
        assert_eq!(before_index, after_index);
        assert_eq!(merged[after_index].1, OsString::from("replacement"));
    }

    #[cfg(unix)]
    #[test]
    fn node_stdin_error_message_matches_node_for_epipe() {
        let error = std::io::Error::from_raw_os_error(libc::EPIPE);
        assert_eq!(node_stdin_error_message(&error), "write EPIPE");

        let other = std::io::Error::other("boom");
        assert_eq!(node_stdin_error_message(&other), "boom");
    }

    #[test]
    fn get_data_maps_failures_to_the_error_message() {
        let failure = RpcResponse::error(Some(json!("req_1")), Some(json!("get_state")), "boom");
        assert_eq!(get_data(&failure).expect_err("failure").to_string(), "boom");

        let mut failure_without_error = failure.clone();
        failure_without_error.error = None;
        // TS: `new Error(undefined).message === ""`（不是字面量 "undefined"）。
        assert_eq!(
            get_data(&failure_without_error)
                .expect_err("failure")
                .to_string(),
            ""
        );

        let success = RpcResponse::success(None, Some(json!("abort")), None);
        assert!(get_data(&success).expect("success").is_none());
    }

    #[test]
    fn handle_line_resolves_pending_and_broadcasts_everything_else() {
        let inner = test_inner();
        let state = test_state(&inner);
        let events: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let listener_events = Arc::clone(&events);
        lock(&inner.listeners).push(Arc::new(move |event: &JsonAgentSessionEvent| {
            lock(&listener_events).push(event.clone());
        }));

        let (sender, mut receiver) = oneshot::channel();
        lock(&state.pending).insert("req_1".to_string(), sender);
        handle_line(
            &inner,
            &state,
            r#"{"id":"req_1","type":"response","command":"get_state","success":true,"data":{"x":1}}"#,
        );
        let response = receiver.try_recv().expect("resolved").expect("success");
        assert_eq!(response.data, Some(json!({"x": 1})));
        assert!(lock(&events).is_empty(), "responses are not broadcast");

        // 未知 id 的响应（例如超时之后才到达）广播。
        handle_line(
            &inner,
            &state,
            r#"{"id":"req_99","type":"response","success":false}"#,
        );
        assert_eq!(lock(&events).len(), 1);

        // 非 JSON 行与顶层 null 被忽略（TS 里 `null.type` 抛错后被 catch 吞掉）。
        handle_line(&inner, &state, "not json");
        handle_line(&inner, &state, "null");
        assert_eq!(lock(&events).len(), 1);

        // 非 response 对象广播。
        handle_line(
            &inner,
            &state,
            r#"{"type":"agent_settled","aborted":false}"#,
        );
        assert_eq!(lock(&events).len(), 2);
    }

    /// TS 的 `waitForIdle()` 在调用时（`new Promise` 执行器）同步注册监听器；不能等到
    /// future 第一次 poll 才注册，否则“先创建等待、再投递事件”会丢事件并白等 60s。
    #[tokio::test]
    async fn wait_for_idle_registers_listener_at_call_time() {
        let client = RpcClient::new(RpcClientOptions::default());
        let inner = Arc::clone(&client.inner);
        let state = test_state(&inner);

        let idle = client.wait_for_idle_with_timeout(Duration::from_secs(5));
        // 还没 poll 就投递事件。
        handle_line(
            &inner,
            &state,
            r#"{"type":"agent_settled","aborted":false}"#,
        );
        idle.await
            .expect("listener must be registered before first poll");
        assert!(lock(&inner.listeners).is_empty(), "unsubscribed");
    }

    /// `collectEvents()` 同样在调用时注册监听器并启动计时器。
    #[tokio::test]
    async fn collect_events_registers_listener_and_deadline_at_call_time() {
        let client = RpcClient::new(RpcClientOptions::default());
        let inner = Arc::clone(&client.inner);
        let state = test_state(&inner);

        let collect = client.collect_events_with_timeout(Duration::from_secs(5));
        handle_line(&inner, &state, r#"{"type":"agent_start"}"#);
        handle_line(&inner, &state, r#"{"type":"agent_settled"}"#);
        let events = collect.await.expect("collect");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["type"], "agent_start");
        assert_eq!(events[1]["type"], "agent_settled");

        // 计时从调用时开始：先耗掉超时再 poll，应立即得到逐字超时错误。
        let expired = client.collect_events_with_timeout(Duration::from_millis(10));
        tokio::time::sleep(Duration::from_millis(30)).await;
        let error = expired.await.expect_err("must time out");
        assert_eq!(error.to_string(), "Timeout collecting events. Stderr: ");

        // future 在 poll 之前被丢弃也应退订（不泄漏监听器）。
        let abandoned = client.collect_events_with_timeout(Duration::from_secs(5));
        assert_eq!(lock(&inner.listeners).len(), 1);
        drop(abandoned);
        assert!(lock(&inner.listeners).is_empty());
    }

    /// 评审发现 5：`stop()` 把 pending 移进 keepalive 而不是丢弃 sender —— TS 的
    /// `pendingRequests.clear()` 只丢 map，Promise 会一直挂到 `send()` 的 30s 定时器。
    #[test]
    fn stop_keepalive_leaves_requests_pending() {
        let inner = test_inner();
        let state = test_state(&inner);
        let (sender, mut receiver) = oneshot::channel::<Result<RpcResponse, Arc<RpcClientError>>>();
        lock(&state.pending).insert("req_1".to_string(), sender);
        keep_pending_alive(&state);
        assert!(lock(&state.pending).is_empty());
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        assert_eq!(lock(&state.stopped_pending).len(), 1);
    }

    /// 评审发现 8：stdin `error` 事件的等价物 `rejectPendingRequests` 用同一个错误拒绝
    /// **所有** pending 请求（不只是触发写入的那一条）。
    #[test]
    fn reject_pending_requests_rejects_every_pending_request() {
        let inner = test_inner();
        let state = test_state(&inner);
        let (first_tx, mut first_rx) =
            oneshot::channel::<Result<RpcResponse, Arc<RpcClientError>>>();
        let (second_tx, mut second_rx) =
            oneshot::channel::<Result<RpcResponse, Arc<RpcClientError>>>();
        lock(&state.pending).insert("req_1".to_string(), first_tx);
        lock(&state.pending).insert("req_2".to_string(), second_tx);

        let error = RpcClientError::new("Agent process stdin error: write EPIPE. Stderr: ");
        reject_pending_requests(&state, &error);

        assert!(lock(&state.pending).is_empty());
        for receiver in [&mut first_rx, &mut second_rx] {
            let received = receiver.try_recv().expect("rejected");
            assert_eq!(
                received.expect_err("error response").to_string(),
                error.to_string()
            );
        }
    }

    /// 评审发现 8：`if (!response.success)` 是 JS 真值判断，`new Error(response.error)`
    /// 对任意值做 `ToString`（`null` -> `"null"`、`5` -> `"5"`、对象 -> `"[object Object]"`）。
    #[test]
    fn response_success_uses_js_truthiness_and_error_is_stringified() {
        let truthy = response_from_value(&json!({
            "type": "response",
            "id": "req_1",
            "success": "false",
            "error": 5,
            "data": { "x": 1 },
        }));
        assert!(truthy.success, "\"false\" is truthy in JS");
        assert_eq!(truthy.error.as_deref(), Some("5"));

        let falsy = response_from_value(&json!({
            "type": "response",
            "success": 0,
            "error": Value::Null,
        }));
        assert!(!falsy.success);
        assert_eq!(falsy.error.as_deref(), Some("null"));

        let object = response_from_value(&json!({
            "type": "response",
            "success": {},
            "error": { "message": "boom" },
        }));
        assert!(object.success);
        assert_eq!(object.error.as_deref(), Some("[object Object]"));

        let missing = response_from_value(&json!({ "type": "response" }));
        assert!(!missing.success);
        assert_eq!(missing.error, None);
        // `new Error(undefined).message === ""`。
        assert_eq!(get_data(&missing).expect_err("failure").to_string(), "");
    }

    /// 评审发现 8：`ModelInfo.reasoning` 按 JS 真值映射（服务端非布尔值不会让列表失败）。
    #[test]
    fn model_info_reasoning_uses_js_truthiness() {
        let model: ModelInfo =
            serde_json::from_value(json!({"provider": "p", "id": "m", "reasoning": "yes"}))
                .expect("model");
        assert!(model.reasoning);
        let model: ModelInfo =
            serde_json::from_value(json!({"provider": "p", "id": "m", "reasoning": 0}))
                .expect("model");
        assert!(!model.reasoning);
        let model: ModelInfo =
            serde_json::from_value(json!({"provider": "p", "id": "m"})).expect("model");
        assert!(!model.reasoning);
    }

    /// `promptAndWait()` 是 TS 的 async 函数：函数体在第一个 `await` 之前同步执行，因此
    /// `collectEvents()` 的监听器在 `promptAndWait()` 返回 Promise 时就已经装好。
    #[tokio::test]
    async fn prompt_and_wait_registers_collector_before_first_poll() {
        let client = RpcClient::new(RpcClientOptions::default());
        let inner = Arc::clone(&client.inner);

        let wait = client.prompt_and_wait_with_timeout("hi", None, Duration::from_secs(5));
        assert_eq!(
            lock(&inner.listeners).len(),
            1,
            "collector registered at call time"
        );
        // 未启动的客户端：prompt 立即失败，waiter 被丢弃并退订。
        let error = wait.await.expect_err("client not started");
        assert_eq!(error.to_string(), "Client not started");
        assert!(lock(&inner.listeners).is_empty(), "waiter unsubscribed");
    }
}
