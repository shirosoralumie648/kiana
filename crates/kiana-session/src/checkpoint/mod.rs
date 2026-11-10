//! 私有 git checkpoint：每个项目一个隔离的 git dir，每个 workdir 一套 ref/index/metadata。
//!
//! 目标：在每个 mutating tool 之前留一个可回滚的树快照，但**绝不触碰用户自己的
//! `.git`**（refs、index、stash、status 都必须原样保留）。
//!
//! 关键设计：
//!
//! - **私有 git dir**：`<state>/projects/<project_key>/repo.git`，所有命令显式传
//!   `--git-dir` / `--work-tree`；`GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` 指向
//!   `/dev/null`，作者/提交者身份用固定值，禁用 hooks/gpg/gc/fsmonitor。
//! - **per-workdir 状态**：ref `refs/kiana/checkpoints/<workdir_key>`、独立 index
//!   文件、独立 metadata JSON；多个 workdir 不会互相覆盖。
//! - **每 turn/目录最多一次**：[`CheckpointManager::ensure_checkpoint`] 按
//!   `(turn_id, workdir)` 去重；同一 turn 的后续 mutating tool 不会重复提交。
//! - **跳过规则**：root/home（及 home 的祖先）、`node_modules`/`target`/`.git`
//!   等目录、文件数/总大小超限、树未变化时不产生 commit。
//! - **原子 metadata**：临时文件 + rename，崩溃后最多留下一个孤儿临时文件。
//!
//! 参考设计（只借鉴行为，不复制代码）：
//! - `reference_project/gemini-cli/packages/core/src/services/gitService.ts` 的
//!   shadow repo + `getShadowRepoEnv`（隔离 global/system config、固定作者、禁用
//!   gpg）；
//! - `reference_project/hermes-agent/tools/checkpoint_manager.py` 的 per-workdir
//!   index/ref、`_too_broad`、`_MAX_FILES`、ledger 与安全恢复；
//! - `reference_project/hermes-agent/tools/checkpoint_maintenance.py` 用父目录
//!   `(st_dev, st_ino)` 区分“项目被删除”和“卷未挂载”。
//!
//! 已知边界：Windows 没有 `(dev, ino)` 证据，孤儿项目清理会保持保守；符号链接的
//! 恢复在 ledger 层被标记为 unsupported（不跟随、不覆盖）。

pub mod ledger;

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::subagent::result::sha256_hex;

pub use ledger::{
    AgentWriteEntry, AgentWriteLedger, DEFAULT_LEDGER_MAX_ENTRIES, DEFAULT_LEDGER_RETENTION_MS,
    LedgerError, LedgerOptions,
};

/// 会修改工作区的内置工具名（用于“每个 mutating tool 前 ensure_checkpoint”）。
///
/// `bash`/`powershell` 可能执行任意写操作，因此按 mutating 处理；只读工具
/// （read/grep/glob/ls 等）不触发 checkpoint。
pub const MUTATING_TOOL_NAMES: &[&str] = &[
    "write",
    "edit",
    "multiedit",
    "apply_patch",
    "patch",
    "str_replace",
    "create_file",
    "delete_file",
    "bash",
    "powershell",
];

/// 工具是否会修改工作区。
pub fn is_mutating_tool(tool_name: &str) -> bool {
    let tool_name = tool_name.to_ascii_lowercase();
    MUTATING_TOOL_NAMES.contains(&tool_name.as_str())
}

/// 每个 mutating tool 前的统一入口：非 mutating 工具直接返回 `None`。
pub fn ensure_checkpoint_before_tool(
    manager: &CheckpointManager,
    workdir: impl AsRef<Path>,
    turn_id: &str,
    tool_name: &str,
) -> Result<Option<CheckpointOutcome>, CheckpointError> {
    if !is_mutating_tool(tool_name) {
        return Ok(None);
    }
    manager.ensure_checkpoint(workdir, turn_id).map(Some)
}

/// checkpoint 配置。
#[derive(Debug, Clone)]
pub struct CheckpointOptions {
    /// 文件数上限；超过则跳过（默认 20,000）。
    pub max_files: usize,
    /// 总字节上限；超过则跳过（默认 256 MiB）。
    pub max_total_bytes: u64,
    /// 额外跳过的目录组件名。
    pub exclude_components: Vec<String>,
    /// ledger 配置。
    pub ledger: LedgerOptions,
}

impl Default for CheckpointOptions {
    fn default() -> Self {
        Self {
            max_files: 20_000,
            max_total_bytes: 256 * 1024 * 1024,
            exclude_components: Vec::new(),
            ledger: LedgerOptions::default(),
        }
    }
}

/// 跳过 checkpoint 的原因（不是错误）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointSkip {
    /// root / home / home 的祖先等过宽目录。
    TooBroad {
        /// 被跳过的目录。
        workdir: String,
    },
    /// 目录不存在或不是目录。
    NotADirectory {
        /// 被跳过的目录。
        workdir: String,
    },
    /// 文件数超限。
    TooManyFiles {
        /// 实际文件数。
        files: u64,
        /// 上限。
        limit: usize,
    },
    /// 总大小超限。
    TooLarge {
        /// 实际字节数。
        bytes: u64,
        /// 上限。
        limit: u64,
    },
}

/// `ensure_checkpoint` 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointOutcome {
    /// 新建了一个 commit。
    Created(CheckpointMetadata),
    /// 树与上一个 checkpoint 相同，没有产生新 commit。
    Unchanged(CheckpointMetadata),
    /// 本 turn 本目录已经检查过。
    AlreadyCheckedThisTurn {
        /// turn id。
        turn_id: String,
        /// workdir。
        workdir: String,
    },
    /// 按规则跳过。
    Skipped(CheckpointSkip),
}

/// checkpoint metadata（每个 workdir 一份）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointMetadata {
    /// 格式版本。
    pub version: u32,
    /// 规范化后的 workdir。
    pub workdir: String,
    /// workdir key（sha256 前 16 位）。
    pub workdir_key: String,
    /// 私有 ref 名。
    pub ref_name: String,
    /// 最新 commit。
    pub commit: String,
    /// 最新 tree。
    pub tree: String,
    /// 首次 checkpoint 时间（Unix 毫秒）。
    pub created_at_ms: u64,
    /// 最近一次 checkpoint/触达时间（Unix 毫秒）。
    pub last_touch_ms: u64,
    /// 累计 commit 数。
    pub snapshots: u64,
    /// workdir 父目录的 `st_dev`（用于区分删除与未挂载）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_dev: Option<u64>,
    /// workdir 父目录的 `st_ino`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_ino: Option<u64>,
}

/// checkpoint 错误。
#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    /// IO 错误。
    #[error("checkpoint io error: {0}")]
    Io(#[from] std::io::Error),
    /// git 命令失败。
    #[error("git {args} failed (status {status:?}): {stderr}")]
    Git {
        /// 命令行（已脱敏，不含环境变量）。
        args: String,
        /// 退出码。
        status: Option<i32>,
        /// stderr。
        stderr: String,
    },
    /// 目录不存在或不是目录。
    #[error("workdir is not a directory: {0}")]
    NotADirectory(String),
    /// 过宽目录。
    #[error("refusing to checkpoint a too-broad directory: {0}")]
    TooBroad(String),
    /// 没有 checkpoint。
    #[error("no checkpoint exists for workdir {0}")]
    NoCheckpoint(String),
    /// commit 格式非法。
    #[error("invalid checkpoint commit: {0}")]
    InvalidCommit(String),
    /// checkpoint 含嵌套 git repo / gitlink。
    #[error("checkpoint contains nested git repositories: {paths:?}")]
    NestedRepository {
        /// 被拒绝的路径。
        paths: Vec<String>,
    },
    /// 恢复中途失败。
    #[error("restore failed for {path}: {message} (rolled_back={rolled_back})")]
    RestoreFailed {
        /// 失败路径。
        path: String,
        /// 错误信息。
        message: String,
        /// 是否已回滚。
        rolled_back: bool,
    },
    /// metadata 损坏。
    #[error("invalid checkpoint metadata: {0}")]
    Metadata(String),
}

/// checkpoint 管理器。
#[derive(Debug)]
pub struct CheckpointManager {
    state_dir: PathBuf,
    options: CheckpointOptions,
    checked_turns: Mutex<HashSet<(String, String)>>,
    operation_lock: Mutex<()>,
}

impl CheckpointManager {
    /// 新建管理器（不创建目录；第一次 checkpoint 时懒创建）。
    pub fn new(state_dir: impl Into<PathBuf>, options: CheckpointOptions) -> Self {
        let state_dir = state_dir.into();
        // git 的 GIT_INDEX_FILE / excludesFile 必须是绝对路径（进程 cwd 不是 workdir），
        // 这里提前把相对 state_dir 固定到当前目录。
        let state_dir = if state_dir.is_absolute() {
            state_dir
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(&state_dir))
                .unwrap_or(state_dir)
        };
        Self {
            state_dir,
            options,
            checked_turns: Mutex::new(HashSet::new()),
            operation_lock: Mutex::new(()),
        }
    }

    /// state 根目录。
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// 每个 mutating tool 之前调用。同一 `(turn_id, workdir)` 最多做一次。
    pub fn ensure_checkpoint(
        &self,
        workdir: impl AsRef<Path>,
        turn_id: &str,
    ) -> Result<CheckpointOutcome, CheckpointError> {
        let workdir = canonical_workdir(workdir.as_ref())?;
        if let Some(reason) = self.too_broad_reason(&workdir) {
            return Ok(CheckpointOutcome::Skipped(reason));
        }
        let key = (turn_id.to_string(), workdir.to_string_lossy().into_owned());
        {
            let mut checked = self
                .checked_turns
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if checked.contains(&key) {
                return Ok(CheckpointOutcome::AlreadyCheckedThisTurn {
                    turn_id: key.0,
                    workdir: key.1,
                });
            }
            checked.insert(key);
        }
        match self.take(&workdir, "automatic checkpoint", true)? {
            TakeOutcome::Created(metadata) => Ok(CheckpointOutcome::Created(metadata)),
            TakeOutcome::Unchanged(metadata) => Ok(CheckpointOutcome::Unchanged(metadata)),
            TakeOutcome::Skipped(reason) => Ok(CheckpointOutcome::Skipped(reason)),
        }
    }

    /// 强制 checkpoint（restore 前的事务快照用），不做 turn 去重。
    pub fn take_checkpoint(
        &self,
        workdir: impl AsRef<Path>,
        reason: &str,
    ) -> Result<CheckpointMetadata, CheckpointError> {
        let workdir = canonical_workdir(workdir.as_ref())?;
        if let Some(reason) = self.too_broad_reason(&workdir) {
            return Err(match reason {
                CheckpointSkip::TooBroad { workdir } => CheckpointError::TooBroad(workdir),
                CheckpointSkip::NotADirectory { workdir } => {
                    CheckpointError::NotADirectory(workdir)
                }
                _ => CheckpointError::TooBroad(workdir.to_string_lossy().into_owned()),
            });
        }
        match self.take(&workdir, reason, true)? {
            TakeOutcome::Created(metadata) | TakeOutcome::Unchanged(metadata) => Ok(metadata),
            TakeOutcome::Skipped(CheckpointSkip::TooManyFiles { files, limit }) => {
                Err(CheckpointError::Metadata(format!(
                    "checkpoint skipped: {files} files exceed limit {limit}"
                )))
            }
            TakeOutcome::Skipped(CheckpointSkip::TooLarge { bytes, limit }) => {
                Err(CheckpointError::Metadata(format!(
                    "checkpoint skipped: {bytes} bytes exceed limit {limit}"
                )))
            }
            TakeOutcome::Skipped(CheckpointSkip::NotADirectory { workdir }) => {
                Err(CheckpointError::NotADirectory(workdir))
            }
            TakeOutcome::Skipped(CheckpointSkip::TooBroad { workdir }) => {
                Err(CheckpointError::TooBroad(workdir))
            }
        }
    }

    /// 读取当前 workdir 的最新 metadata。
    pub fn latest_checkpoint(
        &self,
        workdir: impl AsRef<Path>,
    ) -> Result<Option<CheckpointMetadata>, CheckpointError> {
        let workdir = canonical_workdir(workdir.as_ref())?;
        let paths = self.project_paths(&workdir);
        read_metadata(&paths.metadata)
    }

    /// 打开 workdir 的 agent-write ledger。
    pub fn ledger(&self, workdir: impl AsRef<Path>) -> Result<AgentWriteLedger, CheckpointError> {
        let workdir = canonical_workdir(workdir.as_ref())?;
        let paths = self.project_paths(&workdir);
        Ok(AgentWriteLedger::load(
            &workdir,
            paths.ledger,
            self.options.ledger.clone(),
        ))
    }

    /// 安全恢复。
    ///
    /// 只还原“当前内容 == agent 最后一次写入”的文件；用户手改过的文件进入
    /// `skipped_user_edits`。恢复前先做一次 `restore-transaction` checkpoint，并在内存
    /// 里保留每个目标文件的旧内容； [`RestoreReport::rollback`] 可撤销本次恢复。
    pub fn restore(
        &self,
        workdir: impl AsRef<Path>,
        commit: Option<&str>,
        ledger: &AgentWriteLedger,
    ) -> Result<RestoreReport, CheckpointError> {
        let workdir = canonical_workdir(workdir.as_ref())?;
        if let Some(reason) = self.too_broad_reason(&workdir) {
            return Err(match reason {
                CheckpointSkip::TooBroad { workdir } => CheckpointError::TooBroad(workdir),
                CheckpointSkip::NotADirectory { workdir } => {
                    CheckpointError::NotADirectory(workdir)
                }
                _ => CheckpointError::TooBroad(workdir.to_string_lossy().into_owned()),
            });
        }
        let ledger_workdir = ledger
            .workdir()
            .canonicalize()
            .unwrap_or_else(|_| ledger.workdir().to_path_buf());
        if ledger_workdir != workdir {
            return Err(CheckpointError::Metadata(format!(
                "agent-write ledger belongs to {}, not {}",
                ledger.workdir().to_string_lossy(),
                workdir.to_string_lossy()
            )));
        }
        let paths = self.project_paths(&workdir);
        let metadata = read_metadata(&paths.metadata)?
            .ok_or_else(|| CheckpointError::NoCheckpoint(workdir.to_string_lossy().into_owned()))?;
        let commit = match commit {
            Some(commit) => validate_commit(commit)?,
            None => metadata.commit.clone(),
        };

        // 审计：commit 必须真实存在且是 commit 对象。
        let git = GitRunner {
            manager: self,
            paths: &paths,
            workdir: &workdir,
        };
        let kind = git.run(&["cat-file", "-t", &commit])?;
        if String::from_utf8_lossy(&kind.stdout).trim() != "commit" {
            return Err(CheckpointError::InvalidCommit(commit));
        }

        // 拒绝含 gitlink 的 checkpoint（嵌套 repo 无法安全恢复）。
        let tree = git.run(&["ls-tree", "-r", "-z", &commit])?;
        let nested = nested_gitlinks(&tree.stdout);
        if !nested.is_empty() {
            return Err(CheckpointError::NestedRepository { paths: nested });
        }

        // 把当前工作树 stage 进私有 index，再与目标 commit 比对，得到变更集。
        git.stage_all()?;
        let diff = git.run(&["diff", "--name-only", "-z", &commit, "--cached"])?;
        let changed = split_nul(&diff.stdout);
        // index 回到 ref 指向的树，避免把“当前状态”留成下次 checkpoint 的基线。
        let _ = git.run(&["read-tree", &metadata.commit]);

        let current_nested = detect_current_nested_repos(&workdir, &changed);
        if !current_nested.is_empty() {
            return Err(CheckpointError::NestedRepository {
                paths: current_nested,
            });
        }

        // restore-transaction 快照：先给自己留后路。
        let rollback_commit = match self.take(&workdir, "restore-transaction snapshot", true)? {
            TakeOutcome::Created(metadata) | TakeOutcome::Unchanged(metadata) => {
                Some(metadata.commit)
            }
            TakeOutcome::Skipped(_) => None,
        };

        let mut report = RestoreReport {
            checkpoint: metadata.clone(),
            restored: Vec::new(),
            skipped_user_edits: Vec::new(),
            skipped_no_ledger: Vec::new(),
            skipped_unsupported: Vec::new(),
            refused_nested_repos: Vec::new(),
            rollback_commit: rollback_commit.clone(),
            transaction: RestoreTransaction {
                workdir: workdir.clone(),
                backups: Vec::new(),
                created_dirs: Vec::new(),
            },
        };

        // 分类：哪些文件可以安全还原。
        let mut to_restore = Vec::new();
        for relative in changed {
            if !is_safe_relative(&relative) {
                report.skipped_unsupported.push(relative);
                continue;
            }
            let Some(entry) = ledger.entry_by_relative(&relative) else {
                report.skipped_no_ledger.push(relative);
                continue;
            };
            let absolute = workdir.join(&relative);
            match fs::symlink_metadata(&absolute) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    report.skipped_unsupported.push(relative);
                }
                Ok(metadata) if metadata.is_dir() => {
                    report.skipped_unsupported.push(relative);
                }
                Ok(_metadata) => {
                    let bytes = fs::read(&absolute)?;
                    if sha256_hex(&bytes) == entry.sha256 {
                        to_restore.push(relative);
                    } else {
                        report.skipped_user_edits.push(relative);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // agent 写过后被用户删除：还原是安全的。
                    to_restore.push(relative);
                }
                Err(error) => return Err(CheckpointError::Io(error)),
            }
        }

        // 恢复前的内存快照。
        for relative in &to_restore {
            let absolute = workdir.join(relative);
            let backup = match fs::read(&absolute) {
                Ok(bytes) => FileBackup {
                    relative: relative.clone(),
                    existed: true,
                    bytes: Some(bytes),
                    mode: file_mode(&absolute),
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => FileBackup {
                    relative: relative.clone(),
                    existed: false,
                    bytes: None,
                    mode: None,
                },
                Err(error) => return Err(CheckpointError::Io(error)),
            };
            report.transaction.backups.push(backup);
        }

        for relative in to_restore {
            if let Err(error) = self.restore_one(&git, &commit, &workdir, &relative, &mut report) {
                let rolled_back = report.transaction.rollback().is_ok();
                return Err(CheckpointError::RestoreFailed {
                    path: relative,
                    message: error.to_string(),
                    rolled_back,
                });
            }
            report.restored.push(relative);
        }

        Ok(report)
    }

    /// 清理过期的 checkpoint 项目目录与 ledger 条目。
    ///
    /// 只有**能证明项目已被删除**（父目录仍在、且 `(dev, ino)` 与记录一致、父目录非空）
    /// 才删除项目目录；卷未挂载 / 父目录不可见时一律保留。活跃项目（workdir 仍存在）
    /// 永不删除，即使超过保留期。
    pub fn prune(&self, policy: &RetentionPolicy) -> Result<PruneReport, CheckpointError> {
        let now = now_millis();
        let projects_dir = self.state_dir.join("projects");
        let mut report = PruneReport::default();
        let entries = match fs::read_dir(&projects_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(report),
            Err(error) => return Err(CheckpointError::Io(error)),
        };

        struct Project {
            dir: PathBuf,
            metadata: Vec<CheckpointMetadata>,
            last_touch_ms: u64,
            size: u64,
            orphaned: bool,
            workdir_exists: bool,
        }

        let mut projects = Vec::new();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let dir = entry.path();
            let metas_dir = dir.join("meta");
            let mut metadata = Vec::new();
            if let Ok(files) = fs::read_dir(&metas_dir) {
                for file in files.flatten() {
                    if file.path().extension().and_then(|value| value.to_str()) != Some("json") {
                        continue;
                    }
                    if let Ok(Some(meta)) = read_metadata(&file.path()) {
                        metadata.push(meta);
                    }
                }
            }
            if metadata.is_empty() {
                continue;
            }
            let last_touch_ms = metadata
                .iter()
                .map(|meta| meta.last_touch_ms)
                .max()
                .unwrap_or(0);
            let workdir_exists = metadata
                .iter()
                .any(|meta| Path::new(&meta.workdir).is_dir());
            let orphaned = !workdir_exists && metadata.iter().any(project_orphaned);
            projects.push(Project {
                size: dir_size(&dir),
                dir,
                metadata,
                last_touch_ms,
                orphaned,
                workdir_exists,
            });
        }

        // 先做保留期清理。
        let mut remaining = Vec::new();
        for project in projects {
            let stale = policy.retention_ms == 0
                || now.saturating_sub(project.last_touch_ms) > policy.retention_ms;
            if !project.workdir_exists && stale {
                if project.orphaned {
                    report.reclaimed_bytes += project.size;
                    report
                        .removed_projects
                        .push(project.dir.to_string_lossy().into_owned());
                    let _ = fs::remove_dir_all(&project.dir);
                    continue;
                }
                report
                    .kept_unreachable
                    .push(project.dir.to_string_lossy().into_owned());
            }
            remaining.push(project);
        }

        // 再做总量上限：只删除“可证明已删除”的项目，活跃/未挂载项目不动。
        let mut total: u64 = remaining.iter().map(|project| project.size).sum();
        remaining.sort_by_key(|project| project.last_touch_ms);
        let mut kept = Vec::new();
        for project in remaining {
            if total > policy.max_total_bytes && project.orphaned && !project.workdir_exists {
                total = total.saturating_sub(project.size);
                report.reclaimed_bytes += project.size;
                report
                    .removed_projects
                    .push(project.dir.to_string_lossy().into_owned());
                let _ = fs::remove_dir_all(&project.dir);
            } else {
                kept.push(project);
            }
        }

        // 保留下来的项目：裁剪 ledger 条目。
        for project in kept {
            for meta in &project.metadata {
                let ledger_path = project
                    .dir
                    .join("ledger")
                    .join(format!("{}.json", meta.workdir_key));
                if !ledger_path.exists() {
                    continue;
                }
                let mut ledger =
                    AgentWriteLedger::load(&meta.workdir, ledger_path, self.options.ledger.clone());
                let removed = ledger.prune(now);
                report.removed_ledger_entries += removed;
                if removed > 0 {
                    let _ = ledger.save();
                }
            }
        }
        Ok(report)
    }

    fn restore_one(
        &self,
        git: &GitRunner<'_>,
        commit: &str,
        workdir: &Path,
        relative: &str,
        report: &mut RestoreReport,
    ) -> Result<(), CheckpointError> {
        let in_commit = git.run_allow_status(
            &["cat-file", "-e", &format!("{commit}:{relative}")],
            &[0, 1, 128],
        )?;
        let absolute = workdir.join(relative);
        if in_commit.status.success() {
            let blob = git.run(&["cat-file", "blob", &format!("{commit}:{relative}")])?;
            let mode = git.file_mode(commit, relative)?;
            ensure_parent(&absolute, &mut report.transaction.created_dirs)?;
            let temp = temp_path(&absolute);
            fs::write(&temp, &blob.stdout)?;
            if let Some(mode) = mode {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let permissions = if mode == 0o100755 {
                        fs::Permissions::from_mode(0o755)
                    } else {
                        fs::Permissions::from_mode(0o644)
                    };
                    fs::set_permissions(&temp, permissions)?;
                }
                #[cfg(not(unix))]
                {
                    let _ = mode;
                }
            }
            fs::rename(&temp, &absolute)?;
        } else {
            // checkpoint 里没有这个文件：agent 在 checkpoint 之后创建了它，删除即恢复。
            match fs::remove_file(&absolute) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(CheckpointError::Io(error)),
            }
        }
        Ok(())
    }

    fn take(
        &self,
        workdir: &Path,
        reason: &str,
        _force: bool,
    ) -> Result<TakeOutcome, CheckpointError> {
        let _lock = self
            .operation_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let paths = self.project_paths(workdir);
        match scan_tree(
            workdir,
            &self.options.exclude_components,
            &self.state_dir,
            self.options.max_files,
            self.options.max_total_bytes,
        )? {
            ScanOutcome::Ok => {}
            ScanOutcome::Skip(reason) => return Ok(TakeOutcome::Skipped(reason)),
        }
        fs::create_dir_all(&paths.root)?;
        fs::create_dir_all(paths.git_dir.join("refs").join("kiana").join("checkpoints"))?;
        fs::create_dir_all(paths.git_dir.join("objects"))?;
        if !paths.git_dir.join("HEAD").exists() {
            let mut command = Command::new("git");
            command
                .arg("init")
                .arg("--bare")
                .arg("--quiet")
                .arg(&paths.git_dir)
                .env("GIT_CONFIG_GLOBAL", null_config())
                .env("GIT_CONFIG_SYSTEM", null_config())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .env_remove("GIT_OBJECT_DIRECTORY")
                .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
                .env_remove("GIT_COMMON_DIR")
                .env_remove("GIT_NAMESPACE")
                .env_remove("GIT_CEILING_DIRECTORIES")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let output = command.output()?;
            if !output.status.success() {
                return Err(CheckpointError::Git {
                    args: "init --bare".to_string(),
                    status: output.status.code(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                });
            }
        }
        fs::write(&paths.exclude, self.excludes_content(workdir, &paths))?;
        fs::create_dir_all(&paths.index_dir)?;

        let previous = read_metadata(&paths.metadata)?;
        let git = GitRunner {
            manager: self,
            paths: &paths,
            workdir,
        };
        git.stage_all()?;
        let tree_output = git.run(&["write-tree"])?;
        let tree = String::from_utf8_lossy(&tree_output.stdout)
            .trim()
            .to_string();
        if tree.is_empty() {
            return Err(CheckpointError::Metadata(
                "git write-tree returned empty".to_string(),
            ));
        }

        let now = now_millis();
        let (parent_dev, parent_ino) = parent_identity(workdir);
        if let Some(previous) = previous.as_ref()
            && previous.tree == tree
        {
            let metadata = CheckpointMetadata {
                last_touch_ms: now,
                tree,
                ..previous.clone()
            };
            write_metadata(&paths.metadata, &metadata)?;
            return Ok(TakeOutcome::Unchanged(metadata));
        }

        let mut args: Vec<String> = vec!["commit-tree".to_string(), tree.clone()];
        if let Some(previous) = previous.as_ref() {
            args.push("-p".to_string());
            args.push(previous.commit.clone());
        }
        args.push("-m".to_string());
        args.push(format!("kiana checkpoint: {}", sanitize_message(reason)));
        let commit_output = git.run_owned(&args)?;
        let commit = String::from_utf8_lossy(&commit_output.stdout)
            .trim()
            .to_string();
        if commit.is_empty() {
            return Err(CheckpointError::Metadata(
                "git commit-tree returned empty".to_string(),
            ));
        }
        // update-ref：有旧值就做 compare-and-swap，避免并发覆盖。
        match previous.as_ref() {
            Some(previous) => {
                git.run(&["update-ref", &paths.ref_name, &commit, &previous.commit])?;
            }
            None => {
                git.run(&["update-ref", &paths.ref_name, &commit])?;
            }
        }

        let metadata = CheckpointMetadata {
            version: 1,
            workdir: workdir.to_string_lossy().into_owned(),
            workdir_key: paths.workdir_key.clone(),
            ref_name: paths.ref_name.clone(),
            commit,
            tree,
            created_at_ms: previous
                .as_ref()
                .map(|previous| previous.created_at_ms)
                .unwrap_or(now),
            last_touch_ms: now,
            snapshots: previous
                .as_ref()
                .map(|previous| previous.snapshots.saturating_add(1))
                .unwrap_or(1),
            parent_dev,
            parent_ino,
        };
        write_metadata(&paths.metadata, &metadata)?;
        Ok(TakeOutcome::Created(metadata))
    }

    fn too_broad_reason(&self, workdir: &Path) -> Option<CheckpointSkip> {
        let workdir_string = workdir.to_string_lossy().into_owned();
        if workdir == Path::new("/") {
            return Some(CheckpointSkip::TooBroad {
                workdir: workdir_string,
            });
        }
        if let Some(home) = home_dir()
            && (workdir == home || home.starts_with(workdir))
        {
            return Some(CheckpointSkip::TooBroad {
                workdir: workdir_string,
            });
        }
        if workdir == self.state_dir
            || self.state_dir.starts_with(workdir) && !self.state_dir.as_os_str().is_empty()
        {
            return Some(CheckpointSkip::TooBroad {
                workdir: workdir_string,
            });
        }
        None
    }

    fn excludes_content(&self, workdir: &Path, paths: &ProjectPaths) -> String {
        let mut lines: Vec<String> = vec![
            ".git/".to_string(),
            "node_modules/".to_string(),
            "target/".to_string(),
            ".kiana/".to_string(),
        ];
        for component in &self.options.exclude_components {
            let component = component.trim_matches('/');
            if !component.is_empty() {
                lines.push(format!("{component}/"));
            }
        }
        if let Ok(relative) = paths.root.strip_prefix(workdir) {
            let relative = relative.to_string_lossy().replace('\\', "/");
            if !relative.is_empty() {
                lines.push(format!("/{relative}/"));
            }
        }
        let mut content = lines.join("\n");
        content.push('\n');
        content
    }

    fn project_paths(&self, workdir: &Path) -> ProjectPaths {
        let project_key = short_hash(&workdir.to_string_lossy());
        let workdir_key = project_key.clone();
        let root = self.state_dir.join("projects").join(&project_key);
        let git_dir = root.join("repo.git");
        ProjectPaths {
            git_dir,
            index: root.join("index").join(&workdir_key),
            index_dir: root.join("index"),
            metadata: root.join("meta").join(format!("{workdir_key}.json")),
            ledger: root.join("ledger").join(format!("{workdir_key}.json")),
            exclude: root.join("exclude"),
            ref_name: format!("refs/kiana/checkpoints/{workdir_key}"),
            root,
            workdir_key,
        }
    }
}

/// 保留策略。
#[derive(Debug, Clone, Copy)]
pub struct RetentionPolicy {
    /// 保留期（Unix 毫秒）。
    pub retention_ms: u64,
    /// checkpoint 存储总量上限（字节）。
    pub max_total_bytes: u64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            retention_ms: 30 * 24 * 60 * 60 * 1000,
            max_total_bytes: 1024 * 1024 * 1024,
        }
    }
}

/// prune 报告。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// 被删除的项目目录。
    pub removed_projects: Vec<String>,
    /// 因卷未挂载/无法证明删除而保留的项目。
    pub kept_unreachable: Vec<String>,
    /// 被裁剪的 ledger 条目数。
    pub removed_ledger_entries: usize,
    /// 回收的字节数。
    pub reclaimed_bytes: u64,
}

/// 恢复报告。
#[derive(Debug)]
pub struct RestoreReport {
    /// 恢复到的 checkpoint。
    pub checkpoint: CheckpointMetadata,
    /// 成功还原的相对路径。
    pub restored: Vec<String>,
    /// 因用户手改而跳过的路径。
    pub skipped_user_edits: Vec<String>,
    /// 因 ledger 没有记录而跳过的路径。
    pub skipped_no_ledger: Vec<String>,
    /// 因类型（符号链接/目录）不支持的路径。
    pub skipped_unsupported: Vec<String>,
    /// 被拒绝的嵌套 repo（成功恢复时为空）。
    pub refused_nested_repos: Vec<String>,
    /// restore 前快照的 commit。
    pub rollback_commit: Option<String>,
    /// 可回滚事务。
    pub transaction: RestoreTransaction,
}

impl RestoreReport {
    /// 撤销本次恢复（把文件写回恢复前的内容）。
    pub fn rollback(self) -> Result<RollbackReport, CheckpointError> {
        self.transaction.rollback()
    }
}

/// 恢复前快照 + 内存备份。
#[derive(Debug)]
pub struct RestoreTransaction {
    workdir: PathBuf,
    backups: Vec<FileBackup>,
    created_dirs: Vec<PathBuf>,
}

impl RestoreTransaction {
    /// 回滚到恢复前。
    pub fn rollback(mut self) -> Result<RollbackReport, CheckpointError> {
        let mut report = RollbackReport::default();
        for backup in self.backups.drain(..) {
            let absolute = self.workdir.join(&backup.relative);
            match backup.bytes {
                Some(bytes) => {
                    if let Some(parent) = absolute.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    let temp = temp_path(&absolute);
                    fs::write(&temp, bytes)?;
                    if let Some(mode) = backup.mode {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            let _ = fs::set_permissions(&temp, fs::Permissions::from_mode(mode));
                        }
                        #[cfg(not(unix))]
                        {
                            let _ = mode;
                        }
                    }
                    fs::rename(&temp, &absolute)?;
                    report.restored.push(backup.relative);
                }
                None => {
                    if backup.existed {
                        // existed 但 bytes 为 None 不应发生；保守地跳过。
                        continue;
                    }
                    match fs::remove_file(&absolute) {
                        Ok(()) => report.removed.push(backup.relative),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(CheckpointError::Io(error)),
                    }
                }
            }
        }
        for dir in self.created_dirs.into_iter().rev() {
            let _ = fs::remove_dir(&dir);
        }
        Ok(report)
    }
}

/// 回滚报告。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RollbackReport {
    /// 被写回的文件。
    pub restored: Vec<String>,
    /// 被删除的文件（恢复时新建的）。
    pub removed: Vec<String>,
}

#[derive(Debug, Clone)]
struct FileBackup {
    relative: String,
    existed: bool,
    bytes: Option<Vec<u8>>,
    mode: Option<u32>,
}

enum TakeOutcome {
    Created(CheckpointMetadata),
    Unchanged(CheckpointMetadata),
    Skipped(CheckpointSkip),
}

#[derive(Debug)]
struct ProjectPaths {
    root: PathBuf,
    git_dir: PathBuf,
    index: PathBuf,
    index_dir: PathBuf,
    metadata: PathBuf,
    ledger: PathBuf,
    exclude: PathBuf,
    ref_name: String,
    workdir_key: String,
}

struct GitRunner<'a> {
    manager: &'a CheckpointManager,
    paths: &'a ProjectPaths,
    workdir: &'a Path,
}

impl GitRunner<'_> {
    fn run(&self, args: &[&str]) -> Result<Output, CheckpointError> {
        let output = self.run_allow_status(args, &[0])?;
        Ok(output)
    }

    fn run_owned(&self, args: &[String]) -> Result<Output, CheckpointError> {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.run(&args)
    }

    fn run_allow_status(&self, args: &[&str], allowed: &[i32]) -> Result<Output, CheckpointError> {
        let output = self.command(args).output()?;
        let status = output.status.code();
        if allowed.contains(&status.unwrap_or(-1)) {
            return Ok(output);
        }
        Err(CheckpointError::Git {
            args: args.join(" "),
            status,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new("git");
        command
            .arg("--git-dir")
            .arg(&self.paths.git_dir)
            .arg("--work-tree")
            .arg(self.workdir)
            .arg("-c")
            .arg(format!(
                "core.excludesFile={}",
                self.paths.exclude.to_string_lossy()
            ))
            .arg("-c")
            .arg("core.hooksPath=/dev/null")
            .arg("-c")
            .arg("commit.gpgsign=false")
            .arg("-c")
            .arg("tag.gpgsign=false")
            .arg("-c")
            .arg("gc.auto=0")
            .arg("-c")
            .arg("maintenance.auto=false")
            .arg("-c")
            .arg("core.untrackedCache=false")
            .arg("-c")
            .arg("core.fsmonitor=false")
            .arg("-c")
            .arg("advice.addEmbeddedRepo=false")
            .args(args)
            .env("GIT_DIR", &self.paths.git_dir)
            .env("GIT_WORK_TREE", self.workdir)
            .env("GIT_INDEX_FILE", &self.paths.index)
            .env("GIT_CONFIG_GLOBAL", null_config())
            .env("GIT_CONFIG_SYSTEM", null_config())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_AUTHOR_NAME", "Kiana Checkpoint")
            .env("GIT_AUTHOR_EMAIL", "checkpoint@kiana.local")
            .env("GIT_COMMITTER_NAME", "Kiana Checkpoint")
            .env("GIT_COMMITTER_EMAIL", "checkpoint@kiana.local")
            .env("LC_ALL", "C")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_NAMESPACE")
            .env_remove("GIT_CEILING_DIRECTORIES")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn stage_all(&self) -> Result<(), CheckpointError> {
        let mut args: Vec<String> = vec!["add".to_string(), "-A".to_string()];
        if let Some(exclude) = self.state_exclude_pathspec() {
            args.push("--".to_string());
            args.push(".".to_string());
            args.push(exclude);
        }
        self.run_owned(&args)?;
        Ok(())
    }

    fn state_exclude_pathspec(&self) -> Option<String> {
        let relative = self.manager.state_dir.strip_prefix(self.workdir).ok()?;
        let relative = relative.to_string_lossy().replace('\\', "/");
        if relative.is_empty() {
            return None;
        }
        Some(format!(":(exclude,literal){relative}"))
    }

    fn file_mode(&self, commit: &str, relative: &str) -> Result<Option<u32>, CheckpointError> {
        let literal = format!(":(literal){relative}");
        let output = self.run(&["ls-tree", "-z", commit, "--", &literal])?;
        let text = String::from_utf8_lossy(&output.stdout);
        let first = text.split('\u{0}').next().unwrap_or_default();
        let Some((metadata, _path)) = first.split_once('\t') else {
            return Ok(None);
        };
        let mode = metadata.split_whitespace().next().unwrap_or_default();
        Ok(u32::from_str_radix(mode, 8).ok())
    }
}

enum ScanOutcome {
    Ok,
    Skip(CheckpointSkip),
}

fn scan_tree(
    workdir: &Path,
    extra_excludes: &[String],
    state_dir: &Path,
    max_files: usize,
    max_bytes: u64,
) -> Result<ScanOutcome, CheckpointError> {
    let excludes: HashSet<&str> = HashSet::from(["node_modules", "target", ".git", ".kiana"]);
    let extra: HashSet<String> = extra_excludes
        .iter()
        .map(|component| component.trim_matches('/').to_string())
        .collect();
    let mut stack = vec![workdir.to_path_buf()];
    let mut files = 0u64;
    let mut bytes = 0u64;
    while let Some(directory) = stack.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.starts_with(state_dir) {
                continue;
            }
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if metadata.is_dir() {
                if excludes.contains(name.as_ref()) || extra.contains(name.as_ref()) {
                    continue;
                }
                stack.push(path);
                continue;
            }
            files += 1;
            bytes = bytes.saturating_add(if metadata.file_type().is_symlink() {
                0
            } else {
                metadata.len()
            });
            if files > max_files as u64 {
                return Ok(ScanOutcome::Skip(CheckpointSkip::TooManyFiles {
                    files,
                    limit: max_files,
                }));
            }
            if bytes > max_bytes {
                return Ok(ScanOutcome::Skip(CheckpointSkip::TooLarge {
                    bytes,
                    limit: max_bytes,
                }));
            }
        }
    }
    let _ = (files, bytes);
    Ok(ScanOutcome::Ok)
}

fn canonical_workdir(workdir: &Path) -> Result<PathBuf, CheckpointError> {
    let canonical = workdir.canonicalize().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CheckpointError::NotADirectory(workdir.to_string_lossy().into_owned())
        } else {
            CheckpointError::Io(error)
        }
    })?;
    if !canonical.is_dir() {
        return Err(CheckpointError::NotADirectory(
            workdir.to_string_lossy().into_owned(),
        ));
    }
    Ok(canonical)
}

fn read_metadata(path: &Path) -> Result<Option<CheckpointMetadata>, CheckpointError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| CheckpointError::Metadata(error.to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(CheckpointError::Io(error)),
    }
}

fn write_metadata(path: &Path, metadata: &CheckpointMetadata) -> Result<(), CheckpointError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(metadata)
        .map_err(|error| CheckpointError::Metadata(error.to_string()))?;
    let temp = temp_path(path);
    fs::write(&temp, bytes)?;
    fs::rename(&temp, path)?;
    Ok(())
}

fn validate_commit(commit: &str) -> Result<String, CheckpointError> {
    if commit.is_empty()
        || commit.starts_with('-')
        || commit.len() > 64
        || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(CheckpointError::InvalidCommit(commit.to_string()));
    }
    Ok(commit.to_string())
}

fn nested_gitlinks(ls_tree: &[u8]) -> Vec<String> {
    let mut paths = Vec::new();
    for record in ls_tree.split(|byte| *byte == 0) {
        if record.is_empty() {
            continue;
        }
        let record = String::from_utf8_lossy(record);
        let Some((metadata, path)) = record.split_once('\t') else {
            continue;
        };
        let mut fields = metadata.split_whitespace();
        let mode = fields.next().unwrap_or_default();
        let kind = fields.next().unwrap_or_default();
        if mode == "160000" || kind == "commit" {
            paths.push(path.to_string());
        }
    }
    paths
}

fn detect_current_nested_repos(workdir: &Path, paths: &[String]) -> Vec<String> {
    let mut nested = Vec::new();
    for relative in paths {
        let absolute = workdir.join(relative);
        let mut current = absolute.parent();
        while let Some(directory) = current {
            if directory == workdir {
                break;
            }
            if directory.join(".git").exists() {
                nested.push(relative.clone());
                break;
            }
            current = directory.parent();
        }
        if absolute.join(".git").exists() {
            nested.push(relative.clone());
        }
    }
    nested.sort();
    nested.dedup();
    nested
}

fn split_nul(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .map(|record| String::from_utf8_lossy(record).into_owned())
        .collect()
}

fn is_safe_relative(relative: &str) -> bool {
    let path = Path::new(relative);
    if path.is_absolute() {
        return false;
    }
    path.components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn ensure_parent(path: &Path, created: &mut Vec<PathBuf>) -> Result<(), CheckpointError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.exists() {
        return Ok(());
    }
    let mut missing = Vec::new();
    let mut current = Some(parent);
    while let Some(directory) = current {
        if directory.exists() {
            break;
        }
        missing.push(directory.to_path_buf());
        current = directory.parent();
    }
    fs::create_dir_all(parent)?;
    created.extend(missing);
    Ok(())
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".kiana-tmp.{}", std::process::id()));
    path.with_file_name(name)
}

#[cfg(unix)]
fn file_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions().mode())
}

#[cfg(not(unix))]
fn file_mode(_path: &Path) -> Option<u32> {
    None
}

#[cfg(unix)]
fn parent_identity(workdir: &Path) -> (Option<u64>, Option<u64>) {
    use std::os::unix::fs::MetadataExt;
    match workdir.parent().and_then(|parent| parent.metadata().ok()) {
        Some(metadata) if metadata.dev() != 0 && metadata.ino() != 0 => {
            (Some(metadata.dev()), Some(metadata.ino()))
        }
        _ => (None, None),
    }
}

#[cfg(not(unix))]
fn parent_identity(_workdir: &Path) -> (Option<u64>, Option<u64>) {
    (None, None)
}

fn project_orphaned(metadata: &CheckpointMetadata) -> bool {
    let workdir = Path::new(&metadata.workdir);
    if workdir.exists() {
        return false;
    }
    let Some(parent) = workdir.parent() else {
        return false;
    };
    if parent == workdir || !parent.is_dir() {
        return false;
    }
    match (metadata.parent_dev, metadata.parent_ino) {
        (Some(dev), Some(ino)) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let Ok(stat) = parent.metadata() else {
                    return false;
                };
                if (stat.dev(), stat.ino()) != (dev, ino) {
                    return false;
                }
            }
            #[cfg(not(unix))]
            {
                let _ = (dev, ino);
            }
        }
        _ => return false,
    }
    // 空父目录更可能是“未挂载留下的挂载点”，保守地不判定为删除。
    fs::read_dir(parent)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    total
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|home| home.canonicalize().ok())
}

fn short_hash(value: &str) -> String {
    let hash = sha256_hex(value.as_bytes());
    hash[..16].to_string()
}

fn null_config() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

fn sanitize_message(reason: &str) -> String {
    reason
        .chars()
        .map(|character| {
            if character == '\n' || character == '\r' {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
