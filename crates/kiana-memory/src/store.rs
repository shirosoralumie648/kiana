//! 作用域存储：索引 + 主题文件的读、写、扫描、墓碑。
//!
//! 所有写入都经过同一把跨进程 [`FileLock`]，并遵守以下不变量：
//!
//! 1. **先读后写**：在锁内重新解析索引；revision 不匹配返回
//!    [`MemoryError::RevisionConflict`]，不写任何字节。
//! 2. **批次原子校验**：`add/replace/remove/patch` 全部在内存里应用并校验通过后，才写
//!    第一个文件；任一操作失败，磁盘不变。
//! 3. **temp+rename**：每个文件先写同目录临时文件、`fsync`、再 `rename`；索引最后写，
//!    失败时至少索引仍指向上一版。
//! 4. **损坏拒绝覆盖**：现有文件不能往返解析时，先复制为 `<file>.bak`，返回
//!    `CorruptIndex` / `CorruptEntry`；原文件保持原样。
//! 5. **有界**：正文总量超过 `limits.max_total_bytes` 时拒绝批次并返回使用量。
//!
//! 本模块的写 API 是 `pub(crate)`；外部调用方必须经 [`crate::service::MemoryService`]
//! 通过权限 gate 并写审计。

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::{MemoryConfig, MemoryScope, ScopePaths};
use crate::entry::{
    MemoryEntry, MemoryEntryDraft, MemoryHeader, MemoryType, content_digest, is_valid_id, slugify,
};
use crate::error::{MemoryError, MemoryResult, io_error};
use crate::index::{
    IndexEntry, IndexParseError, ParsedIndex, corrupt_index_error, parse_index, render_index,
};
use crate::lock::FileLock;

/// 索引文件名。
pub const INDEX_FILE_NAME: &str = "MEMORY.md";
/// 摘要文件名（不参与召回扫描）。
pub const SUMMARY_FILE_NAME: &str = "memory_summary.md";
/// tombstone 格式版本。
pub const TOMBSTONE_FORMAT_VERSION: u32 = 1;
/// 摘要格式版本。
pub const SUMMARY_FORMAT_VERSION: u32 = 1;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 索引快照（system prompt 只读它）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreSnapshot {
    /// scope。
    pub scope: MemoryScope,
    /// 索引 revision。
    pub revision: u64,
    /// 已按上限截断的索引内容。
    pub index: String,
    /// 索引中的条目。
    pub entries: Vec<IndexEntry>,
    /// 截断警告（0 或 1 条）。
    pub warnings: Vec<String>,
    /// 是否发生截断。
    pub truncated: bool,
}

/// manifest 扫描错误（坏文件被跳过，不影响其他条目）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanError {
    /// 文件路径。
    pub path: PathBuf,
    /// 错误说明。
    pub message: String,
}

/// 召回 manifest 的一条：**只含 frontmatter**，不包含正文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestEntry {
    /// 条目 id。
    pub id: String,
    /// 显示名称。
    pub name: String,
    /// 一行描述。
    pub description: String,
    /// 类型（缺失/未知为 `None`）。
    pub mtype: Option<MemoryType>,
    /// 条目 revision。
    pub revision: u64,
    /// 派生来源。
    pub derived_from: Vec<String>,
    /// 抽取幂等键。
    pub source_key: Option<String>,
    /// 绝对路径。
    pub path: PathBuf,
    /// 相对 scope 根的路径（`topics/<id>.md`）。
    pub relative_path: String,
    /// 扫描时的 mtime；注入前复查必须匹配。
    pub mtime: SystemTime,
    /// mtime 的 Unix 毫秒（便于序列化/比较）。
    pub mtime_ms: u64,
    /// 距今天数（floor）。
    pub age_days: u64,
}

/// 扫描结果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    /// 最新优先的条目（最多 `scan_max_files`）。
    pub entries: Vec<ManifestEntry>,
    /// 磁盘上的 markdown 文件总数（截断前）。
    pub scanned_files: usize,
    /// 是否因为文件数上限截断。
    pub truncated: bool,
    /// 被跳过的坏文件。
    pub errors: Vec<ScanError>,
}

/// 单次写入操作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteOperation {
    /// 新增条目。
    Add(MemoryEntryDraft),
    /// 完整替换（保留 id，revision +1）。
    Replace {
        /// 目标条目 id。
        id: String,
        /// 新内容。
        entry: MemoryEntryDraft,
    },
    /// 仅在当前条目规范字节的摘要匹配时替换（审批的 TOCTOU-safe 应用）。
    ReplaceIfUnchanged {
        /// 目标条目 id。
        id: String,
        /// 审批时 pin 的完整条目字节摘要（sha256）。
        expected_entry_digest: String,
        /// 新内容。
        entry: MemoryEntryDraft,
    },
    /// 删除条目。
    Remove {
        /// 目标条目 id。
        id: String,
    },
    /// 替换正文中的一个唯一片段。
    Patch {
        /// 目标条目 id。
        id: String,
        /// 旧片段（必须唯一）。
        old_text: String,
        /// 新片段。
        new_text: String,
    },
}

/// 一批写入；`expected_revision` 提供乐观并发控制。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WriteBatch {
    /// 调用方读到的 revision；`None` 表示不做检查。
    pub expected_revision: Option<u64>,
    /// 操作按顺序应用。
    pub operations: Vec<WriteOperation>,
}

impl WriteBatch {
    /// 空批次。
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置期望 revision。
    pub fn expected(mut self, revision: u64) -> Self {
        self.expected_revision = Some(revision);
        self
    }

    /// 追加 add。
    // `add` 是参考实现沿用下来的 builder 方法名，重命名会破坏公开 API。
    #[allow(clippy::should_implement_trait)]
    pub fn add(mut self, entry: MemoryEntryDraft) -> Self {
        self.operations.push(WriteOperation::Add(entry));
        self
    }

    /// 追加 replace。
    pub fn replace(mut self, id: impl Into<String>, entry: MemoryEntryDraft) -> Self {
        self.operations.push(WriteOperation::Replace {
            id: id.into(),
            entry,
        });
        self
    }

    /// 追加条件替换（pinned 字节未变化才写）。
    pub fn replace_if_unchanged(
        mut self,
        id: impl Into<String>,
        expected_entry_digest: impl Into<String>,
        entry: MemoryEntryDraft,
    ) -> Self {
        self.operations.push(WriteOperation::ReplaceIfUnchanged {
            id: id.into(),
            expected_entry_digest: expected_entry_digest.into(),
            entry,
        });
        self
    }

    /// 追加 remove。
    pub fn remove(mut self, id: impl Into<String>) -> Self {
        self.operations
            .push(WriteOperation::Remove { id: id.into() });
        self
    }

    /// 追加 patch。
    pub fn patch(
        mut self,
        id: impl Into<String>,
        old_text: impl Into<String>,
        new_text: impl Into<String>,
    ) -> Self {
        self.operations.push(WriteOperation::Patch {
            id: id.into(),
            old_text: old_text.into(),
            new_text: new_text.into(),
        });
        self
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }
}

/// 操作类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteOpKind {
    /// add
    Add,
    /// replace
    Replace,
    /// remove
    Remove,
    /// patch
    Patch,
}

impl WriteOpKind {
    /// 稳定字符串（审计）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Replace => "replace",
            Self::Remove => "remove",
            Self::Patch => "patch",
        }
    }
}

/// 已应用的操作（供审计）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedOperation {
    /// 操作类型。
    pub kind: WriteOpKind,
    /// 条目 id。
    pub id: String,
    /// 条目名称。
    pub name: String,
}

/// 写入结果。
#[derive(Clone, Debug, PartialEq)]
pub struct WriteReport {
    /// 新 revision。
    pub revision: u64,
    /// 已应用操作。
    pub applied: Vec<AppliedOperation>,
    /// 写入后的正文总字节。
    pub total_bytes: u64,
    /// 正文预算。
    pub limit_bytes: u64,
    /// `total_bytes / limit_bytes`（0..=1）。
    pub usage_ratio: f64,
    /// 索引字节数。
    pub index_bytes: usize,
    /// 索引行数。
    pub index_lines: usize,
    /// 索引是否截断。
    pub index_truncated: bool,
    /// 本次写入中被脱敏替换的次数（service 填充；store 为 0）。
    pub redactions: usize,
}

/// forget 墓碑。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TombstoneRecord {
    /// 格式版本。
    pub version: u32,
    /// 被遗忘条目 id。
    pub entry_id: String,
    /// 条目名称。
    pub name: String,
    /// 原文件相对路径。
    pub path: String,
    /// 正文 sha256（正文本身不落盘）。
    pub body_digest: String,
    /// 正文字节数。
    pub body_bytes: u64,
    /// 遗忘原因。
    pub reason: String,
    /// 遗忘时间（Unix 毫秒）。
    pub forgotten_at_ms: u64,
    /// 级联删除的子条目 id（仅目标条目记录）。
    pub cascaded: Vec<String>,
}

/// 被删除的条目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForgottenEntry {
    /// 条目 id。
    pub id: String,
    /// 条目名称。
    pub name: String,
    /// 删除原因。
    pub reason: String,
}

/// forget 结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForgetReport {
    /// 被删除条目（目标 + 级联）。
    pub removed: Vec<ForgottenEntry>,
    /// 写入的墓碑条目 id。
    pub tombstone_ids: Vec<String>,
    /// 新 revision。
    pub revision: u64,
}

/// 作用域存储。
#[derive(Clone, Debug)]
pub struct MemoryStore {
    paths: ScopePaths,
    config: MemoryConfig,
}

impl MemoryStore {
    /// 打开/初始化一个 scope（创建目录，但不会自动写索引）。
    pub fn open(paths: ScopePaths, config: MemoryConfig) -> MemoryResult<Self> {
        paths
            .ensure_dirs()
            .map_err(|error| io_error(&paths.root, error))?;
        Ok(Self { paths, config })
    }

    /// user scope（`~/.kiana/memory`）。
    pub fn open_user(config: MemoryConfig) -> MemoryResult<Self> {
        let paths = ScopePaths::user_default()
            .ok_or_else(|| MemoryError::InvalidArgument("cannot resolve home directory".into()))?;
        Self::open(paths, config)
    }

    /// project scope（`<cwd>/.kiana/memory`）。
    pub fn open_project(cwd: impl AsRef<Path>, config: MemoryConfig) -> MemoryResult<Self> {
        Self::open(ScopePaths::project_at(cwd), config)
    }

    /// 路径。
    pub fn paths(&self) -> &ScopePaths {
        &self.paths
    }

    /// 配置。
    pub fn config(&self) -> &MemoryConfig {
        &self.config
    }

    /// 读取索引快照。索引缺失时返回 revision 0 的空索引。
    pub fn load_snapshot(&self) -> MemoryResult<StoreSnapshot> {
        let Some(parsed) = self.read_index_optional()? else {
            let rendered = render_index(&[], 0, &self.config.limits);
            return Ok(StoreSnapshot {
                scope: self.paths.scope,
                revision: 0,
                index: rendered.content,
                entries: Vec::new(),
                warnings: rendered.warning.into_iter().collect(),
                truncated: false,
            });
        };
        let rendered = render_index(&parsed.entries, parsed.revision, &self.config.limits);
        let truncated = !parsed.warnings.is_empty();
        Ok(StoreSnapshot {
            scope: self.paths.scope,
            revision: parsed.revision,
            index: rendered.content,
            entries: parsed.entries,
            warnings: parsed.warnings,
            truncated,
        })
    }

    /// 读取单个条目；`id` 必须是合法 slug。
    pub fn read_entry(&self, id: &str) -> MemoryResult<MemoryEntry> {
        if !is_valid_id(id) {
            return Err(MemoryError::InvalidArgument(format!(
                "invalid entry id `{id}`"
            )));
        }
        let path = self.paths.entry_path(id);
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                MemoryError::EntryNotFound(id.to_string())
            } else {
                io_error(&path, error)
            }
        })?;
        if metadata.file_type().is_symlink() {
            return Err(MemoryError::InvalidArgument(format!(
                "refusing to read symlinked memory entry `{id}`"
            )));
        }
        let bytes = fs::read(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                MemoryError::EntryNotFound(id.to_string())
            } else {
                io_error(&path, error)
            }
        })?;
        MemoryEntry::from_file_bytes(&bytes).map_err(|message| {
            let backup = backup_file(&path);
            MemoryError::CorruptEntry {
                path,
                message,
                backup,
            }
        })
    }

    /// 读取全部条目（按 id 排序）。
    pub fn list_entries(&self) -> MemoryResult<Vec<MemoryEntry>> {
        let mut entries = self.load_all_entries()?;
        entries.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(entries)
    }

    /// 扫描召回 manifest：最多 200 个文件，每个只读前 30 行。
    pub fn scan_manifest(&self) -> MemoryResult<Manifest> {
        self.scan_manifest_at(SystemTime::now())
    }

    /// 用给定时间扫描（测试可注入）。
    pub fn scan_manifest_at(&self, now: SystemTime) -> MemoryResult<Manifest> {
        let mut files = Vec::new();
        collect_markdown_files(&self.paths.root, &mut files)?;
        files.sort();
        let scanned_files = files.len();
        let mut stamped = Vec::with_capacity(files.len());
        for path in files {
            match fs::metadata(&path) {
                Ok(metadata) => match metadata.modified() {
                    Ok(mtime) => stamped.push((path, mtime)),
                    Err(_) => continue,
                },
                Err(_) => continue,
            }
        }
        // 最新优先；同一 mtime 按路径稳定排序。
        stamped.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        let truncated = stamped.len() > self.config.limits.scan_max_files;
        stamped.truncate(self.config.limits.scan_max_files);

        let mut entries = Vec::new();
        let mut errors = Vec::new();
        for (path, mtime) in stamped {
            let fallback_id = path
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_string();
            match read_prefix_lines(&path, self.config.limits.scan_max_lines) {
                Ok(prefix) => match MemoryHeader::from_prefix(&prefix, &fallback_id) {
                    Ok(header) => {
                        let relative_path = path
                            .strip_prefix(&self.paths.root)
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .replace('\\', "/");
                        entries.push(ManifestEntry {
                            id: header.id,
                            name: header.name,
                            description: header.description,
                            mtype: header.mtype,
                            revision: header.revision,
                            derived_from: header.derived_from,
                            source_key: header.source_key,
                            path: path.clone(),
                            relative_path,
                            mtime,
                            mtime_ms: system_time_millis(mtime),
                            age_days: age_days(now, mtime),
                        });
                    }
                    Err(message) => errors.push(ScanError { path, message }),
                },
                Err(error) => errors.push(ScanError {
                    path,
                    message: error.to_string(),
                }),
            }
        }
        Ok(Manifest {
            entries,
            scanned_files,
            truncated,
            errors,
        })
    }

    /// 从磁盘重建索引（维护入口；也会刷新 revision）。外部调用请走
    /// [`crate::service::MemoryService::rebuild_index`] 以经过权限 gate 与审计。
    pub(crate) fn rebuild_index(&self) -> MemoryResult<StoreSnapshot> {
        let _lock = self.lock()?;
        let parsed = self.read_index_for_write()?;
        let entries = self.load_all_entries()?;
        let revision = parsed.as_ref().map(|parsed| parsed.revision).unwrap_or(0) + 1;
        let index_entries = to_index_entries(&entries);
        let rendered = render_index(&index_entries, revision, &self.config.limits);
        atomic_write(&self.paths.index_path, rendered.content.as_bytes())?;
        Ok(StoreSnapshot {
            scope: self.paths.scope,
            revision,
            index: rendered.content,
            entries: index_entries,
            warnings: rendered.warning.into_iter().collect(),
            truncated: rendered.dropped > 0,
        })
    }

    /// 写入 `memory_summary.md`（由抽取整合调用；调用方负责脱敏）。
    pub(crate) fn write_summary(&self, text: &str) -> MemoryResult<()> {
        let _lock = self.lock()?;
        let body = text.trim();
        let mut content = format!("<!-- kiana-memory-summary: {SUMMARY_FORMAT_VERSION} -->\n");
        if !body.is_empty() {
            content.push_str(body);
            content.push('\n');
        }
        atomic_write(&self.paths.summary_path, content.as_bytes())
    }

    /// 应用一批写入。
    pub(crate) fn apply_batch(&self, batch: &WriteBatch) -> MemoryResult<WriteReport> {
        if batch.is_empty() {
            return Err(MemoryError::InvalidArgument(
                "write batch contains no operations".into(),
            ));
        }
        let _lock = self.lock()?;
        let parsed = self.read_index_for_write()?;
        let current_revision = parsed.as_ref().map(|parsed| parsed.revision).unwrap_or(0);
        if let Some(expected) = batch.expected_revision
            && expected != current_revision
        {
            return Err(MemoryError::RevisionConflict {
                expected,
                actual: current_revision,
            });
        }

        let existing = self.load_all_entries()?;
        let mut by_id: BTreeMap<String, MemoryEntry> = existing
            .into_iter()
            .map(|entry| (entry.id.clone(), entry))
            .collect();
        let mut names: HashSet<String> = by_id.values().map(|entry| entry.name.clone()).collect();
        let mut applied = Vec::with_capacity(batch.operations.len());
        // 路径 -> 最终内容（None = 删除）。同一路径在一个批次里多次出现时以最后一次为准，
        // 这样 remove 后 add 同名条目不会在写完后又被删除。
        let mut changes: BTreeMap<PathBuf, Option<Vec<u8>>> = BTreeMap::new();

        for operation in &batch.operations {
            match operation {
                WriteOperation::Add(draft) => {
                    draft.validate()?;
                    if names.contains(&draft.name) {
                        return Err(MemoryError::EntryExists(draft.name.clone()));
                    }
                    let id = unique_id(&draft.name, &by_id);
                    let entry = MemoryEntry::from_draft(draft.clone(), id.clone(), 1);
                    names.insert(entry.name.clone());
                    changes.insert(self.paths.entry_path(&id), Some(entry.to_file_bytes()));
                    applied.push(AppliedOperation {
                        kind: WriteOpKind::Add,
                        id: id.clone(),
                        name: entry.name.clone(),
                    });
                    by_id.insert(id, entry);
                }
                WriteOperation::Replace { id, entry } => {
                    entry.validate()?;
                    let current = by_id
                        .get(id)
                        .ok_or_else(|| MemoryError::EntryNotFound(id.clone()))?;
                    if current.name != entry.name {
                        if names.contains(&entry.name) {
                            return Err(MemoryError::EntryExists(entry.name.clone()));
                        }
                        names.remove(&current.name);
                        names.insert(entry.name.clone());
                    }
                    let revision = current.revision.saturating_add(1);
                    let replacement = MemoryEntry::from_draft(entry.clone(), id.clone(), revision);
                    changes.insert(self.paths.entry_path(id), Some(replacement.to_file_bytes()));
                    applied.push(AppliedOperation {
                        kind: WriteOpKind::Replace,
                        id: id.clone(),
                        name: replacement.name.clone(),
                    });
                    by_id.insert(id.clone(), replacement);
                }
                WriteOperation::ReplaceIfUnchanged {
                    id,
                    expected_entry_digest,
                    entry,
                } => {
                    entry.validate()?;
                    let current = by_id
                        .get(id)
                        .ok_or_else(|| MemoryError::EntryNotFound(id.clone()))?;
                    if content_digest(&current.to_file_bytes()) != *expected_entry_digest {
                        return Err(MemoryError::PinnedEntryChanged { entry: id.clone() });
                    }
                    if current.name != entry.name {
                        if names.contains(&entry.name) {
                            return Err(MemoryError::EntryExists(entry.name.clone()));
                        }
                        names.remove(&current.name);
                        names.insert(entry.name.clone());
                    }
                    let revision = current.revision.saturating_add(1);
                    let replacement = MemoryEntry::from_draft(entry.clone(), id.clone(), revision);
                    changes.insert(self.paths.entry_path(id), Some(replacement.to_file_bytes()));
                    applied.push(AppliedOperation {
                        kind: WriteOpKind::Replace,
                        id: id.clone(),
                        name: replacement.name.clone(),
                    });
                    by_id.insert(id.clone(), replacement);
                }
                WriteOperation::Remove { id } => {
                    let current = by_id
                        .get(id)
                        .ok_or_else(|| MemoryError::EntryNotFound(id.clone()))?;
                    names.remove(&current.name);
                    changes.insert(self.paths.entry_path(id), None);
                    applied.push(AppliedOperation {
                        kind: WriteOpKind::Remove,
                        id: id.clone(),
                        name: current.name.clone(),
                    });
                    by_id.remove(id);
                }
                WriteOperation::Patch {
                    id,
                    old_text,
                    new_text,
                } => {
                    if old_text.is_empty() {
                        return Err(MemoryError::InvalidArgument(
                            "patch old_text must not be empty".into(),
                        ));
                    }
                    let current = by_id
                        .get(id)
                        .ok_or_else(|| MemoryError::EntryNotFound(id.clone()))?;
                    let occurrences = count_occurrences(&current.body, old_text);
                    if occurrences == 0 {
                        return Err(MemoryError::NoMatch {
                            entry: id.clone(),
                            old_text: old_text.clone(),
                        });
                    }
                    if occurrences > 1 {
                        return Err(MemoryError::RepeatedPatch {
                            entry: id.clone(),
                            occurrences,
                        });
                    }
                    let body = current.body.replacen(old_text, new_text, 1);
                    let draft = MemoryEntryDraft {
                        name: current.name.clone(),
                        description: current.description.clone(),
                        mtype: current.mtype,
                        body,
                        derived_from: current.derived_from.clone(),
                        source_key: current.source_key.clone(),
                    };
                    draft.validate()?;
                    let revision = current.revision.saturating_add(1);
                    let replacement = MemoryEntry::from_draft(draft, id.clone(), revision);
                    changes.insert(self.paths.entry_path(id), Some(replacement.to_file_bytes()));
                    applied.push(AppliedOperation {
                        kind: WriteOpKind::Patch,
                        id: id.clone(),
                        name: replacement.name.clone(),
                    });
                    by_id.insert(id.clone(), replacement);
                }
            }
        }

        let total_bytes = by_id
            .values()
            .map(|entry| entry.to_file_bytes().len() as u64)
            .sum::<u64>();
        if total_bytes > self.config.limits.max_total_bytes {
            let extra = total_bytes - self.config.limits.max_total_bytes;
            return Err(MemoryError::BudgetExceeded {
                used: total_bytes,
                limit: self.config.limits.max_total_bytes,
                extra,
            });
        }

        let mut entries: Vec<MemoryEntry> = by_id.into_values().collect();
        entries.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });
        let index_entries = to_index_entries(&entries);
        let revision = current_revision.saturating_add(1);
        let rendered = render_index(&index_entries, revision, &self.config.limits);

        // 先按最终状态落主题文件，再写索引。索引最后提交，读取方看到的新旧状态一致。
        for (path, change) in changes {
            match change {
                Some(bytes) => atomic_write(&path, &bytes)?,
                None => match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(io_error(path, error)),
                },
            }
        }
        atomic_write(&self.paths.index_path, rendered.content.as_bytes())?;

        Ok(WriteReport {
            revision,
            applied,
            total_bytes,
            limit_bytes: self.config.limits.max_total_bytes,
            usage_ratio: if self.config.limits.max_total_bytes == 0 {
                0.0
            } else {
                (total_bytes as f64) / (self.config.limits.max_total_bytes as f64)
            },
            index_bytes: rendered.byte_count,
            index_lines: rendered.line_count,
            index_truncated: rendered.dropped > 0,
            redactions: 0,
        })
    }

    /// 遗忘条目：写墓碑（不含正文）、删除文件、级联删除 `derived_from` 后代。
    pub(crate) fn forget(&self, id: &str, reason: &str, now_ms: u64) -> MemoryResult<ForgetReport> {
        if !is_valid_id(id) {
            return Err(MemoryError::InvalidArgument(format!(
                "invalid entry id `{id}`"
            )));
        }
        let _lock = self.lock()?;
        let parsed = self.read_index_for_write()?;
        let current_revision = parsed.as_ref().map(|parsed| parsed.revision).unwrap_or(0);
        let mut by_id: BTreeMap<String, MemoryEntry> = self
            .load_all_entries()?
            .into_iter()
            .map(|entry| (entry.id.clone(), entry))
            .collect();
        let target = by_id
            .remove(id)
            .ok_or_else(|| MemoryError::EntryNotFound(id.to_string()))?;

        let mut removed: Vec<(MemoryEntry, String)> = vec![(target, reason.to_string())];
        let mut frontier: Vec<String> = vec![id.to_string()];
        let mut cascaded_ids: Vec<String> = Vec::new();
        while let Some(parent) = frontier.pop() {
            let child_ids: Vec<String> = by_id
                .values()
                .filter(|entry| entry.derived_from.iter().any(|value| value == &parent))
                .map(|entry| entry.id.clone())
                .collect();
            for child_id in child_ids {
                if let Some(child) = by_id.remove(&child_id) {
                    cascaded_ids.push(child_id.clone());
                    frontier.push(child_id);
                    removed.push((child, format!("derived_from:{parent}")));
                }
            }
        }

        let mut tombstones = Vec::with_capacity(removed.len());
        for (entry, removal_reason) in &removed {
            let body_digest = content_digest(entry.body.as_bytes());
            let record = TombstoneRecord {
                version: TOMBSTONE_FORMAT_VERSION,
                entry_id: entry.id.clone(),
                name: entry.name.clone(),
                path: ScopePaths::entry_relative_path(&entry.id),
                body_digest,
                body_bytes: entry.body.len() as u64,
                reason: removal_reason.clone(),
                forgotten_at_ms: now_ms,
                cascaded: if entry.id == id {
                    cascaded_ids.clone()
                } else {
                    Vec::new()
                },
            };
            tombstones.push(record);
        }

        let mut entries: Vec<MemoryEntry> = by_id.into_values().collect();
        entries.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });
        let revision = current_revision.saturating_add(1);
        let rendered = render_index(&to_index_entries(&entries), revision, &self.config.limits);

        append_tombstones(&self.paths.tombstone_path, &tombstones)?;
        for (entry, _) in &removed {
            let path = self.paths.entry_path(&entry.id);
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_error(path, error)),
            }
        }
        atomic_write(&self.paths.index_path, rendered.content.as_bytes())?;

        Ok(ForgetReport {
            removed: removed
                .into_iter()
                .map(|(entry, reason)| ForgottenEntry {
                    id: entry.id,
                    name: entry.name,
                    reason,
                })
                .collect(),
            tombstone_ids: tombstones
                .into_iter()
                .map(|record| record.entry_id)
                .collect(),
            revision,
        })
    }

    fn lock(&self) -> MemoryResult<FileLock> {
        FileLock::acquire(&self.paths.lock_path, self.config.lock_timeout)
    }

    fn read_index_optional(&self) -> MemoryResult<Option<ParsedIndex>> {
        let path = &self.paths.index_path;
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error(path, error)),
        };
        let text = std::str::from_utf8(&bytes).map_err(|error| {
            let backup = backup_file(path);
            MemoryError::CorruptIndex {
                path: path.clone(),
                message: format!("not UTF-8: {error}"),
                backup,
            }
        })?;
        match parse_index(text) {
            Ok(parsed) => Ok(Some(parsed)),
            Err(IndexParseError::UnsupportedVersion(version)) => {
                Err(MemoryError::UnsupportedFormat {
                    path: path.clone(),
                    version,
                })
            }
            Err(error) => {
                let backup = backup_file(path);
                Err(corrupt_index_error(path.clone(), error, backup))
            }
        }
    }

    fn read_index_for_write(&self) -> MemoryResult<Option<ParsedIndex>> {
        self.read_index_optional()
    }

    fn load_all_entries(&self) -> MemoryResult<Vec<MemoryEntry>> {
        let entries = match fs::read_dir(&self.paths.topics_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io_error(&self.paths.topics_dir, error)),
        };
        let mut result = Vec::new();
        for item in entries {
            let item = item.map_err(|error| io_error(&self.paths.topics_dir, error))?;
            let path = item.path();
            let file_type = item.file_type().map_err(|error| io_error(&path, error))?;
            if file_type.is_symlink() || !file_type.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if !name.ends_with(".md") || name.ends_with(".bak") || name.starts_with('.') {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            if !is_valid_id(stem) {
                continue;
            }
            let bytes = fs::read(&path).map_err(|error| io_error(&path, error))?;
            let entry = MemoryEntry::from_file_bytes(&bytes).map_err(|message| {
                let backup = backup_file(&path);
                MemoryError::CorruptEntry {
                    path: path.clone(),
                    message,
                    backup,
                }
            })?;
            if entry.id != stem {
                let backup = backup_file(&path);
                return Err(MemoryError::CorruptEntry {
                    path: path.clone(),
                    message: format!(
                        "file name `{stem}` does not match frontmatter id `{}`",
                        entry.id
                    ),
                    backup,
                });
            }
            result.push(entry);
        }
        Ok(result)
    }
}

fn to_index_entries(entries: &[MemoryEntry]) -> Vec<IndexEntry> {
    entries
        .iter()
        .map(|entry| {
            IndexEntry::new(
                entry.id.clone(),
                entry.name.clone(),
                entry.description.clone(),
            )
        })
        .collect()
}

fn unique_id(name: &str, existing: &BTreeMap<String, MemoryEntry>) -> String {
    let base = slugify(name);
    if !existing.contains_key(&base) {
        return base;
    }
    for suffix in 2..=u32::MAX {
        let candidate = format!("{base}-{suffix}");
        let candidate = if candidate.len() > 64 {
            let keep = 64 - suffix.to_string().len() - 1;
            format!("{}-{suffix}", &base[..keep.min(base.len())])
        } else {
            candidate
        };
        if !existing.contains_key(&candidate) {
            return candidate;
        }
    }
    format!("{base}-{}", uuid_suffix())
}

fn uuid_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

fn count_occurrences(text: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    text.match_indices(needle).count()
}

/// 收集所有主题 markdown 文件（跳过索引/摘要/状态/备份/隐藏文件）。
fn collect_markdown_files(root: &Path, out: &mut Vec<PathBuf>) -> MemoryResult<()> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error(root, error)),
    };
    for item in entries {
        let item = item.map_err(|error| io_error(root, error))?;
        let path = item.path();
        let file_type = item.file_type().map_err(|error| io_error(&path, error))?;
        // 不跟随 symlink：避免目录环和把 scope 外的文件当作记忆注入。
        if file_type.is_symlink() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if name.starts_with('.') || name == "state" || name == "node_modules" {
            continue;
        }
        if file_type.is_dir() {
            collect_markdown_files(&path, out)?;
            continue;
        }
        if !file_type.is_file() || !name.ends_with(".md") || name.ends_with(".bak") {
            continue;
        }
        if name == INDEX_FILE_NAME || name == SUMMARY_FILE_NAME {
            continue;
        }
        out.push(path);
    }
    Ok(())
}

/// 读取文件前 `max_lines` 行（只读 frontmatter，不把正文读进内存）。
fn read_prefix_lines(path: &Path, max_lines: usize) -> std::io::Result<String> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut out = String::new();
    for line in reader.lines().take(max_lines) {
        let line = line?;
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}

fn system_time_millis(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn age_days(now: SystemTime, then: SystemTime) -> u64 {
    now.duration_since(then)
        .map(|duration| duration.as_secs() / 86_400)
        .unwrap_or(0)
}

/// 备份现有文件为 `<path>.bak`；失败返回 `None`。
pub(crate) fn backup_file(path: &Path) -> Option<PathBuf> {
    if !path.exists() {
        return None;
    }
    let backup = PathBuf::from(format!("{}.bak", path.display()));
    fs::copy(path, &backup).ok().map(|_| backup)
}

/// temp+rename 原子写。
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> MemoryResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    }
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("memory");
    let temp = path.with_file_name(format!(
        ".{file_name}.tmp.{}.{}",
        std::process::id(),
        counter
    ));
    let result = (|| -> MemoryResult<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|error| io_error(&temp, error))?;
        file.write_all(bytes)
            .map_err(|error| io_error(&temp, error))?;
        file.sync_all().map_err(|error| io_error(&temp, error))?;
        drop(file);
        fs::rename(&temp, path).map_err(|error| io_error(path, error))?;
        #[cfg(unix)]
        {
            if let Some(parent) = path.parent()
                && let Ok(dir) = File::open(parent)
            {
                let _ = dir.sync_all();
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn append_tombstones(path: &Path, records: &[TombstoneRecord]) -> MemoryResult<()> {
    if records.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| io_error(path, error))?;
    for record in records {
        let line = serde_json::to_string(record)?;
        file.write_all(line.as_bytes())
            .and_then(|()| file.write_all(b"\n"))
            .map_err(|error| io_error(path, error))?;
    }
    file.sync_data().map_err(|error| io_error(path, error))?;
    Ok(())
}

/// 读取 tombstone（测试/治理展示用）。
pub fn read_tombstones(path: &Path) -> MemoryResult<Vec<TombstoneRecord>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_error(path, error)),
    };
    let mut records = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record = serde_json::from_str(line).map_err(|error| {
            MemoryError::InvalidArgument(format!("invalid tombstone line {}: {error}", index + 1))
        })?;
        records.push(record);
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MemoryConfig;

    fn temp_store() -> (tempfile::TempDir, MemoryStore) {
        let dir = tempfile::tempdir().unwrap();
        let paths = ScopePaths::at_root(MemoryScope::Project, dir.path().join("memory"));
        let store = MemoryStore::open(paths, MemoryConfig::default()).unwrap();
        (dir, store)
    }

    #[test]
    fn atomic_write_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.md");
        atomic_write(&path, b"hello").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn batch_failure_leaves_store_unchanged() {
        let (_dir, store) = temp_store();
        let report = store
            .apply_batch(&WriteBatch::new().add(MemoryEntryDraft::new(
                "ok",
                "fine",
                MemoryType::User,
                "body",
            )))
            .unwrap();
        assert_eq!(report.applied.len(), 1);
        let before = store.load_snapshot().unwrap();
        let before_bytes = fs::read(&store.paths().index_path).unwrap();
        let bad = store.apply_batch(
            &WriteBatch::new()
                .add(MemoryEntryDraft::new(
                    "new",
                    "fine",
                    MemoryType::User,
                    "body",
                ))
                .add(MemoryEntryDraft::new(
                    "bad",
                    "fine",
                    MemoryType::Feedback,
                    "no why",
                )),
        );
        assert!(bad.is_err());
        let after = store.load_snapshot().unwrap();
        assert_eq!(before.revision, after.revision);
        assert_eq!(before_bytes, fs::read(&store.paths().index_path).unwrap());
        assert!(store.read_entry("new").is_err());
    }

    #[test]
    fn corrupt_index_is_backed_up_and_refused() {
        let (_dir, store) = temp_store();
        fs::write(&store.paths().index_path, b"not an index").unwrap();
        let error = store
            .apply_batch(&WriteBatch::new().add(MemoryEntryDraft::new(
                "x",
                "d",
                MemoryType::User,
                "b",
            )))
            .unwrap_err();
        assert!(matches!(error, MemoryError::CorruptIndex { .. }));
        let backup = PathBuf::from(format!("{}.bak", store.paths().index_path.display()));
        assert_eq!(fs::read(&backup).unwrap(), b"not an index");
        assert_eq!(
            fs::read(&store.paths().index_path).unwrap(),
            b"not an index"
        );
    }
}
