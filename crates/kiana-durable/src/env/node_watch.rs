//! env/node-watch.ts。
//!
//! `fs.watch` 的 `notify` 等价物与轮询回退。TS 里 `NodeFileWatcher` 用快照 diff + 原生事件：
//! 原生事件只触发一次去抖后的重扫，变化是两次快照的差异加上事件路径，因此被替换的文件、
//! 被改名或重建的祖先、连同内容一起创建的目录都不依赖操作系统发来的具体事件。
//!
//! # 与 TS 的结构差异
//!
//! TS 用一个 `setTimeout` 事件循环驱动 `#flush`；Rust 侧由一个 tokio 任务（"driver"）持有
//! `State`，用 `tokio::select!` 在「notify 事件 / 去抖到期 / 轮询到期 / darwin settle 到期 /
//! 关闭」之间等待。扫描与 diff 是同步的（`std::fs`），语义与 TS 的异步版本一致：一次扫描是
//! `readdir` + `lstat` 的顺序调用。`#flush` 的 `#running`/`#dirty` 重入保护在同步实现下不需要。
//!
//! `notify` 用**单个** `RecommendedWatcher` 覆盖所有目录（TS 每个目录一个 `fs.watch`）；
//! Linux 上仍逐目录 `NonRecursive`（与 TS 相同），macOS/Windows 上每个 target 一个递归
//! watcher。watcher 错误事件按 `error.paths` 移除对应安装项，然后触发重扫。
//!
//! # 执行流程
//!
//! 1. [`NodeFileWatcher::open`] 用调用方给的 `resolve_path` 把每个
//!    [`crate::env::WatchTarget`] 变成快照键，随后决定模式：显式 `options.mode` 优先；
//!    否则 Windows 或"任意 target 位于不可靠文件系统"时用 `Polling`，其余用 `Native`。
//! 2. `Native` 下创建 `RecommendedWatcher`；失败不在这里报错，留给首轮同步去降级。
//! 3. 首轮 `State::sync`（`report = false`）扫描并安装 watcher；只要本轮装了新 watcher
//!    就再扫一轮，使快照覆盖到"watcher 生效之前"的写入。首轮的错误直接作为 `open` 的
//!    `Err(FileError)`，不会启动 driver。
//! 4. `tokio::spawn` 出 driver，此后扫描、去抖与回调都发生在这个任务里。
//! 5. driver 每轮用 `select!` 等三类事件：关闭令牌、notify 消息、最早的定时器（去抖 50 ms /
//!    轮询间隔 / macOS settle 500 ms）。到期后重扫 → 与上次快照 diff → 并入事件路径 →
//!    按 UTF-16 码元排序 → 投递一次 `Paths`；扫描失败则投递 `Error` 并停止。
//!
//! # 重要约束
//!
//! - 扫描是同步的 `std::fs` 调用：首轮在 `open` 的调用者任务上执行，之后在 driver 任务上
//!   执行。扫描期间该任务不处理新事件或取消；`close` 要等当前一轮扫描结束。
//! - 回调也在 driver 任务里同步调用（首轮同步期间的 `Overflow` 例外，发生在 `open` 返回
//!   前）。回调 panic 会被 `catch_unwind` 吞掉；但回调里长时间阻塞会推迟事件处理。
//! - 快照键与上报路径都保持 `resolve_path` 的输出形式；内部拼接使用 `platform_sep()`
//!   （Windows 为 `\`，其余为 `/`）。
//! - 关闭后 `deliver` 直接丢弃变化；`close` 幂等，driver 退出后不会再触发回调。
//!
//! # 阅读顺序
//!
//! `NodeWatchOptions` → `NodeFileWatcher::open` → `Scanner::run`（快照里有什么）→
//! `State::sync` / `State::reconcile_watchers`（watcher 的安装与移除）→ `State::flush`
//! （如何形成一次上报）→ `run_driver`（定时器与事件循环）。

use std::collections::{HashMap, HashSet};
use std::path::Path as StdPath;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use kiana_chord::context::Context;
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::env::node::{node_io_message, to_file_error_io};
use crate::env::path::{posix, win32};
use crate::env::{
    FileError, FileErrorCode, FileWatcher, WatchCallback, WatchChange, WatchMode, WatchTarget,
};

/// `NodeWatchOptions`：`NodeExecutionEnv` 如何观察文件系统。
///
/// 由 `NodeExecutionEnv` 的 `watch_options` 字段带入 `watch()`；每个字段都是可选的覆盖，
/// `None` 表示使用本文件的默认值。watcher 打开时读取一次，之后不再变化。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeWatchOptions {
    /// 强制模式。默认在 Windows（原生 watcher 保持目录打开，会阻塞其父目录改名）以及不报告
    /// 远端变化的文件系统上选择 `polling`。
    ///
    /// `None` 时由 `open` 决定：Windows 或目标位于不报告远端变化的文件系统上 →
    /// [`crate::env::WatchMode::Polling`]，否则 `Native`。运行中只可能从 `Native` 降级为
    /// `Polling`（原生 watcher 创建或安装失败），不会反向切换。
    pub mode: Option<WatchMode>,
    /// `polling` 模式下两次快照的间隔；默认 2000 ms。
    ///
    /// 非有限、非正或大于 2 147 483 647 的值按 1 ms 处理（对齐 Node `setInterval` 的钳制），
    /// 避免把 NaN/负数直接交给 `Duration::from_secs_f64`。`native` 模式忽略该值。
    pub poll_interval_ms: Option<f64>,
    /// 一个 watcher 覆盖的目录上限；默认 10 000。
    ///
    /// 统计的是所有 target 去重后的目录数（target 根加上递归扫描到的子目录）。超限时：
    /// 首次扫描让 `open` 返回 `invalid`，之后的扫描投递 [`crate::env::WatchChange::Error`]。
    pub max_directories: Option<usize>,
}

/// 解析后的监视目标。
///
/// `path` 是调用方 `resolve_path` 的输出（`NodeExecutionEnv` 会解析成基于 cwd 的绝对路径），
/// 也是快照与事件的键。`recursive` 只有 `WatchTarget.recursive == Some(true)` 才为真；
/// `hidden`/`names` 来自 `exclude`。排除只按条目名匹配，且被排除的目录不会下钻。
#[derive(Debug, Clone)]
struct ResolvedTarget {
    path: String,
    recursive: bool,
    hidden: bool,
    names: HashSet<String>,
}

/// 快照对单个路径的记忆。目录与祖先只比较身份。
///
/// 种类来自 `lstat`（目录列举）或跟随链接的 `stat`（target 自身）：target 若是指向目录的
/// 符号链接，记录的是 `Directory` 而非 `Symlink`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// 一个路径的完整比较键。
///
/// - `dev`/`ino`：身份，用于发现"同一路径换了一个文件/目录"；
/// - `size`/`mtime_ms`：只在非目录上有意义（`entry_of` 把目录清零）；
/// - `hash`：仅在 polling 模式下、对最近修改的小文件计算，用于发现时间戳粒度内的内容变化。
#[derive(Debug, Clone, PartialEq)]
struct Entry {
    kind: EntryKind,
    dev: u64,
    ino: u64,
    size: u64,
    mtime_ms: f64,
    hash: Option<String>,
}

/// 一次扫描：快照，以及"指向文件的符号链接"的 target（其文件需要单独的 watcher）。
///
/// `snapshot` 包含每个 target 自身（跟随符号链接）、它已存在的祖先（只记身份），以及目录
/// target 下未被排除的条目；只有递归 target 才会继续下钻。`linked_files` 只收"target 本身
/// 是指向文件的符号链接"的情况；递归扫描途中遇到的符号链接不会被跟随。
struct Scan {
    snapshot: HashMap<String, Entry>,
    linked_files: HashSet<String>,
}

/// 已安装 watcher 的路径身份：路径被另一个文件替换时需要新 watcher。
///
/// 用安装时快照里的 `dev`/`ino` 与当前快照比较。非 Unix 平台这两个值恒为 0
/// （见 `metadata_dev`/`metadata_ino`），身份比较因此无法发现同路径替换。
#[derive(Debug, Clone, Copy)]
struct Installed {
    dev: u64,
    ino: u64,
}

/// 首个相关 notify 事件到重扫之间的固定延迟：同一批写入通常落在窗口内，只扫一次。
/// 窗口内再来事件不会顺延（`run_driver` 只在 `flush_at` 为空时设置），因此持续写入时两次
/// 去抖重扫至少相隔 50 ms；poll/settle 触发的扫描不受此限。
const DEBOUNCE_MS: u64 = 50;
/// macOS 上 `fs.watch` 在 libuv 的 FSEvents 流生效前就返回，其间发生的变化不会被报告；
/// 安装 watcher 后这么久再重扫一次可以补上。
const FSEVENTS_SETTLE_MS: u64 = 500;
/// 未指定 `poll_interval_ms` 时的轮询间隔（毫秒）。
const DEFAULT_POLL_MS: f64 = 2000.0;
/// 未指定 `max_directories` 时的目录预算。
const DEFAULT_MAX_DIRECTORIES: usize = 10_000;
/// `polling` 模式下，最近修改的小文件还比较内容：文件系统时间戳粒度内的第二次写入可能
/// 保持 size 与 mtime 不变。
const HASH_MAX_BYTES: u64 = 256 * 1024;
/// 计算内容哈希的时间窗口：`mtime` 距现在小于该值（毫秒）才算"最近修改"。
const HASH_RECENT_MS: f64 = 5000.0;

/// 接受 watch 但不报告别处变化的文件系统（Linux `statfs` 的 magic number）。
const UNRELIABLE_FILE_SYSTEMS: &[u64] = &[
    0x6969,     // NFS
    0x517b,     // SMB
    0xff534d42, // CIFS
    0xfe534d42, // SMB2
    0x65735546, // FUSE（sshfs、Android shared storage）
    0x01021997, // 9P（WSL2 Windows 盘）
    0x0bd00bd0, // Lustre
    0x47504653, // GPFS
    0x00c36400, // Ceph
    0x5346414f, // OpenAFS
    0x6b414653, // kAFS
    0x5dca2df5, // sdcardfs
];

/// 取锁；锁中毒时取回内部值继续，而不是把 panic 传播给调用方（关闭流程不应因此失败）。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn platform_sep() -> &'static str {
    if cfg!(windows) { "\\" } else { "/" }
}

/// 使用本平台 `node:path` 语义的 `dirname`（Windows 走 win32，其余走 posix）。
fn platform_dirname(path: &str) -> String {
    if cfg!(windows) {
        win32::dirname(path)
    } else {
        posix::dirname(path)
    }
}

/// 使用本平台 `node:path` 语义的 `path.relative`。
///
/// 走平台默认包装（Windows 下用进程 cwd）而不是传空 cwd：`win32::relative_with_cwd` 的
/// 前置条件是 cwd 为绝对路径，传空串时只要任一侧是相对路径/根相对盘符路径，内部的
/// `resolve_string_with_cwd` 就会索引空串 panic。快照键通常是绝对路径，此时 cwd 不参与
/// 比较，结果与空 cwd 相同；万一出现相对路径，也退回 Node `path.relative` 的正常语义。
fn platform_relative(from: &str, to: &str) -> String {
    if cfg!(windows) {
        win32::relative(from, to)
    } else {
        posix::relative_with_cwd(from, to, "")
    }
}

/// `entryOf(stats, hash)`。
///
/// 目录的 `size`/`mtime_ms` 强制为 0：目录时间戳会随无关兄弟条目的增删/改名变化，
/// 比较它会让无关写入也产生 diff；在 Unix 上目录的替换由 `dev`/`ino` 捕获，非 Unix
/// 二者恒为 0（见 `metadata_dev`/`metadata_ino`），无法发现同路径替换。
fn entry_of(metadata: &std::fs::Metadata, hash: Option<String>) -> Entry {
    let kind = entry_kind(metadata);
    let directory = kind == EntryKind::Directory;
    Entry {
        kind,
        dev: metadata_dev(metadata),
        ino: metadata_ino(metadata),
        size: if directory { 0 } else { metadata.len() },
        mtime_ms: if directory {
            0.0
        } else {
            mtime_millis(metadata)
        },
        hash,
    }
}

fn entry_kind(metadata: &std::fs::Metadata) -> EntryKind {
    let file_type = metadata.file_type();
    if file_type.is_file() {
        EntryKind::File
    } else if file_type.is_dir() {
        EntryKind::Directory
    } else if file_type.is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::Other
    }
}

#[cfg(unix)]
fn metadata_dev(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.dev()
}

#[cfg(unix)]
fn metadata_ino(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.ino()
}

/// 非 Unix 平台没有可用的 `st_dev`：恒为 0，`Installed` 的身份比较因此无法发现同路径替换。
#[cfg(not(unix))]
fn metadata_dev(_metadata: &std::fs::Metadata) -> u64 {
    0
}

/// 非 Unix 平台没有可用的 `st_ino`：恒为 0。
#[cfg(not(unix))]
fn metadata_ino(_metadata: &std::fs::Metadata) -> u64 {
    0
}

/// `stats.mtimeMs`：Unix 毫秒（浮点，保留小数）。
///
/// `modified()` 失败时返回 0.0；早于 `UNIX_EPOCH` 的时间返回负值（与 JS 的毫秒时间戳一致）。
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

fn now_millis() -> f64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => {
            duration.as_secs() as f64 * 1000.0 + duration.subsec_nanos() as f64 / 1_000_000.0
        }
        Err(_) => 0.0,
    }
}

fn same_entry(a: &Entry, b: &Entry) -> bool {
    a.kind == b.kind
        && a.dev == b.dev
        && a.ino == b.ino
        && a.size == b.size
        && a.mtime_ms == b.mtime_ms
        && a.hash == b.hash
}

/// `path` 的祖先，最近的在前，直到根。
///
/// 包含根（`dirname` 不再变化的那一级，posix 上是 `/` 或相对路径的 `.`）；不要求这些祖先
/// 在文件系统上存在，调用方对 `lstat` 失败的结果直接忽略。
fn ancestors_of(path: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = platform_dirname(path);
    loop {
        result.push(current.clone());
        let parent = platform_dirname(&current);
        if parent == current {
            return result;
        }
        current = parent;
    }
}

/// `path` 是 `ancestor` 或在它之下。
///
/// 比较前补分隔符，避免 `/a` 误匹配 `/ab`；`ancestor` 已带尾分隔符时直接做前缀比较。
fn is_within(path: &str, ancestor: &str) -> bool {
    if path == ancestor {
        return true;
    }
    let separator = platform_sep();
    if ancestor.ends_with(separator) {
        path.starts_with(ancestor)
    } else {
        path.starts_with(&format!("{ancestor}{separator}"))
    }
}

/// 条目名是否被 target 排除：点号开头的隐藏名，或名单里的精确名字。
///
/// 只在列举某一层时按条目名判断；被排除的目录不会被下钻，因此它下面的整棵子树都不会
/// 进入快照，也不会被上报。
fn excluded(target: &ResolvedTarget, name: &str) -> bool {
    (target.hidden && name.starts_with('.')) || target.names.contains(name)
}

/// 拒绝访问：`ErrorKind::PermissionDenied`，或原始错误码为 1 / 13。
///
/// 测试用 EACCES=13 锁定后者（`flush_errors_use_node_messages_without_a_path_field`）。
/// 目录列举中的拒绝被当作"跳过"；target 根上的拒绝会让扫描失败，并在
/// `scan_error_for_open` / `scan_error_for_flush` 中映射为 `permission_denied`
/// （`scan_errors_keep_the_failing_target_path` 锁定 open 阶段）。
fn is_denied(error: &std::io::Error) -> bool {
    matches!(error.kind(), std::io::ErrorKind::PermissionDenied)
        || matches!(error.raw_os_error(), Some(1) | Some(13))
}

/// `[...changed].sort()`：`Array.prototype.sort` 的默认比较是 UTF-16 码元顺序，不是 UTF-8
/// 字节顺序（例如 U+1F600 的代理对 D83D DE00 排在 U+E000 之前，而 UTF-8 字节相反）。
///
/// 只在投递 `WatchChange::Paths` 前调用；`diff` 的输出顺序来自 `HashMap` 遍历，本身不稳定。
fn sort_paths_utf16(paths: &mut [String]) {
    paths.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
}

/// 扫描错误：目录预算、I/O 失败。I/O 错误带上触发它的 syscall 与路径，让消息和
/// `FileError.path` 与 TS 抛出的 Node 错误一致。
///
/// `ScanError::Io` 在 `open` 与后续 `flush` 里映射成不同的 `FileError`：前者保留
/// `path`，后者不带 `path`（见下面两个转换函数）；`ScanError::Budget` 在两处都不带
/// `path`。
#[derive(Debug)]
enum ScanError {
    Budget(usize),
    Io {
        error: std::io::Error,
        syscall: &'static str,
        path: String,
    },
}

impl ScanError {
    fn io(error: std::io::Error, syscall: &'static str, path: &str) -> Self {
        ScanError::Io {
            error,
            syscall,
            path: path.to_owned(),
        }
    }
}

/// `open()` 阶段：目录预算 → `FileError("invalid", message)`；其余按 `toFileError` 映射
/// （Node 错误带 `path`，因此 `FileError.path` 是出错的 target 路径）。
fn scan_error_for_open(error: ScanError) -> FileError {
    match error {
        ScanError::Budget(max) => FileError::new(
            FileErrorCode::Invalid,
            format!("Watched paths exceed {max} directories"),
        ),
        ScanError::Io {
            error,
            syscall,
            path,
        } => to_file_error_io(&error, syscall, Some(&path)),
    }
}

/// `#flush` 阶段：`isDenied` → `permission_denied`，否则 `invalid`；消息取原始错误的
/// message（含 syscall 与路径），`FileError` 不带 `path`（TS 的 `new FileError(code, message)`）。
fn scan_error_for_flush(error: ScanError) -> FileError {
    match error {
        ScanError::Budget(max) => FileError::new(
            FileErrorCode::Invalid,
            format!("Watched paths exceed {max} directories"),
        ),
        ScanError::Io {
            error,
            syscall,
            path,
        } => {
            let code = if is_denied(&error) {
                FileErrorCode::PermissionDenied
            } else {
                FileErrorCode::Invalid
            };
            FileError::new(code, node_io_message(&error, syscall, Some(&path), None))
        }
    }
}

/// 一次扫描的构建器（对应 `#scan` 里的局部集合与闭包）。
///
/// - `snapshot` / `linked_files`：本次扫描的产物；
/// - `counted`：跨 target 去重的目录预算集合，决定何时返回 `ScanError::Budget`；
/// - `traversed`：`(target 下标, 目录)` 的遍历去重。同一目录在不同 target 下仍会按各自的
///   递归与排除规则重新遍历，因此键里带 target 下标；
/// - `listed`：已经 `lstat` 过的条目种类缓存，重叠 target 看到同一路径时复用，不再 syscall。
struct Scanner<'a> {
    state: &'a State,
    snapshot: HashMap<String, Entry>,
    linked_files: HashSet<String>,
    counted: HashSet<String>,
    traversed: HashSet<String>,
    listed: HashMap<String, EntryKind>,
}

impl<'a> Scanner<'a> {
    fn new(state: &'a State) -> Self {
        Scanner {
            state,
            snapshot: HashMap::new(),
            linked_files: HashSet::new(),
            counted: HashSet::new(),
            traversed: HashSet::new(),
            listed: HashMap::new(),
        }
    }

    /// 把目录计入预算；去重后超过 `max_directories` 即中止整次扫描。
    ///
    /// target 根无条件计数（即使非递归）；子目录只在递归 target 下计数。
    fn count_directory(&mut self, path: &str) -> Result<(), ScanError> {
        self.counted.insert(path.to_owned());
        if self.counted.len() > self.state.max_directories {
            return Err(ScanError::Budget(self.state.max_directories));
        }
        Ok(())
    }

    /// 把一个路径写入快照；polling 模式下顺带为"最近修改的小文件"计算内容哈希。
    ///
    /// 哈希只用来捕获时间戳粒度内的第二次写入。读取失败（权限、文件已消失）时 `hash`
    /// 为 `None`，该文件仍按 size/mtime 比较，不会让扫描失败。
    fn record(&mut self, path: &str, metadata: &std::fs::Metadata) {
        let hash = if self.state.mode == WatchMode::Polling
            && metadata.is_file()
            && metadata.len() <= HASH_MAX_BYTES
            && now_millis() - mtime_millis(metadata) < HASH_RECENT_MS
        {
            std::fs::read(path).ok().map(|content| hex_digest(&content))
        } else {
            None
        };
        self.snapshot
            .insert(path.to_owned(), entry_of(metadata, hash));
    }

    /// 列出一层目录并把条目写进快照；递归 target 继续下钻。
    ///
    /// 同一个 `(index, directory)` 只处理一次；不同 target 对同一目录会各自按自己的规则
    /// 重新遍历。错误策略：target 根被拒绝访问 → 整次扫描失败；其余 `NotFound` /
    /// `PermissionDenied` / `NotADirectory` 视为"扫描期间消失或不可读"而跳过；其他 I/O
    /// 错误向上传播。
    fn scan_directory(
        &mut self,
        index: usize,
        target: &ResolvedTarget,
        directory: &str,
    ) -> Result<(), ScanError> {
        // 按 target 区分遍历：重叠 target 的递归与排除规则可能不同。
        let key = format!("{index}\0{directory}");
        if !self.traversed.insert(key) {
            return Ok(());
        }
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) => {
                // 被监视目录本身必须可读；其下的不可读目录跳过。
                if directory == target.path && is_denied(&error) {
                    return Err(ScanError::io(error, "scandir", directory));
                }
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound
                        | std::io::ErrorKind::PermissionDenied
                        | std::io::ErrorKind::NotADirectory
                ) {
                    return Ok(());
                }
                return Err(ScanError::io(error, "scandir", directory));
            }
        };
        let separator = platform_sep();
        for entry in entries {
            // 单个条目的读取错误只在目录消失/无权限时跳过，其他错误中止扫描。
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                    ) {
                        continue;
                    }
                    return Err(ScanError::io(error, "scandir", directory));
                }
            };
            // 文件名经 `to_string_lossy` 变成字符串：非 UTF-8 名字可能对不上真实路径，
            // 紧随其后的 `lstat` 失败时该条目被跳过。
            let name = entry.file_name().to_string_lossy().into_owned();
            if excluded(target, &name) {
                continue;
            }
            let path = if directory.ends_with(separator) {
                format!("{directory}{name}")
            } else {
                format!("{directory}{separator}{name}")
            };
            let mut kind = self.listed.get(&path).copied();
            if kind.is_none() {
                // `listed` 跨 target 共享：第一次见到这个路径时用 `lstat` 定种类（不跟随
                // 符号链接），重复见到时直接复用。
                let metadata = match std::fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(_) => continue,
                };
                let discovered = entry_kind(&metadata);
                self.listed.insert(path.clone(), discovered);
                // target 自身的条目（跟随链接）优先于另一个 target 对它的列举。
                if !self.snapshot.contains_key(&path) {
                    self.record(&path, &metadata);
                }
                kind = Some(discovered);
            }
            // 只有递归 target 下钻；符号链接即使指向目录（kind == Symlink）也不跟随。
            if target.recursive && kind == Some(EntryKind::Directory) {
                self.count_directory(&path)?;
                self.scan_directory(index, target, &path)?;
            }
        }
        Ok(())
    }

    /// 构建一次完整快照：target 自身、已存在的祖先、目录内容。
    ///
    /// 每个 target 的处理顺序：
    /// 1. 由近到远记录已存在的祖先，只保留身份（size/mtime 清零）——祖先的时间戳会随同一
    ///    目录里的无关兄弟条目变化，比较它会产生噪声；
    /// 2. `stat` target 本身（跟随符号链接）：被拒绝 → 整次扫描失败；其他错误（通常是不存在）
    ///    → 跳过，表示观察它的"出现"；
    /// 3. 记录 target；若它是"指向文件的符号链接"，加入 `linked_files`；
    /// 4. 目录 target 计入预算并列举；仅 `target.recursive` 为真时继续下钻。
    ///
    /// 快照里同一路径一般以先写入者为准；例外是 target 自身：它无条件覆盖之前某个 target
    /// 递归列举时留下的 `lstat` 结果（跟随链接与不跟随的区别）。
    fn run(mut self) -> Result<Scan, ScanError> {
        let targets = self.state.targets.clone();
        for (index, target) in targets.iter().enumerate() {
            // 阶段 1：祖先只比较身份；缺失的祖先忽略，`ancestors_of` 仍会走到文件系统根。
            for ancestor in ancestors_of(&target.path) {
                if self.snapshot.contains_key(&ancestor) {
                    continue;
                }
                // 只比较身份：祖先自己的时间戳会随无关的兄弟条目变化。
                if let Ok(metadata) = std::fs::symlink_metadata(&ancestor) {
                    let mut entry = entry_of(&metadata, None);
                    entry.size = 0;
                    entry.mtime_ms = 0.0;
                    self.snapshot.insert(ancestor, entry);
                }
            }
            // 阶段 2：target 本身可能是符号链接：跟随它。缺失的 target 被监视其"出现"；因权限不可达则失败。
            let metadata = match std::fs::metadata(&target.path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    if is_denied(&error) {
                        return Err(ScanError::io(error, "stat", &target.path));
                    }
                    continue;
                }
            };
            // 阶段 3：target 自身的条目总是覆盖之前的记录（`metadata` 来自跟随链接的 stat）。
            self.record(&target.path, &metadata);
            if metadata.is_file()
                && std::fs::symlink_metadata(&target.path)
                    .map(|link| link.file_type().is_symlink())
                    .unwrap_or(false)
            {
                // 链接文件的变化不会出现在链接所在目录的事件里，需要单独观察链接路径。
                self.linked_files.insert(target.path.clone());
            }
            // 阶段 4：目录 target 计入预算并列举（预算从这里开始计）；仅 `target.recursive`
            // 为真时继续下钻。
            if metadata.is_dir() {
                self.count_directory(&target.path)?;
                let target = target.clone();
                self.scan_directory(index, &target, &target.path)?;
            }
        }
        Ok(Scan {
            snapshot: self.snapshot,
            linked_files: self.linked_files,
        })
    }
}

/// SHA-256 的小写十六进制表示，对应 Node `createHash("sha256").digest("hex")`。
fn hex_digest(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let digest = hasher.finalize();
    let mut text = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(text, "{byte:02x}");
    }
    text
}

// ---------------------------------------------------------------------------
// watcher 状态
// ---------------------------------------------------------------------------

/// 句柄与 driver 之间共享的少量状态。
///
/// - `mode`：让 `NodeFileWatcher::mode()` 不触碰 driver 就能读到当前模式（driver 内另有
///   同名的普通字段，切换时必须同时更新两处）；
/// - `closed`：回调、扫描与 driver 循环的快速退出标志；
/// - `shutdown`：`close` 用它唤醒阻塞在 `select!` 里的 driver。
///
/// `mode`/`closed` 用 `SeqCst` 读写；`shutdown` 是 `CancellationToken`（`cancel`/`cancelled`）。
struct Shared {
    mode: AtomicU8,
    closed: AtomicBool,
    shutdown: CancellationToken,
}

/// `WatchMode` 在 `AtomicU8` 里的编码；`mode_from_u8` 把其他值都当作 `Native`。
fn mode_to_u8(mode: WatchMode) -> u8 {
    match mode {
        WatchMode::Native => 0,
        WatchMode::Polling => 1,
    }
}

fn mode_from_u8(value: u8) -> WatchMode {
    if value == 1 {
        WatchMode::Polling
    } else {
        WatchMode::Native
    }
}

/// notify 线程送来的原始事件（成功或错误）。
enum DriverMessage {
    Fs(notify::Result<Event>),
}

/// `reconcile_watchers` 的结果。
enum ReconcileOutcome {
    /// 是否至少新增了一个 watcher；`true` 时 `sync` 会再扫一轮。
    Added(bool),
    /// 无法安装原生 watcher，调用方应切换到轮询。
    SwitchToPolling,
}

/// driver 独占的可变状态。
///
/// 除 `shared` 外不跨任务共享：driver 启动后由它独占，所有方法都在 driver 任务里调用
/// （例外是 `open` 的首轮 `sync`，发生在 `open` 的调用者任务上），因此没有内部锁。
struct State {
    shared: Arc<Shared>,
    targets: Vec<ResolvedTarget>,
    on_change: WatchCallback,
    poll_interval: Duration,
    max_directories: usize,
    mode: WatchMode,
    /// notify watcher 本身；polling 模式或切换后为 `None`。
    watcher: Option<RecommendedWatcher>,
    /// watcher 路径 → 安装时看到的目录身份。
    installed: HashMap<String, Installed>,
    /// 上一次成功扫描的快照；diff 的基线。
    snapshot: HashMap<String, Entry>,
    /// 上一次扫描中"指向文件的符号链接" target。
    linked_files: HashSet<String>,
    /// notify 事件已经指出、但尚未随 `Paths` 投递的上报路径。
    events: HashSet<String>,
    /// macOS 上本轮新增 watcher 后由 `sync` 置位，driver 据此安排一次 settle 重扫。
    settle_requested: bool,
}

impl State {
    fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::SeqCst)
    }

    /// 同时更新 driver 内字段与共享原子值；两处不一致会让 `mode()` 返回过期模式。
    fn set_mode(&mut self, mode: WatchMode) {
        self.mode = mode;
        self.shared.mode.store(mode_to_u8(mode), Ordering::SeqCst);
    }

    /// 投递一次变化；关闭后与回调 panic 都被吞掉。
    ///
    /// 通常只在 driver 任务里调用；例外是 `open` 的首轮同步若发生降级，`Overflow` 会在
    /// `open` 返回之前、调用者任务上投递。
    ///
    /// `catch_unwind` + `AssertUnwindSafe` 用来对齐 TS `#deliver` 的 try/catch：回调即使
    /// panic，也不会终止 driver；首轮同步期间（`open` 返回前）发生 panic 时也不会从
    /// `open` 逃逸给调用者。
    fn deliver(&self, change: WatchChange) {
        if self.is_closed() {
            return;
        }
        let callback = self.on_change.clone();
        // 回调抛错不能停止观察（TS 的 `#deliver` 吞掉异常）。
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(change)));
    }

    /// 标记关闭并释放 watcher/安装记录；不投递任何变化。
    fn stop(&mut self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        self.watcher = None;
        self.installed.clear();
    }

    fn scan(&self) -> Result<Scan, ScanError> {
        Scanner::new(self).run()
    }

    /// 重扫、报告差异、为新目录安装 watcher，直到没有新目录（最多 10 轮）。
    ///
    /// # 参数
    /// `report` 只影响第一轮：为 `false` 时首轮只建立覆盖，不把 diff 收进 `changed`
    /// （`open` 用这种方式初始化）；只要循环继续，后续轮次都会报告。
    ///
    /// # 每轮状态机
    /// 1. `scan()`；若期间已关闭则直接退出（不安装 watcher、不采纳快照）。
    /// 2. `report` 为真时对 `diff(旧快照, 新快照)` 的每个路径做 `reported` 归并。
    /// 3. polling 模式跳过 watcher 调和；native 模式调用 `reconcile_watchers`。
    /// 4. 采纳新快照，然后看调和结果：
    ///    - `Added(false)`：没有新 watcher，结束；
    ///    - `Added(true)`：macOS 记账一次 settle；继续下一轮（`report` 置真），让新目录
    ///      watcher 安装前写进去的内容在下一轮 diff 中出现；
    ///    - `SwitchToPolling`：投递 `Overflow` 后结束。
    ///
    /// 扫描错误原样向上返回，旧快照保持不变（调用方决定是 `open` 失败还是 `WatchChange::Error`）。
    fn sync(&mut self, report: bool) -> Result<SyncOutcome, ScanError> {
        let mut changed: HashSet<String> = HashSet::new();
        let mut switched = false;
        let mut report = report;
        for _ in 0..10 {
            if self.is_closed() {
                break;
            }
            let scan = self.scan()?;
            // 扫描期间被关闭：这里再检查一次，避免为即将丢弃的状态安装 watcher。
            if self.is_closed() {
                break;
            }
            if report {
                for path in diff(&self.snapshot, &scan.snapshot) {
                    let reported = self.reported(&path);
                    changed.insert(reported);
                }
            }
            let outcome = if self.mode == WatchMode::Polling {
                ReconcileOutcome::Added(false)
            } else {
                self.reconcile_watchers(&scan.snapshot, &scan.linked_files)
            };
            self.snapshot = scan.snapshot;
            self.linked_files = scan.linked_files;
            match outcome {
                ReconcileOutcome::Added(false) => break,
                ReconcileOutcome::Added(true) => {
                    if cfg!(target_os = "macos") {
                        self.settle_requested = true;
                    }
                }
                ReconcileOutcome::SwitchToPolling => {
                    self.switch_to_polling();
                    switched = true;
                    break;
                }
            }
            // 在新目录 watcher 存在之前写入的内容会在下一轮出现。
            report = true;
        }
        Ok(SyncOutcome { changed, switched })
    }

    /// 把一条变化路径归并成上报路径。
    ///
    /// 规则：
    /// - 路径在某 target 之内（target 是它的祖先）→ 原样上报；
    /// - 否则路径是某 target 的祖先（target 随祖先被移动/重建）→ 上报该 target；
    /// - 都不是 → 原样上报。
    ///
    /// 被上报的祖先路径会覆盖它下面的所有 target，调用方（`covers` 类消费方）按祖先展开。
    fn reported(&self, path: &str) -> String {
        if self
            .targets
            .iter()
            .any(|target| is_within(path, &target.path))
        {
            return path.to_owned();
        }
        self.targets
            .iter()
            .find(|target| is_within(&target.path, path))
            .map(|target| target.path.clone())
            .unwrap_or_else(|| path.to_owned())
    }

    /// 让已安装的 watcher 集合与本次快照一致，返回是否需要重扫或降级。
    ///
    /// 需要 watcher 的路径（值表示是否用递归 watcher）：
    /// - `linked_files` 里的链接（非递归）；
    /// - 每个 target 已存在的目录祖先（非递归）；
    /// - 目录 target 本身：非递归 target 用非递归 watcher；递归 target 在 Linux/Android
    ///   之外用一个递归 watcher，在 Linux/Android 上因为 inotify 只监视单个目录（libuv 的
    ///   `fs.watch` 也同样不递归），改为给树内每个目录各装一个非递归 watcher；
    /// - 同一路径既是某 target 的祖先又是另一个 target 的根时，根那次写入的 `recursive`
    ///   覆盖祖先阶段的 `false`。
    ///
    /// 移除条件：不再需要，或快照中该路径的 `dev`/`ino` 与安装时不同（路径被另一个条目
    /// 替换，旧 watcher 仍跟着旧 inode）。`unwatch` 的错误被忽略。
    ///
    /// 安装失败：`PathNotFound` 与 `NotFound`/`PermissionDenied` 忽略（路径在安装前消失，
    /// 下次 sync 再试）；其他错误（watch 配额、后端不支持）返回 `SwitchToPolling`。
    /// `watcher` 为 `None`（如 `inotify_init` 失败）同样返回 `SwitchToPolling`。
    fn reconcile_watchers(
        &mut self,
        snapshot: &HashMap<String, Entry>,
        linked_files: &HashSet<String>,
    ) -> ReconcileOutcome {
        let mut wanted: HashMap<String, bool> = HashMap::new();
        for path in linked_files {
            wanted.insert(path.clone(), false);
        }
        let per_directory = cfg!(any(target_os = "linux", target_os = "android"));
        for target in &self.targets {
            // 祖先目录的事件覆盖它下面的 target；只给已存在的目录装 watcher。
            for ancestor in ancestors_of(&target.path) {
                if snapshot.get(&ancestor).map(|entry| entry.kind) == Some(EntryKind::Directory)
                    && !wanted.contains_key(&ancestor)
                {
                    wanted.insert(ancestor, false);
                }
            }
            if snapshot.get(&target.path).map(|entry| entry.kind) != Some(EntryKind::Directory) {
                continue;
            }
            let recursive_root = target.recursive && !per_directory;
            wanted.insert(target.path.clone(), recursive_root);
            if target.recursive && per_directory {
                // inotify 只监视单个目录；libuv 的 fs.watch 在 Linux 也不支持 recursive，
                // 因此与 TS 一样逐个目录安装非递归 watcher。
                for (path, entry) in snapshot {
                    if entry.kind == EntryKind::Directory
                        && path != &target.path
                        && is_within(path, &target.path)
                    {
                        wanted.insert(path.clone(), false);
                    }
                }
            }
        }
        let installed_paths: Vec<String> = self.installed.keys().cloned().collect();
        for path in installed_paths {
            let entry = snapshot.get(&path);
            let keep = wanted.contains_key(&path)
                && entry.is_some_and(|entry| {
                    let installed = self.installed[&path];
                    entry.dev == installed.dev && entry.ino == installed.ino
                });
            if !keep {
                if let Some(watcher) = self.watcher.as_mut() {
                    let _ = watcher.unwatch(StdPath::new(&path));
                }
                self.installed.remove(&path);
            }
        }
        let mut added = false;
        for (path, recursive) in wanted {
            let Some(entry) = snapshot.get(&path) else {
                continue;
            };
            if self.installed.contains_key(&path) {
                continue;
            }
            let Some(watcher) = self.watcher.as_mut() else {
                // 没有全局 watcher（例如 `inotify_init` 失败）：与 TS 的逐目录
                // `fs.watch` 失败一样退回轮询。
                return ReconcileOutcome::SwitchToPolling;
            };
            let recursive_mode = if recursive {
                RecursiveMode::Recursive
            } else {
                RecursiveMode::NonRecursive
            };
            match watcher.watch(StdPath::new(&path), recursive_mode) {
                Ok(()) => {
                    self.installed.insert(
                        path,
                        Installed {
                            dev: entry.dev,
                            ino: entry.ino,
                        },
                    );
                    added = true;
                }
                Err(error) => {
                    // 超出 watch 配额或不支持：改为快照对比，并声明覆盖范围不确定。
                    if !is_ignorable_watch_error(&error) {
                        return ReconcileOutcome::SwitchToPolling;
                    }
                }
            }
        }
        ReconcileOutcome::Added(added)
    }

    /// 一次性降级为轮询：丢掉原生 watcher、清空安装记录，并投递 `Overflow` 表示此前的
    /// 事件覆盖可能不完整。不会自动切回 `Native`。
    ///
    /// 定时器的重置不在这里做：driver 每轮末尾看到 `mode == Polling` 会清掉去抖/settle，
    /// 并补上轮询定时器。
    fn switch_to_polling(&mut self) {
        if self.mode == WatchMode::Polling {
            return;
        }
        self.set_mode(WatchMode::Polling);
        self.watcher = None;
        self.installed.clear();
        self.deliver(WatchChange::Overflow);
    }

    /// 处理一条 notify 事件；返回是否需要安排一次去抖重扫。
    ///
    /// - 成功事件：只接受 Create/Modify/Remove 与 Rescan 请求（见 `is_watched_event`）；
    ///   `paths` 为空表示"整个目录变化"（libuv 的 filename 为 null），仍需要重扫；其余每条
    ///   路径先按 `linked_files` 精确匹配，再按 `in_scope` 过滤，最后用 `reported` 归并进
    ///   `events`。任一相关路径都会返回 `true`。
    /// - 错误事件：按 `error.paths` 移除对应安装项（目录可能已消失），总是返回 `true`，
    ///   让重扫重建 watcher 或暴露真正的错误。
    fn handle_event(&mut self, result: notify::Result<Event>) -> bool {
        if self.is_closed() {
            return false;
        }
        match result {
            Ok(event) => {
                // libuv 的 inotify mask 只订阅 ATTRIB/CREATE/MODIFY/DELETE/DELETE_SELF/
                // MOVE_SELF/MOVED_FROM/MOVED_TO，没有 IN_OPEN/IN_CLOSE_WRITE；notify 会为
                // 它们产生 `Access` 事件，而扫描本身（readdir）就会打开被监视目录、触发
                // `Access(Open)`。只接受 Create/Modify/Remove 与“重扫”请求，否则空闲时永远
                // 有"变化"。
                if !is_watched_event(&event) {
                    return false;
                }
                if event.paths.is_empty() {
                    return true;
                }
                let mut relevant_any = false;
                for path in &event.paths {
                    let path = path.to_string_lossy().into_owned();
                    // 被链接的文件变化不是链接所在目录的事件：报告 target。
                    if self.linked_files.contains(&path) {
                        self.events.insert(path);
                        relevant_any = true;
                        continue;
                    }
                    if self.in_scope(&path) {
                        let reported = self.reported(&path);
                        self.events.insert(reported);
                        relevant_any = true;
                    }
                }
                relevant_any
            }
            Err(error) => {
                // 目录消失或失败：重扫会重建 watcher。
                for path in &error.paths {
                    let path = path.to_string_lossy().into_owned();
                    if self.installed.remove(&path).is_some()
                        && let Some(watcher) = self.watcher.as_mut()
                    {
                        let _ = watcher.unwatch(StdPath::new(&path));
                    }
                }
                true
            }
        }
    }

    /// 事件路径是否落在某个 target 的观察范围内。
    ///
    /// 两种接受情况：
    /// - 路径是 target 本身或其祖先：它的变化可能移动/重建整个 target；
    /// - 路径在 target 之下：非递归 target 只接受直接子项，并且相对路径的任一组件命中
    ///   排除规则（隐藏名或名单）就不接受。
    ///
    /// 祖先目录的事件会带来无关兄弟路径，必须先过滤，否则排除项也会被上报
    /// （一致性用例 `watch skips excluded entries and reports a rename out of them` 锁定）。
    fn in_scope(&self, path: &str) -> bool {
        for target in &self.targets {
            if is_within(&target.path, path) {
                return true;
            }
            if !is_within(path, &target.path) || path == target.path {
                continue;
            }
            let relative_target = platform_relative(&target.path, path);
            let components: Vec<&str> = relative_target.split(platform_sep()).collect();
            if !target.recursive && components.len() > 1 {
                continue;
            }
            if components.iter().any(|name| excluded(target, name)) {
                continue;
            }
            return true;
        }
        false
    }

    /// 重扫并投递差异；错误时投递 `{ error }` 并停止。
    ///
    /// 顺序：先取走 `events`（notify 事件已经指出的上报路径），再 `sync(true)`；两者合并后
    /// 非空才按 UTF-16 码元排序并投递一次 `WatchChange::Paths`。扫描失败时事件被丢弃，
    /// 投递 `WatchChange::Error` 后 `stop()`——此后不会再有回调（与 TS 的 `#flush` 一致）。
    ///
    /// 返回值描述本次 flush 造成的状态变化；`run_driver` 目前不读它，而是直接检查
    /// `state.is_closed()` 与 `state.mode`。
    fn flush(&mut self) -> FlushOutcome {
        let events: Vec<String> = self.events.drain().collect();
        let outcome = match self.sync(true) {
            Ok(outcome) => outcome,
            Err(error) => {
                let file_error = scan_error_for_flush(error);
                self.deliver(WatchChange::Error(file_error));
                self.stop();
                return FlushOutcome::Stopped;
            }
        };
        let mut changed = outcome.changed;
        for path in events {
            changed.insert(path);
        }
        if !changed.is_empty() {
            let mut paths: Vec<String> = changed.into_iter().collect();
            sort_paths_utf16(&mut paths);
            self.deliver(WatchChange::Paths(paths));
        }
        if outcome.switched {
            FlushOutcome::SwitchedToPolling
        } else {
            FlushOutcome::Continue
        }
    }

    /// 读取并清除 settle 请求（macOS 新增 watcher 后由 driver 安排一次延迟重扫）。
    fn take_settle_request(&mut self) -> bool {
        std::mem::take(&mut self.settle_requested)
    }
}

/// 一次 `sync` 的结果：变化路径（已经过 `reported` 归并）与是否降级到轮询。
struct SyncOutcome {
    changed: HashSet<String>,
    switched: bool,
}

/// `flush` 的结果。
///
/// `run_driver` 目前忽略它，改为检查 `State` 的 `closed`/`mode` 字段；当前调用方
/// （`run_driver` 与仓库内测试）都不使用该返回值。
enum FlushOutcome {
    /// 继续当前模式。
    Continue,
    /// 本次 flush 中降级到了轮询。
    SwitchedToPolling,
    /// 扫描失败，观察已停止。
    Stopped,
}

/// 新旧快照的差异：新增、字段变化、删除都算。
///
/// 返回的路径不带 `reported` 归并，顺序来自 `HashMap` 遍历、不稳定；调用方负责归并并在
/// 投递前排序。
fn diff(previous: &HashMap<String, Entry>, next: &HashMap<String, Entry>) -> Vec<String> {
    let mut changed = Vec::new();
    for (path, entry) in next {
        match previous.get(path) {
            None => changed.push(path.clone()),
            Some(before) if !same_entry(before, entry) => changed.push(path.clone()),
            Some(_) => {}
        }
    }
    for path in previous.keys() {
        if !next.contains_key(path) {
            changed.push(path.clone());
        }
    }
    changed
}

/// 安装 watcher 时可以忽略的错误：路径在安装前消失，或没有权限。
///
/// 其他错误（watch 配额耗尽、后端不支持等）让调用方降级为轮询。
fn is_ignorable_watch_error(error: &notify::Error) -> bool {
    match &error.kind {
        notify::ErrorKind::PathNotFound => true,
        notify::ErrorKind::Io(io) => matches!(
            io.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
        ),
        _ => false,
    }
}

/// `fs.watch`（libuv）订阅的事件种类：Create/Modify/Remove。notify 额外订阅 IN_OPEN 与
/// IN_CLOSE_WRITE 并映射成 `Access`；这些必须丢弃，否则每次重扫的 `readdir` 都会打开被监视
/// 目录、产生 `Access(Open)`，让 watcher 永远不静止。
///
/// `EventKind::Other` + `Flag::Rescan` 是后端丢弃事件后的重扫请求（FSEvents 的
/// MUST_SCAN_SUBDIRS/USER_DROPPED/KERNEL_DROPPED，inotify 的 IN_Q_OVERFLOW）；TS 的
/// `fs.watch` 回调也会收到这种通知，`#onEvent` 照常安排去抖重扫。
fn is_watched_event(event: &notify::Event) -> bool {
    match event.kind {
        notify::EventKind::Create(_)
        | notify::EventKind::Modify(_)
        | notify::EventKind::Remove(_) => true,
        notify::EventKind::Other => event.need_rescan(),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// driver
// ---------------------------------------------------------------------------

/// driver 主循环：等待 notify 事件或定时器到期，串行执行扫描与回调。
///
/// 最多同时存在三个定时器：
/// - `flush_at`：去抖后的重扫（首个相关事件后 50 ms）；
/// - `poll_at`：polling 模式的下一次快照；
/// - `settle_at`：macOS 安装 watcher 后 500 ms 的补偿重扫。
///
/// 每轮取最早的一个作为 `sleep_until` 的截止时间，并同时等待关闭令牌与 notify 消息。醒来后
/// 依次处理到期定时器（settle → flush → poll）：扫描与回调都在本任务内同步执行，因此同一
/// 时刻只有一次 flush。最后消费 `settle_requested`，并按当前模式重置定时器：
/// polling 下清掉去抖/settle 并确保 `poll_at` 已武装。
///
/// 退出路径：关闭令牌、通道关闭（所有 sender 被 drop；正常由 `keepalive` 防止）、或循环
/// 顶部/底部的 `is_closed()`。无论哪条路径，退出前都会 `state.stop()` 释放 watcher；
/// `close` 通过等待 `JoinHandle` 来保证这一点。
async fn run_driver(
    mut state: State,
    mut receiver: mpsc::UnboundedReceiver<DriverMessage>,
    // 保持通道打开：见 `NodeFileWatcher::open`。
    _keepalive: mpsc::UnboundedSender<DriverMessage>,
) {
    let mut flush_at: Option<Instant> = None;
    let mut settle_at: Option<Instant> = None;
    let mut poll_at: Option<Instant> = if state.mode == WatchMode::Polling {
        Some(Instant::now() + state.poll_interval)
    } else {
        None
    };
    loop {
        if state.is_closed() {
            break;
        }
        // 没有定时器时用永不完成的 future 占位，只等事件或关闭。
        let deadline = [flush_at, poll_at, settle_at].into_iter().flatten().min();
        let sleep = async {
            match deadline {
                Some(at) => {
                    tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = state.shared.shutdown.cancelled() => break,
            message = receiver.recv() => {
                let Some(message) = message else { break };
                match message {
                    DriverMessage::Fs(result) => {
                        // 消息分支只处理事件并设置去抖时间；扫描留到 select 之后统一执行。
                        let wanted = state.handle_event(result);
                        if wanted && flush_at.is_none() && !state.is_closed() {
                            flush_at = Some(Instant::now() + Duration::from_millis(DEBOUNCE_MS));
                        }
                    }
                }
            }
            _ = sleep => {}
        }
        let now = Instant::now();
        // 多个定时器可能同时到期；flush 本身幂等（没有新变化就不回调），重复执行无害。
        if settle_at.is_some_and(|at| at <= now) {
            settle_at = None;
            state.flush();
        }
        if flush_at.is_some_and(|at| at <= now) {
            flush_at = None;
            state.flush();
        }
        if poll_at.is_some_and(|at| at <= now) {
            poll_at = None;
            state.flush();
            // 若本轮回调没有降级/关闭，继续按固定间隔轮询。
            if state.mode == WatchMode::Polling && !state.is_closed() {
                poll_at = Some(Instant::now() + state.poll_interval);
            }
        }
        if state.take_settle_request() {
            settle_at = Some(Instant::now() + Duration::from_millis(FSEVENTS_SETTLE_MS));
        }
        // 切换到 polling（watcher 创建失败）后清掉去抖/settle 定时器并开始轮询。
        if state.mode == WatchMode::Polling {
            flush_at = None;
            settle_at = None;
            if poll_at.is_none() && !state.is_closed() {
                poll_at = Some(Instant::now() + state.poll_interval);
            }
        }
        if state.is_closed() {
            break;
        }
    }
    // 释放 watcher 与安装记录；`close` 正在等待这个任务结束。
    state.stop();
}

// ---------------------------------------------------------------------------
// anyUnreliable
// ---------------------------------------------------------------------------

/// 是否有任一被监视根（或最近的已存在祖先）位于不报告远端变化的文件系统上。
///
/// 每个路径按"路径本身 → 由近到远的祖先"尝试 `statfs`：第一个成功的候选决定文件系统
/// 类型（失败就继续向上），命中 `UNRELIABLE_FILE_SYSTEMS` 立即返回 `true`。非
/// Linux/Android 直接返回 `false`；`open` 只在没有强制模式时调用它。
fn any_unreliable(paths: &[String]) -> bool {
    if !cfg!(any(target_os = "linux", target_os = "android")) {
        return false;
    }
    for path in paths {
        let mut candidates = vec![path.clone()];
        candidates.extend(ancestors_of(path));
        for candidate in candidates {
            match statfs_type(&candidate) {
                Some(file_system) => {
                    if UNRELIABLE_FILE_SYSTEMS.contains(&file_system) {
                        return true;
                    }
                    break;
                }
                None => continue,
            }
        }
    }
    false
}

/// Linux/Android 的 `statfs().f_type`；路径含 NUL 或调用失败时返回 `None`。
#[cfg(any(target_os = "linux", target_os = "android"))]
fn statfs_type(path: &str) -> Option<u64> {
    let path = std::ffi::CString::new(path).ok()?;
    let mut buffer: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` 是有效的 NUL 结尾字符串，`buffer` 指向有效的 `statfs`。
    let result = unsafe { libc::statfs(path.as_ptr(), &mut buffer) };
    if result == 0 {
        Some(buffer.f_type as u64)
    } else {
        None
    }
}

/// 其他平台没有实现 `statfs` 探测，`any_unreliable` 也因此在非 Linux/Android 上直接返回
/// `false`，不会走这里。
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn statfs_type(_path: &str) -> Option<u64> {
    None
}

// ---------------------------------------------------------------------------
// NodeFileWatcher
// ---------------------------------------------------------------------------

/// 观察文件与目录的变化（TS: `node-watch.ts#NodeFileWatcher`）。
///
/// 句柄只保存共享状态与 driver 的 `JoinHandle`：扫描、去抖与回调都在 driver 任务里，
/// `mode()` 无锁读取原子值。`close` 之后仍可安全再次调用，driver 退出后不会再收到回调。
pub struct NodeFileWatcher {
    /// 与 driver 共享的模式、关闭标志与取消令牌。
    shared: Arc<Shared>,
    /// driver 任务句柄；`close` 取走后为 `None`。
    driver: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl std::fmt::Debug for NodeFileWatcher {
    // 只输出当前模式；driver 句柄不进入 Debug 表示。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NodeFileWatcher")
            .field("mode", &self.mode())
            .finish_non_exhaustive()
    }
}

impl NodeFileWatcher {
    /// 建立覆盖：先安装 watcher，再取之后的变化所比较的快照。
    ///
    /// `resolve_path` 是 TS 第二个参数（`(path) => string`），不是 `Context`。
    ///
    /// # 参数
    /// - `targets`：每个路径先经 `resolve_path` 变成快照键，之后上报的路径也使用这个形式。
    /// - `on_change`：变化回调。`open` 返回后它只在 driver 任务里被调用；例外是首轮同步
    ///   发生降级时，`Overflow` 会在本函数返回前、调用者任务上投递。
    /// - `options`：只在这里读取一次。
    ///
    /// # 模式选择
    /// 显式 `options.mode` 优先；否则 Windows 或任一 target 位于
    /// `UNRELIABLE_FILE_SYSTEMS`（只在 Linux/Android 上探测）→ `Polling`，其余 `Native`。
    ///
    /// # 错误
    /// 只有首轮扫描会返回 `Err`：目录预算超限，或扫描遇到未被当作"目标消失"处理的 I/O
    /// 错误（典型是 target 根被拒绝访问；映射见 `scan_error_for_open`，`FileError.path`
    /// 是出错路径——通常是 target 根，也可能是递归扫描到的子目录）。返回 `Err` 前不会
    /// 启动 driver。原生 watcher 创建失败不在这里报错：安装阶段发现没有 watcher 时降级为
    /// 轮询，并投递一次 `Overflow`。
    ///
    /// # 运行环境
    /// 成功路径会 `tokio::spawn`，因此必须在 tokio 运行时内调用；首轮扫描是同步
    /// `std::fs` 调用，发生在调用者任务上，扫描完成前不会返回。
    pub async fn open(
        targets: &[WatchTarget],
        resolve_path: Arc<dyn Fn(&str) -> String + Send + Sync>,
        on_change: WatchCallback,
        options: &NodeWatchOptions,
    ) -> Result<NodeFileWatcher, FileError> {
        // 先把所有 target 解析成快照键；`recursive` 只有显式 `Some(true)` 才为真。
        let resolved: Vec<ResolvedTarget> = targets
            .iter()
            .map(|target| ResolvedTarget {
                path: resolve_path(&target.path),
                recursive: target.recursive == Some(true),
                hidden: target.exclude.as_ref().and_then(|exclude| exclude.hidden) == Some(true),
                names: target
                    .exclude
                    .as_ref()
                    .and_then(|exclude| exclude.names.clone())
                    .unwrap_or_default()
                    .into_iter()
                    .collect(),
            })
            .collect();
        // Windows 拒绝在目录仍被打开时改名其父目录，而原生 watcher 保持每个被监视目录打开：
        // 观察会破坏父目录的改名，所以 Windows 轮询。
        let mode = match options.mode {
            Some(mode) => mode,
            None => {
                let paths: Vec<String> =
                    resolved.iter().map(|target| target.path.clone()).collect();
                // 不可靠文件系统接受 watch，但不报告别处做的修改；native 会静默漏事件。
                if cfg!(windows) || any_unreliable(&paths) {
                    WatchMode::Polling
                } else {
                    WatchMode::Native
                }
            }
        };
        let (sender, receiver) = mpsc::unbounded_channel();
        // driver 自己持有 sender 的一个克隆：`#switchToPolling` 会 drop 原生 watcher（以及
        // notify 线程里的 handler），没有这个克隆 `recv()` 会返回 None，driver 随之退出，
        // 轮询再也不会发生。
        let keepalive = sender.clone();
        let watcher = if mode == WatchMode::Native {
            let handler = move |result: notify::Result<Event>| {
                let _ = sender.send(DriverMessage::Fs(result));
            };
            // `inotify_init1` 失败（EMFILE/ENOSPC 等）：首轮 sync 退回轮询。
            RecommendedWatcher::new(handler, Config::default()).ok()
        } else {
            // polling 模式不安装原生 watcher（TS 也不调用 fs.watch）。
            drop(sender);
            None
        };
        let poll_interval_ms = options.poll_interval_ms.unwrap_or(DEFAULT_POLL_MS);
        // `setInterval` 把非有限、非正或超过 TIMEOUT_MAX 的延迟按 1 ms 处理；
        // `Duration::from_secs_f64` 对 NaN 会 panic，因此这里先按计时器语义收紧。
        let poll_interval = if poll_interval_ms.is_finite()
            && poll_interval_ms > 0.0
            && poll_interval_ms <= 2_147_483_647.0
        {
            Duration::from_secs_f64(poll_interval_ms / 1000.0)
        } else {
            Duration::from_millis(1)
        };
        let shared = Arc::new(Shared {
            mode: AtomicU8::new(mode_to_u8(mode)),
            closed: AtomicBool::new(false),
            shutdown: CancellationToken::new(),
        });
        let mut state = State {
            shared: Arc::clone(&shared),
            targets: resolved,
            on_change,
            poll_interval,
            max_directories: options.max_directories.unwrap_or(DEFAULT_MAX_DIRECTORIES),
            mode,
            watcher,
            installed: HashMap::new(),
            snapshot: HashMap::new(),
            linked_files: HashSet::new(),
            events: HashSet::new(),
            settle_requested: false,
        };
        // 首轮同步建立覆盖并取初始快照；这里同步等待扫描完成，错误直接作为 `open` 的失败。
        if let Err(error) = state.sync(false) {
            state.stop();
            return Err(scan_error_for_open(error));
        }
        // driver 独占 state；`close` 通过 JoinHandle 等它退出。
        let driver = tokio::spawn(run_driver(state, receiver, keepalive));
        Ok(NodeFileWatcher {
            shared,
            driver: Mutex::new(Some(driver)),
        })
    }
}

#[async_trait]
impl FileWatcher for NodeFileWatcher {
    /// 当前模式；原生 watcher 创建或安装失败并降级后，这里会从 `Native` 变成 `Polling`。
    fn mode(&self) -> WatchMode {
        mode_from_u8(self.shared.mode.load(Ordering::SeqCst))
    }

    /// 幂等关闭：置 `closed` → 取消令牌 → 取走并等待 driver 任务。
    ///
    /// 关闭后 `deliver` 直接丢弃变化；driver 退出前会释放 watcher。正在进行的同步扫描不会
    /// 被取消，因此 `close` 会等它结束。若另一个并发 `close` 已经取走句柄，本次调用只设置
    /// 标志并立即返回，不等那个任务。
    async fn close(&self, _context: &dyn Context) {
        self.shared.closed.store(true, Ordering::SeqCst);
        self.shared.shutdown.cancel();
        let driver = lock(&self.driver).take();
        if let Some(driver) = driver {
            let _ = driver.await;
        }
    }
}

// ---------------------------------------------------------------------------
// 单元测试（内部状态：TS 用假定时器/桩替换 fs.watch，Rust 直接驱动 driver 与 State）
//
// 涉及文件系统的用例使用真实临时目录，锁定：事件种类过滤、上报路径的 UTF-16 排序、无原生
// watcher 时降级轮询、driver 在 watcher 被 drop 后仍继续轮询，以及 open/flush 两种错误映射在
// `FileError.path` 上的差异。
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    fn temp_root(tag: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "kiana-durable-watch-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp root");
        path.to_string_lossy().into_owned()
    }

    fn target(path: &str, recursive: bool) -> ResolvedTarget {
        ResolvedTarget {
            path: path.to_owned(),
            recursive,
            hidden: false,
            names: HashSet::new(),
        }
    }

    fn collector() -> (WatchCallback, Arc<StdMutex<Vec<WatchChange>>>) {
        let changes: Arc<StdMutex<Vec<WatchChange>>> = Arc::new(StdMutex::new(Vec::new()));
        let sink = Arc::clone(&changes);
        let callback: WatchCallback = Arc::new(move |change| {
            sink.lock().expect("changes lock").push(change);
        });
        (callback, changes)
    }

    fn state_for(
        mode: WatchMode,
        roots: &[(&str, bool)],
        on_change: WatchCallback,
        watcher: Option<RecommendedWatcher>,
    ) -> State {
        State {
            shared: Arc::new(Shared {
                mode: AtomicU8::new(mode_to_u8(mode)),
                closed: AtomicBool::new(false),
                shutdown: CancellationToken::new(),
            }),
            targets: roots
                .iter()
                .map(|(path, recursive)| target(path, *recursive))
                .collect(),
            on_change,
            poll_interval: Duration::from_millis(20),
            max_directories: 1000,
            mode,
            watcher,
            installed: HashMap::new(),
            snapshot: HashMap::new(),
            linked_files: HashSet::new(),
            events: HashSet::new(),
            settle_requested: false,
        }
    }

    #[test]
    fn only_create_modify_and_remove_events_match_fs_watch() {
        // notify 的 inotify 后端额外订阅 IN_OPEN/IN_CLOSE_WRITE 并映射成 Access；libuv 不订阅。
        let watched = |kind| is_watched_event(&notify::Event::new(kind));
        assert!(!watched(notify::EventKind::Access(
            notify::event::AccessKind::Open(notify::event::AccessMode::Any)
        )));
        assert!(!watched(notify::EventKind::Access(
            notify::event::AccessKind::Close(notify::event::AccessMode::Write)
        )));
        assert!(watched(notify::EventKind::Create(
            notify::event::CreateKind::File
        )));
        assert!(watched(notify::EventKind::Modify(
            notify::event::ModifyKind::Data(notify::event::DataChange::Any)
        )));
        assert!(watched(notify::EventKind::Remove(
            notify::event::RemoveKind::File
        )));
        assert!(!watched(notify::EventKind::Any));
        // 后端丢弃事件后的重扫请求（FSEvents/inotify overflow）必须触发重扫。
        assert!(is_watched_event(
            &notify::Event::new(notify::EventKind::Other).set_flag(notify::event::Flag::Rescan)
        ));
        assert!(!watched(notify::EventKind::Other));
    }

    #[test]
    fn sorts_reported_paths_in_utf16_code_unit_order() {
        let mut paths = vec!["\u{e000}".to_owned(), "\u{1f600}".to_owned()];
        sort_paths_utf16(&mut paths);
        // UTF-8 字节顺序会把 U+E000（EE 80 80）排在 U+1F600（F0 9F 98 80）之前；
        // JS 默认比较看 UTF-16 码元，代理对 D83D 在 E000 之前。
        assert_eq!(paths, vec!["\u{1f600}".to_owned(), "\u{e000}".to_owned()]);
    }

    #[test]
    fn a_missing_native_watcher_switches_to_polling() {
        let root = temp_root("no-watcher");
        std::fs::create_dir_all(format!("{root}/tree")).expect("create tree");
        let (callback, changes) = collector();
        let mut state = state_for(
            WatchMode::Native,
            &[(&format!("{root}/tree"), true)],
            callback,
            None,
        );
        state.sync(false).expect("initial sync");
        // `switchToPolling` 投递一次 overflow，并保持轮询模式。
        assert_eq!(state.mode, WatchMode::Polling);
        assert_eq!(
            changes.lock().expect("changes lock").as_slice(),
            &[WatchChange::Overflow]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `#switchToPolling` 会 drop 原生 watcher（notify 线程随之释放 handler 里的 sender）。
    /// driver 必须仍然轮询，而不是因为 `recv()` 返回 None 就退出。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_driver_keeps_polling_after_the_native_watcher_is_dropped() {
        let root = temp_root("driver-polling");
        std::fs::create_dir_all(format!("{root}/tree")).expect("create tree");
        let (callback, changes) = collector();
        let mut state = state_for(
            WatchMode::Polling,
            &[(&format!("{root}/tree"), true)],
            callback,
            None,
        );
        state.sync(false).expect("initial sync");
        let shutdown = Arc::clone(&state.shared);

        let (sender, receiver) = mpsc::unbounded_channel();
        let keepalive = sender.clone();
        // 模拟 watcher 被 drop：除 driver 的克隆外没有 sender。
        drop(sender);
        let driver = tokio::spawn(run_driver(state, receiver, keepalive));

        std::fs::write(format!("{root}/tree/new.txt"), "x").expect("write file");
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if changes
                .lock()
                .expect("changes lock")
                .iter()
                .any(|change| matches!(change, WatchChange::Paths(paths) if paths.iter().any(|path| path.ends_with("new.txt"))))
            {
                break;
            }
            assert!(Instant::now() < deadline, "polling driver stopped");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        shutdown.shutdown.cancel();
        driver.await.expect("driver task");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_errors_keep_the_failing_target_path() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let root = temp_root("scan-error");
            let locked = format!("{root}/locked");
            std::fs::create_dir_all(&locked).expect("create locked");
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000))
                .expect("chmod");
            let target_path = format!("{locked}/missing");
            let error = Scanner::new(&state_for(
                WatchMode::Polling,
                &[(&target_path, false)],
                Arc::new(|_| {}),
                None,
            ))
            .run()
            .err()
            .expect("a denied target must fail the scan");
            let file_error = scan_error_for_open(error);
            assert_eq!(file_error.code, FileErrorCode::PermissionDenied);
            assert_eq!(file_error.path.as_deref(), Some(target_path.as_str()));
            assert_eq!(
                file_error.message,
                format!("EACCES: permission denied, stat '{target_path}'")
            );
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))
                .expect("chmod back");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn flush_errors_use_node_messages_without_a_path_field() {
        let error = ScanError::io(
            std::io::Error::from_raw_os_error(libc::EACCES),
            "scandir",
            "/watched",
        );
        let file_error = scan_error_for_flush(error);
        assert_eq!(file_error.code, FileErrorCode::PermissionDenied);
        assert_eq!(
            file_error.message,
            "EACCES: permission denied, scandir '/watched'"
        );
        assert_eq!(file_error.path, None);
    }
}
