//! `core/footer-data-provider.ts`：`FooterDataProvider` 与 `findGitPaths`。
//!
//! 逐字移植 `packages/coding-agent/src/core/footer-data-provider.ts`（388 行）。
//! 实现 [`kiana_ext_api::ui::ReadonlyFooterDataProvider`]（接口上移的理由见 `ARCHITECTURE.md` §3.5）。
//!
//! 并发模型：
//! - `cached_branch` 用 `Option<Option<String>>` 表示 TS 的三态（`None` = 未解析）；
//! - 去抖 500ms、`refreshInFlight` / `refreshPending` 合并、watcher 错误后 5s 重试都用
//!   `tokio::spawn` + 代际计数复刻（`spec/ca-resources.md` §2.15、§8.4 第 13 条）；
//! - watcher 用 `notify`（`kiana_core::fs_watch`），WSL `/mnt/[a-z]` 走 1s 轮询（`watchFile` 等价物）。
//!
//! `getExtensionStatuses` 在 TS 返回实时引用；这里把状态表放在 `&self` 可直接借出的字段里，
//! 因此修改状态的 setter 也接收 `&self`，通过 `UnsafeCell` 写入；调用方必须保证读写都在
//! UI 线程（见 `unsafe impl Sync` 的说明）。
//!
//! # 这个文件解决什么问题
//! `kiana-interactive` 的 footer 要显示“当前 git 分支”，扩展还能通过 `ctx.ui.setStatus`
//! 往 footer 写状态文本。本文件提供这两类数据的唯一来源，并在仓库切换分支时主动通知 UI
//! 重绘。没有它，footer 只能显示启动时的快照，无法感知运行期的分支变化。
//!
//! # 调用方与被调用方
//! - `kiana-interactive` 在交互模式启动时构造 `Arc<FooterDataProvider>`，同时交给 footer
//!   组件与扩展 UI（作为 `Arc<dyn ReadonlyFooterDataProvider>`）；会话切目录时调用
//!   `set_cwd`，退出时调用 `dispose`。分支变化回调只用来请求一次重绘。
//! - `kiana-resources::loader::context_files` 只使用 [`find_git_paths`] 判断 worktree 布局。
//! - 文件监听走 `kiana_core::fs_watch`（`notify`），子进程走 `kiana_core::child_process`。
//!
//! # 一次分支更新的执行流程
//! 1. `new` / `set_cwd` 调用 [`find_git_paths`] 定位 `.git` 元数据，并把缓存标记为“未解析”；
//! 2. `get_git_branch` 只在未解析时同步读取 `HEAD`（reftable 场景会启动 `git` 子进程），
//!    无论成功失败都把结果缓存下来；
//! 3. watcher 收到事件后不立刻解析，而是经 `WATCH_DEBOUNCE_MS`（500ms）去抖；
//! 4. 异步解析完成后写回缓存，只有值真的变化才调用订阅者（避免多余重绘）。
//!
//! # 注意事项
//! - 所有 git 相关的 IO 错误（文件不存在、读失败、`git` 退出码非 0）都被折叠成
//!   `None`/`"detached"`，本模块没有错误类型；调用方无法区分失败原因。
//! - 后台任务通过代际计数（`refresh_generation` / `retry_generation`）识别“已被取消”的
//!   定时器；`dispose` 只标记 `disposed` 并 abort 轮询任务，不 join 已经 spawn 的去抖/
//!   刷新任务，它们醒来后自行返回。
//! - 状态表与 `UnsafeCell` 的线程约束见 `FooterDataProvider::statuses` 字段文档。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use indexmap::IndexMap;
use kiana_core::child_process::{
    SpawnOptions, SpawnSyncOptions, StdioMode, spawn_process, spawn_process_sync,
};
use kiana_core::event_bus::Unsubscribe;
use kiana_core::fs_watch::{FS_WATCH_RETRY_DELAY_MS, close_watcher, watch_with_error_handler};
use notify::RecommendedWatcher;

/// git 元数据路径。TS: `footer-data-provider.ts#GitPaths`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPaths {
    /// 仓库工作目录。
    pub repo_dir: PathBuf,
    /// 公共 git 目录（worktree 时指向主仓库的 `.git`）。
    pub common_git_dir: PathBuf,
    /// `HEAD` 文件。
    pub head_path: PathBuf,
}

/// 从 `cwd` 向上查找 git 元数据（支持 `.git` 目录与 worktree 的 `.git` 文件）。
///
/// TS: `footer-data-provider.ts#findGitPaths`。
///
/// 返回的 `repo_dir` 是**找到 `.git` 的那个目录**，不一定是 `cwd`；`head_path` 一定存在，
/// 否则返回 `None`。以下是容易踩坑的边界：
/// - `.git` 是目录：普通仓库，`common_git_dir == .git`；
/// - `.git` 是文件：worktree/submodule 的 gitdir 指针，内容必须形如 `gitdir: <path>`，
///   相对路径按 `.git` 所在目录解析；不是该格式时**跳过并继续向上找**；
/// - 找到 `.git` 但里面没有 `HEAD`：视为无效仓库，立即返回 `None`，不再继续向上；
/// - 读 `.git`/`commondir` 失败：同样返回 `None`，不向调用方暴露具体错误。
pub fn find_git_paths(cwd: &Path) -> Option<GitPaths> {
    let mut dir = cwd.to_path_buf();
    loop {
        let git_path = dir.join(".git");
        if git_path.exists() {
            // `metadata` 跟随符号链接：`.git` 指向目录时按目录处理。
            match std::fs::metadata(&git_path) {
                Ok(stat) => {
                    if stat.is_file() {
                        // worktree 的 `.git` 是一个文本指针文件。
                        match std::fs::read_to_string(&git_path) {
                            Ok(content) => {
                                let content = kiana_json::js_string::js_trim(&content);
                                if let Some(rest) = content.strip_prefix("gitdir: ") {
                                    let git_dir = resolve_path_from(
                                        &dir,
                                        kiana_json::js_string::js_trim(rest),
                                    );
                                    // 没有 HEAD 说明这不是可用的 git 目录；与“继续向上找”的
                                    // 语义不同，这里直接失败。
                                    let head_path = git_dir.join("HEAD");
                                    if !head_path.exists() {
                                        return None;
                                    }
                                    // `commondir` 存在时（linked worktree）指向主仓库的 `.git`；
                                    // 否则当前 git_dir 就是公共目录。内容按 git_dir 解析。
                                    let common_dir_path = git_dir.join("commondir");
                                    let common_git_dir = if common_dir_path.exists() {
                                        match std::fs::read_to_string(&common_dir_path) {
                                            Ok(common) => resolve_path_from(
                                                &git_dir,
                                                kiana_json::js_string::js_trim(&common),
                                            ),
                                            Err(_) => return None,
                                        }
                                    } else {
                                        git_dir
                                    };
                                    return Some(GitPaths {
                                        repo_dir: dir,
                                        common_git_dir,
                                        head_path,
                                    });
                                }
                            }
                            Err(_) => return None,
                        }
                    } else if stat.is_dir() {
                        // 普通仓库：`.git` 目录本身既含 HEAD 也是公共目录。
                        let head_path = git_path.join("HEAD");
                        if !head_path.exists() {
                            return None;
                        }
                        return Some(GitPaths {
                            repo_dir: dir,
                            common_git_dir: git_path,
                            head_path,
                        });
                    }
                }
                Err(_) => return None,
            }
        }
        // 到文件系统根目录后 `parent()` 返回 `None`：没有更上层的仓库了。
        let parent = dir.parent()?;
        if parent == dir {
            return None;
        }
        dir = parent.to_path_buf();
    }
}

/// `resolve(base, relative)`（node:path 词法）。
///
/// 只做字符串层面的路径归一化，不访问文件系统；因此调用方必须自己检查结果是否存在。
fn resolve_path_from(base: &Path, relative: &str) -> PathBuf {
    // `node:path.resolve` 不展开 `~`（`resolvePath` 才会）。
    PathBuf::from(kiana_core::paths::resolve_path(
        relative,
        &base.to_string_lossy(),
        &kiana_core::paths::PathInputOptions {
            expand_tilde: false,
            ..Default::default()
        },
    ))
}

/// `git --no-optional-locks symbolic-ref --quiet --short HEAD` 的参数。
const GIT_BRANCH_ARGS: [&str; 5] = [
    "--no-optional-locks",
    "symbolic-ref",
    "--quiet",
    "--short",
    "HEAD",
];

fn git_args() -> Vec<String> {
    GIT_BRANCH_ARGS
        .iter()
        .map(|arg| (*arg).to_string())
        .collect()
}

/// 同步询问 git 当前分支。TS: `footer-data-provider.ts#resolveBranchWithGitSync`。
///
/// 关闭 stdin、丢弃 stderr，避免 git 等待输入或噪声污染输出；退出码非 0 或输出（去空白后）
/// 为空都当作“解析不到”，返回 `None`。
fn resolve_branch_with_git_sync(repo_dir: &Path) -> Option<String> {
    let options = SpawnSyncOptions {
        cwd: Some(repo_dir.to_path_buf()),
        stdin: StdioMode::Ignore,
        stdout: StdioMode::Pipe,
        stderr: StdioMode::Ignore,
        ..Default::default()
    };
    let result = spawn_process_sync("git", &git_args(), &options);
    if result.status != Some(0) {
        return None;
    }
    let branch = kiana_json::js_string::js_trim(&result.stdout).to_string();
    (!branch.is_empty()).then_some(branch)
}

/// 异步询问 git 当前分支。TS: `footer-data-provider.ts#resolveBranchWithGitAsync`。
///
/// 参数、stdio 与失败判定与同步版一致；区别只是等待子进程在 tokio 任务里完成，不阻塞
/// 调用线程。
async fn resolve_branch_with_git_async(repo_dir: &Path) -> Option<String> {
    let options = SpawnOptions {
        cwd: Some(repo_dir.to_path_buf()),
        stdin: StdioMode::Ignore,
        stdout: StdioMode::Pipe,
        stderr: StdioMode::Ignore,
        ..Default::default()
    };
    let child = spawn_process("git", &git_args(), &options).ok()?;
    let output = child.wait_with_output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    let branch =
        kiana_json::js_string::js_trim(&String::from_utf8_lossy(&output.stdout)).to_string();
    (!branch.is_empty()).then_some(branch)
}

/// `platform === "linux" && (WSL_DISTRO_NAME || WSL_INTEROP)`。
///
/// `cfg!` 是编译期判定：非 Linux 构建恒为 `false`。两个环境变量都要求非空。
fn is_wsl_environment() -> bool {
    cfg!(target_os = "linux")
        && (std::env::var("WSL_DISTRO_NAME").is_ok_and(|value| !value.is_empty())
            || std::env::var("WSL_INTEROP").is_ok_and(|value| !value.is_empty()))
}

/// `/^\/mnt\/[a-z](?:\/|$)/i`。
///
/// 只匹配 `/mnt/` 加单个 ASCII 字母，且其后必须是 `/` 或字符串结尾。
fn is_windows_mounted_repo_path(repo_dir: &Path) -> bool {
    let path = repo_dir.to_string_lossy();
    let bytes = path.as_bytes();
    if bytes.len() < 6 || !bytes[..5].eq_ignore_ascii_case(b"/mnt/") {
        return false;
    }
    if !bytes[5].is_ascii_alphabetic() {
        return false;
    }
    bytes.len() == 6 || bytes[6] == b'/'
}

/// `shouldPollGitHead(repoDir)`。
fn should_poll_git_head(repo_dir: &Path) -> bool {
    is_wsl_environment() && is_windows_mounted_repo_path(repo_dir)
}

/// `watchFile` 的轮询间隔（`HEAD`）。TS: `{ interval: 1000 }`。
const HEAD_POLL_INTERVAL_MS: u64 = 1000;
/// `watchFile` 对 `tables.list` 的轮询间隔。TS: `{ interval: 250 }`。
const TABLES_LIST_POLL_INTERVAL_MS: u64 = 250;
/// 静态去抖时长。TS: `FooterDataProvider.WATCH_DEBOUNCE_MS`。
pub const WATCH_DEBOUNCE_MS: u64 = 500;

/// 文件状态快照（对应 Node `Stats` 的 `mtimeMs` / `ctimeMs` / `size`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileSnapshot {
    mtime_secs: i64,
    mtime_nanos: u32,
    ctime_secs: i64,
    ctime_nanos: u32,
    size: u64,
}

/// 读取一次元数据快照；文件不存在、无权限等 IO 错误返回 `None`。
///
/// Unix 记录 mtime/ctime 的“秒 + 纳秒”字段，比较时逐字段相等；非 Unix 没有 ctime，
/// ctime 字段固定为 0。
fn file_snapshot(path: &Path) -> Option<FileSnapshot> {
    let metadata = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(FileSnapshot {
            mtime_secs: metadata.mtime(),
            mtime_nanos: metadata.mtime_nsec() as u32,
            ctime_secs: metadata.ctime(),
            ctime_nanos: metadata.ctime_nsec() as u32,
            size: metadata.len(),
        })
    }
    #[cfg(not(unix))]
    {
        // Windows 没有 ctime（spec §8.6 第 4 条）：退化为 mtime + size。
        let mtime = metadata.modified().ok()?;
        let since_epoch = mtime.duration_since(std::time::UNIX_EPOCH).ok()?;
        Some(FileSnapshot {
            mtime_secs: since_epoch.as_secs() as i64,
            mtime_nanos: since_epoch.subsec_nanos(),
            ctime_secs: 0,
            ctime_nanos: 0,
            size: metadata.len(),
        })
    }
}

type BranchCallback = Arc<dyn Fn() + Send + Sync>;

/// 共享的可变状态（定时器、watcher、回调都在这里，便于 watcher 线程访问）。
#[derive(Default)]
struct Shared {
    cwd: String,
    git_paths: Option<GitPaths>,
    /// `None` = 未解析（TS 的 `undefined`）；`Some(None)` = null。
    cached_branch: Option<Option<String>>,
    /// 订阅者列表；回调只在锁外调用（先拷贝快照），避免回调重入 `Shared` 造成死锁。
    callbacks: Vec<(u64, BranchCallback)>,
    next_callback_id: u64,
    disposed: bool,
    /// 去抖定时器是否已安排；为 `true` 时新的 watcher 事件直接并入这一次。
    refresh_timer_active: bool,
    /// 每次安排定时器递增；定时器醒来后若代际不匹配，说明已被 `set_cwd`/`dispose` 取消。
    refresh_generation: u64,
    /// 是否已有异步分支解析在跑；此时新事件只置 `refresh_pending`。
    refresh_in_flight: bool,
    /// 在途刷新期间又来了事件，结束后需要再刷一次（合并成一次）。
    refresh_pending: bool,
    /// watcher 失败后的 5s 重试是否已安排；同样用代际识别过期定时器。
    retry_scheduled: bool,
    retry_generation: u64,
    // watcher 必须持续持有：`RecommendedWatcher` 一旦 drop，监听立即停止（见 fs_watch 文档）。
    head_watcher: Option<RecommendedWatcher>,
    reftable_watcher: Option<RecommendedWatcher>,
    tables_list_watcher: Option<RecommendedWatcher>,
    /// WSL 下 `/mnt/*` 的 HEAD 轮询任务；取消时 `abort`。
    head_poll: Option<tokio::task::JoinHandle<()>>,
    /// reftable 仓库 `tables.list` 的轮询任务。
    tables_list_poll: Option<tokio::task::JoinHandle<()>>,
}

/// 取共享状态锁；锁中毒时恢复内层数据（与仓库其它模块一致），避免一次 panic 让 footer
/// 永久不可用。
fn lock(shared: &Arc<Mutex<Shared>>) -> std::sync::MutexGuard<'_, Shared> {
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 无 tokio 运行时时使用的后台运行时（只为 watcher/去抖定时器服务）。
///
/// 单 worker、`enable_all`，由 `OnceLock` 持有到进程结束；不会随 provider 的 `dispose`
/// 关闭。
fn fallback_handle() -> tokio::runtime::Handle {
    static FALLBACK: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    let runtime = FALLBACK.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("footer fallback runtime must build")
    });
    runtime.handle().clone()
}

/// git 分支 / 扩展状态 / 可用 provider 数的数据提供者。
///
/// TS: `footer-data-provider.ts#FooterDataProvider`。
pub struct FooterDataProvider {
    /// 扩展状态（TS 的可变 `Map`）。TS 的 UI 是单线程的，状态只在 UI 线程读写；
    /// `ReadonlyFooterDataProvider` 需要 `Sync` 以便交给扩展工厂，因此用 `UnsafeCell`
    /// 提供与 TS 相同的“共享引用可变”语义（见下方 `unsafe impl Sync` 的说明）。
    statuses: std::cell::UnsafeCell<IndexMap<String, String>>,
    /// 有可用模型的 provider 数；footer 渲染线程与扩展查询都可能读，所以用原子。
    available_provider_count: AtomicUsize,
    shared: Arc<Mutex<Shared>>,
    handle: tokio::runtime::Handle,
}

// SAFETY: `statuses` 是 `UnsafeCell`，但只在 UI 线程读写（TS 的单线程 UI 语义）。
// 其余字段本身满足 `Send + Sync`（`Shared` 用 `Mutex`，计数是原子）。
unsafe impl Sync for FooterDataProvider {}

impl FooterDataProvider {
    /// 构造并启动 git watcher。TS: `new FooterDataProvider(cwd)`。
    ///
    /// 立即定位 git 元数据并尝试启动 watcher；`cwd` 不在 git 仓库中时 `git_paths` 为
    /// `None`，不会启动任何监听。当前线程没有 tokio runtime 时会回退到进程级的后台
    /// runtime（见 `fallback_handle`）。
    pub fn new(cwd: impl Into<String>) -> Self {
        let handle = tokio::runtime::Handle::try_current().unwrap_or_else(|_| fallback_handle());
        let cwd = cwd.into();
        let provider = FooterDataProvider {
            statuses: std::cell::UnsafeCell::new(IndexMap::new()),
            available_provider_count: AtomicUsize::new(0),
            shared: Arc::new(Mutex::new(Shared::default())),
            handle,
        };
        {
            let mut shared = lock(&provider.shared);
            shared.cwd = cwd.clone();
            shared.git_paths = find_git_paths(Path::new(&cwd));
        }
        provider.setup_git_watcher();
        provider
    }

    /// 当前 git 分支：不在仓库中为 `None`，detached HEAD 为 `"detached"`。
    ///
    /// TS: `FooterDataProvider#getGitBranch`（带同步缓存）。
    ///
    /// 首次调用会同步读 `HEAD`（reftable 场景还会同步启动 `git`），之后直接返回缓存；
    /// watcher 触发的异步刷新会更新这份缓存。读取失败与“不是仓库”都表现为 `None`。
    pub fn get_git_branch(&self) -> Option<String> {
        let mut shared = lock(&self.shared);
        if shared.cached_branch.is_none() {
            let resolved = resolve_git_branch_sync_at(&shared.git_paths);
            shared.cached_branch = Some(resolved);
        }
        shared.cached_branch.clone().flatten()
    }

    /// 扩展状态文本（实时引用，插入序）。TS: `getExtensionStatuses`。
    pub fn get_extension_statuses(&self) -> &IndexMap<String, String> {
        // SAFETY: 状态只由 UI 线程写入；读取发生在同一线程的渲染/工厂调用里。
        unsafe { &*self.statuses.get() }
    }

    /// 订阅 git 分支变化；返回退订闭包。TS: `onBranchChange`。
    ///
    /// 回调在触发它的线程上同步执行：可能是后台刷新任务，也可能是调用 `set_cwd` 的线程。
    /// 回调若长时间阻塞会拖住该线程。退订按注册 id 从回调表移除，重复退订是无操作。
    pub fn on_branch_change(&self, callback: BranchCallback) -> Unsubscribe {
        let id = {
            let mut shared = lock(&self.shared);
            let id = shared.next_callback_id;
            shared.next_callback_id += 1;
            shared.callbacks.push((id, callback));
            id
        };
        let shared = Arc::clone(&self.shared);
        Box::new(move || {
            let mut shared = lock(&shared);
            shared
                .callbacks
                .retain(|(callback_id, _)| *callback_id != id);
        })
    }

    /// 设置扩展状态（`None` 删除）。TS: `setExtensionStatus`。
    ///
    /// 签名用 `&self`（TS 的字段可变）：宿主与 footer 共享同一个 `Arc<FooterDataProvider>`，
    /// 无法取得 `&mut`。见字段注释的线程约束。
    ///
    /// `Some("")` 会保留一个空字符串状态，并不等于删除；删除必须传 `None`。
    pub fn set_extension_status(&self, key: &str, text: Option<&str>) {
        // SAFETY: 见字段注释；只由 UI 线程调用。
        let statuses = unsafe { &mut *self.statuses.get() };
        match text {
            Some(text) => {
                statuses.insert(key.to_string(), text.to_string());
            }
            None => {
                statuses.shift_remove(key);
            }
        }
    }

    /// 清空扩展状态。TS: `clearExtensionStatuses`。
    pub fn clear_extension_statuses(&self) {
        // SAFETY: 见字段注释；只由 UI 线程调用。
        unsafe { &mut *self.statuses.get() }.clear();
    }

    /// 可用 provider 数。TS: `getAvailableProviderCount`。
    pub fn get_available_provider_count(&self) -> usize {
        self.available_provider_count.load(Ordering::SeqCst)
    }

    /// 更新可用 provider 数。TS: `setAvailableProviderCount`。
    pub fn set_available_provider_count(&self, count: usize) {
        self.available_provider_count.store(count, Ordering::SeqCst);
    }

    /// 切换工作目录并重建 watcher。TS: `setCwd`。
    ///
    /// 目录字符串相同时直接返回。否则先取消进行中的去抖（递增代际），在锁外关闭旧
    /// watcher，再重新定位 git 元数据并启动 watcher，最后**无条件**通知一次分支订阅者
    /// （不比较新旧分支值）。
    pub fn set_cwd(&self, cwd: &str) {
        {
            let mut shared = lock(&self.shared);
            if shared.cwd == cwd {
                return;
            }
            shared.cwd = cwd.to_string();
            if shared.refresh_timer_active {
                shared.refresh_generation += 1;
                shared.refresh_timer_active = false;
            }
        }
        // `clearGitWatchers` 自己取锁；必须在锁外调用。
        clear_git_watchers(&self.shared);
        {
            let mut shared = lock(&self.shared);
            shared.cached_branch = None;
            shared.git_paths = find_git_paths(Path::new(cwd));
        }
        self.setup_git_watcher();
        notify_branch_change(&self.shared);
    }

    /// 清理资源。TS: `dispose`。
    ///
    /// 标记 `disposed`、清空订阅者、关闭 watcher 并 abort 轮询任务。可重复调用；不会等待
    /// 已 spawn 的去抖/刷新任务结束，它们会在检查 `disposed` 后自行返回。
    pub fn dispose(&self) {
        {
            let mut shared = lock(&self.shared);
            shared.disposed = true;
            if shared.refresh_timer_active {
                shared.refresh_generation += 1;
                shared.refresh_timer_active = false;
            }
            shared.callbacks.clear();
        }
        clear_git_watchers(&self.shared);
    }

    /// 测试钩子：模拟一次异步 `fs.watch` 错误（触发清空 + 5s 重试）。
    ///
    /// 行为与生产路径的 watcher 错误回调完全一致，仅用于测试。
    #[doc(hidden)]
    pub fn simulate_git_watcher_error(&self) {
        handle_git_watcher_error(&self.shared, &self.handle);
    }

    /// 测试钩子：当前是否持有 HEAD 目录的 watcher。
    #[doc(hidden)]
    pub fn has_head_watcher(&self) -> bool {
        lock(&self.shared).head_watcher.is_some()
    }

    /// 测试钩子：手动触发一次去抖刷新（等价于 watcher 收到事件）。
    #[doc(hidden)]
    pub fn trigger_refresh(&self) {
        schedule_refresh(&self.shared, &self.handle);
    }

    /// 测试钩子：当前是否安排了 watcher 重试。
    #[doc(hidden)]
    pub fn has_git_watcher_retry(&self) -> bool {
        lock(&self.shared).retry_scheduled
    }

    fn setup_git_watcher(&self) {
        setup_git_watcher(&self.shared, &self.handle);
    }
}

impl kiana_ext_api::ui::ReadonlyFooterDataProvider for FooterDataProvider {
    fn get_git_branch(&self) -> Option<String> {
        FooterDataProvider::get_git_branch(self)
    }

    fn get_extension_statuses(&self) -> &IndexMap<String, String> {
        self.get_extension_statuses()
    }

    fn get_available_provider_count(&self) -> usize {
        self.get_available_provider_count()
    }

    fn on_branch_change(&self, callback: BranchCallback) -> Unsubscribe {
        FooterDataProvider::on_branch_change(self, callback)
    }
}

// ---------------------------------------------------------------------------
// 分支解析
// ---------------------------------------------------------------------------

/// `resolveGitBranchSync()`（读取缓存之外的路径）。
///
/// 规则：
/// - 没有 git 元数据、`HEAD` 读不到 → `None`；
/// - `HEAD` 内容形如 `ref: refs/heads/<branch>` → 直接取 `<branch>`；
/// - `<branch>` 是 `.invalid`（reftable 仓库的占位）→ 同步问 `git`，失败则回退 `"detached"`；
/// - 其它内容（裸 commit id 等）→ `"detached"`。
///
/// 注意“读不到 HEAD”与“HEAD 不是引用”结果不同：前者是 `None`（不是仓库），后者是
/// `"detached"`。
fn resolve_git_branch_sync_at(git_paths: &Option<GitPaths>) -> Option<String> {
    let paths = git_paths.as_ref()?;
    let content = std::fs::read_to_string(&paths.head_path).ok()?;
    let content = kiana_json::js_string::js_trim(&content);
    match content.strip_prefix("ref: refs/heads/") {
        Some(branch) => {
            if branch == ".invalid" {
                Some(
                    resolve_branch_with_git_sync(&paths.repo_dir)
                        .unwrap_or_else(|| "detached".to_string()),
                )
            } else {
                Some(branch.to_string())
            }
        }
        None => Some("detached".to_string()),
    }
}

/// `resolveGitBranchAsync()`：判定规则与同步版相同，只把 `git` 调用换成异步等待。
async fn resolve_git_branch_async_at(git_paths: &Option<GitPaths>) -> Option<String> {
    let paths = git_paths.as_ref()?;
    let content = std::fs::read_to_string(&paths.head_path).ok()?;
    let content = kiana_json::js_string::js_trim(&content);
    match content.strip_prefix("ref: refs/heads/") {
        Some(branch) => {
            if branch == ".invalid" {
                Some(
                    resolve_branch_with_git_async(&paths.repo_dir)
                        .await
                        .unwrap_or_else(|| "detached".to_string()),
                )
            } else {
                Some(branch.to_string())
            }
        }
        None => Some("detached".to_string()),
    }
}

// ---------------------------------------------------------------------------
// 去抖与刷新
// ---------------------------------------------------------------------------

/// 通知所有订阅者。
///
/// 先在锁内拷贝回调快照，再在锁外逐个调用：不在持锁期间执行外部代码，回调内若再取同一把
/// 锁也不会死锁。
fn notify_branch_change(shared: &Arc<Mutex<Shared>>) {
    let callbacks: Vec<BranchCallback> = {
        let shared = lock(shared);
        shared
            .callbacks
            .iter()
            .map(|(_, callback)| Arc::clone(callback))
            .collect()
    };
    for callback in callbacks {
        callback();
    }
}

/// TS: `scheduleRefresh`。
///
/// 去抖与合并：
/// - 已有定时器在等 → 直接返回（事件被这一次吸收）；
/// - 已有异步刷新在跑 → 只置 `refresh_pending`，由 `finish_refresh` 在结束后补一次；
/// - 否则标记定时器、递增代际，spawn 一个睡眠 `WATCH_DEBOUNCE_MS` 的任务。醒来后若代际
///   已变（`set_cwd`/`dispose` 取消，或又安排了新的定时器）就放弃本次刷新。
fn schedule_refresh(shared: &Arc<Mutex<Shared>>, handle: &tokio::runtime::Handle) {
    let generation = {
        let mut state = lock(shared);
        if state.disposed || state.refresh_timer_active {
            return;
        }
        if state.refresh_in_flight {
            state.refresh_pending = true;
            return;
        }
        state.refresh_timer_active = true;
        state.refresh_generation += 1;
        state.refresh_generation
    };

    let shared = Arc::clone(shared);
    let handle_clone = handle.clone();
    handle.spawn(async move {
        tokio::time::sleep(Duration::from_millis(WATCH_DEBOUNCE_MS)).await;
        {
            let mut state = lock(&shared);
            if state.disposed || state.refresh_generation != generation {
                return;
            }
            state.refresh_timer_active = false;
        }
        refresh_git_branch_async(shared, handle_clone).await;
    });
}

/// TS: `refreshGitBranchAsync`。
///
/// 阶段：
/// 1. 在锁内检查 `disposed` 与 in-flight；已有人在跑就置 pending 后返回；
/// 2. 拷贝 `git_paths` 后在锁外做 IO（读 HEAD / 启动 git），不持锁等待；
/// 3. 再次检查 `disposed`（刷新期间可能被关闭），然后比较旧缓存决定是否通知；
/// 4. 无论是否变化都调用 `finish_refresh` 复位 in-flight，并处理合并的 pending 刷新。
async fn refresh_git_branch_async(shared: Arc<Mutex<Shared>>, handle: tokio::runtime::Handle) {
    let git_paths = {
        let mut state = lock(&shared);
        if state.disposed {
            return;
        }
        if state.refresh_in_flight {
            state.refresh_pending = true;
            return;
        }
        state.refresh_in_flight = true;
        state.git_paths.clone()
    };

    let next_branch = resolve_git_branch_async_at(&git_paths).await;

    let disposed = lock(&shared).disposed;
    if disposed {
        finish_refresh(&shared, &handle);
        return;
    }
    let notify = {
        let mut state = lock(&shared);
        let changed = match &state.cached_branch {
            Some(previous) => previous != &next_branch,
            None => false,
        };
        state.cached_branch = Some(next_branch);
        changed
    };
    if notify {
        notify_branch_change(&shared);
    }
    finish_refresh(&shared, &handle);
}

/// `finally`：复位 in-flight，并合并 pending 刷新。
///
/// 只有 `disposed` 为 false 且确有 pending 时才再安排一次，避免关闭后继续起任务。
fn finish_refresh(shared: &Arc<Mutex<Shared>>, handle: &tokio::runtime::Handle) {
    let should_schedule = {
        let mut state = lock(shared);
        state.refresh_in_flight = false;
        if state.refresh_pending && !state.disposed {
            state.refresh_pending = false;
            true
        } else {
            false
        }
    };
    if should_schedule {
        schedule_refresh(shared, handle);
    }
}

// ---------------------------------------------------------------------------
// watcher 生命周期
// ---------------------------------------------------------------------------

/// TS: `clearGitWatchers`（关闭所有 watcher、取消轮询与重试定时器）。
///
/// 先在锁内把 watcher/任务句柄 `take` 出来，再在锁外关闭、abort，避免 drop watcher 的
/// 开销或回调持锁。若已有重试定时器，递增代际使其失效。
fn clear_git_watchers(shared: &Arc<Mutex<Shared>>) {
    let (head, reftable, tables, head_poll, tables_poll) = {
        let mut state = lock(shared);
        (
            state.head_watcher.take(),
            state.reftable_watcher.take(),
            state.tables_list_watcher.take(),
            state.head_poll.take(),
            state.tables_list_poll.take(),
        )
    };
    close_watcher(head);
    close_watcher(reftable);
    close_watcher(tables);
    if let Some(poll) = head_poll {
        poll.abort();
    }
    if let Some(poll) = tables_poll {
        poll.abort();
    }
    let mut state = lock(shared);
    if state.retry_scheduled {
        state.retry_scheduled = false;
        state.retry_generation += 1;
    }
}

/// TS: `scheduleGitWatcherRetry`。
///
/// 已安排或已 dispose 时直接返回；否则标记并 spawn 一个 `FS_WATCH_RETRY_DELAY_MS`（5s）
/// 的睡眠任务。醒来后用代际确认自己仍是最新的一次，再重新 `setup_git_watcher`。重试再次
/// 失败会重新排下一次，因此会每 5s 持续尝试，直到成功或被 `dispose` 阻止。
fn schedule_git_watcher_retry(shared: &Arc<Mutex<Shared>>, handle: &tokio::runtime::Handle) {
    let generation = {
        let mut state = lock(shared);
        if state.disposed || state.retry_scheduled {
            return;
        }
        state.retry_scheduled = true;
        state.retry_generation += 1;
        state.retry_generation
    };
    let shared = Arc::clone(shared);
    let handle_clone = handle.clone();
    handle.spawn(async move {
        tokio::time::sleep(Duration::from_millis(FS_WATCH_RETRY_DELAY_MS)).await;
        {
            let mut state = lock(&shared);
            if state.disposed || !state.retry_scheduled || state.retry_generation != generation {
                return;
            }
            state.retry_scheduled = false;
        }
        setup_git_watcher(&shared, &handle_clone);
    });
}

/// TS: `handleGitWatcherError`：先拆掉现有监听（避免重复/失效的 watcher 残留），再排一次
/// 5s 重试。
fn handle_git_watcher_error(shared: &Arc<Mutex<Shared>>, handle: &tokio::runtime::Handle) {
    clear_git_watchers(shared);
    schedule_git_watcher_retry(shared, handle);
}

/// 启动一个 `watchFile` 等价的轮询任务：状态快照变化时 `schedule_refresh`。
///
/// 第一次 `tick` 会立即返回，用当前元数据初始化 `previous`。只有“前后两次都成功读到”
/// 且快照不同才触发刷新；文件消失（`None`）或从缺失恢复都不会单独触发，只更新基准。
/// `MissedTickBehavior::Delay` 保证任务被阻塞后不会补发一串 tick。
fn spawn_poll_task(
    path: PathBuf,
    interval_ms: u64,
    shared: &Arc<Mutex<Shared>>,
    handle: &tokio::runtime::Handle,
) -> tokio::task::JoinHandle<()> {
    let shared = Arc::clone(shared);
    let handle_clone = handle.clone();
    handle.spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(interval_ms));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut previous: Option<FileSnapshot> = None;
        loop {
            interval.tick().await;
            {
                let state = lock(&shared);
                if state.disposed {
                    return;
                }
            }
            let current = file_snapshot(&path);
            if let (Some(current), Some(previous)) = (current, previous)
                && current != previous
            {
                schedule_refresh(&shared, &handle_clone);
            }
            previous = current;
        }
    })
}

/// TS: `setupGitWatcher`。
///
/// 顺序：
/// 1. 先清掉旧监听（重复调用是安全的）；
/// 2. 没有 git 元数据或已 `dispose` → 什么都不装；
/// 3. 监听 `HEAD` 所在**目录**（不是文件，见下方注释）；WSL 的 `/mnt/*` 额外起 1s 轮询；
/// 4. 若 HEAD 监听和轮询都没成功，直接返回，不再装 reftable 监听；
/// 5. reftable 仓库再监听 `reftable` 目录与 `tables.list`（后者再加 250ms 轮询）。
///
/// 任何一次 `watch_with_error_handler` 同步失败都会回调 `handle_git_watcher_error`：
/// 清掉本次已装好的监听并安排 5s 后重试。
fn setup_git_watcher(shared: &Arc<Mutex<Shared>>, handle: &tokio::runtime::Handle) {
    clear_git_watchers(shared);

    let git_paths = {
        let state = lock(shared);
        if state.disposed {
            return;
        }
        state.git_paths.clone()
    };
    let Some(git_paths) = git_paths else {
        return;
    };

    // WSL 的 `/mnt/<盘符>` 仓库改用轮询（TS 的 `watchFile` 兜底路径）。
    let poll_git_head = should_poll_git_head(&git_paths.repo_dir);

    // 监视 HEAD 所在目录（不是文件）：git 用临时文件 + rename 覆盖，inode 变化后对文件的
    // `fs.watch` 会失效。
    let head_dir = git_paths
        .head_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| git_paths.common_git_dir.clone());
    let listener_shared = Arc::clone(shared);
    let listener_handle = handle.clone();
    let error_shared = Arc::clone(shared);
    let error_handle = handle.clone();
    let head_watcher = watch_with_error_handler(
        &head_dir,
        move |_event, filename| {
            if filename.is_none() || filename.as_deref() == Some("HEAD") {
                schedule_refresh(&listener_shared, &listener_handle);
            }
        },
        move || handle_git_watcher_error(&error_shared, &error_handle),
    );
    // 只关心目录内名为 `HEAD` 的条目；`filename == None` 表示平台不提供名字，保守刷新。
    let has_head_watcher = head_watcher.is_some();
    if let Some(watcher) = head_watcher {
        lock(shared).head_watcher = Some(watcher);
    }

    if poll_git_head {
        let poll = spawn_poll_task(
            git_paths.head_path.clone(),
            HEAD_POLL_INTERVAL_MS,
            shared,
            handle,
        );
        lock(shared).head_poll = Some(poll);
    }

    // 连 HEAD 都监控不到（且不是 WSL 轮询）时，装 reftable 监听也没有意义。
    if !has_head_watcher && !poll_git_head {
        return;
    }

    // reftable 仓库切分支时更新 reftable 目录，而不是 HEAD。
    let reftable_dir = git_paths.common_git_dir.join("reftable");
    if reftable_dir.exists() {
        let listener_shared = Arc::clone(shared);
        let listener_handle = handle.clone();
        let error_shared = Arc::clone(shared);
        let error_handle = handle.clone();
        let reftable_watcher = watch_with_error_handler(
            &reftable_dir,
            move |_event, _filename| {
                schedule_refresh(&listener_shared, &listener_handle);
            },
            move || handle_git_watcher_error(&error_shared, &error_handle),
        );
        // 创建失败时 `watch_with_error_handler` 已调用错误回调（清空 + 排重试），这里只需退出。
        let Some(reftable_watcher) = reftable_watcher else {
            return;
        };
        lock(shared).reftable_watcher = Some(reftable_watcher);

        let tables_list_path = reftable_dir.join("tables.list");
        if tables_list_path.exists() {
            let listener_shared = Arc::clone(shared);
            let listener_handle = handle.clone();
            let error_shared = Arc::clone(shared);
            let error_handle = handle.clone();
            let tables_list_watcher = watch_with_error_handler(
                &tables_list_path,
                move |_event, _filename| {
                    schedule_refresh(&listener_shared, &listener_handle);
                },
                move || handle_git_watcher_error(&error_shared, &error_handle),
            );
            let Some(tables_list_watcher) = tables_list_watcher else {
                return;
            };
            lock(shared).tables_list_watcher = Some(tables_list_watcher);
            // 目录监听与 `tables.list` 轮询同时启用；任一来源触发都会走同一个去抖刷新。
            let poll = spawn_poll_task(
                tables_list_path,
                TABLES_LIST_POLL_INTERVAL_MS,
                shared,
                handle,
            );
            lock(shared).tables_list_poll = Some(poll);
        }
    }
}
