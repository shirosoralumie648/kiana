//! `transports/stdio.ts`（`StdioTransport`）：按行分帧的 stdio 传输。
//!
//! TS 源：`packages/mcp/src/transports/stdio.ts`；行为清单见 `spec/telemetry-mcp.md` §15、§20.1、
//! §27.3、§30.6、§30.22。
//!
//! # 线格式（§20.1 / §15.4）
//!
//! 每条消息是 `JSON.stringify(message) + "\n"`（紧凑、无 CR）。读取侧按 `0x0A` 切行：
//! 剥**一个**尾随 `\r`、`trim()` 后为空的行走跳过、超过 `maxMessageBytes` 的行发
//! `` `MCP stdio message exceeds ${max} bytes` `` 并继续（未换行的缓冲超限则清空缓冲并报错）。
//! 子进程 `close` 时若缓冲里还有非空白内容，发
//! `MCP stdio server closed with an incomplete JSON-RPC message` 后再 `emitClose()`。
//! `JSON.parse` 或 `parseJsonRpcMessage` 失败只发 error，传输保持打开（§15.4）。
//!
//! # stderr（§15.5）
//!
//! 尾部缓冲保留**最后** `maxStderrBytes`（默认 64 KiB）字节，`stderr()` 用 UTF-8 lossy 解码
//! （截断处可能产生 U+FFFD，与 Node 的 `Buffer.toString("utf8")` 等价）；`on_stderr` 收到的是
//! **每个原始 chunk** 的解码，不是缓冲。
//!
//! # 关闭阶梯（§15.3 / §20.1）
//!
//! `stdin.end()` → `min(500, close_timeout_ms)` 后向**进程组**发 SIGTERM →
//! `close_timeout_ms` 后再发 SIGKILL → 子进程 `close` 后再次向进程组发 SIGTERM
//! （回收忽略 stdin EOF 的孙进程）。`close()` 时子进程**已退出**（`wait()` 已返回、`close`
//! 事件尚未派发）只 `end()` stdin 且**不**发 close；子进程已关闭或从未启动时才 `emitClose()`。
//!
//! # spawn 的失败分类（逐字复刻 Node）
//!
//! `crossSpawn` 的失败分三层，各有不同的可观察行为：
//!
//! 1. **JS 层的参数校验**（`normalizeSpawnArguments`）：空 `command`、`args`/`command`/`cwd`/
//!    `env` 里的 NUL → `TypeError`（`code = ERR_INVALID_ARG_VALUE`，消息里带 `util.inspect`
//!    渲染的值）。没有 child、**没有** error/close 事件；`started` 仍是 true。
//! 2. **异步 spawn 错误**（`ENOENT`/`EACCES`/`EAGAIN`/`EMFILE`/`ENFILE`）：`` `spawn <file> <CODE>` ``
//!    （带文件名），随后照常触发 close（因此会 emitClose 一次）。
//! 3. **同步抛出的 errno**（`E2BIG`、`ENAMETOOLONG`、`ENOTDIR`、`ELOOP` …）：`` `spawn <CODE>` ``
//!    （**不带**文件名），没有 child、没有 close 事件。
//!
//! 错误 `name` 都是 `Error`，`code` 是 libuv 的 `Exxx` 名。`ENOEXEC` 由 libuv 换成
//! `/bin/sh <file> <args...>` 重试，Rust 侧显式补上这个回退。
//!
//! # 与 TS 的实现差异（全部是被迫或已文档化的）
//!
//! 1. **进程组**：Unix 用 `pre_exec(setsid)` 建独立会话/进程组（与 Node `detached: true` 相同），
//!    用 `libc::killpg` 杀组；失败回退 `libc::kill(pid, signal)`（TS 回退 `child.kill(signal)`）。
//! 2. **退出钩子**：TS 用 `process.once("exit")`；Rust 用 `Once` + `libc::atexit` 注册的 C 函数，
//!    对注册表里每个存活进程组尝试 SIGTERM。宿主被 SIGKILL 时同样没有清理（Node 也一样）。
//!    `atexit` 在进程正常退出时运行；注册表锁被 poison（持锁线程 panic）时钩子会跳过清理；
//!    锁只是被其他线程正常持有则钩子会阻塞等待，而不是跳过。
//! 3. **env**：TS 的 `{...process.env}` 只能枚举能表示成 JS 字符串的条目——名字不是合法 UTF-8
//!    的环境变量在 Node 里不可枚举（`Object.getOwnPropertyNames` 里 descriptor 是 `undefined`，
//!    `Object.keys`/spread 都看不到），因此会被丢掉；值则由 `process.env` 以 lossy UTF-8 解码成
//!    U+FFFD（两者均已实测）。Rust 侧同样**跳过非 UTF-8 的名字**、对值用 `to_string_lossy()`
//!    （见 `computed_env`）。`inherit_env == false` 时只传 `options.env`（可能是空环境）。
//!    TS 的 `Object.freeze` 是**浅**冻结，调用方在构造之后修改 `options.env` 对象仍会影响
//!    子进程；Rust 的 `StdioTransportOptions` 按值持有 `IndexMap`，[`StdioTransport::new`] 会
//!    克隆它（与 `args` 一样），因此构造后的修改不会生效。这是所有权模型的必然差异
//!    （要在 Rust 里别名就得引入 `Arc<Mutex<..>>`，会改变公开字段类型，§4.9 冻结）。
//! 4. **spawn 失败的错误文本**：见上；`io::Error` 的 errno 由 `node_errno_code` 映射成 Node 的
//!    `Exxx`，表外的罕见 errno 是 `UNKNOWN`（Node 对未映射 errno 同样给出 `UNKNOWN`）。
//! 5. **stdin 写错误**：Node 的 `EPIPE` 错误同时经 `stdin.on("error")` 发 error 并 reject `send`，
//!    流随后 `destroy()`；之后的 `send()` 抛 `McpConnectionClosedError` 且不再发 error 事件。
//!    Rust 同样三件事都做（错误文本 `write EPIPE`）。写路径按 Node 的 `Writable` 语义分成两条：
//!    流空闲时**同步** `write(2)`（成功立刻返回；`EPIPE` 当场 destroy，因此同一批并发 `send()`
//!    里只有第一个报 `write EPIPE`，其余抛 closed）；管道写满时进入内部缓冲，断流后缓冲里的
//!    写入**全部**以同一个错误失败，而流只发**一个** error 事件。
//! 6. **UTF-16 截断**：`ERR_INVALID_ARG_VALUE` 的消息按 Node 的 `inspected.length > 128` /
//!    `slice(0, 128)` 截断，计数单位是 **UTF-16 码元**（emoji 算 2 个），切点落在代理对中间时
//!    Node 留下孤立代理项；Rust 的 `String` 无法表示它，写成 U+FFFD（见 `truncate_utf16`）。
//! 7. **监听器异常**：TS 里 error listener 再抛会冲出 `handleStdout` 进入 Node 的 data 回调
//!    （未捕获异常）；Rust 的读取任务没有调用方，只能丢弃（§30.4 已记录该差异）。
//! 8. **非 Unix**：没有进程组；关闭阶梯用 `taskkill /pid <pid> /T /F`（与 TS 的 win32 分支一致）。
//!    Windows 分支未经本机（linux）验证，属"编译通过、行为按 TS 分支复刻"的占位实现。
//! 9. **stderr 分块**：读缓冲是 64 KiB（libuv 的 `data` chunk 上限），因此 `on_stderr` 的分块
//!    边界与 Node 相同；单个 chunk 内的 lossy 解码与 `Buffer.toString("utf8")` 一致（跨块的半个
//!    多字节字符同样变成 U+FFFD）。
//! 10. **setTimeout 钳位**：> 2^31-1 的 `closeTimeoutMs` 与 0 都按 1 ms 生效（Node 的
//!     `TimeoutOverflowWarning` 路径），见 `node_timer_delay`。钳位后 SIGKILL 定时器可能**早于**
//!     SIGTERM 定时器到期；Node 的定时器按到期时间触发，因此关闭阶梯也按"两个截止时间里较早的
//!     一个"等待，而不是固定先等 SIGTERM。
//! 11. **并发 start/close**：TS 的 `start()` 在第一个 await 之前就发布 `this.child`，因此
//!     `close()` 总能看见它。Rust 用"同一把锁内完成 spawn 与发布"复刻（见
//!     `start_impl`），否则会出现 close 先返回、子进程被泄漏的竞态。
//!
//! # 职责与调用方
//!
//! 本文件把子进程的 stdin/stdout 接入统一的 [`McpTransport`] 抽象：上层（`client.rs` 的
//! `McpClient`、`kiana-builtin-extensions` 的 MCP runtime）只看到"发 Value、收 Value、关连接"，
//! 不需要区分本地进程与 HTTP。边界是**进程生命周期 + 按行分帧**：JSON 语法校验由
//! `kiana_json::js_parse` 负责，JSON-RPC 形状校验由 `parse_json_rpc_message_value` 负责，
//! 本文件只把它们的 `Result` 转成 message / error 事件。请求与响应的配对、超时和取消由
//! `client.rs` 的 pending 表负责；本传输只按消息到达的顺序派发，不保存任何请求状态。
//!
//! `kiana-builtin-extensions` 会直接构造 `StdioTransport` 并在创建时登记，以便连接关闭后取
//! `stderr()` 拼错误提示（TS 用 `instanceof StdioTransport`；Rust 的 trait object 不能
//! 向下转型，所以只能另存一份 `Arc`）。crate 根只再导出 `StdioTransport` 与
//! `StdioTransportOptions`（见 `index.rs`）；`StdioStderrMode`、`StdioStderrListener` 等
//! 模块级名字要用 `transports::stdio` 路径访问。
//!
//! # 主要数据结构与后台任务
//!
//! - `StdioTransport` 是外层句柄，内部是 `Arc<Inner>`；克隆共享同一条连接、同一组监听器与
//!   同一份缓冲，不存在独立的"连接副本"。
//! - `Inner` 持有选项、监听器表 [`TransportEvents`]、`State` 与 `child_closed` 电平信号。
//!   `State` 由一把 `std::sync::Mutex` 保护；持锁期间不做 `.await`，所有等待都在锁外。
//! - `StdinPipe` 复刻 Node `Writable` 的两条写路径：流空闲时同步 `write(2)`，流忙或写满时
//!   进入 FIFO 缓冲，由唯一的 `run_stdin_writer` 任务串行冲刷。
//! - `start` 最多派生四个任务：stdout 读取、stderr 读取、stdin 写入、`child.wait()`。
//!   两个读任务在 EOF/读错误后把各自的 `watch` 电平置 true；`wait` 任务要等两个读任务都
//!   结束后才调用 `handle_child_close`，因此 stdout 的残余字节总在 `emitClose` 之前处理。
//!
//! # 一次完整生命周期
//!
//! 1. `new`：冻结选项，不 spawn。
//! 2. `start`：校验参数 → spawn → 发布 pid 与 stdin 写入口（同一互斥区）→ 启动后台任务。
//! 3. 运行中：stdout 的行变成消息/错误事件；stderr 只保留尾部；`send` 把序列化后的行写进
//!    stdin，进入内部缓冲时等待写任务真正写入。
//! 4. 子进程退出：`child.wait()` 返回 → `handle_child_exit`（stdin 立即不可写、注销进程组）
//!    → stdout/stderr 读到 EOF → `handle_child_close`（残留缓冲报错、`emitClose`、置位
//!    `child_closed`）。
//! 5. `close`：见上面的"关闭阶梯"，幂等；它在某些状态下**不**立刻 `emitClose`（子进程已
//!    退出但 close 事件未到、或 spawn 失败且 close 事件在途），close 事件由后台任务补发
//!    （正常路径是第 4 步的 wait 任务；spawn 失败时是 `start_impl` 单独派生的那个任务）。
//!
//! # 阅读顺序
//!
//! `node_timer_delay` → `computed_env`/`configure_command`/`spawn_child`（怎么起进程）→
//! `StdinPipe`/`try_inline_write`/`run_stdin_writer`（怎么写）→
//! `Inner::handle_stdout`/`handle_stderr`/`handle_child_exit`/`handle_child_close`
//! （怎么读与收尾）→ `StdioTransport::start_impl`/`send_impl`/`close_impl`（三个对外入口）。

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once, OnceLock};
use std::task::{Context, Poll, Waker};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_sandbox::{
    SandboxDecision, SandboxManager, SpawnOptions as SandboxSpawnOptions,
    StdioMode as SandboxStdioMode, ToolInput,
};
use serde_json::Value;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::{ChildStdin, Command};
use tokio::sync::{oneshot, watch};

use crate::auth_provider::McpTransportError;
use crate::jsonrpc::{McpError, parse_json_rpc_message_value};
use crate::transports::{
    McpTransport, McpTransportCloseListener, McpTransportErrorListener,
    McpTransportMessageListener, McpTransportSendOptions, Subscription, TransportEvents,
};

/// `DEFAULT_MAX_STDERR_BYTES = 64 * 1024`（§15）。
const DEFAULT_MAX_STDERR_BYTES: usize = 64 * 1024;

/// stdio 单行消息的默认上限：**8 MiB**。
///
/// 与 HTTP/SSE 的 [`DEFAULT_MAX_MESSAGE_BYTES`](crate::transports::DEFAULT_MAX_MESSAGE_BYTES)
///（16 MiB）刻意分开：stdio 子进程是本地
/// 不受信服务器，单行超过 8 MiB 基本意味着协议错误或内存攻击面，整行丢弃并报错比继续
/// 缓冲更安全。调用方仍可用 [`StdioTransportOptions::max_message_bytes`] 覆盖。
pub const DEFAULT_STDIO_MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// 默认透传的环境变量名（Unix 大小写敏感；代理变量同时列大小写两种写法）。
pub const DEFAULT_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "SHELL",
    "LANG",
    "TMPDIR",
    "TMP",
    "TEMP",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
];

/// 名字是否命中默认敏感模式：`*_KEY` / `*_TOKEN` / `*_SECRET` / `*_PASSWORD` /
/// `*_CREDENTIAL*`。
///
/// 比较前统一转大写，因此 `openai_api_key` 也会被剥离。这些变量即便匹配了白名单
/// （理论上不会，白名单里没有敏感名）也不继承，显式配置的 `env` 是唯一覆盖方式。
pub fn is_sensitive_env_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.contains("_KEY")
        || upper.contains("_TOKEN")
        || upper.contains("_SECRET")
        || upper.contains("_PASSWORD")
        || upper.contains("_CREDENTIAL")
}

/// 名字是否属于默认白名单（`LC_*` 按前缀、其余精确匹配）。
pub fn is_default_allowed_env_name(name: &str) -> bool {
    DEFAULT_ENV_ALLOWLIST.contains(&name) || name.starts_with("LC_")
}

/// 子进程环境变量白名单策略。
///
/// 默认行为（字段全空）只继承 [`DEFAULT_ENV_ALLOWLIST`] 与 `LC_*`，并剥离所有敏感名。
/// `extra_allow` 用于显式放行额外变量（例如某个服务器需要的 `GITHUB_HOST`）。
/// 服务器配置里的 `env` 条目总是最后写入，允许显式覆盖（包括敏感变量）——这是配置者
/// 的明确意图，不是继承泄漏。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StdioEnvPolicy {
    /// 额外放行的环境变量名（精确匹配）。
    pub extra_allow: Vec<String>,
}

impl StdioEnvPolicy {
    /// 判断一个继承变量是否允许。
    pub fn allows(&self, name: &str) -> bool {
        is_default_allowed_env_name(name) || self.extra_allow.iter().any(|extra| extra == name)
    }
}

/// 按白名单计算子进程环境：继承变量按 [`StdioEnvPolicy`] 过滤，然后写入显式 `env` 覆盖。
///
/// 顺序保证：先过滤父进程环境，再插入 explicit，因此 explicit 中的敏感变量会保留。
/// 父进程变量名必须是合法 UTF-8（Node 的 `{...process.env}` 同样看不到非法 UTF-8 名字）。
pub fn sanitized_env<I>(
    parent: I,
    explicit: &IndexMap<String, String>,
    policy: &StdioEnvPolicy,
) -> IndexMap<String, String>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut env: IndexMap<String, String> = IndexMap::new();
    for (key, value) in parent {
        if explicit.contains_key(&key) {
            continue;
        }
        if is_sensitive_env_name(&key) || !policy.allows(&key) {
            continue;
        }
        env.insert(key, value);
    }
    for (key, value) in explicit {
        env.insert(key.clone(), value.clone());
    }
    env
}
/// `DEFAULT_CLOSE_TIMEOUT_MS = 2000`（§15）。
const DEFAULT_CLOSE_TIMEOUT_MS: u64 = 2_000;
/// `STDIN_CLOSE_GRACE_MS = 500`：关掉 stdin 后给服务器自己退出的时间（§15）。
const STDIN_CLOSE_GRACE_MS: u64 = 500;

/// `setTimeout(delay)` 的实际延迟（Node/libuv 的钳位规则）。
///
/// `2^31-1` 是 libuv 定时器能接受的最大值；更大的值触发 `TimeoutOverflowWarning` 并在 1 ms
/// 后触发（Node 把 `delay > 2^31-1 || delay < 1` 统一改成 1）。见 §30.25 第 4 条。
const NODE_MAX_TIMER_MS: u64 = 2_147_483_647;

/// 把毫秒延迟换算成 `setTimeout` 实际使用的值。
///
/// Node 的 `setTimeout` 对 `!(delay >= 1 && delay <= 2^31-1)` 一律改成 1 ms（含 `0`、`NaN`
/// 与超上限）。
fn node_timer_delay(delay_ms: u64) -> u64 {
    if (1..=NODE_MAX_TIMER_MS).contains(&delay_ms) {
        delay_ms
    } else {
        1
    }
}

/// `USE_PROCESS_GROUPS = process.platform !== "win32"`。
const USE_PROCESS_GROUPS: bool = cfg!(unix);

// ---------------------------------------------------------------------------
// 存活进程组注册表 + 退出钩子（§15.3）
// ---------------------------------------------------------------------------

/// 存活进程组 pid 的全局注册表；`OnceLock` 让 `atexit` 钩子也能访问它。
#[cfg(unix)]
fn live_process_groups() -> &'static Mutex<std::collections::HashSet<i32>> {
    static LIVE: OnceLock<Mutex<std::collections::HashSet<i32>>> = OnceLock::new();
    LIVE.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// `process.once("exit")` 的等价物：正常退出时对每个存活进程组发 SIGTERM。
#[cfg(unix)]
extern "C" fn kill_live_process_groups_at_exit() {
    // 先把注册表复制出来再解锁：随后逐个进程组发信号时不再持有互斥锁。
    let Some(groups) = live_process_groups()
        .lock()
        .ok()
        .map(|groups| groups.clone())
    else {
        return;
    };
    for pid in groups {
        // SAFETY: `killpg` 只读内核状态；错误一律忽略（与 TS 的 `try { process.kill(-pid) } catch {}` 一致）。
        unsafe {
            libc::killpg(pid, libc::SIGTERM);
        }
    }
}

/// `installExitHook()`：只安装一次。
#[cfg(unix)]
fn install_exit_hook() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        // SAFETY: `atexit` 注册一个只做 `killpg` 的 C 函数；`kill_live_process_groups_at_exit`
        // 不 panic、不做 I/O。注册失败（内存不足）时静默放弃，与 Node 无 exit 钩子的情形等价。
        unsafe {
            libc::atexit(kill_live_process_groups_at_exit);
        }
    });
}

/// 把 pid 加入 `liveProcessGroups`。
#[cfg(unix)]
fn register_process_group(pid: u32) {
    install_exit_hook();
    if let Ok(mut groups) = live_process_groups().lock() {
        groups.insert(pid as i32);
    }
}

/// `child.once("exit", () => liveProcessGroups.delete(pid))`。
#[cfg(unix)]
fn unregister_process_group(pid: u32) {
    if let Ok(mut groups) = live_process_groups().lock() {
        groups.remove(&(pid as i32));
    }
}

/// `killProcessTree(child, signal)`（§15.3）。
///
/// Unix：负 pid（整个进程组）发信号；组不在（ESRCH）时回退直接子进程。错误一律吞掉，
/// 与 TS 的 `try/catch {}` / `child.kill(signal)` 一致。
#[cfg(unix)]
fn kill_process_tree(pid: u32, signal: i32) {
    if USE_PROCESS_GROUPS && unsafe { libc::killpg(pid as i32, signal) } == 0 {
        return;
    }
    unsafe {
        libc::kill(pid as i32, signal);
    }
}

/// `killProcessTree` 的 win32 分支：`taskkill /pid <pid> /T /F`（信号只有一种，忽略 `signal`）。
#[cfg(not(unix))]
fn kill_process_tree(pid: u32, _signal: i32) {
    use std::process::{Command as StdCommand, Stdio};
    let _ = StdCommand::new("taskkill")
        .arg("/pid")
        .arg(pid.to_string())
        .args(["/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// SIGTERM 的编号（TS 传的是信号名；Rust 传编号）。
#[cfg(unix)]
const SIGTERM: i32 = libc::SIGTERM;
/// SIGKILL 的编号。
#[cfg(unix)]
const SIGKILL: i32 = libc::SIGKILL;
#[cfg(not(unix))]
const SIGTERM: i32 = 15;
#[cfg(not(unix))]
const SIGKILL: i32 = 9;

// ---------------------------------------------------------------------------
// 选项
// ---------------------------------------------------------------------------

/// `onStderr?: (chunk: string) => void`。
///
/// 每个从管道读到的原始 chunk 调用一次（不是尾部缓冲的内容）；回调在 stderr 读取任务里
/// 同步执行，阻塞会推迟后续 chunk 的读取。`stderr = "inherit"` 时没有管道，回调不会被调用。
pub type StdioStderrListener = Arc<dyn Fn(&str) + Send + Sync>;

/// `stderr: "pipe" | "inherit"`。
///
/// TS 的类型是字符串字面量联合，运行时任何非 `"inherit"` 的值都落到 `"pipe"`
/// （`stderr === "inherit" ? "inherit" : "pipe"`）；Rust 用枚举把这一点收窄成两个合法值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StdioStderrMode {
    /// `"pipe"`（默认）：捕获 stderr 并保留尾部；每个原始 chunk 解码后也交给 `on_stderr`。
    #[default]
    Pipe,
    /// `"inherit"`：直接继承父进程的 stderr（不捕获、没有 `stderr()` 内容）。
    Inherit,
}

/// `StdioTransportOptions`（§15）。
///
/// TS 的 `options` 会被 `Object.freeze` 且 `args` 被拷贝一份；[`StdioTransport::new`] 做同样的冻结
/// （克隆 `args`/`env`，忽略调用方之后的修改）。
///
/// TS: `transports/stdio.ts#StdioTransportOptions`
#[derive(Clone)]
pub struct StdioTransportOptions {
    /// 可执行文件（或 PATH 上的命令名）。
    ///
    /// 空字符串或含 NUL 会被 `start()` 拒绝（`ERR_INVALID_ARG_VALUE`）。
    pub command: String,
    /// 参数列表（`args ?? []`）。任一项含 NUL 会被 `start()` 拒绝。
    pub args: Option<Vec<String>>,
    /// 子进程工作目录（`undefined` = 继承）。
    ///
    /// 空路径按"未设置"处理，子进程在父进程的当前目录里运行（见 `configure_command`）；
    /// 含 NUL 会被 `start()` 拒绝。
    pub cwd: Option<PathBuf>,
    /// 追加/覆盖的环境变量。键或值含 NUL 会被 `start()` 拒绝。
    pub env: Option<IndexMap<String, String>>,
    /// `inheritEnv === false` 时**不**继承 `process.env`（默认 `true`）。
    pub inherit_env: Option<bool>,
    /// 继承环境变量的白名单。`None` 保持旧行为（完整继承）；`Some` 时只继承
    /// [`StdioEnvPolicy`] 允许且非敏感的变量，再叠加 `env` 的显式覆盖。
    ///
    /// MCP 默认传输会设置 `Some(StdioEnvPolicy::default())`，避免把宿主进程的
    /// `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` 泄漏给第三方 MCP 服务器。
    pub env_policy: Option<StdioEnvPolicy>,
    /// `"pipe"`（默认）或 `"inherit"`。
    pub stderr: Option<StdioStderrMode>,
    /// 收到原始 stderr chunk 时回调（解码为 UTF-8，见模块文档）。
    pub on_stderr: Option<StdioStderrListener>,
    /// 单条消息的字节上限（默认 [`DEFAULT_STDIO_MAX_LINE_BYTES`] = 8 MiB，不含 `\n`，
    /// 含可能存在的尾随 `\r`）。
    ///
    /// 按**原始行字节**计数；`0` 表示任何长度大于 0 的行都超限。超限行被整行丢弃并发
    /// error 事件，传输继续读下一行。
    pub max_message_bytes: Option<usize>,
    /// stderr 尾部缓冲上限（默认 64 KiB）。
    ///
    /// `0` 表示不截断：尾部缓冲会随 stderr 持续增长（见 `handle_stderr`）。
    pub max_stderr_bytes: Option<usize>,
    /// SIGTERM 之后等待子进程退出的时间（默认 2000 ms）。
    ///
    /// 参与定时器计算时按 `setTimeout` 的钳位规则处理（见 `node_timer_delay` 与模块文档
    /// "与 TS 的实现差异"第 10 条）；超过 `2^31-1` 的值会让 SIGKILL 定时器早于 SIGTERM 到期。
    pub close_timeout_ms: Option<u64>,
    /// stdio 子进程的沙箱。`None` 时保持原有行为（直接 spawn，不隔离）。
    ///
    /// 会话层把与 bash/execute_bash 相同的 `SandboxManager` 传进来：`spawn_child` 先调用
    /// `ExecutionPolicy::resolve("mcp", McpProcess)`，拒绝时不创建进程；后端不可用时
    /// `SandboxManager::spawn` 返回 `Unavailable`，调用方必须当成 transport 启动失败，
    /// **不能**回退到无沙箱 spawn。
    pub sandbox: Option<Arc<SandboxManager>>,
}

impl StdioTransportOptions {
    /// 只指定命令的选项（其余取默认）。
    pub fn new(command: impl Into<String>) -> Self {
        StdioTransportOptions {
            command: command.into(),
            args: None,
            cwd: None,
            env: None,
            inherit_env: None,
            env_policy: None,
            stderr: None,
            on_stderr: None,
            max_message_bytes: None,
            max_stderr_bytes: None,
            close_timeout_ms: None,
            sandbox: None,
        }
    }

    /// `Object.freeze({ ...options, args: options.args ? [...options.args] : undefined })`。
    fn frozen(&self) -> Self {
        StdioTransportOptions {
            command: self.command.clone(),
            args: self.args.clone(),
            cwd: self.cwd.clone(),
            env: self.env.clone(),
            inherit_env: self.inherit_env,
            env_policy: self.env_policy.clone(),
            stderr: self.stderr,
            on_stderr: self.on_stderr.clone(),
            max_message_bytes: self.max_message_bytes,
            max_stderr_bytes: self.max_stderr_bytes,
            close_timeout_ms: self.close_timeout_ms,
            sandbox: self.sandbox.clone(),
        }
    }
}

impl std::fmt::Debug for StdioTransportOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StdioTransportOptions")
            .field("command", &self.command)
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .field("env", &self.env)
            .field("inherit_env", &self.inherit_env)
            .field("env_policy", &self.env_policy)
            .field("stderr", &self.stderr)
            .field("on_stderr", &self.on_stderr.as_ref().map(|_| "<callback>"))
            .field("max_message_bytes", &self.max_message_bytes)
            .field("max_stderr_bytes", &self.max_stderr_bytes)
            .field("close_timeout_ms", &self.close_timeout_ms)
            .field("sandbox", &self.sandbox.is_some())
            .finish()
    }
}

/// `{ ...process.env, ...options.env }`（§15.1），可选的子进程白名单。
fn computed_env(options: &StdioTransportOptions) -> IndexMap<String, String> {
    if options.inherit_env == Some(false) {
        return options.env.clone().unwrap_or_default();
    }
    // `{...process.env}` 枚举不到名字不是合法 UTF-8 的条目（Node 把它们藏在
    // `getOwnPropertyNames` 里且 descriptor 为 undefined，spread/`Object.keys` 都丢弃），
    // 因此这里也跳过；值则被 `process.env` 以 lossy UTF-8 解码（U+FFFD），与
    // `to_string_lossy` 一致（已实测）。
    let parent = std::env::vars_os().filter_map(|(key, value)| {
        let key = key.into_string().ok()?;
        Some((key, value.to_string_lossy().into_owned()))
    });
    let explicit = options.env.clone().unwrap_or_default();
    if let Some(policy) = &options.env_policy {
        return sanitized_env(parent, &explicit, policy);
    }
    let mut env: IndexMap<String, String> = parent.collect();
    for (key, value) in &explicit {
        env.insert(key.clone(), value.clone());
    }
    env
}

/// `crossSpawn(command, args ?? [], { cwd, env, stdio, windowsHide: true, detached })`
/// 对应的 [`Command`]（`spawn` 之前的部分）。
///
/// 空字符串的 `cwd` 在 Node 里等同于"不传"（`spawn(..., {cwd: ""})` 在父进程当前目录里运行子
/// 进程，已实测），而 `Command::current_dir("")` 会 ENOENT。这里把空路径当作 `None`。
fn build_command(options: &StdioTransportOptions) -> Command {
    let args = options.args.as_deref().unwrap_or(&[]);
    configure_command(&options.command, args, options)
}

/// 组装 `program` + `args`，其余选项（cwd/env/stdio/进程组）取自 `options`。
fn configure_command(program: &str, args: &[String], options: &StdioTransportOptions) -> Command {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(cwd) = &options.cwd
        && !cwd.as_os_str().is_empty()
    {
        command.current_dir(cwd);
    }
    // 先清空再整体写入 `computed_env`：`inherit_env = false` 时子进程只看到 `options.env`；
    // 继承模式下 `computed_env` 已经把父进程环境合并进来，因此不会丢失。
    command.env_clear();
    command.envs(computed_env(options));
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    match options.stderr.unwrap_or_default() {
        StdioStderrMode::Inherit => {
            command.stderr(std::process::Stdio::inherit());
        }
        StdioStderrMode::Pipe => {
            command.stderr(std::process::Stdio::piped());
        }
    }
    // 不随 `Child` 句柄 drop 杀进程：子进程只由关闭阶梯显式发信号（进程正常退出时另有
    // `atexit` 钩子兜底）；句柄本身被移进 wait 任务，会一直活到 `wait()` 返回。
    command.kill_on_drop(false);
    // `detached: USE_PROCESS_GROUPS`：自建会话/进程组，关闭时可以杀掉服务器派生的子进程。
    #[cfg(unix)]
    if USE_PROCESS_GROUPS {
        // SAFETY: `setsid` 是 async-signal-safe 的（`pre_exec` 的唯一要求）；失败时把 errno
        // 交回 spawn，Node 的 detached 分支同样会失败。
        unsafe {
            command.as_std_mut().pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    command
}

/// spawn 子进程；失败时返回 `(Node 的错误, 是否为异步错误)`（见 [`spawn_error`]）。
///
/// `ENOEXEC` 特例：`execvp` 失败于 `ENOEXEC` 时，POSIX 的 shell-script 兼容行为是改用
/// `/bin/sh <file> <args...>` 重试，Node（经 libuv）因此能运行没有 shebang 的可执行脚本
/// （已实测：Node 里 `$0` 是脚本路径、`$@` 是原参数）。`std::process::Command` 不做这个重试，
/// 这里显式补上。
fn spawn_child(
    options: &StdioTransportOptions,
) -> Result<tokio::process::Child, (McpTransportError, bool)> {
    if let Some(sandbox) = &options.sandbox {
        return spawn_sandboxed_child(sandbox, options);
    }
    match build_command(options).spawn() {
        Ok(child) => Ok(child),
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::ENOEXEC) => {
            let args = options.args.as_deref().unwrap_or(&[]);
            match configure_command("/bin/sh", &[], options)
                .arg(&options.command)
                .args(args)
                .spawn()
            {
                Ok(child) => Ok(child),
                Err(error) => Err(spawn_error(&options.command, &error)),
            }
        }
        Err(error) => Err(spawn_error(&options.command, &error)),
    }
}

/// 沙箱路径：先 `ExecutionPolicy::resolve("mcp", McpProcess)`，再
/// `SandboxManager::spawn`。
///
/// MCP 是长生命周期服务器，使用 `spawn` 而不是 `spawn_relayed`：relay 的
/// `Unconfirmed` 语义是给一次性命令设计的，传输自己负责关闭阶梯与进程组。
/// 后端不可用时错误原样返回，调用方把它当作 transport 启动失败（不会回退到无沙箱）。
fn spawn_sandboxed_child(
    sandbox: &Arc<SandboxManager>,
    options: &StdioTransportOptions,
) -> Result<tokio::process::Child, (McpTransportError, bool)> {
    let args = options.args.clone().unwrap_or_default();
    let policy = sandbox.execution_policy();
    if let SandboxDecision::Deny(denial) = policy.resolve(
        "mcp",
        &ToolInput::McpProcess {
            program: &options.command,
            args: &args,
        },
    ) {
        return Err((McpTransportError::new(denial.message()), false));
    }
    let spawn_options = SandboxSpawnOptions {
        cwd: options.cwd.clone(),
        stdin: SandboxStdioMode::Pipe,
        stdout: SandboxStdioMode::Pipe,
        stderr: match options.stderr.unwrap_or_default() {
            StdioStderrMode::Inherit => SandboxStdioMode::Inherit,
            StdioStderrMode::Pipe => SandboxStdioMode::Pipe,
        },
        // 与 `configure_command` 的 `USE_PROCESS_GROUPS` 等价：关闭时按进程组发信号。
        detached: USE_PROCESS_GROUPS,
        windows_hide: false,
        env: Some(
            computed_env(options)
                .into_iter()
                .map(|(key, value)| (OsString::from(key), OsString::from(value)))
                .collect(),
        ),
        // MCP 传输不参与 W3C trace 上下文注入（由调用方按需扩展）。
        ..SandboxSpawnOptions::default()
    };
    match sandbox.spawn(&options.command, &args, &spawn_options) {
        Ok(child) => Ok(child),
        #[cfg(unix)]
        Err(kiana_sandbox::SandboxError::Spawn(error))
            if error.raw_os_error() == Some(libc::ENOEXEC) =>
        {
            // 与无沙箱路径的 ENOEXEC 兼容行为一致：改用 /bin/sh 执行脚本。
            let mut shell_args = Vec::with_capacity(args.len() + 1);
            shell_args.push(options.command.clone());
            shell_args.extend(args);
            sandbox
                .spawn("/bin/sh", &shell_args, &spawn_options)
                .map_err(|error| (McpTransportError::new(error.to_string()), false))
        }
        Err(error) => Err((McpTransportError::new(error.to_string()), false)),
    }
}

// ---------------------------------------------------------------------------
// Node 的系统错误（ErrnoException）与 spawn 参数校验（TypeError）
// ---------------------------------------------------------------------------

/// `util.getSystemErrorName(errno)` 的等价物（libuv 的 errno → `Exxx` 码表）。
///
/// Node 的 `ErrnoException` 用这张表把 `error.code` 与消息里的码都填成 `ENOENT` 这类名字；
/// 未命中的 errno 退化为 `UNKNOWN`（`uvErrmapGet` 未命中时取 `['UNKNOWN','unknown error']`）。
/// 这里列出 spawn/写管道可能遇到的常见项；表外的罕见 errno 与 Node 对未映射 errno 的行为一致。
#[cfg(unix)]
fn node_errno_code(errno: i32) -> &'static str {
    match errno {
        libc::E2BIG => "E2BIG",
        libc::EACCES => "EACCES",
        libc::EADDRINUSE => "EADDRINUSE",
        libc::EADDRNOTAVAIL => "EADDRNOTAVAIL",
        libc::EAFNOSUPPORT => "EAFNOSUPPORT",
        libc::EAGAIN => "EAGAIN",
        libc::EALREADY => "EALREADY",
        libc::EBADF => "EBADF",
        libc::EBUSY => "EBUSY",
        libc::ECANCELED => "ECANCELED",
        libc::ECHILD => "ECHILD",
        libc::ECONNABORTED => "ECONNABORTED",
        libc::ECONNREFUSED => "ECONNREFUSED",
        libc::ECONNRESET => "ECONNRESET",
        libc::EDESTADDRREQ => "EDESTADDRREQ",
        libc::EDOM => "EDOM",
        libc::EDQUOT => "EDQUOT",
        libc::EEXIST => "EEXIST",
        libc::EFAULT => "EFAULT",
        libc::EFBIG => "EFBIG",
        libc::EHOSTUNREACH => "EHOSTUNREACH",
        libc::EIDRM => "EIDRM",
        libc::EILSEQ => "EILSEQ",
        libc::EINPROGRESS => "EINPROGRESS",
        libc::EINTR => "EINTR",
        libc::EINVAL => "EINVAL",
        libc::EIO => "EIO",
        libc::EISCONN => "EISCONN",
        libc::EISDIR => "EISDIR",
        libc::ELOOP => "ELOOP",
        libc::EMFILE => "EMFILE",
        libc::EMLINK => "EMLINK",
        libc::EMSGSIZE => "EMSGSIZE",
        libc::ENAMETOOLONG => "ENAMETOOLONG",
        libc::ENETDOWN => "ENETDOWN",
        libc::ENETRESET => "ENETRESET",
        libc::ENETUNREACH => "ENETUNREACH",
        libc::ENFILE => "ENFILE",
        libc::ENOBUFS => "ENOBUFS",
        libc::ENODEV => "ENODEV",
        libc::ENOENT => "ENOENT",
        libc::ENOEXEC => "ENOEXEC",
        libc::ENOLCK => "ENOLCK",
        libc::ENOMEM => "ENOMEM",
        libc::ENOSPC => "ENOSPC",
        libc::ENOSYS => "ENOSYS",
        libc::ENOTCONN => "ENOTCONN",
        libc::ENOTDIR => "ENOTDIR",
        libc::ENOTEMPTY => "ENOTEMPTY",
        libc::ENOTSOCK => "ENOTSOCK",
        libc::ENOTSUP => "ENOTSUP",
        libc::ENOTTY => "ENOTTY",
        libc::ENXIO => "ENXIO",
        libc::EOVERFLOW => "EOVERFLOW",
        libc::EPERM => "EPERM",
        libc::EPIPE => "EPIPE",
        libc::EPROTO => "EPROTO",
        libc::EPROTONOSUPPORT => "EPROTONOSUPPORT",
        libc::EPROTOTYPE => "EPROTOTYPE",
        libc::ERANGE => "ERANGE",
        libc::EROFS => "EROFS",
        libc::ESPIPE => "ESPIPE",
        libc::ESRCH => "ESRCH",
        libc::ETIMEDOUT => "ETIMEDOUT",
        libc::ETXTBSY => "ETXTBSY",
        libc::EXDEV => "EXDEV",
        // macOS 上 EOPNOTSUPP(102) 与 ENOTSUP(45) 是不同的 errno（Linux 上两者相同）。
        #[cfg(target_os = "macos")]
        libc::EOPNOTSUPP => "EOPNOTSUPP",
        _ => "UNKNOWN",
    }
}

/// 非 Unix：没有 errno 码表，退回 [`io::ErrorKind`]（Windows 分支只要求编译通过；CI 目标是
/// linux + macOS）。
#[cfg(not(unix))]
fn io_error_code(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::NotFound => "ENOENT",
        io::ErrorKind::PermissionDenied => "EACCES",
        io::ErrorKind::BrokenPipe => "EPIPE",
        io::ErrorKind::ConnectionReset => "ECONNRESET",
        io::ErrorKind::ConnectionRefused => "ECONNREFUSED",
        io::ErrorKind::NotADirectory => "ENOTDIR",
        io::ErrorKind::IsADirectory => "EISDIR",
        io::ErrorKind::AlreadyExists => "EEXIST",
        io::ErrorKind::InvalidInput => "EINVAL",
        _ => "UNKNOWN",
    }
}

/// `error.code`（`util.getSystemErrorName(errno)`）。
#[cfg(unix)]
fn error_code(error: &io::Error) -> &'static str {
    match error.raw_os_error() {
        Some(errno) => node_errno_code(errno),
        None => "UNKNOWN",
    }
}

/// `error.code`（非 Unix：按 [`io::ErrorKind`] 映射）。
#[cfg(not(unix))]
fn error_code(error: &io::Error) -> &'static str {
    io_error_code(error)
}

/// `` `write ${code}` ``（`ErrnoException(err, "write")`）。
fn write_error(error: &io::Error) -> McpTransportError {
    let code = error_code(error);
    McpTransportError::with_code(format!("write {code}"), code)
}

/// Node `child_process.spawn` 的失败错误（`ErrnoException`），返回 `(error, emits_close)`。
///
/// Node 把 errno 分成两类（`ChildProcess.prototype.spawn` + `onexit`）：
/// - **异步**（`EACCES` / `EAGAIN` / `EMFILE` / `ENFILE` / `ENOENT`）：`spawn()` 返回 child，
///   下一个 tick 触发 `error`（`` `spawn ${file} ${code}` ``、`errno` 为负、`path` 为文件名），
///   随后 `exit`/`close` 事件照常触发（`exitCode = -errno`）。
/// - **同步抛出**（其余 errno，如 `E2BIG`、`ENAMETOOLONG`、`ENOTDIR`、`ELOOP`）：
///   `` `spawn ${code}` ``（**不带**文件名），没有 child、没有后续 `close` 事件。
fn spawn_error(command: &str, error: &io::Error) -> (McpTransportError, bool) {
    let code = error_code(error);
    let emitted = matches!(code, "ENOENT" | "EACCES" | "EAGAIN" | "EMFILE" | "ENFILE");
    let message = if emitted {
        format!("spawn {command} {code}")
    } else {
        format!("spawn {code}")
    };
    (McpTransportError::with_code(message, code), emitted)
}

/// `util.inspect(str)` 对**单个字符串**的输出（无 ANSI 颜色、无换行）。
///
/// `ERR_INVALID_ARG_VALUE` 的消息是 `` `... Received ${inspect(value)}` ``，所以需要逐字复刻：
/// 引号选择（默认 `'`；含 `'` 且不含 `"` 时用 `"`；两者都有且不含 `` ` ``/`${` 时用反引号）、
/// 控制字符与 `\x7f`–`\x9f` 写成 `\xNN`（**大写**十六进制）、`\b \t \n \f \r` 用短转义、`\\`，
/// 以及所选引号自身的转义。Rust `String` 里没有孤立代理项，故省略该分支。
fn inspect_string(value: &str) -> String {
    let quote = if !value.contains('\'') {
        '\''
    } else if !value.contains('"') {
        '"'
    } else if !value.contains('`') && !value.contains("${") {
        '`'
    } else {
        '\''
    };
    let mut escaped = String::with_capacity(value.len() + 2);
    for ch in value.chars() {
        let code = ch as u32;
        if ch == quote || ch == '\\' || code < 0x20 || (0x7f..=0x9f).contains(&code) {
            match ch {
                '\u{08}' => escaped.push_str("\\b"),
                '\t' => escaped.push_str("\\t"),
                '\n' => escaped.push_str("\\n"),
                '\u{0c}' => escaped.push_str("\\f"),
                '\r' => escaped.push_str("\\r"),
                '\\' => escaped.push_str("\\\\"),
                '\'' if quote == '\'' => escaped.push_str("\\'"),
                _ => escaped.push_str(&format!("\\x{code:02X}")),
            }
        } else {
            escaped.push(ch);
        }
    }
    let mut out = String::with_capacity(escaped.len() + 2);
    out.push(quote);
    out.push_str(&escaped);
    out.push(quote);
    out
}

/// `new ERR_INVALID_ARG_VALUE(name, value, reason)`（`TypeError`，`code = ERR_INVALID_ARG_VALUE`）。
///
/// 消息是 `` `The ${type} '${name}' ${reason}. Received ${inspect(value)}` ``；`name` 含 `.`
/// 时用 `property`（`options.cwd` / `options.env['K']`），否则用 `argument`（`file` /
/// `args[0]`）。`inspect` 结果超过 128 个字符时截断并加 `...`。
fn invalid_arg_value(name: &str, value: &str, reason: &str) -> McpTransportError {
    let mut inspected = inspect_string(value);
    // Node 的 `inspected.length` / `StringPrototypeSlice(inspected, 0, 128)` 按 **UTF-16 码元**
    // 计数与切分，不是 Unicode 标量：emoji 占 2 个码元（评审 finding #2）。
    if inspected.encode_utf16().count() > 128 {
        inspected = truncate_utf16(&inspected, 128) + "...";
    }
    let kind = if name.contains('.') {
        "property"
    } else {
        "argument"
    };
    McpTransportError::type_error_with_code(
        format!("The {kind} '{name}' {reason}. Received {inspected}"),
        "ERR_INVALID_ARG_VALUE",
    )
}

/// `String.prototype.slice(0, units)`（按 UTF-16 码元截断）。
///
/// 切点落在代理对中间时 Node 会留下一个**孤立代理项**；Rust 的 `String` 不能表示它，这里写成
/// U+FFFD（与把该字符串编码成 UTF-8 时 Node 的 `Buffer.from(str, "utf8")` 同样的替换）。见模块
/// 文档"与 TS 的实现差异"第 6 条。
fn truncate_utf16(value: &str, units: usize) -> String {
    let mut out = String::with_capacity(value.len().min(units * 4));
    let mut used = 0;
    for ch in value.chars() {
        let width = ch.len_utf16();
        if used + width > units {
            out.push('\u{FFFD}');
            break;
        }
        out.push(ch);
        used += width;
        if used == units {
            break;
        }
    }
    out
}

/// `child_process` 在 spawn 之前的同步参数校验（`normalizeSpawnArguments`）。
///
/// 任一项失败时 Node 抛 `TypeError`（`code = ERR_INVALID_ARG_VALUE`）：`start()` reject，
/// **没有** error/close 事件，`this.child` 保持 undefined（之后的 `close()` 只会 `emitClose()`）。
/// 校验顺序逐字：file 的 NUL → file 为空 → args 逐项 NUL → cwd 的 NUL → env 键/值 NUL。
fn validate_spawn_arguments(options: &StdioTransportOptions) -> Result<(), McpTransportError> {
    if options.command.contains('\0') {
        return Err(invalid_arg_value(
            "file",
            &options.command,
            "must be a string without null bytes",
        ));
    }
    if options.command.is_empty() {
        return Err(invalid_arg_value(
            "file",
            &options.command,
            "cannot be empty",
        ));
    }
    if let Some(args) = &options.args {
        for (index, arg) in args.iter().enumerate() {
            if arg.contains('\0') {
                return Err(invalid_arg_value(
                    &format!("args[{index}]"),
                    arg,
                    "must be a string without null bytes",
                ));
            }
        }
    }
    if let Some(cwd) = &options.cwd {
        // `getValidatedPath(cwd, "options.cwd")`：NUL 检查作用在路径字符串上。
        let cwd = cwd.to_string_lossy();
        if cwd.contains('\0') {
            return Err(invalid_arg_value(
                "options.cwd",
                &cwd,
                "must be a string, Uint8Array, or URL without null bytes",
            ));
        }
    }
    if let Some(env) = &options.env {
        for (key, value) in env {
            let name = format!("options.env['{key}']");
            if key.contains('\0') {
                return Err(invalid_arg_value(
                    &name,
                    key,
                    "must be a string without null bytes",
                ));
            }
            if value.contains('\0') {
                return Err(invalid_arg_value(
                    &name,
                    value,
                    "must be a string without null bytes",
                ));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 内部状态
// ---------------------------------------------------------------------------

/// stdin 流内部缓冲里的一项工作（Node 的 `stdin.write` 把数据交给流之后返回）。
enum StdinJob {
    /// 写入整段载荷；ack 报告 `stdin.write` 的回调结果。
    Write(Vec<u8>, oneshot::Sender<Result<(), io::Error>>),
    /// `stdin.end()`：冲刷已排队的写入并关闭写端。
    End,
}

/// 子进程 stdin 的写端 + Node `Writable` 流式的内部缓冲。
///
/// Node 的 `stdin.write(payload, cb)` 在流空闲时**同步**调用 `write(2)`：成功就立刻回调，
/// 失败（例如 EPIPE）会当场 `destroy()` 流（`writable === false`），因此同一 tick 里随后的
/// `send()` 会在 `stdin?.writable` 检查处抛 `McpConnectionClosedError`。管道写满时数据进入流的
/// 内部缓冲，回调稍后触发；此时若管道断开，**所有**已进入缓冲的写入都以同一个错误失败，而流
/// 只发**一个** `error` 事件（已用 TS 参考实现实测）。
///
/// 这里用"能拿到写端锁就同步写、拿不到（或队列非空）就排队"复刻这两条路径。
struct StdinPipe {
    /// 写端（`None` = 已 `end()` 或失败后丢弃）。
    slot: tokio::sync::Mutex<Option<ChildStdin>>,
    /// 内部缓冲队列（FIFO）。
    queue: Mutex<VecDeque<StdinJob>>,
    /// 叫醒写任务。
    notify: tokio::sync::Notify,
    /// Node 的 `stream.writable`：`end()`、子进程退出、写失败后都变 `false`；此后的
    /// `send()` 直接抛 `McpConnectionClosedError`，不再碰管道。
    writable: AtomicBool,
}

impl StdinPipe {
    fn new(stdin: ChildStdin) -> Self {
        StdinPipe {
            slot: tokio::sync::Mutex::new(Some(stdin)),
            queue: Mutex::new(VecDeque::new()),
            notify: tokio::sync::Notify::new(),
            writable: AtomicBool::new(true),
        }
    }

    fn is_writable(&self) -> bool {
        self.writable.load(Ordering::SeqCst)
    }

    /// Node 的 `stream.destroy()` / `stream.end()`：`writable === false`。
    fn close_write(&self) {
        self.writable.store(false, Ordering::SeqCst);
    }

    fn queue_is_empty(&self) -> bool {
        match self.queue.lock() {
            Ok(queue) => queue.is_empty(),
            Err(poisoned) => poisoned.into_inner().is_empty(),
        }
    }

    fn push(&self, job: StdinJob) {
        match self.queue.lock() {
            Ok(mut queue) => queue.push_back(job),
            Err(poisoned) => poisoned.into_inner().push_back(job),
        }
        self.notify.notify_one();
    }

    fn pop(&self) -> Option<StdinJob> {
        match self.queue.lock() {
            Ok(mut queue) => queue.pop_front(),
            Err(poisoned) => poisoned.into_inner().pop_front(),
        }
    }

    /// 把写入放进内部缓冲，返回"回调"的接收端。
    ///
    /// 调用方若持有写端锁，**必须先 push 再释放**：否则另一笔同时到来的写入会拿到锁、看到空
    /// 队列而插队同步写，打乱线上的字节顺序（Node 的 `send()` 到 `write()` 全是同步执行，
    /// 同一 tick 内不存在这种交错）。
    fn buffer(&self, payload: Vec<u8>) -> StdinSubmit {
        let (acknowledgement, result) = oneshot::channel();
        self.push(StdinJob::Write(payload, acknowledgement));
        StdinSubmit::Buffered(result)
    }

    /// `stdin.write(payload, cb)`：空闲流上同步 `write(2)`，否则进入内部缓冲。
    fn submit(&self, payload: Vec<u8>) -> StdinSubmit {
        if !self.is_writable() {
            return StdinSubmit::Closed;
        }
        let mut slot = match self.slot.try_lock() {
            Ok(slot) => slot,
            // 写任务正持有写端（流里已有一笔进行中的写入）：Node 会把数据缓冲起来。
            Err(_) => return self.buffer(payload),
        };
        if slot.is_none() {
            // 写任务已经结束（处理过 `End` 或写失败后丢掉了写端）：流不可写，按 closed 处理。
            return StdinSubmit::Closed;
        }
        if !self.queue_is_empty() {
            // 已有缓冲：FIFO 追加（Node 不会插队写）。
            return self.buffer(payload);
        }
        let stdin = slot.as_mut().expect("checked above");
        match try_inline_write(stdin, &payload) {
            InlineWrite::Done => StdinSubmit::Written,
            InlineWrite::Buffered(written) => {
                let mut payload = payload;
                // 已经写进内核的前缀丢掉，剩余字节进入内部缓冲。
                let rest = payload.split_off(written);
                self.buffer(rest)
            }
            InlineWrite::Failed(error) => {
                // Node 在写失败时 destroy 流：丢掉写端（关闭 fd），之后一律 closed。
                *slot = None;
                StdinSubmit::Failed(error)
            }
        }
    }

    /// 丢弃队列里剩下的写入：每一项都以 `error` 失败（Node 断流时对已缓冲写入的处理）。
    fn fail_pending(&self, error: &io::Error) {
        while let Some(job) = self.pop() {
            if let StdinJob::Write(_, acknowledgement) = job {
                let _ = acknowledgement.send(Err(clone_io_error(error)));
            }
        }
    }
}

/// 一次 `stdin.write(payload, cb)` 的结果。
enum StdinSubmit {
    /// 整段载荷已在当前调用里同步写进内核。
    Written,
    /// 载荷（或剩余部分）进入内部缓冲；接收端报告 `write` 回调的结果。
    Buffered(oneshot::Receiver<Result<(), io::Error>>),
    /// `stdin.writable === false`：`send` 抛 `McpConnectionClosedError`。
    Closed,
    /// 同步写失败（流已被 destroy），错误就是 `write` 回调的错误。
    Failed(io::Error),
}

/// 同步 `write(2)` 的结果。
enum InlineWrite {
    /// 整段载荷都交给了内核。
    Done,
    /// 只写进了前 `usize` 个字节（管道写满 / `EAGAIN`），剩余部分进入内部缓冲。
    Buffered(usize),
    /// 写失败（`EPIPE` 等）。
    Failed(io::Error),
}

/// 复刻 Node 在空闲流上的一次同步 `write(2)`。
///
/// tokio 已把子进程管道设成非阻塞，[`AsyncWrite::poll_write`] 恰好是"试写一次"：
/// `Ready(Ok(n))` 是 `write(2)` 的返回值，`Pending` 就是 `EAGAIN`（此后再靠写任务等待可写）。
/// noop waker 只用于这一次轮询；真正等待可写的是 [`run_stdin_writer`]。
fn try_inline_write(stdin: &mut ChildStdin, payload: &[u8]) -> InlineWrite {
    let mut context = Context::from_waker(Waker::noop());
    match Pin::new(stdin).poll_write(&mut context, payload) {
        Poll::Pending => InlineWrite::Buffered(0),
        Poll::Ready(Ok(written)) if written >= payload.len() => InlineWrite::Done,
        Poll::Ready(Ok(written)) => InlineWrite::Buffered(written),
        Poll::Ready(Err(error)) => InlineWrite::Failed(error),
    }
}

/// `io::Error` 的克隆（同一个 errno，文本一致）。
fn clone_io_error(error: &io::Error) -> io::Error {
    match error.raw_os_error() {
        Some(errno) => io::Error::from_raw_os_error(errno),
        None => io::Error::new(error.kind(), error.to_string()),
    }
}

/// 解析 `handleStdout` 得到的、待派发的事件（按出现顺序）。
enum StdoutOutput {
    /// 通过 JSON-RPC 形状校验、可以直接交给 `emit_message` 的消息。
    Message(Value),
    /// 超限、JSON 语法错误或 JSON-RPC 形状不合法；交给 `emit_error`。
    Error(McpTransportError),
}

/// 传输的可变状态（互斥保护）。
///
/// 不变式：所有字段都由同一把锁保护；更新只在临界区内完成，监听器一律在锁外调用
/// （`handle_stdout` 先把事件收集到 `Vec`，解锁后再派发），所以监听器里回调 `send`/`close`
/// 不会在同一线程上重入这把锁。
struct State {
    /// `start()` 已被调用过。**在 spawn 之前**置位，因此 spawn/参数校验失败后仍为 true，
    /// 再次 `start()` 会报 already started。
    started: bool,
    /// `close()` 已进入关闭流程（幂等标记）；之后的 `send` 报连接已关闭。`start` 只在
    /// **从未启动过**时报连接已关闭：已启动过的传输（包括参数校验/spawn 失败后 `started`
    /// 已置为 true 的）会先命中 `started` 检查，返回 `MCP stdio transport already started`，
    /// 因为 `start_impl` 先查 `started`、再查 `closed`。
    closed: bool,
    /// 子进程 pid（`child` 存在时才有；TS 的 `this.child?.pid`）。
    pid: Option<u32>,
    /// 子进程尚未被回收（TS 的 `child.exitCode === null && child.signalCode === null`）。
    child_alive: bool,
    /// 子进程 `close` 事件的处理已排定但还没跑（spawn 失败时 Node 也会在下一个 tick 触发 `close`）。
    close_pending: bool,
    /// stdin 写端（`None` = 不可写：`end()` 之后、写失败之后、或子进程已退出）。
    stdin: Option<Arc<StdinPipe>>,
    /// 未按行切分的 stdout 原始字节。
    stdout_buffer: Vec<u8>,
    /// stderr 尾部（最多 `max_stderr_bytes` 字节）。
    stderr_buffer: VecDeque<u8>,
}

/// 传输共享状态。任务（stdout/stderr 读取、子进程等待）与传输对象都持有它。
struct Inner {
    options: StdioTransportOptions,
    events: TransportEvents,
    state: Mutex<State>,
    /// 子进程 `close` 事件已处理（`watch` 是电平触发的，订阅方不会错过）。
    child_closed: watch::Sender<bool>,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        match self.state.lock() {
            Ok(state) => state,
            // 监听器 panic 不会发生在持锁期间（派发前先解锁），但仍不让一次 panic 毒化整个传输。
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// `handleStdout(chunk)`（§15.4）：切行、解析、派发；锁内收集、锁外派发，监听器因此可以安全地回调 `send`/`close`。
    fn handle_stdout(&self, chunk: &[u8]) {
        let max_message_bytes = self
            .options
            .max_message_bytes
            .unwrap_or(DEFAULT_STDIO_MAX_LINE_BYTES);
        let mut outputs: Vec<StdoutOutput> = Vec::new();
        {
            let mut state = self.lock();
            state.stdout_buffer.extend_from_slice(chunk);
            // 两种超限路径：带换行的超长行整行丢弃后继续读下一行；没有换行的
            // 缓冲超限则清空整段缓冲，避免缓冲无界增长（该段剩余字节不会拼回后续输入）。
            loop {
                let Some(newline) = state.stdout_buffer.iter().position(|byte| *byte == b'\n')
                else {
                    if state.stdout_buffer.len() > max_message_bytes {
                        state.stdout_buffer.clear();
                        outputs.push(StdoutOutput::Error(McpTransportError::new(format!(
                            "MCP stdio message exceeds {max_message_bytes} bytes"
                        ))));
                    }
                    break;
                };
                let line: Vec<u8> = state.stdout_buffer.drain(..=newline).collect();
                let line = &line[..newline];
                // 长度检查发生在剥 `\r` 与空白行判定之前，所以 `line` 仍含尾随 `\r`。
                if line.len() > max_message_bytes {
                    outputs.push(StdoutOutput::Error(McpTransportError::new(format!(
                        "MCP stdio message exceeds {max_message_bytes} bytes"
                    ))));
                    continue;
                }
                // 整行按 UTF-8 lossy 解码：无法解码的字节变成 U+FFFD。替换若落在 JSON 字符串
                // 内部，U+FFFD 是合法字符串字符，`kiana_json::js_parse::parse` 仍会解析成功并
                // 照常经 `emit_message` 派发（传输保持打开）；只有替换落在字符串外或使 JSON
                // 结构非法时，才会走下面的 SyntaxError 分支（传输同样保持打开）。
                let text = String::from_utf8_lossy(line);
                // `.replace(/\r$/, "")`：只剥一个尾随 CR。
                let text = text.strip_suffix('\r').unwrap_or(&text);
                if kiana_json::js_string::js_trim(text).is_empty() {
                    continue;
                }
                match kiana_json::js_parse::parse(text) {
                    // `parseJsonRpcMessage(JSON.parse(text))`：TS 通过类型守卫后返回同一个对象，
                    // 所以派发的是**原值**（未知顶层键、键序、params 的原始形状都保留）。
                    Ok(value) => match parse_json_rpc_message_value(&value) {
                        Ok(message) => outputs.push(StdoutOutput::Message(message)),
                        Err(error) => outputs.push(StdoutOutput::Error(error.into())),
                    },
                    // TS 的 `JSON.parse` 抛 `SyntaxError`（`name === "SyntaxError"`），不是普通 Error。
                    Err(error) => outputs.push(StdoutOutput::Error(McpTransportError::syntax(
                        error.message,
                    ))),
                }
            }
        }
        self.dispatch_stdout(outputs);
    }

    /// 派发 `handleStdout` 的事件（锁外；监听器可以安全地回调 `send`/`close`）。
    fn dispatch_stdout(&self, outputs: Vec<StdoutOutput>) {
        for output in outputs {
            match output {
                StdoutOutput::Message(message) => {
                    // TS：`try { emitMessage(...) } catch (e) { emitError(e) }`。
                    if let Err(error) = self.events.emit_message(&message) {
                        let _ = self.events.emit_error(&error);
                    }
                }
                StdoutOutput::Error(error) => {
                    // error listener 再抛时 TS 会冲出 data 回调（未捕获异常）；读取任务无处可抛，丢弃。
                    let _ = self.events.emit_error(&error);
                }
            }
        }
    }

    /// `handleStderr(chunk)`（§15.5）。
    fn handle_stderr(&self, chunk: &[u8]) {
        let max_stderr_bytes = self
            .options
            .max_stderr_bytes
            .unwrap_or(DEFAULT_MAX_STDERR_BYTES);
        {
            let mut state = self.lock();
            state.stderr_buffer.extend(chunk.iter().copied());
            // TS 是 `subarray(-max)`：`max === 0` 时 `subarray(-0)` 就是 `subarray(0)`，
            // 即**整段保留**（缓冲无限增长）。用 `len - max` 会错误地清空，所以先判 0。
            if max_stderr_bytes > 0 && state.stderr_buffer.len() > max_stderr_bytes {
                let excess = state.stderr_buffer.len() - max_stderr_bytes;
                state.stderr_buffer.drain(..excess);
            }
        }
        // 回调在锁外执行，因此回调里可以安全地再调用 `stderr()`/`pid()`；它在读取任务里
        // 同步运行，回调阻塞多久，后续 stderr chunk 的读取就被推迟多久。
        if let Some(on_stderr) = &self.options.on_stderr {
            on_stderr(&String::from_utf8_lossy(chunk));
        }
    }

    /// 子进程 `exit`：`liveProcessGroups.delete(pid)`，标记已退出（`close` 事件还没到）。
    ///
    /// Node 在 `exit` 时销毁子进程的 stdio 流（stdin 不再 `writable`），因此这里同时让 `send`
    /// 走 `McpConnectionClosedError`，而不是把字节写进已断开的管道。写任务若正卡在一笔写入上，
    /// 内核会给出 `EPIPE`（Node 同样如此：`write EPIPE` + 一个 error 事件）；`End` 只是让空闲的
    /// 写任务退出。
    fn handle_child_exit(&self) {
        let (pid, stdin) = {
            let mut state = self.lock();
            state.child_alive = false;
            (state.pid, state.stdin.take())
        };
        if let Some(stdin) = stdin {
            stdin.close_write();
            stdin.push(StdinJob::End);
        }
        if let Some(pid) = pid {
            #[cfg(unix)]
            unregister_process_group(pid);
            #[cfg(not(unix))]
            let _ = pid;
        }
    }

    /// 子进程 `close`：残留缓冲报错 → 清缓冲 → `emitClose()`（§15.1）。
    fn handle_child_close(&self) {
        let (incomplete, stdin) = {
            let mut state = self.lock();
            state.pid = None;
            state.child_alive = false;
            state.close_pending = false;
            let stdin = state.stdin.take();
            let leftover = String::from_utf8_lossy(&state.stdout_buffer).into_owned();
            state.stdout_buffer.clear();
            (!kiana_json::js_string::js_trim(&leftover).is_empty(), stdin)
        };
        // `child.stdin` 随 child 一起消失：之后的 `send()` 抛 `McpConnectionClosedError`。
        if let Some(stdin) = stdin {
            stdin.close_write();
        }
        if incomplete {
            let _ = self.events.emit_error(&McpTransportError::new(
                "MCP stdio server closed with an incomplete JSON-RPC message",
            ));
        }
        let _ = self.events.emit_close();
        // 置位放在 `emit_close` 之后：`close_impl` 的等待者被唤醒时，close 监听器已经跑完。
        self.child_closed.send_replace(true);
    }

    /// `child.stdin?.end()`（同时让后续 `send` 走 `McpConnectionClosedError`）。
    fn end_stdin(&self) {
        let stdin = {
            let mut state = self.lock();
            state.stdin.take()
        };
        if let Some(stdin) = stdin {
            // `end()` 让 `writable` 立刻变 `false`；已排队的写入先冲刷，然后关闭写端。
            stdin.close_write();
            stdin.push(StdinJob::End);
        }
    }

    /// stdin 写失败：Node 的流发出一个 `error` 事件并 `destroy()`（`writable === false`）。
    ///
    /// 已缓冲的写入全部以同一个错误失败，但流只发**一个** `error` 事件（TS 实测：同一 tick 里
    /// 的多个 `send()` 只有第一个报 `write EPIPE`，其余抛 `McpConnectionClosedError`；缓冲中的
    /// 大块写入则一起以 `EPIPE` 失败、只发一个事件）。事件本身受 `closed` 抑制（TS 的
    /// `stdin.on("error")` 处理器里带 `!this.closed` 守卫）。
    fn report_stdin_failure(&self, pipe: &Arc<StdinPipe>, error: &io::Error) {
        pipe.close_write();
        {
            let mut state = self.lock();
            // 只有共享状态里登记的仍是本 pipe 时才清空：`close()`/`handle_child_exit()` 可能
            // 已经把它取走，此时不应再改动状态。
            if state
                .stdin
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, pipe))
            {
                state.stdin = None;
            }
        }
        pipe.fail_pending(error);
        if !self.is_closed() {
            let _ = self.events.emit_error(&write_error(error));
        }
    }
}

// ---------------------------------------------------------------------------
// StdioTransport
// ---------------------------------------------------------------------------

/// `StdioTransport`（§15）：用子进程的 stdin/stdout 承载 JSON-RPC 的 `McpTransport`。
///
/// `Arc` 共享（clonable）：`start`/`send`/`close` 都取 `&self`，与 [`McpTransport`] 一致。
/// 克隆共享同一份 `Inner`，因此连接状态、监听器与缓冲都不独立：任一 clone 调用 `close()`
/// 之后，其他 clone 的 `send()` 同样会报连接已关闭。
///
/// TS: `transports/stdio.ts#StdioTransport`
#[derive(Clone)]
pub struct StdioTransport {
    inner: Arc<Inner>,
}

impl StdioTransport {
    /// `new StdioTransport(options)`：冻结选项（克隆 `args`/`env`）。
    ///
    /// 构造不 spawn 子进程；`start()` 才创建它。因此可以在 `start()` 之前注册监听器。
    pub fn new(options: StdioTransportOptions) -> Self {
        let (child_closed, _) = watch::channel(false);
        StdioTransport {
            inner: Arc::new(Inner {
                options: options.frozen(),
                events: TransportEvents::new(),
                state: Mutex::new(State {
                    started: false,
                    closed: false,
                    pid: None,
                    child_alive: false,
                    close_pending: false,
                    stdin: None,
                    stdout_buffer: Vec::new(),
                    stderr_buffer: VecDeque::new(),
                }),
                child_closed,
            }),
        }
    }

    /// `readonly options`（冻结后的副本）。
    ///
    /// 这里拿不到可变引用；要换配置只能新建一个 transport。
    pub fn options(&self) -> &StdioTransportOptions {
        &self.inner.options
    }

    /// `get pid()`：子进程 pid；从未启动、spawn 失败、或子进程 `close` 事件处理之后为 `None`。
    ///
    /// 子进程已退出（`wait()` 已返回）但 stdio 尚未全部读到 EOF 时，`pid` 仍是旧值，直到
    /// `handle_child_close` 清空它。
    pub fn pid(&self) -> Option<u32> {
        self.inner.lock().pid
    }

    /// `get stderr()`：stderr 尾部缓冲的 UTF-8 解码（最后 `maxStderrBytes` 字节）。
    ///
    /// 解码是 lossy 的：截断点落在多字节字符中间时，相关字节变成 U+FFFD。`stderr = "inherit"`
    /// 时没有管道读取，所以永远返回空串；`max_stderr_bytes = 0` 时返回全部已读内容。
    pub fn stderr(&self) -> String {
        let mut state = self.inner.lock();
        let buffer = state.stderr_buffer.make_contiguous();
        String::from_utf8_lossy(buffer).into_owned()
    }

    /// `start()`（§15.1）。
    ///
    /// 重复调用返回 `MCP stdio transport already started`；`close()` 之后再调用时，从未启动过
    /// 的传输返回 `McpConnectionClosedError`，启动过的仍返回 already started（`started` 的检查
    /// 在 `closed` 之前）。
    ///
    /// TS 的 `start()` 在**任何 await 之前**就完成 `started = true`、`crossSpawn(...)` 与
    /// `this.child = child` 三步（stdio.ts:90-104），所以并发调用的 `close()` 只可能看到
    /// "还没有 child"（start 返回前）或"已有 child"（start 返回后）两种状态。Rust 侧把
    /// 状态检查、参数校验、spawn 与 child 发布全部放在**同一个互斥区**里（其间没有 `.await`），
    /// 复刻这一点：否则 `close()` 可能在 `started = true` 之后、`pid` 记录之前插进来，发出
    /// close 并返回，随后 spawn 出的子进程再也没有人杀（评审 finding #1）。
    async fn start_impl(&self) -> Result<(), McpTransportError> {
        let mut state = self.inner.lock();
        if state.started {
            return Err(McpTransportError::new(
                "MCP stdio transport already started",
            ));
        }
        if state.closed {
            return Err(McpError::connection_closed().into());
        }
        // `started = true` 在 spawn **之前**：spawn 失败也会留下 started。
        state.started = true;

        let options = &self.inner.options;
        // `crossSpawn` 先在 JS 层做参数校验（`TypeError` / `ERR_INVALID_ARG_VALUE`），失败时
        // 没有 child、没有 error/close 事件，`this.child` 保持 undefined。
        validate_spawn_arguments(options)?;

        let mut child = match spawn_child(options) {
            Ok(child) => child,
            Err((error, emitted)) => {
                if emitted {
                    // Node 在**异步** spawn 失败（ENOENT/EACCES/EAGAIN/EMFILE/ENFILE）时先触发
                    // `error`（start 的 promise reject），再在一个 tick 后触发 `exit`+`close`
                    // （清空 child 并 emitClose）。这里同样把 close 处理推迟到下一个任务；期间
                    // `close()` 不自己 emitClose（TS 的 `exitCode !== null` 分支只 end stdin）。
                    state.close_pending = true;
                    let inner = Arc::clone(&self.inner);
                    drop(state);
                    tokio::spawn(async move { inner.handle_child_close() });
                }
                // 同步抛出的 errno（E2BIG 等）没有 child、没有 close 事件——与 Node 的
                // `throw new ErrnoException(err, 'spawn')` 一致。
                return Err(error);
            }
        };

        let pid = child.id();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // 发布 child（pid + stdin 写入口）之后再解锁：`close()` 从这里开始就能走关闭阶梯，
        // 且 `stdin.end()` 一定作用在已发布的写入口上（TS 里 `this.child` 与 `child.stdin`
        // 是同一个同步赋值，不存在"close 先到、stdin 后装"的窗口）。
        let stdin_pipe = stdin.map(|stdin| Arc::new(StdinPipe::new(stdin)));
        state.pid = pid;
        state.child_alive = true;
        state.stdin = stdin_pipe.clone();
        drop(state);

        #[cfg(unix)]
        if USE_PROCESS_GROUPS && let Some(pid) = pid {
            register_process_group(pid);
        }

        // stdout / stderr 读取任务。
        // 读任务在 EOF 或读错误后把对应的 `watch` 置 true（流为 `None` 时立即置位）；
        // 下面的 wait 任务用它判断两个 stdio 流都已读到结尾。
        let (stdout_done_tx, stdout_done) = watch::channel(false);
        match stdout {
            Some(mut stdout) => {
                let inner = Arc::clone(&self.inner);
                tokio::spawn(async move {
                    let mut buffer = vec![0u8; 64 * 1024];
                    loop {
                        match stdout.read(&mut buffer).await {
                            Ok(0) => break,
                            Ok(read) => inner.handle_stdout(&buffer[..read]),
                            Err(error) => {
                                // TS: `child.stdout.on("error", (e) => this.emitError(e))`——
                                // **没有** `closed` 守卫（只有 stdin 的处理器有）。
                                let _ = inner
                                    .events
                                    .emit_error(&McpTransportError::new(error.to_string()));
                                break;
                            }
                        }
                    }
                    stdout_done_tx.send_replace(true);
                });
            }
            None => {
                stdout_done_tx.send_replace(true);
            }
        }

        let (stderr_done_tx, stderr_done) = watch::channel(false);
        match stderr {
            Some(mut stderr) => {
                let inner = Arc::clone(&self.inner);
                tokio::spawn(async move {
                    // 与 libuv 的 `data` chunk 上限一致（64 KiB）：`on_stderr` 的**分块边界**
                    // 因而与 Node 相同，跨块的多字节字符在相同的字节位置被 lossy 解码成 U+FFFD
                    // （Node 的 `Buffer.toString("utf8")` 同样不跨块保留半个字符，已实测）。
                    let mut buffer = vec![0u8; 64 * 1024];
                    loop {
                        match stderr.read(&mut buffer).await {
                            Ok(0) => break,
                            Ok(read) => inner.handle_stderr(&buffer[..read]),
                            Err(error) => {
                                // TS: `child.stderr.on("error", (e) => this.emitError(e))`——同样
                                // 没有 `closed` 守卫。
                                let _ = inner
                                    .events
                                    .emit_error(&McpTransportError::new(error.to_string()));
                                break;
                            }
                        }
                    }
                    stderr_done_tx.send_replace(true);
                });
            }
            None => {
                stderr_done_tx.send_replace(true);
            }
        }

        // stdin 写任务：冲刷 `submit` 排进内部缓冲的写入，`End` 时关闭写端。
        if let Some(stdin) = stdin_pipe {
            let inner = Arc::clone(&self.inner);
            tokio::spawn(run_stdin_writer(inner, stdin));
        }

        // `child.on("close", ...)`：先等 `wait()`（exit），再等 stdio 全部 EOF（close），
        // 之后才处理"不完整消息"与 `emitClose()`——与 Node 的 exit/close 次序一致。
        // 因此子进程已经退出、但孙进程仍持有 stdout 管道时，这个任务会停在 EOF 等待上，
        // `close` 事件（以及 `emitClose`）随之推迟。
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let wait_result = child.wait().await;
            inner.handle_child_exit();
            if let Err(error) = wait_result
                && !inner.is_closed()
            {
                // 关闭流程已经开始（`closed` 已置位）时不再上报 wait 的错误。
                let _ = inner
                    .events
                    .emit_error(&McpTransportError::new(error.to_string()));
            }
            let mut stdout_done = stdout_done;
            let mut stderr_done = stderr_done;
            let _ = stdout_done.wait_for(|done| *done).await;
            let _ = stderr_done.wait_for(|done| *done).await;
            inner.handle_child_close();
        });

        Ok(())
    }

    /// `send(message)`（§15.2）：载荷是 `JSON.stringify(message) + "\n"`，`signal` 被**完全**忽略。
    ///
    /// 未 `start()`、stdin 不可写（子进程已退出或已关闭）或已 `close()` 时返回
    /// `McpConnectionClosedError`；写失败返回 `` `write <CODE>` `` 并让流进入不可写状态。
    async fn send_impl(&self, message: Value) -> Result<(), McpTransportError> {
        let stdin = {
            let state = self.inner.lock();
            if !state.started || state.closed {
                return Err(McpError::connection_closed().into());
            }
            state.stdin.clone()
        };
        let Some(stdin) = stdin else {
            return Err(McpError::connection_closed().into());
        };
        let payload = format!("{}\n", kiana_json::js_object::stringify(&message)).into_bytes();
        match stdin.submit(payload) {
            // 空闲流上的 `write(2)` 一次写完全部载荷。
            StdinSubmit::Written => Ok(()),
            // 载荷进入内部缓冲时，这里要等到写任务真正写完；`signal` 不参与等待（§15.2）。
            StdinSubmit::Buffered(acknowledgement) => match acknowledgement.await {
                Ok(Ok(())) => Ok(()),
                // 写任务已经报过 error 事件并让流进入不可写状态（缓冲中的写入一起失败）。
                Ok(Err(error)) => Err(write_error(&error)),
                // 写任务没来得及回调就消失了（通道关闭）。
                Err(_) => Err(McpError::connection_closed().into()),
            },
            StdinSubmit::Closed => Err(McpError::connection_closed().into()),
            StdinSubmit::Failed(error) => {
                // 这次调用就是失败现场：Node 在这里 destroy 流、发唯一一个 error 事件，
                // 之后的 `send()` 在 `stdin?.writable` 检查处抛 `McpConnectionClosedError`。
                self.inner.report_stdin_failure(&stdin, &error);
                Err(write_error(&error))
            }
        }
    }

    /// `close()`（§15.3）：幂等；关闭阶梯见模块文档。
    ///
    /// 返回 `Ok` 不代表 `emitClose` 已经发生：子进程已退出但 stdio 未关、或 spawn 失败且
    /// close 事件在途时，本调用只结束 stdin，close 事件由后台任务稍后发出。
    async fn close_impl(&self) -> Result<(), McpTransportError> {
        let (pid, child_alive, close_pending) = {
            let mut state = self.inner.lock();
            if state.closed {
                return Ok(());
            }
            state.closed = true;
            (state.pid, state.child_alive, state.close_pending)
        };
        let Some(pid) = pid else {
            if close_pending {
                // spawn 失败的 close 事件已在路上：TS 在那里走 `exitCode !== null` 分支（只 end stdin，
                // 不 emitClose），close 事件随后由 `close` 处理器发出。
                self.inner.end_stdin();
                return Ok(());
            }
            // 没有子进程（从未启动，或 close 事件已处理）→ emitClose（每个 transport 最多一次）。
            self.inner.events.emit_close()?;
            return Ok(());
        };
        if !child_alive {
            // 子进程已退出但 close 事件还没派发：只 end stdin，**不** emitClose（§15.3）。
            self.inner.end_stdin();
            return Ok(());
        }

        let close_timeout_ms = self
            .inner
            .options
            .close_timeout_ms
            .unwrap_or(DEFAULT_CLOSE_TIMEOUT_MS);
        let grace = STDIN_CLOSE_GRACE_MS.min(close_timeout_ms);
        let start = tokio::time::Instant::now();
        // Node 的 `setTimeout` 把任何 > 2^31-1 的延迟钳到 1 ms（`TimeoutOverflowWarning`），
        // 负延迟同样按 1 ms 处理。`grace` 恒 <= 500，因此只有第二个定时器可能溢出；溢出后它的
        // 截止时间会**早于** SIGTERM 定时器——Node 的定时器按到期时间排序，所以 SIGKILL 先触发
        // 并直接结束子进程（实测 closeTimeoutMs >= 2^31 时约 5 ms 就关闭）。这里按两个截止时间的
        // 较早者等待，而不是按注册顺序把 SIGTERM 排在前面（§30.25 第 4 条）。
        let term_deadline = start + std::time::Duration::from_millis(node_timer_delay(grace));
        let kill_deadline = start
            + std::time::Duration::from_millis(node_timer_delay(
                grace.saturating_add(close_timeout_ms),
            ));

        let mut child_closed = self.inner.child_closed.subscribe();
        // 先订阅再读电平，保证"已经关闭"不会因为订阅晚于 `send_replace` 而丢失。
        if *child_closed.borrow() {
            // 读 pid 时子进程还活着，但 close 处理器可能在那之后、订阅之前已经跑完（此时
            // `pid` 是旧值）。电平为 true 时补一次进程组 SIGTERM 再返回；进程组已不存在时
            // `kill_process_tree` 忽略错误。
            kill_process_tree(pid, SIGTERM);
            return Ok(());
        }
        // 与 TS 相同的次序：先起定时器，再 `stdin.end()`。
        self.inner.end_stdin();

        let mut term_deadline = Some(term_deadline);
        let mut kill_deadline = Some(kill_deadline);
        loop {
            // 两个定时器各按自己的到期时间触发；同时到期时按注册顺序（SIGTERM 先、SIGKILL 后）。
            let next = match (term_deadline, kill_deadline) {
                (Some(term), Some(kill)) => Some(term.min(kill)),
                (Some(term), None) => Some(term),
                (None, Some(kill)) => Some(kill),
                (None, None) => None,
            };
            let Some(deadline) = next else {
                // 两个信号都发过了：等 close 事件，再补一次进程组 SIGTERM 回收孙进程。
                let _ = child_closed.wait_for(|closed| *closed).await;
                kill_process_tree(pid, SIGTERM);
                return Ok(());
            };
            tokio::select! {
                () = async {
                    let _ = child_closed.wait_for(|closed| *closed).await;
                } => {
                    // 子进程关闭后再次 SIGTERM 进程组：回收忽略 stdin EOF 的孙进程。
                    kill_process_tree(pid, SIGTERM);
                    return Ok(());
                }
                () = tokio::time::sleep_until(deadline) => {
                    if term_deadline.is_some_and(|term| term <= deadline) {
                        kill_process_tree(pid, SIGTERM);
                        term_deadline = None;
                    }
                    if kill_deadline.is_some_and(|kill| kill <= deadline) {
                        kill_process_tree(pid, SIGKILL);
                        kill_deadline = None;
                    }
                }
            }
        }
    }
}

impl std::fmt::Debug for StdioTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StdioTransport")
            .field("options", &self.inner.options)
            .field("pid", &self.pid())
            .finish()
    }
}

/// stdin 写任务：串行执行内部缓冲里的写入，`End` 时关闭写端（drop `ChildStdin` 即关闭）。
async fn run_stdin_writer(inner: Arc<Inner>, stdin: Arc<StdinPipe>) {
    loop {
        let mut slot = stdin.slot.lock().await;
        // 持着写端锁做整笔写入：`submit` 的 `try_lock` 因此能区分"流空闲"与"写任务正忙"，
        // 忙碌时新数据只能进入内部缓冲，保证管道上的字节顺序不被打乱。
        if slot.is_none() {
            break;
        }
        let Some(job) = stdin.pop() else {
            drop(slot);
            // `push` 先入队再通知；没有等待者时 `Notify` 会把通知存成许可，所以这里不会错过。
            stdin.notify.notified().await;
            continue;
        };
        match job {
            StdinJob::Write(payload, acknowledgement) => {
                let stream = slot.as_mut().expect("checked above");
                match stream.write_all(&payload).await {
                    Ok(()) => {
                        drop(slot);
                        let _ = acknowledgement.send(Ok(()));
                    }
                    Err(error) => {
                        // 管道断开：Node 会 destroy 流，缓冲中的写入全部以同一个错误失败，
                        // 流只发一个 error 事件。
                        let _ = slot.take();
                        drop(slot);
                        inner.report_stdin_failure(&stdin, &error);
                        let _ = acknowledgement.send(Err(error));
                        break;
                    }
                }
            }
            StdinJob::End => {
                let _ = slot.take();
                break;
            }
        }
    }
}

#[allow(clippy::double_must_use)]
#[async_trait]
impl McpTransport for StdioTransport {
    /// 启动子进程并开始读写；重复调用报 already started，spawn/参数校验失败的错误分类见模块文档。
    async fn start(&self) -> Result<(), McpTransportError> {
        self.start_impl().await
    }

    /// 把序列化后的载荷（含换行）写入 stdin；`options.signal` 被忽略，关闭后报连接已关闭。
    async fn send(
        &self,
        message: Value,
        _options: McpTransportSendOptions,
    ) -> Result<(), McpTransportError> {
        // `signal` 被完全忽略（§15.2 / §30.6）。
        self.send_impl(message).await
    }

    /// 幂等关闭；返回 `Ok` 不代表 close 事件已经派发（见 `close_impl` 的注释）。
    async fn close(&self) -> Result<(), McpTransportError> {
        self.close_impl().await
    }

    fn on_message(&self, listener: McpTransportMessageListener) -> Subscription {
        self.inner
            .events
            .on_message(move |message| listener(message))
    }

    fn on_error(&self, listener: McpTransportErrorListener) -> Subscription {
        self.inner.events.on_error(move |error| listener(error))
    }

    fn on_close(&self, listener: McpTransportCloseListener) -> Subscription {
        self.inner.events.on_close(move || listener())
    }
}

#[cfg(test)]
mod env_policy_tests {
    use super::*;

    fn parent() -> Vec<(String, String)> {
        [
            ("PATH", "/usr/bin"),
            ("HOME", "/home/me"),
            ("SHELL", "/bin/bash"),
            ("LANG", "C.UTF-8"),
            ("LC_ALL", "C.UTF-8"),
            ("TMPDIR", "/tmp"),
            ("HTTPS_PROXY", "http://proxy:3128"),
            ("http_proxy", "http://proxy:3128"),
            ("OPENAI_API_KEY", "sk-openai"),
            ("ANTHROPIC_API_KEY", "sk-anthropic"),
            ("GITHUB_TOKEN", "ghp_x"),
            ("DB_PASSWORD", "pw"),
            ("AWS_CREDENTIALS", "cred"),
            ("RANDOM_VAR", "nope"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
    }

    #[test]
    fn default_policy_keeps_whitelisted_and_learns_lc_vars() {
        let policy = StdioEnvPolicy::default();
        let env = sanitized_env(parent(), &IndexMap::new(), &policy);
        for key in [
            "PATH",
            "HOME",
            "SHELL",
            "LANG",
            "LC_ALL",
            "TMPDIR",
            "HTTPS_PROXY",
            "http_proxy",
        ] {
            assert!(env.contains_key(key), "{key} should be inherited");
        }
        assert!(!env.contains_key("RANDOM_VAR"));
    }

    #[test]
    fn default_policy_strips_credentials_even_if_pattern_like() {
        let policy = StdioEnvPolicy::default();
        let env = sanitized_env(parent(), &IndexMap::new(), &policy);
        for key in [
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "GITHUB_TOKEN",
            "DB_PASSWORD",
            "AWS_CREDENTIALS",
        ] {
            assert!(!env.contains_key(key), "{key} must be stripped");
        }
        assert!(is_sensitive_env_name("some_api_key"));
        assert!(is_sensitive_env_name("X_CREDENTIAL_FILE"));
        assert!(!is_sensitive_env_name("KEYBOARD_LAYOUT"));
    }

    #[test]
    fn explicit_env_entries_override_the_strip_list() {
        let policy = StdioEnvPolicy::default();
        let mut explicit = IndexMap::new();
        explicit.insert("OPENAI_API_KEY".to_string(), "configured".to_string());
        let env = sanitized_env(parent(), &explicit, &policy);
        assert_eq!(
            env.get("OPENAI_API_KEY").map(String::as_str),
            Some("configured")
        );
        // explicit 条目不会重复（sanitized_env 先跳过同名父变量再统一插入）。
        assert_eq!(
            env.iter()
                .filter(|(key, _)| key.as_str() == "OPENAI_API_KEY")
                .count(),
            1
        );
    }

    #[test]
    fn extra_allow_is_exact_match_only() {
        let policy = StdioEnvPolicy {
            extra_allow: vec!["RANDOM_VAR".to_string()],
        };
        let env = sanitized_env(parent(), &IndexMap::new(), &policy);
        assert_eq!(env.get("RANDOM_VAR").map(String::as_str), Some("nope"));
        assert!(!env.contains_key("PATH_EXTRA"));
    }

    #[test]
    fn stdio_default_line_limit_is_eight_mib() {
        assert_eq!(DEFAULT_STDIO_MAX_LINE_BYTES, 8 * 1024 * 1024);
        let options = StdioTransportOptions::new("x");
        assert_eq!(options.max_message_bytes, None);
    }
}
