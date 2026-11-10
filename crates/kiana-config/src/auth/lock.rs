//! `proper-lockfile` 4.1.2 的等价物：目录锁 + 10×20 ms 同步重试 + Windows EPERM 额外重试。
//!
//! 必须与 Node 版**互操作**（两者同时操作同一 `~/.kiana`）。见 `ca-config-models.md` §8.3 第 5 条。
//!
//! ## 复刻的语义
//!
//! - **目录锁**：锁文件是**目录**（`fs.mkdir` 原子创建），默认路径 `<target>.lock`；
//!   `lockfilePath` 可覆盖（trust 锁的是目录，但锁文件名是 `${trust.json}.lock`）。
//! - **`realpath: false`**：target 只做绝对化/词法规范化，不解析符号链接。
//! - **ELOCKED**：创建目录遇到 `AlreadyExists` 即“已被持有”（`Lock file is already being held`）。
//! - **陈旧锁**：锁目录 mtime 早于 `now - stale_ms` 时删除并重试一次（此后不再做陈旧判定，
//!   与 proper-lockfile 的 `{...options, stale: 0}` 递归一致）。`stale_ms` 与 `update_ms`
//!   按 proper-lockfile 的 `lock()` 规则钳制：`stale >= 2000`、`1000 <= update <= stale/2`。
//! - **心跳**：持有锁期间每 `update_ms` 用 `utimes` 刷新锁目录 mtime。若目录消失或 mtime
//!   不再是自己的值 → `onCompromised`（[`LOCK_COMPROMISED_MESSAGE`]）；stat/utimes 的其它
//!   错误在 `lastUpdate + stale` 阈值内按 1 s 间隔重试，越过阈值才判为破坏
//!   （与 proper-lockfile 的 `updateLock` 一致）。
//! - **同步重试**：`lock_sync` 最多 10 次、每次间隔 20 ms，**只**重试 `AlreadyExists`
//!   （= ELOCKED）；其它错误立即返回。（auth-storage.ts 的同步重试在 win32 上还会重试 EPERM；
//!   该差异只在 Windows 上可见，Windows 分支只保证编译，见 ARCHITECTURE §8 第 26 条。）
//! - **异步重试**：`lock_async` 以 `stale_ms`（30 s）为总预算，指数退避
//!   `min(10·2^n, 1000)` ms × `(1 + jitter)`，总延迟不超过剩余预算；ELOCKED 重试，
//!   Windows 上 `PermissionDenied`（EPERM）另算、最多 5 次。
//! - **release**：停止心跳并删除锁目录，忽略删除错误（与 TS 调用方一致）。
//! - **I/O 错误文案**：`Display` 逐字复刻 Node/libuv 的
//!   `EACCES: permission denied, mkdir '/path'`（ARCHITECTURE §4.1）。
//!
//! ## 已知偏差
//!
//! - proper-lockfile 用 `mtime-precision` 探测把锁目录 mtime 设到“整秒 + 5 ms”；这里直接设为
//!   `SystemTime::now()`（只影响陈旧判定的毫秒级边界）。
//! - proper-lockfile 的定时器在 `update` 之后按“回调完成后重新计时”调度；这里的心跳线程用固定
//!   间隔的 `recv_timeout`，每次 tick 耗时不计入间隔（差异在毫秒级）。
//! - `onCompromised` 的错误文案统一为 [`LOCK_COMPROMISED_MESSAGE`]
//!   （`Auth storage lock was compromised`，`ca-config-models.md` §3.4），而不是 proper-lockfile
//!   内部的 ECOMPROMISED 文案（那些文案不会出现在用户可见输出里）。
//! - Windows 无法用 `std::fs::File` 打开目录，心跳改用“创建并删除探针文件”更新目录 mtime；
//!   Windows 分支只保证编译（ARCHITECTURE §8 第 26 条）。
//! - `getcwd` 失败时 Node 的文案是 `ENOENT: no such file or directory, uv_cwd`（无路径）；
//!   [`LockError::Io`] 恒定携带路径，因此这里会多出 `'<target>'`。该路径只在进程 cwd 被删除
//!   时可达。
//!
//! ## 调用方
//!
//! - `auth/storage.rs`：`with_lock`（同步）与 `with_lock_async`（异步，30 s 预算，并把
//!   `onCompromised` 转成带 `ECOMPROMISED` 错误的 `AuthError`）。
//! - `settings/storage.rs`：文件已存在时直接调用 `acquire_lock_sync_with_retry`；文件不存在
//!   时先执行回调，只在需要写回时才加锁并创建父目录（只读场景不创建锁）。
//! - `trust/manager.rs`：`acquire_lock_sync_with_retry_at`，`target` 是 trust 目录，实际锁
//!   路径为 `${trust.json}.lock`。
//!
//! ## 执行流程
//!
//! 1. 每次尝试先把 `target` 绝对化（`realpath: false` 时只做词法规范化），再由 `lock_path`
//!    得到锁目录：默认 `<resolved>.lock`，设置了 `lockfilePath` 则直接使用它。
//! 2. 单次尝试 `try_acquire`：`create_dir` 成功即持锁；`AlreadyExists` 时 `stat` 锁目录并按
//!    mtime 判陈旧，陈旧则删除后在**同一次调用内**重试一次（此后不再判陈旧）。
//! 3. 持锁后立刻把锁目录 mtime 设为当前时间并回读，作为“自己的值”；然后启动心跳线程。
//! 4. 释放走 `LockState::release`（置 `released` → 停心跳 → 删锁目录）；破坏走
//!    `LockState::mark_compromised`（置 `released` 与 `compromised`，由心跳线程在返回前
//!    调用，锁目录不删除）。
//!
//! ## 并发与失败模式
//!
//! - 互斥来自 `mkdir` 的原子性：两个进程同时创建同一锁目录只有一个成功，失败者得到
//!   ELOCKED。陈旧锁接管是“先删后建”，两步之间没有原子性：另一个接管者可能在这段窗口里
//!   删掉刚建好的锁。代码用“本次调用只判一次陈旧”限制连续删除；被抢走的一方通常会在
//!   下一次心跳发现锁目录消失或 mtime 不匹配，标记为 compromised（mtime 是唯一判据，
//!   精度不足且两次设置落在同一刻度时可能漏检）。
//! - 心跳是 `std::thread`（阻塞 `recv_timeout` + 文件操作），不占用 Tokio worker；但
//!   [`AsyncLockGuard::release`] 会在调用者的执行器线程上同步删除目录并 `join` 心跳线程。
//!   `lock_async` 的单次尝试（mkdir/stat/rmdir 等文件系统调用）同样是同步调用，没有
//!   `spawn_blocking`；只有退避 `sleep` 是异步的。
//! - `LockState::release` 使用非递归的 `remove_dir` 且忽略错误。锁目录非空（例如外部写入
//!   文件、Windows 探针文件残留）时释放会静默失败；下一个持有者虽会按陈旧规则尝试删除，
//!   但 `remove_dir` 同样会因目录非空失败，最终以 `Io`（syscall `rmdir`）报错。进程崩溃
//!   留下的是空目录，可以在 `stale` 到期后被接管。
//! - 测试钩子 `try_lock_async_with_clock` / `lock_async_with_clock` 只替换陈旧判定与重试
//!   预算所用的 `now`；心跳刷新 mtime 与 `last_update_ms` 仍取真实系统时钟。

use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio_util::sync::CancellationToken;

use kiana_core::sleep::sleep;

/// 同步锁的最大尝试次数（TS: `maxAttempts = 10`）。
///
/// 含第一次尝试；两次尝试之间睡眠 [`SYNC_RETRY_DELAY_MS`]，最后一次失败立即返回，
/// 因此最多睡眠 9 次（约 180 ms）。
pub const SYNC_MAX_ATTEMPTS: usize = 10;

/// 同步锁的每次重试间隔（TS: `delayMs = 20`）。
pub const SYNC_RETRY_DELAY_MS: u64 = 20;

/// proper-lockfile 的 `stale` 下限（`Math.max(stale || 0, 2000)`）。
///
/// 传 0 不会禁用陈旧判定，而是按 2000 ms 生效（测试
/// `treats_zero_stale_as_the_proper_lockfile_minimum` 锁定此行为）。
pub const MIN_STALE_MS: u64 = 2_000;

/// proper-lockfile 的 `update` 下限（`Math.max(..., 1000)`）。
///
/// `stale` 已被钳制到至少 2000，因此上限 `stale / 2 >= 1000`，与下限 1000 不冲突；
/// 但 `update_ms` 本身可能小于 1000（例如测试 `clamps_heartbeat_update_interval_to_one_second`
/// 传入 5），最终由 `.max(MIN_UPDATE_MS)` 抬到 1000。
pub const MIN_UPDATE_MS: u64 = 1_000;

/// 心跳遇到非 ENOENT 错误后的重试间隔（proper-lockfile 的 `lock.updateDelay = 1000`）。
pub const UPDATE_RETRY_DELAY_MS: u64 = 1_000;

/// 异步锁的陈旧阈值/重试预算（TS: `staleMs = 30_000`）。
pub const ASYNC_STALE_MS: u64 = 30_000;

/// 异步退避的基线上限（TS: `maxDelayMs / 2 = 1_000`）。
pub const ASYNC_MAX_BASE_DELAY_MS: u64 = 1_000;

/// Windows `EPERM` 的额外重试上限（TS: `MAX_EPERM_RETRIES = 5`）。
pub const MAX_EPERM_RETRIES: u32 = 5;

/// `onCompromised` 的错误文案（`ca-config-models.md` §3.4）。
pub const LOCK_COMPROMISED_MESSAGE: &str = "Auth storage lock was compromised";

/// `onCompromised` 回调。
///
/// 由心跳线程在检出锁被破坏时同步调用，最多一次（见 `LockState::mark_compromised`）。
/// 参数总是 [`LockError::Compromised`]，`message` 为 [`LOCK_COMPROMISED_MESSAGE`]。
/// 回调应尽快返回：它运行在心跳线程上，会阻塞该线程退出；若 guard 同时在释放，
/// `release` 里的 `join` 也会一起等待。
pub type CompromisedCallback = Arc<dyn Fn(LockError) + Send + Sync>;

/// 加锁选项（对应 `proper-lockfile` 的 `LockOptions` 子集）。
///
/// `stale_ms` / `update_ms` 是**原始值**；加锁时按 proper-lockfile 的规则钳制
/// （`LockOptions::effective_stale_ms` / `LockOptions::effective_update_ms`）。
///
/// 派生 `Clone`；`on_compromised` 是 `Arc`，克隆后共享同一个回调。`Debug` 为手写：只输出
/// 回调是否设置（`is_some()`），因为 `Arc<dyn Fn ...>` 不实现 `Debug`，无法派生。
#[derive(Clone)]
pub struct LockOptions {
    /// 自定义锁路径（对应 `lockfilePath`）；`None` → `<target>.lock`。
    pub lockfile_path: Option<PathBuf>,
    /// 是否解析符号链接（对应 `realpath`）。本 crate 的调用点一律 `false`。
    ///
    /// `true` 走 `fs::canonicalize`，要求目标已存在（失败返回 `Io`，syscall `lstat`）；
    /// 规范化后的路径还决定默认锁路径。当前仓库没有调用点使用 `true`。
    pub realpath: bool,
    /// 陈旧阈值（毫秒）。加锁时被钳制到 `>= 2000`（[`MIN_STALE_MS`]）。
    ///
    /// 这里保存调用方传入的原值：`lock_async` 还用它作为重试总预算（不经过钳制），
    /// 见 `lock_async_with_clock`。
    pub stale_ms: u64,
    /// 心跳间隔（毫秒）。加锁时被钳制到 `[1000, stale/2]`
    /// （[`MIN_UPDATE_MS`]；proper-lockfile 默认 `stale / 2`）。
    ///
    /// 同样保存原值；测试用 `with_update_ms` 注入小值来观察钳制与心跳重试。
    pub update_ms: u64,
    /// 锁被外部破坏时的回调（对应 `onCompromised`）。
    ///
    /// 在心跳线程上同步调用，最多一次；见 [`CompromisedCallback`]。
    pub on_compromised: Option<CompromisedCallback>,
}

impl Default for LockOptions {
    /// 同 [`LockOptions::sync_default`]（同步锁的 proper-lockfile 默认值）。
    fn default() -> Self {
        Self::sync_default()
    }
}

impl LockOptions {
    /// `lockfile.lockSync(path, { realpath: false })` 的默认值：
    /// proper-lockfile 的 `stale: 10_000`、`update: stale / 2`。
    pub fn sync_default() -> Self {
        Self {
            lockfile_path: None,
            realpath: false,
            stale_ms: 10_000,
            update_ms: 5_000,
            on_compromised: None,
        }
    }

    /// `auth-storage.ts` 的 `acquireLockAsync` 选项：`stale: 30_000`、`update: stale / 2`。
    pub fn async_default() -> Self {
        Self {
            lockfile_path: None,
            realpath: false,
            stale_ms: ASYNC_STALE_MS,
            update_ms: ASYNC_STALE_MS / 2,
            on_compromised: None,
        }
    }

    /// 设置 `lockfilePath`。
    pub fn with_lockfile_path(mut self, lockfile_path: impl Into<PathBuf>) -> Self {
        self.lockfile_path = Some(lockfile_path.into());
        self
    }

    /// 设置 `stale_ms`（原样保存；锁核心按 `>= 2000` 钳制，异步重试预算用原值）。
    pub fn with_stale_ms(mut self, stale_ms: u64) -> Self {
        self.stale_ms = stale_ms;
        self
    }

    /// 设置心跳间隔（测试钩子；生产用默认值）。生效值仍会被钳制到 `[1000, stale/2]`。
    pub fn with_update_ms(mut self, update_ms: u64) -> Self {
        self.update_ms = update_ms;
        self
    }

    /// 设置 `onCompromised` 回调。
    pub fn with_on_compromised(mut self, callback: CompromisedCallback) -> Self {
        self.on_compromised = Some(callback);
        self
    }

    /// proper-lockfile 的 `options.stale = Math.max(options.stale || 0, 2000)`。
    fn effective_stale_ms(&self) -> u64 {
        self.stale_ms.max(MIN_STALE_MS)
    }

    /// proper-lockfile 的
    /// `update = Math.max(Math.min(update ?? stale / 2, stale / 2), 1000)`。
    ///
    /// 整数除法向下取整；`stale` 已 >= 2000，所以 `stale / 2 >= 1000`。
    fn effective_update_ms(&self) -> u64 {
        let stale = self.effective_stale_ms();
        self.update_ms.min(stale / 2).max(MIN_UPDATE_MS)
    }
}

impl fmt::Debug for LockOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LockOptions")
            .field("lockfile_path", &self.lockfile_path)
            .field("realpath", &self.realpath)
            .field("stale_ms", &self.stale_ms)
            .field("update_ms", &self.update_ms)
            .field("on_compromised", &self.on_compromised.is_some())
            .finish()
    }
}

/// 加锁/解锁错误。
///
/// `Display` 的文案逐字对齐 TS 的 `error.message`（ARCHITECTURE §4.1）；`auth/storage.rs`
/// 另按变体设置 TS 的 `error.code`（ELOCKED / ERELEASED / ENOTACQUIRED / ECOMPROMISED，
/// I/O 错误取底层 errno 名）。
///
/// `Released` / `NotAcquired` 在当前仓库没有构造点，保留的是 TS 错误码的对应关系。
#[derive(Debug)]
pub enum LockError {
    /// ELOCKED：`Lock file is already being held`。
    Locked {
        /// 锁目录路径。
        path: PathBuf,
    },
    /// 文件系统错误（Node 的 `Error` + `code`）。
    Io {
        /// 出错的路径。
        path: PathBuf,
        /// 失败的系统调用（libuv 的 `syscall` 字段：`mkdir` / `stat` / `rmdir` / `utime` /
        /// `open` / `lstat` / `uv_cwd`）。
        syscall: &'static str,
        /// 底层错误。
        source: io::Error,
    },
    /// ERELEASED：`Lock is already released`。
    Released {
        /// 锁目录路径。
        path: PathBuf,
    },
    /// ENOTACQUIRED：`Lock is not acquired/owned by you`。
    NotAcquired {
        /// 锁目录路径。
        path: PathBuf,
    },
    /// ECOMPROMISED：锁被外部删除或改写。
    Compromised {
        /// 锁目录路径。
        path: PathBuf,
        /// 错误文案（默认 [`LOCK_COMPROMISED_MESSAGE`]）。
        message: String,
    },
    /// `AbortSignal` 已取消。
    Aborted,
}

impl LockError {
    /// 是否为“已被持有”（ELOCKED）。
    pub fn is_locked(&self) -> bool {
        matches!(self, LockError::Locked { .. })
    }

    /// 是否为锁被破坏（ECOMPROMISED）。
    pub fn is_compromised(&self) -> bool {
        matches!(self, LockError::Compromised { .. })
    }

    /// 底层 I/O 错误类型（非 I/O 错误 → `None`）。
    pub fn io_kind(&self) -> Option<io::ErrorKind> {
        match self {
            LockError::Io { source, .. } => Some(source.kind()),
            _ => None,
        }
    }
}

/// libuv `uv_strerror` 的文案（`uv-common.c`），只覆盖文件锁会遇到的 errno。
#[cfg(unix)]
const UNIX_ERRNO_PARTS: &[(i32, &str, &str)] = &[
    (libc::E2BIG, "E2BIG", "argument list too long"),
    (libc::EACCES, "EACCES", "permission denied"),
    (libc::EBUSY, "EBUSY", "resource busy or locked"),
    (libc::EDQUOT, "EDQUOT", "disk quota exceeded"),
    (libc::EEXIST, "EEXIST", "file already exists"),
    (libc::EFAULT, "EFAULT", "bad address"),
    (libc::EIO, "EIO", "i/o error"),
    (libc::EISDIR, "EISDIR", "illegal operation on a directory"),
    (libc::ELOOP, "ELOOP", "too many symbolic links encountered"),
    (libc::EMFILE, "EMFILE", "too many open files"),
    (libc::ENAMETOOLONG, "ENAMETOOLONG", "name too long"),
    (libc::ENFILE, "ENFILE", "file table overflow"),
    (libc::ENOENT, "ENOENT", "no such file or directory"),
    (libc::ENOMEM, "ENOMEM", "not enough memory"),
    (libc::ENOSPC, "ENOSPC", "no space left on device"),
    (libc::ENOTDIR, "ENOTDIR", "not a directory"),
    (libc::ENOTEMPTY, "ENOTEMPTY", "directory not empty"),
    (libc::EPERM, "EPERM", "operation not permitted"),
    (libc::EROFS, "EROFS", "read-only file system"),
    (libc::ESPIPE, "ESPIPE", "invalid seek"),
    (libc::ETXTBSY, "ETXTBSY", "text file busy"),
    (libc::EXDEV, "EXDEV", "cross-device link not permitted"),
];

/// Windows：`io::Error::raw_os_error()` 是 Win32 错误码；映射到 libuv 的 errno 名称与文案。
#[cfg(windows)]
const WINDOWS_ERROR_PARTS: &[(i32, &str, &str)] = &[
    (2, "ENOENT", "no such file or directory"), // ERROR_FILE_NOT_FOUND
    (3, "ENOENT", "no such file or directory"), // ERROR_PATH_NOT_FOUND
    (5, "EACCES", "permission denied"),         // ERROR_ACCESS_DENIED
    (19, "EROFS", "read-only file system"),     // ERROR_WRITE_PROTECT
    (32, "EBUSY", "resource busy or locked"),   // ERROR_SHARING_VIOLATION
    (33, "EBUSY", "resource busy or locked"),   // ERROR_LOCK_VIOLATION
    (112, "ENOSPC", "no space left on device"), // ERROR_DISK_FULL
    (145, "ENOTEMPTY", "directory not empty"),  // ERROR_DIR_NOT_EMPTY
    (183, "EEXIST", "file already exists"),     // ERROR_ALREADY_EXISTS
    (206, "ENAMETOOLONG", "name too long"),     // ERROR_FILENAME_EXCED_RANGE
    (267, "ENOTDIR", "not a directory"),        // ERROR_DIRECTORY
];

/// 把底层 `io::Error` 转成 Node/libuv 的 `(code, message)`，用于
/// `EACCES: permission denied, mkdir '/path'` 形式的 `Display`。
///
/// 匹配顺序：先按 `raw_os_error()` 查平台 errno 表（Unix 用 libc 常量，Windows 用 Win32
/// 错误码），命中时文案逐字取自 libuv；否则退回 `ErrorKind` 的粗略映射（未识别的 kind
/// 得到 `UNKNOWN`），并把 `Display` 文本尾部的 ` (os error N)` 去掉、首字母改为小写。
/// 返回的 `message` 不含 syscall 与路径，由调用方拼接。
pub(crate) fn node_io_error_parts(error: &io::Error) -> (&'static str, String) {
    #[cfg(unix)]
    if let Some(raw) = error.raw_os_error()
        && let Some((_, code, message)) = UNIX_ERRNO_PARTS.iter().find(|(errno, ..)| *errno == raw)
    {
        return (code, (*message).to_string());
    }
    #[cfg(windows)]
    if let Some(raw) = error.raw_os_error()
        && let Some((_, code, message)) =
            WINDOWS_ERROR_PARTS.iter().find(|(errno, ..)| *errno == raw)
    {
        return (code, (*message).to_string());
    }

    let code = match error.kind() {
        io::ErrorKind::NotFound => "ENOENT",
        io::ErrorKind::PermissionDenied => "EACCES",
        io::ErrorKind::AlreadyExists => "EEXIST",
        io::ErrorKind::TimedOut => "ETIMEDOUT",
        _ => "UNKNOWN",
    };
    let display = error.to_string();
    let message = display
        .rsplit_once(" (os error ")
        .map_or(display.as_str(), |(message, _)| message);
    (code, lowercase_first(message))
}

/// libuv 文案以小写字母开头（`permission denied`），Rust/strerror 以大写开头。
fn lowercase_first(message: &str) -> String {
    let mut message = message.to_string();
    if let Some(first) = message.chars().next() {
        let lowered = first.to_lowercase().to_string();
        message.replace_range(..first.len_utf8(), &lowered);
    }
    message
}

impl fmt::Display for LockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LockError::Locked { .. } => formatter.write_str("Lock file is already being held"),
            LockError::Io {
                path,
                syscall,
                source,
            } => {
                let (code, message) = node_io_error_parts(source);
                write!(
                    formatter,
                    "{code}: {message}, {syscall} '{}'",
                    path.display()
                )
            }
            LockError::Released { .. } => formatter.write_str("Lock is already released"),
            LockError::NotAcquired { .. } => {
                formatter.write_str("Lock is not acquired/owned by you")
            }
            LockError::Compromised { message, .. } => formatter.write_str(message),
            LockError::Aborted => formatter.write_str("The operation was aborted"),
        }
    }
}

impl std::error::Error for LockError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LockError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// 一次成功加锁的共享状态（同步/异步 guard 共用）。
///
/// 由 `Arc` 在 guard 与心跳线程之间共享；心跳线程只持 `Weak`，guard 释放时通过 `stop_sender`
/// 让它退出，通道断开或 `Arc` 全部释放时也会退出。各字段都是可独立解读的简单状态，
/// 没有跨字段不变式，因此互斥量中毒后仍可取回内部值继续使用（见 `lock_slot`）。
struct LockState {
    /// 锁目录。
    lock_dir: PathBuf,
    /// 是否已释放（包括被破坏后的隐式释放）。
    ///
    /// 置位后心跳线程在下一次检查时退出，`release()` 也跳过删除锁目录。
    released: AtomicBool,
    /// 是否被破坏。
    compromised: AtomicBool,
    /// 被破坏时的错误文案。
    compromised_message: Mutex<Option<String>>,
    /// 最后一次观察到/设置的锁目录 mtime。
    ///
    /// 心跳每轮先比较当前 mtime 与它，不相等即判破坏；刷新成功后再用回读值更新。
    expected_mtime: Mutex<SystemTime>,
    /// 最后一次成功刷新的时间（毫秒，Unix epoch）。proper-lockfile 的 `lock.lastUpdate`。
    ///
    /// `update_over_threshold` 用它判断心跳是否已经超出 stale 阈值。
    last_update_ms: Mutex<u64>,
    /// 通知心跳线程退出。
    ///
    /// `std::sync::mpsc` 是无界通道：`send(())` 不会阻塞；sender 被 drop 导致通道断开时
    /// 心跳线程同样会退出。
    stop_sender: Mutex<Option<Sender<()>>>,
    /// 心跳线程句柄。
    ///
    /// `spawn` 失败时这里是 `None`（错误被忽略）：该锁没有心跳，不会在锁目录被外部删除时
    /// 触发回调，只能在 mtime 过期后被接管。
    updater: Mutex<Option<JoinHandle<()>>>,
    /// 破坏回调。
    ///
    /// 构造时从选项克隆，之后不可变；只在 `mark_compromised` 中调用。
    on_compromised: Option<CompromisedCallback>,
}

/// 取互斥量；中毒（持锁线程 panic）时取回内部值继续使用。
///
/// 这里保护的是标量 / `Option` / 简单容器，不存在“写到一半”的复合状态。若改成 `unwrap()`，
/// 中毒时会在释放路径 panic：panic 发生在 `Drop` 中会中断释放，若当时正在处理另一个
/// panic 还会让进程 abort。加锁与心跳不会在持有这些互斥量时调用用户回调，正常路径不会中毒。
fn lock_slot<T>(slot: &Mutex<Option<T>>) -> MutexGuard<'_, Option<T>> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn mtime_slot(slot: &Mutex<SystemTime>) -> MutexGuard<'_, SystemTime> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn last_update_slot(slot: &Mutex<u64>) -> MutexGuard<'_, u64> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// proper-lockfile 的 `isOverThreshold = lock.lastUpdate + options.stale < Date.now()`。
///
/// `saturating_add` 防止 `last_update_ms` 接近 `u64::MAX` 时溢出；正常值来自系统时钟。
fn update_over_threshold(state: &LockState, stale_ms: u64) -> bool {
    let last_update_ms = *last_update_slot(&state.last_update_ms);
    system_now_ms() > last_update_ms.saturating_add(stale_ms)
}

impl LockState {
    fn is_compromised(&self) -> bool {
        self.compromised.load(Ordering::SeqCst)
    }

    fn compromised_error(&self) -> Option<LockError> {
        let message = self
            .compromised_message
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        message.map(|message| LockError::Compromised {
            path: self.lock_dir.clone(),
            message,
        })
    }

    /// 停止心跳并删除锁目录（忽略删除错误）。
    ///
    /// 状态转换：置 `released` → 发送停止信号并 `join` 心跳线程 → 只有 `released` 此前为
    /// false 的调用才 `remove_dir`。若锁已被 `mark_compromised` 标记，`released` 已经是
    /// true，因此不会删除锁目录。幂等：重复调用（例如 `release(self)` 之后紧接 `Drop`）
    /// 直接返回。锁目录非空时删除失败，错误被忽略；不会尝试递归删除。
    fn release(&self) {
        // 先置位再通知：心跳线程一旦看到 released 就直接退出，不会把正在进行的删除误判为破坏。
        let already_released = self.released.swap(true, Ordering::SeqCst);
        if let Some(sender) = lock_slot(&self.stop_sender).take() {
            let _ = sender.send(());
        }
        if let Some(handle) = lock_slot(&self.updater).take() {
            let _ = handle.join();
        }
        if already_released {
            return;
        }
        let _ = fs::remove_dir(&self.lock_dir);
    }

    /// 标记锁被破坏并调用回调（只调用一次）。
    ///
    /// 状态转换：`released.swap(true)` —— 若已释放/已破坏则直接返回；否则置 `compromised`、
    /// 写入文案，最后在**当前线程**（心跳线程）调用回调。置 `released` 同时让随后的
    /// `release()` 跳过删除锁目录。回调 panic 会结束心跳线程（`updater` 的 `JoinHandle`
    /// 结果在 `release` 里被忽略），但状态与文案已经写好。
    fn mark_compromised(&self) {
        if self.released.swap(true, Ordering::SeqCst) {
            return;
        }
        self.compromised.store(true, Ordering::SeqCst);
        {
            let mut slot = self
                .compromised_message
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *slot = Some(LOCK_COMPROMISED_MESSAGE.to_string());
        }
        if let Some(callback) = &self.on_compromised {
            callback(LockError::Compromised {
                path: self.lock_dir.clone(),
                message: LOCK_COMPROMISED_MESSAGE.to_string(),
            });
        }
    }
}

/// 同步锁 guard。`Drop` 等价于 `release()`（忽略错误）。
///
/// `release(self)` 消费 guard 后 `Drop` 仍会再调用一次 `LockState::release`；后者幂等，
/// 不会重复删除或阻塞。
pub struct LockGuard {
    state: Arc<LockState>,
}

impl LockGuard {
    /// 锁目录路径。
    pub fn path(&self) -> &Path {
        &self.state.lock_dir
    }

    /// 锁是否已被外部破坏。
    pub fn is_compromised(&self) -> bool {
        self.state.is_compromised()
    }

    /// 被破坏时的错误。
    pub fn compromised_error(&self) -> Option<LockError> {
        self.state.compromised_error()
    }

    /// 释放锁：停止心跳并删除锁目录（忽略删除错误，与 TS 的 `release()` 一致）。
    ///
    /// 随后的 `Drop` 会再调用一次 `LockState::release`，靠幂等性无副作用。
    pub fn release(self) {
        self.state.release();
    }
}

impl fmt::Debug for LockGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LockGuard")
            .field("path", &self.state.lock_dir)
            .field("compromised", &self.state.is_compromised())
            .finish()
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        self.state.release();
    }
}

/// 异步锁 guard。`release()` 是 async 的（TS 的 `await release()`）；`Drop` 也会释放。
///
/// 注意：`release()` 的函数体没有 `.await`，内部是同步的 `join` 心跳线程 + `remove_dir`，
/// 不做 `spawn_blocking`。在 Tokio worker 上调用会阻塞该线程直到心跳线程退出。
pub struct AsyncLockGuard {
    state: Arc<LockState>,
}

impl AsyncLockGuard {
    /// 锁目录路径。
    pub fn path(&self) -> &Path {
        &self.state.lock_dir
    }

    /// 锁是否已被外部破坏。
    pub fn is_compromised(&self) -> bool {
        self.state.is_compromised()
    }

    /// 被破坏时的错误。
    pub fn compromised_error(&self) -> Option<LockError> {
        self.state.compromised_error()
    }

    /// 释放锁（TS: `await release()`）。忽略删除错误。
    ///
    /// 签名是 `async`，但实现不切换线程也不让出执行器：删除目录与 `join` 心跳都在调用者
    /// 线程上完成（原因见类型文档）。
    pub async fn release(self) {
        self.state.release();
    }

    /// 同步释放（`notify_waiters`/blocking 调用点用）。
    pub fn release_blocking(self) {
        self.state.release();
    }
}

impl fmt::Debug for AsyncLockGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AsyncLockGuard")
            .field("path", &self.state.lock_dir)
            .field("compromised", &self.state.is_compromised())
            .finish()
    }
}

impl Drop for AsyncLockGuard {
    fn drop(&mut self) {
        self.state.release();
    }
}

/// `path.resolve(file)` 的等价物：绝对化 + 词法规范化（不解析符号链接）。
///
/// - `realpath: false`：相对路径用 `current_dir()` 拼接后再做纯词法规范化；`current_dir()`
///   失败按 syscall `uv_cwd` 上报（`path` 字段仍是原始 target）。这条路径不访问目标本身，
///   因此目标不存在也能“解析”。
/// - `realpath: true`：`fs::canonicalize` 会解析所有符号链接，目标不存在即失败。
///
/// `realpath: true` 对应 `fs.realpath`：Node 的 libuv 实现内部用 `lstat`，错误文案的
/// syscall 是 `lstat`（node 26 实测）。
fn resolve_target_path(target: &Path, realpath: bool) -> Result<PathBuf, LockError> {
    if realpath {
        return fs::canonicalize(target).map_err(|source| LockError::Io {
            path: target.to_path_buf(),
            syscall: "lstat",
            source,
        });
    }
    let absolute = if target.is_absolute() {
        target.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|source| LockError::Io {
                path: target.to_path_buf(),
                syscall: "uv_cwd",
                source,
            })?
            .join(target)
    };
    Ok(normalize_lexically(&absolute))
}

/// 去掉 `.`、折叠 `..`、保留根/前缀。
///
/// 纯词法：不查文件系统，所以在存在符号链接时 `a/../b` 未必等于真实的 `b`；这正是
/// `realpath: false` 想要的 `path.resolve` 语义。只应用于绝对路径——对相对路径，开头的
/// `..` 会因 `PathBuf::pop` 作用于空缓冲区而被丢弃。
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// `options.lockfilePath || `${file}.lock``。
///
/// 在 `OsString` 末尾追加 `.lock`（不是替换扩展名）：`auth.json` → `auth.json.lock`。
/// `lockfile_path` 原样使用，不相对 `resolved_target` 拼接、也不做规范化；相对路径按
/// 进程 cwd 解释（`create_dir` 的常规语义）。
fn lock_path(resolved_target: &Path, options: &LockOptions) -> PathBuf {
    match &options.lockfile_path {
        Some(path) => path.clone(),
        None => {
            let mut path = resolved_target.as_os_str().to_os_string();
            path.push(".lock");
            PathBuf::from(path)
        }
    }
}

/// `SystemTime` → Unix 毫秒；epoch 之前返回负数（用 `i128` 避免无符号回绕）。
fn system_time_ms(time: SystemTime) -> i128 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as i128,
        Err(error) => -(error.duration().as_millis() as i128),
    }
}

/// `isLockStale(stat, options)`：`mtime < now - stale`（严格小于）。
///
/// 恰好等于阈值不算陈旧；未来 mtime（时钟回拨或测试把时间推后）也不算。两边都用 `i128`
/// 相减，`now_ms < stale_ms` 时不会下溢。
fn is_lock_stale(mtime: SystemTime, now_ms: u64, stale_ms: u64) -> bool {
    system_time_ms(mtime) < now_ms as i128 - stale_ms as i128
}

/// 设置目录 mtime。Unix 用 `futimens`（`File::set_times`）。
///
/// 只设置 modified，不显式改动 accessed；`File::open` 以只读方式打开目录。打开或设置
/// 失败都原样返回 `io::Error`，syscall 文案由调用方决定（加锁路径记为 `utime`）。
#[cfg(unix)]
fn set_dir_mtime(path: &Path, time: SystemTime) -> io::Result<()> {
    let directory = fs::File::open(path)?;
    directory.set_times(fs::FileTimes::new().set_modified(time))
}

/// 设置目录 mtime。Windows 的 `std::fs` 不能打开目录，改用探针文件的创建/删除更新父目录 mtime。
///
/// 探针的写入与删除是两步：若进程在两步之间退出，会留下 `.kiana-lock-touch`，使锁目录
/// 非空，后续 `remove_dir`（释放或陈旧接管）都会失败。`_time` 未使用，mtime 由写入动作
/// 决定。该分支按约定只保证编译（ARCHITECTURE §8 第 26 条）。
#[cfg(windows)]
fn set_dir_mtime(path: &Path, _time: SystemTime) -> io::Result<()> {
    let probe = path.join(".kiana-lock-touch");
    fs::write(&probe, b"")?;
    fs::remove_file(&probe)
}

/// 读取路径自身（跟随符号链接）的 mtime；路径不存在时返回 `NotFound`。
fn read_mtime(path: &Path) -> io::Result<SystemTime> {
    fs::metadata(path)?.modified()
}

/// 真实系统时钟的 Unix 毫秒。注入时钟的测试钩子不经过这里：`last_update_ms` 初始化与
/// 心跳刷新始终使用真实时间（见 `start_updater`）。
fn system_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// 启动心跳线程：按 proper-lockfile 的 `updateLock` 语义校验并刷新 mtime。
///
/// - 定时器间隔为 `update_ms`（调用前已按 proper-lockfile 规则钳制）；
/// - stat/utimes 的非 ENOENT 错误在 `lastUpdate + stale` 阈值内**重试**（间隔 1 s），
///   越过阈值或 ENOENT 才 `onCompromised`；
/// - mtime 不再是自己的 → 立即 compromised（proper-lockfile 的
///   `Unable to update lock within the stale threshold`）。
///
/// 线程生命周期：启动时把 sender 存入 `stop_sender`；`LockState::release` 发送 `()` 后
/// `join` 本线程。循环靠 `recv_timeout` 等待：超时才执行一次校验，收到消息或通道断开即
/// 返回；`Weak` 升级失败（state 已释放）也返回。
///
/// 单轮成功路径：stat → 比较 `expected_mtime` → 设置 mtime → 回读并更新
/// `expected_mtime` / `last_update_ms`。失败路径：
/// - stat 或设置返回 `NotFound`（锁目录没了）→ 立即 `mark_compromised`；
/// - 其它错误 → 若 `last_update + stale` 已过阈值则 `mark_compromised`，否则按固定 1 s
///   重试（重试不累加 `update_ms`，见模块文档的“已知偏差”）。
///
/// `spawn` 失败被 `.ok()` 忽略：`updater` 为 `None`，没有心跳，也不会在锁目录被外部删除时
/// 触发回调；该锁只能在 mtime 超过 `stale` 后被下一个持有者接管。
fn start_updater(state: &Arc<LockState>, update_ms: u64, stale_ms: u64) {
    let (sender, receiver) = mpsc::channel::<()>();
    *lock_slot(&state.stop_sender) = Some(sender);

    let weak = Arc::downgrade(state);
    let update = Duration::from_millis(update_ms.max(1));
    let handle = std::thread::Builder::new()
        .name("kiana-config-lock".to_string())
        .spawn(move || {
            let mut delay = update;
            loop {
                match receiver.recv_timeout(delay) {
                    Err(RecvTimeoutError::Timeout) => {}
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                }
                let Some(state) = weak.upgrade() else {
                    return;
                };
                if state.released.load(Ordering::SeqCst) {
                    return;
                }

                // 正常 tick 恢复 update_ms 间隔；下面的错误分支会把它覆盖成 1 s 重试。
                delay = update;
                match fs::metadata(&state.lock_dir).and_then(|metadata| metadata.modified()) {
                    Ok(current) => {
                        let expected = *mtime_slot(&state.expected_mtime);
                        // mtime 是唯一判据（proper-lockfile 同样只比较 mtime）：stat 成功时
                        // 只比较 mtime，mtime 变化（外部 utimes 或重建出不同 mtime）在此判为
                        // 破坏；目录消失走下面 metadata 的 NotFound 分支处理，不依赖值比较
                        // （重建出相同 mtime 则可能漏检，见模块文档的精度说明）。
                        if current != expected {
                            state.mark_compromised();
                            return;
                        }

                        let now = SystemTime::now();
                        match set_dir_mtime(&state.lock_dir, now) {
                            Ok(()) => {
                                let observed = read_mtime(&state.lock_dir).unwrap_or(now);
                                *mtime_slot(&state.expected_mtime) = observed;
                                *last_update_slot(&state.last_update_ms) = system_now_ms();
                            }
                            Err(error) => {
                                // proper-lockfile: utimes 回调里先看 `lock.released`。
                                if state.released.load(Ordering::SeqCst) {
                                    return;
                                }
                                if error.kind() == io::ErrorKind::NotFound
                                    || update_over_threshold(&state, stale_ms)
                                {
                                    state.mark_compromised();
                                    return;
                                }
                                // 阈值内：1 s 后再试（重试期间 mtime 不再刷新）。
                                delay = Duration::from_millis(UPDATE_RETRY_DELAY_MS);
                            }
                        }
                    }
                    Err(error) => {
                        if error.kind() == io::ErrorKind::NotFound
                            || update_over_threshold(&state, stale_ms)
                        {
                            state.mark_compromised();
                            return;
                        }
                        // 非 ENOENT 的 stat 失败：1 s 后重试（阈值内）。
                        delay = Duration::from_millis(UPDATE_RETRY_DELAY_MS);
                    }
                }
            }
        })
        .ok();

    *lock_slot(&state.updater) = handle;
}

/// proper-lockfile 的 `acquireLock`：mkdir → 陈旧判定 → 拿锁并启动心跳。
///
/// 单次尝试，不做重试（重试由 `lock_sync` / `lock_async` 负责）。分支：
/// - `create_dir` 成功：设置 mtime → 回读作为 `expected_mtime` → 构造 `LockState` →
///   启动心跳。设置或回读失败会先删掉刚创建的锁目录，再返回 `Io`
///   （syscall 分别是 `utime` / `stat`）。
/// - `AlreadyExists` 且本调用尚未做过陈旧判定：`stat` 锁目录。恰好被删（`NotFound`）
///   则置 `skip_stale_check` 并重试一次循环；其它 `stat` 错误返回 `Io`（syscall `stat`）。
///   mtime 不陈旧 → `Locked`；陈旧 → `remove_dir`（`NotFound` 视为已完成）后置
///   `skip_stale_check`，再试一次 `create_dir`。
/// - `AlreadyExists` 且 `skip_stale_check == true`：直接 `Locked`，不再删除。
/// - 其它 `create_dir` 错误：返回 `Io`（syscall `mkdir`），不重试。
///
/// `now_ms` 只参与陈旧判定；`LockState::last_update_ms` 用真实时钟初始化。
fn try_acquire(
    lock_dir: &Path,
    options: &LockOptions,
    now_ms: &(dyn Fn() -> u64 + Send + Sync),
) -> Result<Arc<LockState>, LockError> {
    // 一次调用最多执行一次陈旧锁接管；重建仍失败时直接放弃，不再执行第二次删除。
    let mut skip_stale_check = false;
    let stale_ms = options.effective_stale_ms();

    loop {
        match fs::create_dir(lock_dir) {
            Ok(()) => {
                // 拿锁后立即把 mtime 归一化，并记录回读值作为“ours”的基准。
                let now = SystemTime::now();
                if let Err(error) = set_dir_mtime(lock_dir, now) {
                    let _ = fs::remove_dir(lock_dir);
                    return Err(LockError::Io {
                        path: lock_dir.to_path_buf(),
                        syscall: "utime",
                        source: error,
                    });
                }
                let expected = match read_mtime(lock_dir) {
                    Ok(mtime) => mtime,
                    Err(error) => {
                        // proper-lockfile 的 mtime 探测失败：删除锁并冒泡（syscall `stat`）。
                        let _ = fs::remove_dir(lock_dir);
                        return Err(LockError::Io {
                            path: lock_dir.to_path_buf(),
                            syscall: "stat",
                            source: error,
                        });
                    }
                };
                let state = Arc::new(LockState {
                    lock_dir: lock_dir.to_path_buf(),
                    released: AtomicBool::new(false),
                    compromised: AtomicBool::new(false),
                    compromised_message: Mutex::new(None),
                    expected_mtime: Mutex::new(expected),
                    last_update_ms: Mutex::new(system_now_ms()),
                    stop_sender: Mutex::new(None),
                    updater: Mutex::new(None),
                    on_compromised: options.on_compromised.clone(),
                });
                start_updater(&state, options.effective_update_ms(), stale_ms);
                return Ok(state);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                // 本调用已经删过一次陈旧锁：重建仍被抢先 → ELOCKED，不再删除。
                if skip_stale_check {
                    return Err(LockError::Locked {
                        path: lock_dir.to_path_buf(),
                    });
                }

                let metadata = match fs::metadata(lock_dir) {
                    Ok(metadata) => metadata,
                    Err(stat_error) if stat_error.kind() == io::ErrorKind::NotFound => {
                        // 锁目录在 stat 前被移除：跳过陈旧检查再试一次。
                        skip_stale_check = true;
                        continue;
                    }
                    Err(stat_error) => {
                        return Err(LockError::Io {
                            path: lock_dir.to_path_buf(),
                            syscall: "stat",
                            source: stat_error,
                        });
                    }
                };
                // 平台不提供 mtime（modified() 报错）时按 epoch 处理 → 该锁会被判为陈旧。
                let mtime = metadata.modified().unwrap_or(UNIX_EPOCH);
                if !is_lock_stale(mtime, now_ms(), stale_ms) {
                    return Err(LockError::Locked {
                        path: lock_dir.to_path_buf(),
                    });
                }

                // 陈旧 → 删除并重试（此后不再做陈旧判定）。
                match fs::remove_dir(lock_dir) {
                    Ok(()) => {}
                    // 另一个接管者已经删掉：等同删除成功，继续重建。
                    Err(remove_error) if remove_error.kind() == io::ErrorKind::NotFound => {}
                    Err(remove_error) => {
                        return Err(LockError::Io {
                            path: lock_dir.to_path_buf(),
                            syscall: "rmdir",
                            source: remove_error,
                        });
                    }
                }
                skip_stale_check = true;
            }
            Err(error) => {
                return Err(LockError::Io {
                    path: lock_dir.to_path_buf(),
                    syscall: "mkdir",
                    source: error,
                });
            }
        }
    }
}

/// `acquireLockSyncWithRetry` 的单次版本（`lockfile.lockSync(path, { realpath: false })`）。
///
/// 不重试：并发持有时直接返回 ELOCKED。成功返回的 [`LockGuard`] 在 drop 时释放锁。
pub fn try_lock_sync(target: &Path, options: &LockOptions) -> Result<LockGuard, LockError> {
    let state = acquire_once(target, options, &system_now_ms)?;
    Ok(LockGuard { state })
}

/// `acquireLockSyncWithRetry`：最多 10 次、每次 20 ms，只重试 ELOCKED。
///
/// 10 次是总尝试次数（含第一次），两次尝试之间睡眠 20 ms，所以最多 9 次退避。非
/// ELOCKED 错误立即返回、不消耗剩余次数；每次尝试都会重新解析路径并执行陈旧判定。
///
/// auth storage / settings storage / trust manager 的同步锁入口。
pub fn lock_sync(target: &Path, options: &LockOptions) -> Result<LockGuard, LockError> {
    let mut last_error: Option<LockError> = None;
    for attempt in 1..=SYNC_MAX_ATTEMPTS {
        match acquire_once(target, options, &system_now_ms) {
            Ok(state) => return Ok(LockGuard { state }),
            Err(error) => {
                if !error.is_locked() || attempt == SYNC_MAX_ATTEMPTS {
                    return Err(error);
                }
                last_error = Some(error);
                std::thread::sleep(Duration::from_millis(SYNC_RETRY_DELAY_MS));
            }
        }
    }

    // 循环在最后一次尝试时必然返回；这里只是保持签名完整。
    Err(last_error.unwrap_or_else(|| LockError::Locked {
        path: target.to_path_buf(),
    }))
}

/// `lockfile.lockSync(path, { realpath: false })` + 10×20 ms 重试的便捷入口。
///
/// 等价于 `lock_sync(target, &LockOptions::sync_default())`；settings storage 在需要加锁时
/// 使用它（文件已存在，或回调要求写回）。
pub fn acquire_lock_sync_with_retry(target: &Path) -> Result<LockGuard, LockError> {
    lock_sync(target, &LockOptions::sync_default())
}

/// trust manager 的变体：锁 `<target>` 目录，但锁文件是 `lockfile_path`
/// （TS: `lockSync(trustDir, { realpath: false, lockfilePath: `${path}.lock` })`）。
///
/// `target` 只参与路径解析（`realpath: false` 时甚至不访问它）；实际 `create_dir` 的是
/// `lockfile_path`。`create_dir` 不创建父目录，调用方必须先保证父目录存在（trust manager
/// 会先 `create_dir_all`）。
pub fn acquire_lock_sync_with_retry_at(
    target: &Path,
    lockfile_path: &Path,
) -> Result<LockGuard, LockError> {
    lock_sync(
        target,
        &LockOptions::sync_default().with_lockfile_path(lockfile_path),
    )
}

/// 解析目标路径 → 计算锁目录 → 单次 `try_acquire`。
///
/// 重试由调用方负责；每次尝试都会重新解析路径。`current_dir()` 失败产生的是
/// `Io`（syscall `uv_cwd`），不是 ELOCKED，`lock_sync`/`lock_async` 通常会立即
/// 返回该错误而不是反复重试。
fn acquire_once(
    target: &Path,
    options: &LockOptions,
    now_ms: &(dyn Fn() -> u64 + Send + Sync),
) -> Result<Arc<LockState>, LockError> {
    let resolved = resolve_target_path(target, options.realpath)?;
    let lock_dir = lock_path(&resolved, options);
    try_acquire(&lock_dir, options, now_ms)
}

/// `lockfile.lock(path, { retries: 0, stale })` 的单次版本。
pub async fn try_lock_async(
    target: &Path,
    options: &LockOptions,
) -> Result<AsyncLockGuard, LockError> {
    try_lock_async_with_clock(target, options, &system_now_ms).await
}

/// 注入时钟的单次异步加锁。**test hook, not public API**。
///
/// `now_ms` 只用于陈旧判定；它不参与异步重试预算（预算是 `lock_async_with_clock` 的事）。
#[doc(hidden)]
pub async fn try_lock_async_with_clock(
    target: &Path,
    options: &LockOptions,
    now_ms: &(dyn Fn() -> u64 + Send + Sync),
) -> Result<AsyncLockGuard, LockError> {
    let state = acquire_once(target, options, now_ms)?;
    Ok(AsyncLockGuard { state })
}

/// `auth-storage.ts` 的 `acquireLockAsync(signal, onCompromised)` 等价物。
///
/// 以 `options.stale_ms` 为总预算（默认 [`ASYNC_STALE_MS`]），只重试 ELOCKED（Windows 上另有
/// EPERM，最多 [`MAX_EPERM_RETRIES`] 次），指数退避且带抖动。`signal` 取消 → [`LockError::Aborted`]。
///
/// 失败语义：不可重试的错误立即原样返回；预算耗尽返回最后一次错误（通常是 ELOCKED）；
/// 等待退避期间被取消同样返回 [`LockError::Aborted`]。
pub async fn lock_async(
    target: &Path,
    options: &LockOptions,
    signal: Option<&CancellationToken>,
) -> Result<AsyncLockGuard, LockError> {
    lock_async_with_clock(target, options, signal, &system_now_ms).await
}

/// 注入时钟的 [`lock_async`]。**test hook, not public API**。
///
/// 每轮顺序：
/// 1. 先查取消 → `Aborted`；然后执行一次 `acquire_once`。
/// 2. 成功后**再查一次**取消：已取消则先 `release()` 删除可能已创建的锁目录，再返回
///    `Aborted`（对应 TS 的 `if (signal?.aborted) { await release(); throwIfAborted(); }`）。
/// 3. 失败后查取消；可重试条件是 ELOCKED，或 Windows 上的 `PermissionDenied`。后者用
///    编译期常量 `cfg!(windows)` 判断，其它平台不重试 EPERM。
/// 4. `remaining = deadline - now`；为 0 立即返回。EPERM 计数超过
///    [`MAX_EPERM_RETRIES`] 也返回。
/// 5. 退避 `round(min(10·2^retry, 1000) · (1 + jitter))` 并截到 `remaining`，用
///    `kiana_core::sleep` 等待；等待期间取消则返回 `Aborted`。
///
/// 时间语义：`deadline = now_ms() + options.stale_ms` 使用**原始** stale（不经过
/// [`MIN_STALE_MS`] 钳制），与 TS 的 `deadline = Date.now() + staleMs` 一致；锁核心的
/// 陈旧阈值仍按钳制后的值。`now_ms` 只影响预算与陈旧判定，心跳与 `last_update_ms` 使用
/// 真实系统时钟。
#[doc(hidden)]
pub async fn lock_async_with_clock(
    target: &Path,
    options: &LockOptions,
    signal: Option<&CancellationToken>,
    now_ms: &(dyn Fn() -> u64 + Send + Sync),
) -> Result<AsyncLockGuard, LockError> {
    let deadline = now_ms().saturating_add(options.stale_ms);
    let mut retry: u32 = 0;
    let mut eperm_retries: u32 = 0;

    loop {
        if signal.is_some_and(CancellationToken::is_cancelled) {
            return Err(LockError::Aborted);
        }

        match acquire_once(target, options, now_ms) {
            Ok(state) => {
                if signal.is_some_and(CancellationToken::is_cancelled) {
                    // TS: `if (signal?.aborted) { await release(); signal.throwIfAborted(); }`
                    state.release();
                    return Err(LockError::Aborted);
                }
                return Ok(AsyncLockGuard { state });
            }
            Err(error) => {
                if signal.is_some_and(CancellationToken::is_cancelled) {
                    return Err(LockError::Aborted);
                }

                // `getRetryableLockErrorCode`：ELOCKED 总是可重试；EPERM 仅在 Windows 上可重试。
                let eperm = cfg!(windows)
                    && matches!(
                        &error,
                        LockError::Io { source, .. }
                            if source.kind() == io::ErrorKind::PermissionDenied
                    );
                // 退避 sleep 的等待时长被截到 `remaining`，因此退避总等待不超过 deadline；
                // 单次 `acquire_once` 是同步文件系统调用，循环顶部不检查剩余预算，sleep 到
                // deadline 后仍会再执行一次 acquire_once，实际耗时可能越过 deadline。
                let remaining = deadline.saturating_sub(now_ms());
                if (!error.is_locked() && !eperm) || remaining == 0 {
                    return Err(error);
                }
                if eperm {
                    eperm_retries += 1;
                    if eperm_retries > MAX_EPERM_RETRIES {
                        return Err(error);
                    }
                }

                // `min(10 * 2 ** retry, maxDelayMs / 2) * (1 + Math.random())`，再截到剩余预算。
                let base = std::cmp::min(
                    10u64.saturating_mul(1u64 << retry.min(20)),
                    ASYNC_MAX_BASE_DELAY_MS,
                );
                retry += 1;
                let delay = js_round(base as f64 * (1.0 + jitter())).min(remaining as f64) as u64;
                if sleep(Duration::from_millis(delay), signal).await.is_err() {
                    return Err(LockError::Aborted);
                }
            }
        }
    }
}

/// `Math.random()` 的替代：用时钟纳秒做 [0, 1) 的伪抖动（退避只需避免同步惊群）。
///
/// 取真实时钟的亚秒纳秒再截断到百万分之一：只有 1e6 个离散值，且不保证独立同分布；
/// 仅用于打散重试，不用于任何需要随机性的场景。
fn jitter() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| f64::from(duration.subsec_nanos() % 1_000_000) / 1_000_000.0)
        .unwrap_or(0.0)
}

/// JS `Math.round`（.5 向 +∞）。
fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}
