//! `watch.rs`：Durable `NodeFileWatcher`（packages/durable/src/env/node-watch.ts）的移植，跑在文件旁边，
//! 一次扫描不需要往返。原生事件只触发防抖后的重扫；变化是快照差异加上事件路径，作为该请求的 `change`
//! 事件发出，直到请求被取消。
//!
//! TS 源：packages/env/daemon/src/watch.rs（原 daemon 本身就是 Rust；逐字移植）。
//! `watch`: Durable's `NodeFileWatcher` (packages/durable/src/env/node-watch.ts) running next to the files, so a scan
//! costs no round trips. Native events only trigger a debounced rescan; changes are the difference between snapshots
//! plus the event paths. Events go out as `change` events of the request, which lasts until it is cancelled.
//!
//! # 在系统中的位置
//! 上游：`main.rs` 的请求分发在 `op == "watch"` 时调用 [`run`]；watch 与 `exec`、`pty` 一样独占一个线程，
//! 不占用 16 个工作线程的池子，因为扫描会递归整棵目录树。
//! 下游：`output.rs` 的 `Output::control`（事件帧优先于命令的 bulk 输出）、`sys` 平台层
//! （路径分隔符、文件身份、mtime）、`notify` crate（原生文件系统事件）。
//! 对应 TS：`packages/durable/src/env/node-watch.ts` 的 `NodeFileWatcher`；协议语义见
//! `spec/durable-rest.md` §2.24。
//!
//! # 设计原因：为什么事件只当"重扫提示"
//! 本模块的判定不依赖原生事件是否完整或逐文件：事件只用来尽快触发一次重扫，
//! 变化最终由两次扫描快照的差异给出，事件路径只作为补充并入结果：
//! 1. 快照比较的字段见 `Entry`，同一套逻辑在 native 与 polling 两种模式下都能工作；
//! 2. 扫描发生在文件旁边，只需要本地系统调用，没有网络往返；
//! 3. 事件路径并入变化集合，用来捕捉"扫描间隙里发生、且快照字段无法区分"的改动；
//! 4. `DEBOUNCE` 把短时间内的多个事件合并成一次重扫。
//!
//! # 执行流程（一次 watch 请求）
//! 1. `parse_targets` 解析 `targets`（路径、是否递归、排除规则）；
//! 2. 选择 `Mode`：显式 `mode` 优先；否则 Windows 或不可靠文件系统（见 `any_unreliable`）用 polling；
//! 3. 首次 `sync(false)` 建立快照（native 模式下同时安装 watcher；polling 模式下 `sync` 扫描一轮后
//!    直接结束，不调用 `reconcile`，因此不会安装任何 watcher），失败直接作为请求错误返回
//!    （若 native watcher 安装失败会退化为 polling，并先发一次 `overflow`）；
//! 4. 发出 `ready` 事件，携带实际模式（`native` / `polling`）；
//! 5. 主循环用 `recv_timeout` 等原生事件，同时按最近的截止时间醒来（取消检查、防抖、FSEvents settle、轮询）；
//!    但 polling 模式不再产生原生事件，防抖与 FSEvents settle 也不会由 polling 路径设置，
//!    正常只按取消检查与轮询到期醒来；
//! 6. `FileWatcher::flush` 重扫并 diff，发出 `change` 事件（路径按 JS 的 UTF-16 顺序排序）；
//! 7. 收到取消后返回 `{}`；重扫出错则发出 `error` 事件并结束。
//!
//! # 状态与不变量
//! - `snapshot` 是"上一次成功扫描"的视图，只在 `sync` 中整体替换；
//! - `installed` 记录每个实际安装的 watcher 及其 dev/ino；目录被删除后同名重建（inode 变化）必须重装；
//! - `events` 是本轮由原生事件推出的候选路径，`flush` 时与 diff 结果合并；
//! - 目录预算 `max_directories` 是全部 target 共享的全局计数，不是每个 target 各算一次。
//!
//! # 注意事项
//! - 事件走 `Output::control`，不会被命令的大量输出堵住；
//! - 取消不是即时中断：空闲时主循环最迟约一个 `CANCEL_POLL` 后观察到标志；若正在扫描/重扫，
//!   要等当前这一轮扫描结束（`sync` 每轮开始会检查一次）；
//! - `error` 事件是终端事件，之后不会再有任何事件；
//! - 客户端传入的路径写法贯穿快照的键，平台返回的规范路径需要经 `watched_spelling` 翻译回来。

use crate::errors::Failure;
use crate::exec::Control;
use crate::frame::{EVENT, Frame};
use crate::output::Output;
use crate::sys;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fs::{self, Metadata};
use std::hash::Hasher;
use std::io;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime};

/// 原生事件的合并窗口：短时间内到达的多个事件只触发一次重扫，避免逐事件扫描。
const DEBOUNCE: Duration = Duration::from_millis(50);
/// macOS: FSEvents streams go live after `watch` returns; a rescan this long after installing watchers catches changes
/// made in between.
const FSEVENTS_SETTLE: Duration = Duration::from_millis(500);
/// polling 模式的默认扫描间隔（毫秒），客户端可用 `pollIntervalMs` 覆盖。
const DEFAULT_POLL_MS: u64 = 2000;
/// 默认目录预算；超过后初次扫描报 `EINVAL`，后续重扫发出 `error` 事件，见 `FileWatcher::count`。
const DEFAULT_MAX_DIRECTORIES: usize = 10_000;
/// In polling mode, recently modified small files are also compared by content.
/// 仅在 polling 模式生效：某些文件系统的 mtime 精度不足以区分连续两次写入，
/// 对"最近修改过且足够小"的文件额外做内容哈希，见 `FileWatcher::record`。
const HASH_MAX_BYTES: u64 = 256 * 1024;
/// 只有 mtime 距现在不超过这个时长的文件才算"最近"；哈希只用于捕捉刚发生、mtime 精度可能不足的改动，
/// 超过这个年龄的文件不再读全文，避免每次轮询都重复读盘。
const HASH_RECENT: Duration = Duration::from_secs(5);
/// How often a waiting watcher looks for a cancel.
const CANCEL_POLL: Duration = Duration::from_millis(100);

/// Linux `statfs` magic numbers of file systems that accept watches but do not report changes made elsewhere.
#[cfg(any(target_os = "linux", target_os = "android"))]
const UNRELIABLE_FILE_SYSTEMS: [u64; 12] = [
    0x6969,     // NFS
    0x517b,     // SMB
    0xff534d42, // CIFS
    0xfe534d42, // SMB2
    0x65735546, // FUSE (sshfs, Android shared storage)
    0x01021997, // 9P (WSL2 Windows drives)
    0x0bd00bd0, // Lustre
    0x47504653, // GPFS
    0x00c36400, // Ceph
    0x5346414f, // OpenAFS
    0x6b414653, // kAFS
    0x5dca2df5, // sdcardfs
];

/// 一个被监视的根。`path` 允许暂时不存在（此时 watch 继续运行，等待它出现）；
/// `recursive` 决定是否下探子目录；`hidden` 与 `names` 是该根的排除规则，任一命中即排除（见 `excluded`）。
struct Target {
    path: String,
    recursive: bool,
    hidden: bool,
    names: HashSet<String>,
}

/// 路径类型：文件、目录、符号链接、其他（fifo、socket、设备等）。
/// 符号链接在列目录时用 `lstat` 判定，因此链接本身是 `Symlink` 而不是它指向的类型。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Directory,
    Symlink,
    Other,
}

/// What a snapshot remembers of one path. Directories and ancestors are compared by identity only.
///
/// 快照里每个路径"记得"的字段，`PartialEq` 的结果就是"是否发生变化"：
/// - 非目录项比较类型、设备号、inode、大小和 mtime（符号链接按 `lstat` 的结果比较）；
/// - `hash` 只在 polling 模式下、对最近修改过的小文件计算，用来弥补 mtime 精度不足；
/// - 目录与祖先的 `size`/`mtime` 被固定为 0，只比较 `dev`/`ino`：
///   目录的 mtime 会随无关子项的创建、删除或改名变化，比较它会产生大量噪声。
#[derive(Clone, PartialEq, Eq)]
struct Entry {
    kind: Kind,
    dev: u64,
    ino: u64,
    size: u64,
    mtime: (i64, i64),
    hash: Option<u64>,
}

/// 路径到 `Entry` 的映射，一次扫描的完整结果。键基于客户端传入的路径写法：
/// target 及其祖先沿用原字符串，子项在 target 路径后拼接文件名。
type Snapshot = HashMap<String, Entry>;

/// 变化检测方式。`Native` 依赖 `notify` 报告的文件系统事件；`Polling` 按固定间隔主动重扫。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Native,
    Polling,
}

impl Mode {
    /// `ready` 事件的 `mode` 字段使用这里的名字；`overflow` 事件则在 `switch_to_polling` 里直接写 `"polling"`。
    fn name(self) -> &'static str {
        match self {
            Mode::Native => "native",
            Mode::Polling => "polling",
        }
    }
}

/// 一次扫描可能失败的两类原因。两者的上报方式不同：
/// 打开阶段（首次 `sync(false)`）转成请求错误 `Failure`；后续重扫则变成 `error` 事件，
/// 事件里的 `code` 由 `ScanError::code` 决定。
enum ScanError {
    /// 目录数超过预算，携带预算值用于生成消息。
    Budget(usize),
    /// 系统调用失败：原始 `io::Error`、系统调用名、出错路径。
    Io(io::Error, &'static str, String),
}

impl ScanError {
    /// The error `NodeExecutionEnv.watch` returns when opening fails.
    ///
    /// 打开阶段的映射：预算超限 → `EINVAL`；IO 错误 → Node 风格错误（`Failure::io`，
    /// 带 `syscall` 与 `path` 字段）。这是"请求失败"，不是 watcher 事件。
    fn failure(&self) -> Failure {
        match self {
            ScanError::Budget(max) => {
                Failure::new("EINVAL", format!("Watched paths exceed {max} directories"))
            }
            ScanError::Io(error, syscall, path) => Failure::io(error, syscall, path),
        }
    }

    /// The `FileError` code `NodeFileWatcher` reports when a later rescan fails.
    ///
    /// 重扫阶段的映射：只有权限错误是 `permission_denied`，其余（含预算超限和普通 IO 错误）
    /// 都是 `invalid`。与 `failure` 不同，这里只给错误事件一个客户端可分支的代码。
    fn code(&self) -> &'static str {
        match self {
            ScanError::Io(error, ..) if denied(error) => "permission_denied",
            _ => "invalid",
        }
    }
}

/// 是否为权限拒绝。调用点对它有特殊处理：target 根目录不可读视为扫描失败，
/// 而它下面的子目录无权限只会被跳过。
fn denied(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::PermissionDenied
}

/// 由 metadata 判定路径类型。只按 `FileType` 判断，不会因符号链接而去看目标。
fn kind_of(metadata: &Metadata) -> Kind {
    let file_type = metadata.file_type();
    if file_type.is_file() {
        Kind::File
    } else if file_type.is_dir() {
        Kind::Directory
    } else if file_type.is_symlink() {
        Kind::Symlink
    } else {
        Kind::Other
    }
}

/// What a snapshot remembers of `path`, whose `metadata` came from `stat` (`follow`) or `lstat`.
///
/// `follow` 只影响 `dev`/`ino` 的取得方式：target 自身跟随符号链接，列目录得到的子项不跟随。
/// 目录的 `size`/`mtime` 一律归零，原因见 `Entry`。
fn entry_of(path: &str, metadata: &Metadata, follow: bool, hash: Option<u64>) -> Entry {
    let kind = kind_of(metadata);
    let (dev, ino) = sys::path_identity(path, metadata, follow);
    let directory = kind == Kind::Directory;
    Entry {
        kind,
        dev,
        ino,
        size: if directory { 0 } else { metadata.len() },
        mtime: if directory {
            (0, 0)
        } else {
            sys::mtime(metadata)
        },
        hash,
    }
}

/// Ancestors of `path`, nearest first, up to the root.
fn ancestors(path: &str) -> Vec<String> {
    Path::new(path)
        .ancestors()
        .skip(1)
        .map(|ancestor| ancestor.to_string_lossy().into_owned())
        .collect()
}

/// 给目录补一个平台分隔符；已经以分隔符结尾时原样返回，避免出现 `//`。
fn with_separator(directory: &str) -> String {
    if directory.ends_with(sys::PATH_SEPARATOR) {
        directory.to_string()
    } else {
        format!("{directory}{}", sys::PATH_SEPARATOR)
    }
}

/// Whether `path` is `ancestor` or below it.
///
/// 前缀比较必须带上分隔符：`/a` 是 `/a/b` 的祖先，但不能把 `/ab` 误判为在 `/a` 之下。
fn is_within(path: &str, ancestor: &str) -> bool {
    path == ancestor || path.starts_with(&with_separator(ancestor))
}

/// 按名字判断是否排除：`hidden` 排除以 `.` 开头的名字，`names` 是精确名字集合；任一命中即排除。
/// 规则只看单个路径组件，不看完整路径，调用方负责把它应用到每一层。
fn excluded(target: &Target, name: &str) -> bool {
    (target.hidden && name.starts_with('.')) || target.names.contains(name)
}

/// JavaScript's default sort: by UTF-16 code units.
///
/// 必须与客户端 `Array.prototype.sort()` 的默认顺序一致；UTF-16 code unit 顺序不同于 UTF-8 字节序，
/// 例如 emoji（代理对 D83D DE00）会排在 U+FF5E 之前，而按 UTF-8 字节比较会排到最后。
fn sort_like_javascript(paths: &mut [String]) {
    paths.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
}

/// Whether any path, or its nearest existing ancestor, is on a file system that does not report remote changes.
///
/// 网络/用户态文件系统（NFS、SMB、FUSE、9P 等，见 `UNRELIABLE_FILE_SYSTEMS`）能安装 watcher，
/// 但不会报告其他机器上的修改，所以命中时强制 polling。判定方式：
/// 对每个 target 从自身到祖先依次尝试 `statfs`，取第一个成功的候选；全部失败视为可靠
/// （拿不到信息时不改变默认行为）。只在 Linux/Android 上有实际实现。
#[cfg(any(target_os = "linux", target_os = "android"))]
fn any_unreliable(targets: &[Target]) -> bool {
    use std::ffi::CString;
    for target in targets {
        // 先看 target 自身；statfs 失败（路径不存在、无权限等）时逐级退到祖先。
        // 文件系统类型由所在挂载点决定，所以第一个 statfs 成功的候选就是答案。
        for candidate in std::iter::once(target.path.clone()).chain(ancestors(&target.path)) {
            let Ok(name) = CString::new(candidate) else {
                // 路径含内嵌 NUL（非法路径）时无法传给 C，跳过该 target 的探测。
                break;
            };
            // SAFETY: `CString` 保证 `name` 以 NUL 结尾且不含内嵌 NUL；`statfs` 只读取该字符串并填充
            // 本函数独占的 `info` 缓冲区；只有返回 0 时才读取 `info.f_type`。
            let mut info: libc::statfs = unsafe { std::mem::zeroed() };
            if unsafe { libc::statfs(name.as_ptr(), &mut info) } != 0 {
                // 该候选 statfs 失败：继续尝试祖先，而不是立刻判定为可靠。
                continue;
            }
            // f_type 在不同平台宽度不同，统一截到 32 位再查表（表里的魔数都是 32 位）。
            #[allow(clippy::unnecessary_cast)]
            let kind = (info.f_type as u64) & 0xffff_ffff;
            if UNRELIABLE_FILE_SYSTEMS.contains(&kind) {
                return true;
            }
            // 第一个能 statfs 成功的候选已代表该路径所在的挂载点，无需再看更上层祖先。
            break;
        }
    }
    false
}

/// 非 Linux 平台不探测文件系统类型，一律视为可靠；Windows 的强制 polling 在 `run` 里单独判断。
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn any_unreliable(_targets: &[Target]) -> bool {
    false
}

/// One scan: the snapshot, and targets that are symbolic links to files, whose files need their own watchers.
///
/// 一次扫描的中间结果：
/// - `snapshot`：本轮视图；
/// - `linked_files`：指向文件的符号链接（target 自身），其内容变化只会以目标文件事件出现，需额外 watch；
/// - `counted`：全局目录预算计数，同一目录只算一次；
/// - `traversed`：`(target 下标, 目录)` 去重，保证同一目录对每个 target 只递归一次；
/// - `listed`：本轮 `lstat` 结果缓存，同一路径被多个 target 看到时避免重复系统调用。
struct Scan {
    snapshot: Snapshot,
    linked_files: HashSet<String>,
    /// Directories counted against the limit, and directories traversed per target.
    counted: HashSet<String>,
    traversed: HashSet<(usize, String)>,
    /// Kinds of listed entries without following links.
    listed: HashMap<String, Kind>,
}

/// 一个已成功安装的 watcher 记录。
struct Installed {
    /// 安装时路径的 `dev`/`ino`；重扫时不同说明该路径已被删除重建（或挂载点更换），
    /// watcher 跟随的是旧 inode，必须替换。
    dev: u64,
    ino: u64,
    /// The watched path with links resolved, as FSEvents spells event paths (`/private/var` for `/var`).
    ///
    /// 只在解析结果与监视路径不同时有值；用于把平台事件里的规范路径翻译回客户端传入的写法。
    canonical: Option<String>,
}

/// 一个 watch 请求的全部可变状态，只在处理该请求的线程里访问，因此不使用 `Arc`/锁。
struct FileWatcher<'a> {
    /// 请求帧 id，所有事件沿用同一个 id，客户端据此把事件归到对应请求。
    id: u32,
    output: &'a Output,
    control: &'a Control,
    targets: Vec<Target>,
    mode: Mode,
    poll_interval: Duration,
    max_directories: usize,
    /// 上一次成功扫描的结果；`sync` 里整体替换。
    snapshot: Snapshot,
    /// `notify` 的 watcher。切到 polling 后置 `None`；此后不再有原生事件，变化改由轮询发现。
    native: Option<RecommendedWatcher>,
    /// 原生事件通道的发送端；watcher 回调持有它的克隆，主循环持有接收端。
    events_sender: Sender<notify::Result<Event>>,
    /// 路径 → 已安装 watcher 的记录。
    installed: HashMap<String, Installed>,
    /// 本轮由原生事件推出的候选变化路径（已经过 `reported` 归一化）。
    events: HashSet<String>,
    /// macOS FSEvents 的 settle 截止时间：watcher 安装后延迟一小段时间再重扫，补上安装前的空隙。
    settle_at: Option<Instant>,
}

impl FileWatcher<'_> {
    /// 发一个 `EVENT` 帧给该请求的客户端。走 `Output::control`，保证事件排在命令的大量输出之前。
    fn send(&self, json: Value) {
        self.output.control(Frame::new(EVENT, self.id, json));
    }

    /// 是否收到取消。
    ///
    /// 只看 `Control::aborted`：普通取消会设置它。`Control::cancel(true)`（`{ mode: "kill" }`）
    /// 只设置 `killed`，本模块不读取该字段，因此 kill 模式不会让 watch 主循环退出；
    /// `docs/protocol.md` 的取消一节只约定"watch 停止并返回 `{}`"，未单列 kill，调用方应使用普通取消。
    fn cancelled(&self) -> bool {
        self.control.aborted.load(Ordering::SeqCst)
    }

    /// 把 `path` 的 `metadata` 记入本轮快照，必要时附带内容哈希。
    ///
    /// polling 模式下的哈希策略：只对"普通文件、大小不超过 `HASH_MAX_BYTES`、mtime 在 `HASH_RECENT` 内"
    /// 的路径读全文算哈希。原因是文件系统 mtime 精度可能不足以区分紧邻的两次写入，
    /// 而大小也可能不变；内容哈希能发现这种改动。读取失败（如文件刚被删除）时 `hash` 保持 `None`，
    /// 退化为只比较元数据，不会让整次扫描失败。
    fn record(&self, scan: &mut Scan, path: &str, metadata: &Metadata, follow: bool) {
        let mut hash = None;
        let recent = metadata
            .modified()
            .map(|modified| {
                SystemTime::now()
                    .duration_since(modified)
                    .map_or(true, |age| age < HASH_RECENT)
            })
            .unwrap_or(false);
        if self.mode == Mode::Polling
            && metadata.is_file()
            && metadata.len() <= HASH_MAX_BYTES
            && recent
        {
            hash = fs::read(path).ok().map(|content| {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                hasher.write(&content);
                hasher.finish()
            });
        }
        scan.snapshot
            .insert(path.to_string(), entry_of(path, metadata, follow, hash));
    }

    /// 把一个目录计入全局预算；超过 `max_directories` 立即失败。
    ///
    /// 预算是所有 target 共享的（`counted` 挂在本次扫描上），为递归扫描与 watcher 安装设置一个保护性上限。
    /// 打开阶段超限变成 `EINVAL` 请求错误，后续重扫超限变成 `error` 事件。
    fn count(&self, scan: &mut Scan, path: &str) -> Result<(), ScanError> {
        scan.counted.insert(path.to_string());
        if scan.counted.len() > self.max_directories {
            return Err(ScanError::Budget(self.max_directories));
        }
        Ok(())
    }

    /// 遍历 `directory` 这一层；`index` 是 target 下标，用于按 target 去重。
    ///
    /// 失败策略与参考实现一致：读取失败时，只有 **target 根目录因权限被拒** 才算扫描失败；
    /// 其余情况（根目录在扫描途中消失、子目录不存在/无权限/已不是目录）直接跳过——
    /// 监视一棵树时，子目录被删除或权限收紧不应让整个 watch 结束；其他 IO 错误仍然上抛。
    fn scan_directory(
        &self,
        scan: &mut Scan,
        index: usize,
        directory: &str,
    ) -> Result<(), ScanError> {
        let target = &self.targets[index];
        // 同一目录可能被多个 target 覆盖（target 嵌套）；每个 target 只需遍历一次。
        if !scan.traversed.insert((index, directory.to_string())) {
            return Ok(());
        }
        let skip = |error: io::Error| -> Result<(), ScanError> {
            // The watched directory itself must be readable; below it, unreadable directories are skipped.
            if directory == target.path && denied(&error) {
                return Err(ScanError::Io(error, "scandir", directory.to_string()));
            }
            match error.kind() {
                io::ErrorKind::NotFound
                | io::ErrorKind::PermissionDenied
                | io::ErrorKind::NotADirectory => Ok(()),
                _ => Err(ScanError::Io(error, "scandir", directory.to_string())),
            }
        };
        // 先读完这一层的名字再逐个处理：迭代器自身的错误与 read_dir 的错误走同一 skip 策略；
        // 中途出错时整层放弃（已收集的名字不再处理），等同于"这个目录读不到"。
        let mut names = Vec::new();
        match fs::read_dir(directory) {
            Err(error) => return skip(error),
            Ok(entries) => {
                for entry in entries {
                    match entry {
                        Ok(entry) => names.push(entry.file_name().to_string_lossy().into_owned()),
                        Err(error) => return skip(error),
                    }
                }
            }
        }
        let prefix = with_separator(directory);
        for name in names {
            if excluded(target, &name) {
                continue;
            }
            let path = format!("{prefix}{name}");
            // 同一路径可能被另一个 target 先看到：复用本轮 `lstat` 缓存，避免重复系统调用。
            let kind = match scan.listed.get(&path) {
                Some(kind) => *kind,
                None => {
                    let Ok(metadata) = fs::symlink_metadata(&path) else {
                        continue;
                    };
                    let kind = kind_of(&metadata);
                    scan.listed.insert(path.clone(), kind);
                    // A target's own entry (following links) wins over its listing by another target.
                    // 即：target 自身用 `stat`（跟随链接）写下的条目，不被这里用 `lstat` 的结果覆盖。
                    if !scan.snapshot.contains_key(&path) {
                        self.record(scan, &path, &metadata, false);
                    }
                    kind
                }
            };
            if target.recursive && kind == Kind::Directory {
                self.count(scan, &path)?;
                self.scan_directory(scan, index, &path)?;
            }
        }
        Ok(())
    }

    /// 扫描全部 target，构造一份新快照。
    ///
    /// 对每个 target 依次做三件事：
    /// 1. 记录它的全部祖先（不跟随链接），只保留身份字段——祖先的 dev/ino 变化意味着整棵子树被移动或替换；
    /// 2. 用 `stat` 记录 target 自身（跟随符号链接）；target 暂时不存在时跳过并继续，等待它出现，
    ///    只有权限错误才让扫描失败；若它是"指向文件的符号链接"，记入 `linked_files` 以便单独 watch；
    /// 3. target 是目录时计入预算并递归遍历（是否下探子目录由 `recursive` 决定）。
    ///
    /// 快照的键基于客户端给出的路径写法；祖先条目由 `contains_key` 去重（先写入者生效），
    /// 而 target 自身用 `stat` 写入时会覆盖它先前作为"别人的祖先/子项"留下的条目——
    /// 这正是"跟随符号链接的 target 自身优先"的实现方式。
    fn scan(&self) -> Result<Scan, ScanError> {
        let mut scan = Scan {
            snapshot: Snapshot::new(),
            linked_files: HashSet::new(),
            counted: HashSet::new(),
            traversed: HashSet::new(),
            listed: HashMap::new(),
        };
        for (index, target) in self.targets.iter().enumerate() {
            for ancestor in ancestors(&target.path) {
                if scan.snapshot.contains_key(&ancestor) {
                    continue;
                }
                // Identity only: an ancestor's own timestamps change with every unrelated sibling.
                // 祖先只记录 dev/ino，size/mtime 归零，避免无关兄弟的创建/删除/改名被误报成变化。
                // 取不到祖先 metadata 时直接跳过（不视为失败）：只有 target 自身不可读才是致命错误。
                if let Ok(metadata) = fs::symlink_metadata(&ancestor) {
                    let entry = Entry {
                        size: 0,
                        mtime: (0, 0),
                        ..entry_of(&ancestor, &metadata, false, None)
                    };
                    scan.snapshot.insert(ancestor, entry);
                }
            }
            // The target may be a symbolic link to what is watched; follow it. A missing target is watched for its
            // creation; one that cannot be reached for lack of permission fails.
            // 注意这里是 `stat`（跟随链接）：监视的是链接指向的对象，而不是链接本身。
            let metadata = match fs::metadata(&target.path) {
                Ok(metadata) => metadata,
                Err(error) if denied(&error) => {
                    return Err(ScanError::Io(error, "stat", target.path.clone()));
                }
                // 其他错误（ENOENT 等）视为"暂时不存在"：保留 target，等它创建后由事件/轮询发现。
                Err(_) => continue,
            };
            self.record(&mut scan, &target.path, &metadata, true);
            if metadata.is_file()
                && fs::symlink_metadata(&target.path)
                    .is_ok_and(|link| link.file_type().is_symlink())
            {
                // 指向文件的符号链接：其内容变化以目标文件事件出现，不在链接所在目录的事件里。
                scan.linked_files.insert(target.path.clone());
            }
            if metadata.is_dir() {
                self.count(&mut scan, &target.path)?;
                self.scan_directory(&mut scan, index, &target.path)?;
            }
        }
        Ok(scan)
    }

    /// An ancestor that changed identity moved every target below it; report those targets.
    ///
    /// 用于把内部路径归一化成客户端关心的路径：
    /// - 路径本身在某个 target 内 → 原样返回；
    /// - 路径是某个 target 的祖先 → 返回该 target 的根（祖先的身份变化会影响整棵 target）；
    /// - 都不匹配 → 原样返回。
    fn reported(&self, path: &str) -> String {
        if self
            .targets
            .iter()
            .any(|target| is_within(path, &target.path))
        {
            return path.to_string();
        }
        self.targets
            .iter()
            .find(|target| is_within(&target.path, path))
            .map_or_else(|| path.to_string(), |target| target.path.clone())
    }

    /// 比较两次快照，返回变化路径的集合（可能重复，调用方用 `HashSet` 去重）。
    ///
    /// 两个方向都要看：新快照里新增或值不同的路径，以及旧快照里有、新快照里已消失的路径。
    /// "值不同"由 `Entry` 的 `PartialEq` 决定（目录只看 dev/ino）。每条路径都先经过 `reported`，
    /// 这样祖先身份变化会落到某个 target 根上。注意落到哪一个由 `targets` 数组顺序决定：
    /// `reported` 用 `find` 返回第一个包含该路径的 target，而不是层级上最外层的 target
    /// （例如 targets 为 `["/a/b", "/a"]` 时，`"/"` 的祖先变化报告的是 `"/a/b"`）。
    fn diff(&self, previous: &Snapshot, next: &Snapshot) -> Vec<String> {
        let mut changed = Vec::new();
        for (path, entry) in next {
            if previous.get(path) != Some(entry) {
                changed.push(self.reported(path));
            }
        }
        for path in previous.keys() {
            if !next.contains_key(path) {
                changed.push(self.reported(path));
            }
        }
        changed
    }

    /// Rescan, report differences, and install watchers for new directories, rescanning until none are new.
    ///
    /// 为什么需要循环：一个刚创建并立刻写入内容的新目录，在第一轮扫描时还没有它的 watcher；
    /// 只有再扫一轮（此时 `report` 已为 true）才能把这段写入算进差异。最多 10 轮，避免与
    /// 持续变动的目录树形成无限循环。
    ///
    /// `report == false` 用于首次打开：只建立快照，不报告差异，否则所有已存在的文件都会被当成"新增"。
    /// 返回本次发现的变化路径；扫描失败原样上抛，由调用方决定变成请求错误还是 `error` 事件。
    fn sync(&mut self, mut report: bool) -> Result<HashSet<String>, ScanError> {
        let mut changed = HashSet::new();
        for _ in 0..10 {
            if self.cancelled() {
                break;
            }
            let scan = self.scan()?;
            if report {
                changed.extend(self.diff(&self.snapshot, &scan.snapshot));
            }
            self.snapshot = scan.snapshot;
            // polling 没有 watcher 需要协调，扫一轮即可；native 下若没有新增 watcher，快照已稳定。
            if self.mode == Mode::Polling || !self.reconcile(&scan.linked_files) {
                break;
            }
            if cfg!(target_os = "macos") {
                self.settle_at = Some(Instant::now() + FSEVENTS_SETTLE);
            }
            // Something written into a new directory before its watcher existed shows up in the next round.
            // 首轮之后都报告差异，逐轮累积，调用方最终拿到的是并集。
            report = true;
        }
        Ok(changed)
    }

    /// Watch every existing ancestor of each target, each target directory, each target that links to a file, and on
    /// Linux each directory below a recursive target (elsewhere one recursive watcher per target). Returns whether a
    /// watcher was added.
    ///
    /// 三类需要 watch 的路径：
    /// 1. 每个 target 的已存在祖先目录：祖先的身份变化（改名、删除、移动）会影响它下面的 target，
    ///    必须在祖先上装 watcher 才能观察到；
    /// 2. target 目录本身，以及指向文件的符号链接（其内容变化只会以目标文件事件出现）；
    /// 3. Linux/Android 上代码选择逐目录安装（`per_directory`）：递归 target 下已经知道的每个子目录
    ///    各装一个非递归 watcher；其他平台每个 target 一个 watcher，是否递归由 `target.recursive`
    ///    决定（默认 `false` 的 target 得到的是非递归 watcher，而不是递归 watcher）。
    /// 返回 `true` 表示本轮装了新 watcher，`sync` 会再扫一轮补齐间隙内的变化。
    fn reconcile(&mut self, linked_files: &HashSet<String>) -> bool {
        let per_directory = cfg!(any(target_os = "linux", target_os = "android"));
        // 值表示安装时是否使用递归模式：祖先目录一律非递归；target 目录在非 Linux 平台按自身
        // `recursive` 决定，在 Linux 上 target 目录本身也非递归（子目录逐项加入）。
        let mut wanted: HashMap<String, bool> = linked_files
            .iter()
            .map(|path| (path.clone(), false))
            .collect();
        for target in &self.targets {
            for ancestor in ancestors(&target.path) {
                if self
                    .snapshot
                    .get(&ancestor)
                    .is_some_and(|entry| entry.kind == Kind::Directory)
                {
                    wanted.entry(ancestor).or_insert(false);
                }
            }
            // target 不存在、或不是目录（普通文件、指向文件的链接等）时没有子树要 watch；
            // 指向文件的链接已由 `linked_files` 单独加入 `wanted`。
            if self
                .snapshot
                .get(&target.path)
                .is_none_or(|entry| entry.kind != Kind::Directory)
            {
                continue;
            }
            wanted.insert(target.path.clone(), target.recursive && !per_directory);
            if target.recursive && per_directory {
                for (path, entry) in &self.snapshot {
                    if entry.kind == Kind::Directory
                        && *path != target.path
                        && is_within(path, &target.path)
                    {
                        wanted.insert(path.clone(), false);
                    }
                }
            }
        }
        // Gone, or replaced: a watcher follows the directory it was installed on, not the path.
        // watcher 绑定的是安装时的 inode：路径仍在但 dev/ino 变了（删除后同名重建、挂载点更换），
        // 旧 watcher 已经看不到新目录，必须卸载后重装。
        let stale: Vec<String> = self
            .installed
            .iter()
            .filter(|(path, installed)| {
                !wanted.contains_key(*path)
                    || self.snapshot.get(*path).is_none_or(|entry| {
                        entry.dev != installed.dev || entry.ino != installed.ino
                    })
            })
            .map(|(path, _)| path.clone())
            .collect();
        for path in stale {
            self.installed.remove(&path);
            if let Some(native) = &mut self.native {
                let _ = native.unwatch(Path::new(&path));
            }
        }
        let mut added = false;
        for (path, recursive) in wanted {
            let Some(entry) = self.snapshot.get(&path) else {
                continue;
            };
            if self.installed.contains_key(&path) {
                continue;
            }
            let (dev, ino) = (entry.dev, entry.ino);
            if self.native.is_none() {
                let sender = self.events_sender.clone();
                // 回调用克隆的发送端把事件送回主循环线程；接收端断开时 `send` 只返回错误，被丢弃。
                match RecommendedWatcher::new(
                    move |event| drop(sender.send(event)),
                    notify::Config::default(),
                ) {
                    Ok(native) => self.native = Some(native),
                    Err(_) => {
                        // 连 watcher 实例都建不起来：原生模式不可用，整个请求退化为 polling。
                        self.switch_to_polling();
                        return false;
                    }
                }
            }
            let mode = if recursive {
                RecursiveMode::Recursive
            } else {
                RecursiveMode::NonRecursive
            };
            match self.native.as_mut().unwrap().watch(Path::new(&path), mode) {
                Ok(()) => {
                    // 记录规范路径（解析符号链接后的写法），事件来自平台时用它翻译回监视路径。
                    // 与原路径相同则不存：翻译时两者没有区别，`watched_spelling` 无需为它比较。
                    let canonical = fs::canonicalize(&path)
                        .ok()
                        .map(|canonical| canonical.to_string_lossy().into_owned())
                        .filter(|canonical| *canonical != path);
                    self.installed.insert(
                        path,
                        Installed {
                            dev,
                            ino,
                            canonical,
                        },
                    );
                    added = true;
                }
                Err(error) => {
                    let gone = match &error.kind {
                        notify::ErrorKind::PathNotFound => true,
                        notify::ErrorKind::Io(io) => {
                            matches!(
                                io.kind(),
                                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                            )
                        }
                        _ => false,
                    };
                    // Out of watches or unsupported: compare snapshots from now on, and say coverage was uncertain.
                    // 路径只是暂时消失/无权限：跳过，不切模式，下一轮扫描会重试安装。
                    if !gone {
                        // 其他错误（如 ENOSPC 耗尽 watch 配额、平台不支持递归）意味着原生覆盖不可靠：
                        // 切到 polling，并先发 `overflow` 告诉客户端这段时间的事件不可信。
                        self.switch_to_polling();
                        return false;
                    }
                }
            }
        }
        added
    }

    /// 从原生事件模式退化为轮询模式。
    ///
    /// 顺序与实现一致：先把 `mode` 置为 `Polling`、`native` 置 `None`、`installed` 清空，
    /// 最后才发一个 `{ kind: "change", overflow: true, mode: "polling" }`。`overflow` 的含义是
    /// "这段时间的覆盖不可靠，请做一次全量重扫"，客户端据此重新拉取完整状态。
    /// 清空 `native` 后不会再产生新的原生事件，变化改由轮询发现；但 `events` 通道里已经入队的
    /// 旧事件仍会被 `run` 的主循环取出交给 `on_event`（那里无条件处理，只是模式不再是 `Native`，
    /// 因此不会据此安排防抖重扫），仍可能并入下一次 `change` 的路径集合。
    /// 幂等：已经是 polling 时直接返回，不会重复发 `overflow`。
    fn switch_to_polling(&mut self) {
        if self.mode == Mode::Polling {
            return;
        }
        self.mode = Mode::Polling;
        self.native = None;
        self.installed.clear();
        self.send(json!({ "kind": "change", "overflow": true, "mode": "polling" }));
    }

    /// 判断原生事件路径是否属于某个 target 的监视范围。
    ///
    /// 规则（按 target 依次尝试）：
    /// - 路径是 target 的祖先 → 在范围内（祖先的身份变化会移动 target）；
    /// - 路径不在 target 之下 → 跳过；
    /// - 非递归 target 只接受一层子项（相对路径含分隔符即拒绝）；
    /// - 任一路径组件命中该 target 的排除规则 → 跳过。
    /// 调用方（`on_event`）已在之前过滤掉 `Access` 事件。
    fn in_scope(&self, path: &str) -> bool {
        for target in &self.targets {
            if is_within(&target.path, path) {
                return true;
            }
            if !is_within(path, &target.path) || path == target.path {
                continue;
            }
            let relative = &path[with_separator(&target.path).len()..];
            let components: Vec<&str> = relative.split(sys::PATH_SEPARATOR).collect();
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

    /// An event path in the spelling of the watched path it is under.
    ///
    /// 有些平台会把事件路径规范化为解析链接后的写法（macOS 的 `/var` 实际是 `/private/var`），
    /// 而快照的键是客户端传入的写法。这里在 `installed` 里找"能包含该路径的最长规范前缀"，
    /// 用对应的监视路径前缀替换，保证事件路径与快照键可以直接比较/去重。
    fn watched_spelling(&self, path: &str) -> String {
        let mut best: Option<(&str, &str)> = None;
        for (watched, installed) in &self.installed {
            if let Some(canonical) = &installed.canonical
                && is_within(path, canonical)
                && best.is_none_or(|(_, longest)| canonical.len() > longest.len())
            {
                best = Some((watched, canonical));
            }
        }
        match best {
            Some((watched, canonical)) => format!("{watched}{}", &path[canonical.len()..]),
            None => path.to_string(),
        }
    }

    /// Note a native event; returns whether it calls for a rescan.
    ///
    /// 处理顺序：
    /// 1. 事件本身是错误（`notify::Result::Err`）→ 移除相关路径的安装记录并安排重扫，重扫会按需重装 watcher；
    /// 2. `Access` 类事件（打开、关闭、读取等）不表示内容变化，直接忽略；
    /// 3. `need_rescan()`（事件队列溢出等）或路径为空 → 必须重扫；
    /// 4. 其余事件逐路径翻译写法、按 `in_scope` 过滤、经 `reported` 归一化后放入 `events`。
    /// 返回值只表示"是否需要安排一次 flush"，不表示是否真的发现了变化。
    fn on_event(&mut self, event: notify::Result<Event>) -> bool {
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                // A watched directory that disappears or fails: rescan, which also replaces its watcher.
                for path in &error.paths {
                    self.installed.remove(&*path.to_string_lossy());
                }
                return true;
            }
        };
        // Reads are not changes (libuv does not ask for them).
        if matches!(event.kind, EventKind::Access(_)) {
            return false;
        }
        // 事件流不可靠（如内核队列溢出）或没有给出路径：无法判断具体变化，直接整树重扫。
        if event.need_rescan() || event.paths.is_empty() {
            return true;
        }
        let mut flush = false;
        for path in &event.paths {
            let path = self.watched_spelling(&path.to_string_lossy());
            // Events about unrelated siblings of an ancestor, or about excluded entries, are ignored.
            if self.in_scope(&path) {
                let reported = self.reported(&path);
                self.events.insert(reported);
                flush = true;
            }
        }
        flush
    }

    /// Rescan and report what changed; an error ends the watcher.
    ///
    /// 步骤：
    /// 1. 先取走 `events`（本轮事件已消费，无论成败都不留到下一轮）；
    /// 2. `sync(true)` 重扫并 diff；
    /// 3. 合并 diff 与事件路径、按 JS 顺序排序；非空且未取消才发 `change`；
    /// 4. 成功返回 `true`；扫描失败发 `error` 事件并返回 `false`，调用方据此结束整个 watch。
    /// 取消后不再发事件，避免客户端已经关闭请求还收到帧。
    fn flush(&mut self) -> bool {
        let events = std::mem::take(&mut self.events);
        match self.sync(true) {
            Ok(mut changed) => {
                changed.extend(events);
                if !changed.is_empty() && !self.cancelled() {
                    let mut paths: Vec<String> = changed.into_iter().collect();
                    sort_like_javascript(&mut paths);
                    self.send(json!({ "kind": "change", "paths": paths }));
                }
                true
            }
            Err(error) => {
                let message = error.failure().message;
                self.send(json!({ "kind": "error", "code": error.code(), "message": message }));
                false
            }
        }
    }
}

/// 解析请求里的 `targets` 数组。
///
/// 默认值：`recursive` 为 `false`，`exclude.hidden` 为 `false`，`exclude.names` 缺失/非数组时为空集。
/// 容忍策略：`names` 里的非字符串项被静默丢弃；缺 `targets` 或某项缺 `path` 都返回同一条
/// `EINVAL`（与参考实现共用一个 `invalid` 闭包，错误文案相同）。
fn parse_targets(json: &Value) -> Result<Vec<Target>, Failure> {
    let invalid = || Failure::new("EINVAL", "watch needs targets");
    json["targets"]
        .as_array()
        .ok_or_else(invalid)?
        .iter()
        .map(|target| {
            Ok(Target {
                path: target["path"].as_str().ok_or_else(invalid)?.to_string(),
                recursive: target["recursive"].as_bool().unwrap_or(false),
                hidden: target["exclude"]["hidden"].as_bool().unwrap_or(false),
                names: target["exclude"]["names"]
                    .as_array()
                    .map(|names| {
                        names
                            .iter()
                            .filter_map(|name| name.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// Watch until cancelled: a `ready` event once coverage is established, then `change` events, or an `error` event
/// after which nothing follows.
///
/// `op == "watch"` 的处理函数，也是本模块唯一的公开入口。
///
/// 参数：
/// - `id`：请求帧 id；`ready` / `change` / `error` 事件都复用同一个 id；
/// - `json`：请求体，读取 `targets`、可选 `mode`（`"native"` / `"polling"`）、`pollIntervalMs`、`maxDirectories`；
/// - `output`：事件出口，走 control 队列（优先于 bulk 输出）；
/// - `control`：取消标志，循环顶部与 `sync` 内部都会检查。
///
/// 返回值：取消时返回 `{}`，由 `main.rs` 作为该请求的 `result` 帧发出；打开阶段失败返回 `Err(Failure)`，
/// 变成 `error` 帧。注意本函数的生命周期很长：`result` 只在结束时出现，期间变化都靠事件帧推送。
///
/// 事件序列：首次 `sync` 期间若原生 watcher 安装失败，会先发一次 `overflow`（此时模式已切到 polling）；
/// 随后 `ready`（恰好一次）→ 若干 `change` → 取消时 `result`。
/// 若重扫失败，则先发一次 `error`，随后同样以 `result` 结束，之后不会再有事件。
///
/// 边界：`pollIntervalMs` 与 `maxDirectories` 只做 `as_u64` 解析，没有上下限校验；
/// 传 0 会让 polling 没有间隔（`maxDirectories` 为 0 时任何目录都会立刻超预算），调用方应传合理值。
///
/// 模式选择：显式 `mode` 优先；否则 Windows（watcher 长期占用目录句柄，会阻止重命名）和
/// 不可靠文件系统（见 `any_unreliable`）用 polling，其余用 native。
pub fn run(id: u32, json: &Value, output: &Output, control: &Control) -> Result<Value, Failure> {
    let targets = parse_targets(json)?;
    // Windows refuses to rename a directory while a directory below it is open, and native watchers keep watched
    // directories open, so Windows polls, as do file systems that do not report remote changes.
    let mode = match json["mode"].as_str() {
        Some("native") => Mode::Native,
        Some("polling") => Mode::Polling,
        _ if cfg!(windows) || any_unreliable(&targets) => Mode::Polling,
        _ => Mode::Native,
    };
    // notify 的回调在它自己的线程执行，事件必须经通道送回本线程；接收端在下面的循环里带超时读取。
    let (events_sender, events) = mpsc::channel();
    let mut watcher = FileWatcher {
        id,
        output,
        control,
        targets,
        mode,
        poll_interval: Duration::from_millis(
            json["pollIntervalMs"].as_u64().unwrap_or(DEFAULT_POLL_MS),
        ),
        max_directories: json["maxDirectories"]
            .as_u64()
            .map_or(DEFAULT_MAX_DIRECTORIES, |max| max as usize),
        snapshot: Snapshot::new(),
        native: None,
        events_sender,
        installed: HashMap::new(),
        events: HashSet::new(),
        settle_at: None,
    };
    // 首次同步只建立快照和 watcher，不报告差异：否则所有已存在的文件都会被当成新增。
    // 这一步失败是请求级错误（与后续重扫失败不同）。
    watcher.sync(false).map_err(|error| error.failure())?;
    // `ready` 必须在首次 sync 之后发：客户端看到它时快照已经可用，之后的变化不会漏。
    watcher.send(json!({ "kind": "ready", "mode": watcher.mode.name() }));
    let mut flush_at: Option<Instant> = None;
    let mut poll_at = Instant::now() + watcher.poll_interval;
    loop {
        if watcher.cancelled() {
            return Ok(json!({}));
        }
        let now = Instant::now();
        // 取所有可能醒来的时间中最早的一个：取消检查、防抖截止、FSEvents settle、轮询到期。
        // `recv_timeout` 到点就返回，避免忙等；没有事件时也按这个节拍检查取消。
        let mut wake = now + CANCEL_POLL;
        for at in [flush_at, watcher.settle_at].into_iter().flatten() {
            wake = wake.min(at);
        }
        if watcher.mode == Mode::Polling {
            wake = wake.min(poll_at);
        }
        match events.recv_timeout(wake.saturating_duration_since(now)) {
            Ok(event) => {
                // 事件只用于"排定一次防抖后的重扫"；真正的变化由 flush 里的快照 diff 决定。
                // 只有 native 模式需要防抖；polling 模式没有原生事件。
                if watcher.on_event(event) && flush_at.is_none() && watcher.mode == Mode::Native {
                    flush_at = Some(Instant::now() + DEBOUNCE);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            // watcher 自己持有发送端（`events_sender`），接收端不可能先看到断开。
            Err(RecvTimeoutError::Disconnected) => unreachable!("the watcher holds a sender"),
        }
        let now = Instant::now();
        let due = |at: Option<Instant>| at.is_some_and(|at| at <= now);
        let polling_due = watcher.mode == Mode::Polling && poll_at <= now;
        // 事件之外的醒来原因：防抖到期、FSEvents settle 到期、polling 到期。三者任一都触发一次 flush。
        if due(flush_at) || due(watcher.settle_at) || polling_due {
            if due(flush_at) {
                flush_at = None;
            }
            if due(watcher.settle_at) {
                watcher.settle_at = None;
            }
            // flush 只有在发出 `error` 事件后才返回 false：那之后 watcher 已结束。
            if !watcher.flush() {
                return Ok(json!({}));
            }
            if polling_due {
                poll_at = Instant::now() + watcher.poll_interval;
            }
        }
    }
}

// 测试锁定的是这些纯函数行为：路径分隔与包含判断、祖先顺序、排除规则、
// UTF-16 码元排序、错误码映射、targets 解析默认值，以及 FileWatcher 的 scope/reported/spelling 逻辑。
// 涉及文件系统的用例只使用临时目录，不依赖仓库内容。
#[cfg(test)]
mod tests {
    use super::*;

    fn target(path: &str) -> Target {
        Target {
            path: path.to_string(),
            recursive: true,
            hidden: false,
            names: HashSet::new(),
        }
    }

    #[test]
    fn separators_and_containment_follow_the_platform() {
        let separator = sys::PATH_SEPARATOR;
        assert_eq!(with_separator("/a"), format!("/a{separator}"));
        assert_eq!(
            with_separator(&format!("/a{separator}")),
            format!("/a{separator}"),
            "an existing separator is not doubled"
        );
        assert!(is_within("/a", "/a"), "a path contains itself");
        assert!(is_within(&format!("/a{separator}b"), "/a"));
        assert!(is_within(&format!("/a{separator}b{separator}c"), "/a/b"));
        assert!(
            !is_within("/a", &format!("/a{separator}b")),
            "a parent is not inside its child"
        );
        assert!(
            !is_within("/ab", "/a"),
            "no prefix match across a separator"
        );
    }

    #[test]
    fn ancestors_are_nearest_first_and_exclude_the_path_itself() {
        let separator = sys::PATH_SEPARATOR;
        let path = format!("/a{separator}b{separator}c");
        assert_eq!(
            ancestors(&path),
            [format!("/a{separator}b"), "/a".to_string(), "/".to_string()]
        );
    }

    #[test]
    fn exclusions_cover_hidden_names_and_the_named_set() {
        let mut target = target("/a");
        assert!(!excluded(&target, ".hidden"));
        assert!(!excluded(&target, "visible"));
        target.hidden = true;
        assert!(excluded(&target, ".hidden"));
        assert!(!excluded(&target, "visible"));
        target.names.insert("skip.txt".into());
        assert!(excluded(&target, "skip.txt"));
        assert!(!excluded(&target, "other.txt"));
    }

    #[test]
    fn sorting_is_by_utf16_code_units() {
        // JavaScript's default sort compares UTF-16 code units, so an astral character (surrogate D83D DE00)
        // sorts before U+FF5E; UTF-8 byte order would put it last.
        let mut paths = vec![
            "😀".to_string(),
            "～".to_string(),
            "a".to_string(),
            "A".to_string(),
        ];
        sort_like_javascript(&mut paths);
        assert!(paths == ["A", "a", "😀", "～"], "got {paths:?}");
        let mut mixed = vec!["b/c".to_string(), "b".to_string(), "a/x".to_string()];
        sort_like_javascript(&mut mixed);
        assert!(mixed == ["a/x", "b", "b/c"]);
    }

    #[test]
    fn modes_are_named_as_the_protocol_says() {
        assert_eq!(Mode::Native.name(), "native");
        assert_eq!(Mode::Polling.name(), "polling");
    }

    #[test]
    fn scan_errors_map_to_the_documented_failures() {
        let budget = ScanError::Budget(10_000);
        let failure = budget.failure().to_json();
        assert_eq!(failure["code"], json!("EINVAL"));
        assert_eq!(
            failure["message"],
            json!("Watched paths exceed 10000 directories")
        );
        assert_eq!(budget.code(), "invalid");

        let denied_error = ScanError::Io(
            io::Error::from(io::ErrorKind::PermissionDenied),
            "stat",
            "/p".into(),
        );
        assert_eq!(denied_error.code(), "permission_denied");

        let missing = ScanError::Io(
            io::Error::from(io::ErrorKind::NotFound),
            "scandir",
            "/p".into(),
        );
        assert_eq!(missing.code(), "invalid");
        let failure = missing.failure().to_json();
        assert_eq!(failure["code"], json!("UNKNOWN"));
        assert_eq!(failure["syscall"], json!("scandir"));
        assert_eq!(failure["path"], json!("/p"));
    }

    #[test]
    fn parse_targets_defaults_and_rejects() {
        let failure = parse_targets(&json!({})).err().unwrap().to_json();
        assert_eq!(failure["code"], json!("EINVAL"));
        assert_eq!(failure["message"], json!("watch needs targets"));
        let failure = parse_targets(&json!({ "targets": "x" }))
            .err()
            .unwrap()
            .to_json();
        assert_eq!(failure["message"], json!("watch needs targets"));
        // A target without a path is the same message, as the reference's `invalid` closure is reused.
        let failure = parse_targets(&json!({ "targets": [{}] }))
            .err()
            .unwrap()
            .to_json();
        assert_eq!(failure["message"], json!("watch needs targets"));
        let targets = parse_targets(&json!({
            "targets": [
                { "path": "/a" },
                { "path": "/b", "recursive": true, "exclude": { "hidden": true, "names": ["x", 7] } },
                { "path": "/c", "exclude": { "names": "not an array" } },
            ],
        }))
        .unwrap();
        assert_eq!(targets[0].path, "/a");
        assert!(!targets[0].recursive);
        assert!(!targets[0].hidden);
        assert!(targets[0].names.is_empty());
        assert!(targets[1].recursive);
        assert!(targets[1].hidden);
        // Non-string names are dropped silently.
        assert_eq!(targets[1].names.iter().collect::<Vec<_>>(), ["x"]);
        // A non-array `names` is an empty set, not an error.
        assert!(targets[2].names.is_empty());
    }

    #[test]
    fn entries_zero_the_timestamp_of_a_directory() {
        let dir = std::env::temp_dir();
        let metadata = fs::metadata(&dir).unwrap();
        let entry = entry_of(&dir.to_string_lossy(), &metadata, true, None);
        assert!(matches!(entry.kind, Kind::Directory));
        assert_eq!(entry.size, 0);
        assert_eq!(entry.mtime, (0, 0));
        let path = dir.join(format!("kiana-env-watch-entry-{}", std::process::id()));
        fs::write(&path, b"hello").unwrap();
        let metadata = fs::metadata(&path).unwrap();
        let entry = entry_of(&path.to_string_lossy(), &metadata, true, Some(7));
        assert!(matches!(entry.kind, Kind::File));
        assert_eq!(entry.size, 5);
        assert_ne!(entry.mtime, (0, 0));
        assert_eq!(entry.hash, Some(7));
        // Snapshot entries compare by value, so an unchanged file yields an unchanged entry.
        assert!(entry == entry.clone());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn a_watcher_with_only_pure_fields_can_be_built() {
        // `run` builds this from JSON; the pure helpers on it are what the loop relies on.
        let output = Output::default();
        let control = Control::default();
        let (sender, _events) = mpsc::channel();
        let watcher = FileWatcher {
            id: 3,
            output: &output,
            control: &control,
            targets: vec![Target {
                path: "/watched/sub".into(),
                recursive: false,
                hidden: true,
                names: ["skip".to_string()].into_iter().collect(),
            }],
            mode: Mode::Native,
            poll_interval: Duration::from_millis(2000),
            max_directories: DEFAULT_MAX_DIRECTORIES,
            snapshot: Snapshot::new(),
            native: None,
            events_sender: sender,
            installed: HashMap::new(),
            events: HashSet::new(),
            settle_at: None,
        };
        // A path inside a target reports itself; an ancestor reports the target below it.
        assert_eq!(watcher.reported("/watched/sub/a.txt"), "/watched/sub/a.txt");
        assert_eq!(watcher.reported("/watched/sub"), "/watched/sub");
        assert_eq!(watcher.reported("/"), "/watched/sub");
        assert_eq!(watcher.reported("/elsewhere"), "/elsewhere");
        // Events inside a target are in scope; excluded names and deeper paths of a non-recursive target are not.
        assert!(watcher.in_scope("/watched/sub/a.txt"));
        assert!(watcher.in_scope("/watched/sub"));
        assert!(
            !watcher.in_scope("/watched/sub/deep/a.txt"),
            "non-recursive target"
        );
        assert!(!watcher.in_scope("/watched/sub/skip"), "excluded name");
        assert!(
            !watcher.in_scope("/watched/sub/.hidden"),
            "hidden excluded for that target"
        );
        assert!(!watcher.in_scope("/elsewhere"));
        // An ancestor of a target is in scope too (its identity changes move the target).
        assert!(watcher.in_scope("/"));
        // The event path is reported in the spelling of the watched path.
        assert_eq!(
            watcher.watched_spelling("/watched/sub/a.txt"),
            "/watched/sub/a.txt"
        );
    }
}
