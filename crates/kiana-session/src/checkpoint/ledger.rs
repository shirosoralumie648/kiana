//! agent-write ledger：记录 agent 最后一次写入每个文件的内容哈希。
//!
//! 安全恢复的前提是能区分“agent 写的文件”和“用户手改过的文件”。每次 mutating
//! tool 成功执行后，调用方用 [`AgentWriteLedger::record_agent_write`] 记下
//! `(相对路径, sha256)`；恢复时只有当**当前内容仍等于 agent 最后一次写入**时才还原，
//! 否则把文件列进 `skipped_user_edits`，绝不覆盖用户的修改。
//!
//! 参考设计（只借鉴行为，不复制代码）：
//! - `reference_project/hermes-agent/tools/checkpoint_manager.py` 的
//!   `record_agent_write`、`_load_ledger`、`_save_ledger`、`_safe_restore_plan`
//!   （ledger 为 best-effort、按路径存 sha256、保存时按时间裁剪）；
//! - `reference_project/hermes-agent/tools/checkpoint_pruning.py` 的保留期/总量上限
//!   思路（本模块只负责条目裁剪，项目目录清理在 `checkpoint` 模块）。
//!
//! 设计约束：
//! - 写入失败（权限、磁盘满）只让 `save` 返回错误，不 panic；ledger 是辅助证据，
//!   不能阻断 agent 主流程。
//! - 路径统一存成 workdir 相对、`/` 分隔的形式，避免绝对路径泄漏与跨平台差异。
//! - 损坏的 ledger 文件按“空 ledger”处理：保守地跳过所有恢复目标，而不是误删。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::subagent::result::sha256_hex;

/// 默认保留期：30 天。
pub const DEFAULT_LEDGER_RETENTION_MS: u64 = 30 * 24 * 60 * 60 * 1000;
/// 默认最多保留的条目数。
pub const DEFAULT_LEDGER_MAX_ENTRIES: usize = 10_000;

/// ledger 配置。
#[derive(Debug, Clone)]
pub struct LedgerOptions {
    /// 超过保留期的条目在保存时被删除。
    pub retention_ms: u64,
    /// 条目数上限（按记录时间保留最新的）。
    pub max_entries: usize,
}

impl Default for LedgerOptions {
    fn default() -> Self {
        Self {
            retention_ms: DEFAULT_LEDGER_RETENTION_MS,
            max_entries: DEFAULT_LEDGER_MAX_ENTRIES,
        }
    }
}

/// 一次 agent 写入的记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentWriteEntry {
    /// 写入内容的 sha256（小写 hex）。
    pub sha256: String,
    /// 记录时间（Unix 毫秒）。
    pub recorded_at_ms: u64,
    /// 写入时的文件字节数。
    pub bytes: u64,
}

/// ledger 错误。
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    /// IO 错误。
    #[error("agent-write ledger io error: {0}")]
    Io(#[from] std::io::Error),
    /// 路径不在 workdir 内。
    #[error("path {path} is outside the workdir {workdir}")]
    OutsideWorkdir {
        /// 被记录的路径。
        path: String,
        /// 工作目录。
        workdir: String,
    },
    /// 哈希格式不是 64 位小写 hex。
    #[error("invalid sha256 for {path}: {value}")]
    InvalidHash {
        /// 被记录的路径。
        path: String,
        /// 原始值。
        value: String,
    },
    /// ledger 的 workdir 与调用方不一致。
    #[error("ledger workdir mismatch: expected {expected}, got {actual}")]
    WorkdirMismatch {
        /// 期望值。
        expected: String,
        /// 文件里的值。
        actual: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LedgerFile {
    version: u32,
    workdir: String,
    entries: BTreeMap<String, AgentWriteEntry>,
}

/// per-workdir 的 agent 写入账本。
#[derive(Debug)]
pub struct AgentWriteLedger {
    workdir: PathBuf,
    path: PathBuf,
    entries: BTreeMap<String, AgentWriteEntry>,
    options: LedgerOptions,
}

impl AgentWriteLedger {
    /// 新建（不读取磁盘）。
    pub fn new(
        workdir: impl AsRef<Path>,
        path: impl Into<PathBuf>,
        options: LedgerOptions,
    ) -> Self {
        Self {
            workdir: workdir.as_ref().to_path_buf(),
            path: path.into(),
            entries: BTreeMap::new(),
            options,
        }
    }

    /// 从磁盘加载；文件不存在或损坏时返回空 ledger（损坏不阻断启动）。
    pub fn load(
        workdir: impl AsRef<Path>,
        path: impl Into<PathBuf>,
        options: LedgerOptions,
    ) -> Self {
        let workdir = workdir.as_ref().to_path_buf();
        let path = path.into();
        let entries = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<LedgerFile>(&bytes) {
                Ok(file) if file.version == 1 => {
                    // workdir 不一致说明 ledger 被错放/项目路径变了；保守地当成空账本。
                    let expected = workdir.to_string_lossy();
                    if file.workdir == expected || file.workdir.is_empty() {
                        file.entries
                    } else {
                        BTreeMap::new()
                    }
                }
                _ => BTreeMap::new(),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(_) => BTreeMap::new(),
        };
        Self {
            workdir,
            path,
            entries,
            options,
        }
    }

    /// 记录 agent 对 `path` 的一次写入。
    ///
    /// `path` 可以是绝对路径（必须在 workdir 内）或 workdir 相对路径。返回规范化后的
    /// 相对路径。哈希必须是 64 位 hex；调用方通常传 [`sha256_hex`] 的结果。
    pub fn record_agent_write(
        &mut self,
        path: impl AsRef<Path>,
        sha256: &str,
    ) -> Result<String, LedgerError> {
        let relative = normalize_relative(&self.workdir, path.as_ref())?;
        let normalized_hash = normalize_hash(sha256).ok_or_else(|| LedgerError::InvalidHash {
            path: relative.clone(),
            value: sha256.to_string(),
        })?;
        let bytes = fs::metadata(self.workdir.join(&relative))
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        self.entries.insert(
            relative.clone(),
            AgentWriteEntry {
                sha256: normalized_hash,
                recorded_at_ms: now_millis(),
                bytes,
            },
        );
        Ok(relative)
    }

    /// 读取文件内容、计算 sha256 并记录（文件不存在/不可读返回 `Ok(None)`）。
    pub fn record_agent_write_file(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<Option<String>, LedgerError> {
        let path = path.as_ref();
        let relative = normalize_relative(&self.workdir, path)?;
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workdir.join(path)
        };
        let bytes = match fs::read(&absolute) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(LedgerError::Io(error)),
        };
        let sha256 = sha256_hex(&bytes);
        self.entries.insert(
            relative.clone(),
            AgentWriteEntry {
                sha256,
                recorded_at_ms: now_millis(),
                bytes: bytes.len() as u64,
            },
        );
        Ok(Some(relative))
    }

    /// 查询某路径最后一次 agent 写入。
    pub fn entry(&self, path: impl AsRef<Path>) -> Option<&AgentWriteEntry> {
        let relative = normalize_relative(&self.workdir, path.as_ref()).ok()?;
        self.entries.get(&relative)
    }

    /// 查询已规范化的相对路径。
    pub fn entry_by_relative(&self, relative: &str) -> Option<&AgentWriteEntry> {
        self.entries.get(relative)
    }

    /// 删除一条记录。
    pub fn remove(&mut self, relative: &str) -> Option<AgentWriteEntry> {
        self.entries.remove(relative)
    }

    /// 当前全部条目。
    pub fn entries(&self) -> &BTreeMap<String, AgentWriteEntry> {
        &self.entries
    }

    /// 条目数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// workdir。
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// 按保留期/条目数上限裁剪，返回被删除的条目数。
    pub fn prune(&mut self, now_ms: u64) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, entry| {
            self.options.retention_ms == 0
                || now_ms.saturating_sub(entry.recorded_at_ms) <= self.options.retention_ms
        });
        if self.entries.len() > self.options.max_entries {
            let mut ordered: Vec<(String, u64)> = self
                .entries
                .iter()
                .map(|(path, entry)| (path.clone(), entry.recorded_at_ms))
                .collect();
            ordered.sort_by_key(|(_, recorded_at)| *recorded_at);
            let excess = self.entries.len() - self.options.max_entries;
            for (path, _) in ordered.into_iter().take(excess) {
                self.entries.remove(&path);
            }
        }
        before - self.entries.len()
    }

    /// 持久化（原子写：临时文件 + rename）。
    pub fn save(&mut self) -> Result<(), LedgerError> {
        self.prune(now_millis());
        let file = LedgerFile {
            version: 1,
            workdir: self.workdir.to_string_lossy().into_owned(),
            entries: self.entries.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|error| {
            LedgerError::Io(std::io::Error::other(format!(
                "serialize agent-write ledger: {error}"
            )))
        })?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = temp_path(&self.path);
        fs::write(&temp, &bytes)?;
        fs::rename(&temp, &self.path)?;
        Ok(())
    }

    /// 当前账本文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// 把路径规范化为 workdir 相对、`/` 分隔的形式。
pub fn normalize_relative(workdir: &Path, path: &Path) -> Result<String, LedgerError> {
    let relative: PathBuf = if path.is_absolute() {
        let workdir_canonical = workdir
            .canonicalize()
            .unwrap_or_else(|_| workdir.to_path_buf());
        let path_canonical = path
            .parent()
            .and_then(|parent| parent.canonicalize().ok())
            .map(|parent| match path.file_name() {
                Some(name) => parent.join(name),
                None => parent,
            })
            .unwrap_or_else(|| path.to_path_buf());
        path_canonical
            .strip_prefix(&workdir_canonical)
            .or_else(|_| path.strip_prefix(workdir))
            .map(Path::to_path_buf)
            .map_err(|_| LedgerError::OutsideWorkdir {
                path: path.to_string_lossy().into_owned(),
                workdir: workdir.to_string_lossy().into_owned(),
            })?
    } else {
        path.to_path_buf()
    };

    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(LedgerError::OutsideWorkdir {
                    path: path.to_string_lossy().into_owned(),
                    workdir: workdir.to_string_lossy().into_owned(),
                });
            }
        }
    }
    if parts.is_empty() {
        return Err(LedgerError::OutsideWorkdir {
            path: path.to_string_lossy().into_owned(),
            workdir: workdir.to_string_lossy().into_owned(),
        });
    }
    Ok(parts.join("/"))
}

fn normalize_hash(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(value)
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp.{}", std::process::id()));
    path.with_file_name(name)
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
