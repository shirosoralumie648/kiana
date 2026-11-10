//! env/node.ts。
//!
//! 本地文件系统与 shell 的 `ExecutionEnv` 实现（TS: `NodeExecutionEnv`）。
//!
//! # 与 TS 的对应
//!
//! - `node:fs/promises` → `tokio::fs`；元数据用 `symlink_metadata`（`lstat`，不跟随符号链接）。
//! - `node:path` 的 posix/win32 语义来自 [`crate::env::path`]；用户可见的路径字符串一律用
//!   该模块生成，不用 `std::path`。
//! - `AbortSignal` → [`kiana_chord::context::AbortSignal`]；TS 在每个方法入口与每次 `await`
//!   之后检查 `signal.aborted`，这里保持同样的检查点。
//! - `child_process.spawn` → `tokio::process`；POSIX 上 `detached: true` → `process_group(0)`，
//!   杀进程树用 `kill(-pid, SIGKILL)` 再回退 `kill(pid, SIGKILL)`。
//! - spill 的 `createWriteStream(..., { flags: "a", highWaterMark: 1 MiB })` → 追加写句柄 +
//!   逐块 `write_all`；写入即背压（await 期间不读管道，只让写端等待）。
//! - `mtimeMs` 是浮点毫秒（`f64`），保留小数。
//!
//! `fs.watch` 的语义在 [`super::node_watch`]。
//!
//! # 这个文件负责什么
//!
//! [`NodeExecutionEnv`] 是面向本地磁盘的 `ExecutionEnv` 实现：把“读/写/删除/监视文件”
//! 和“通过 bash 执行命令”两套能力拼在一起。本地 JSONL 存储的
//! `kiana_durable::storage::jsonl::node::open_node_jsonl_storage` 每次调用都用它包住
//! `process.cwd()`；示例代码也直接构造它。
//!
//! 这里不是一层轻量适配：路径拼接、错误消息文本、退出码都刻意复刻 Node/libuv 的输出，
//! `crate::env` 的 trait 文档只描述语义，**字符串级别的契约**由本文件的实现与
//! `tests/env_node.rs` 的断言共同锁定。改动消息格式会直接破坏这些测试。
//!
//! # 文件结构（建议阅读顺序）
//!
//! 1. 构造选项 [`NodeEnvOptions`]；
//! 2. 路径解析与错误构造辅助（`resolve_path`、`node_io_message`、`to_file_error_*`）；
//! 3. shell 探测、环境变量合并与进程树终止（`get_shell_config`、`get_shell_env`、
//!    `kill_process_tree`）；
//! 4. 三个读取器：`NodeTextLineReader`、`NodeBinaryReader`、`NodeDirReader`；
//! 5. spill 状态 `SpillState` 与最复杂的 `exec_impl`；
//! 6. `FileSystem` / `Shell` trait 实现（本文件的主要公开面）；
//! 7. 单元测试 `node_tests`。
//!
//! # `exec` 的一次完整执行
//!
//! 1. 入口先检查 `Context::abort_signal()` 是否已中止，再把以**秒**为单位的超时校验并
//!    换算成毫秒：非有限值、`<= 0`、超过 `MAX_TIMEOUT_MS` 都以 `timeout` 错误返回。
//! 2. `ShellCommand::Shell` 交给 shell：给出显式 `shellPath` 时它必须存在，否则直接
//!    报 `shell_unavailable`、不回退；未给出时才按平台探测（Windows：Git Bash 的两个
//!    候选 → `where bash.exe`；其他：`/bin/bash` → `which bash` → `sh`）。旧版 WSL 的
//!    `System32\bash.exe` 用 `-s` 并把脚本写到 stdin，其余 bash 用 `-c <script>`。
//!    `ShellCommand::Argv` 直接把第一个元素当程序，不经过 shell 解析。
//! 3. 解析工作目录、合并环境变量后 spawn：stdout/stderr 永远是管道；stdin 只在需要
//!    送脚本时打开。POSIX 用 `process_group(0)` 建立独立进程组，方便整组 `SIGKILL`。
//! 4. 后台任务与主循环并行：可选的 stdin 写入任务、等待退出状态的 `wait()` 任务、
//!    每个输出流一个读取任务。主循环用 `select!` 同时处理取消、退出、输出块、
//!    超时与排空截止时间。
//! 5. 每个输出块先经各自的 `StreamDecoder` 解码（跨块的多字节 UTF-8 不会被截断），
//!    再按到达顺序回调 `onOutput`；若给了 `spill`，同样的**原始字节**按阈值决定是否
//!    写入临时文件。
//! 6. 进程退出后最多再等 `EXIT_STDIO_GRACE_MS`（100 ms；期间每收到一块输出就重新
//!    计时）让管道排空。两个输出流都结束，或排空期限到达（此时 abort 读取任务），
//!    循环才结束。
//! 7. 收尾顺序固定：abort 读取任务 → flush spill → flush 两个解码器的尾巴并回调 →
//!    从活跃 pid 集合移除 → 依次判定回调错误、超时、中止、spill 错误，最后返回
//!    `ShellExecResult`。
//!
//! # 错误与优先级
//!
//! `exec` 返回 `Result<ShellExecResult, ExecutionError>`；非零退出码**不是**错误，
//! 而是 `exit_code`（信号终止按 `128 + signal` 折算）。同一个命令可能同时遇到多种
//! 失败，收尾时的优先级是：`callback_error` > `timeout` > `aborted` > spill 写入
//! 错误。spill 的创建失败与 `write_all` 失败会杀掉进程树；这些错误统一成 `unknown`，消息以
//! `Failed to preserve complete shell output:` 开头，避免“要求保留完整输出却静默
//! 丢失”。
//!
//! # 并发与生命周期
//!
//! - `NodeExecutionEnv` 的方法都取 `&self`，同一个环境可以被多个任务共享。可变状态
//!   只有两把 `std::sync::Mutex`（`cwd_override`、`active_child_pids`），持锁范围都
//!   只是几次集合操作，没有跨 `await`；锁中毒被 `lock` 辅助函数忽略并继续使用。
//! - 三个读取器各自用 `tokio::sync::Mutex` 包住内部状态，`read_line`/`read`/`next`
//!   在整次操作期间持锁，所以对同一读取器的并发调用是串行的；`close` 也取同一把锁。
//! - `exec` 的 `onOutput` 回调在主循环里同步执行。回调变慢会让读取任务阻塞在有界
//!   通道（容量 8）的 `send` 上，进而让子进程的管道写端承受背压。回调 panic 被
//!   `catch_unwind` 捕获（payload 转成 `CallbackError` 消息），随后终止进程树。
//! - `cleanup` 只对 `active_child_pids` 的当前快照发送终止信号并清空集合，不等待
//!   进程退出；被它杀死的命令仍走正常退出码路径结束，见
//!   `tests/env_node.rs` 的 `cleanup_terminates_active_shell_processes`。
//!
//! # 容易踩的坑
//!
//! - `FileSystem::cwd` 返回 `&str`，所以 `set_cwd` 把每次传入的字符串 `Box::leak`
//!   掉：内存不会回收，调用次数应保持很少。
//! - `resolve_path` 只在 `file://` 能按 Node 规则解析时才转换；非法 URL（编码的
//!   `/`、非法百分号转义、非法 UTF-8、非 localhost 宿主）保留原串并当作相对路径，
//!   见 `keeps_file_urls_that_node_rejects_as_relative_paths`。
//! - 非 Unix 平台没有 errno 表，`node_errno` 返回 `None`，错误消息退化为
//!   `io::Error` 的 Display；依赖 Node 文案的断言只在 Unix 上成立。
//! - Windows 的 `no_follow` 是“先 `lstat` 再打开”，两步之间存在竞态窗口；Unix 用
//!   `O_NOFOLLOW` 原子拒绝最后一段是符号链接的路径。

use std::collections::HashSet;
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, UNIX_EPOCH};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_chord::context::{AbortSignal, Context};
use kiana_json::format_js_number;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::env::decode::StreamDecoder;
use crate::env::line_scan::LineScanner;
use crate::env::node_watch::NodeFileWatcher;
pub use crate::env::node_watch::NodeWatchOptions; // TS: `export type { NodeWatchOptions }`
use crate::env::path::{posix, win32};
use crate::env::{
    BinaryReader, CreateDirOptions, CreateTempFileOptions, DirPage, DirReader, ExecutionError,
    ExecutionErrorCode, FileContent, FileError, FileErrorCode, FileInfo, FileKind, FileSystem,
    FileWatcher, LineRange, LineScan, OpenBinaryReaderOptions, ReadTextLinesOptions, RemoveOptions,
    Shell, ShellCommand, ShellExecOptions, ShellExecResult, ShellOutputInfo, ShellOutputStream,
    TextLine, TextLineReader, WatchCallback, WatchTarget,
};

const MAX_TIMEOUT_MS: f64 = 2_147_483_647.0;
const MAX_TIMEOUT_SECONDS: f64 = MAX_TIMEOUT_MS / 1000.0;
/// 子进程退出后再等多久让 stdout/stderr 排空（TS 的 `EXIT_STDIO_GRACE_MS`）。
///
/// 主循环的结束条件要求退出状态已到**且**两个输出流都已 EOF，因此收到退出状态后
/// 仍会继续收数据；这里给这段排空窗口一个上限：到期仍未 EOF 就 abort 读取任务、
/// 放弃剩余输出，避免管道迟迟不关闭时 `exec` 永远等下去。
const EXIT_STDIO_GRACE_MS: u64 = 100;
/// 单个 `BinaryReader.read` 的最大块，避免巨大的 `length` 一次分配过多。
const BINARY_READ_CHUNK: usize = 1024 * 1024;
/// `TextLineReader` / `scanLines` 的读取块（源码里是局部 64 KiB，**不是** 1 MiB）。
const TEXT_CHUNK: usize = 64 * 1024;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
/// Node 的 `windowsHide: true`（libuv `UV_PROCESS_WINDOWS_HIDE_CONSOLE` → `CREATE_NO_WINDOW`）。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Node 的 `detached: true`（libuv `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`）。
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// 构造选项
// ---------------------------------------------------------------------------

/// `new NodeExecutionEnv(options)` 的选项。
///
/// 所有字段都可省略（`Default`）；`From<String>` / `From<&str>` 等价于
/// [`NodeEnvOptions::new`]。`cwd` 可以是相对路径——`resolve_path` 会以
/// `process.cwd()` 为基准解析，而不是把相对 `cwd` 再前置一次。
///
/// ```ignore
/// NodeExecutionEnv::new(NodeEnvOptions { cwd, ..Default::default() })
/// NodeExecutionEnv::new(NodeEnvOptions::new(cwd))
/// NodeExecutionEnv::new(cwd)
/// ```
#[derive(Debug, Clone, Default)]
pub struct NodeEnvOptions {
    /// 当前工作目录。
    pub cwd: String,
    /// 显式的 bash 路径。
    pub shell_path: Option<String>,
    /// 构造时固定的环境变量（在 `process.env` 之上、每次调用的 `options.env` 之下）。
    ///
    /// 这一层只在继承被打开（`inheritEnv != Some(false)`）时生效；调用方显式关闭继承
    /// 时，它与 `process.env` 一起被丢弃，只剩每次调用给的 `env`。
    pub shell_env: Option<IndexMap<String, String>>,
    /// 文件监视选项。
    pub watch: Option<NodeWatchOptions>,
}

impl NodeEnvOptions {
    /// 只给出 cwd 的选项。
    pub fn new(cwd: impl Into<String>) -> Self {
        NodeEnvOptions {
            cwd: cwd.into(),
            ..Default::default()
        }
    }
}

impl From<String> for NodeEnvOptions {
    fn from(cwd: String) -> Self {
        NodeEnvOptions::new(cwd)
    }
}

impl From<&str> for NodeEnvOptions {
    fn from(cwd: &str) -> Self {
        NodeEnvOptions::new(cwd)
    }
}

// ---------------------------------------------------------------------------
// 路径辅助
// ---------------------------------------------------------------------------

fn platform_is_absolute(path: &str) -> bool {
    if cfg!(windows) {
        win32::is_absolute(path)
    } else {
        posix::is_absolute(path)
    }
}

fn platform_join(parts: &[&str]) -> String {
    if cfg!(windows) {
        win32::join(parts)
    } else {
        posix::join(parts)
    }
}

fn platform_resolve(parts: &[&str], cwd: &str) -> String {
    if cfg!(windows) {
        win32::resolve_string_with_cwd(parts, cwd, cfg!(windows))
    } else {
        posix::resolve_string_with_cwd(parts, cwd)
    }
}

fn platform_basename(path: &str) -> String {
    if cfg!(windows) {
        win32::basename(path, None)
    } else {
        posix::basename(path, None)
    }
}

fn platform_dirname(path: &str) -> String {
    if cfg!(windows) {
        win32::dirname(path)
    } else {
        posix::dirname(path)
    }
}

/// `os.homedir()`。
fn home_dir() -> String {
    match dirs::home_dir() {
        Some(path) => path.to_string_lossy().into_owned(),
        None => String::new(),
    }
}

/// `os.tmpdir()`（libuv `uv_os_tmpdir`：空值被跳过）。
fn temp_dir() -> String {
    temp_dir_from_env(|name| {
        std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
    })
}

/// `uv_os_tmpdir` 的变量顺序：POSIX 是 `TMPDIR` → `TMP` → `TEMP` → `/tmp`；
/// Windows 是 `TMP` → `TEMP` → `USERPROFILE` → `%SystemRoot%\Temp`。
///
/// 空字符串与未设置等价（libuv 会跳过），因此 `TMPDIR=""` 不会得到空路径；Windows
/// 连 `SystemRoot` 都没有时使用字面量 `C:\Windows\Temp`。`get` 由调用方注入，单测据此
/// 不修改进程环境。
fn temp_dir_from_env(get: impl Fn(&str) -> Option<String>) -> String {
    let lookup = |name: &str| get(name).filter(|value| !value.is_empty());
    #[cfg(windows)]
    {
        for name in ["TMP", "TEMP", "USERPROFILE"] {
            if let Some(value) = lookup(name) {
                return value;
            }
        }
        if let Some(root) = lookup("SystemRoot") {
            return platform_join(&[&root, "Temp"]);
        }
        "C:\\Windows\\Temp".to_owned()
    }
    #[cfg(not(windows))]
    {
        for name in ["TMPDIR", "TMP", "TEMP"] {
            if let Some(value) = lookup(name) {
                return value;
            }
        }
        "/tmp".to_owned()
    }
}

/// `fileURLToPath(url)`；不是合法的 `file:` URL 时返回 `None`（保留原字符串）。
///
/// Node 在 POSIX 上返回 `decodeURIComponent(url.pathname)`：编码的 `/`（`%2f`）抛
/// `ERR_INVALID_FILE_URL_PATH`，非法百分号转义或非法 UTF-8 抛 `URIError`，宿主不是空串或
/// `localhost` 时抛 `ERR_INVALID_FILE_URL_HOST`。`url::Url::to_file_path` 会静默解码 `%2F`
/// 并用替换字符处理非法 UTF-8，因此先按 Node 的规则校验 pathname。
fn file_url_to_path(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    node_file_url_pathname(parsed.path())?;
    parsed
        .to_file_path()
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

/// `getPathFromURLPosix` / `getPathFromURLWin32` 对 pathname 的校验与解码；失败返回 `None`。
fn node_file_url_pathname(pathname: &str) -> Option<String> {
    let bytes = pathname.as_bytes();
    let mut decoded: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        // `decodeURIComponent` 对截断或非十六进制的转义抛 `URIError`。
        let high = hex_digit(*bytes.get(index + 1)?)?;
        let low = hex_digit(*bytes.get(index + 2)?)?;
        let byte = (high << 4) | low;
        // 编码的路径分隔符：POSIX 拒绝 `%2f`；Windows 还拒绝 `%5c`。
        if byte == b'/' || (cfg!(windows) && byte == b'\\') {
            return None;
        }
        decoded.push(byte);
        index += 3;
    }
    String::from_utf8(decoded).ok()
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// `process.cwd()`：`path.resolve` 用它作为相对路径的起点。
fn process_cwd() -> String {
    match std::env::current_dir() {
        Ok(path) => path.to_string_lossy().into_owned(),
        Err(_) => String::new(),
    }
}

/// `resolvePath(cwd, path)`：`~`、`file://` 展开，然后按平台解析为绝对路径。
///
/// 展开是有条件的，且按固定顺序：
/// 1. 恰好 `~` 或 `~/`（Windows 上还有 `~\`）开头才替换为 home；路径中间的 `~` 原样保留。
/// 2. 只有 `file://` **且** `file_url_to_path` 接受时才转换；被 Node 拒绝的 URL
///    （编码的 `/`、非法转义、非法 UTF-8、非 localhost 宿主）保留原字符串，随后按
///    相对路径解析——这正是 `keeps_file_urls_that_node_rejects_as_relative_paths`
///    锁定的行为。
/// 3. 相对 `cwd` 本身也以 `process.cwd()` 为起点解析，而不会把相对 `cwd` 再前置一次；
///    这就是 `resolves_a_relative_cwd_against_the_process_cwd` 的回归点。
pub(crate) fn resolve_path(cwd: &str, path: &str) -> String {
    let mut normalized = path.to_owned();
    if normalized == "~" {
        normalized = home_dir();
    } else if normalized.starts_with("~/") || (cfg!(windows) && normalized.starts_with("~\\")) {
        let rest = normalized[2..].to_owned();
        let home = home_dir();
        normalized = platform_join(&[&home, &rest]);
    } else if normalized.starts_with("file://")
        && let Some(converted) = file_url_to_path(&normalized)
    {
        normalized = converted;
    }
    // TS 的 `resolve(cwd, normalized)` 在 `cwd` 相对时也以 `process.cwd()` 为起点，而不是把
    // 相对 `cwd` 再前置一次。
    let base = process_cwd();
    if platform_is_absolute(&normalized) {
        platform_resolve(&[normalized.as_str()], &base)
    } else {
        platform_resolve(&[cwd, normalized.as_str()], &base)
    }
}

// ---------------------------------------------------------------------------
// 错误辅助
// ---------------------------------------------------------------------------

/// libuv 的 errno → `(code, description)`。Node 的 fs 错误消息是
/// `"<CODE>: <description>, <syscall> '<path>'"`（双路径 syscall 是
/// `'<path>' -> '<dest>'`；已打开句柄上的 syscall 没有路径）。描述逐字来自 libuv
/// 的 `uv_strerror` 表。非 Unix 平台的 `raw_os_error` 是 Windows 错误码，不做映射。
#[cfg(unix)]
fn node_errno(error: &std::io::Error) -> Option<(&'static str, &'static str)> {
    let code = error.raw_os_error()?;
    Some(match code {
        libc::EPERM => ("EPERM", "operation not permitted"),
        libc::ENOENT => ("ENOENT", "no such file or directory"),
        libc::ESRCH => ("ESRCH", "no such process"),
        libc::EINTR => ("EINTR", "interrupted system call"),
        libc::EIO => ("EIO", "i/o error"),
        libc::ENXIO => ("ENXIO", "no such device or address"),
        libc::EBADF => ("EBADF", "bad file descriptor"),
        libc::EAGAIN => ("EAGAIN", "resource temporarily unavailable"),
        libc::ENOMEM => ("ENOMEM", "not enough memory"),
        libc::EACCES => ("EACCES", "permission denied"),
        libc::EFAULT => ("EFAULT", "bad address in system call argument"),
        libc::EBUSY => ("EBUSY", "resource busy or locked"),
        libc::EEXIST => ("EEXIST", "file already exists"),
        libc::EXDEV => ("EXDEV", "cross-device link not permitted"),
        libc::ENODEV => ("ENODEV", "no such device"),
        libc::ENOTDIR => ("ENOTDIR", "not a directory"),
        libc::EISDIR => ("EISDIR", "illegal operation on a directory"),
        libc::EINVAL => ("EINVAL", "invalid argument"),
        libc::ENFILE => ("ENFILE", "file table overflow"),
        libc::EMFILE => ("EMFILE", "too many open files"),
        libc::ETXTBSY => ("ETXTBSY", "text file is busy"),
        libc::EFBIG => ("EFBIG", "file too large"),
        libc::ENOSPC => ("ENOSPC", "no space left on device"),
        libc::ESPIPE => ("ESPIPE", "invalid seek"),
        libc::EROFS => ("EROFS", "read-only file system"),
        libc::EMLINK => ("EMLINK", "too many links"),
        libc::EPIPE => ("EPIPE", "broken pipe"),
        libc::ENAMETOOLONG => ("ENAMETOOLONG", "name too long"),
        libc::ENOSYS => ("ENOSYS", "function not implemented"),
        libc::ENOTEMPTY => ("ENOTEMPTY", "directory not empty"),
        libc::ELOOP => ("ELOOP", "too many symbolic links encountered"),
        libc::EOVERFLOW => ("EOVERFLOW", "value too large for defined data type"),
        libc::EILSEQ => ("EILSEQ", "illegal byte sequence"),
        libc::ENOTSUP => ("ENOTSUP", "operation not supported on socket"),
        libc::ECONNABORTED => ("ECONNABORTED", "software caused connection abort"),
        libc::ECONNRESET => ("ECONNRESET", "connection reset by peer"),
        libc::ENOBUFS => ("ENOBUFS", "no buffer space available"),
        libc::EISCONN => ("EISCONN", "socket is already connected"),
        libc::ENOTCONN => ("ENOTCONN", "socket is not connected"),
        libc::ESHUTDOWN => ("ESHUTDOWN", "cannot send after transport endpoint shutdown"),
        libc::ETIMEDOUT => ("ETIMEDOUT", "connection timed out"),
        libc::ECONNREFUSED => ("ECONNREFUSED", "connection refused"),
        libc::EHOSTDOWN => ("EHOSTDOWN", "host is down"),
        libc::EHOSTUNREACH => ("EHOSTUNREACH", "host is unreachable"),
        _ => return None,
    })
}

#[cfg(not(unix))]
fn node_errno(_error: &std::io::Error) -> Option<(&'static str, &'static str)> {
    None
}

/// `toFileError` 的错误码映射：只识别 ENOENT/EACCES/EPERM/ENOTDIR/EISDIR/EINVAL，
/// 其余 errno（包括 ENOSYS、ENOTSUP、ELOOP）都落到 `unknown`。
#[cfg(unix)]
fn file_error_code_for(error: &std::io::Error) -> FileErrorCode {
    match error.raw_os_error() {
        Some(code) if code == libc::ENOENT => FileErrorCode::NotFound,
        Some(code) if code == libc::EACCES || code == libc::EPERM => {
            FileErrorCode::PermissionDenied
        }
        Some(code) if code == libc::ENOTDIR => FileErrorCode::NotDirectory,
        Some(code) if code == libc::EISDIR => FileErrorCode::IsDirectory,
        Some(code) if code == libc::EINVAL => FileErrorCode::Invalid,
        _ => file_error_code_for_kind(error),
    }
}

#[cfg(not(unix))]
fn file_error_code_for(error: &std::io::Error) -> FileErrorCode {
    file_error_code_for_kind(error)
}

fn file_error_code_for_kind(error: &std::io::Error) -> FileErrorCode {
    match error.kind() {
        std::io::ErrorKind::NotFound => FileErrorCode::NotFound,
        std::io::ErrorKind::PermissionDenied => FileErrorCode::PermissionDenied,
        std::io::ErrorKind::NotADirectory => FileErrorCode::NotDirectory,
        std::io::ErrorKind::IsADirectory => FileErrorCode::IsDirectory,
        std::io::ErrorKind::InvalidInput => FileErrorCode::Invalid,
        _ => FileErrorCode::Unknown,
    }
}

/// Node 的 fs 错误消息（`<CODE>: <description>, <syscall> '<path>'`）。
///
/// 未知 errno 回退到 `io::Error` 的 Display（Node 对 libuv 未知的 errno 用
/// `"Unknown system error -N"`；两者都只出现在极端的 I/O 失败下）。
pub(crate) fn node_io_message(
    error: &std::io::Error,
    syscall: &str,
    path: Option<&str>,
    dest: Option<&str>,
) -> String {
    let Some((code, description)) = node_errno(error) else {
        return error.to_string();
    };
    match (path, dest) {
        (Some(path), Some(dest)) => {
            format!("{code}: {description}, {syscall} '{path}' -> '{dest}'")
        }
        (Some(path), None) => format!("{code}: {description}, {syscall} '{path}'"),
        (None, _) => format!("{code}: {description}, {syscall}"),
    }
}

/// 把 `io::Error` 复制成可克隆的 `cause`：只保留 `ErrorKind` 与 Display 文本，
/// `raw_os_error()` 不会保留（错误消息已由调用方按 Node 格式生成）。
fn io_error_cause(error: &std::io::Error) -> Arc<dyn std::error::Error + Send + Sync> {
    Arc::new(std::io::Error::new(error.kind(), error.to_string()))
}

/// `child_process.spawn` 失败时的消息。
///
/// Node 只把 EACCES/EAGAIN/EMFILE/ENFILE/ENOENT 作为异步 `error` 事件发出，消息是
/// `spawn <file> <CODE>`；其余错误（例如 cwd 不是目录时的 `ENOTDIR`）在 `spawn` 调用里同步
/// 抛出 `new ErrnoException(err, 'spawn')`，消息是 `spawn <CODE>`，**没有文件名**。
fn spawn_error_message(program: &str, error: &std::io::Error) -> String {
    let Some((code, _)) = node_errno(error) else {
        return error.to_string();
    };
    #[cfg(unix)]
    let async_error = matches!(
        error.raw_os_error(),
        Some(raw) if raw == libc::EACCES
            || raw == libc::EAGAIN
            || raw == libc::EMFILE
            || raw == libc::ENFILE
            || raw == libc::ENOENT
    );
    #[cfg(not(unix))]
    let async_error = matches!(
        error.kind(),
        std::io::ErrorKind::NotFound
            | std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::WouldBlock
    );
    if async_error {
        format!("spawn {program} {code}")
    } else {
        format!("spawn {code}")
    }
}

/// `toFileError(error, fallbackPath)`：消息带 syscall 与路径，`path` 字段取该路径。
pub(crate) fn to_file_error_io(
    error: &std::io::Error,
    syscall: &str,
    path: Option<&str>,
) -> FileError {
    file_error_from_io(error, syscall, path, None, path)
}

/// 双路径 syscall（`rename`）：消息里两个路径都出现，`path` 字段是源路径（Node 的
/// `nodeError.path`）。
pub(crate) fn to_file_error_io_between(
    error: &std::io::Error,
    syscall: &str,
    source: &str,
    destination: &str,
) -> FileError {
    file_error_from_io(
        error,
        syscall,
        Some(source),
        Some(destination),
        Some(source),
    )
}

/// 已打开句柄上的 syscall（`read`/`write`/`fstat`/`fsync`/`ftruncate`）：Node 的错误消息
/// 没有路径，`toFileError` 用调用方给的 fallback 路径填 `path`。
pub(crate) fn to_file_error_io_fd(
    error: &std::io::Error,
    syscall: &str,
    fallback_path: Option<&str>,
) -> FileError {
    file_error_from_io(error, syscall, None, None, fallback_path)
}

fn file_error_from_io(
    error: &std::io::Error,
    syscall: &str,
    message_path: Option<&str>,
    dest: Option<&str>,
    path: Option<&str>,
) -> FileError {
    let mut file_error = FileError::new(
        file_error_code_for(error),
        node_io_message(error, syscall, message_path, dest),
    )
    .with_cause(io_error_cause(error));
    if let Some(path) = path {
        file_error = file_error.with_path(path);
    }
    file_error
}

fn aborted_error(path: Option<&str>) -> FileError {
    let file_error = FileError::new(FileErrorCode::Aborted, "aborted");
    match path {
        Some(path) => file_error.with_path(path),
        None => file_error,
    }
}

/// `abortResult(signal, path)`：已中止 → `Err(FileError("aborted", "aborted", path))`。
fn abort_result<T>(
    signal: &Option<AbortSignal>,
    path: Option<&str>,
) -> Option<Result<T, FileError>> {
    if signal.as_ref().is_some_and(AbortSignal::is_aborted) {
        Some(Err(aborted_error(path)))
    } else {
        None
    }
}

fn closed_result<T>(what: &str, path: &str) -> Result<T, FileError> {
    Err(FileError::new(FileErrorCode::Invalid, format!("{what} is closed")).with_path(path))
}

/// `fs.readFile`/`fs.writeFile` 的 `signal`：进行中的 I/O 被取消时，Node 的 `readFile`/
/// `writeFile` 以 `AbortError`（"The operation was aborted"）拒绝，`toFileError` 把它变成
/// `FileError("aborted", …)` 并用调用方的 fallback 路径填 `path`。
///
/// `select!` 用 `biased`，取消分支先被轮询：若取消与 I/O 完成同时就绪，返回的是
/// `aborted`，不会把刚完成的结果交出去。
async fn cancellable_io<T>(
    future: impl std::future::Future<Output = Result<T, FileError>>,
    signal: &Option<AbortSignal>,
    path: &str,
) -> Result<T, FileError> {
    match signal {
        Some(signal) => {
            tokio::select! {
                biased;
                _ = signal.token().cancelled() => Err(FileError::new(
                    FileErrorCode::Aborted,
                    "The operation was aborted",
                )
                .with_path(path)),
                result = future => result,
            }
        }
        None => future.await,
    }
}

/// `resolveTimeoutMs`：秒 → 毫秒，`timeout * 1000` 不做取整（`0.0015` → `1.5` ms）。
///
/// 非有限值、`<= 0`、超过 `MAX_TIMEOUT_MS` 三种情况都返回 `timeout` 错误码（不是
/// `unknown`）；错误文案是当前稳定契约，改动需同步测试（测试只断言其中的 "Invalid timeout"）。
fn resolve_timeout_ms(timeout: Option<f64>) -> Result<Option<f64>, ExecutionError> {
    let Some(timeout) = timeout else {
        return Ok(None);
    };
    if !timeout.is_finite() || timeout <= 0.0 {
        return Err(ExecutionError::new(
            ExecutionErrorCode::Timeout,
            "Invalid timeout: must be a finite number of seconds",
        ));
    }
    let timeout_ms = timeout * 1000.0;
    if timeout_ms > MAX_TIMEOUT_MS {
        return Err(ExecutionError::new(
            ExecutionErrorCode::Timeout,
            format!(
                "Invalid timeout: maximum is {} seconds",
                format_js_number(MAX_TIMEOUT_SECONDS)
            ),
        ));
    }
    Ok(Some(timeout_ms))
}

/// `fileKindFromStats`。
fn file_kind_from_metadata(metadata: &std::fs::Metadata) -> Option<FileKind> {
    let file_type = metadata.file_type();
    if file_type.is_file() {
        Some(FileKind::File)
    } else if file_type.is_dir() {
        Some(FileKind::Directory)
    } else if file_type.is_symlink() {
        Some(FileKind::Symlink)
    } else {
        None
    }
}

/// `stats.mtimeMs`：保留小数毫秒；时间早于 `UNIX_EPOCH` 时返回负数（与 JS Date 的值
/// 一致），`modified()` 本身取不到时返回 0。
fn mtime_millis(metadata: &std::fs::Metadata) -> f64 {
    match metadata.modified() {
        Ok(modified) => match modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => {
                duration.as_secs() as f64 * 1000.0 + duration.subsec_nanos() as f64 / 1_000_000.0
            }
            Err(error) => {
                -(error.duration().as_secs() as f64 * 1000.0
                    + error.duration().subsec_nanos() as f64 / 1_000_000.0)
            }
        },
        Err(_) => 0.0,
    }
}

/// `fileInfoFromStats`：不支持的条目类型（FIFO、socket 等）返回 `invalid`。
///
/// 调用方对它的处理不同：`NodeDirReader::next` / `list_dir` 跳过该条，而直接调用
/// `fileInfo` 的环境方法会把错误原样返回。
fn file_info_from_metadata(
    path: &str,
    metadata: &std::fs::Metadata,
) -> Result<FileInfo, FileError> {
    let Some(kind) = file_kind_from_metadata(metadata) else {
        return Err(
            FileError::new(FileErrorCode::Invalid, "Unsupported file type").with_path(path),
        );
    };
    Ok(FileInfo {
        name: platform_basename(path),
        path: path.to_owned(),
        kind,
        size: metadata.len(),
        mtime_ms: mtime_millis(metadata),
    })
}

fn symlink_refused(
    path: &str,
    cause: Option<Arc<dyn std::error::Error + Send + Sync>>,
) -> FileError {
    let error = FileError::new(FileErrorCode::Invalid, "Refusing to follow a symbolic link")
        .with_path(path);
    match cause {
        Some(cause) => error.with_cause(cause),
        None => error,
    }
}

/// 路径存在性检查；`metadata` 会跟随符号链接，因此断链返回 `false`。
async fn path_exists(path: &str) -> bool {
    tokio::fs::metadata(path).await.is_ok()
}

// ---------------------------------------------------------------------------
// shell 配置
// ---------------------------------------------------------------------------

/// 字符串命令怎么交给 shell：`Argv` 把脚本作为 `-c` 的参数；`Stdin` 用 `-s` 并把脚本
/// 写进子进程 stdin（旧版 WSL 的 `System32\bash.exe`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandTransport {
    Argv,
    Stdin,
}

/// shell 的可执行路径、固定参数（`-c` 或 `-s`）与脚本传输方式。
#[derive(Debug, Clone)]
struct ShellConfig {
    shell: String,
    args: Vec<String>,
    transport: CommandTransport,
}

/// `/^[a-z]:[\\/]/i`：判断字符串是否以 `C:\` / `D:/` 这样的盘符前缀开头。
///
/// 这里用于非空 `SystemRoot` 的合法性检查：`is_legacy_wsl_bash_path_with` 先用
/// `filter(|value| !value.is_empty())` 滤掉空串，只有非空值才走到这里；非空且不是盘符
/// 前缀时直接返回 `false`（与 spec §2.23 记录的 TS 行为一致），不会退回
/// `legacy_wsl_fallback` 的正则匹配。空串按未设置处理，仍走 `legacy_wsl_fallback`。
fn is_windows_drive_root(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// `isLegacyWslBashPath(path)`（`SystemRoot` 由调用方给出，便于单测）。
///
/// `SystemRoot` 为空串时按 TS 的 `process.env.SystemRoot` 假值处理：走常规路径的回退匹配。
fn is_legacy_wsl_bash_path_with(path: &str, system_root: Option<&str>) -> bool {
    let normalized = win32::normalize(path).to_lowercase();
    if let Some(system_root) = system_root.filter(|value| !value.is_empty()) {
        if !is_windows_drive_root(system_root) {
            return false;
        }
        return ["System32", "Sysnative"].iter().any(|directory| {
            let candidate = win32::join(&[system_root, directory, "bash.exe"]).to_lowercase();
            normalized == candidate
        });
    }
    legacy_wsl_fallback(&normalized)
}

/// `/^[a-z]:\\windows\\(?:system32|sysnative)\\bash\.exe$/`。
fn legacy_wsl_fallback(normalized: &str) -> bool {
    let bytes = normalized.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return false;
    }
    let Some(rest) = normalized.get(2..) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix('\\') else {
        return false;
    };
    // `^[a-z]:\\windows\\(?:system32|sysnative)\\bash\.exe$`
    let Some(rest) = rest.strip_prefix("windows\\") else {
        return false;
    };
    for directory in ["system32", "sysnative"] {
        if let Some(tail) = rest.strip_prefix(directory)
            && let Some(tail) = tail.strip_prefix('\\')
            && tail == "bash.exe"
        {
            return true;
        }
    }
    false
}

fn is_legacy_wsl_bash_path(path: &str) -> bool {
    let system_root = std::env::var("SystemRoot").ok();
    is_legacy_wsl_bash_path_with(path, system_root.as_deref())
}

/// `getBashShellConfig(shell)`：判定传输方式并给出固定参数。
///
/// 只有 `is_legacy_wsl_bash_path` 命中的旧版 WSL bash 用 `-s` + stdin；其他路径一律
/// `-c`，脚本在 `exec` 里追加为最后一个参数。
fn get_bash_shell_config(shell: &str) -> ShellConfig {
    if is_legacy_wsl_bash_path(shell) {
        ShellConfig {
            shell: shell.to_owned(),
            args: vec!["-s".to_owned()],
            transport: CommandTransport::Stdin,
        }
    } else {
        ShellConfig {
            shell: shell.to_owned(),
            args: vec!["-c".to_owned()],
            transport: CommandTransport::Argv,
        }
    }
}

/// `runCommand(program, args, timeoutMs)`：stdout 文本与退出码；超时/启动失败时退出码为 `None`。
///
/// 只用于探测 shell 路径：stderr 丢弃，stdout 用 `from_utf8_lossy` 解码；超时后
/// `command.output()` 的 future 被丢弃，`kill_on_drop(true)` 随之杀掉这个子进程。
async fn run_command(program: &str, args: &[&str], timeout_ms: u64) -> (String, Option<i32>) {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // TS 在超时时 killProcessTree(child.pid)；单进程等价于 drop 即杀。
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // TS: `windowsHide: true`。
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let output = tokio::time::timeout(Duration::from_millis(timeout_ms), command.output()).await;
    match output {
        Ok(Ok(output)) => (
            String::from_utf8_lossy(&output.stdout).into_owned(),
            output.status.code(),
        ),
        Ok(Err(_)) => (String::new(), None),
        Err(_) => (String::new(), None),
    }
}

/// `findBashOnPath()`：`where bash.exe`（Windows）或 `which bash`（其他平台）。
///
/// 5 秒超时；命令失败或 stdout 为空返回 `None`；否则取 stdout 第一行、去掉行尾 `\r`，
/// 再用 `path_exists` 复核该路径确实存在。
async fn find_bash_on_path() -> Option<String> {
    let (stdout, status) = if cfg!(windows) {
        run_command("where", &["bash.exe"], 5000).await
    } else {
        run_command("which", &["bash"], 5000).await
    };
    if status != Some(0) || stdout.is_empty() {
        return None;
    }
    let trimmed = stdout.trim();
    let first = trimmed
        .split('\n')
        .next()
        .unwrap_or("")
        .trim_end_matches('\r');
    if !first.is_empty() && path_exists(first).await {
        Some(first.to_owned())
    } else {
        None
    }
}

/// `getShellConfig(customShellPath)`：决定字符串命令用哪个 shell。
///
/// 探测顺序固定：
/// - 显式路径：存在 → 用该路径；不存在 → `shell_unavailable`，**不再**回退
///   （空串按未配置处理，见下方注释）。
/// - Windows：`%ProgramFiles%\Git\bin\bash.exe` → `%ProgramFiles(x86)%\Git\bin\bash.exe`
///   → `where bash.exe` 的第一行；全部失败返回 `shell_unavailable`，消息里列出尝试过的
///   Git Bash 候选路径。
/// - 其他平台：`/bin/bash` → `which bash` 的第一行 → 直接使用 `sh`（成功返回，不是错误）。
///
/// 无论路径来自显式配置还是探测，最终都交给 `get_bash_shell_config`，由
/// `is_legacy_wsl_bash_path` 决定用 `-c` 还是 `-s`。
async fn get_shell_config(custom_shell_path: Option<&str>) -> Result<ShellConfig, ExecutionError> {
    // TS 的 `if (customShellPath)` 把空串当作未配置；否则 `shellPath: ""` 会把所有
    // 字符串命令变成 `Custom shell path not found: `。
    let custom_shell_path = custom_shell_path.filter(|path| !path.is_empty());
    if let Some(custom_shell_path) = custom_shell_path {
        if path_exists(custom_shell_path).await {
            return Ok(get_bash_shell_config(custom_shell_path));
        }
        return Err(ExecutionError::new(
            ExecutionErrorCode::ShellUnavailable,
            format!("Custom shell path not found: {custom_shell_path}"),
        ));
    }
    if cfg!(windows) {
        let mut candidates: Vec<String> = Vec::new();
        if let Ok(program_files) = std::env::var("ProgramFiles") {
            candidates.push(format!("{program_files}\\Git\\bin\\bash.exe"));
        }
        if let Ok(program_files_x86) = std::env::var("ProgramFiles(x86)") {
            candidates.push(format!("{program_files_x86}\\Git\\bin\\bash.exe"));
        }
        for candidate in &candidates {
            if path_exists(candidate).await {
                return Ok(get_bash_shell_config(candidate));
            }
        }
        if let Some(bash_on_path) = find_bash_on_path().await {
            return Ok(get_bash_shell_config(&bash_on_path));
        }
        let searched = candidates
            .iter()
            .map(|path| format!("  {path}"))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(ExecutionError::new(
            ExecutionErrorCode::ShellUnavailable,
            format!(
                "No bash shell found. Options:\n  1. Install Git for Windows: https://git-scm.com/download/win\n  2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n  3. Configure an explicit shellPath\n\nSearched Git Bash in:\n{searched}"
            ),
        ));
    }
    if path_exists("/bin/bash").await {
        return Ok(get_bash_shell_config("/bin/bash"));
    }
    if let Some(bash_on_path) = find_bash_on_path().await {
        return Ok(get_bash_shell_config(&bash_on_path));
    }
    Ok(ShellConfig {
        shell: "sh".to_owned(),
        args: vec!["-c".to_owned()],
        transport: CommandTransport::Argv,
    })
}

/// `getShellEnv(baseEnv, extraEnv, inheritEnv)`：算出传给子进程的完整环境。
///
/// 优先级从低到高是 `process.env` < `baseEnv`（构造时的 `shellEnv`）< `extraEnv`
/// （每次 `exec` 的 `options.env`）；同名键后者覆盖前者。
///
/// 关键边界：`inheritEnv === false` 时整段 `process.env` 与 `baseEnv` **都不参与**，
/// 结果只含 `extraEnv`。也就是说关闭继承会连带丢弃构造时配置的 `shellEnv`，这是 TS
/// 的行为，不是遗漏。
fn get_shell_env(
    base_env: Option<&IndexMap<String, String>>,
    extra_env: Option<&IndexMap<String, String>>,
    inherit_env: Option<bool>,
) -> IndexMap<String, String> {
    let mut environment: IndexMap<String, String> = IndexMap::new();
    if inherit_env != Some(false) {
        for (key, value) in std::env::vars_os() {
            environment.insert(
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            );
        }
        if let Some(base_env) = base_env {
            for (key, value) in base_env {
                environment.insert(key.clone(), value.clone());
            }
        }
    }
    if let Some(extra_env) = extra_env {
        for (key, value) in extra_env {
            environment.insert(key.clone(), value.clone());
        }
    }
    environment
}

/// `killProcessTree(pid)`：尽力终止一条命令的整棵进程树。
///
/// `pid` 为 `None`（`Child::id()` 取不到 id，例如进程已被回收）时什么也不做，因此
/// 调用点可以在任何阶段无条件调用。Unix 先对进程组 `-pid` 发 `SIGKILL`，失败再回退为只杀 `pid`；
/// Windows 把 `taskkill /F /T /PID` 启动为独立进程、不等待结果。两种实现都是
/// “发信号/启动命令后立即返回”，不会等待目标真正退出。
fn kill_process_tree(pid: Option<u32>) {
    let Some(pid) = pid else {
        return;
    };
    #[cfg(unix)]
    {
        // 子进程在 POSIX 上以 `detached: true` 启动，即自己是进程组的组长。
        // SAFETY: 两次 `libc::kill` 都是 FFI 系统调用，参数只有整数 pid 与信号编号，
        // 不读写本进程内存，因此不存在指针/别名方面的前置条件；返回值只表示信号是否
        // 送达，失败（组已不存在、权限不足等）时回退为只杀单个 pid。
        let result = unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        if result != 0 {
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
    }
    #[cfg(windows)]
    {
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_owned());
        let executable = win32::join(&[&system_root, "System32", "taskkill.exe"]);
        let mut command = std::process::Command::new(executable);
        command
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // TS: `detached: true, windowsHide: true`（libuv 同时设置 DETACHED_PROCESS、
        // CREATE_NEW_PROCESS_GROUP 与 CREATE_NO_WINDOW）。
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        let _ = command.spawn();
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
        })
        .unwrap_or_else(|| "callback failed".to_owned())
}

// ---------------------------------------------------------------------------
// 读取器实现
// ---------------------------------------------------------------------------

/// `NodeTextLineReader` 的共享状态。
///
/// - `byte_offset` 是下一次 `read` 的**显式**起点；用显式 `seek` 而不是依赖文件游标，
///   是为了在取消后重试时不会跳过已经读过的字节。
/// - `decoder` 跨块保存不完整的 UTF-8；`buffered` 保存尚未组成完整行的已解码文本，
///   所以超长行会一直增长到遇到换行为止。
/// - `ended` 表示底层文件已读到 EOF，之后还要把 `buffered` 里的残余作为最后一行
///   （`terminated: false`）返回一次；`closed` 一旦置位，后续读取都报 invalid。
struct TextLineReaderInner {
    file: Option<tokio::fs::File>,
    path: String,
    decoder: StreamDecoder,
    chunk: Vec<u8>,
    byte_offset: u64,
    buffered: String,
    ended: bool,
    closed: bool,
}

/// 严格的 LF 读取器（TS: `NodeTextLineReader`）。
///
/// Node 的 readline 不报告最后一行是否以换行结束，所以这里自己解码。
struct NodeTextLineReader {
    inner: tokio::sync::Mutex<TextLineReaderInner>,
}

impl NodeTextLineReader {
    fn new(file: tokio::fs::File, path: String) -> Self {
        NodeTextLineReader {
            inner: tokio::sync::Mutex::new(TextLineReaderInner {
                file: Some(file),
                path,
                decoder: StreamDecoder::new(),
                chunk: vec![0u8; TEXT_CHUNK],
                byte_offset: 0,
                buffered: String::new(),
                ended: false,
                closed: false,
            }),
        }
    }
}

#[async_trait]
impl TextLineReader for NodeTextLineReader {
    async fn read_line(&self, context: &dyn Context) -> Result<Option<TextLine>, FileError> {
        let signal = context.abort_signal();
        let mut inner = self.inner.lock().await;
        if let Some(result) = abort_result::<Option<TextLine>>(&signal, Some(&inner.path)) {
            return result;
        }
        if inner.closed {
            return Err(
                FileError::new(FileErrorCode::Invalid, "Text line reader is closed")
                    .with_path(&inner.path),
            );
        }
        // 通过 `&mut` 结构体投影字段，让 `file` 的读与 `chunk`/`decoder` 的借用互不冲突。
        let inner = &mut *inner;
        // 每一轮先尝试从 `buffered` 切出一整行（只认 `\n`，保留 `\r`，与 readline 一致）；
        // 没有就再读一块。超长行会跨多轮累积在 `buffered` 里，不会被块大小截断。
        loop {
            if let Some(newline) = inner.buffered.find('\n') {
                let text = inner.buffered[..newline].to_owned();
                inner.buffered.drain(..=newline);
                return Ok(Some(TextLine {
                    text,
                    terminated: true,
                }));
            }
            if inner.ended {
                if inner.buffered.is_empty() {
                    return Ok(None);
                }
                let text = std::mem::take(&mut inner.buffered);
                return Ok(Some(TextLine {
                    text,
                    terminated: false,
                }));
            }
            let path = inner.path.clone();
            let Some(file) = inner.file.as_mut() else {
                return Err(
                    FileError::new(FileErrorCode::Invalid, "Text line reader is closed")
                        .with_path(&path),
                );
            };
            // 显式位置，让被取消的读取可以重试而不跳过字节。
            let offset = inner.byte_offset;
            if let Err(error) = file.seek(std::io::SeekFrom::Start(offset)).await {
                return Err(to_file_error_io_fd(&error, "read", Some(&path)));
            }
            let read = file.read(&mut inner.chunk).await;
            let read = match read {
                Ok(read) => read,
                Err(error) => return Err(to_file_error_io_fd(&error, "read", Some(&path))),
            };
            if let Some(result) = abort_result::<Option<TextLine>>(&signal, Some(&path)) {
                return result;
            }
            if read == 0 {
                // EOF：冲刷解码器里暂存的不完整序列，并把 `ended` 置位；下一轮循环会
                // 把 `buffered` 的残余作为 `terminated: false` 的最后一行返回。
                let text = inner.decoder.decode(None);
                inner.buffered.push_str(&text);
                inner.ended = true;
            } else {
                inner.byte_offset += read as u64;
                let text = inner.decoder.decode(Some(&inner.chunk[..read]));
                inner.buffered.push_str(&text);
            }
        }
    }

    async fn close(&self, _context: &dyn Context) {
        let mut inner = self.inner.lock().await;
        if inner.closed {
            return;
        }
        inner.closed = true;
        inner.buffered.clear();
        // `FileHandle.close()`：句柄在 close() 时释放，而不是等读取器对象被 drop。
        inner.file = None;
    }
}

/// `NodeBinaryReader` 的共享状态；`file: None` 或 `closed` 都表示句柄已释放。
struct BinaryReaderInner {
    file: Option<tokio::fs::File>,
    path: String,
    closed: bool,
}

/// 对一个已打开普通文件的位置读取器（TS: `NodeBinaryReader`）。
struct NodeBinaryReader {
    inner: tokio::sync::Mutex<BinaryReaderInner>,
}

impl NodeBinaryReader {
    fn new(file: tokio::fs::File, path: String) -> Self {
        NodeBinaryReader {
            inner: tokio::sync::Mutex::new(BinaryReaderInner {
                file: Some(file),
                path,
                closed: false,
            }),
        }
    }
}

#[async_trait]
impl BinaryReader for NodeBinaryReader {
    async fn info(&self, context: &dyn Context) -> Result<FileInfo, FileError> {
        let signal = context.abort_signal();
        let inner = self.inner.lock().await;
        if let Some(result) = abort_result::<FileInfo>(&signal, Some(&inner.path)) {
            return result;
        }
        if inner.closed {
            return closed_result("Binary reader", &inner.path);
        }
        let Some(file) = inner.file.as_ref() else {
            return closed_result("Binary reader", &inner.path);
        };
        match file.metadata().await {
            Ok(metadata) => file_info_from_metadata(&inner.path, &metadata),
            Err(error) => Err(to_file_error_io_fd(&error, "fstat", Some(&inner.path))),
        }
    }

    async fn read(
        &self,
        offset: u64,
        length: usize,
        context: &dyn Context,
    ) -> Result<Vec<u8>, FileError> {
        let signal = context.abort_signal();
        let mut inner = self.inner.lock().await;
        if let Some(result) = abort_result::<Vec<u8>>(&signal, Some(&inner.path)) {
            return result;
        }
        if inner.closed {
            return closed_result("Binary reader", &inner.path);
        }
        let path = inner.path.clone();
        let inner = &mut *inner;
        // 分块读取并累积：每块最多 `BINARY_READ_CHUNK`（1 MiB），而不是一次分配
        // `length` 字节。遇到 EOF（read == 0）提前结束，因此返回值可能短于 `length`，
        // 这是正常结果而不是错误。
        let mut bytes: Vec<u8> = Vec::new();
        let mut total: usize = 0;
        while total < length {
            let want = (length - total).min(BINARY_READ_CHUNK);
            let mut chunk = vec![0u8; want];
            let Some(file) = inner.file.as_mut() else {
                return closed_result("Binary reader", &path);
            };
            if let Err(error) = file
                .seek(std::io::SeekFrom::Start(offset + total as u64))
                .await
            {
                return Err(to_file_error_io_fd(&error, "read", Some(&path)));
            }
            let read = file.read(&mut chunk).await;
            let read = match read {
                Ok(read) => read,
                Err(error) => return Err(to_file_error_io_fd(&error, "read", Some(&path))),
            };
            if let Some(result) = abort_result::<Vec<u8>>(&signal, Some(&path)) {
                return result;
            }
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..read]);
            total += read;
        }
        Ok(bytes)
    }

    async fn scan_lines(
        &self,
        range: LineRange,
        context: &dyn Context,
    ) -> Result<LineScan, FileError> {
        let signal = context.abort_signal();
        let mut inner = self.inner.lock().await;
        if let Some(result) = abort_result::<LineScan>(&signal, Some(&inner.path)) {
            return result;
        }
        if inner.closed {
            return closed_result("Binary reader", &inner.path);
        }
        let path = inner.path.clone();
        let mut scanner = match LineScanner::new(
            range.start_line as f64,
            range.end_line.map(|end| end as f64),
        ) {
            Ok(scanner) => scanner,
            Err(_) => {
                return Err(
                    FileError::new(FileErrorCode::Invalid, "Invalid line range").with_path(&path)
                );
            }
        };
        let inner = &mut *inner;
        // 用 64 KiB 块（`TEXT_CHUNK`，与 TS 的局部常量一致，不是读取器的 1 MiB）
        // 顺序喂给 `LineScanner`；后者只累计行边界与字节偏移（外加最多 3 字节的 BOM
        // 判定缓冲），不会保留整份文件内容。
        let mut chunk = vec![0u8; TEXT_CHUNK];
        let mut position: u64 = 0;
        loop {
            let Some(file) = inner.file.as_mut() else {
                return closed_result("Binary reader", &path);
            };
            if let Err(error) = file.seek(std::io::SeekFrom::Start(position)).await {
                return Err(to_file_error_io_fd(&error, "read", Some(&path)));
            }
            let read = file.read(&mut chunk).await;
            let read = match read {
                Ok(read) => read,
                Err(error) => return Err(to_file_error_io_fd(&error, "read", Some(&path))),
            };
            if let Some(result) = abort_result::<LineScan>(&signal, Some(&path)) {
                return result;
            }
            if read == 0 {
                return Ok(scanner.finish());
            }
            scanner.push(&chunk[..read]);
            position += read as u64;
        }
    }

    async fn close(&self, _context: &dyn Context) {
        let mut inner = self.inner.lock().await;
        if inner.closed {
            return;
        }
        inner.closed = true;
        inner.file = None;
    }
}

/// `NodeDirReader` 的共享状态：`done` 一旦为真，分页读取就只返回空页。
struct DirReaderInner {
    dir: Option<tokio::fs::ReadDir>,
    path: String,
    done: bool,
    closed: bool,
}

/// 目录分页读取器（TS: `NodeDirReader`）。
struct NodeDirReader {
    inner: tokio::sync::Mutex<DirReaderInner>,
}

impl NodeDirReader {
    fn new(dir: tokio::fs::ReadDir, path: String) -> Self {
        NodeDirReader {
            inner: tokio::sync::Mutex::new(DirReaderInner {
                dir: Some(dir),
                path,
                done: false,
                closed: false,
            }),
        }
    }
}

#[async_trait]
impl DirReader for NodeDirReader {
    async fn next(&self, max_entries: usize, context: &dyn Context) -> Result<DirPage, FileError> {
        let signal = context.abort_signal();
        let mut inner = self.inner.lock().await;
        if let Some(result) = abort_result::<DirPage>(&signal, Some(&inner.path)) {
            return result;
        }
        if inner.closed {
            return closed_result("Directory reader", &inner.path);
        }
        if max_entries == 0 {
            return Err(FileError::new(
                FileErrorCode::Invalid,
                "maxEntries must be a positive safe integer",
            )
            .with_path(&inner.path));
        }
        let path = inner.path.clone();
        let inner = &mut *inner;
        // 每次 `next` 只消费到 `max_entries` 条，`ReadDir` 的游标保存在 `inner.dir`
        // 里，所以调用方可以分页；`max_entries == 0` 已在上面按非法参数拒绝。
        let mut entries: Vec<FileInfo> = Vec::new();
        while !inner.done && entries.len() < max_entries {
            let Some(dir) = inner.dir.as_mut() else {
                return closed_result("Directory reader", &path);
            };
            let entry = match dir.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => {
                    inner.done = true;
                    break;
                }
                Err(error) => return Err(to_file_error_io_fd(&error, "scandir", Some(&path))),
            };
            if let Some(result) = abort_result::<DirPage>(&signal, Some(&path)) {
                return result;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let entry_path = platform_resolve(&[&path, &name], &process_cwd());
            let metadata = match tokio::fs::symlink_metadata(&entry_path).await {
                Ok(metadata) => metadata,
                // 枚举与 lstat 之间被删：不再是列表的一部分。
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(to_file_error_io(&error, "lstat", Some(&entry_path)));
                }
            };
            // 非普通文件/目录/符号链接（FIFO、socket 等）不进入分页结果：TS 的
            // `fileInfoFromStats` 同样只跳过该条，不报错。
            if let Ok(info) = file_info_from_metadata(&entry_path, &metadata) {
                entries.push(info);
            }
        }
        Ok(DirPage {
            entries,
            done: inner.done,
        })
    }

    async fn close(&self, _context: &dyn Context) {
        let mut inner = self.inner.lock().await;
        inner.closed = true;
        // `Dir.close()`：目录句柄在 close() 时释放。
        inner.dir = None;
    }
}

// ---------------------------------------------------------------------------
// spill
// ---------------------------------------------------------------------------

/// 一次 `exec` 的 spill 状态。
///
/// 状态机很简单但有两个不变量：
/// - 阈值未越过时，原始输出块只进入 `prefix`；`started` 一置位就不再回退，即使随后
///   创建/写入失败也不会重试（错误已记录，后续块只做解码与回调）。
/// - `error` 保存**第一个**失败：`fail` 在已有错误时直接返回，避免后续错误覆盖根因。
struct SpillState {
    started: bool,
    path: Option<String>,
    file: Option<tokio::fs::File>,
    prefix: Vec<Vec<u8>>,
    seen_bytes: usize,
    seen_newlines: usize,
    error: Option<ExecutionError>,
}

impl SpillState {
    fn new() -> Self {
        SpillState {
            started: false,
            path: None,
            file: None,
            prefix: Vec::new(),
            seen_bytes: 0,
            seen_newlines: 0,
            error: None,
        }
    }

    fn fail(&mut self, message: String, cause: Option<Arc<dyn std::error::Error + Send + Sync>>) {
        if self.error.is_some() {
            return;
        }
        let mut error = ExecutionError::new(
            ExecutionErrorCode::Unknown,
            format!("Failed to preserve complete shell output: {message}"),
        );
        if let Some(cause) = cause {
            error = error.with_cause(cause);
        }
        self.error = Some(error);
    }

    /// `createWriteStream(spillPath, ...)` 的打开失败：Node 的 `fs.open` 错误带路径
    /// （`ENOENT: no such file or directory, open '<spillPath>'`）。
    fn fail_open(&mut self, error: &std::io::Error, path: &str) {
        self.fail(
            node_io_message(error, "open", Some(path), None),
            Some(io_error_cause(error)),
        );
    }

    /// 已打开句柄上的写入失败：Node 的 `fs.write` 错误不带路径（`, write`）。
    fn fail_io(&mut self, error: &std::io::Error) {
        self.fail(
            node_io_message(error, "write", None, None),
            Some(io_error_cause(error)),
        );
    }
}

// ---------------------------------------------------------------------------
// NodeExecutionEnv
// ---------------------------------------------------------------------------

/// `NodeExecutionEnv`：本地文件系统与 shell。
///
/// 一个实例代表一块文件命名空间加一组固定的 shell 配置：
/// - `cwd` 是初始工作目录；`cwd_override` 支持 trait 的 `set_cwd`（内部可变性，旧值
///   通过 `Box::leak` 保活）。
/// - `shell_path` / `shell_env` 来自构造选项，决定字符串命令用哪个 shell、固定注入
///   哪些环境变量。
/// - `watch_options` 原样转交给 `NodeFileWatcher`。
/// - `active_child_pids` 记录正在运行的直接子进程：`exec` spawn 成功后加入、收尾时
///   移除，[`NodeExecutionEnv::cleanup`] 据此终止整条命令。
///
/// 实例本身不持有后台任务的句柄：`exec` spawn 的读取任务在收尾时 abort，等待任务在
/// 送出退出状态后自然结束，stdin 写入任务是写完自行结束的 detached 任务，没有需要
/// join 的句柄。但丢弃实例**不会**自动杀死已经在跑的子进程；需要终止时显式调用
/// `cleanup`。
pub struct NodeExecutionEnv {
    cwd: String,
    cwd_override: Mutex<Option<&'static str>>,
    shell_path: Option<String>,
    shell_env: Option<IndexMap<String, String>>,
    watch_options: NodeWatchOptions,
    active_child_pids: Mutex<HashSet<u32>>,
    #[cfg(test)]
    spill_temp_file: Option<Arc<dyn Fn() -> Result<String, FileError> + Send + Sync>>,
}

impl std::fmt::Debug for NodeExecutionEnv {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NodeExecutionEnv")
            .field("cwd", &self.cwd())
            .finish_non_exhaustive()
    }
}

impl NodeExecutionEnv {
    /// `new NodeExecutionEnv({ cwd, shellPath?, shellEnv?, watch? })`。
    ///
    /// 参数用 `impl Into<NodeEnvOptions>`，所以可以直接传 [`NodeEnvOptions`]、`String`
    /// 或 `&str`（后两者只设置 `cwd`）。
    pub fn new(options: impl Into<NodeEnvOptions>) -> Self {
        let options = options.into();
        NodeExecutionEnv {
            cwd: options.cwd,
            cwd_override: Mutex::new(None),
            shell_path: options.shell_path,
            shell_env: options.shell_env,
            watch_options: options.watch.unwrap_or_default(),
            active_child_pids: Mutex::new(HashSet::new()),
            #[cfg(test)]
            spill_temp_file: None,
        }
    }

    /// `id`：每个本地环境看到同一份文件；常量值固定为 `node:local`。
    pub const ID: &'static str = "node:local";

    /// 杀死该环境仍在运行的所有命令（`FileSystem::cleanup` 与 `Shell::cleanup` 的共同实现）。
    ///
    /// 实现只做两件事：对 `active_child_pids` 的**当前快照**逐个调用
    /// `kill_process_tree`（Unix 先杀整个进程组，失败再回退单 pid；Windows 走
    /// `taskkill /F /T`），然后清空集合。它既不等待进程退出，也不等待正在进行的
    /// `exec` 收尾；被杀命令的 `exec` 在 Unix 上会拿到信号退出状态，在 Windows 上拿到
    /// `taskkill` 之后的普通退出码（`exit_status_code` 只取 `status.code()`），都按正常的
    /// `ShellExecResult` 返回（`tests/env_node.rs` 的
    /// `cleanup_terminates_active_shell_processes` 断言 `Ok`），而不是 `aborted` 错误。
    ///
    /// 并发调用不会造成数据竞争（快照在锁内复制，集合的读写都在锁内），但快照与 `kill`
    /// 之间没有 pid 复用保护：进程可能在这段时间退出、pid 被新命令复用，此时会向复用的
    /// pid 发送 `SIGKILL` / `taskkill`；两个并发调用也可能对同一批 pid 重复发送信号。
    pub async fn cleanup(&self, _context: &dyn Context) {
        let pids: Vec<u32> = lock(&self.active_child_pids).iter().copied().collect();
        for pid in pids {
            kill_process_tree(Some(pid));
        }
        lock(&self.active_child_pids).clear();
    }

    /// spill 文件走 `create_temp_file`：名字是 `kiana-output-<uuid>.log`，放在新建的
    /// 临时目录里：Unix 上以 0700 创建，非 Unix 分支只调用 `std::fs::create_dir`、不设置
    /// 0700（Windows 上目录权限由父目录 ACL 决定）；见 `create_private_dir`。
    async fn create_spill_file(&self, context: &dyn Context) -> Result<String, FileError> {
        #[cfg(test)]
        if let Some(factory) = &self.spill_temp_file {
            return factory();
        }
        self.create_temp_file(
            Some(&CreateTempFileOptions {
                prefix: Some("kiana-output-".to_owned()),
                suffix: Some(".log".to_owned()),
            }),
            context,
        )
        .await
    }

    /// `startSpill(chunk)`：首次调用创建临时文件，然后按顺序写入。
    ///
    /// 打开模式是 `append + create`，对应 TS `createWriteStream` 的 `{ flags: "a" }`。
    /// 创建临时文件（含 mkdtemp 目录）或打开失败时立刻 `kill_process_tree(pid)`：命令本身
    /// 继续跑下去只会产生无法保存的输出，不如按失败收场。
    async fn start_spill(
        &self,
        spill: &mut SpillState,
        context: &dyn Context,
        chunk: &[u8],
        pid: Option<u32>,
    ) {
        if spill.file.is_some() {
            Self::write_spill(spill, chunk, pid).await;
            return;
        }
        if spill.started {
            // 创建曾失败：不再重试（TS 里 `spillStart` 已定义，chunk 只入队）。
            return;
        }
        spill.started = true;
        match self.create_spill_file(context).await {
            Ok(path) => {
                spill.path = Some(path.clone());
                match tokio::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(&path)
                    .await
                {
                    Ok(file) => {
                        spill.file = Some(file);
                        Self::write_spill(spill, chunk, pid).await;
                    }
                    Err(error) => {
                        spill.fail_open(&error, &path);
                        kill_process_tree(pid);
                    }
                }
            }
            Err(error) => {
                spill.fail(error.to_string(), error.cause.clone());
                kill_process_tree(pid);
            }
        }
    }

    async fn write_spill(spill: &mut SpillState, chunk: &[u8], pid: Option<u32>) {
        if spill.error.is_some() || chunk.is_empty() {
            return;
        }
        let Some(file) = spill.file.as_mut() else {
            return;
        };
        if let Err(error) = file.write_all(chunk).await {
            spill.fail_io(&error);
            kill_process_tree(pid);
        }
    }

    /// `finishSpill()`。
    async fn finish_spill(spill: &mut SpillState) {
        if spill.error.is_some() {
            return;
        }
        let Some(file) = spill.file.as_mut() else {
            return;
        };
        if let Err(error) = file.flush().await {
            spill.fail_io(&error);
        }
    }

    async fn exec_impl(
        &self,
        command: ShellCommand,
        options: Option<&ShellExecOptions>,
        context: &dyn Context,
    ) -> Result<ShellExecResult, ExecutionError> {
        // 流程：入口检查（abort/timeout）→ 解析命令与 shell → 校验 cwd → 合并环境 →
        // spawn 并登记 pid → 主循环收输出 → 收尾与错误优先级。各阶段的契约见模块文档。
        let signal = context.abort_signal();
        if signal.as_ref().is_some_and(AbortSignal::is_aborted) {
            return Err(ExecutionError::new(ExecutionErrorCode::Aborted, "aborted"));
        }
        let timeout_ms = resolve_timeout_ms(options.and_then(|options| options.timeout))?;

        // 字符串走环境的 shell；argv 数组直接执行程序，不经 shell 解析。
        let (program, args, stdin_command): (String, Vec<String>, Option<String>) = match &command {
            ShellCommand::Shell(script) => {
                let config = get_shell_config(self.shell_path.as_deref()).await?;
                let mut args = config.args.clone();
                let stdin_command = match config.transport {
                    CommandTransport::Stdin => Some(script.clone()),
                    CommandTransport::Argv => {
                        args.push(script.clone());
                        None
                    }
                };
                (config.shell, args, stdin_command)
            }
            ShellCommand::Argv(argv) => {
                let Some((first, rest)) = argv.split_first() else {
                    return Err(ExecutionError::new(
                        ExecutionErrorCode::SpawnError,
                        "Empty argv: no program to run",
                    ));
                };
                (first.clone(), rest.to_vec(), None)
            }
        };

        let cwd = match options
            .and_then(|options| options.cwd.as_deref())
            .filter(|cwd| !cwd.is_empty())
        {
            Some(cwd) => resolve_path(self.cwd(), cwd),
            None => self.cwd().to_owned(),
        };
        // TS 在 spawn 之前先 `access(cwd, F_OK)`：cwd 取不到元数据时返回这条固定文案，
        // 而不是 spawn 的 ENOTDIR/ENOENT。注意这里用的是 `tokio::fs::metadata`（`stat`，
        // 会跟随符号链接，断链会在此失败），只检查“能 stat/access 到”；cwd 是普通文件时
        // 仍会走到 spawn 并得到 `spawn ENOTDIR`（见 `tests/env_node.rs` 的
        // `reports_a_synchronous_spawn_enotdir_without_the_program_name`）。
        if let Err(error) = tokio::fs::metadata(&cwd).await {
            let cause: Arc<dyn std::error::Error + Send + Sync> =
                Arc::new(std::io::Error::new(error.kind(), error.to_string()));
            return Err(ExecutionError::new(
                ExecutionErrorCode::SpawnError,
                format!("Working directory does not exist: {cwd}\nCannot execute bash commands."),
            )
            .with_cause(cause));
        }

        // 先 `env_clear()` 再逐项注入：`inheritEnv: false` 时 `get_shell_env` 只返回
        // 调用方给的变量，清空能防止父进程的环境从这里泄漏进子进程。
        let environment = get_shell_env(
            self.shell_env.as_ref(),
            options.and_then(|options| options.env.as_ref()),
            options.and_then(|options| options.inherit_env),
        );
        let mut child_command = tokio::process::Command::new(&program);
        child_command
            .args(&args)
            .current_dir(&cwd)
            .env_clear()
            .stdin(if stdin_command.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &environment {
            child_command.env(key, value);
        }
        #[cfg(unix)]
        {
            // TS: `detached: process.platform !== "win32"`（POSIX 上创建进程组）。
            child_command.process_group(0);
        }
        #[cfg(windows)]
        {
            // TS: `windowsHide: true`（Windows 上 `detached` 是 false）。
            child_command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = match child_command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let cause = io_error_cause(&error);
                return Err(ExecutionError::new(
                    ExecutionErrorCode::SpawnError,
                    spawn_error_message(&program, &error),
                )
                .with_cause(cause));
            }
        };
        let pid = child.id();
        if let Some(pid) = pid {
            lock(&self.active_child_pids).insert(pid);
        }
        // 只有 stdin 传输才会走到这里。写入放在独立任务里，避免脚本较大时阻塞主循环
        // 开始读输出；写错误（子进程提前关闭管道）被忽略，`shutdown` 负责发 EOF。
        if let Some(stdin_command) = stdin_command
            && let Some(mut stdin) = child.stdin.take()
        {
            tokio::spawn(async move {
                let _ = stdin.write_all(stdin_command.as_bytes()).await;
                let _ = stdin.shutdown().await;
            });
        }
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        // `wait()` 必须进独立任务：主循环要在同一个 select 里同时等退出状态、输出、
        // 取消与超时，不能阻塞在一个 await 上。
        let (exit_sender, mut exit_receiver) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let status = child.wait().await;
            let _ = exit_sender.send(status);
        });

        let callback = options.and_then(|options| options.on_output.clone());
        let spill_options = options.and_then(|options| options.spill.as_ref()).copied();
        // 有界通道把“读管道”与“解码/回调/spill”解耦：容量 8 意味着读取任务最多领先
        // 主循环 8 块；主循环忙时 `send` 会阻塞读取，进而让子进程的管道写端阻塞。
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<ReaderMessage>(8);
        let mut reader_tasks = Vec::new();
        let mut stdout_eof = stdout.is_none();
        let mut stderr_eof = stderr.is_none();
        if let Some(stdout) = stdout {
            reader_tasks.push(spawn_reader(
                stdout,
                ShellOutputStream::Stdout,
                sender.clone(),
            ));
        }
        if let Some(stderr) = stderr {
            reader_tasks.push(spawn_reader(stderr, ShellOutputStream::Stderr, sender));
        }

        let mut stdout_decoder = StreamDecoder::new();
        let mut stderr_decoder = StreamDecoder::new();
        let mut spill = SpillState::new();
        let mut exited: Option<std::io::Result<std::process::ExitStatus>> = None;
        let mut grace_at: Option<Instant> = None;
        let mut timed_out = false;
        let mut abort_killed = false;
        let mut callback_error: Option<ExecutionError> = None;
        let timeout_deadline =
            timeout_ms.map(|ms| Instant::now() + Duration::from_secs_f64((ms / 1000.0).max(0.0)));
        let abort_token = signal.as_ref().map(|signal| signal.token().clone());

        // 主循环是一个显式状态机，状态由 `exited` / `stdout_eof` / `stderr_eof` /
        // `timed_out` / `abort_killed` 组成：
        // - 退出：wait 任务通过 oneshot 送回状态；收到后挂 100 ms 排空截止时间，
        //   之后每收一块数据都顺延。
        // - 输出：reader 任务把块送进容量 8 的通道；通道关闭（`None`）按两个流都
        //   结束处理。
        // - 取消：只杀一次进程树（`abort_killed` 去重），循环继续，以便收完已产生的
        //   输出再按 `aborted` 返回。
        // - 超时：`sleep` 到点后置 `timed_out` 并杀进程树，同样继续排空。
        // 因此循环只在“已退出且两个流都 EOF”或“退出后的排空期限到达”时结束；
        // 超时/取消本身不会直接跳出。
        loop {
            if exited.is_some() && stdout_eof && stderr_eof {
                break;
            }
            let mut deadline: Option<Instant> = None;
            if exited.is_some() {
                deadline = grace_at;
            }
            if !timed_out && let Some(timeout_deadline) = timeout_deadline {
                deadline = Some(match deadline {
                    Some(current) => current.min(timeout_deadline),
                    None => timeout_deadline,
                });
            }
            let sleep = async move {
                match deadline {
                    Some(at) => {
                        tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            let cancelled = async {
                match &abort_token {
                    Some(token) => token.cancelled().await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                _ = cancelled, if !abort_killed => {
                    abort_killed = true;
                    kill_process_tree(pid);
                }
                status = &mut exit_receiver, if exited.is_none() => {
                    match status {
                        Ok(status) => exited = Some(status),
                        // 等待任务被丢弃：按失败处理并不再等待进程。
                        Err(_) => exited = Some(Err(std::io::Error::other("child process wait was cancelled"))),
                    }
                    grace_at = Some(Instant::now() + Duration::from_millis(EXIT_STDIO_GRACE_MS));
                }
                message = receiver.recv(), if !stdout_eof || !stderr_eof => {
                    match message {
                        None => {
                            stdout_eof = true;
                            stderr_eof = true;
                        }
                        Some(ReaderMessage::End(source)) => match source {
                            ShellOutputStream::Stdout => stdout_eof = true,
                            ShellOutputStream::Stderr => stderr_eof = true,
                        },
                        Some(ReaderMessage::Data(source, bytes)) => {
                            if exited.is_some() {
                                grace_at = Some(Instant::now() + Duration::from_millis(EXIT_STDIO_GRACE_MS));
                            }
                            let decoder = match source {
                                ShellOutputStream::Stdout => &mut stdout_decoder,
                                ShellOutputStream::Stderr => &mut stderr_decoder,
                            };
                            let text = decoder.decode(Some(&bytes));
                            emit_output(&callback, context, &text, source, &mut callback_error, pid);
                            // spill 判定按**累计**字节数与“含未终结末行”
                            // 的行数判断；恰好等于阈值仍算未越界（`<=`），此时块只暂存进
                            // `prefix`。一旦越界，先把 `prefix` 按原顺序补写、再写当前块，
                            // 从而让文件内容与原始输出（未解码字节）的顺序、内容都一致。
                            if let Some(spill_options) = spill_options
                                && !bytes.is_empty()
                            {
                                if spill.started {
                                    Self::write_spill(&mut spill, &bytes, pid).await;
                                } else {
                                    spill.seen_bytes += bytes.len();
                                    spill.seen_newlines += bytes.iter().filter(|byte| **byte == 0x0a).count();
                                    let lines = spill.seen_newlines
                                        + usize::from(bytes.last() != Some(&0x0a));
                                    if spill.seen_bytes <= spill_options.after_bytes
                                        && lines <= spill_options.after_lines
                                    {
                                        spill.prefix.push(bytes);
                                    } else {
                                        let prefix = std::mem::take(&mut spill.prefix);
                                        for chunk in prefix {
                                            self.start_spill(&mut spill, context, &chunk, pid).await;
                                        }
                                        self.start_spill(&mut spill, context, &bytes, pid).await;
                                    }
                                }
                            }
                        }
                    }
                }
                _ = sleep => {
                    let now = Instant::now();
                    if !timed_out && timeout_deadline.is_some_and(|at| at <= now) {
                        timed_out = true;
                        kill_process_tree(pid);
                    }
                    if exited.is_some() && grace_at.is_some_and(|at| at <= now) {
                        break;
                    }
                }
            }
        }
        // 收尾：先停掉可能仍卡在 `read`/`send` 的读取任务，再 flush spill，最后冲刷两个
        // 解码器的尾巴并各回调一次（不完整 UTF-8 会在这里变成 U+FFFD）。
        // 下面的判定顺序固定为 callback > timeout > aborted > spill，与 TS 的 settle
        // 顺序一致；`spill_path` 只在 timeout/aborted 错误上附带。
        for task in reader_tasks {
            task.abort();
        }
        Self::finish_spill(&mut spill).await;
        let stdout_tail = stdout_decoder.decode(None);
        emit_output(
            &callback,
            context,
            &stdout_tail,
            ShellOutputStream::Stdout,
            &mut callback_error,
            pid,
        );
        let stderr_tail = stderr_decoder.decode(None);
        emit_output(
            &callback,
            context,
            &stderr_tail,
            ShellOutputStream::Stderr,
            &mut callback_error,
            pid,
        );
        if let Some(pid) = pid {
            lock(&self.active_child_pids).remove(&pid);
        }
        if let Some(error) = callback_error {
            return Err(error);
        }
        let signal_aborted = signal.as_ref().is_some_and(AbortSignal::is_aborted);
        if timed_out {
            // 消息是 `timeout:<原始秒数>`（例如 `timeout:0.01`），数字格式走
            // `format_js_number` 以保持 JS 的写法；不是自然语言句子。
            let timeout_text = match options.and_then(|options| options.timeout) {
                Some(timeout) => format_js_number(timeout),
                None => "undefined".to_owned(),
            };
            let mut error = ExecutionError::new(
                ExecutionErrorCode::Timeout,
                format!("timeout:{timeout_text}"),
            );
            if let Some(path) = &spill.path {
                error = error.with_spill_path(path.clone());
            }
            return Err(error);
        }
        if signal_aborted {
            // 中止错误固定为 `aborted`；若 spill 已启动（`spill.path` 已设置），路径随
            // 错误返回。调用方可以读到取消前已写入 spill 的内容，但不能假定它完整：
            // 排空期限（`EXIT_STDIO_GRACE_MS`）到达时剩余输出会被放弃（见模块文档第 6 步），
            // 打开失败时 path 已设置而文件不可读/为空，`write_all` 失败也会留下部分文件。
            let mut error = ExecutionError::new(ExecutionErrorCode::Aborted, "aborted");
            if let Some(path) = &spill.path {
                error = error.with_spill_path(path.clone());
            }
            return Err(error);
        }
        if let Some(error) = spill.error.take() {
            return Err(error);
        }
        let exit_code = match exited {
            Some(Ok(status)) => exit_status_code(status),
            _ => 1,
        };
        Ok(ShellExecResult {
            exit_code,
            spill_path: spill.path.clone(),
        })
    }
}

enum ReaderMessage {
    Data(ShellOutputStream, Vec<u8>),
    End(ShellOutputStream),
}

/// 把一个输出管道泵进主循环的通道，直到 EOF、读错误或主循环退出。
///
/// `Ok(0)`（EOF）与 `Err` 都发送 `End`：`ReaderMessage` 没有错误分支，管道读错误不会
/// 出现在 `exec` 的返回值里；决定结果的是退出状态与超时/取消。`send` 失败说明主循环
/// 已经结束，任务随之退出。
fn spawn_reader<R>(
    mut stream: R,
    source: ShellOutputStream,
    sender: tokio::sync::mpsc::Sender<ReaderMessage>,
) -> tokio::task::JoinHandle<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buffer = vec![0u8; TEXT_CHUNK];
        loop {
            match stream.read(&mut buffer).await {
                Ok(0) | Err(_) => {
                    let _ = sender.send(ReaderMessage::End(source)).await;
                    break;
                }
                Ok(read) => {
                    if sender
                        .send(ReaderMessage::Data(source, buffer[..read].to_vec()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    })
}

/// `emit(text, stream)` 的三重闸门：`settled || text === "" || onOutput === undefined ||
/// callbackError !== undefined` → 丢弃。
///
/// 回调在本任务里同步执行，不另起线程；`catch_unwind` 只防止回调 panic 把 `exec`
/// 任务带走（error message 取 panic payload），回调在 panic 前已经改动过的外部状态
/// 不会被回滚。第一个 panic 会被记进 `callback_error`，后续输出因此全部丢弃。
fn emit_output(
    callback: &Option<crate::env::OutputCallback>,
    context: &dyn Context,
    text: &str,
    stream: ShellOutputStream,
    callback_error: &mut Option<ExecutionError>,
    pid: Option<u32>,
) {
    if text.is_empty() || callback_error.is_some() {
        return;
    }
    let Some(callback) = callback else {
        return;
    };
    let info = ShellOutputInfo {
        stream,
        skipped: None,
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        callback(text, context, &info);
    }));
    if let Err(payload) = outcome {
        let message = panic_message(payload);
        *callback_error = Some(ExecutionError::new(
            ExecutionErrorCode::CallbackError,
            message,
        ));
        kill_process_tree(pid);
    }
}

/// `code ?? (exitSignal ? 128 + signal : 1)`（信号被杀等价于 `128 + 信号编号`）。
fn exit_status_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    1
}

// ---------------------------------------------------------------------------
// FileSystem
// ---------------------------------------------------------------------------

// `FileSystem` 实现的共同约定：
// - 所有路径参数先 `resolve_path(self.cwd(), path)`；文件操作的路径/IO 错误经
//   `to_file_error_*` 构造，让消息文本、`path` 字段与 `cause` 和 Node 对齐
//   （参数校验错误与 `watch` 的错误在别处构造）。
// - 带 I/O 的异步方法在入口检查一次 abort（纯路径计算的 `absolute_path`/`join_path`
//   与 `cleanup` 不检查）。可能长时间阻塞的读/写（`read_text_file`、
//   `read_binary_file`、`write_file`）用 `cancellable_io` 与取消竞速；`append_file`
//   在写完后补查一次；打开句柄的操作在打开后补查一次。`create_dir`/`remove`/`rename_file`/`canonical_path`
//   只在入口检查，操作过程中到来的取消不会中断这次调用。
#[async_trait]
impl FileSystem for NodeExecutionEnv {
    fn id(&self) -> &str {
        Self::ID
    }

    fn cwd(&self) -> &str {
        if let Some(cwd) = *lock(&self.cwd_override) {
            return cwd;
        }
        &self.cwd
    }

    fn set_cwd(&self, cwd: String) {
        // `cwd()` 返回 `&str`（冻结的 trait 签名），因此旧值必须保持存活；每次设置泄漏一
        // 个小字符串。`set_cwd` 在实际使用中很少调用。
        *lock(&self.cwd_override) = Some(Box::leak(cwd.into_boxed_str()));
    }

    // `absolutePath` 也做 `~` / `file://` 展开与平台解析；`join_path` 对应
    // `node:path.join`：会 normalize（`.`、`..`、重复分隔符），但不会把相对结果变成
    // 绝对路径，也不做 `~`/`file://` 展开。
    async fn absolute_path(&self, path: &str, _context: &dyn Context) -> Result<String, FileError> {
        Ok(resolve_path(self.cwd(), path))
    }

    async fn join_path(
        &self,
        parts: &[String],
        _context: &dyn Context,
    ) -> Result<String, FileError> {
        let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
        Ok(platform_join(&parts))
    }

    async fn read_text_file(&self, path: &str, context: &dyn Context) -> Result<String, FileError> {
        let resolved = resolve_path(self.cwd(), path);
        let signal = context.abort_signal();
        if let Some(result) = abort_result::<String>(&signal, Some(&resolved)) {
            return result;
        }
        // `readFile(path, {encoding, signal})`：打开失败带 `open '<path>'`，读取失败带
        // `read '<path>'`（Node 对目录的 EISDIR 就是后者）。
        let read = async {
            let mut file = tokio::fs::File::open(&resolved)
                .await
                .map_err(|error| to_file_error_io(&error, "open", Some(&resolved)))?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .await
                .map_err(|error| to_file_error_io(&error, "read", Some(&resolved)))?;
            Ok(bytes)
        };
        let bytes = cancellable_io(read, &signal, &resolved).await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn open_text_line_reader(
        &self,
        path: &str,
        context: &dyn Context,
    ) -> Result<Arc<dyn TextLineReader>, FileError> {
        let resolved = resolve_path(self.cwd(), path);
        let signal = context.abort_signal();
        if let Some(result) = abort_result::<Arc<dyn TextLineReader>>(&signal, Some(&resolved)) {
            return result;
        }
        match tokio::fs::File::open(&resolved).await {
            Ok(file) => {
                // 打开期间被取消：先显式释放句柄，再返回 aborted，不能把已打开的文件
                // 交给调用方。
                if let Some(result) =
                    abort_result::<Arc<dyn TextLineReader>>(&signal, Some(&resolved))
                {
                    drop(file);
                    return result;
                }
                Ok(Arc::new(NodeTextLineReader::new(file, resolved)))
            }
            Err(error) => Err(to_file_error_io(&error, "open", Some(&resolved))),
        }
    }

    async fn read_text_lines(
        &self,
        path: &str,
        options: Option<&ReadTextLinesOptions>,
        context: &dyn Context,
    ) -> Result<Vec<String>, FileError> {
        // TS 对 `maxLines !== undefined && maxLines <= 0` 返回空数组；Rust 的 `usize`
        // 没有负数，因此只需处理 0，而且是**先返回**、不打开文件。
        let max_lines = options.and_then(|options| options.max_lines);
        if max_lines == Some(0) {
            return Ok(Vec::new());
        }
        let reader = self.open_text_line_reader(path, context).await?;
        // 无论正常读完还是中途失败，读取器都会在返回前关闭（失败时先 close 再透传
        // 原始错误，不覆盖错误内容）。
        let mut lines: Vec<String> = Vec::new();
        loop {
            if max_lines.is_some_and(|max| lines.len() >= max) {
                break;
            }
            match reader.read_line(context).await {
                Ok(Some(line)) => lines.push(line.text),
                Ok(None) => break,
                Err(error) => {
                    reader.close(context).await;
                    return Err(error);
                }
            }
        }
        reader.close(context).await;
        Ok(lines)
    }

    async fn read_binary_file(
        &self,
        path: &str,
        context: &dyn Context,
    ) -> Result<Vec<u8>, FileError> {
        let resolved = resolve_path(self.cwd(), path);
        let signal = context.abort_signal();
        if let Some(result) = abort_result::<Vec<u8>>(&signal, Some(&resolved)) {
            return result;
        }
        let read = async {
            let mut file = tokio::fs::File::open(&resolved)
                .await
                .map_err(|error| to_file_error_io(&error, "open", Some(&resolved)))?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .await
                .map_err(|error| to_file_error_io(&error, "read", Some(&resolved)))?;
            Ok(bytes)
        };
        cancellable_io(read, &signal, &resolved).await
    }

    async fn open_binary_reader(
        &self,
        path: &str,
        options: Option<&OpenBinaryReaderOptions>,
        context: &dyn Context,
    ) -> Result<Arc<dyn BinaryReader>, FileError> {
        let resolved = resolve_path(self.cwd(), path);
        let signal = context.abort_signal();
        if let Some(result) = abort_result::<Arc<dyn BinaryReader>>(&signal, Some(&resolved)) {
            return result;
        }
        let no_follow = options.and_then(|options| options.no_follow) == Some(true);
        // 先打开再 fstat：目录、FIFO、设备也可能打开成功，但 `BinaryReader` 只接受
        // 普通文件（FIFO 在 POSIX 上还要靠 `O_NONBLOCK` 才不会卡在 open 上）。
        let file = match open_binary_file(&resolved, no_follow).await {
            Ok(file) => file,
            Err(error) => return Err(error),
        };
        let metadata = match file.metadata().await {
            Ok(metadata) => metadata,
            Err(error) => {
                drop(file);
                return Err(to_file_error_io_fd(&error, "fstat", Some(&resolved)));
            }
        };
        if !metadata.is_file() {
            drop(file);
            if metadata.is_dir() {
                return Err(FileError::new(
                    FileErrorCode::IsDirectory,
                    "EISDIR: illegal operation on a directory, read",
                )
                .with_path(&resolved));
            }
            return Err(
                FileError::new(FileErrorCode::Invalid, "Not a regular file").with_path(&resolved)
            );
        }
        if let Some(result) = abort_result::<Arc<dyn BinaryReader>>(&signal, Some(&resolved)) {
            drop(file);
            return result;
        }
        Ok(Arc::new(NodeBinaryReader::new(file, resolved)))
    }

    async fn write_file(
        &self,
        path: &str,
        content: FileContent,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        let resolved = resolve_path(self.cwd(), path);
        let signal = context.abort_signal();
        if let Some(result) = abort_result::<()>(&signal, Some(&resolved)) {
            return result;
        }
        // TS 在这一层先 `mkdir(parent, { recursive: true })`，所以写文件不要求父目录
        // 已存在；`create_dir_all` 对已存在的父目录是成功的。
        let parent = platform_dirname(&resolved);
        if let Err(error) = tokio::fs::create_dir_all(&parent).await {
            return Err(to_file_error_io(&error, "mkdir", Some(&parent)));
        }
        if let Some(result) = abort_result::<()>(&signal, Some(&resolved)) {
            return result;
        }
        let bytes = match content {
            FileContent::Text(text) => text.into_bytes(),
            FileContent::Bytes(bytes) => bytes,
        };
        // `writeFile(path, data, {signal})`：打开失败带 `open '<path>'`，写入失败是句柄上的
        // `write`（消息不带路径，`path` 字段用 fallback 路径）。
        let write = async {
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&resolved)
                .await
                .map_err(|error| to_file_error_io(&error, "open", Some(&resolved)))?;
            file.write_all(&bytes)
                .await
                .map_err(|error| to_file_error_io_fd(&error, "write", Some(&resolved)))?;
            Ok(())
        };
        cancellable_io(write, &signal, &resolved).await
    }

    async fn append_file(
        &self,
        path: &str,
        content: FileContent,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        let resolved = resolve_path(self.cwd(), path);
        let signal = context.abort_signal();
        if let Some(result) = abort_result::<()>(&signal, Some(&resolved)) {
            return result;
        }
        let parent = platform_dirname(&resolved);
        if let Err(error) = tokio::fs::create_dir_all(&parent).await {
            return Err(to_file_error_io(&error, "mkdir", Some(&parent)));
        }
        if let Some(result) = abort_result::<()>(&signal, Some(&resolved)) {
            return result;
        }
        let bytes = match content {
            FileContent::Text(text) => text.into_bytes(),
            FileContent::Bytes(bytes) => bytes,
        };
        let opened = tokio::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&resolved)
            .await;
        match opened {
            Ok(mut file) => {
                if let Err(error) = file.write_all(&bytes).await {
                    return Err(to_file_error_io_fd(&error, "write", Some(&resolved)));
                }
                // `tokio::fs::File` 的写入是缓冲的；`Task::appendFile` 在返回时数据已经可见。
                if let Err(error) = file.flush().await {
                    return Err(to_file_error_io_fd(&error, "write", Some(&resolved)));
                }
                match abort_result::<()>(&signal, Some(&resolved)) {
                    Some(result) => result,
                    None => Ok(()),
                }
            }
            Err(error) => Err(to_file_error_io(&error, "open", Some(&resolved))),
        }
    }

    async fn truncate_file(
        &self,
        path: &str,
        size: u64,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        let resolved = resolve_path(self.cwd(), path);
        let signal = context.abort_signal();
        if let Some(result) = abort_result::<()>(&signal, Some(&resolved)) {
            return result;
        }
        // `r+` 语义：文件必须已存在（缺失是 `open ... ENOENT`），不会顺手创建。上限与
        // TS 的 `Number.isSafeInteger` 对齐；`u64` 无法表达负数，所以只查上界。
        if size > MAX_SAFE_INTEGER {
            return Err(FileError::new(
                FileErrorCode::Invalid,
                "File size must be a non-negative safe integer",
            )
            .with_path(&resolved));
        }
        let file = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&resolved)
            .await;
        let file = match file {
            Ok(file) => file,
            Err(error) => return Err(to_file_error_io(&error, "open", Some(&resolved))),
        };
        let truncate = file.set_len(size).await;
        let outcome = match truncate {
            Ok(()) => match abort_result::<()>(&signal, Some(&resolved)) {
                Some(result) => result,
                None => Ok(()),
            },
            Err(error) => Err(to_file_error_io_fd(&error, "ftruncate", Some(&resolved))),
        };
        drop(file);
        outcome
    }

    async fn flush_file(&self, path: &str, context: &dyn Context) -> Result<(), FileError> {
        let resolved = resolve_path(self.cwd(), path);
        let signal = context.abort_signal();
        if let Some(result) = abort_result::<()>(&signal, Some(&resolved)) {
            return result;
        }
        let file = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&resolved)
            .await;
        let file = match file {
            Ok(file) => file,
            Err(error) => return Err(to_file_error_io(&error, "open", Some(&resolved))),
        };
        let outcome = match file.metadata().await {
            Ok(metadata) if metadata.is_dir() => {
                // POSIX 拒绝以写方式打开目录；Windows 会打开，所以显式检查。
                Err(FileError::new(FileErrorCode::IsDirectory, "Is a directory")
                    .with_path(&resolved))
            }
            Ok(_) => match file.sync_all().await {
                Ok(()) => match abort_result::<()>(&signal, Some(&resolved)) {
                    Some(result) => result,
                    None => Ok(()),
                },
                Err(error) => Err(to_file_error_io_fd(&error, "fsync", Some(&resolved))),
            },
            Err(error) => Err(to_file_error_io_fd(&error, "fstat", Some(&resolved))),
        };
        drop(file);
        outcome
    }

    async fn rename_file(
        &self,
        source_path: &str,
        destination_path: &str,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        let source = resolve_path(self.cwd(), source_path);
        let destination = resolve_path(self.cwd(), destination_path);
        if let Some(result) = abort_result::<()>(&context.abort_signal(), Some(&destination)) {
            return result;
        }
        match tokio::fs::rename(&source, &destination).await {
            Ok(()) => Ok(()),
            // Node 的双路径消息：`rename '<source>' -> '<destination>'`。
            Err(error) => Err(to_file_error_io_between(
                &error,
                "rename",
                &source,
                &destination,
            )),
        }
    }

    async fn file_info(&self, path: &str, context: &dyn Context) -> Result<FileInfo, FileError> {
        let resolved = resolve_path(self.cwd(), path);
        let signal = context.abort_signal();
        if let Some(result) = abort_result::<FileInfo>(&signal, Some(&resolved)) {
            return result;
        }
        match tokio::fs::symlink_metadata(&resolved).await {
            Ok(metadata) => file_info_from_metadata(&resolved, &metadata),
            Err(error) => Err(to_file_error_io(&error, "lstat", Some(&resolved))),
        }
    }

    async fn list_dir(
        &self,
        path: &str,
        context: &dyn Context,
    ) -> Result<Vec<FileInfo>, FileError> {
        let resolved = resolve_path(self.cwd(), path);
        let signal = context.abort_signal();
        if let Some(result) = abort_result::<Vec<FileInfo>>(&signal, Some(&resolved)) {
            return result;
        }
        // 与 `NodeDirReader::next` 的差异：这里任何一条 `lstat` 失败（包括枚举与
        // lstat 之间被删的 ENOENT）都会让整个列表失败；不支持的条目类型仍被跳过。
        let mut directory = match tokio::fs::read_dir(&resolved).await {
            Ok(directory) => directory,
            Err(error) => return Err(to_file_error_io(&error, "scandir", Some(&resolved))),
        };
        let mut infos: Vec<FileInfo> = Vec::new();
        loop {
            let entry = match directory.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(error) => return Err(to_file_error_io(&error, "scandir", Some(&resolved))),
            };
            if let Some(result) = abort_result::<Vec<FileInfo>>(&signal, Some(&resolved)) {
                return result;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let entry_path = platform_resolve(&[&resolved, &name], &process_cwd());
            match tokio::fs::symlink_metadata(&entry_path).await {
                Ok(metadata) => {
                    if let Ok(info) = file_info_from_metadata(&entry_path, &metadata) {
                        infos.push(info);
                    }
                }
                Err(error) => return Err(to_file_error_io(&error, "lstat", Some(&entry_path))),
            }
        }
        Ok(infos)
    }

    // TS 的 `NodeExecutionEnv` 没有 `listFiles`；trait 的默认实现返回同样的
    // `not_supported` + "listFiles is not supported by this environment"。

    async fn open_dir_reader(
        &self,
        path: &str,
        context: &dyn Context,
    ) -> Result<Arc<dyn DirReader>, FileError> {
        let resolved = resolve_path(self.cwd(), path);
        let signal = context.abort_signal();
        if let Some(result) = abort_result::<Arc<dyn DirReader>>(&signal, Some(&resolved)) {
            return result;
        }
        match tokio::fs::read_dir(&resolved).await {
            Ok(directory) => {
                if let Some(result) = abort_result::<Arc<dyn DirReader>>(&signal, Some(&resolved)) {
                    drop(directory);
                    return result;
                }
                Ok(Arc::new(NodeDirReader::new(directory, resolved)))
            }
            // TS 用 `opendir`。
            Err(error) => Err(to_file_error_io(&error, "opendir", Some(&resolved))),
        }
    }

    async fn watch(
        &self,
        targets: &[WatchTarget],
        on_change: WatchCallback,
        context: &dyn Context,
    ) -> Result<Arc<dyn FileWatcher>, FileError> {
        if let Some(result) = abort_result::<Arc<dyn FileWatcher>>(&context.abort_signal(), None) {
            return result;
        }
        // 解析器捕获的是**创建 watcher 时**的 cwd：之后再 `set_cwd` 不会改变已有
        // watcher 观察的目标，新目标需要重新调用 `watch`。
        let cwd = self.cwd().to_owned();
        let resolver = Arc::new(move |path: &str| resolve_path(&cwd, path));
        let watcher =
            match NodeFileWatcher::open(targets, resolver, on_change, &self.watch_options).await {
                Ok(watcher) => watcher,
                Err(error) => return Err(error),
            };
        if let Some(result) = abort_result::<Arc<dyn FileWatcher>>(&context.abort_signal(), None) {
            watcher.close(context).await;
            return result;
        }
        Ok(Arc::new(watcher))
    }

    async fn canonical_path(&self, path: &str, context: &dyn Context) -> Result<String, FileError> {
        let resolved = resolve_path(self.cwd(), path);
        let signal = context.abort_signal();
        if let Some(result) = abort_result::<String>(&signal, Some(&resolved)) {
            return result;
        }
        match tokio::fs::canonicalize(&resolved).await {
            Ok(path) => Ok(path.to_string_lossy().into_owned()),
            Err(error) => Err(to_file_error_io(&error, "realpath", Some(&resolved))),
        }
    }

    async fn exists(&self, path: &str, context: &dyn Context) -> Result<bool, FileError> {
        // 由 `file_info`（lstat）实现：只有 `not_found` 映射为 false，权限不足等错误
        // 照常向上返回，不会把“读不到”误报成“不存在”。
        match self.file_info(path, context).await {
            Ok(_) => Ok(true),
            Err(error) if error.code == FileErrorCode::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    async fn create_dir(
        &self,
        path: &str,
        options: Option<&CreateDirOptions>,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        let resolved = resolve_path(self.cwd(), path);
        let signal = context.abort_signal();
        if let Some(result) = abort_result::<()>(&signal, Some(&resolved)) {
            return result;
        }
        // `recursive` 默认 true（TS: `options?.recursive ?? true`），因此默认会补建
        // 缺失的父目录；`create_dir` 在目标已存在时报 EEXIST，`create_dir_all` 不报。
        let recursive = options
            .and_then(|options| options.recursive)
            .unwrap_or(true);
        let outcome = if recursive {
            tokio::fs::create_dir_all(&resolved).await
        } else {
            tokio::fs::create_dir(&resolved).await
        };
        match outcome {
            Ok(()) => Ok(()),
            Err(error) => Err(to_file_error_io(&error, "mkdir", Some(&resolved))),
        }
    }

    async fn remove(
        &self,
        path: &str,
        options: Option<&RemoveOptions>,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        let resolved = resolve_path(self.cwd(), path);
        let signal = context.abort_signal();
        if let Some(result) = abort_result::<()>(&signal, Some(&resolved)) {
            return result;
        }
        // 默认 `recursive = false`、`force = false`（与 `fs.rm` 一致）。注意目录在
        // 非递归模式下即使 `force: true` 也报错，理由见 `remove_path` 的文档。
        let recursive = options
            .and_then(|options| options.recursive)
            .unwrap_or(false);
        let force = options.and_then(|options| options.force).unwrap_or(false);
        remove_path(&resolved, recursive, force).await
    }

    async fn create_temp_dir(
        &self,
        prefix: Option<&str>,
        context: &dyn Context,
    ) -> Result<String, FileError> {
        if let Some(result) = abort_result::<String>(&context.abort_signal(), None) {
            return result;
        }
        let prefix = prefix.unwrap_or("tmp-");
        create_temp_dir_in(&temp_dir(), prefix)
    }

    async fn create_temp_file(
        &self,
        options: Option<&CreateTempFileOptions>,
        context: &dyn Context,
    ) -> Result<String, FileError> {
        // 先用固定的 `tmp-` 前缀建一个唯一目录，再在目录里建文件。所以
        // 返回路径的父目录一定是新目录，而不是系统 tmpdir 本身；文件名由 UUID v4 拼成。
        let directory = self.create_temp_dir(Some("tmp-"), context).await?;
        let prefix = options
            .and_then(|options| options.prefix.as_deref())
            .unwrap_or("");
        let suffix = options
            .and_then(|options| options.suffix.as_deref())
            .unwrap_or("");
        let name = format!("{prefix}{}{suffix}", uuid::Uuid::new_v4());
        let file_path = platform_join(&[&directory, &name]);
        // TS 用 `writeFile(filePath, "")`。
        match tokio::fs::File::create(&file_path).await {
            Ok(_file) => Ok(file_path),
            Err(error) => Err(to_file_error_io(&error, "open", Some(&file_path))),
        }
    }

    async fn cleanup(&self, context: &dyn Context) {
        NodeExecutionEnv::cleanup(self, context).await;
    }
}

/// `mkdtemp(join(base, prefix))`。
///
/// `join` 先归一化（`""`/`"."`/`"a/.."` 都变成 base 本身），`mkdtemp` 再把 6 个随机字符
/// **追加到这个字符串**上，而不是加一个新路径段。TS 用的是 `node:fs/promises` 的异步
/// `mkdtemp`：失败消息里的路径是加过随机字符的候选路径（同步版本才会报字面 `XXXXXX`）。
///
/// 只对 `AlreadyExists` 更换随机后缀重试，最多 100 次；其他错误立即返回。100 次都
/// 冲突时返回 `unknown` 错误，消息里带最后一个候选路径（但该错误不设置 `path` 字段）。
///
/// 注意：这里把候选路径同时填进 `FileError.path`；`spec/durable-rest.md` §2.23 把 TS 的
/// `createTempDir` 记为 `toFileError(error)`（不带 fallback path，spec 描述其无 path）。
/// 依赖该字段做对齐的调用方需要留意这处可能的差异。
fn create_temp_dir_in(base: &str, prefix: &str) -> Result<String, FileError> {
    let template = platform_join(&[base, prefix]);
    let mut candidate = String::new();
    for _ in 0..100 {
        candidate = format!("{template}{}", mkdtemp_suffix());
        match create_private_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(to_file_error_io(&error, "mkdtemp", Some(&candidate))),
        }
    }
    Err(FileError::new(
        FileErrorCode::Unknown,
        format!("Failed to create a unique temporary directory: mkdtemp '{candidate}'"),
    ))
}

/// libuv `mkdtemp` 用于替换模板 `XXXXXX` 的字符表（`[a-zA-Z0-9]`）。
///
/// 随机源取 UUID v4 的前 6 个字节，再按 `byte % 62` 映射到字符表；字符集与 libuv 一致，
/// 但随机源是本实现的 UUID v4，不要求与 libuv 生成的随机字符相同。
fn mkdtemp_suffix() -> String {
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    uuid::Uuid::new_v4()
        .as_bytes()
        .iter()
        .take(6)
        .map(|byte| LETTERS[*byte as usize % LETTERS.len()] as char)
        .collect()
}

/// `fs.mkdtemp` 以 0o700 创建目录：否则按 umask（常见 0755/0770）创建，spill 的原始 shell
/// 输出可能被其他本地用户读取。
fn create_private_dir(path: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir(path)
    }
}

/// `rm(path, { recursive, force })` 的等价实现。
///
/// `fs.rm` 对目录（**包括空目录**）在 `recursive: false` 时总是以 `ERR_FS_EISDIR` 失败，
/// `force` 也不改变这一点；错误码 `ERR_FS_EISDIR` 不在 `toFileError` 的映射表里，因此
/// `FileError.code` 是 `unknown`。
async fn remove_path(path: &str, recursive: bool, force: bool) -> Result<(), FileError> {
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if force && error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(to_file_error_io(&error, "lstat", Some(path))),
    };
    if !recursive {
        if metadata.is_dir() {
            return Err(FileError::new(
                FileErrorCode::Unknown,
                format!("Path is a directory: rm returned EISDIR (is a directory) {path}"),
            )
            .with_path(path));
        }
        return match tokio::fs::remove_file(path).await {
            Ok(()) => Ok(()),
            Err(error) if force && error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(to_file_error_io(&error, "unlink", Some(path))),
        };
    }
    remove_tree(path).await
}

/// `rm(recursive: true)`：后序遍历（先子项、后目录），错误带实际的 syscall 与路径，
/// 与 Node 的 rimraf 一致；遍历中消失的条目视为已删除。
async fn remove_tree(root: &str) -> Result<(), FileError> {
    // 用显式栈而不是递归：目录深度/条目数没有上限，递归有爆栈风险。栈元素是
    // (路径, 是否已展开子项)：目录第一次弹出时先把自己以 expanded=true 压回，再压
    // 子项，保证 `rmdir` 一定在子项删除之后执行。
    let mut stack: Vec<(String, bool)> = vec![(root.to_owned(), false)];
    while let Some((path, expanded)) = stack.pop() {
        if expanded {
            match tokio::fs::remove_dir(&path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(to_file_error_io(&error, "rmdir", Some(&path))),
            }
            continue;
        }
        let metadata = match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(to_file_error_io(&error, "lstat", Some(&path))),
        };
        if !metadata.is_dir() {
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(to_file_error_io(&error, "unlink", Some(&path))),
            }
            continue;
        }
        stack.push((path.clone(), true));
        let mut entries = match tokio::fs::read_dir(&path).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(to_file_error_io(&error, "scandir", Some(&path))),
        };
        loop {
            match entries.next_entry().await {
                Ok(Some(entry)) => {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    stack.push((platform_resolve(&[&path, &name], &process_cwd()), false));
                }
                Ok(None) => break,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => return Err(to_file_error_io(&error, "scandir", Some(&path))),
            }
        }
    }
    Ok(())
}

/// `openFile(resolved, flags)` 的 POSIX/Windows 分支。
///
/// POSIX 用同步的 `std::fs::OpenOptions` 打开再转成 `tokio::fs::File`：这样能直接传
/// `O_NONBLOCK`/`O_NOFOLLOW`；`O_NONBLOCK` 保证打开 FIFO 不会等待写端。Windows 没有
/// 这两个标志，走 `tokio::fs::File::open`，`no_follow` 退化为打开前的 `lstat` 检查。
async fn open_binary_file(path: &str, no_follow: bool) -> Result<tokio::fs::File, FileError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // `O_NONBLOCK` 让打开 FIFO 不等待写端；`O_NOFOLLOW` 拒绝最后一段是符号链接。
        let mut flags = libc::O_RDONLY | libc::O_NONBLOCK;
        if no_follow {
            flags |= libc::O_NOFOLLOW;
        }
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(flags)
            .open(path);
        match opened {
            Ok(file) => Ok(tokio::fs::File::from_std(file)),
            Err(error) => {
                // TS 用 `error.code === "ELOOP" || error.code === "EMLINK"` 判定；libuv 的 code
                // 来自平台 errno，因此必须比较本平台的常量（Linux 40/31，macOS/BSD 62/31）。
                if no_follow
                    && matches!(error.raw_os_error(), Some(code) if code == libc::ELOOP || code == libc::EMLINK)
                {
                    let cause: Arc<dyn std::error::Error + Send + Sync> =
                        Arc::new(std::io::Error::new(error.kind(), error.to_string()));
                    return Err(symlink_refused(path, Some(cause)));
                }
                Err(to_file_error_io(&error, "open", Some(path)))
            }
        }
    }
    #[cfg(not(unix))]
    {
        // Windows 没有 O_NOFOLLOW：打开前检查最后一段（源码注释明言非竞态安全）。
        if no_follow {
            match tokio::fs::symlink_metadata(path).await {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(symlink_refused(path, None));
                }
                Ok(_) => {}
                Err(error) => return Err(to_file_error_io(&error, "lstat", Some(path))),
            }
        }
        match tokio::fs::File::open(path).await {
            Ok(file) => Ok(file),
            Err(error) => Err(to_file_error_io(&error, "open", Some(path))),
        }
    }
}

// ---------------------------------------------------------------------------
// Shell
// ---------------------------------------------------------------------------

// `Shell` 的两个方法都只是转发：`exec` 的全部逻辑在 `exec_impl`，`cleanup` 与
// `FileSystem::cleanup` 共用同一个 inherent 方法。
#[async_trait]
impl Shell for NodeExecutionEnv {
    async fn exec(
        &self,
        command: ShellCommand,
        options: Option<&ShellExecOptions>,
        context: &dyn Context,
    ) -> Result<ShellExecResult, ExecutionError> {
        self.exec_impl(command, options, context).await
    }

    async fn cleanup(&self, context: &dyn Context) {
        NodeExecutionEnv::cleanup(self, context).await;
    }
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

// 这里的单测只覆盖能直接调用的内部辅助函数：WSL shell 判定、tmpdir 顺序、Node 错误
// 文案、`resolvePath` 的 file URL 回退、mkdtemp 语义，以及用私有 hook
// (`spill_temp_file`) 注入 spill 打开失败。跨平台分支用 `*_with` 变体注入 SystemRoot；
// 进程、文件系统与 spill 的端到端行为在 `tests/env_node.rs`、`tests/env_node_spill.rs`
// 与 `tests/env_node_conformance.rs`。
#[cfg(test)]
mod node_tests {
    use super::*;
    use kiana_chord::context::BACKGROUND_CONTEXT;

    fn context() -> &'static dyn Context {
        &**BACKGROUND_CONTEXT
    }

    /// TS: `selects command transport for %s (SystemRoot=%j)` 的 5 个参数组合。
    ///
    /// TS 用 `Object.defineProperty(process, "platform", "win32")` 打桩后才走到这里；Rust 的
    /// `cfg!(windows)` 在编译期固定，因此直接测判定函数（同样的输入、同样的结果）。
    #[test]
    fn selects_command_transport_for_legacy_wsl_bash_paths() {
        let cases: [(&str, Option<&str>, bool); 5] = [
            ("C:\\Windows\\System32\\bash.exe", None, true),
            ("D:\\WinNT\\System32\\bash.exe", Some("D:\\WinNT"), true),
            ("D:\\WinNT\\Sysnative\\bash.exe", Some("D:\\WinNT"), true),
            ("D:\\WinNT\\System32\\bash.exe", Some(""), false),
            ("E:\\Windows\\System32\\bash.exe", Some("D:\\WinNT"), false),
        ];
        for (path, system_root, expected) in cases {
            assert_eq!(
                is_legacy_wsl_bash_path_with(path, system_root),
                expected,
                "isLegacyWslBashPath({path:?}, SystemRoot={system_root:?})"
            );
        }
    }

    /// 非 WSL 的 bash 走 `-c`（argv 传输）。
    #[test]
    fn plain_bash_paths_use_the_argv_transport() {
        let config = get_bash_shell_config("/bin/bash");
        assert_eq!(config.args, vec!["-c".to_owned()]);
        assert_eq!(config.transport, CommandTransport::Argv);
    }

    /// TS 的 `FailingSpillExecutionEnv`：`createTempFile({prefix: "kiana-output-"})` 返回一个不
    /// 存在目录下的路径，spill 打开失败后 `exec` 必须报错而不是静默丢失 spill。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fails_rather_than_silently_losing_a_requested_spill() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!(
            "kiana-durable-env-failing-spill-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create temp dir");
        let cwd = root.to_string_lossy().into_owned();
        let missing = format!("{cwd}/missing/spill.log");

        let mut env = NodeExecutionEnv::new(cwd);
        let spill_path = missing.clone();
        env.spill_temp_file = Some(Arc::new(move || Ok(spill_path.clone())));

        let result = env
            .exec(
                ShellCommand::Shell("printf 12345678901234567890".to_owned()),
                Some(&ShellExecOptions {
                    spill: Some(crate::env::ShellSpillOptions {
                        after_bytes: 10,
                        after_lines: 10,
                    }),
                    ..Default::default()
                }),
                context(),
            )
            .await;
        let _ = std::fs::remove_dir_all(&root);

        let error = match result {
            Ok(_) => panic!("a failed spill must fail the command"),
            Err(error) => error,
        };
        assert_eq!(error.code, ExecutionErrorCode::Unknown);
        // Node 的 `createWriteStream` 在这里是 `fs.open` 失败：消息带 "open '<path>'"。
        assert_eq!(
            error.message,
            format!(
                "Failed to preserve complete shell output: ENOENT: no such file or directory, open '{missing}'"
            )
        );
    }

    /// TS 的 5 个 WSL 判定还决定 `getBashShellConfig` 的 `-s`（stdin 传输）与 `-c`（argv）。
    #[test]
    fn legacy_wsl_bash_paths_run_commands_over_stdin() {
        let wsl = get_bash_shell_config("C:\\Windows\\System32\\bash.exe");
        assert_eq!(wsl.args, vec!["-s".to_owned()]);
        assert_eq!(wsl.transport, CommandTransport::Stdin);
        let plain = get_bash_shell_config("/bin/bash");
        assert_eq!(plain.args, vec!["-c".to_owned()]);
        assert_eq!(plain.transport, CommandTransport::Argv);
    }

    /// `uv_os_tmpdir` 的变量顺序（POSIX：TMPDIR → TMP → TEMP → /tmp，空值跳过）。
    #[test]
    fn tmpdir_follows_libuvs_environment_order() {
        let resolve = |pairs: &[(&str, &str)]| {
            let pairs: Vec<(String, String)> = pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect();
            temp_dir_from_env(move |name| {
                pairs
                    .iter()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value.clone())
            })
        };
        assert_eq!(
            resolve(&[("TMPDIR", "/a"), ("TMP", "/b"), ("TEMP", "/c")]),
            "/a"
        );
        assert_eq!(resolve(&[("TMP", "/b"), ("TEMP", "/c")]), "/b");
        assert_eq!(resolve(&[("TEMP", "/c")]), "/c");
        assert_eq!(
            resolve(&[]),
            if cfg!(windows) {
                "C:\\Windows\\Temp"
            } else {
                "/tmp"
            }
        );
        assert_eq!(
            resolve(&[("TMPDIR", "")]),
            if cfg!(windows) {
                "C:\\Windows\\Temp"
            } else {
                "/tmp"
            }
        );
    }

    /// Node 的 fs 错误消息：`<CODE>: <description>, <syscall> '<path>'`。
    #[test]
    fn formats_io_errors_like_node() {
        let missing = std::io::Error::from_raw_os_error(libc::ENOENT);
        assert_eq!(
            node_io_message(&missing, "lstat", Some("/x"), None),
            "ENOENT: no such file or directory, lstat '/x'"
        );
        assert_eq!(
            node_io_message(&missing, "rename", Some("/a"), Some("/b")),
            "ENOENT: no such file or directory, rename '/a' -> '/b'"
        );
        assert_eq!(
            node_io_message(&missing, "fstat", None, None),
            "ENOENT: no such file or directory, fstat"
        );
    }

    /// TS 的 flush 测试注入 `EIO: i/o error, fsync`：`EIO` 不在 `toFileError` 的映射表里，
    /// 因此 code 是 `unknown`，消息是 Node 的文本（句柄上的 syscall 不带路径）。
    #[test]
    fn maps_a_flush_failure_like_node() {
        let error = std::io::Error::from_raw_os_error(libc::EIO);
        let file_error = to_file_error_io_fd(&error, "fsync", Some("/tmp/file.txt"));
        assert_eq!(file_error.code, FileErrorCode::Unknown);
        assert_eq!(file_error.message, "EIO: i/o error, fsync");
        assert_eq!(file_error.path.as_deref(), Some("/tmp/file.txt"));
    }

    /// Node 只把 EACCES/EAGAIN/EMFILE/ENFILE/ENOENT 作为异步 `error` 事件（`spawn <file>
    /// <CODE>`）；其余错误（如 cwd 不是目录时的 ENOTDIR）同步抛出 `spawn <CODE>`。
    #[test]
    fn formats_spawn_errors_like_node() {
        let async_codes = [
            libc::EACCES,
            libc::EAGAIN,
            libc::EMFILE,
            libc::ENFILE,
            libc::ENOENT,
        ];
        for raw in async_codes {
            let error = std::io::Error::from_raw_os_error(raw);
            let (code, _) = node_errno(&error).expect("async code is a libuv code");
            assert_eq!(
                spawn_error_message("/bin/bash", &error),
                format!("spawn /bin/bash {code}")
            );
        }
        let sync_error = std::io::Error::from_raw_os_error(libc::ENOTDIR);
        assert_eq!(
            spawn_error_message("/bin/bash", &sync_error),
            "spawn ENOTDIR"
        );
    }

    /// `resolvePath` 在环境 cwd 相对时以 `process.cwd()` 为起点，而不是把相对 cwd
    /// 再前置一次。
    #[test]
    fn resolves_a_relative_cwd_against_the_process_cwd() {
        let process_cwd = std::env::current_dir().expect("process cwd");
        let process_cwd = process_cwd.to_string_lossy().into_owned();
        assert_eq!(
            resolve_path("relative/dir", "x.txt"),
            format!("{process_cwd}/relative/dir/x.txt")
        );
        assert_eq!(resolve_path(".", "x.txt"), format!("{process_cwd}/x.txt"));
        assert_eq!(resolve_path(".", "."), process_cwd);
    }

    /// TS 的 `fileURLToPath` 失败时 `catch` 保留原字符串，于是它作为**相对路径**解析：
    /// 编码的 `/`、非法百分号转义、非法 UTF-8 与非法宿主都必须保留原串。
    #[test]
    fn keeps_file_urls_that_node_rejects_as_relative_paths() {
        for (raw, expected) in [
            ("file:///tmp/a%2Fb", "/base/file:/tmp/a%2Fb"),
            ("file:///tmp/a%2fb", "/base/file:/tmp/a%2fb"),
            ("file:///tmp/%zz", "/base/file:/tmp/%zz"),
            ("file:///tmp/%", "/base/file:/tmp/%"),
            ("file:///tmp/%FF", "/base/file:/tmp/%FF"),
            ("file://host/tmp/x", "/base/file:/host/tmp/x"),
        ] {
            assert_eq!(resolve_path("/base", raw), expected, "raw: {raw}");
        }
        // 合法的 file URL 仍按 fileURLToPath 的百分号解码结果解析。
        assert_eq!(resolve_path("/base", "file:///tmp/a%20b"), "/tmp/a b");
        assert_eq!(resolve_path("/base", "file://localhost/tmp/x"), "/tmp/x");
        assert_eq!(resolve_path("/base", "file:///tmp/%C3%A9"), "/tmp/é");
        assert_eq!(resolve_path("/base", "file:///tmp/a%3Fb"), "/tmp/a?b");
        assert_eq!(resolve_path("/base", "file:///tmp/%252F"), "/tmp/%2F");
        assert_eq!(resolve_path("/base", "file:///tmp/a%2Eb"), "/tmp/a.b");
        assert_eq!(resolve_path("/base", "file:///tmp/a/../b"), "/tmp/b");
        assert_eq!(resolve_path("/base", "file:///tmp/a%5Cb"), "/tmp/a\\b");
        assert_eq!(resolve_path("/base", "file://"), "/");
        assert_eq!(resolve_path("/base", "file:///"), "/");
        assert_eq!(resolve_path("/base", "file:////x"), "/x");
    }

    /// Node 是 `mkdtemp(join(base, prefix))`：随机字符追加到 `join` 的结果上，而不是新路径段。
    #[test]
    fn mkdtemp_appends_random_characters_to_the_joined_prefix() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!(
            "kiana-durable-env-mkdtemp-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create temp dir");
        let base = root.to_string_lossy().into_owned();

        // `path.join(base, "")` 是 base 本身；6 个随机字符直接追加，创建的是 base 的**兄弟**。
        let created = create_temp_dir_in(&base, "").expect("mkdtemp");
        assert_eq!(created.len(), base.len() + 6, "created: {created}");
        assert!(created.starts_with(&base), "created: {created}");
        assert_eq!(
            std::path::Path::new(&created).parent(),
            std::path::Path::new(&base).parent(),
            "created: {created}"
        );
        // 普通前缀：`join` 加分隔符，随机字符仍直接追加。
        let inner = create_temp_dir_in(&base, "abc-").expect("mkdtemp");
        assert_eq!(
            std::path::Path::new(&inner).parent(),
            Some(std::path::Path::new(&base)),
            "created: {inner}"
        );
        assert_eq!(inner.len(), base.len() + 1 + 4 + 6);
        let _ = std::fs::remove_dir(&created);
        let _ = std::fs::remove_dir(&inner);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `mkdtemp` 的错误消息带**追加随机字符后的候选路径**（TS 用异步 API；同步 API 才报
    /// 字面 `XXXXXX`），与 Node 一致。
    #[test]
    fn mkdtemp_errors_report_the_candidate_path() {
        let missing = format!(
            "/nonexistent-kiana-env-parent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        );
        let error = create_temp_dir_in(&missing, "tmp-").expect_err("missing parent must fail");
        let prefix = format!("{missing}/tmp-");
        let path = error.path.expect("candidate path");
        assert_eq!(error.code, FileErrorCode::NotFound);
        assert_eq!(
            error.message,
            format!("ENOENT: no such file or directory, mkdtemp '{path}'")
        );
        assert!(path.starts_with(&prefix), "path: {path}");
        assert_eq!(path.len(), prefix.len() + 6, "path: {path}");
    }
}
