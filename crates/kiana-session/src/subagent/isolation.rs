//! 子代理工作区隔离（对应 Claude Code 的 `isolation: worktree` 与 CowClone 设计）。
//!
//! 三种模式：
//!
//! | 模式 | 实现 | 隔离强度 | 典型用途 |
//! |---|---|---|---|
//! | `Shared` | 直接使用原目录 | 无 | 只读调研、无副作用任务 |
//! | `Worktree` | `git worktree add --detach` | 文件系统 + git 索引 | 并行改代码 |
//! | `CowClone` | btrfs/XFS reflink，或 overlayfs | 文件系统 | 非 git 项目 / 需要完整目录克隆 |
//!
//! 关键约束（DoD）：
//!
//! - baseline 必须捕获 HEAD、staged、unstaged、untracked，以及 nested repo；
//! - delta patch 对 git 树用 `git diff --binary --full-index`，保证 `git apply`
//!   能字节一致地重放；untracked 文件用 `git diff --no-index --binary` 生成同格式补丁；
//! - ownership 记录写到状态目录，供下一次启动清理；加载记录时校验 workspace 路径在
//!   状态目录内，避免被篡改的记录诱导删除任意目录；
//! - 平台/文件系统不支持时**显式拒绝**（[`IsolationError::Unsupported`]），绝不静默
//!   退化成 `Shared`。
//!
//! 文件系统能力探测与实现是 Linux 优先；其它平台返回
//! [`IsolationError::UnsupportedPlatform`]。

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::result::sha256_hex;

/// 直接声明的系统调用（不依赖 `libc` crate，避免改动 workspace 依赖与 `Cargo.lock`）。
#[cfg(unix)]
mod sys {
    use std::ffi::{c_char, c_int, c_ulong, c_void};

    unsafe extern "C" {
        pub fn geteuid() -> u32;
        /// `int ioctl(int fd, unsigned long request, ...);`
        pub fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
        #[cfg(target_os = "linux")]
        pub fn mount(
            source: *const c_char,
            target: *const c_char,
            fstype: *const c_char,
            flags: c_ulong,
            data: *const c_void,
        ) -> c_int;
        #[cfg(target_os = "linux")]
        pub fn umount2(target: *const c_char, flags: c_int) -> c_int;
    }

    /// `<sys/mount.h>`：detach 后立刻返回，即使仍有使用者。
    #[cfg(target_os = "linux")]
    pub const MNT_DETACH: c_int = 2;
    /// `FICLONE` 定义在 `<linux/fs.h>`：`_IOW(0x94, 9, int)`。
    #[cfg(target_os = "linux")]
    pub const FICLONE: c_ulong = 0x4004_9409;
}

#[cfg(unix)]
fn is_effective_root() -> bool {
    unsafe { sys::geteuid() == 0 }
}

#[cfg(not(unix))]
fn is_effective_root() -> bool {
    false
}

/// 工作区隔离模式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IsolationMode {
    /// 与原会话共享目录。
    #[default]
    Shared,
    /// 独立 git worktree。
    Worktree,
    /// 写时复制克隆（reflink 或 overlayfs）。
    CowClone,
}

impl IsolationMode {
    /// 解析 frontmatter / 配置值。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "shared" | "none" => Some(Self::Shared),
            "worktree" | "git-worktree" => Some(Self::Worktree),
            "cow-clone" | "cowclone" | "cow_clone" | "reflink" | "overlayfs" => {
                Some(Self::CowClone)
            }
            _ => None,
        }
    }

    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Worktree => "worktree",
            Self::CowClone => "cow-clone",
        }
    }
}

impl std::fmt::Display for IsolationMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 实际使用的隔离后端。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IsolationBackend {
    /// 无隔离。
    Shared,
    /// git worktree。
    GitWorktree,
    /// btrfs/XFS reflink 递归克隆。
    Reflink,
    /// overlayfs（需要 root/CAP_SYS_ADMIN）。
    Overlayfs,
}

/// 隔离相关错误。
#[derive(Debug, thiserror::Error)]
pub enum IsolationError {
    /// 平台/文件系统/仓库状态不支持请求的模式。
    #[error("isolation mode `{mode}` is not supported: {reason}")]
    Unsupported {
        /// 请求的模式。
        mode: IsolationMode,
        /// 拒绝原因。
        reason: String,
    },
    /// 当前目录不在 git 仓库内。
    #[error("`{root}` is not inside a git repository")]
    NotGitRepository {
        /// 输入目录。
        root: PathBuf,
    },
    /// 找不到 git 可执行文件。
    #[error("git is not available: {message}")]
    GitUnavailable {
        /// 进程错误。
        message: String,
    },
    /// git 命令返回非零。
    #[error("git {args} failed (status {status:?}): {stderr}")]
    GitCommand {
        /// 参数。
        args: String,
        /// 退出码。
        status: Option<i32>,
        /// stderr。
        stderr: String,
    },
    /// IO 错误。
    #[error(transparent)]
    Io(#[from] io::Error),
    /// 目标已存在。
    #[error("isolation target already exists: {path}")]
    AlreadyExists {
        /// 路径。
        path: PathBuf,
    },
    /// ownership 记录损坏。
    #[error("invalid ownership record `{path}`: {message}")]
    InvalidOwnership {
        /// 记录路径。
        path: PathBuf,
        /// 原因。
        message: String,
    },
    /// 补丁应用失败。
    #[error("patch apply failed: {stderr}")]
    PatchApply {
        /// git apply 的 stderr。
        stderr: String,
    },
    /// 当前平台没有实现该模式。
    #[error("isolation mode `{mode}` is not implemented on this platform")]
    UnsupportedPlatform {
        /// 请求的模式。
        mode: IsolationMode,
    },
    /// 隔离 id 不是安全文件名。
    #[error("invalid isolation id `{id}`: {reason}")]
    InvalidId {
        /// 输入 id。
        id: String,
        /// 原因。
        reason: String,
    },
    /// 拿 baseline 去计算另一个仓库的 delta。
    #[error("baseline root `{expected}` does not match delta root `{actual}`")]
    BaselineMismatch {
        /// baseline 记录的仓库根。
        expected: PathBuf,
        /// 调用方传入的根。
        actual: PathBuf,
    },
    /// delta 无法表示的结构性变化（nested repo 新增/删除）。
    ///
    /// nested repo 的完整内容不在 baseline 里（只有 HEAD/patches/untracked 字节），
    /// 因此既不能生成“删除整个仓库”的补丁，也不能重放一个全新的仓库。显式失败，
    /// 避免 delta 静默丢失改动。
    #[error("workspace delta cannot represent {change} at `{path}`")]
    DeltaUnsupported {
        /// 变化描述。
        change: String,
        /// 受影响的路径。
        path: PathBuf,
    },
}

impl IsolationError {
    fn unsupported(mode: IsolationMode, reason: impl Into<String>) -> Self {
        Self::Unsupported {
            mode,
            reason: reason.into(),
        }
    }
}

/// 创建隔离工作区的选项。
#[derive(Debug, Clone)]
pub struct IsolationOptions {
    /// 状态目录（worktree/clone/ownership 记录都放这里）。默认 `<root>/.kiana/subagents`。
    pub state_dir: Option<PathBuf>,
    /// 显式 id；缺省用 UUID。
    pub id: Option<String>,
    /// baseline 中单个 untracked 文件的字节上限（超过则记录 skipped，不读入内存）。
    pub max_baseline_file_bytes: u64,
    /// 是否允许尝试 overlayfs（需要 root/CAP_SYS_ADMIN）。
    pub allow_overlayfs: bool,
}

impl Default for IsolationOptions {
    fn default() -> Self {
        Self {
            state_dir: None,
            id: None,
            max_baseline_file_bytes: 8 * 1024 * 1024,
            allow_overlayfs: true,
        }
    }
}

/// 一次隔离工作区的所有权记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceOwnership {
    /// 隔离 id。
    pub id: String,
    /// 请求的模式。
    pub mode: IsolationMode,
    /// 实际后端。
    pub backend: IsolationBackend,
    /// 原目录（规范化的绝对路径）。
    pub original_root: PathBuf,
    /// 子代理使用的根目录。
    pub workspace_root: PathBuf,
    /// worktree 的顶层路径（`workspace_root` 可能是仓库内子目录）；非 worktree 为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<PathBuf>,
    /// 记录所在的状态目录。
    pub state_dir: PathBuf,
    /// git 顶层目录。
    pub git_root: Option<PathBuf>,
    /// worktree 分支（detached 时为 `None`）。
    pub branch: Option<String>,
    /// 创建时的 HEAD。
    pub head: Option<String>,
    /// nested repo 相对路径。
    pub nested_repos: Vec<String>,
    /// 创建时间（Unix ms）。
    pub created_at_ms: u64,
    /// 创建该工作区的进程 id；启动 sweep 用它跳过仍被其它进程使用的记录。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// 有改动的子代理工作区会被**保留**（供调用方审阅/合并）；`true` 时启动 sweep
    /// 也跳过这条记录，不会在下一个进程里被当成遗留资源回收。
    #[serde(default)]
    pub retained: bool,
}

impl WorkspaceOwnership {
    /// ownership 记录路径。
    pub fn record_path(&self) -> PathBuf {
        ownership_record_path(&self.state_dir, &self.id)
    }
}

/// 已创建（或复用）的隔离工作区。
#[derive(Debug, Clone)]
pub struct IsolatedWorkspace {
    ownership: WorkspaceOwnership,
}

impl IsolatedWorkspace {
    /// 所有权记录。
    pub fn ownership(&self) -> &WorkspaceOwnership {
        &self.ownership
    }

    /// 子代理工作根目录。
    pub fn root(&self) -> &Path {
        &self.ownership.workspace_root
    }

    /// 请求的模式。
    pub fn mode(&self) -> IsolationMode {
        self.ownership.mode
    }

    /// 实际后端。
    pub fn backend(&self) -> IsolationBackend {
        self.ownership.backend
    }

    /// baseline/delta 使用的仓库根。
    ///
    /// `root()` 是子代理的工作目录（父 cwd 是仓库子目录时会映射到 worktree 内同一
    /// 相对子目录）；`git worktree add` 的工作树顶层才是 delta 的边界。
    pub fn repo_root(&self) -> &Path {
        self.ownership
            .worktree_path
            .as_deref()
            .unwrap_or(&self.ownership.workspace_root)
    }

    /// 清理工作区并删除 ownership 记录。
    ///
    /// 失败时记录保留（供下一次 sweep 重试）；`Shared` 永远是无操作。
    pub fn cleanup(&self) -> Result<(), IsolationError> {
        cleanup_ownership(&self.ownership)?;
        let record = self.ownership.record_path();
        if record.exists() {
            std::fs::remove_file(&record)?;
        }
        Ok(())
    }

    /// 把工作区标记为“有改动、保留”：重写 ownership 记录，使启动 sweep 不再回收它。
    ///
    /// 调用方负责决定何时清理；[`IsolatedWorkspace::cleanup`] 仍然可以显式删除。
    pub fn retain(&self) -> Result<(), IsolationError> {
        let mut ownership = self.ownership.clone();
        ownership.retained = true;
        // 恢复流程里的 ownership 是 adopt 时加载的旧快照，claim 刷新的是磁盘记录；
        // retain 若原样写回旧 pid，后续进程会看到一个可能已被复用的死 pid，记录
        // 既不会被 sweep 回收，也可能因该 pid 被别的进程复用而无法 adopt。
        ownership.pid = Some(std::process::id());
        write_ownership_record(&ownership)
    }
}

/// 隔离工作区相对创建时是否有改动。
///
/// `GitWorktree` 从创建时的 HEAD 干净 checkout。判据必须同时覆盖两种情况：
/// `git status --porcelain`（工作区/untracked/**ignored** 文件）与 HEAD 是否离开
/// 创建时的提交（子代理可能在 worktree 里 commit）。只看 status 会把“已提交的产物”
/// 和“写到 gitignored 目录的产物”判成无改动，随后 `git worktree remove --force`
/// 会连同 detached commit 一起丢掉。其它后端（cow-clone / overlayfs）复制的是父
/// 工作区**当前**状态：没有 baseline 相对的 diff 就无法区分“父工作区原有改动”和
/// “子代理改动”，因此保守地视为有改动。
pub fn workspace_has_changes(workspace: &IsolatedWorkspace) -> bool {
    match workspace.backend() {
        IsolationBackend::GitWorktree => {
            let root = workspace.repo_root();
            if let Some(base) = workspace.ownership.head.as_deref().map(str::trim)
                && !base.is_empty()
            {
                match run_git(root, &["rev-parse", "HEAD"]) {
                    Ok(current) => {
                        if String::from_utf8_lossy(&current).trim() != base {
                            return true;
                        }
                    }
                    // 无法判断时按“有改动”处理，避免丢产物。
                    Err(_) => return true,
                }
            }
            match run_git(
                root,
                &[
                    "status",
                    "--porcelain",
                    "--untracked-files=all",
                    "--ignored=matching",
                ],
            ) {
                Ok(output) => !output.iter().all(u8::is_ascii_whitespace),
                Err(_) => true,
            }
        }
        _ => true,
    }
}

/// 创建隔离工作区。
///
/// `root` 必须已存在。`Shared` 不写 ownership 记录（没有需要清理的资源）。
pub fn create_workspace(
    mode: IsolationMode,
    root: impl AsRef<Path>,
    options: &IsolationOptions,
) -> Result<IsolatedWorkspace, IsolationError> {
    let root = std::fs::canonicalize(root.as_ref())?;
    let state_dir = match &options.state_dir {
        Some(dir) => dir.clone(),
        None => root.join(".kiana").join("subagents"),
    };
    let id = options
        .id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    validate_isolation_id(&id)?;

    let mut ownership = WorkspaceOwnership {
        id: id.clone(),
        mode,
        backend: IsolationBackend::Shared,
        original_root: root.clone(),
        workspace_root: root.clone(),
        worktree_path: None,
        state_dir: state_dir.clone(),
        git_root: None,
        branch: None,
        head: None,
        nested_repos: Vec::new(),
        created_at_ms: now_ms(),
        pid: Some(std::process::id()),
        retained: false,
    };

    // Shared 不产生任何需要清理的资源；不要为了它创建 `.kiana/subagents`。
    if mode == IsolationMode::Shared {
        return Ok(IsolatedWorkspace { ownership });
    }
    // 默认状态目录位于工作区内，仓库可以提交 `.kiana`（或更深层）的符号链接，
    // 把 worktree/clone/ownership 和子代理的权限根一起引到仓库之外。
    reject_symlinked_state_path(&root, &state_dir)?;
    std::fs::create_dir_all(&state_dir)?;
    let state_dir = std::fs::canonicalize(&state_dir)?;
    ownership.state_dir = state_dir.clone();

    match mode {
        IsolationMode::Shared => unreachable!("shared returned above"),
        IsolationMode::Worktree => {
            let git_root = git_toplevel(&root)?;
            let head = git_head(&root)?;
            if head.is_none() {
                return Err(IsolationError::unsupported(
                    mode,
                    "worktree isolation requires at least one commit (unborn HEAD)",
                ));
            }
            let workspace_root = state_dir.join("worktrees").join(&id);
            if workspace_root.exists() {
                return Err(IsolationError::AlreadyExists {
                    path: workspace_root,
                });
            }
            std::fs::create_dir_all(
                workspace_root
                    .parent()
                    .ok_or_else(|| IsolationError::unsupported(mode, "invalid worktree path"))?,
            )?;
            let head_value = head.clone().unwrap_or_default();
            // `git worktree add` 可能在返回非零前就已经登记并填充了工作树（例如
            // checkout 成功后 Hook 失败）。先按临时记录设置清理目标，失败时走同一套
            // 回滚，避免留下 sweep 找不到的 worktree 登记。
            ownership.backend = IsolationBackend::GitWorktree;
            ownership.worktree_path = Some(workspace_root.clone());
            ownership.git_root = Some(git_root.clone());
            ownership.head = head.clone();
            ownership.workspace_root = workspace_root.clone();
            if let Err(error) = run_git(
                &git_root,
                &[
                    "worktree",
                    "add",
                    "--detach",
                    &path_string(&workspace_root),
                    &head_value,
                ],
            ) {
                let _ = cleanup_ownership(&ownership);
                return Err(match error {
                    IsolationError::GitCommand { stderr, .. } => IsolationError::unsupported(
                        mode,
                        format!("git worktree add failed: {stderr}"),
                    ),
                    other => other,
                });
            }
            // 父会话可能从仓库子目录启动；worktree 本身是仓库根，子代理 cwd 应映射到
            // worktree 内的同一相对子目录。清理仍使用 worktree 顶层路径。
            let working_root = root
                .strip_prefix(&git_root)
                .ok()
                .filter(|relative| !relative.as_os_str().is_empty())
                .map(|relative| workspace_root.join(relative))
                .unwrap_or_else(|| workspace_root.clone());
            if working_root != workspace_root
                && let Err(error) = std::fs::create_dir_all(&working_root)
            {
                // 记录还没写：先按记录里的 worktree 目标回滚，否则 `git worktree add`
                // 创建的登记和目录会变成 sweep 找不到的孤儿。
                let _ = cleanup_ownership(&ownership);
                return Err(error.into());
            }
            ownership.workspace_root = working_root;
            ownership.head = head;
            ownership.nested_repos = find_nested_repos(&root, &state_dir)
                .into_iter()
                .filter_map(|path| path.strip_prefix(&root).ok().map(path_to_slash))
                .collect();
        }
        IsolationMode::CowClone => {
            let (backend, workspace_root) = create_cow_clone(&root, &state_dir, &id, options)?;
            ownership.backend = backend;
            ownership.workspace_root = workspace_root;
            ownership.git_root = git_toplevel(&root).ok();
            ownership.head = git_head(&root).ok().flatten();
            ownership.nested_repos = find_nested_repos(&root, &state_dir)
                .into_iter()
                .filter_map(|path| path.strip_prefix(&root).ok().map(path_to_slash))
                .collect();
        }
    }

    // 记录是清理的唯一入口：写记录失败时必须回滚刚创建的工作区，否则 worktree/
    // clone 会变成没有任何 ownership 记录的孤儿，sweep 永远找不到它。
    if let Err(error) = write_ownership_record(&ownership) {
        let _ = cleanup_ownership(&ownership);
        return Err(error);
    }
    Ok(IsolatedWorkspace { ownership })
}

fn create_cow_clone(
    root: &Path,
    state_dir: &Path,
    id: &str,
    options: &IsolationOptions,
) -> Result<(IsolationBackend, PathBuf), IsolationError> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (root, state_dir, id, options);
        return Err(IsolationError::UnsupportedPlatform {
            mode: IsolationMode::CowClone,
        });
    }
    #[cfg(target_os = "linux")]
    {
        // `.git` 是文件/符号链接说明该目录是 linked worktree 或 submodule：克隆会
        // 原样复制这个 `gitdir:` 指针，子代理的 git 命令会直接改写原仓库的索引/
        // HEAD。指针可能位于任意嵌套目录，且 overlayfs 下同样有效，因此整棵树都要
        // 扫描，而不只是根目录。无法在拷贝里“物化”独立仓库，显式拒绝。
        if let Some(pointer) = find_gitdir_pointer(root, state_dir) {
            return Err(IsolationError::unsupported(
                IsolationMode::CowClone,
                format!(
                    "cow-clone cannot isolate a linked worktree or submodule: {} points at another repository; use worktree isolation",
                    pointer.display()
                ),
            ));
        }
        // 克隆目录默认位于 `<cwd>/.kiana/subagents`（仓库内）。父 cwd 是仓库子目录
        // 时，克隆里没有 `.git`，子代理的 git 命令会向上发现原仓库并改写父
        // checkout（`git reset --hard` 会丢掉父工作区的未提交改动）。状态目录在
        // 仓库内时必须要求 root 是仓库顶层；worktree 模式已按子目录映射处理。
        if let Ok(git_root) = git_toplevel(root) {
            let git_root = std::fs::canonicalize(&git_root).unwrap_or(git_root);
            let state_inside_repo = std::fs::canonicalize(state_dir)
                .map(|state| state.starts_with(&git_root))
                .unwrap_or_else(|_| state_dir.starts_with(&git_root));
            if root != git_root && state_inside_repo {
                return Err(IsolationError::unsupported(
                    IsolationMode::CowClone,
                    format!(
                        "cow-clone of a git subdirectory cannot prevent git in the clone from discovering the parent repository at {}; use worktree isolation or run from the repository root",
                        git_root.display()
                    ),
                ));
            }
        }
        // 状态目录等于工作区时，克隆目录位于源树内部，复制会递归包含自身；
        // 必须显式拒绝。祖先/兄弟位置不会把克隆写进源树，可以正常复制。
        if state_dir == root {
            return Err(IsolationError::unsupported(
                IsolationMode::CowClone,
                "state dir must not be the workspace root",
            ));
        }
        let clone_root = state_dir.join("clones").join(id);
        if clone_root.exists() {
            return Err(IsolationError::AlreadyExists { path: clone_root });
        }
        std::fs::create_dir_all(clone_root.parent().ok_or_else(|| {
            IsolationError::unsupported(IsolationMode::CowClone, "invalid clone path")
        })?)?;
        match copy_tree_reflink(root, &clone_root, state_dir) {
            Ok(()) => return Ok((IsolationBackend::Reflink, clone_root)),
            Err(error) => {
                let _ = std::fs::remove_dir_all(&clone_root);
                if !options.allow_overlayfs {
                    return Err(IsolationError::unsupported(
                        IsolationMode::CowClone,
                        format!("reflink clone failed and overlayfs is disabled: {error}"),
                    ));
                }
            }
        }
        match create_overlayfs(root, state_dir, id, options) {
            Ok(merged) => Ok((IsolationBackend::Overlayfs, merged)),
            Err(error) => Err(IsolationError::unsupported(
                IsolationMode::CowClone,
                format!("neither reflink nor overlayfs is available: {error}"),
            )),
        }
    }
}

#[cfg(target_os = "linux")]
fn create_overlayfs(
    lower: &Path,
    state_dir: &Path,
    id: &str,
    _options: &IsolationOptions,
) -> Result<PathBuf, IsolationError> {
    // overlayfs 需要 CAP_SYS_ADMIN；普通用户可先用 user namespace，但 mount 仍需权限。
    // 这里只做能力探测并返回明确错误，不尝试提权。检查必须在建目录之前：否则每次
    // 失败都会在状态目录里留下一个空的 overlay 布局。
    if !is_effective_root() {
        return Err(IsolationError::unsupported(
            IsolationMode::CowClone,
            "overlayfs requires root (CAP_SYS_ADMIN)",
        ));
    }
    let base = state_dir.join("overlays").join(id);
    let upper = base.join("upper");
    let work = base.join("work");
    let merged = base.join("merged");
    let options = format!(
        "lowerdir={},upperdir={},workdir={}",
        lower.display(),
        upper.display(),
        work.display()
    );
    let source = std::ffi::CString::new("overlay").expect("static string");
    let fstype = std::ffi::CString::new("overlay").expect("static string");
    let target = std::ffi::CString::new(path_string(&merged)).map_err(|_| {
        IsolationError::unsupported(IsolationMode::CowClone, "merged path contains NUL")
    })?;
    let data = std::ffi::CString::new(options).map_err(|_| {
        IsolationError::unsupported(IsolationMode::CowClone, "overlay options contain NUL")
    })?;
    std::fs::create_dir_all(&upper)?;
    std::fs::create_dir_all(&work)?;
    std::fs::create_dir_all(&merged)?;
    let cleanup_base = || {
        let _ = std::fs::remove_dir_all(&base);
    };
    let result = unsafe {
        sys::mount(
            source.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            0,
            data.as_ptr() as *const std::ffi::c_void,
        )
    };
    if result != 0 {
        let error = io::Error::last_os_error();
        cleanup_base();
        return Err(IsolationError::unsupported(
            IsolationMode::CowClone,
            format!("overlayfs mount failed: {error}"),
        ));
    }
    Ok(merged)
}

#[cfg(target_os = "linux")]
fn copy_tree_reflink(src: &Path, dst: &Path, exclude: &Path) -> Result<(), IsolationError> {
    copy_dir_reflink(src, dst, nested_state_dir(src, exclude))
}

/// 只有状态目录是工作区的严格后代时才需要从复制/扫描中排除。
///
/// 直接把祖先目录当排除项会命中工作区里的每一条路径：`state_dir=/work`、
/// `root=/work/repo` 时克隆会变成空目录并返回成功。状态目录等于工作区本身
/// 由 `create_cow_clone` 显式拒绝。
fn nested_state_dir<'a>(root: &Path, state_dir: &'a Path) -> Option<&'a Path> {
    (state_dir != root && state_dir.starts_with(root)).then_some(state_dir)
}

#[cfg(target_os = "linux")]
fn copy_dir_reflink(src: &Path, dst: &Path, exclude: Option<&Path>) -> Result<(), IsolationError> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        if exclude.is_some_and(|exclude| path == exclude || path.starts_with(exclude)) {
            continue;
        }
        // 目标路径与 `src` 是一一对应的目录，逐项拼接文件名即可；用
        // `path.strip_prefix(原始 root)` 再拼到递归后的 dst 上会把 `src/a/b`
        // 复制成 `dst/a/a/b`（每下一层多叠一段）。
        let target = dst.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            let link = std::fs::read_link(&path)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&link, &target)?;
            continue;
        }
        if metadata.is_dir() {
            copy_dir_reflink(&path, &target, exclude)?;
            continue;
        }
        if metadata.is_file() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            clone_file(&path, &target, &metadata)?;
            continue;
        }
        return Err(IsolationError::unsupported(
            IsolationMode::CowClone,
            format!("unsupported file type at {}", path.display()),
        ));
    }
    // 目录权限最后复制：源目录只读（0555）时，先设权限会让后面创建子项全部 EACCES。
    copy_permissions(src, dst)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn clone_file(src: &Path, dst: &Path, metadata: &std::fs::Metadata) -> Result<(), IsolationError> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::io::AsRawFd;

    let source = std::fs::File::open(src)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    options.mode(metadata.permissions().mode());
    let destination = options.open(dst)?;
    // FICLONE 定义在 <linux/fs.h>：_IOW(0x94, 9, int)，见上面的 sys 模块。
    let result = unsafe { sys::ioctl(destination.as_raw_fd(), sys::FICLONE, source.as_raw_fd()) };
    if result != 0 {
        let error = io::Error::last_os_error();
        return Err(IsolationError::unsupported(
            IsolationMode::CowClone,
            format!("reflink ioctl failed for {}: {error}", src.display()),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn copy_permissions(src: &Path, dst: &Path) -> Result<(), IsolationError> {
    use std::os::unix::fs::PermissionsExt;
    let permissions = std::fs::metadata(src)?.permissions();
    std::fs::set_permissions(dst, std::fs::Permissions::from_mode(permissions.mode()))?;
    Ok(())
}

#[cfg(not(unix))]
fn copy_permissions(_src: &Path, _dst: &Path) -> Result<(), IsolationError> {
    Ok(())
}

/// baseline 中的 untracked 文件快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselineFile {
    /// 相对仓库根的 slash 分隔路径。
    pub path: String,
    /// 内容 sha256。
    pub sha256: String,
    /// 原始字节（超过上限的文件不读入，见 [`BaselineSkip`]）。
    pub bytes: Vec<u8>,
    /// Unix mode（Windows 为 0）。
    pub mode: u32,
}

/// 因大小/类型被跳过的 baseline 文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselineSkip {
    /// 路径。
    pub path: String,
    /// 原因。
    pub reason: String,
}

/// 单个仓库（根仓库或 nested repo）的 baseline。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoBaseline {
    /// HEAD commit；unborn 仓库为 `None`。
    pub head: Option<String>,
    /// staged 改动（`git diff --cached --binary --full-index`）。
    pub staged_patch: Vec<u8>,
    /// unstaged 改动（`git diff --binary --full-index`）。
    pub unstaged_patch: Vec<u8>,
    /// untracked 文件。
    pub untracked: Vec<BaselineFile>,
    /// 跳过的文件。
    pub skipped: Vec<BaselineSkip>,
}

/// nested repo 的 baseline。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedBaseline {
    /// 相对根仓库的路径。
    pub path: String,
    /// baseline。
    pub repo: RepoBaseline,
}

/// 工作区 baseline。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceBaseline {
    /// 仓库根。
    pub root: PathBuf,
    /// 根仓库 baseline。
    pub repo: RepoBaseline,
    /// nested repos。
    pub nested: Vec<NestedBaseline>,
}

/// nested repo 的 delta。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedDelta {
    /// 相对根仓库路径。
    pub path: String,
    /// tracked 改动。
    pub tracked_patch: Vec<u8>,
    /// untracked 新文件补丁。
    pub untracked_patch: Vec<u8>,
}

/// 工作区 delta。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceDelta {
    /// 仓库根。
    pub root: PathBuf,
    /// 相对 baseline HEAD 的 tracked 改动。
    pub tracked_patch: Vec<u8>,
    /// untracked 新文件补丁。
    pub untracked_patch: Vec<u8>,
    /// nested repo 改动。
    pub nested: Vec<NestedDelta>,
    /// 改动文件列表（诊断用）。
    pub changed_files: Vec<String>,
}

impl WorkspaceDelta {
    /// 根仓库补丁（tracked + untracked），可直接交给 `git apply`。
    pub fn root_patch(&self) -> Vec<u8> {
        let mut patch = self.tracked_patch.clone();
        if !patch.is_empty() && !patch.ends_with(b"\n") {
            patch.push(b'\n');
        }
        patch.extend_from_slice(&self.untracked_patch);
        patch
    }

    /// 是否没有改动。
    pub fn is_empty(&self) -> bool {
        self.tracked_patch.is_empty()
            && self.untracked_patch.is_empty()
            && self
                .nested
                .iter()
                .all(|nested| nested.tracked_patch.is_empty() && nested.untracked_patch.is_empty())
    }
}

/// 捕获 baseline。
///
/// `root` 必须是 git 仓库（或位于仓库内；以 `git rev-parse --show-toplevel` 为准）。
/// nested repo 会被递归捕获，且其内容不会被根仓库的 delta 重复计算。
pub fn capture_baseline(
    root: impl AsRef<Path>,
    options: &IsolationOptions,
) -> Result<WorkspaceBaseline, IsolationError> {
    let root = std::fs::canonicalize(root.as_ref())?;
    let git_root = git_toplevel(&root)?;
    let repo = capture_repo_baseline(&git_root, options)?;
    let mut nested = Vec::new();
    for path in find_nested_repos(&git_root, &state_dir_for(&git_root, options)) {
        let relative = match path.strip_prefix(&git_root) {
            Ok(relative) => path_to_slash(relative),
            Err(_) => continue,
        };
        let nested_repo = capture_repo_baseline(&path, options)?;
        nested.push(NestedBaseline {
            path: relative,
            repo: nested_repo,
        });
    }
    Ok(WorkspaceBaseline {
        root: git_root,
        repo,
        nested,
    })
}

fn capture_repo_baseline(
    repo: &Path,
    options: &IsolationOptions,
) -> Result<RepoBaseline, IsolationError> {
    let head = git_head(repo)?;
    let staged_patch = run_git(
        repo,
        &[
            "diff",
            "--cached",
            "--binary",
            "--full-index",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-relative",
            "--src-prefix=a/",
            "--dst-prefix=b/",
        ],
    )?;
    let unstaged_patch = run_git(
        repo,
        &[
            "diff",
            "--binary",
            "--full-index",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-relative",
            "--src-prefix=a/",
            "--dst-prefix=b/",
        ],
    )?;
    let untracked_output = run_git(repo, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    let mut untracked = Vec::new();
    let mut skipped = Vec::new();
    for raw in untracked_output.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        // baseline 会序列化成 JSON，无法表示任意字节的文件名。非 UTF-8 路径
        // 不能 `from_utf8_lossy` 后当作真实路径（后续 stat 会 404 并静默漏掉）。
        let Ok(relative) = std::str::from_utf8(raw) else {
            skipped.push(BaselineSkip {
                path: String::from_utf8_lossy(raw).into_owned(),
                reason: "untracked path is not valid UTF-8; baseline cannot represent it"
                    .to_string(),
            });
            continue;
        };
        let path = repo.join(relative);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_file() && !metadata.file_type().is_symlink() {
            skipped.push(BaselineSkip {
                path: relative.to_string(),
                reason: "not a regular file".to_string(),
            });
            continue;
        }
        if metadata.is_file() && metadata.len() > options.max_baseline_file_bytes {
            skipped.push(BaselineSkip {
                path: relative.to_string(),
                reason: format!(
                    "file size {} exceeds baseline limit {}",
                    metadata.len(),
                    options.max_baseline_file_bytes
                ),
            });
            continue;
        }
        let (bytes, mode) = untracked_payload(&path, &metadata)?.ok_or_else(|| {
            IsolationError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("cannot read untracked path {relative}"),
            ))
        })?;
        untracked.push(BaselineFile {
            path: relative.to_string(),
            sha256: sha256_hex(&bytes),
            bytes,
            mode,
        });
    }
    Ok(RepoBaseline {
        head,
        staged_patch,
        unstaged_patch,
        untracked,
        skipped,
    })
}

/// 计算相对 baseline 的 delta。
pub fn capture_delta(
    root: impl AsRef<Path>,
    baseline: &WorkspaceBaseline,
    options: &IsolationOptions,
) -> Result<WorkspaceDelta, IsolationError> {
    let root = std::fs::canonicalize(root.as_ref())?;
    if root != baseline.root {
        return Err(IsolationError::BaselineMismatch {
            expected: baseline.root.clone(),
            actual: root,
        });
    }
    let (tracked_patch, untracked_patch, changed_files) =
        capture_repo_delta(&root, &baseline.repo)?;
    let mut nested = Vec::new();
    let baseline_nested: BTreeSet<&str> = baseline
        .nested
        .iter()
        .map(|nested| nested.path.as_str())
        .collect();
    for nested_baseline in &baseline.nested {
        let path = root.join(&nested_baseline.path);
        if !path.exists() {
            // 删除一个 nested repo 等于删除整棵仓库内容；baseline 只保存了 HEAD 与
            // patch，无法重建删除补丁。显式失败，不能让 delta 静默变成“无改动”。
            return Err(IsolationError::DeltaUnsupported {
                change: "deleted nested repository".to_string(),
                path,
            });
        }
        let (tracked_patch, untracked_patch, _) = capture_repo_delta(&path, &nested_baseline.repo)?;
        nested.push(NestedDelta {
            path: nested_baseline.path.clone(),
            tracked_patch,
            untracked_patch,
        });
    }
    // 新出现的 nested repo（例如子代理在 `packages/new` 里 `git init`）同样无法用
    // 文件补丁表示，必须显式失败。
    for candidate in find_nested_repos(&root, &state_dir_for(&root, options)) {
        let relative = candidate
            .strip_prefix(&root)
            .map(path_to_slash)
            .unwrap_or_else(|_| candidate.display().to_string());
        if !baseline_nested.contains(relative.as_str()) {
            return Err(IsolationError::DeltaUnsupported {
                change: "new nested repository".to_string(),
                path: candidate,
            });
        }
    }
    Ok(WorkspaceDelta {
        root,
        tracked_patch,
        untracked_patch,
        nested,
        changed_files,
    })
}

/// 仓库 delta：tracked patch、staged patch、变更文件列表。
type RepoDelta = (Vec<u8>, Vec<u8>, Vec<String>);

/// 构造 baseline 快照对应的 index（HEAD + staged + unstaged + untracked）。
///
/// 返回临时 index 文件路径，调用方把 `GIT_INDEX_FILE` 指向它，让 `git diff` /
/// `git ls-files` 以「baseline 快照」而不是 HEAD/当前 index 为基准：
/// - 父工作区在 baseline 时可能已经是脏的（cow-clone 拷贝的就是这份脏状态），
///   用 HEAD 作基准会把父改动混进子代理的 delta，且无法应用到 baseline 快照；
/// - baseline-untracked 文件被修改/删除/`git add` 成 tracked 时，diff 会正确
///   表现为「修改/删除」，而不是「新建」或干脆漏掉。
fn baseline_snapshot_index(
    repo: &Path,
    baseline: &RepoBaseline,
) -> Result<PathBuf, IsolationError> {
    let index = std::env::temp_dir().join(format!("kiana-index-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let envs = [("GIT_INDEX_FILE", index.as_os_str())];
        if baseline.head.is_some() {
            run_git_env(repo, &envs, &["read-tree", "HEAD"])?;
        } else {
            run_git_env(repo, &envs, &["read-tree", "--empty"])?;
        }
        for patch in [&baseline.staged_patch, &baseline.unstaged_patch] {
            if patch.is_empty() {
                continue;
            }
            let path = std::env::temp_dir().join(format!(
                "kiana-baseline-patch-{}.patch",
                uuid::Uuid::new_v4()
            ));
            std::fs::write(&path, patch)?;
            let applied = run_git_env(
                repo,
                &envs,
                &[
                    "apply",
                    "--cached",
                    "--binary",
                    "--whitespace=nowarn",
                    &path_string(&path),
                ],
            );
            let _ = std::fs::remove_file(&path);
            applied?;
        }
        for file in &baseline.untracked {
            let object = hash_object_stdin(repo, &file.bytes)?;
            let mode = if file.mode & 0o170000 == 0o120000 {
                "120000".to_string()
            } else {
                format!("{:o}", file.mode & 0o7777)
            };
            run_git_env(
                repo,
                &envs,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("{mode},{object},{}", file.path),
                ],
            )?;
        }
        Ok(())
    })();
    result.map(|()| index.clone()).inspect_err(|_| {
        let _ = std::fs::remove_file(&index);
    })
}

fn run_git_env(
    root: &Path,
    envs: &[(&str, &std::ffi::OsStr)],
    args: &[&str],
) -> Result<Vec<u8>, IsolationError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .envs(envs.iter().map(|(key, value)| (key, value)))
        .args(args)
        .output()
        .map_err(|error| IsolationError::GitUnavailable {
            message: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(IsolationError::GitCommand {
            args: args.join(" "),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(output.stdout)
}

fn hash_object_stdin(repo: &Path, bytes: &[u8]) -> Result<String, IsolationError> {
    use std::io::Write as _;

    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["hash-object", "-w", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| IsolationError::GitUnavailable {
            message: error.to_string(),
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(bytes)?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| IsolationError::GitUnavailable {
            message: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(IsolationError::GitCommand {
            args: "hash-object -w --stdin".to_string(),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn capture_repo_delta(repo: &Path, baseline: &RepoBaseline) -> Result<RepoDelta, IsolationError> {
    let index = baseline_snapshot_index(repo, baseline)?;
    let result = capture_repo_delta_against(repo, baseline, &index);
    let _ = std::fs::remove_file(&index);
    result
}

fn capture_repo_delta_against(
    repo: &Path,
    baseline: &RepoBaseline,
    index: &Path,
) -> Result<RepoDelta, IsolationError> {
    let envs = [("GIT_INDEX_FILE", index.as_os_str())];
    // index→worktree：baseline-untracked 文件已在临时 index 里，因此它们的修改
    // 和删除都会出现在 tracked patch 中；子代理新增的 untracked 文件不在 index
    // 里，由下面的 `ls-files --others` 单独处理。
    let tracked_patch = run_git_env(
        repo,
        &envs,
        &[
            "diff",
            "--binary",
            "--full-index",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-relative",
            "--src-prefix=a/",
            "--dst-prefix=b/",
        ],
    )?;
    let changed_output = run_git_env(
        repo,
        &envs,
        &[
            "diff",
            "--name-only",
            "-z",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-relative",
        ],
    )?;
    let mut changed_files: Vec<String> = Vec::new();
    for raw in changed_output.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        let Ok(name) = std::str::from_utf8(raw) else {
            // 与 baseline 相同：delta 的路径字段（JSON）无法表示非 UTF-8 文件名，
            // 静默跳过会让“有改动”变成“无改动”，必须显式失败。
            return Err(IsolationError::DeltaUnsupported {
                change: "tracked path is not valid UTF-8".to_string(),
                path: PathBuf::from(String::from_utf8_lossy(raw).into_owned()),
            });
        };
        changed_files.push(name.to_string());
    }

    let current_untracked = run_git_env(
        repo,
        &envs,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    // baseline 因大小/类型跳过的路径没有保存内容：它们仍然存在时无法生成可应用到
    // baseline 快照的补丁（`diff --no-index /dev/null` 会声称“新建”一个已经存在
    // 的文件）。静默跳过会丢改动、强制生成又会生成不可应用的补丁，因此显式失败。
    let baseline_skipped: BTreeSet<&str> = baseline
        .skipped
        .iter()
        .map(|skip| skip.path.as_str())
        .collect();
    let mut untracked_patch = Vec::new();
    for raw in current_untracked.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        let Ok(relative) = std::str::from_utf8(raw) else {
            return Err(IsolationError::DeltaUnsupported {
                change: "untracked path is not valid UTF-8".to_string(),
                path: PathBuf::from(String::from_utf8_lossy(raw).into_owned()),
            });
        };
        let relative = relative.to_string();
        let path = repo.join(&relative);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_file() && !metadata.file_type().is_symlink() {
            // 嵌套仓库在 `ls-files --others` 里是形如 `inner/` 的目录项，由
            // `capture_delta` 递归捕获，不在这里生成文件补丁。
            continue;
        }
        if baseline_skipped.contains(relative.as_str()) {
            return Err(IsolationError::DeltaUnsupported {
                change:
                    "a baseline-skipped untracked path still exists; its baseline content is unavailable"
                        .to_string(),
                path,
            });
        }
        // delta 不能静默漏掉大文件或符号链接：超过 baseline 上限的文件只是不做
        // 哈希短路（不读进内存），仍然生成补丁；链接按 symlink 补丁输出。
        let patch = diff_untracked_file(repo, &relative)?;
        if !patch.is_empty() {
            untracked_patch.extend_from_slice(&patch);
            if !untracked_patch.ends_with(b"\n") {
                untracked_patch.push(b'\n');
            }
            changed_files.push(relative);
        }
    }
    // baseline 因大小/类型跳过的路径没有保存内容；若它们已被删除，连删除补丁都
    // 无法生成（需要 baseline 字节参与补丁校验）。静默漏掉会让 apply 声称成功但
    // 文件仍在，必须显式失败。目录项（嵌套仓库）由 nested 处理负责。
    for skip in &baseline.skipped {
        if skip.path.ends_with('/') {
            continue;
        }
        if repo.join(&skip.path).symlink_metadata().is_ok() {
            continue;
        }
        return Err(IsolationError::DeltaUnsupported {
            change:
                "a baseline-skipped untracked path was deleted; its baseline content is unavailable"
                    .to_string(),
            path: repo.join(&skip.path),
        });
    }
    Ok((tracked_patch, untracked_patch, changed_files))
}

/// `git diff --no-index --binary -- /dev/null <relative>`；退出码 1 表示有差异。
fn diff_untracked_file(repo: &Path, relative: &str) -> Result<Vec<u8>, IsolationError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "diff",
            "--no-index",
            "--binary",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-relative",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--",
            "/dev/null",
            relative,
        ])
        .output()
        .map_err(|error| IsolationError::GitUnavailable {
            message: error.to_string(),
        })?;
    // `--no-index` 用 1 表示「有差异」，这是正常路径。
    if output.status.code() == Some(1) || output.status.success() {
        Ok(output.stdout)
    } else {
        Err(IsolationError::GitCommand {
            args: format!("diff --no-index --binary -- /dev/null {relative}"),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

/// 把 delta 应用到目标仓库（根仓库 + nested repos）。
pub fn apply_delta(root: impl AsRef<Path>, delta: &WorkspaceDelta) -> Result<(), IsolationError> {
    let root = root.as_ref();
    // 补丁路径相对 delta 捕获时的仓库根。`git apply` 在子目录里运行时会静默忽略
    // 目录外的路径并返回成功——必须拒绝这种“合并成功但只应用了一部分”的调用。
    let canonical = std::fs::canonicalize(root)?;
    if canonical != delta.root {
        return Err(IsolationError::BaselineMismatch {
            expected: delta.root.clone(),
            actual: canonical,
        });
    }
    apply_patch(&canonical, &delta.root_patch())?;
    for nested in &delta.nested {
        let mut patch = nested.tracked_patch.clone();
        if !patch.is_empty() && !patch.ends_with(b"\n") {
            patch.push(b'\n');
        }
        patch.extend_from_slice(&nested.untracked_patch);
        if patch.is_empty() {
            continue;
        }
        let path = root.join(&nested.path);
        apply_patch(&path, &patch)?;
    }
    Ok(())
}

/// 目标仓库根：`path` 所在 git 仓库的顶层（`apply_delta` / `capture_baseline` 都
/// 以仓库顶层为边界）。
pub fn repository_root(path: impl AsRef<Path>) -> Result<PathBuf, IsolationError> {
    git_toplevel(path.as_ref())
}

fn apply_patch(repo: &Path, patch: &[u8]) -> Result<(), IsolationError> {
    if patch.is_empty() {
        return Ok(());
    }
    let path = std::env::temp_dir().join(format!("kiana-delta-{}.patch", uuid::Uuid::new_v4()));
    std::fs::write(&path, patch)?;
    let result = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "apply",
            "--binary",
            "--whitespace=nowarn",
            &path_string(&path),
        ])
        .output()
        .map_err(|error| IsolationError::GitUnavailable {
            message: error.to_string(),
        });
    let _ = std::fs::remove_file(&path);
    let output = result?;
    if !output.status.success() {
        return Err(IsolationError::PatchApply {
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    apply_patch_modes(repo, patch)?;
    Ok(())
}

/// `git apply` 对 mode 变更的处理受进程 umask 影响（无内容 hunks 的纯 mode 补丁在
/// umask 0007 下会得到 0770 而不是 100755 的 0755）。补丁的应用结果必须与 delta
/// 记录一致，因此按 diff 头里的 `new mode` 再显式 chmod 一次。
fn apply_patch_modes(repo: &Path, patch: &[u8]) -> Result<(), IsolationError> {
    use std::os::unix::fs::PermissionsExt;

    let mut current: Option<PathBuf> = None;
    for line in patch.split(|byte| *byte == b'\n') {
        if let Some(rest) = line.strip_prefix(b"diff --git ") {
            current = diff_git_target(rest);
            continue;
        }
        // `new mode`（已有文件的 mode 变更）与 `new file mode`（新文件）都受
        // `git apply` 的 umask 影响，统一按补丁记录的 mode 复核。
        let mode_line = line
            .strip_prefix(b"new mode ")
            .or_else(|| line.strip_prefix(b"new file mode "));
        if let Some(rest) = mode_line {
            let Some(path) = current.as_deref() else {
                continue;
            };
            let Ok(text) = std::str::from_utf8(rest) else {
                continue;
            };
            let Ok(mode) = u32::from_str_radix(text.trim(), 8) else {
                continue;
            };
            // 只处理普通文件；120000（符号链接）由 git apply 自己创建。
            if mode & 0o170000 != 0o100000 {
                continue;
            }
            std::fs::set_permissions(
                repo.join(path),
                std::fs::Permissions::from_mode(mode & 0o7777),
            )?;
        }
    }
    Ok(())
}

/// 从 `diff --git a/<p> b/<p>` 的剩余部分取出 b 侧路径（还原 C-quote）。
///
/// 我们处理的补丁两侧路径相同（修改/mode 变更/删除），因此找到把剩余部分
/// 均分成 `a/<p>` 与 `b/<p>` 的分隔位置；带引号的路径按 git 的 C 风格反转义。
fn diff_git_target(rest: &[u8]) -> Option<PathBuf> {
    if rest.starts_with(b"\"") {
        let first = unquote_git_path(rest)?;
        let consumed = first.0;
        let tail = rest.get(consumed..)?.strip_prefix(b" ")?;
        let second = unquote_git_path(tail)?;
        let path = second.1.strip_prefix(b"b/")?;
        return Some(PathBuf::from(String::from_utf8_lossy(path).into_owned()));
    }
    let mut index = 0;
    while let Some(found) = rest[index..].windows(3).position(|window| window == b" b/") {
        let split = index + found;
        if let (Some(left), Some(right)) = (rest.get(2..split), rest.get(split + 3..))
            && left == right
        {
            return Some(PathBuf::from(String::from_utf8_lossy(right).into_owned()));
        }
        index = split + 1;
    }
    None
}

/// 解析 git 的 C-quoted 路径；返回 (原始片段长度, 反转义后的字节)。
fn unquote_git_path(input: &[u8]) -> Option<(usize, Vec<u8>)> {
    let mut output = Vec::new();
    let mut index = 1; // 跳过开头的 `"`
    while index < input.len() {
        match input[index] {
            b'"' => return Some((index + 1, output)),
            b'\\' => {
                index += 1;
                let escaped = *input.get(index)?;
                match escaped {
                    b'n' => output.push(b'\n'),
                    b't' => output.push(b'\t'),
                    b'r' => output.push(b'\r'),
                    b'"' => output.push(b'"'),
                    b'\\' => output.push(b'\\'),
                    b'0'..=b'7' => {
                        let mut value = 0u32;
                        let mut digits = 0;
                        while digits < 3 {
                            let Some(byte) = input.get(index) else { break };
                            if !(b'0'..=b'7').contains(byte) {
                                break;
                            }
                            value = value * 8 + u32::from(byte - b'0');
                            index += 1;
                            digits += 1;
                        }
                        output.push(u8::try_from(value).ok()?);
                        continue;
                    }
                    other => output.push(other),
                }
                index += 1;
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    None
}

/// 启动 sweep 的结果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// 扫描到的记录数（含损坏）。
    pub records: usize,
    /// 成功清理并删除记录的数量。
    pub cleaned: usize,
    /// 清理失败、记录被保留的 `(record path, error)`。
    pub failed: Vec<(PathBuf, String)>,
    /// 损坏记录被隔离后的路径（`<record>.corrupt[.N]`）。
    pub quarantined: Vec<PathBuf>,
    /// 记录里的进程仍存活（另一个 kiana 进程正在使用该工作区），本次跳过的记录。
    pub skipped_live: Vec<PathBuf>,
    /// 有改动的子代理工作区（已标记 `retained`）：保留给调用方审阅/合并，不自动回收。
    pub skipped_retained: Vec<PathBuf>,
}

/// 扫描并清理状态目录里的 ownership 记录（会话启动时调用）。
///
/// 规则：
/// - 记录里的 workspace 必须位于状态目录内（加载时校验），否则视为损坏并隔离；
/// - 只有后端资源确实删除成功后才删除 ownership 记录；失败时记录保留，供下次重试；
/// - 损坏记录重命名为 `.corrupt`，不会被误用于删除目录，也不会阻塞其它记录。
pub fn sweep_ownership_records(state_dir: impl AsRef<Path>) -> SweepReport {
    sweep_ownership_records_with_keep(state_dir, &[])
}

/// 与 [`sweep_ownership_records`] 相同，但把 `keep_pids` 里的工作区视为仍在使用。
///
/// 崩溃遗留的可恢复 Work 的 worktree / 分支是 `resume_work` 继续运行的 cwd，不能
/// 按“死 pid”回收。SDK 先跑 `recover_work`，成功后才调用本函数，并从 Work 账本
/// 收集 keep pid；recover 失败时跳过 sweep，避免用不完整的 keep-set 回收工作区。
pub fn sweep_ownership_records_with_keep(
    state_dir: impl AsRef<Path>,
    keep_pids: &[u32],
) -> SweepReport {
    let state_dir = state_dir.as_ref();
    let mut report = SweepReport::default();
    for (path, record) in scan_ownership_records(state_dir) {
        report.records += 1;
        match record {
            Ok(ownership) => {
                // 有改动的子代理工作区是调用方的产物（可能还没审阅/合并）：保留记录，
                // 不自动回收（与参照实现“有改动就保留 worktree”一致）。
                if ownership.retained {
                    report.skipped_retained.push(path);
                    continue;
                }
                // 另一个还活着的进程可能正在使用这个工作区（状态目录按仓库共享）：
                // 替它删除会让运行中的子代理瞬间失去 cwd。记录保留到它退出后的下一次
                // sweep。
                //
                // 同一进程也算“可能仍在使用”：`/new`、`/resume` 会在同一进程里创建
                // 新会话，它的启动 sweep 看到的是另一个会话里仍在运行的子代理。
                // 崩溃残留由下一次进程启动（pid 不同）回收。
                if ownership.pid.is_some_and(|pid| {
                    keep_pids.contains(&pid) || pid == std::process::id() || process_is_alive(pid)
                }) {
                    report.skipped_live.push(path);
                    continue;
                }
                match cleanup_ownership(&ownership) {
                    Ok(()) => match std::fs::remove_file(&path) {
                        Ok(()) => report.cleaned += 1,
                        Err(error) => report.failed.push((path, error.to_string())),
                    },
                    Err(error) => report.failed.push((path, error.to_string())),
                }
            }
            Err(_) => match quarantine_record(&path) {
                Ok(quarantined) => report.quarantined.push(quarantined),
                Err(error) => report.failed.push((path, error.to_string())),
            },
        }
    }
    report
}

fn quarantine_record(path: &Path) -> Result<PathBuf, IsolationError> {
    let mut candidate = path.with_extension("json.corrupt");
    let mut index = 1u32;
    while candidate.exists() {
        candidate = path.with_extension(format!("json.corrupt.{index}"));
        index = index.saturating_add(1);
    }
    std::fs::rename(path, &candidate)?;
    Ok(candidate)
}

/// 扫描状态目录里的 ownership 记录（含路径，供 sweep 隔离损坏记录）。
fn scan_ownership_records(
    state_dir: &Path,
) -> Vec<(PathBuf, Result<WorkspaceOwnership, IsolationError>)> {
    let dir = state_dir.join("ownership");
    let mut records = Vec::new();
    // 状态目录的自身与父级都必须是真实目录：默认布局是 `<cwd>/.kiana/subagents`，
    // 仓库可以提交 `.kiana`（或 `subagents`）符号链接，把扫描和清理引到另一个
    // 项目的状态目录。更高层（用户 home 等）可能合法地是链接，不检查。
    let real_state = std::fs::symlink_metadata(state_dir)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink());
    let real_parent = state_dir
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .is_none_or(|parent| {
            std::fs::symlink_metadata(parent)
                .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        });
    if !real_state || !real_parent {
        return records;
    }
    // 状态目录来自被克隆的仓库：仓库可以提交一个指向任意位置的 `ownership`
    // 符号链接。`read_dir` 会跟随链接，sweep 随后会把状态目录之外的 `.json`
    // 文件重命名成 `.corrupt`。只扫描真实目录。
    let real_directory = std::fs::symlink_metadata(&dir)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink());
    if !real_directory {
        return records;
    }
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => return records,
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .collect();
    paths.sort();
    for path in paths {
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        records.push((path.clone(), load_ownership_record(&path, state_dir)));
    }
    records
}

/// 列出状态目录里的 ownership 记录（供启动 sweep）。
///
/// 损坏或被篡改的记录被跳过并返回错误；调用方可以选择记录日志后继续。
pub fn list_ownership_records(
    state_dir: impl AsRef<Path>,
) -> Vec<Result<WorkspaceOwnership, IsolationError>> {
    scan_ownership_records(state_dir.as_ref())
        .into_iter()
        .map(|(_, record)| record)
        .collect()
}

fn load_ownership_record(
    path: &Path,
    state_dir: &Path,
) -> Result<WorkspaceOwnership, IsolationError> {
    // 状态目录可能来自被克隆的仓库（`.kiana/subagents/ownership`）。符号链接与超大
    // 文件都不能让启动 sweep 无界读取：`std::fs::read` 会跟随链接并一次读进内存，
    // 一个指向 /dev/zero 的链接就足以卡死/耗尽每个会话的启动过程。
    const MAX_OWNERSHIP_RECORD_BYTES: u64 = 64 * 1024;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(IsolationError::InvalidOwnership {
            path: path.to_path_buf(),
            message: "ownership record is not a regular file".to_string(),
        });
    }
    if metadata.len() > MAX_OWNERSHIP_RECORD_BYTES {
        return Err(IsolationError::InvalidOwnership {
            path: path.to_path_buf(),
            message: format!(
                "ownership record is {} bytes; limit is {MAX_OWNERSHIP_RECORD_BYTES}",
                metadata.len()
            ),
        });
    }
    let bytes = std::fs::read(path)?;
    let ownership: WorkspaceOwnership =
        serde_json::from_slice(&bytes).map_err(|error| IsolationError::InvalidOwnership {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    // 正文里的 id 决定 `record_path()`。它必须与文件名一致，否则一条放在
    // `ownership/evil.json`、正文 id 指向别的会话的记录会让 claim 的 pid 刷新
    // 写到另一份记录上（或借 `..` 跳出状态目录）。
    if path.file_stem().and_then(|stem| stem.to_str()) != Some(ownership.id.as_str()) {
        return Err(IsolationError::InvalidOwnership {
            path: path.to_path_buf(),
            message: format!(
                "ownership record id `{}` does not match its file name",
                ownership.id
            ),
        });
    }
    validate_ownership_targets(&ownership, state_dir).map_err(|error| match error {
        IsolationError::InvalidOwnership { message, .. } => IsolationError::InvalidOwnership {
            path: path.to_path_buf(),
            message,
        },
        other => other,
    })?;
    Ok(ownership)
}

/// 校验 ownership 记录的清理目标是否可信。
///
/// sweep 会对记录里的路径执行 `remove_dir_all`、`git worktree remove` 与
/// `git branch -D`；被篡改或损坏的记录不能把这些操作引到状态目录之外。规则：
///
/// - `workspace_root` / `worktree_path` 必须位于状态目录内（`Shared` 除外）；
/// - worktree 后端的 `git_root` 必须是存在的目录，`branch` 只接受本实现创建分支时
///   使用的 `kiana/` 前缀（当前 worktree 用 detached HEAD，正常记录里 `branch` 恒为
///   `None`）且必须是合法 ref 名。
fn validate_ownership_targets(
    ownership: &WorkspaceOwnership,
    state_dir: &Path,
) -> Result<(), IsolationError> {
    let invalid = |message: String| IsolationError::InvalidOwnership {
        path: ownership.record_path(),
        message,
    };
    // `create_workspace(Shared)` 从不写记录：磁盘上的 Shared 记录只可能来自伪造
    // 或损坏文件。它会绕过下面所有布局校验（claim 还会按正文的 `state_dir`/`id`
    // 重写记录），因此一律拒绝。
    if ownership.backend == IsolationBackend::Shared {
        return Err(invalid(
            "shared workspaces never have an ownership record".to_string(),
        ));
    }
    // id 直接参与 overlay 布局与记录路径的计算；加载时同样要求是安全文件名。
    if let Err(error) = validate_isolation_id(&ownership.id) {
        return Err(invalid(error.to_string()));
    }
    let canonical_state =
        std::fs::canonicalize(state_dir).unwrap_or_else(|_| state_dir.to_path_buf());
    // 记录里的 `state_dir` 必须就是扫描它的目录：`cleanup_ownership` 用记录自己的
    // 字段做校验，两者不同会让“校验通过”和“删除目标”分叉。
    let recorded_state =
        std::fs::canonicalize(&ownership.state_dir).unwrap_or_else(|_| ownership.state_dir.clone());
    if recorded_state != canonical_state {
        return Err(invalid(format!(
            "ownership state dir {} does not match the scanned state dir {}",
            ownership.state_dir.display(),
            canonical_state.display()
        )));
    }
    // 必须是严格后代：`Path::starts_with` 对相等路径也返回 `true`，一条把
    // `workspace_root` 指向状态目录本身的伪造记录会让 `cleanup_ownership` 的
    // `remove_dir_all` 删掉整个状态目录（所有会话的隔离资源与记录）。
    let inside_state = |path: &Path| {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        canonical != canonical_state && canonical.starts_with(&canonical_state)
    };
    if !inside_state(&ownership.workspace_root) {
        return Err(invalid(format!(
            "workspace root {} is outside state dir {}",
            ownership.workspace_root.display(),
            canonical_state.display()
        )));
    }
    if let Some(worktree) = &ownership.worktree_path
        && !inside_state(worktree)
    {
        return Err(invalid(format!(
            "worktree path {} is outside state dir {}",
            worktree.display(),
            canonical_state.display()
        )));
    }
    // 删除目标必须严格匹配本记录自己的布局：只要求“在状态目录内”会让一条伪造
    // 记录指向另一个会话的 `worktrees/<id>`，sweep 的 `remove_dir_all` 就会删掉
    // 别人正在使用的工作区。
    let canonical =
        |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    match ownership.backend {
        IsolationBackend::GitWorktree => {
            let worktree = ownership
                .worktree_path
                .as_deref()
                .unwrap_or(&ownership.workspace_root);
            let expected = canonical_state.join("worktrees").join(&ownership.id);
            let actual_worktree = canonical(worktree);
            if actual_worktree != expected {
                return Err(invalid(format!(
                    "worktree path {} is not the expected layout {}",
                    worktree.display(),
                    expected.display()
                )));
            }
            let workspace = canonical(&ownership.workspace_root);
            if workspace != actual_worktree && !workspace.starts_with(&actual_worktree) {
                return Err(invalid(format!(
                    "workspace root {} is not inside worktree {}",
                    ownership.workspace_root.display(),
                    actual_worktree.display()
                )));
            }
        }
        IsolationBackend::Reflink => {
            let expected = canonical_state.join("clones").join(&ownership.id);
            let workspace = canonical(&ownership.workspace_root);
            if workspace != expected {
                return Err(invalid(format!(
                    "reflink workspace {} is not the expected layout {}",
                    ownership.workspace_root.display(),
                    expected.display()
                )));
            }
        }
        IsolationBackend::Shared | IsolationBackend::Overlayfs => {}
    }
    // `git_root`/`branch` 只在 worktree 后端参与清理（`git worktree remove`、
    // `git branch -D`）；Reflink/Overlayfs 只删除状态目录内的 `workspace_root`，
    // 原仓库移动或删除不应阻止它们清理。
    if ownership.backend == IsolationBackend::GitWorktree {
        if let Some(git_root) = &ownership.git_root {
            match std::fs::metadata(git_root) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => {
                    return Err(invalid(format!(
                        "git root {} is not a directory",
                        git_root.display()
                    )));
                }
                Err(error) => {
                    return Err(invalid(format!(
                        "git root {} is unavailable: {error}",
                        git_root.display()
                    )));
                }
            }
        }
        if let Some(branch) = &ownership.branch
            && !is_owned_branch_name(branch)
        {
            return Err(invalid(format!(
                "branch {branch:?} is not an ownership-created branch"
            )));
        }
    }
    // overlayfs 清理会删除 `workspace_root.parent()`（整个 overlay 布局）；必须确认
    // 它就是本记录自己的 `<state>/overlays/<id>/merged`，否则一条伪造记录就能让
    // `remove_dir_all` 打到状态目录本身。
    if ownership.backend == IsolationBackend::Overlayfs {
        validate_overlay_layout(ownership, state_dir)?;
    }
    Ok(())
}

/// 校验 overlayfs 记录的 workspace 就是 `<state_dir>/overlays/<id>/merged`。
fn validate_overlay_layout(
    ownership: &WorkspaceOwnership,
    state_dir: &Path,
) -> Result<(), IsolationError> {
    let canonical_state =
        std::fs::canonicalize(state_dir).unwrap_or_else(|_| state_dir.to_path_buf());
    let expected = canonical_state
        .join("overlays")
        .join(&ownership.id)
        .join("merged");
    let actual = std::fs::canonicalize(&ownership.workspace_root)
        .unwrap_or_else(|_| ownership.workspace_root.clone());
    if actual != expected {
        return Err(IsolationError::InvalidOwnership {
            path: ownership.record_path(),
            message: format!(
                "overlay workspace {} is not the expected layout {}",
                ownership.workspace_root.display(),
                expected.display()
            ),
        });
    }
    Ok(())
}

/// 是否是本实现会创建的分支名（`kiana/` 前缀 + 合法 ref 字符）。
fn is_owned_branch_name(branch: &str) -> bool {
    let Some(rest) = branch.strip_prefix("kiana/") else {
        return false;
    };
    if rest.is_empty() || rest.starts_with('-') || rest.ends_with('/') {
        return false;
    }
    if rest.contains("..") || rest.contains("@{") || rest.contains("//") {
        return false;
    }
    rest.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
}

/// 进程是否仍存活（`kill(pid, 0)` 只做存在性与权限检查，不发送信号）。
#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    // `pid_t` 是 i32：直接 `as` 会让超出范围的 pid 回绕成负数，而 `kill(-1, 0)`
    // 探测的是“所有可见进程”，会把死进程的工作区误判成仍在使用的。
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: 传 0 信号不会实际投递；全部参数都是标量。
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    false
}

fn cleanup_ownership(ownership: &WorkspaceOwnership) -> Result<(), IsolationError> {
    // Shared 不产生需要清理的资源，也从不写记录；它没有可校验的目标。必须在
    // `validate_ownership_targets` 之前返回（该校验拒绝磁盘上的 Shared 记录）。
    if ownership.backend == IsolationBackend::Shared {
        return Ok(());
    }
    // 即使记录来自本进程创建（未经过 load 校验），删除前也复核一遍目标路径：清理会
    // 执行 `remove_dir_all` 与 `git branch -D`，不能让一个坏记录删到状态目录之外。
    validate_ownership_targets(ownership, &ownership.state_dir)?;
    match ownership.backend {
        IsolationBackend::Shared => Ok(()),
        IsolationBackend::GitWorktree => {
            let git_root =
                ownership
                    .git_root
                    .as_ref()
                    .ok_or_else(|| IsolationError::InvalidOwnership {
                        path: ownership.record_path(),
                        message: "worktree ownership is missing gitRoot".to_string(),
                    })?;
            let worktree = ownership
                .worktree_path
                .as_deref()
                .unwrap_or(&ownership.workspace_root);
            // 只有该路径确实注册在 `git_root` 的 worktree 列表里，才对仓库执行
            // `worktree remove` / `branch -D`。伪造记录可以把 git_root 指向任意
            // 仓库，否则这里会删掉受害仓库的 `kiana/...` 分支。
            if worktree_registered(git_root, worktree)? {
                let removed = run_git(
                    git_root,
                    &["worktree", "remove", "--force", &path_string(worktree)],
                );
                if removed.is_err() {
                    // `git worktree remove` 可能因为目录被占用/权限失败；只有目录确实
                    // 消失才算清理成功。删除失败必须返回错误并保留记录。
                    match std::fs::remove_dir_all(worktree) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(IsolationError::Io(error)),
                    }
                    // prune 只清理 git 元数据；失败不构成资源泄漏，但记录仍可删除。
                    let _ = run_git(git_root, &["worktree", "prune"]);
                }
                if let Some(branch) = &ownership.branch
                    && branch_exists(git_root, branch)?
                {
                    run_git(git_root, &["branch", "-D", branch])?;
                    if branch_exists(git_root, branch)? {
                        return Err(IsolationError::GitCommand {
                            args: format!("branch -D {branch}"),
                            status: None,
                            stderr: format!("branch `{branch}` still exists after deletion"),
                        });
                    }
                }
            } else {
                // 未注册：只清理状态目录里的目录，绝不动 git_root 的分支。
                match std::fs::remove_dir_all(worktree) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(IsolationError::Io(error)),
                }
            }
            Ok(())
        }
        IsolationBackend::Reflink => {
            if ownership.workspace_root.exists() {
                std::fs::remove_dir_all(&ownership.workspace_root)?;
            }
            Ok(())
        }
        IsolationBackend::Overlayfs => cleanup_overlay(ownership),
    }
}

/// worktree 路径是否注册在 `git_root` 的 worktree 列表里。
fn worktree_registered(git_root: &Path, worktree: &Path) -> Result<bool, IsolationError> {
    let output = run_git(git_root, &["worktree", "list", "--porcelain"])?;
    let target = std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf());
    for line in output.split(|byte| *byte == b'\n') {
        let Some(rest) = line.strip_prefix(b"worktree ") else {
            continue;
        };
        let listed = String::from_utf8_lossy(rest).trim().to_string();
        if listed.is_empty() {
            continue;
        }
        let candidate = std::fs::canonicalize(&listed).unwrap_or_else(|_| PathBuf::from(&listed));
        if candidate == target {
            return Ok(true);
        }
    }
    Ok(false)
}

fn branch_exists(git_root: &Path, branch: &str) -> Result<bool, IsolationError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(git_root)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .map_err(|error| IsolationError::GitUnavailable {
            message: error.to_string(),
        })?;
    Ok(output.status.success())
}

#[cfg(target_os = "linux")]
fn cleanup_overlay(ownership: &WorkspaceOwnership) -> Result<(), IsolationError> {
    // 只删除本记录自己的 overlay 布局（`<state>/overlays/<id>`），绝不用
    // `workspace_root.parent()`：伪造/损坏的记录可能指向状态目录本身。
    validate_overlay_layout(ownership, &ownership.state_dir)?;
    if ownership.workspace_root.exists() {
        let target =
            std::ffi::CString::new(path_string(&ownership.workspace_root)).map_err(|_| {
                IsolationError::InvalidOwnership {
                    path: ownership.record_path(),
                    message: "overlay path contains NUL".to_string(),
                }
            })?;
        unsafe {
            sys::umount2(target.as_ptr(), sys::MNT_DETACH);
        }
    }
    let overlay_root = ownership.state_dir.join("overlays").join(&ownership.id);
    if overlay_root.exists() {
        std::fs::remove_dir_all(&overlay_root)?;
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn cleanup_overlay(_ownership: &WorkspaceOwnership) -> Result<(), IsolationError> {
    Err(IsolationError::UnsupportedPlatform {
        mode: IsolationMode::CowClone,
    })
}

fn write_ownership_record(ownership: &WorkspaceOwnership) -> Result<(), IsolationError> {
    let path = ownership.record_path();
    if let Some(parent) = path.parent() {
        ensure_real_directory(parent)?;
    }
    // `rename` 不会写到符号链接目标，但临时文件用 `write` 会；记录目标本身也不应
    // 是链接，否则后续读取/清理会指向别处。
    reject_symlink(&path.with_extension("json.tmp"))?;
    reject_symlink(&path)?;
    // 落盘 JSON 走 kiana-json：写盘格式必须复刻 JS 的数字格式与对象键序。
    let value = serde_json::to_value(ownership)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let text = kiana_json::stringify(&kiana_json::normalize_numbers(value));
    let temporary = path.with_extension("json.tmp");
    write_ownership_temp(&temporary, text.as_bytes())?;
    std::fs::rename(&temporary, &path)?;
    Ok(())
}

/// 拒绝状态目录路径中由仓库控制的符号链接（例如仓库提交的 `.kiana -> /home`）。
///
/// 默认状态目录位于工作区内：`root` 之下任何一段已存在的符号链接都会让隔离资源
/// 建到仓库之外。自定义状态目录的位置由宿主决定，只拒绝它本身是符号链接，
/// 允许宿主路径中既有的祖先链接（如 macOS 的 `/tmp`）。
fn reject_symlinked_state_path(root: &Path, state_dir: &Path) -> Result<(), IsolationError> {
    let mut checked = Vec::new();
    match state_dir.strip_prefix(root) {
        Ok(relative) => {
            let mut current = root.to_path_buf();
            for component in relative.components() {
                current.push(component);
                checked.push(current.clone());
            }
        }
        Err(_) => checked.push(state_dir.to_path_buf()),
    }
    for path in checked {
        if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "refusing symlinked isolation state path `{}`",
                    path.display()
                ),
            )
            .into());
        }
    }
    Ok(())
}

/// 写入 ownership 临时文件。`reject_symlink` 与真正写入之间存在竞态：子代理可以
/// 在两步之间把固定临时名替换成符号链接。`O_NOFOLLOW` 让 `open` 直接拒绝链接；
/// 非 Unix 平台保留前置检查作为尽力而为。
fn write_ownership_temp(path: &Path, bytes: &[u8]) -> Result<(), IsolationError> {
    use std::io::Write as _;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

/// `create_dir_all` 会跟随符号链接：默认 state dir 位于被克隆仓库内，攻击者可以把
/// `ownership` 提交成指向工作区外的链接。已存在的路径必须是真实目录。
fn ensure_real_directory(path: &Path) -> Result<(), IsolationError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing symlinked ownership directory `{}`",
                path.display()
            ),
        )
        .into()),
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("ownership path `{}` is not a directory", path.display()),
        )
        .into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            std::fs::create_dir_all(path)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn reject_symlink(path: &Path) -> Result<(), IsolationError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing to write ownership record through symlink `{}`",
                path.display()
            ),
        )
        .into()),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// ownership 记录路径：`<state_dir>/ownership/<id>.json`。
pub fn ownership_record_path(state_dir: &Path, id: &str) -> PathBuf {
    state_dir.join("ownership").join(format!("{id}.json"))
}

/// 从隔离工作区内的路径反推所有可能的 `(状态目录, 隔离 id)` 候选。
///
/// 布局固定为 `<state>/worktrees/<id>[/...]`、`<state>/clones/<id>[/...]` 与
/// `<state>/overlays/<id>/merged[/...]`；worktree 的 `workspace_root` 可能是
/// 仓库内子目录，而子目录本身也可能叫 `worktrees`/`clones`，所以从深到浅返回
/// 全部候选，由调用方按“记录是否真实存在”挑选。
fn workspace_ownership_candidates(workspace: &Path) -> Vec<(PathBuf, String)> {
    let mut candidates = Vec::new();
    for ancestor in workspace.ancestors() {
        let Some(parent) = ancestor.parent() else {
            break;
        };
        let Some(name) = ancestor.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let container = parent.file_name().and_then(|name| name.to_str());
        if matches!(container, Some("worktrees" | "clones")) && validate_isolation_id(name).is_ok()
        {
            if let Some(state) = parent.parent() {
                candidates.push((state.to_path_buf(), name.to_string()));
            }
            continue;
        }
        if name == "merged"
            && let Some(overlays) = parent.parent()
            && overlays.file_name().and_then(|name| name.to_str()) == Some("overlays")
            && let Some(id) = parent.file_name().and_then(|name| name.to_str())
            && validate_isolation_id(id).is_ok()
            && let Some(state) = overlays.parent()
        {
            candidates.push((state.to_path_buf(), id.to_string()));
        }
    }
    candidates
}

/// 续跑占用已有隔离工作区时，把 ownership 记录的 pid 刷新为当前进程。
///
/// 记录的 pid 是启动 sweep 判断“记录是否仍被活进程使用”的依据：进程重启后
/// resume 上次的隔离工作区时旧 pid 已经不存在，其它会话启动的 sweep 会把仍在
/// 使用的工作区当成遗留资源回收。返回是否更新了记录；不是隔离布局或记录已
/// 不存在时返回 `Ok(false)`。
pub fn claim_workspace_ownership(workspace: &Path) -> Result<bool, IsolationError> {
    let Some((state_dir, id)) = workspace_ownership_candidates(workspace)
        .into_iter()
        .find(|(state_dir, id)| ownership_record_path(state_dir, id).is_file())
    else {
        return Ok(false);
    };
    let path = ownership_record_path(&state_dir, &id);
    let mut ownership = load_ownership_record(&path, &state_dir)?;
    if ownership.pid == Some(std::process::id()) {
        return Ok(false);
    }
    ownership.pid = Some(std::process::id());
    write_ownership_record(&ownership)?;
    Ok(true)
}

/// 接管一个已经存在的隔离工作区（resume 上次保留的工作区）。
///
/// 只接受路径确实位于记录 `workspace_root` 之内的有效记录（worktree 后端的
/// `workspace_root` 可能是 worktree 内子目录）；记录缺失或损坏时返回 `None`，
/// 恢复流程退化为普通共享 cwd。
/// 该路径是否挂着一份隔离工作区 ownership 记录（无论记录是否可接管）。
///
/// 恢复流程用它区分“普通共享子目录”与“另一个进程/损坏记录持有的隔离工作区”：
/// 后者不能退回 `resume_cwd_in_scope_path`，否则两个运行实例会共用同一 worktree。
pub fn workspace_has_ownership_record(workspace: &Path) -> bool {
    let Ok(workspace) = std::fs::canonicalize(workspace) else {
        return false;
    };
    workspace_ownership_candidates(&workspace)
        .into_iter()
        .any(|(state_dir, id)| ownership_record_path(&state_dir, &id).is_file())
}

pub fn adopt_workspace(workspace: &Path) -> Option<IsolatedWorkspace> {
    // 会话头里的路径可以包含 `..` 或符号链接：先解析成真实路径，否则
    // `starts_with` 只做词法比较，`<workspace>/../../elsewhere` 会被误认成工作区。
    let workspace = std::fs::canonicalize(workspace).ok()?;
    let (state_dir, id) = workspace_ownership_candidates(&workspace)
        .into_iter()
        .find(|(state_dir, id)| ownership_record_path(state_dir, id).is_file())?;
    let path = ownership_record_path(&state_dir, &id);
    let ownership = load_ownership_record(&path, &state_dir).ok()?;
    // 另一个仍存活的进程正在使用这个工作区：接管它的记录会让它的 `Drop` 在删除
    // 工作区时与本运行冲突（或者本运行被它的清理删掉）。
    if ownership
        .pid
        .is_some_and(|pid| pid != std::process::id() && process_is_alive(pid))
    {
        return None;
    }
    let root = std::fs::canonicalize(&ownership.workspace_root)
        .unwrap_or_else(|_| ownership.workspace_root.clone());
    if workspace != root && !workspace.starts_with(&root) {
        return None;
    }
    Some(IsolatedWorkspace { ownership })
}

fn validate_isolation_id(id: &str) -> Result<(), IsolationError> {
    let valid = !id.is_empty()
        && id != "."
        && id != ".."
        && id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        });
    if valid {
        Ok(())
    } else {
        Err(IsolationError::InvalidId {
            id: id.to_string(),
            reason: "id must contain only ASCII letters, digits, `-`, `_` and `.`".to_string(),
        })
    }
}

/// 文件系统能力探测结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsolationCapabilities {
    /// 是否在 git 仓库内。
    pub git: bool,
    /// 当前文件系统是否支持 reflink（实测 FICLONE）。
    pub reflink: bool,
    /// 是否具备 overlayfs 权限（需要 root/CAP_SYS_ADMIN）。
    pub overlayfs: bool,
}

impl IsolationCapabilities {
    /// 探测目录所在文件系统的能力。
    pub fn probe(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        let git = git_toplevel(root).is_ok();
        let reflink = probe_reflink(root);
        let overlayfs = cfg!(target_os = "linux") && is_effective_root();
        Self {
            git,
            reflink,
            overlayfs,
        }
    }

    /// 请求的模式是否有可用后端。
    pub fn supports(&self, mode: IsolationMode) -> bool {
        match mode {
            IsolationMode::Shared => true,
            IsolationMode::Worktree => self.git,
            IsolationMode::CowClone => {
                cfg!(target_os = "linux") && (self.reflink || self.overlayfs)
            }
        }
    }
}

fn probe_reflink(root: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        let directory = root.join(format!(".kiana-reflink-probe-{}", uuid::Uuid::new_v4()));
        if std::fs::create_dir_all(&directory).is_err() {
            return false;
        }
        let source = directory.join("source");
        let target = directory.join("target");
        let result = (|| {
            std::fs::write(&source, b"probe")?;
            let metadata = std::fs::metadata(&source)?;
            clone_file(&source, &target, &metadata)
                .map_err(|error| io::Error::other(error.to_string()))
        })();
        let _ = std::fs::remove_dir_all(&directory);
        result.is_ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = root;
        false
    }
}

fn git_toplevel(root: &Path) -> Result<PathBuf, IsolationError> {
    let output = run_git(root, &["rev-parse", "--show-toplevel"]).map_err(|error| match error {
        IsolationError::GitCommand { .. } => IsolationError::NotGitRepository {
            root: root.to_path_buf(),
        },
        other => other,
    })?;
    let text = String::from_utf8_lossy(&output).trim().to_string();
    if text.is_empty() {
        return Err(IsolationError::NotGitRepository {
            root: root.to_path_buf(),
        });
    }
    Ok(PathBuf::from(text))
}

fn git_head(root: &Path) -> Result<Option<String>, IsolationError> {
    match run_git(root, &["rev-parse", "HEAD"]) {
        Ok(output) => {
            let text = String::from_utf8_lossy(&output).trim().to_string();
            Ok((!text.is_empty()).then_some(text))
        }
        Err(IsolationError::GitCommand { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

fn run_git(root: &Path, args: &[&str]) -> Result<Vec<u8>, IsolationError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|error| IsolationError::GitUnavailable {
            message: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(IsolationError::GitCommand {
            args: args.join(" "),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(output.stdout)
}

/// 查找工作区里指向外部仓库的 `.git` 指针（linked worktree / submodule 的
/// `gitdir:` 文件或符号链接）。目录形式的 `.git` 属于独立仓库，可以安全复制。
fn find_gitdir_pointer(root: &Path, state_dir: &Path) -> Option<PathBuf> {
    let exclude = nested_state_dir(root, state_dir);
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if exclude.is_some_and(|exclude| path == exclude || path.starts_with(exclude)) {
                continue;
            }
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if entry.file_name() == ".git" {
                if !metadata.is_dir() {
                    return Some(path);
                }
                continue;
            }
            // 不跟随符号链接目录：复制阶段也只是复制链接本身。
            if metadata.is_dir() {
                stack.push(path);
            }
        }
    }
    None
}

fn find_nested_repos(root: &Path, state_dir: &Path) -> Vec<PathBuf> {
    let exclude = nested_state_dir(root, state_dir);
    let mut results = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((directory, depth)) = stack.pop() {
        if depth > 8 {
            continue;
        }
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if exclude.is_some_and(|exclude| path == exclude || path.starts_with(exclude)) {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == ".git" {
                continue;
            }
            // `Path::is_dir()` 会跟随符号链接：一个 `cache -> /` 的链接会让创建隔离
            // 工作区的同步路径遍历整个文件系统（LocalSet 被阻塞），也可能把仓库外
            // 的 git 仓库记成 nested repo。符号链接一律不进入。
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                continue;
            }
            // `.git` 本身也不能跟随：linked worktree 的 `.git` 是文件，符号链接则
            // 可能指向任意位置。只有真实的目录/普通文件才算嵌套仓库边界。
            let has_git_entry = std::fs::symlink_metadata(path.join(".git"))
                .is_ok_and(|entry| entry.file_type().is_dir() || entry.file_type().is_file());
            if has_git_entry {
                if path != root {
                    results.push(path);
                }
                continue;
            }
            if matches!(
                name.as_ref(),
                "target" | "node_modules" | ".venv" | "vendor"
            ) {
                continue;
            }
            stack.push((path, depth + 1));
        }
    }
    results.sort();
    results
}

fn state_dir_for(root: &Path, options: &IsolationOptions) -> PathBuf {
    options
        .state_dir
        .clone()
        .unwrap_or_else(|| root.join(".kiana").join("subagents"))
}

fn file_mode(metadata: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode()
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        0
    }
}

/// 读取 untracked 路径的可重放内容。
///
/// 普通文件返回原始字节；符号链接返回**链接目标**并带上 git 的 `120000` 模式（`git
/// diff --no-index` 能直接为它生成 symlink 补丁）。目录、设备、FIFO 等返回 `None`。
fn untracked_payload(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> io::Result<Option<(Vec<u8>, u32)>> {
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            Ok(Some((target.as_os_str().as_bytes().to_vec(), 0o120000)))
        }
        #[cfg(not(unix))]
        {
            Ok(Some((
                target.to_string_lossy().into_owned().into_bytes(),
                0o120000,
            )))
        }
    } else if metadata.is_file() {
        Ok(Some((std::fs::read(path)?, file_mode(metadata))))
    } else {
        Ok(None)
    }
}

fn path_to_slash(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/")
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn init_repo(path: &Path) {
        std::fs::create_dir_all(path).expect("repo dir");
        run_git(path, &["init", "-q"]).expect("init");
        run_git(path, &["config", "user.email", "test@example.com"]).expect("email");
        run_git(path, &["config", "user.name", "Test"]).expect("name");
        std::fs::write(path.join("tracked.txt"), b"base\n").expect("write");
        run_git(path, &["add", "."]).expect("add");
        run_git(path, &["commit", "-q", "-m", "init"]).expect("commit");
    }

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        let mut file = std::fs::File::create(path).expect("create");
        file.write_all(bytes).expect("write");
    }

    #[test]
    fn shared_mode_does_not_create_a_record() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = create_workspace(
            IsolationMode::Shared,
            temp.path(),
            &IsolationOptions::default(),
        )
        .expect("shared");
        assert_eq!(workspace.backend(), IsolationBackend::Shared);
        assert_eq!(
            workspace.root(),
            std::fs::canonicalize(temp.path()).unwrap()
        );
        assert!(
            !temp.path().join(".kiana").exists(),
            "shared mode must not create state directories"
        );
        assert!(
            !ownership_record_path(
                &temp.path().join(".kiana/subagents"),
                &workspace.ownership().id
            )
            .exists()
        );
        workspace.cleanup().expect("cleanup");
    }

    #[test]
    fn path_traversal_isolation_id_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let options = IsolationOptions {
            state_dir: Some(temp.path().join("state")),
            id: Some("../escape".to_string()),
            ..Default::default()
        };
        let error = create_workspace(IsolationMode::Shared, temp.path(), &options).unwrap_err();
        assert!(
            matches!(error, IsolationError::InvalidId { .. }),
            "{error:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn create_workspace_rejects_a_symlinked_state_path() {
        use std::os::unix::fs::symlink;

        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside).expect("outside");
        // 仓库可以提交 `.kiana -> outside`：默认状态目录会被引到仓库外。
        symlink(&outside, repo.join(".kiana")).expect("kiana symlink");
        let error = create_workspace(IsolationMode::Worktree, &repo, &IsolationOptions::default())
            .expect_err("a symlinked state path must be refused");
        assert!(matches!(error, IsolationError::Io(_)), "{error:?}");
        assert!(
            !outside.join("subagents").exists(),
            "nothing may be created through the symlink"
        );

        // 自定义 state_dir 本身是符号链接同样拒绝。
        std::fs::remove_file(repo.join(".kiana")).expect("remove link");
        let state_link = temp.path().join("state");
        symlink(&outside, &state_link).expect("state symlink");
        let options = IsolationOptions {
            state_dir: Some(state_link),
            ..Default::default()
        };
        let error = create_workspace(IsolationMode::Worktree, &repo, &options)
            .expect_err("a symlinked custom state dir must be refused");
        assert!(matches!(error, IsolationError::Io(_)), "{error:?}");
        assert!(!outside.join("worktrees").exists());
    }

    #[test]
    fn baseline_and_delta_ignore_repository_diff_configuration() {
        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        write(&repo.join("tracked.txt"), b"changed\n");
        // 仓库/用户配置可以改写 diff 输出（外部 diff、无前缀），生成的内容
        // 不再是可重放的 git patch。
        run_git(&repo, &["config", "diff.noprefix", "true"]).expect("noprefix");
        run_git(&repo, &["config", "diff.external", "/bin/false"]).expect("external");

        let options = IsolationOptions::default();
        let baseline = capture_baseline(&repo, &options).expect("baseline must ignore diff config");
        let patch = String::from_utf8_lossy(&baseline.repo.unstaged_patch);
        assert!(patch.contains("--- a/tracked.txt"), "{patch}");
        assert!(patch.contains("+++ b/tracked.txt"), "{patch}");

        // baseline 之后新增的 untracked 文件由 delta 生成 `--no-index` 补丁。
        write(&repo.join("new.txt"), b"new\n");
        let delta = capture_delta(&repo, &baseline, &options).expect("delta");
        assert!(
            delta.tracked_patch.is_empty(),
            "{}",
            delta.tracked_patch.len()
        );
        assert!(
            String::from_utf8_lossy(&delta.untracked_patch).contains("new.txt"),
            "untracked patch missing new.txt"
        );
    }

    #[test]
    fn delta_rejects_a_different_repository() {
        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        init_repo(&first);
        init_repo(&second);
        let baseline = capture_baseline(&first, &IsolationOptions::default()).expect("baseline");
        let error = capture_delta(&second, &baseline, &IsolationOptions::default()).unwrap_err();
        assert!(
            matches!(error, IsolationError::BaselineMismatch { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn worktree_is_created_and_cleaned_up() {
        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        let options = IsolationOptions {
            state_dir: Some(temp.path().join("state")),
            ..Default::default()
        };
        let workspace =
            create_workspace(IsolationMode::Worktree, &repo, &options).expect("worktree");
        assert_eq!(workspace.backend(), IsolationBackend::GitWorktree);
        assert!(workspace.root().join("tracked.txt").exists());
        assert_ne!(workspace.root(), repo);
        assert!(workspace.ownership().record_path().exists());
        let root = workspace.root().to_path_buf();
        workspace.cleanup().expect("cleanup");
        assert!(!root.exists(), "worktree removed");
    }

    #[cfg(unix)]
    #[test]
    fn worktree_creation_rolls_back_when_the_mapped_subdirectory_cannot_be_created() {
        use std::os::unix::fs::PermissionsExt;

        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        std::fs::create_dir_all(repo.join("tools/sub")).expect("sub dir");
        write(&repo.join("tools/sub/keep.txt"), b"keep");
        run_git(&repo, &["add", "."]).expect("add");
        run_git(&repo, &["commit", "-q", "-m", "sub"]).expect("commit");
        // 让新 worktree 的 checkout 把映射后的子目录替换成一个普通文件：
        // create_dir_all(<worktree>/tools/sub) 会以 ENOTDIR 失败。
        let hook = repo.join(".git/hooks/post-checkout");
        std::fs::write(
            &hook,
            b"#!/bin/sh\nrm -rf tools/sub\nprintf x > tools/sub\n",
        )
        .expect("hook");
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
            .expect("chmod hook");

        let options = IsolationOptions {
            state_dir: Some(temp.path().join("state")),
            ..Default::default()
        };
        let error = create_workspace(IsolationMode::Worktree, repo.join("tools/sub"), &options)
            .expect_err("mapped subdirectory cannot be created");
        assert!(matches!(error, IsolationError::Io(_)), "{error:?}");

        // 回滚必须真正删掉 worktree 与 git 登记，而不是留下 sweep 找不到的孤儿。
        let worktrees = temp.path().join("state").join("worktrees");
        let leftover = std::fs::read_dir(&worktrees)
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(leftover, 0, "failed creation must not leave a worktree");
        let list = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .expect("git worktree list");
        let text = String::from_utf8_lossy(&list.stdout);
        assert_eq!(text.matches("worktree ").count(), 1, "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn worktree_registration_is_rolled_back_when_git_worktree_add_fails() {
        use std::os::unix::fs::PermissionsExt;

        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        // post-checkout 在工作树登记并填充之后运行；失败会让 `git worktree add`
        // 返回非零，但登记已经存在。
        let hook = repo.join(".git/hooks/post-checkout");
        std::fs::write(&hook, b"#!/bin/sh\nexit 1\n").expect("hook");
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
            .expect("chmod hook");

        let options = IsolationOptions {
            state_dir: Some(temp.path().join("state")),
            ..Default::default()
        };
        let error = create_workspace(IsolationMode::Worktree, &repo, &options)
            .expect_err("git worktree add must fail");
        assert!(
            matches!(error, IsolationError::Unsupported { .. }),
            "{error:?}"
        );

        let worktrees = temp.path().join("state").join("worktrees");
        let leftover = std::fs::read_dir(&worktrees)
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(leftover, 0, "failed creation must not leave a worktree");
        let list = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .expect("git worktree list");
        let text = String::from_utf8_lossy(&list.stdout);
        assert_eq!(text.matches("worktree ").count(), 1, "{text}");
    }

    #[test]
    fn worktree_from_subdirectory_keeps_relative_cwd() {
        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        std::fs::create_dir_all(repo.join("sub")).expect("mkdir");
        std::fs::write(repo.join("sub/file.txt"), b"sub\n").expect("write");
        run_git(&repo, &["add", "."]).expect("add");
        run_git(&repo, &["commit", "-q", "-m", "sub"]).expect("commit");

        let options = IsolationOptions {
            state_dir: Some(temp.path().join("state")),
            ..Default::default()
        };
        let workspace = create_workspace(IsolationMode::Worktree, repo.join("sub"), &options)
            .expect("worktree");
        let root = workspace.root().to_path_buf();
        assert!(
            root.ends_with("sub"),
            "root should map to worktree/sub: {}",
            root.display()
        );
        assert_eq!(std::fs::read(root.join("file.txt")).unwrap(), b"sub\n");
        let top = workspace
            .ownership()
            .worktree_path
            .clone()
            .expect("worktree path");
        workspace.cleanup().expect("cleanup");
        assert!(!top.exists());
    }

    #[test]
    fn workspace_changes_are_detected_and_retained_records_survive_sweep() {
        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        let state = temp.path().join("state");
        let options = IsolationOptions {
            state_dir: Some(state.clone()),
            ..Default::default()
        };
        let workspace =
            create_workspace(IsolationMode::Worktree, &repo, &options).expect("worktree");
        assert!(
            !workspace_has_changes(&workspace),
            "a fresh worktree has no changes"
        );
        write(&workspace.root().join("new.txt"), b"change\n");
        assert!(workspace_has_changes(&workspace));

        // 有改动的工作区被标记保留后，启动 sweep 不再回收它。
        workspace.retain().expect("retain");
        let report = sweep_ownership_records(&state);
        assert_eq!(report.skipped_retained.len(), 1, "{report:?}");
        assert!(workspace.root().exists());
        assert!(workspace.ownership().record_path().exists());

        let root = workspace.root().to_path_buf();
        workspace.cleanup().expect("cleanup");
        assert!(!root.exists());
    }

    #[test]
    fn committed_and_gitignored_deliverables_count_as_workspace_changes() {
        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        write(&repo.join(".gitignore"), b"dist/\n");
        run_git(&repo, &["add", ".gitignore"]).expect("add ignore");
        run_git(&repo, &["commit", "-q", "-m", "ignore"]).expect("commit ignore");

        let state = temp.path().join("state");
        let options = IsolationOptions {
            state_dir: Some(state.clone()),
            ..Default::default()
        };
        let workspace =
            create_workspace(IsolationMode::Worktree, &repo, &options).expect("worktree");
        assert!(!workspace_has_changes(&workspace));

        // gitignored 的交付物也算产物：fresh worktree 里出现的 ignored 文件只可能
        // 是子代理写的。
        write(&workspace.root().join("dist/out.bin"), b"artifact\n");
        assert!(
            workspace_has_changes(&workspace),
            "ignored deliverables must keep the worktree"
        );
        workspace.cleanup().expect("cleanup ignored workspace");

        // 子代理在自己的 worktree 里 commit 后 status 是干净的，但 HEAD 已离开创建点。
        let workspace =
            create_workspace(IsolationMode::Worktree, &repo, &options).expect("worktree");
        assert!(!workspace_has_changes(&workspace));
        write(&workspace.root().join("committed.txt"), b"done\n");
        run_git(workspace.root(), &["add", "committed.txt"]).expect("git add");
        run_git(workspace.root(), &["commit", "-q", "-m", "subagent"]).expect("git commit");
        assert!(
            workspace_has_changes(&workspace),
            "a detached commit must keep the worktree"
        );
        workspace.cleanup().expect("cleanup committed workspace");
    }

    #[test]
    fn worktree_outside_git_is_rejected_explicitly() {
        let temp = tempfile::tempdir().expect("tempdir");
        let options = IsolationOptions {
            state_dir: Some(temp.path().join("state")),
            ..Default::default()
        };
        let error = create_workspace(IsolationMode::Worktree, temp.path(), &options).unwrap_err();
        assert!(
            matches!(error, IsolationError::NotGitRepository { .. }),
            "{error:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reflink_copy_preserves_nested_layout_and_read_only_directories() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let src = temp.path().join("src");
        write(&src.join("top.txt"), b"top\n");
        write(&src.join("nested/deep/file.txt"), b"deep\n");
        write(&src.join("readonly/child.txt"), b"child\n");
        std::fs::set_permissions(src.join("readonly"), std::fs::Permissions::from_mode(0o555))
            .expect("chmod readonly");

        let dst = temp.path().join("dst");
        let exclude = temp.path().join("excluded");
        if let Err(IsolationError::Unsupported { .. }) = copy_tree_reflink(&src, &dst, &exclude) {
            return; // 文件系统不支持 reflink：由 overlayfs 分支处理。
        }
        assert_eq!(std::fs::read(dst.join("top.txt")).unwrap(), b"top\n");
        assert_eq!(
            std::fs::read(dst.join("nested/deep/file.txt")).unwrap(),
            b"deep\n"
        );
        assert!(
            !dst.join("nested/nested").exists(),
            "each entry must be copied to exactly one path"
        );
        assert_eq!(
            std::fs::read(dst.join("readonly/child.txt")).unwrap(),
            b"child\n"
        );
        let mode = std::fs::metadata(dst.join("readonly"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o555, "directory permissions must be preserved");

        // 便于 tempdir 清理。
        std::fs::set_permissions(src.join("readonly"), std::fs::Permissions::from_mode(0o755))
            .expect("restore source mode");
        std::fs::set_permissions(dst.join("readonly"), std::fs::Permissions::from_mode(0o755))
            .expect("restore copy mode");
    }

    #[test]
    fn cow_clone_of_a_git_subdirectory_is_refused() {
        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        let sub = repo.join("crates").join("app");
        std::fs::create_dir_all(&sub).expect("subdir");
        let error = create_workspace(IsolationMode::CowClone, &sub, &IsolationOptions::default())
            .expect_err("a subdirectory clone would let git escape into the parent repository");
        assert!(
            matches!(
                error,
                IsolationError::Unsupported {
                    mode: IsolationMode::CowClone,
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn mode_only_untracked_change_is_captured_and_applied() {
        use std::os::unix::fs::PermissionsExt;

        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        write(&repo.join("script.sh"), b"#!/bin/sh\n");
        std::fs::set_permissions(
            repo.join("script.sh"),
            std::fs::Permissions::from_mode(0o644),
        )
        .expect("chmod 644");
        let options = IsolationOptions::default();
        let baseline = capture_baseline(&repo, &options).expect("baseline");

        std::fs::set_permissions(
            repo.join("script.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("chmod 755");
        let delta = capture_delta(&repo, &baseline, &options).expect("delta");
        assert!(!delta.is_empty(), "chmod-only change must not be dropped");
        assert!(delta.changed_files.iter().any(|path| path == "script.sh"));

        // 恢复到 baseline 快照（内容相同、mode 0644）后应用 delta。
        std::fs::set_permissions(
            repo.join("script.sh"),
            std::fs::Permissions::from_mode(0o644),
        )
        .expect("restore 644");
        apply_delta(&repo, &delta).expect("apply");
        let mode = std::fs::metadata(repo.join("script.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755, "the delta must restore the executable bit");

        // 新文件同样按 `new file mode` 复核：umask 0007 下 git apply 会创建成 0770。
        write(&repo.join("tool.sh"), b"#!/bin/sh\n");
        std::fs::set_permissions(repo.join("tool.sh"), std::fs::Permissions::from_mode(0o755))
            .expect("chmod new file");
        let delta = capture_delta(&repo, &baseline, &options).expect("delta");
        std::fs::remove_file(repo.join("tool.sh")).expect("restore snapshot");
        apply_delta(&repo, &delta).expect("apply");
        let mode = std::fs::metadata(repo.join("tool.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755, "new files must keep the recorded mode");
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_untracked_paths_fail_closed_instead_of_being_dropped() {
        use std::os::unix::ffi::OsStringExt;

        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        let name = std::ffi::OsString::from_vec(b"raw-\xff.txt".to_vec());
        std::fs::write(repo.join(&name), b"bytes\n").expect("write non-utf8 path");

        let baseline = capture_baseline(&repo, &IsolationOptions::default()).expect("baseline");
        assert!(
            baseline.repo.untracked.is_empty(),
            "a non-UTF-8 path cannot be represented in the serialized baseline"
        );
        assert!(
            baseline
                .repo
                .skipped
                .iter()
                .any(|skip| skip.reason.contains("not valid UTF-8")),
            "{:?}",
            baseline.repo.skipped
        );
        let error = capture_delta(&repo, &baseline, &IsolationOptions::default())
            .expect_err("delta must not silently omit a non-UTF-8 path");
        assert!(
            matches!(error, IsolationError::DeltaUnsupported { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn adopt_workspace_loads_a_valid_record_for_a_nested_workspace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let ownership = reflink_ownership(&state, "id-1");
        write_ownership_record(&ownership).expect("write");
        let nested = ownership.workspace_root.join("sub");
        std::fs::create_dir_all(&nested).expect("nested");
        let adopted = adopt_workspace(&nested).expect("adopt");
        assert_eq!(adopted.root(), ownership.workspace_root);
        assert_eq!(adopted.ownership().id, "id-1");
        assert!(adopt_workspace(temp.path()).is_none());
    }

    #[test]
    fn adopt_workspace_refuses_a_record_owned_by_another_live_process() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let mut ownership = reflink_ownership(&state, "id-1");
        // pid 1 一定存在；用当前进程之外的存活 pid 模拟“另一个进程正在使用”。
        ownership.pid = Some(1);
        write_ownership_record(&ownership).expect("write");
        assert!(
            adopt_workspace(&ownership.workspace_root).is_none(),
            "a live foreign owner must not be adopted"
        );

        ownership.pid = Some(std::process::id());
        write_ownership_record(&ownership).expect("rewrite");
        assert!(
            adopt_workspace(&ownership.workspace_root).is_some(),
            "our own record is resumable"
        );
    }

    #[test]
    fn copy_tree_excludes_only_a_state_dir_inside_the_source() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src = temp.path().join("work/repo");
        write(&src.join("keep.txt"), b"keep");
        let ancestor_state = temp.path().join("work");
        let dst = temp.path().join("clone");
        match copy_tree_reflink(&src, &dst, &ancestor_state) {
            Ok(()) => {
                assert!(
                    dst.join("keep.txt").exists(),
                    "an ancestor state dir must not exclude the whole workspace"
                );
            }
            Err(IsolationError::Unsupported { .. }) => return, // 文件系统不支持 reflink
            Err(error) => panic!("{error:?}"),
        }

        let inside_state = src.join(".kiana/subagents");
        write(&inside_state.join("record.json"), b"{}");
        let nested = temp.path().join("clone-nested");
        match copy_tree_reflink(&src, &nested, &inside_state) {
            Ok(()) => {
                assert!(nested.join("keep.txt").exists());
                assert!(
                    !nested.join(".kiana/subagents").exists(),
                    "a state dir inside the workspace must stay excluded"
                );
            }
            Err(IsolationError::Unsupported { .. }) => {}
            Err(error) => panic!("{error:?}"),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cow_clone_rejects_a_state_dir_equal_to_the_workspace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("repo");
        write(&root.join("keep.txt"), b"keep");
        let options = IsolationOptions {
            state_dir: Some(root.clone()),
            allow_overlayfs: false,
            ..Default::default()
        };
        let error = create_workspace(IsolationMode::CowClone, &root, &options)
            .expect_err("the clone would be created inside the source tree");
        assert!(
            matches!(error, IsolationError::Unsupported { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn cow_clone_reports_explicit_refusal_on_unsupported_fs() {
        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        let options = IsolationOptions {
            state_dir: Some(temp.path().join("state")),
            allow_overlayfs: false,
            ..Default::default()
        };
        match create_workspace(IsolationMode::CowClone, &repo, &options) {
            Ok(workspace) => {
                assert_eq!(workspace.backend(), IsolationBackend::Reflink);
                assert!(workspace.root().join("tracked.txt").exists());
                workspace.cleanup().expect("cleanup");
            }
            Err(error) => {
                assert!(
                    matches!(
                        error,
                        IsolationError::Unsupported {
                            mode: IsolationMode::CowClone,
                            ..
                        }
                    ),
                    "{error:?}"
                );
            }
        }
    }

    #[test]
    fn baseline_captures_staged_unstaged_and_untracked() {
        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        std::fs::write(repo.join("tracked.txt"), b"base\nmodified\n").expect("write");
        std::fs::write(repo.join("staged.txt"), b"staged\n").expect("write");
        run_git(&repo, &["add", "staged.txt"]).expect("add");
        write(&repo.join("untracked.txt"), b"untracked\n");

        let baseline = capture_baseline(&repo, &IsolationOptions::default()).expect("baseline");
        assert!(baseline.repo.head.is_some());
        assert!(String::from_utf8_lossy(&baseline.repo.staged_patch).contains("staged.txt"));
        assert!(String::from_utf8_lossy(&baseline.repo.unstaged_patch).contains("tracked.txt"));
        assert_eq!(baseline.repo.untracked.len(), 1);
        assert_eq!(baseline.repo.untracked[0].path, "untracked.txt");
        assert_eq!(baseline.repo.untracked[0].bytes, b"untracked\n");
    }

    #[test]
    fn delta_round_trips_byte_identically_through_git_apply() {
        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        let options = IsolationOptions {
            state_dir: Some(temp.path().join("state")),
            ..Default::default()
        };
        let baseline = capture_baseline(&repo, &options).expect("baseline");

        // 修改 tracked、暂存一个新文件、再加一个二进制 untracked 文件。
        std::fs::write(repo.join("tracked.txt"), b"base\nchanged\n").expect("write");
        std::fs::write(repo.join("staged.txt"), b"staged\n").expect("write");
        run_git(&repo, &["add", "staged.txt"]).expect("add");
        write(&repo.join("untracked.txt"), b"untracked\n");
        write(&repo.join("binary.dat"), b"bin\x00ary");

        let delta = capture_delta(&repo, &baseline, &options).expect("delta");
        assert!(!delta.tracked_patch.is_empty());
        assert!(!delta.untracked_patch.is_empty());
        let mut changed = delta.changed_files.clone();
        changed.sort();
        assert!(changed.contains(&"tracked.txt".to_string()));
        assert!(changed.contains(&"untracked.txt".to_string()));

        // 把仓库重置到 baseline 提交，再应用 delta，逐字节比较。
        run_git(&repo, &["reset", "--hard", "-q", "HEAD"]).expect("reset");
        run_git(&repo, &["clean", "-fdq"]).expect("clean");
        apply_delta(&repo, &delta).expect("apply");
        assert_eq!(
            std::fs::read(repo.join("tracked.txt")).unwrap(),
            b"base\nchanged\n"
        );
        assert_eq!(std::fs::read(repo.join("staged.txt")).unwrap(), b"staged\n");
        assert_eq!(
            std::fs::read(repo.join("untracked.txt")).unwrap(),
            b"untracked\n"
        );
        assert_eq!(
            std::fs::read(repo.join("binary.dat")).unwrap(),
            b"bin\x00ary"
        );
    }

    #[test]
    fn deleted_baseline_untracked_file_appears_as_deletion_patch() {
        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        write(&repo.join("old.txt"), b"old\n");
        let options = IsolationOptions::default();
        let baseline = capture_baseline(&repo, &options).expect("baseline");
        assert_eq!(baseline.repo.untracked.len(), 1);

        std::fs::remove_file(repo.join("old.txt")).expect("remove");
        let delta = capture_delta(&repo, &baseline, &options).expect("delta");
        // baseline-untracked 文件在临时 index 里是“已跟踪”的，因此它的删除表现为
        // tracked patch 的 deleted-file 补丁，而不是 new-file 的反向拼凑。
        assert!(String::from_utf8_lossy(&delta.tracked_patch).contains("deleted file mode"));
        assert!(delta.untracked_patch.is_empty());

        // 恢复到 baseline 快照（重新创建 untracked 文件）后应用 delta，文件应被删除。
        write(&repo.join("old.txt"), b"old\n");
        apply_delta(&repo, &delta).expect("apply");
        assert!(!repo.join("old.txt").exists());
    }

    #[test]
    fn delta_is_relative_to_a_dirty_baseline() {
        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);

        // baseline 时父工作区已经是脏的：一个 unstaged 修改、一个 staged 新文件、
        // 一个 untracked 文件。cow-clone 克隆的就是这份状态。
        write(&repo.join("tracked.txt"), b"base\nparent\n");
        write(&repo.join("staged.txt"), b"parent staged\n");
        run_git(&repo, &["add", "staged.txt"]).expect("add");
        write(&repo.join("untracked.txt"), b"parent untracked\n");
        let options = IsolationOptions::default();
        let baseline = capture_baseline(&repo, &options).expect("baseline");

        // 子代理只改 tracked.txt，并把 baseline 的 untracked 文件改成 tracked 内容。
        write(&repo.join("tracked.txt"), b"base\nchild\n");
        write(&repo.join("untracked.txt"), b"parent untracked\nchild\n");
        run_git(&repo, &["add", "untracked.txt"]).expect("add untracked");
        let delta = capture_delta(&repo, &baseline, &options).expect("delta");

        let tracked_patch = String::from_utf8_lossy(&delta.tracked_patch);
        assert!(tracked_patch.contains("tracked.txt"), "{tracked_patch}");
        assert!(
            tracked_patch.contains("untracked.txt"),
            "untracked-at-baseline -> tracked must be a modification: {tracked_patch}"
        );
        assert!(
            !tracked_patch.contains("staged.txt"),
            "parent's staged change must not leak into the child delta: {tracked_patch}"
        );
        assert!(delta.untracked_patch.is_empty());

        // 恢复“脏 baseline”快照后应用 delta：只应叠加子代理的改动。
        run_git(&repo, &["reset", "--hard", "-q", "HEAD"]).expect("reset");
        run_git(&repo, &["clean", "-fdq"]).expect("clean");
        write(&repo.join("tracked.txt"), b"base\nparent\n");
        write(&repo.join("staged.txt"), b"parent staged\n");
        run_git(&repo, &["add", "staged.txt"]).expect("add");
        write(&repo.join("untracked.txt"), b"parent untracked\n");

        apply_delta(&repo, &delta).expect("apply to dirty baseline");
        assert_eq!(
            std::fs::read(repo.join("tracked.txt")).unwrap(),
            b"base\nchild\n"
        );
        assert_eq!(
            std::fs::read(repo.join("staged.txt")).unwrap(),
            b"parent staged\n"
        );
        assert_eq!(
            std::fs::read(repo.join("untracked.txt")).unwrap(),
            b"parent untracked\nchild\n"
        );
    }

    #[test]
    fn modified_baseline_untracked_file_appears_as_modification_patch() {
        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        write(&repo.join("notes.txt"), b"old\n");
        let options = IsolationOptions::default();
        let baseline = capture_baseline(&repo, &options).expect("baseline");
        assert_eq!(baseline.repo.untracked.len(), 1);

        write(&repo.join("notes.txt"), b"new\n");
        let delta = capture_delta(&repo, &baseline, &options).expect("delta");
        let patch = String::from_utf8_lossy(&delta.tracked_patch);
        assert!(
            patch.contains("notes.txt") && !patch.contains("new file mode"),
            "baseline-present file must be a modification patch: {patch}"
        );
        assert!(delta.untracked_patch.is_empty());

        // 恢复 baseline 快照（文件存在、内容为旧值）后应用 delta：必须原地改内容。
        write(&repo.join("notes.txt"), b"old\n");
        apply_delta(&repo, &delta).expect("apply to baseline snapshot");
        assert_eq!(std::fs::read(repo.join("notes.txt")).unwrap(), b"new\n");
    }

    #[test]
    fn nested_repo_is_captured_and_delta_is_separate() {
        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        let nested = repo.join("nested");
        init_repo(&nested);
        std::fs::write(nested.join("inner.txt"), b"inner\n").expect("write");

        let baseline = capture_baseline(&repo, &IsolationOptions::default()).expect("baseline");
        assert_eq!(baseline.nested.len(), 1);
        assert_eq!(baseline.nested[0].path, "nested");

        std::fs::write(nested.join("inner.txt"), b"inner changed\n").expect("write");
        let delta = capture_delta(&repo, &baseline, &IsolationOptions::default()).expect("delta");
        assert_eq!(delta.nested.len(), 1);
        assert!(
            String::from_utf8_lossy(&delta.nested[0].tracked_patch).contains("inner.txt"),
            "inner.txt was untracked at baseline and modified afterwards"
        );

        run_git(&nested, &["reset", "--hard", "-q", "HEAD"]).expect("reset");
        run_git(&nested, &["clean", "-fdq"]).expect("clean");
        // 恢复 baseline 快照：inner.txt 在 baseline 里是 untracked，delta 是「从
        // baseline 到当前」的修改补丁，必须应用在包含该文件的快照上。
        std::fs::write(nested.join("inner.txt"), b"inner\n").expect("restore baseline file");
        apply_delta(&repo, &delta).expect("apply");
        assert_eq!(
            std::fs::read(nested.join("inner.txt")).unwrap(),
            b"inner changed\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn delta_includes_untracked_symlinks_and_large_files() {
        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        let options = IsolationOptions::default();
        let baseline = capture_baseline(&repo, &options).expect("baseline");

        #[cfg(unix)]
        std::os::unix::fs::symlink("target.txt", repo.join("link")).expect("symlink");
        let large = vec![b'x'; options.max_baseline_file_bytes as usize + 1];
        std::fs::write(repo.join("large.bin"), &large).expect("large");

        let delta = capture_delta(&repo, &baseline, &options).expect("delta");
        let patch = String::from_utf8_lossy(&delta.untracked_patch);
        assert!(patch.contains("120000"), "symlink patch missing: {patch}");
        assert!(delta.changed_files.contains(&"link".to_string()));
        assert!(delta.changed_files.contains(&"large.bin".to_string()));

        // 清空工作区后重放：符号链接必须仍是链接，大文件字节完整。
        run_git(&repo, &["clean", "-fdq"]).expect("clean");
        apply_delta(&repo, &delta).expect("apply");
        let metadata = std::fs::symlink_metadata(repo.join("link")).expect("link");
        assert!(
            metadata.file_type().is_symlink(),
            "link must stay a symlink"
        );
        assert_eq!(
            std::fs::read_link(repo.join("link"))
                .expect("target")
                .as_os_str(),
            std::ffi::OsStr::new("target.txt")
        );
        assert_eq!(
            std::fs::read(repo.join("large.bin")).expect("large").len(),
            large.len()
        );
    }

    #[test]
    fn deleted_nested_repository_is_reported_instead_of_silently_dropped() {
        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        let nested = repo.join("nested");
        init_repo(&nested);
        let options = IsolationOptions::default();
        let baseline = capture_baseline(&repo, &options).expect("baseline");

        std::fs::remove_dir_all(&nested).expect("remove nested");
        let error = capture_delta(&repo, &baseline, &options)
            .expect_err("a deleted nested repo cannot be represented");
        assert!(
            matches!(error, IsolationError::DeltaUnsupported { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn new_nested_repository_is_reported_instead_of_being_dropped() {
        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        let options = IsolationOptions::default();
        let baseline = capture_baseline(&repo, &options).expect("baseline");

        init_repo(&repo.join("fresh"));
        let error = capture_delta(&repo, &baseline, &options)
            .expect_err("a new nested repo cannot be represented");
        assert!(
            matches!(error, IsolationError::DeltaUnsupported { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn ownership_records_round_trip_and_reject_outside_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let workspace = state.join("worktrees").join("id-1");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let ownership = WorkspaceOwnership {
            id: "id-1".to_string(),
            mode: IsolationMode::Worktree,
            backend: IsolationBackend::GitWorktree,
            original_root: temp.path().to_path_buf(),
            workspace_root: workspace,
            worktree_path: None,
            state_dir: state.clone(),
            git_root: None,
            branch: None,
            head: None,
            nested_repos: Vec::new(),
            created_at_ms: 0,
            pid: None,
            retained: false,
        };
        write_ownership_record(&ownership).expect("write");
        let records = list_ownership_records(&state);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].as_ref().expect("record"), &ownership);

        // 篡改：把 workspace 指到状态目录外。
        let mut tampered = ownership.clone();
        tampered.workspace_root = temp.path().join("outside");
        tampered.id = "id-2".to_string();
        write_ownership_record(&tampered).expect("write");
        let records = list_ownership_records(&state);
        assert_eq!(records.len(), 2);
        assert!(records.iter().any(|record| record.is_err()));
    }

    fn reflink_ownership(state: &Path, id: &str) -> WorkspaceOwnership {
        let workspace = state.join("clones").join(id);
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::write(workspace.join("payload.txt"), b"data").expect("payload");
        WorkspaceOwnership {
            id: id.to_string(),
            mode: IsolationMode::CowClone,
            backend: IsolationBackend::Reflink,
            original_root: state.to_path_buf(),
            workspace_root: workspace,
            worktree_path: None,
            state_dir: state.to_path_buf(),
            git_root: None,
            branch: None,
            head: None,
            nested_repos: Vec::new(),
            created_at_ms: 0,
            pid: None,
            retained: false,
        }
    }

    #[cfg(unix)]
    #[test]
    fn ownership_writes_reject_symlinked_directories_and_temp_files() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        std::fs::create_dir_all(&state).expect("state");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside).expect("outside");
        symlink(&outside, state.join("ownership")).expect("ownership symlink");
        let ownership = reflink_ownership(&state, "id-1");
        let error = write_ownership_record(&ownership)
            .expect_err("a symlinked ownership directory must be refused");
        assert!(matches!(error, IsolationError::Io(_)), "{error:?}");
        assert_eq!(
            std::fs::read_dir(&outside).expect("outside").count(),
            0,
            "no record may be written through the symlink"
        );

        // 目标目录是真实目录时，临时文件本身也不能是符号链接。
        std::fs::remove_file(state.join("ownership")).expect("remove symlink");
        std::fs::create_dir_all(state.join("ownership")).expect("real ownership dir");
        let record = ownership.record_path();
        let temporary = record.with_extension("json.tmp");
        let target = temp.path().join("victim.txt");
        std::fs::write(&target, b"untouched").expect("victim");
        symlink(&target, &temporary).expect("temp symlink");
        let error =
            write_ownership_record(&ownership).expect_err("a symlinked temp file must be refused");
        assert!(matches!(error, IsolationError::Io(_)), "{error:?}");
        assert_eq!(
            std::fs::read(&target).expect("victim"),
            b"untouched",
            "the symlink target must not be overwritten"
        );
    }

    #[test]
    fn cleanup_rejects_an_ownership_record_pointing_at_the_state_dir_itself() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let mut ownership = reflink_ownership(&state, "id-1");
        ownership.workspace_root = state.clone();
        let error = cleanup_ownership(&ownership).expect_err("state dir itself is not a target");
        assert!(
            matches!(error, IsolationError::InvalidOwnership { .. }),
            "{error:?}"
        );
        assert!(state.exists(), "the shared state directory must survive");

        // 落盘的伪造记录同样在加载时被拒绝（sweep 不会拿它删任何东西）。
        write_ownership_record(&ownership).expect("write");
        let records = list_ownership_records(&state);
        assert!(records.iter().all(|record| record.is_err()), "{records:?}");
        assert!(state.exists(), "loading must not touch the state dir");
    }

    #[test]
    fn workspace_ownership_candidates_recognize_every_isolation_layout() {
        let candidates = |path: &str| workspace_ownership_candidates(Path::new(path));
        assert!(
            candidates("/state/worktrees/id-1/sub")
                .contains(&(PathBuf::from("/state"), "id-1".to_string()))
        );
        assert!(
            candidates("/state/clones/id-2")
                .contains(&(PathBuf::from("/state"), "id-2".to_string()))
        );
        assert!(
            candidates("/state/overlays/id-3/merged/sub")
                .contains(&(PathBuf::from("/state"), "id-3".to_string()))
        );
        assert!(candidates("/elsewhere/repo").is_empty());
    }

    #[test]
    fn claim_workspace_ownership_refreshes_the_record_pid() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let ownership = reflink_ownership(&state, "id-1");
        let mut recorded = ownership.clone();
        // 一个不可能出现的 pid：模拟上次进程已经退出。
        recorded.pid = Some(u32::MAX);
        write_ownership_record(&recorded).expect("write");

        assert!(
            claim_workspace_ownership(&ownership.workspace_root).expect("claim"),
            "a record owned by another process must be claimed"
        );
        let records = list_ownership_records(&state);
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].as_ref().expect("record").pid,
            Some(std::process::id())
        );

        // 已经是当前进程：不重复写记录。
        assert!(!claim_workspace_ownership(&ownership.workspace_root).expect("claim"));
        // 不是隔离布局：没有可认领的记录。
        assert!(!claim_workspace_ownership(temp.path()).expect("claim"));

        // 工作区内也有名为 `worktrees` 的子目录时，深层伪候选不能遮住真实布局。
        recorded.pid = Some(u32::MAX);
        write_ownership_record(&recorded).expect("write");
        let nested = ownership
            .workspace_root
            .join("src")
            .join("worktrees")
            .join("foo");
        std::fs::create_dir_all(&nested).expect("nested");
        assert!(claim_workspace_ownership(&nested).expect("claim"));
        assert_eq!(
            list_ownership_records(&state)[0]
                .as_ref()
                .expect("record")
                .pid,
            Some(std::process::id())
        );
    }

    #[test]
    fn retain_keeps_the_pid_claimed_by_the_current_process() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let ownership = reflink_ownership(&state, "id-retain");
        let mut recorded = ownership.clone();
        // 上次进程已经退出：adopt 会加载这份旧 pid 快照。
        recorded.pid = Some(u32::MAX);
        write_ownership_record(&recorded).expect("write");

        let adopted = adopt_workspace(&ownership.workspace_root).expect("adopt");
        assert!(claim_workspace_ownership(&ownership.workspace_root).expect("claim"));
        adopted.retain().expect("retain");

        let records = list_ownership_records(&state);
        assert_eq!(records.len(), 1);
        let record = records[0].as_ref().expect("record");
        assert!(record.retained);
        assert_eq!(
            record.pid,
            Some(std::process::id()),
            "retain must not write the stale adopted pid back"
        );
    }

    #[test]
    fn sweep_cleans_reflink_workspace_and_removes_record() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let ownership = reflink_ownership(&state, "id-1");
        write_ownership_record(&ownership).expect("write");
        let workspace = ownership.workspace_root.clone();
        assert!(workspace.exists());

        let report = sweep_ownership_records(&state);
        assert_eq!(report.records, 1);
        assert_eq!(report.cleaned, 1, "{report:?}");
        assert!(report.failed.is_empty(), "{report:?}");
        assert!(!workspace.exists(), "workspace must be deleted");
        assert!(!ownership.record_path().exists(), "record must be deleted");
    }

    #[test]
    fn sweep_keeps_the_record_when_cleanup_fails() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let ownership = reflink_ownership(&state, "id-1");
        // 把 workspace 目录替换成同名文件：remove_dir_all 会失败。
        std::fs::remove_dir_all(&ownership.workspace_root).expect("remove dir");
        std::fs::write(&ownership.workspace_root, b"not a directory").expect("file");
        write_ownership_record(&ownership).expect("write");

        let report = sweep_ownership_records(&state);
        assert_eq!(report.records, 1);
        assert_eq!(report.cleaned, 0, "{report:?}");
        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert!(
            ownership.record_path().exists(),
            "failed cleanup must keep the ownership record for retry"
        );

        // 修好障碍后下一次 sweep 能完成清理。
        std::fs::remove_file(&ownership.workspace_root).expect("remove file");
        let report = sweep_ownership_records(&state);
        assert_eq!(report.cleaned, 1, "{report:?}");
        assert!(!ownership.record_path().exists());
    }

    #[test]
    fn sweep_quarantines_records_with_tampered_cleanup_targets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");

        // worktree_path 指到状态目录外：清理会 remove_dir_all 任意目录。
        let mut outside_worktree = reflink_ownership(&state, "id-1");
        outside_worktree.worktree_path = Some(temp.path().join("outside"));
        write_ownership_record(&outside_worktree).expect("write");
        assert!(
            list_ownership_records(&state)
                .iter()
                .any(|record| record.is_err()),
            "worktree outside the state dir must be rejected"
        );

        // branch 不是本实现创建的分支：清理会 git branch -D 用户仓库里的任意分支。
        let mut foreign_branch = reflink_ownership(&state, "id-2");
        foreign_branch.backend = IsolationBackend::GitWorktree;
        foreign_branch.branch = Some("main".to_string());
        write_ownership_record(&foreign_branch).expect("write");

        let report = sweep_ownership_records(&state);
        assert_eq!(report.records, 2, "{report:?}");
        assert_eq!(report.cleaned, 0, "{report:?}");
        assert_eq!(report.quarantined.len(), 2, "{report:?}");
        assert!(outside_worktree.workspace_root.exists(), "not deleted");
    }

    #[test]
    fn sweep_keeps_workspaces_referenced_by_recoverable_work() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let mut ownership = reflink_ownership(&state, "id-keep");
        ownership.pid = Some(424_242);
        write_ownership_record(&ownership).expect("write");

        let report = sweep_ownership_records_with_keep(&state, &[424_242]);
        assert_eq!(report.records, 1);
        assert_eq!(report.cleaned, 0, "{report:?}");
        assert_eq!(report.failed.len(), 0, "{report:?}");
        assert!(
            ownership.workspace_root.exists(),
            "a resumable work's workspace must survive the startup sweep"
        );

        // 没有 Work 引用它的下一次启动正常回收。
        let report = sweep_ownership_records(&state);
        assert_eq!(report.cleaned, 1, "{report:?}");
        assert!(!ownership.workspace_root.exists());
    }

    #[cfg(unix)]
    #[test]
    fn sweep_skips_records_owned_by_a_live_process() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let mut ownership = reflink_ownership(&state, "id-live");
        // 用真实存活的子进程充当“另一个正在运行的 kiana 进程”。
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn live process");
        ownership.pid = Some(child.id());
        write_ownership_record(&ownership).expect("write");

        let report = sweep_ownership_records(&state);
        assert_eq!(report.records, 1, "{report:?}");
        assert_eq!(report.skipped_live.len(), 1, "{report:?}");
        assert_eq!(report.cleaned, 0, "{report:?}");
        assert!(
            ownership.workspace_root.exists(),
            "another live process still owns the workspace"
        );
        assert!(ownership.record_path().exists());

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn sweep_skips_records_owned_by_this_process() {
        // 同一进程里的新会话（/new、/resume）会跑 sweep；另一个会话仍在运行的子代理
        // 也带着本进程的 pid，不能替它删除工作区。
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let mut ownership = reflink_ownership(&state, "id-self");
        ownership.pid = Some(std::process::id());
        write_ownership_record(&ownership).expect("write");

        let report = sweep_ownership_records(&state);
        assert_eq!(report.records, 1, "{report:?}");
        assert_eq!(report.skipped_live.len(), 1, "{report:?}");
        assert_eq!(report.cleaned, 0, "{report:?}");
        assert!(
            ownership.workspace_root.exists(),
            "a live in-process subagent still owns the workspace"
        );
        assert!(ownership.record_path().exists());
    }

    #[test]
    fn sweep_rejects_overlay_records_that_are_not_the_expected_layout() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        std::fs::create_dir_all(&state).expect("state");
        let mut ownership = reflink_ownership(&state, "id-ov");
        ownership.backend = IsolationBackend::Overlayfs;
        // 最坏情况：workspace_root 指向状态目录本身，旧实现会 remove_dir_all(parent)
        // 把整个状态目录删掉。
        ownership.workspace_root = state.clone();
        write_ownership_record(&ownership).expect("write");

        let report = sweep_ownership_records(&state);
        assert_eq!(report.records, 1, "{report:?}");
        assert_eq!(report.quarantined.len(), 1, "{report:?}");
        assert!(
            state.exists(),
            "state dir must survive a tampered overlay record"
        );
        assert!(state.join("ownership").exists());
        assert_eq!(report.cleaned, 0, "{report:?}");
    }

    #[cfg(unix)]
    #[test]
    fn overlay_creation_leaves_no_layout_when_it_is_refused() {
        if is_effective_root() {
            // root 会真的尝试 mount；本测试只覆盖“能力不足时不留残骸”。
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let lower = temp.path().join("lower");
        std::fs::create_dir_all(&lower).expect("lower");
        let error = create_overlayfs(&lower, &state, "id-ov", &IsolationOptions::default())
            .expect_err("unprivileged overlayfs must be refused");
        assert!(matches!(error, IsolationError::Unsupported { .. }));
        assert!(
            !state.join("overlays").exists(),
            "refused overlay creation must not leave directories behind"
        );
    }

    #[test]
    fn sweep_quarantines_corrupt_records() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let ownership_dir = state.join("ownership");
        std::fs::create_dir_all(&ownership_dir).expect("ownership dir");
        let path = ownership_dir.join("bad.json");
        std::fs::write(&path, b"{not json").expect("write corrupt");

        let report = sweep_ownership_records(&state);
        assert_eq!(report.records, 1);
        assert_eq!(report.quarantined.len(), 1, "{report:?}");
        assert!(!path.exists(), "corrupt record must be moved away");
        let quarantined = &report.quarantined[0];
        assert!(quarantined.exists());
        assert_eq!(
            quarantined.file_name().and_then(|name| name.to_str()),
            Some("bad.json.corrupt")
        );
    }

    #[cfg(unix)]
    #[test]
    fn sweep_quarantines_symlinked_and_oversized_ownership_records() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let ownership_dir = state.join("ownership");
        std::fs::create_dir_all(&ownership_dir).expect("ownership dir");
        // 指向 /dev/zero 的链接：修复前 `std::fs::read` 会无界读取并卡死启动。
        std::os::unix::fs::symlink("/dev/zero", ownership_dir.join("link.json")).expect("symlink");
        std::fs::write(ownership_dir.join("huge.json"), vec![b' '; 65 * 1024]).expect("oversized");

        let report = sweep_ownership_records(&state);
        assert_eq!(report.records, 2, "{report:?}");
        assert_eq!(report.quarantined.len(), 2, "{report:?}");
        assert!(!ownership_dir.join("link.json").exists());
        assert!(!ownership_dir.join("huge.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn sweep_does_not_follow_a_symlinked_ownership_directory() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        std::fs::create_dir_all(&state).expect("state");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside).expect("outside");
        let victim = outside.join("victim.json");
        std::fs::write(&victim, b"{}").expect("victim");
        symlink(&outside, state.join("ownership")).expect("ownership symlink");

        let report = sweep_ownership_records(&state);
        assert_eq!(report.records, 0, "{report:?}");
        assert_eq!(report.quarantined.len(), 0, "{report:?}");
        assert!(
            victim.exists(),
            "a symlinked ownership dir must not expose files outside the state dir"
        );
        assert!(list_ownership_records(&state).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn sweep_does_not_follow_symlinked_state_ancestors() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let victim = temp.path().join("victim");
        let victim_state = victim.join(".kiana/subagents");
        let victim_records = victim_state.join("ownership");
        std::fs::create_dir_all(&victim_records).expect("victim records");
        let broken = victim_records.join("broken.json");
        std::fs::write(&broken, b"{}").expect("broken record");

        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).expect("project");
        symlink(victim.join(".kiana"), project.join(".kiana")).expect("kiana symlink");

        let report = sweep_ownership_records(project.join(".kiana/subagents"));
        assert_eq!(report.records, 0, "{report:?}");
        assert_eq!(report.quarantined.len(), 0, "{report:?}");
        assert!(
            broken.exists(),
            "a symlinked `.kiana` must not expose another project's state dir"
        );
    }

    fn write_raw_record(path: &Path, ownership: &WorkspaceOwnership) {
        let value = serde_json::to_value(ownership).expect("record JSON");
        let text = kiana_json::stringify(&kiana_json::normalize_numbers(value));
        std::fs::write(path, text.as_bytes()).expect("write record");
    }

    #[test]
    fn ownership_record_id_must_match_its_file_name() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let ownership = reflink_ownership(&state, "victim");
        write_ownership_record(&ownership).expect("write");
        // 正文 id 仍指向 `victim`，但文件被改名成 `evil.json`：claim 不能借正文
        // 里的 id 找到并改写另一份记录。
        let forged = state.join("ownership").join("evil.json");
        std::fs::rename(ownership.record_path(), &forged).expect("rename");
        let records = list_ownership_records(&state);
        assert_eq!(records.len(), 1);
        assert!(records[0].is_err(), "{records:?}");

        std::fs::create_dir_all(state.join("clones").join("evil")).expect("workspace");
        assert!(claim_workspace_ownership(&state.join("clones").join("evil")).is_err());
        assert!(
            !ownership.record_path().exists(),
            "a forged record must not make claim write through the body id"
        );
    }

    #[test]
    fn shared_backend_ownership_records_are_never_trusted() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let ownership_dir = state.join("ownership");
        std::fs::create_dir_all(&ownership_dir).expect("ownership");
        let mut ownership = reflink_ownership(&state, "evil");
        ownership.mode = IsolationMode::Shared;
        ownership.backend = IsolationBackend::Shared;
        // 正文声明另一个 state_dir：旧实现跳过全部校验，claim 会按它重写记录。
        let elsewhere = temp.path().join("elsewhere");
        ownership.state_dir = elsewhere.clone();
        ownership.workspace_root = temp.path().to_path_buf();
        write_raw_record(&ownership_dir.join("evil.json"), &ownership);
        std::fs::create_dir_all(state.join("clones").join("evil")).expect("workspace");

        let error = claim_workspace_ownership(&state.join("clones").join("evil"))
            .expect_err("shared records must be rejected");
        assert!(
            matches!(error, IsolationError::InvalidOwnership { .. }),
            "{error:?}"
        );
        assert!(
            !elsewhere.exists(),
            "claim must not create a record outside the scanned state dir"
        );
        assert!(adopt_workspace(&state.join("clones").join("evil")).is_none());

        let report = sweep_ownership_records(&state);
        assert_eq!(report.quarantined.len(), 1, "{report:?}");
    }

    #[test]
    fn workspace_cleanup_failure_keeps_the_ownership_record() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let ownership = reflink_ownership(&state, "id-1");
        std::fs::remove_dir_all(&ownership.workspace_root).expect("remove dir");
        std::fs::write(&ownership.workspace_root, b"blocked").expect("file");
        write_ownership_record(&ownership).expect("write");
        let workspace = IsolatedWorkspace {
            ownership: ownership.clone(),
        };
        workspace.cleanup().expect_err("cleanup must fail");
        assert!(
            ownership.record_path().exists(),
            "failure must not delete the retry record"
        );
    }

    #[test]
    fn cow_clone_refuses_a_linked_worktree_git_pointer() {
        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        let linked = temp.path().join("linked");
        let Ok(status) = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["worktree", "add", "--detach", &path_string(&linked), "HEAD"])
            .status()
        else {
            return; // 没有 git：跳过
        };
        if !status.success() {
            return;
        }
        assert!(
            std::fs::symlink_metadata(linked.join(".git"))
                .expect("git pointer")
                .file_type()
                .is_file()
        );
        let error = create_workspace(
            IsolationMode::CowClone,
            &linked,
            &IsolationOptions::default(),
        )
        .expect_err("a linked worktree cannot be cloned safely");
        assert!(
            matches!(
                error,
                IsolationError::Unsupported {
                    mode: IsolationMode::CowClone,
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cow_clone_refuses_a_nested_linked_worktree_git_pointer() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo");
        let nested = repo.join("vendor").join("linked");
        std::fs::create_dir_all(&nested).expect("nested");
        // nested 仓库的 `.git` 是指向原仓库的指针文件；复制后会直接改写原仓库。
        std::fs::write(nested.join(".git"), b"gitdir: /tmp/elsewhere\n").expect("pointer");

        let error = create_workspace(IsolationMode::CowClone, &repo, &IsolationOptions::default())
            .expect_err("a nested linked worktree must be refused");
        assert!(
            matches!(
                error,
                IsolationError::Unsupported {
                    mode: IsolationMode::CowClone,
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn ownership_records_reject_a_sibling_worktree_target() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("state");
        let sibling = state.join("worktrees").join("other-session");
        std::fs::create_dir_all(&sibling).expect("sibling");
        let ownership = WorkspaceOwnership {
            id: "id-1".to_string(),
            mode: IsolationMode::Worktree,
            backend: IsolationBackend::GitWorktree,
            original_root: temp.path().to_path_buf(),
            workspace_root: sibling.clone(),
            worktree_path: Some(sibling.clone()),
            state_dir: state.clone(),
            git_root: None,
            branch: None,
            head: None,
            nested_repos: Vec::new(),
            created_at_ms: 0,
            pid: None,
            retained: false,
        };
        write_ownership_record(&ownership).expect("write");
        let records = list_ownership_records(&state);
        assert_eq!(records.len(), 1);
        assert!(
            records[0].is_err(),
            "a record pointing at another session's worktree must be rejected"
        );
        // 清理路径同样拒绝：不能在 sibling 目录上执行 remove_dir_all。
        cleanup_ownership(&ownership).expect_err("cleanup must refuse a foreign target");
        assert!(sibling.exists(), "the sibling workspace must survive");
    }

    #[test]
    fn nested_repo_scan_does_not_follow_symlinks() {
        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        let external = temp.path().join("external");
        init_repo(&external);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&external, repo.join("cache")).expect("symlink");
        let found = find_nested_repos(&repo, &repo.join(".kiana").join("subagents"));
        assert!(
            !found.iter().any(|path| path.ends_with("cache")),
            "symlinked directories must not be treated as nested repositories: {found:?}"
        );

        // `.git` 本身是符号链接时也不能当作嵌套仓库边界。
        let fake = repo.join("fake");
        std::fs::create_dir_all(&fake).expect("fake");
        #[cfg(unix)]
        std::os::unix::fs::symlink(external.join(".git"), fake.join(".git")).expect("git symlink");
        let found = find_nested_repos(&repo, &repo.join(".kiana").join("subagents"));
        assert!(
            !found.iter().any(|path| path.ends_with("fake")),
            "{found:?}"
        );
    }

    #[test]
    fn apply_delta_rejects_a_subdirectory_root() {
        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        write(&repo.join("sub").join("s.txt"), b"base\n");
        run_git(&repo, &["add", "."]).expect("add");
        run_git(&repo, &["commit", "-q", "-m", "sub"]).expect("commit");
        let options = IsolationOptions::default();
        let baseline = capture_baseline(&repo, &options).expect("baseline");
        write(&repo.join("sub").join("s.txt"), b"changed\n");
        let delta = capture_delta(&repo, &baseline, &options).expect("delta");

        let error = apply_delta(repo.join("sub"), &delta)
            .expect_err("git apply from a subdirectory silently skips root paths");
        assert!(
            matches!(error, IsolationError::BaselineMismatch { .. }),
            "{error:?}"
        );

        write(&repo.join("sub").join("s.txt"), b"base\n");
        apply_delta(&repo, &delta).expect("apply at the delta root");
        assert_eq!(
            std::fs::read(repo.join("sub").join("s.txt")).unwrap(),
            b"changed\n"
        );
    }

    #[test]
    fn baseline_skipped_untracked_paths_cannot_be_deltaed() {
        if !git_available() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        init_repo(&repo);
        write(&repo.join("artifact.bin"), &[b'x'; 64]);
        let options = IsolationOptions {
            max_baseline_file_bytes: 8,
            ..Default::default()
        };
        let baseline = capture_baseline(&repo, &options).expect("baseline");
        assert!(
            baseline
                .repo
                .skipped
                .iter()
                .any(|skip| skip.path == "artifact.bin"),
            "{:?}",
            baseline.repo.skipped
        );
        let error = capture_delta(&repo, &baseline, &options)
            .expect_err("a skipped baseline file cannot be represented as a delta");
        assert!(
            matches!(error, IsolationError::DeltaUnsupported { .. }),
            "{error:?}"
        );

        // 删除同样无法表达：没有 baseline 字节就生成不了可应用到快照的删除补丁，
        // 静默跳过会让 apply 报成功而文件仍在。
        std::fs::remove_file(repo.join("artifact.bin")).expect("remove");
        let error = capture_delta(&repo, &baseline, &options)
            .expect_err("a deleted skipped baseline file cannot be represented as a delta");
        match error {
            IsolationError::DeltaUnsupported { change, .. } => {
                assert!(change.contains("deleted"), "{change}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
