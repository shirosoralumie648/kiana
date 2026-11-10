//! utils/child-process.ts。
//!
//! 逐字移植 `packages/coding-agent/src/utils/child-process.ts`（137 行）与
//! `spec/ca-cli.md` §2.21.18 / `spec/ca-tools-compaction-export.md` §2.28.3 的契约。
//!
//! ## 平台
//!
//! - Unix：`spawn_process` 直接用 `tokio::process::Command`，`spawn_process_sync` 用
//!   `std::process::Command`（等价 TS 的非 win32 分支）。`detached: true` 用 `setsid()`
//!   复刻 Node 的 `detached`（新会话），不是单纯的 `setpgid`。
//! - Windows：复刻 `cross-spawn`（`npm`/`pnpm`/`yarn`/`bun` 在 Windows 上是 `.cmd` shim）：
//!   PATHEXT 解析 + shebang 探测 + `.cmd`/`.bat`/非 `.exe` 走
//!   `cmd.exe /d /s /c "<escaped command line>"`，引号规则照搬
//!   `cross-spawn/lib/util/escape.js`。按 `ARCHITECTURE.md` §8 第 26 条，Windows 分支只保证
//!   `cargo check --target x86_64-pc-windows-msvc` 通过，不进入黄金测试矩阵。
//!
//! ## spawn 错误与 spawnSync 超时
//!
//! - `error.message` 与 Node/libuv 一致，以真实 errno 名结尾（`ENOENT`/`EACCES`/`ENOTDIR`…），
//!   不把 `io::ErrorKind` 之外的错误一律写成 `UNKNOWN`。
//! - 空命令名：Node 在 `spawn`/`spawnSync` 里同步抛
//!   `TypeError [ERR_INVALID_ARG_VALUE]`（message 为
//!   `The argument 'file' cannot be empty. Received ''`）。冻结的 Rust 签名不能抛错，
//!   `spawn_process` 返回 `Err`、`spawn_process_sync` 放进 `error` 字段；两处共用同一条消息文本（稳定契约，改动需同步测试）。
//! - 空 `cwd`：Node 的**异步** `spawn` 把 `cwd: ""` 当作未设置（在进程 cwd 里运行），
//!   而 `spawnSync` 会以 `ENOENT` 失败——两处分别复刻（`exec_command` 走异步语义）。
//! - `spawn_process_sync` 的 `timeout` 与 Node 相同：超时瞬间发送默认 `killSignal`（SIGTERM）
//!   **并销毁 stdio**（读端关闭，之后的输出被丢弃、写入方收到 EPIPE），然后继续等待子进程
//!   退出；如果子进程在超时前已经退出，`status` 保留其退出码（Node 实测 `status:0` +
//!   `error:ETIMEDOUT`），否则 `status` 为 `None`（即使它后来自行退出）。
//!
//! ## `wait_for_child_process` 与 #5303
//!
//! TS 的 `waitForChildProcess` 监听 `exit`/`close`/`end`/`data`：
//!
//! - `close`（进程退出且所有 stdio 关闭）→ 立即 resolve；
//! - `exit` 后两个流都 `end` → 立即 resolve；
//! - `exit` 后仍有 `data` → 重置 100ms 空闲计时器（`EXIT_STDIO_GRACE_MS`），超时才
//!   finalize 并 `destroy()` 两个流（earendil-works/kiana#5303：孙进程持有继承的 stdout 时
//!   不能按固定 deadline 截断尾部输出）。
//!
//! `tokio::process::Child::wait()` **只**等进程退出（不等 stdio 关闭，这与 planner 注记里的
//! 假设不同），所以 Rust 侧显式读这两个流：`wait_for_child_process` 会把 `child.stdout` /
//! `child.stderr` 取走并排空（调用方若需要输出，必须先自己 `take()` 流；此时该流按
//! TS 的 `child.stdout === null` 语义视为已结束）。
//!
//! `wait_for_child_process_parts` 是需要从 `tests/` 驱动的测试缝（duplex/mock reader +
//! `tokio::time::pause`）。`wait_for_child_process_parts_observed` 额外把数据块回放给回调，
//! 供 `regressions_5303` 断言“没有截断”；两者都标注 test hook、不是公开 API。
//!
//! ## 设计原因
//!
//! - 两套入口直接对应 Node 的两个 API：异步 `spawn` 立即返回句柄，同步 `spawnSync` 阻塞到
//!   进程退出。`spawn_process` 因此只创建进程并把 stdio 交给调用方；等待、超时与输出读取
//!   都由调用方或 [`wait_for_child_process`] 负责。
//! - #5303 的语义要求“退出之后仍继续读，直到连续 100ms 没有新数据”，这无法用一次
//!   `Child::wait()` 表达（它只等进程退出，不等 stdio 关闭），所以需要
//!   [`wait_for_child_process`] 里的显式读取状态机。
//! - 同步路径在 Unix 上不用读线程：`spawnSync` 的 `timeout` 要求到点立刻关闭读端、丢弃
//!   之后的输出。`PipePump` 因此由调用线程用 `O_NONBLOCK` 直接持有并轮询 fd，`destroy`
//!   只是在同一线程里 drop 读端；其它平台才退化为“读线程 + channel”。
//! - Windows 的 cross-spawn 复刻集中在 `resolve_program` 与参数转义函数；Unix 上
//!   `resolve_program` 是恒等映射（PATH 查找交给操作系统）。
//!
//! ## 执行流程（`spawn_process_sync_bytes`）
//!
//! 1. 空命令检查：不创建进程，直接把 `error` 填成 TypeError 文本；
//! 2. `resolve_program` 解析出可执行文件与参数（只有 Windows 会改写）；
//! 3. 依次设置 cwd、env、三路 stdio，Windows 再加 `CREATE_NO_WINDOW`；
//! 4. `spawn` 成功后记录 pid，并立刻丢弃 stdin 写端（Node 无 `input` 时子进程读到 EOF）；
//! 5. 轮询循环：`try_wait` → 用 `PipePump` 排空 stdout/stderr → 判断是否两个流都结束 →
//!    检查 deadline（到点则 SIGTERM + 销毁管道）；
//! 6. 汇总 `status`（是否超时决定取哪一份退出码）与 `error`（ETIMEDOUT 或 `try_wait` 错误）。
//!
//! ## 主要调用方
//!
//! - 异步 `spawn_process`：`kiana-tools` 的 bash/find/grep、`kiana-resources` 的
//!   `package/exec` 与 `footer_data_provider`、`kiana-rpc-print` 的客户端，以及
//!   `kiana-extensions` 的 JS 宿主（`loader/js_host.rs`）。
//! - 同步 `spawn_process_sync` / `spawn_process_sync_bytes`：本 crate 的 `shell.rs`、
//!   `paths.rs`、`config.rs`；`kiana-resources` 的 `bug_report` / `footer_data_provider` /
//!   `package/exec`；`kiana-interactive` 的会话选择器；`kiana-coding-agent` 的
//!   `package_manager_cli`。
//! - `wait_for_child_process`：`kiana-coding-agent/src/package_manager_cli.rs` 与
//!   `tests/child_process.rs`；需要原始 `ExitStatus`（信号编号）或流式回调时用
//!   `wait_for_child_process_observed`（bash 工具）。
//!
//! ## 注意事项
//!
//! - [`wait_for_child_process`] 会 `take()` 并排空两个管道；调用方需要输出就必须先自己
//!   `take()`，被取走的流按 TS 的 `child.stdout === null` 处理（视为已结束）。
//! - 同步路径把管道读错误当作流结束处理，不写进 `error`；`error` 只覆盖启动失败、
//!   `try_wait` 失败与超时。
//! - `wait_for_child_process_parts*` 是 `#[doc(hidden)]` 的测试缝：`Option<流>`、注入的
//!   exit future 与 `on_chunk` 回调只为 `tests/` 存在，不是稳定 API。
//! - 回归测试见 `tests/child_process.rs` 与
//!   `tests/regressions_5303_bash_output_truncation.rs`。下游复用：`kiana-tools` 的 bash
//!   工具走 [`wait_for_child_process_observed`]（同一状态机，回调转发输出），本 crate 的
//!   `exec.rs` 使用 `PipePump` 与 [`EXIT_STDIO_GRACE_MS`]。

use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};

/// `EXIT_STDIO_GRACE_MS`：`exit` 之后最后一次 `data` 起的空闲宽限。
///
/// 等待状态机在进程退出时武装它，之后每读到一块数据就重置；连续这么久没有新数据才
/// finalize 并丢弃 stdio（#5303）。`kiana-tools/src/bash.rs` 的流式等待循环与本 crate
/// `exec.rs` 的退出后宽限循环也复用该常量，修改它会同时改变三处的收尾延迟
/// （本文件 `wait_parts_core`、`exec.rs` 与 `kiana-tools/src/bash.rs`）。
pub const EXIT_STDIO_GRACE_MS: u64 = 100;

/// `stdio` 的单流模式（Node 的 `"ignore" | "pipe" | "inherit"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StdioMode {
    /// `"ignore"`：子进程拿到空设备。
    Ignore,
    /// `"pipe"`：父进程持有管道（Node 的默认值）。
    #[default]
    Pipe,
    /// `"inherit"`：继承父进程的句柄。
    Inherit,
}

fn to_stdio(mode: StdioMode) -> Stdio {
    match mode {
        StdioMode::Ignore => Stdio::null(),
        StdioMode::Pipe => Stdio::piped(),
        StdioMode::Inherit => Stdio::inherit(),
    }
}

/// `SpawnOptions`：`spawnProcess` 的选项（对应 TS 的 `SpawnOptions` 子集）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SpawnOptions {
    /// 子进程的工作目录；`None` 继承父进程 cwd。**异步路径**把 `Some("")` 当作未设置
    /// （与 Node 的 `if (options.cwd)` 一致）；同步路径的空 cwd 语义见
    /// [`SpawnSyncOptions::cwd`]。
    pub cwd: Option<PathBuf>,
    /// stdin 模式；默认 `Pipe`（Node 的默认值）。
    pub stdin: StdioMode,
    /// stdout 模式；默认 `Pipe`。
    pub stdout: StdioMode,
    /// stderr 模式；默认 `Pipe`。
    pub stderr: StdioMode,
    /// `true` 时让子进程脱离父进程：Unix 用 `setsid()`（新会话 + 新进程组）；
    /// Windows 设置 `DETACHED_PROCESS`（`0x0000_0008`）与
    /// `CREATE_NEW_PROCESS_GROUP`（`0x0000_0200`）。**不能**用 `0x0000_0002`：那是
    /// `DEBUG_ONLY_THIS_PROCESS`，会让子进程以被调试者身份启动。
    pub detached: bool,
    /// 仅 Windows 生效：设置 `CREATE_NO_WINDOW`，避免子进程弹出控制台窗口；
    /// Unix 上该字段被忽略。
    pub windows_hide: bool,
    /// 子进程环境变量（Node `spawn` 的 `env`）。`Some` 时以 `env_clear()` + 逐项设置
    /// 替换整个环境；`None` 继承宿主环境。
    ///
    /// 顺序保留 `getShellEnv()` 的 `environ` 顺序（Node 的对象键序）。
    pub env: Option<Vec<(std::ffi::OsString, std::ffi::OsString)>>,
}

impl SpawnOptions {
    /// 三路 stdio 同一种模式（TS 的 `stdio: "inherit"` / `stdio: "ignore"`）。
    pub fn with_stdio(mode: StdioMode) -> Self {
        Self {
            stdin: mode,
            stdout: mode,
            stderr: mode,
            ..Self::default()
        }
    }
}

/// `SpawnSyncOptions`：`spawnProcessSync` 的选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSyncOptions {
    /// 子进程的工作目录；`None` 继承父进程 cwd。与异步 spawn 不同，`Some("")` 会原样交给
    /// `Command::current_dir`，由操作系统报 `ENOENT`（Node `spawnSync` 的可观测行为，
    /// 见 `tests/child_process.rs` 的 `spawn_process_sync_fails_for_an_empty_cwd_like_node`）。
    pub cwd: Option<PathBuf>,
    /// stdin 模式；默认 `Ignore`（`StdioMode::Pipe` 时 spawn 后也会立刻丢弃写端，
    /// 子进程读到的仍是 EOF）。
    pub stdin: StdioMode,
    /// stdout 模式；默认 `Pipe`，字节以原始形式放进 [`SpawnSyncBytesResult::stdout`]。
    pub stdout: StdioMode,
    /// stderr 模式；默认 `Pipe`。
    pub stderr: StdioMode,
    /// 仅 Windows 生效：设置 `CREATE_NO_WINDOW`，避免子进程弹出控制台窗口。
    pub windows_hide: bool,
    /// Node 的 `timeout`（毫秒）：超时后 kill 并设置 `error`。`None` 或 `0` 表示不设超时；
    /// 超时只发送一次 SIGTERM（不是 SIGKILL）并销毁 stdio，然后继续等待子进程退出。
    /// 退出码只在超时**之前**被观察到时保留，否则 `status` 为 `None`。
    pub timeout_ms: Option<u64>,
    /// 子进程环境变量（Node `spawnSync` 的 `env`）。`Some` 时以 `env_clear()` +
    /// 逐项设置替换整个环境；`None` 继承宿主环境。
    pub env: Option<Vec<(std::ffi::OsString, std::ffi::OsString)>>,
}

impl Default for SpawnSyncOptions {
    fn default() -> Self {
        Self {
            cwd: None,
            // Node 默认是 "pipe"，但 `spawnSync` 会立刻给 stdin 一个 EOF；`Stdio::null()`
            // 的语义等价，且不会让子进程在 `spawn_process_sync` 里挂住。
            stdin: StdioMode::Ignore,
            stdout: StdioMode::Pipe,
            stderr: StdioMode::Pipe,
            windows_hide: false,
            timeout_ms: None,
            env: None,
        }
    }
}

impl SpawnSyncOptions {
    /// 三路都 `"ignore"`（`stdio: "ignore"`）。
    pub fn ignored() -> Self {
        Self {
            stdin: StdioMode::Ignore,
            stdout: StdioMode::Ignore,
            stderr: StdioMode::Ignore,
            ..Self::default()
        }
    }
}

/// `SpawnSyncResult`：`spawnSync` 的返回值。**`spawn_process_sync` 永不返回 `Err`**：
/// 启动失败时 `error` 有值、`status` 为 `None`。
#[derive(Debug)]
pub struct SpawnSyncResult {
    /// 退出码；被信号杀死时为 `None`。
    pub status: Option<i32>,
    /// 解码后的 stdout（UTF-8，非法字节替换为 U+FFFD）。
    pub stdout: String,
    /// 解码后的 stderr。
    pub stderr: String,
    /// 启动/超时错误。
    pub error: Option<io::Error>,
}

/// 原始字节版 [`SpawnSyncResult`]：`node:child_process` 的 `spawnSync` 在未指定
/// `encoding` 时返回 `Buffer`，因此不能经过 UTF-8 有损解码。
#[derive(Debug)]
pub struct SpawnSyncBytesResult {
    /// 子进程 pid（Node 的 `spawnSync().pid`；启动失败时为 0）。
    pub pid: u32,
    /// 退出码；被信号杀死时为 `None`。
    pub status: Option<i32>,
    /// 原始 stdout 字节。
    pub stdout: Vec<u8>,
    /// 原始 stderr 字节。
    pub stderr: Vec<u8>,
    /// 启动/超时错误。
    pub error: Option<io::Error>,
}

/// 把底层 `io::Error` 转成 Node 的 spawn 错误文本：`spawn <command> ENOENT` /
/// `spawnSync <command> EACCES`（`error.message` 是 TS 里唯一被读取的字段）。
///
/// Node 的 `error.message` 以 libuv 的 errno 名结尾（`ENOENT`/`EACCES`/**`ENOTDIR`** 等），
/// 不是值域有限的 `io::ErrorKind`；因此 Unix 上优先用 `raw_os_error()` 查表。
///
/// 只改写 message 并保留原 `io::ErrorKind`；原错误的 `raw_os_error()` 不会带到新错误上
/// （新值由 `io::Error::new(kind, ..)` 构造）。
pub(crate) fn node_spawn_error(command: &str, verb: &str, error: io::Error) -> io::Error {
    let code = errno_name(&error).unwrap_or("UNKNOWN");
    io::Error::new(error.kind(), format!("{verb} {command} {code}"))
}

/// 常用 errno 名称（`uv_err_name` 的子集；覆盖 spawn/chdir 会遇到的错误）。
#[cfg(unix)]
const ERRNO_NAMES: &[(i32, &str)] = &[
    (libc::E2BIG, "E2BIG"),
    (libc::EACCES, "EACCES"),
    (libc::EAGAIN, "EAGAIN"),
    (libc::EBADF, "EBADF"),
    (libc::EBUSY, "EBUSY"),
    (libc::EDOM, "EDOM"),
    (libc::EEXIST, "EEXIST"),
    (libc::EFAULT, "EFAULT"),
    (libc::EFBIG, "EFBIG"),
    (libc::EILSEQ, "EILSEQ"),
    (libc::EINTR, "EINTR"),
    (libc::EINVAL, "EINVAL"),
    (libc::EIO, "EIO"),
    (libc::EISDIR, "EISDIR"),
    (libc::ELOOP, "ELOOP"),
    (libc::EMFILE, "EMFILE"),
    (libc::EMLINK, "EMLINK"),
    (libc::ENAMETOOLONG, "ENAMETOOLONG"),
    (libc::ENFILE, "ENFILE"),
    (libc::ENODEV, "ENODEV"),
    (libc::ENOENT, "ENOENT"),
    (libc::ENOEXEC, "ENOEXEC"),
    (libc::ENOMEM, "ENOMEM"),
    (libc::ENOSPC, "ENOSPC"),
    (libc::ENOSYS, "ENOSYS"),
    (libc::ENOTDIR, "ENOTDIR"),
    (libc::ENOTEMPTY, "ENOTEMPTY"),
    (libc::ENOTTY, "ENOTTY"),
    (libc::ENXIO, "ENXIO"),
    (libc::EOVERFLOW, "EOVERFLOW"),
    (libc::EPERM, "EPERM"),
    (libc::EPIPE, "EPIPE"),
    (libc::EPROTO, "EPROTO"),
    (libc::ERANGE, "ERANGE"),
    (libc::EROFS, "EROFS"),
    (libc::ESPIPE, "ESPIPE"),
    (libc::ESRCH, "ESRCH"),
    (libc::ETIMEDOUT, "ETIMEDOUT"),
    (libc::ETXTBSY, "ETXTBSY"),
    (libc::EXDEV, "EXDEV"),
];

/// 把 `io::Error` 映射为 Node/libuv 风格的 errno 名。
///
/// Unix 上先查 `raw_os_error()` 的码表；查不到（或非 Unix）再退回 `ErrorKind` 里
/// `NotFound`/`PermissionDenied` 两条常见映射；两者都没有时返回 `None`，由调用方写成
/// `UNKNOWN`。
fn errno_name(error: &io::Error) -> Option<&'static str> {
    #[cfg(unix)]
    if let Some(raw) = error.raw_os_error()
        && let Some((_, name)) = ERRNO_NAMES.iter().find(|(code, _)| *code == raw)
    {
        return Some(name);
    }
    match error.kind() {
        io::ErrorKind::NotFound => Some("ENOENT"),
        io::ErrorKind::PermissionDenied => Some("EACCES"),
        _ => None,
    }
}

/// 启动子进程（Unix：`tokio::process`；Windows：cross-spawn 等价解析）。
///
/// 这是 Node `spawn` 的对应物：调用本身只创建进程并立刻返回 `Child`，不等待退出，也不读取
/// 输出；`stdout`/`stderr` 是否可读取决于 [`SpawnOptions`] 里的 stdio 模式。回收进程
/// （`Child::wait`/`start_kill`）与读取管道由调用方负责。
///
/// # 平台
///
/// - Unix：命令与参数原样交给 `Command`（PATH 查找在 exec 时由操作系统完成）；
///   `detached` 通过 `pre_exec` 调用 `setsid()`。
/// - Windows：先解析 PATH/PATHEXT 与 shebang；`.cmd`/`.bat` 等非可执行文件走
///   `cmd.exe /d /s /c "..."`，参数以 `raw_arg` 原样拼接。该分支只保证
///   `cargo check --target x86_64-pc-windows-msvc` 通过，不在验收矩阵内（见模块文档）。
///
/// # 错误
///
/// `command` 为空时不创建进程，返回 `InvalidInput`，文本为
/// `The argument 'file' cannot be empty. Received ''`；创建失败时消息为
/// `spawn <command> <errno>`（如 `spawn foo ENOENT`），错误类型沿用底层 `io::ErrorKind`。
pub fn spawn_process(
    command: &str,
    args: &[String],
    options: &SpawnOptions,
) -> io::Result<tokio::process::Child> {
    if command.is_empty() {
        // Node 的 `spawn("")` 同步抛 TypeError [ERR_INVALID_ARG_VALUE]。
        return Err(empty_command_error());
    }
    let resolved = resolve_program(command, args);
    let mut cmd = tokio::process::Command::new(&resolved.program);
    apply_args_tokio(&mut cmd, &resolved);
    // Node 的异步 spawn 把 `cwd: ""` 当作未设置（`if (options.cwd)`）。
    if let Some(cwd) = &options.cwd
        && !cwd.as_os_str().is_empty()
    {
        cmd.current_dir(cwd);
    }
    cmd.stdin(to_stdio(options.stdin))
        .stdout(to_stdio(options.stdout))
        .stderr(to_stdio(options.stderr));
    if let Some(env) = &options.env {
        cmd.env_clear();
        cmd.envs(env.iter().map(|(key, value)| (key, value)));
    }
    #[cfg(unix)]
    if options.detached {
        // Node 的 `detached: true` 在 Unix 上调用 `setsid()`（新会话 + 新进程组），
        // 而 `Command::process_group(0)` 只做 setpgid（还在原会话里）。二者对
        // `kill(-pid)` 都有效，但会话/控制终端语义不同，这里用 pre_exec 复刻 setsid。
        // SAFETY: `pre_exec` 在 fork 之后、exec 之前运行；`setsid(2)` 是 async-signal-safe 的。
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        let _ = &mut cmd;
        let mut flags = 0u32;
        if options.windows_hide {
            flags |= 0x0800_0000; // CREATE_NO_WINDOW
        }
        if options.detached {
            flags |= 0x0000_0200; // CREATE_NEW_PROCESS_GROUP
            flags |= 0x0000_0008; // DETACHED_PROCESS
        }
        if flags != 0 {
            cmd.creation_flags(flags);
        }
    }
    cmd.spawn()
        .map_err(|error| node_spawn_error(command, "spawn", error))
}

/// 同步启动子进程（Unix：`std::process`；Windows：cross-spawn 等价解析）。
/// **永不返回 `Err`**：错误放进 [`SpawnSyncBytesResult::error`]。stdout/stderr 是原始字节。
///
/// 本函数阻塞调用线程直到子进程退出（轮询 `try_wait` + 1ms 睡眠），不要在异步任务里直接
/// 调用；异步场景用 [`spawn_process`] 配合 [`wait_for_child_process`]。
///
/// # 返回值
///
/// `pid` 是操作系统分配的进程号（启动失败时为 0）；`status` 是退出码，被信号杀死或超时后
/// 才退出时为 `None`；`stdout`/`stderr` 只包含成功读到的字节。
///
/// # 超时语义
///
/// `timeout_ms` 到点后：若进程尚未退出，发送一次 SIGTERM（Unix）或 `Child::kill`
/// （其它平台）；无论进程是否已退出，都关闭两个管道读端并丢弃之后的输出。随后**继续等待
/// 进程退出**再返回（子进程可以捕获/忽略 SIGTERM）。`status` 只保留超时之前观察到的退出码，
/// 因此超时后才自行退出的进程仍得到 `None`（与 Node 一致）。
///
/// deadline 的检查排在每轮 `try_wait` 之后：同一轮先观察到的退出（即使发生在 deadline
/// 之后）仍算“超时前退出”，退出码会被保留。
///
/// # 失败模式
///
/// 读管道时 `EINTR` 会重试，`EAGAIN`/`EWOULDBLOCK` 表示暂时无数据；其余读错误被当作流
/// 结束处理，不写入 `error`。`error` 只承载启动失败、`try_wait` 失败与超时。
///
/// 输出没有 `maxBuffer` 之类的上限：stdout/stderr 会完整缓冲在内存里，直到两个流都读到
/// 结束（可能晚于进程退出）或超时；需要截断的调用方必须自己控制——`kiana-config` 的
/// `resolve_config_value` 就是因此自行实现了带 1 MiB 上限的 `spawnSync` 等价物
/// （`spawn_sync_command`）。
pub fn spawn_process_sync_bytes(
    command: &str,
    args: &[String],
    options: &SpawnSyncOptions,
) -> SpawnSyncBytesResult {
    if command.is_empty() {
        // Node 的 `spawnSync("")` 同步抛 TypeError [ERR_INVALID_ARG_VALUE]；冻结签名不能抛，
        // 放进 `error` 字段；错误消息文本是稳定契约，改动需同步测试。
        return SpawnSyncBytesResult {
            pid: 0,
            status: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            error: Some(empty_command_error()),
        };
    }
    let resolved = resolve_program(command, args);
    let mut cmd = std::process::Command::new(&resolved.program);
    apply_args_std(&mut cmd, &resolved);
    if let Some(cwd) = &options.cwd {
        // 与异步 spawn 不同：Node 的 spawnSync 对 `cwd: ""` 报 ENOENT（node 26 实测）。
        cmd.current_dir(cwd);
    }
    if let Some(env) = &options.env {
        cmd.env_clear();
        cmd.envs(env.iter().map(|(key, value)| (key, value)));
    }
    cmd.stdin(to_stdio(options.stdin))
        .stdout(to_stdio(options.stdout))
        .stderr(to_stdio(options.stderr));
    #[cfg(windows)]
    if options.windows_hide {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            return SpawnSyncBytesResult {
                pid: 0,
                status: None,
                stdout: Vec::new(),
                stderr: Vec::new(),
                error: Some(node_spawn_error(command, "spawnSync", error)),
            };
        }
    };
    let pid = child.id();
    // Node 的 `spawnSync` 没有 `input` 时立刻关闭 stdin（子进程读到 EOF）。
    let _ = child.stdin.take();

    // 计时从 spawn 成功之后开始（不包含解析命令与创建进程的时间）。`timeout: 0` 与 `None`
    // 一样表示不设超时（见 `SpawnSyncOptions::timeout_ms`）。
    let started = std::time::Instant::now();
    let deadline = options
        .timeout_ms
        .filter(|timeout| *timeout > 0)
        .map(|timeout| started + Duration::from_millis(timeout));

    let mut stdout_pump = child.stdout.take().map(PipePump::new);
    let mut stderr_pump = child.stderr.take().map(PipePump::new);
    let mut stdout_bytes: Vec<u8> = Vec::new();
    let mut stderr_bytes: Vec<u8> = Vec::new();
    let mut exit_status: Option<std::process::ExitStatus> = None;
    // 超时**之前**观察到的退出码：Node 的 spawnSync 在这种情况下仍保留 `status`。
    let mut exit_before_timeout: Option<std::process::ExitStatus> = None;
    let mut timed_out = false;
    let mut wait_error: Option<io::Error> = None;

    // 轮询式等待：std 没有跨平台的“带超时 wait”，也无法在单线程上同时阻塞读两个管道。
    // 每轮先观察退出状态，再把两个管道里已有的数据读干，最后检查 deadline。1ms 的睡眠
    // 让出 CPU，也是超时判定误差的来源之一（另加本轮排空数据的时间）。
    loop {
        if exit_status.is_none() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    exit_status = Some(status);
                    if !timed_out {
                        exit_before_timeout = Some(status);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    wait_error = Some(error);
                    break;
                }
            }
        }

        let out_ended = match stdout_pump.as_mut() {
            Some(pump) => pump.drain(&mut |chunk| stdout_bytes.extend_from_slice(chunk)),
            None => true,
        };
        let err_ended = match stderr_pump.as_mut() {
            Some(pump) => pump.drain(&mut |chunk| stderr_bytes.extend_from_slice(chunk)),
            None => true,
        };

        // 正常退出后要等两个流都读到结束，否则可能丢掉尾部输出；超时后管道已被销毁，
        // 读端会在后续轮次返回结束，但仍必须等进程真正退出（`exit_status.is_some()`）。
        if exit_status.is_some() && ((out_ended && err_ended) || timed_out) {
            break;
        }

        if !timed_out
            && let Some(deadline) = deadline
            && std::time::Instant::now() >= deadline
        {
            // 到点后只处理一次：`timed_out` 置位后本分支不再进入，SIGTERM 不会重复发送。
            // deadline 的检查晚于本轮 `try_wait`，所以本轮刚观察到的退出会被记入
            // `exit_before_timeout`，最终保留其退出码。
            timed_out = true;
            if exit_status.is_none() {
                // Node 的 spawnSync 在超时后只发送一次 killSignal（默认 SIGTERM），然后继续
                // 等待子进程退出（子进程可以捕获/忽略 SIGTERM）。不是 SIGKILL。
                terminate_sync_child(&mut child);
            }
            // Node 在超时瞬间销毁 stdio：之后的输出被丢弃，写端收到 EPIPE。
            if let Some(pump) = stdout_pump.as_mut() {
                pump.destroy();
            }
            if let Some(pump) = stderr_pump.as_mut() {
                pump.destroy();
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    let status = if timed_out {
        // 超时前已经退出 → 保留退出码；超时后才退出（或被杀）→ null。
        exit_before_timeout.and_then(|status| status.code())
    } else {
        exit_status.and_then(|status| status.code())
    };
    let error = match (timed_out, wait_error) {
        (true, _) => Some(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("spawnSync {command} ETIMEDOUT"),
        )),
        (false, Some(wait_error)) => Some(wait_error),
        (false, None) => None,
    };

    SpawnSyncBytesResult {
        pid,
        status,
        stdout: stdout_bytes,
        stderr: stderr_bytes,
        error,
    }
}

/// 同步启动子进程，stdout/stderr 按 UTF-8 有损解码（[`spawn_process_sync_bytes`] 的
/// 字符串便捷版；Node 的 `encoding` 选项路径用它）。
///
/// 非法字节被替换为 U+FFFD；需要逐字节保真（Node 未指定 `encoding` 时的 `Buffer`）时用
/// [`spawn_process_sync_bytes`]。两版的 `status`/`error` 语义相同，本函数只负责解码。
pub fn spawn_process_sync(
    command: &str,
    args: &[String],
    options: &SpawnSyncOptions,
) -> SpawnSyncResult {
    let raw = spawn_process_sync_bytes(command, args, options);
    SpawnSyncResult {
        status: raw.status,
        stdout: String::from_utf8_lossy(&raw.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&raw.stderr).into_owned(),
        error: raw.error,
    }
}

/// Node 对空 `file` 参数抛出的 `TypeError [ERR_INVALID_ARG_VALUE]` 消息。
pub(crate) const EMPTY_COMMAND_MESSAGE: &str = "The argument 'file' cannot be empty. Received ''";

fn empty_command_error() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, EMPTY_COMMAND_MESSAGE)
}

/// 子进程管道读端：Unix 上由 [`PipePump`] 直接用非阻塞 `read(2)` 轮询（无读线程），
/// 因此 `destroy` 能立即关闭 fd（等价 TS 的 `stream.destroy()`，见 #5303 的 finalize）。
/// 其它平台没有可轮询的 std 管道，退化为读线程 + channel（Windows 不进验收矩阵）。
pub(crate) trait ReadPipe: io::Read + Send + 'static {
    /// 返回底层管道的原始 fd；仅 Unix 用它做非阻塞 `read(2)`。
    #[cfg(unix)]
    fn pipe_fd(&self) -> std::os::fd::RawFd;
}

#[cfg(unix)]
impl ReadPipe for std::process::ChildStdout {
    fn pipe_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.as_raw_fd()
    }
}

#[cfg(unix)]
impl ReadPipe for std::process::ChildStderr {
    fn pipe_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.as_raw_fd()
    }
}

#[cfg(not(unix))]
impl ReadPipe for std::process::ChildStdout {}

#[cfg(not(unix))]
impl ReadPipe for std::process::ChildStderr {}

/// 子进程管道的增量读取器（见 [`ReadPipe`]）。
pub(crate) struct PipePump<R: ReadPipe> {
    /// Unix：仍然持有的读端（`destroy` 时 drop 并关闭 fd）。
    #[cfg_attr(not(unix), allow(dead_code))]
    reader: Option<R>,
    #[cfg(not(unix))]
    receiver: Option<std::sync::mpsc::Receiver<Vec<u8>>>,
}

impl<R: ReadPipe> PipePump<R> {
    /// 接管读端。Unix 上立即把 fd 切到 `O_NONBLOCK`；`fcntl` 失败时管道保持阻塞模式，
    /// 此时 [`Self::drain`] 的 `read` 可能阻塞调用线程。其它平台启动一个读线程把数据块
    /// 送进 channel。
    pub(crate) fn new(reader: R) -> Self {
        #[cfg(unix)]
        {
            let fd = reader.pipe_fd();
            // SAFETY: `fcntl` 只读取/设置本进程持有的 fd 的标志位。
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFL);
                if flags >= 0 {
                    libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
                }
            }
            Self {
                reader: Some(reader),
            }
        }
        #[cfg(not(unix))]
        {
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || read_pipe_into_channel(reader, sender));
            Self {
                reader: None,
                receiver: Some(receiver),
            }
        }
    }

    /// 读走当前可用的全部数据；返回 `true` 表示读端已结束或已被 [`Self::destroy`] 关闭。
    ///
    /// Unix：循环 `read(2)` 直到 `EAGAIN`/`EWOULDBLOCK`（返回 `false`，下一轮再试）；
    /// `EINTR` 重试；其它读错误与 EOF 一样把读端置空并返回 `true`（错误被丢弃，不上报）。
    /// 非 Unix：从 channel 取块，`Empty` 返回 `false`，发送端断开（读线程结束）返回 `true`。
    pub(crate) fn drain(&mut self, on_data: &mut dyn FnMut(&[u8])) -> bool {
        #[cfg(unix)]
        {
            let Some(fd) = self.reader.as_ref().map(ReadPipe::pipe_fd) else {
                return true;
            };
            let mut buffer = [0u8; 8192];
            loop {
                // SAFETY: fd 由本结构独占持有；buffer 是可写内存。
                let read = unsafe {
                    libc::read(fd, buffer.as_mut_ptr().cast::<libc::c_void>(), buffer.len())
                };
                if read > 0 {
                    on_data(&buffer[..read as usize]);
                    continue;
                }
                if read == 0 {
                    break;
                }
                let error = io::Error::last_os_error();
                match error.raw_os_error() {
                    Some(libc::EINTR) => continue,
                    Some(code) if code == libc::EAGAIN || code == libc::EWOULDBLOCK => {
                        return false;
                    }
                    _ => break,
                }
            }
            self.reader = None;
            true
        }
        #[cfg(not(unix))]
        {
            let mut ended = false;
            loop {
                let next = match self.receiver.as_ref() {
                    Some(receiver) => receiver.try_recv(),
                    None => return true,
                };
                match next {
                    Ok(chunk) => on_data(&chunk),
                    Err(std::sync::mpsc::TryRecvError::Empty) => return false,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        ended = true;
                        break;
                    }
                }
            }
            if ended {
                self.receiver = None;
            }
            ended
        }
    }

    /// 关闭读端（TS 的 `stream.destroy()`）。Unix 上后续写入会 EPIPE/SIGPIPE。
    ///
    /// 可重复调用（reader 已是 `None` 时无操作）。非 Unix 上只是丢弃 receiver：读线程要等
    /// 下一次 `send` 失败或管道自身 EOF 才会结束。
    pub(crate) fn destroy(&mut self) {
        self.reader = None;
        #[cfg(not(unix))]
        {
            // 读线程可能仍阻塞在 read(2)；丢弃 receiver 后它会在下一次 send 失败时退出。
            self.receiver = None;
        }
    }
}

/// 读线程（仅非 Unix）：把管道里的块塞进 channel。
#[cfg(not(unix))]
fn read_pipe_into_channel(mut pipe: impl io::Read, sender: std::sync::mpsc::Sender<Vec<u8>>) {
    let mut buffer = [0u8; 8192];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                if sender.send(buffer[..read].to_vec()).is_err() {
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

/// 给 `spawnSync` 的子进程发送默认 `killSignal`（Unix: SIGTERM；其它平台: TerminateProcess）。
fn terminate_sync_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        if pid > 0 {
            // SAFETY: `kill(2)` 只接收 pid 与信号编号，无内存安全前提。
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
}

// =============================================================================
// waitForChildProcess
// =============================================================================

/// 等待子进程结束（见模块文档的 #5303 语义）。
///
/// 先 `take()` 走 `child.stdout`/`child.stderr`（存在时排空），避免子进程因管道写满而
/// 阻塞；因此调用方若需要输出，必须在调用前自行 `take()`：被取走的流不再由本函数读取，
/// 按 TS 的 `child.stdout === null` 语义视为已结束（见测试
/// `wait_for_child_process_drains_output`）。
///
/// 返回 `Ok(Some(code))` 表示正常退出码；被信号杀死时为 `Ok(None)`。进程退出后若仍有流
/// 未结束，进入空闲宽限：连续 [`EXIT_STDIO_GRACE_MS`] 毫秒没有新数据才返回，期间每读到
/// 一块数据都重新计时（#5303：孙进程持有 stdout 时不能按固定 deadline 截断尾部输出）。
///
/// 等待本身失败时返回 `io::Error`（exit future 的错误直接向上传播，两个流随函数返回被
/// 丢弃）；读流时的错误不会变成 `Err`，而是按 EOF 处理。
///
/// # 取消
///
/// `take()` 在 future 第一次被 poll 时执行。若该 future 在那之后被取消或丢弃，已取走的
/// `ChildStdout`/`ChildStderr` 会随 future 一起 drop（读端关闭），这部分输出不再可读，
/// 也无法从 `child` 上重新取回；`child` 本身仍归调用方所有，本函数不会 kill 它。因此
/// 需要超时/取消语义时，应在调用方先自行 `take()` 两条流并并发读取，或改用其他能保留
/// 流的方案。
pub async fn wait_for_child_process(child: &mut tokio::process::Child) -> io::Result<Option<i32>> {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let exit = async { child.wait().await.map(|status| status.code()) };
    wait_parts_core(exit, stdout, stderr, |_, _| {}).await
}

/// 同 [`wait_for_child_process`]，但返回原始 [`std::process::ExitStatus`]，并把每个数据块
/// 回放给 `on_chunk(is_stdout, data)`。
///
/// 需要区分“正常退出码”与“被信号终止”的调用方（例如 bash 工具的 `signalCode`）用这个，
/// 不必再复制一份等待状态机。
pub async fn wait_for_child_process_observed<F>(
    child: &mut tokio::process::Child,
    on_chunk: F,
) -> io::Result<std::process::ExitStatus>
where
    F: FnMut(bool, &[u8]),
{
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    wait_parts_core(child.wait(), stdout, stderr, on_chunk).await
}

/// 测试缝（test hook, not public API）：用注入的 exit future 与两个流复刻
/// `waitForChildProcess` 的状态机。`stdout`/`stderr` 为 `None` 等价 TS 的 `child.stdout === null`。
///
/// `exit` 的 `Ok(Some(code))` / `Ok(None)` 分别是正常退出码与被信号杀死；`Err` 会原样
/// 返回。流里的数据被读取后丢弃，需要断言内容时用
/// [`wait_for_child_process_parts_observed`]。
#[doc(hidden)]
pub async fn wait_for_child_process_parts<E, O, R>(
    exit: E,
    stdout: Option<O>,
    stderr: Option<R>,
) -> io::Result<Option<i32>>
where
    E: Future<Output = io::Result<Option<i32>>>,
    O: AsyncRead + Unpin,
    R: AsyncRead + Unpin,
{
    wait_parts_core(exit, stdout, stderr, |_, _| {}).await
}

/// 测试缝（test hook, not public API）：同 [`wait_for_child_process_parts`]，但把读到的
/// 每个数据块回放给 `on_chunk(is_stdout, data)`，供 #5303 断言“尾部输出没有被截断”。
///
/// 回调在等待 future 内同步调用；`is_stdout == false` 表示数据来自 stderr。
#[doc(hidden)]
pub async fn wait_for_child_process_parts_observed<E, O, R, F>(
    exit: E,
    stdout: Option<O>,
    stderr: Option<R>,
    on_chunk: F,
) -> io::Result<Option<i32>>
where
    E: Future<Output = io::Result<Option<i32>>>,
    O: AsyncRead + Unpin,
    R: AsyncRead + Unpin,
    F: FnMut(bool, &[u8]),
{
    wait_parts_core(exit, stdout, stderr, on_chunk).await
}

/// 单次读取一个流的结果。把“没有这个流”与“读到 EOF/出错”分开，主循环据此推进
/// `stdout_ended`/`stderr_ended`。
enum ChunkRead {
    /// 读到一块数据（内部缓冲区 8 KiB，因此单块不超过 8 KiB）。
    Data(Vec<u8>),
    /// `read` 返回 0（对端关闭管道）。
    Eof,
    /// `read` 返回错误；调用方把它当 EOF，不向上传播。
    Error,
    /// 流为 `None`（调用方已取走，或该路 stdio 不是 `Pipe`）。
    Absent,
}

/// 从 `Some` 流读取一块数据；`None` 直接返回 [`ChunkRead::Absent`]。
async fn read_chunk<R: AsyncRead + Unpin>(stream: Option<&mut R>) -> ChunkRead {
    let Some(stream) = stream else {
        return ChunkRead::Absent;
    };
    let mut buffer = [0u8; 8192];
    match stream.read(&mut buffer).await {
        Ok(0) => ChunkRead::Eof,
        Ok(n) => ChunkRead::Data(buffer[..n].to_vec()),
        Err(_) => ChunkRead::Error,
    }
}

/// [`wait_for_child_process`] 的状态机内核。
///
/// `exit` future 的返回类型就是函数的返回类型：退出码（`Option<i32>`）或原始
/// `ExitStatus` 都走同一套状态机。
///
/// 状态变量：
/// - `exited` / `exit_value`：注入的 exit future 是否完成，以及它返回的值；
/// - `stdout_ended` / `stderr_ended`：对应流是否读到 EOF、读取出错，或本来就不存在；
/// - `deadline`：只在“已经退出但仍有流未结束”时设置；每读到一个数据块就重置为
///   `now + EXIT_STDIO_GRACE_MS`。
///
/// 每轮 `select!` 的分支（`biased`，按声明顺序优先）：
/// 1. exit future 完成 → 记录返回值；若还有流未结束则武装 100ms 宽限；
/// 2. stdout 有数据 → 回放给 `on_chunk(true, ..)`，已退出时续期；
/// 3. stderr 有数据 → 回放给 `on_chunk(false, ..)`，已退出时续期；
/// 4. 宽限计时器到点 → 丢弃两个流并返回退出值（等价 TS 的 `destroy()`）。
///
/// 每轮先检查终止条件：`exited && stdout_ended && stderr_ended` 同时为真时立即返回
/// `exit_value`。exit future 返回 `Err` 时立即向上传播，两个流随函数返回被丢弃。
async fn wait_parts_core<E, T, O, R, F>(
    exit: E,
    mut stdout: Option<O>,
    mut stderr: Option<R>,
    mut on_chunk: F,
) -> io::Result<T>
where
    E: Future<Output = io::Result<T>>,
    O: AsyncRead + Unpin,
    R: AsyncRead + Unpin,
    F: FnMut(bool, &[u8]),
{
    let mut exit = std::pin::pin!(exit);
    let mut stdout_ended = stdout.is_none();
    let mut stderr_ended = stderr.is_none();
    let mut exited = false;
    let mut exit_value: Option<T> = None;
    let mut deadline: Option<tokio::time::Instant> = None;
    let grace = Duration::from_millis(EXIT_STDIO_GRACE_MS);

    loop {
        if exited && stdout_ended && stderr_ended {
            return exit_value.ok_or_else(|| io::Error::other("child process did not exit"));
        }
        let timer_deadline = deadline;
        let idle_timer = async move {
            match timer_deadline {
                Some(instant) => tokio::time::sleep_until(instant).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            biased;
            result = &mut exit, if !exited => {
                match result {
                    Ok(value) => {
                        exited = true;
                        exit_value = Some(value);
                        if !stdout_ended || !stderr_ended {
                            deadline = Some(tokio::time::Instant::now() + grace);
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
            chunk = read_chunk(stdout.as_mut()), if !stdout_ended => {
                match chunk {
                    ChunkRead::Data(data) => {
                        on_chunk(true, &data);
                        // 已退出后仍有数据：续期，避免孙进程持有 stdout 时尾部输出被截断
                        // （#5303 的修复点）。
                        if exited {
                            deadline = Some(tokio::time::Instant::now() + grace);
                        }
                    }
                    // EOF 与读错误都视为该流结束；读错误不向上传播。
                    ChunkRead::Eof | ChunkRead::Error => stdout_ended = true,
                    ChunkRead::Absent => stdout_ended = true,
                }
            }
            chunk = read_chunk(stderr.as_mut()), if !stderr_ended => {
                match chunk {
                    ChunkRead::Data(data) => {
                        on_chunk(false, &data);
                        // 与 stdout 相同：退出后的每个 stderr 数据块也重置空闲计时器。
                        if exited {
                            deadline = Some(tokio::time::Instant::now() + grace);
                        }
                    }
                    ChunkRead::Eof | ChunkRead::Error => stderr_ended = true,
                    ChunkRead::Absent => stderr_ended = true,
                }
            }
            _ = idle_timer, if deadline.is_some() => {
                // 与 TS 的 finalize 一致：丢弃两个流（destroy）并返回退出值。
                return exit_value.ok_or_else(|| io::Error::other("child process did not exit"));
            }
        }
    }
}

/// 把解析结果写进异步 `Command`。Windows 上 `verbatim` 为真时用 `raw_arg`：整行
/// `cmd.exe /d /s /c "..."` 已经由 `resolve_program` 拼好并转义，必须绕过标准库的
/// 参数转义原样传入。非 Windows 恒走 `args()`。
fn apply_args_tokio(cmd: &mut tokio::process::Command, resolved: &ResolvedProgram) {
    #[cfg(windows)]
    if resolved.verbatim {
        for arg in &resolved.args {
            cmd.raw_arg(arg);
        }
        return;
    }
    cmd.args(&resolved.args);
}

/// [`apply_args_tokio`] 的同步版本，`verbatim` 语义相同。
fn apply_args_std(cmd: &mut std::process::Command, resolved: &ResolvedProgram) {
    #[cfg(windows)]
    if resolved.verbatim {
        use std::os::windows::process::CommandExt;
        for arg in &resolved.args {
            cmd.raw_arg(arg);
        }
        return;
    }
    cmd.args(&resolved.args);
}

// =============================================================================
// Windows: cross-spawn 等价解析
// =============================================================================

/// cross-spawn 解析结果：可执行文件 + 参数；`verbatim` 表示参数已自行转义，
/// 必须以 `raw_arg` 原样拼接（`cmd.exe /d /s /c "..."`）。
struct ResolvedProgram {
    program: String,
    args: Vec<String>,
    /// 仅 Windows 的 cmd.exe 路径使用；其它平台恒为 `false`。
    #[cfg_attr(not(windows), allow(dead_code))]
    verbatim: bool,
}

/// 非 Windows：恒等映射——命令与参数原样交给 `Command`，PATH 查找由操作系统完成。
#[cfg(not(windows))]
fn resolve_program(command: &str, args: &[String]) -> ResolvedProgram {
    ResolvedProgram {
        program: command.to_string(),
        args: args.to_vec(),
        verbatim: false,
    }
}

/// Windows：按 cross-spawn 的顺序解析命令。
///
/// 1. `resolve_command` 在显式路径或 PATH/PATHEXT 中找文件；找不到时原样返回命令，
///    交给 `Command` 自己处理；
/// 2. 文件有 shebang → 换成解释器（解释器本身再解析一次），脚本路径紧跟在 shebang
///    自带的解释器参数之后、用户参数之前（`#!/usr/bin/env node` 得到
///    `env node <script> ...`）；
/// 3. `.com`/`.exe` → 直接执行；
/// 4. 其余（`.cmd`/`.bat` 等）→ 用 `cmd.exe /d /s /c "<转义后的整行>"` 执行，并把
///    `verbatim` 置为 `true`，让调用方用 `raw_arg` 原样拼接。
///
/// 该分支只保证 `cargo check --target x86_64-pc-windows-msvc` 通过，不在验收矩阵内。
#[cfg(windows)]
fn resolve_program(command: &str, args: &[String]) -> ResolvedProgram {
    let Some(file) = resolve_command(command) else {
        return ResolvedProgram {
            program: command.to_string(),
            args: args.to_vec(),
            verbatim: false,
        };
    };

    // cross-spawn 的 shebang 探测：找到解释器后把脚本路径插在 shebang 参数之后、
    // 用户参数之前（`#!/usr/bin/env node` → `node <script> ...`）。
    if let Some((interpreter, mut interpreter_args)) = read_shebang(&file) {
        let program = resolve_command(&interpreter).unwrap_or(interpreter);
        interpreter_args.push(file);
        interpreter_args.extend_from_slice(args);
        return ResolvedProgram {
            program,
            args: interpreter_args,
            verbatim: false,
        };
    }

    if is_executable_file(&file) {
        return ResolvedProgram {
            program: file,
            args: args.to_vec(),
            verbatim: false,
        };
    }

    // 走到这里说明不是可直接执行的 .com/.exe：用 cmd.exe 执行。
    // `node_modules/.bin/` 下的 .cmd shim 需要把参数转义两遍（cross-spawn 的
    // needsDoubleEscapeMetaChars），其它 .cmd/.bat 只转义一遍。
    let needs_double_escape = is_cmd_shim(&file);
    let escaped_command = escape_command(&normalize_windows_path(&file));
    let escaped_args: Vec<String> = args
        .iter()
        .map(|arg| escape_argument(arg, needs_double_escape))
        .collect();
    let mut parts = vec![escaped_command];
    parts.extend(escaped_args);
    let shell_command = parts.join(" ");
    // cross-spawn 读 `process.env.comspec`；这里依次尝试小写与 `ComSpec` 两种拼写，
    // 都取不到时回退到 `cmd.exe`。
    let comspec = std::env::var("comspec")
        .or_else(|_| std::env::var("ComSpec"))
        .unwrap_or_else(|_| "cmd.exe".to_string());
    ResolvedProgram {
        program: comspec,
        args: vec![
            "/d".to_string(),
            "/s".to_string(),
            "/c".to_string(),
            format!("\"{shell_command}\""),
        ],
        verbatim: true,
    }
}

/// `.com`/`.exe`（不区分大小写）视为可直接执行的文件。
#[cfg(windows)]
fn is_executable_file(file: &str) -> bool {
    let lower = file.to_ascii_lowercase();
    lower.ends_with(".com") || lower.ends_with(".exe")
}

/// 判断是否 `node_modules/.bin/` 下的 `.cmd` 文件（参数需要二次转义的 shim）。
#[cfg(windows)]
fn is_cmd_shim(file: &str) -> bool {
    let normalised = file.replace('\\', "/").to_ascii_lowercase();
    normalised.contains("node_modules/.bin/") && normalised.ends_with(".cmd")
}

/// `path.win32.normalize` 的近似：只把 `/` 换成 `\`。**不**折叠重复分隔符，也不处理
/// `.`/`..`（当前实现只有逐字符替换）。
#[cfg(windows)]
fn normalize_windows_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if c == '/' {
            out.push('\\');
        } else {
            out.push(c);
        }
    }
    out
}

/// 读取 `PATHEXT` 并拆成小写扩展名列表；变量未设置或为空时回退到
/// `.com/.exe/.bat/.cmd`（顺序与回退表一致）。
#[cfg(windows)]
fn path_extensions() -> Vec<String> {
    match std::env::var("PATHEXT") {
        Ok(value) if !value.is_empty() => value
            .split(';')
            .filter(|ext| !ext.is_empty())
            .map(|ext| ext.to_ascii_lowercase())
            .collect(),
        _ => [".com", ".exe", ".bat", ".cmd"]
            .iter()
            .map(|ext| ext.to_string())
            .collect(),
    }
}

/// 在 Windows 上解析命令路径。
///
/// - 显式路径的判定是：含 `/`、含 `\`，或长度至少 2 且第二个字节是 `:`（盘符）。先按
///   原样检查文件，再逐个追加 PATHEXT 扩展名重试；
/// - 否则按 `PATH`（`;` 分隔）逐目录拼接命令名，同样先原样、再补扩展名；
/// - 空目录项被跳过；全部失败返回 `None`，由调用方回退为原命令名。
#[cfg(windows)]
fn resolve_command(command: &str) -> Option<String> {
    let has_separator = command.contains('/')
        || command.contains('\\')
        || (command.len() >= 2 && command.as_bytes()[1] == b':');
    if has_separator {
        let candidate = std::path::Path::new(command);
        if candidate.is_file() {
            return Some(command.to_string());
        }
        // 补 PATHEXT 再试一次。
        for ext in path_extensions() {
            let with_ext = format!("{command}{ext}");
            if std::path::Path::new(&with_ext).is_file() {
                return Some(with_ext);
            }
        }
        return None;
    }
    let path_value = std::env::var("PATH")
        .or_else(|_| std::env::var("Path"))
        .ok()?;
    let extensions = path_extensions();
    for directory in path_value.split(';') {
        if directory.is_empty() {
            continue;
        }
        let base = std::path::Path::new(directory).join(command);
        let base_str = base.to_string_lossy().into_owned();
        if std::path::Path::new(&base_str).is_file() {
            return Some(base_str);
        }
        for ext in &extensions {
            let candidate = format!("{base_str}{ext}");
            if std::path::Path::new(&candidate).is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// `shebang-command` 的近似：读取前 150 字节；没有 shebang 返回 `None`。
/// 只解析第一行，`#!` 之后按空白切出解释器与参数。
#[cfg(windows)]
fn read_shebang(file: &str) -> Option<(String, Vec<String>)> {
    use std::io::Read;
    let mut buffer = [0u8; 150];
    let mut handle = std::fs::File::open(file).ok()?;
    let read = handle.read(&mut buffer).ok()?;
    let text = String::from_utf8_lossy(&buffer[..read]);
    let first_line = text.lines().next()?;
    let rest = first_line.strip_prefix("#!")?;
    let mut parts = rest.split_whitespace();
    let interpreter = parts.next()?.to_string();
    let args: Vec<String> = parts.map(|part| part.to_string()).collect();
    Some((interpreter, args))
}

/// `cross-spawn/lib/util/escape.js` 的 `escapeCommand`。
#[cfg(windows)]
fn escape_command(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len());
    for c in arg.chars() {
        if matches!(
            c,
            '(' | ')'
                | ']'
                | '['
                | '%'
                | '!'
                | '^'
                | '"'
                | '`'
                | '<'
                | '>'
                | '&'
                | '|'
                | ';'
                | ','
                | ' '
                | '*'
                | '?'
        ) {
            out.push('^');
        }
        out.push(c);
    }
    out
}

/// `cross-spawn/lib/util/escape.js` 的 `escapeArgument`。
///
/// `double_escape_meta_chars` 为真时只对转义结果**再执行一遍元字符替换**（cross-spawn 的
/// `needsDoubleEscapeMetaChars` 分支原样重复 `replace(metaCharsRegExp, '^$1')`），不会再加
/// 一层引号或重新加倍反斜杠。
#[cfg(windows)]
fn escape_argument(arg: &str, double_escape_meta_chars: bool) -> String {
    let mut escaped = escape_argument_step(arg);
    if double_escape_meta_chars {
        escaped = escape_command(&escaped);
    }
    escaped
}

/// `escapeArgument` 的单次转义：处理 `\` 后跟 `"` 的序列（反斜杠加倍后再补一个反斜杠
/// 转义引号）、字符串结尾的连续反斜杠（加倍），最后整体加引号并用 `escape_command`
/// 转义元字符。
#[cfg(windows)]
fn escape_argument_step(arg: &str) -> String {
    // 1) 反斜杠后跟 `"`：反斜杠加倍并转义引号。
    let chars: Vec<char> = arg.chars().collect();
    let mut step1 = String::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '"' {
            // 没有前置反斜杠的引号也要转义（cross-spawn 的 `\1$1\\"` 展开：`$1` 为空）。
            step1.push('\\');
            step1.push('"');
            index += 1;
            continue;
        }
        if chars[index] == '\\' {
            let mut run = 0;
            while index + run < chars.len() && chars[index + run] == '\\' {
                run += 1;
            }
            if index + run < chars.len() && chars[index + run] == '"' {
                step1.push_str(&"\\".repeat(run * 2));
                step1.push('\\');
                step1.push('"');
                index += run + 1;
                continue;
            }
        }
        step1.push(chars[index]);
        index += 1;
    }
    // 2) 字符串结尾的连续反斜杠：加倍。
    let trailing = step1.chars().rev().take_while(|c| *c == '\\').count();
    if trailing > 0 {
        let without = &step1[..step1.len() - trailing];
        step1 = format!("{without}{}", "\\".repeat(trailing * 2));
    }
    // 3) 整体加引号，再转义元字符。
    let quoted = format!("\"{step1}\"");
    escape_command(&quoted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_for_child_process_returns_exit_code() {
        let mut child = spawn_process(
            "/bin/sh",
            &["-c".to_string(), "exit 7".to_string()],
            &SpawnOptions {
                stdout: StdioMode::Pipe,
                stderr: StdioMode::Pipe,
                ..SpawnOptions::default()
            },
        )
        .expect("spawn");
        assert_eq!(
            wait_for_child_process(&mut child).await.expect("wait"),
            Some(7)
        );
    }

    #[tokio::test]
    async fn wait_for_child_process_drains_output() {
        let mut child = spawn_process(
            "/bin/sh",
            &["-c".to_string(), "printf 'hello'".to_string()],
            &SpawnOptions {
                stdout: StdioMode::Pipe,
                stderr: StdioMode::Pipe,
                ..SpawnOptions::default()
            },
        )
        .expect("spawn");
        let stdout = child.stdout.take();
        assert_eq!(
            wait_for_child_process(&mut child).await.expect("wait"),
            Some(0)
        );
        // 流已被调用方取走：`wait_for_child_process` 只等退出（TS 的 null 语义）。
        assert!(stdout.is_some());
    }
}
