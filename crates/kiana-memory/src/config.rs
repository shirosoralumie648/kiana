//! 配置与 scope 路径。
//!
//! 每个 scope 一个目录：
//!
//! - user：`~/.kiana/memory`（`dirs::home_dir()` 解析）；
//! - project：`<cwd>/.kiana/memory`。
//!
//! 目录布局（`ScopePaths` 的字段）：
//!
//! ```text
//! <root>/
//!   MEMORY.md              索引（人工可读，最多 200 行 / 25KB）
//!   memory_summary.md      抽取整合后的摘要（可选）
//!   audit.jsonl            每次写操作一条审计
//!   topics/<id>.md         主题文件（frontmatter + 正文）
//!   state/
//!     extraction.json      抽取 watermark / lease / 每日预算
//!     extraction.lock      lease 的跨进程互斥锁
//!     tombstones.jsonl     forget 墓碑（不含正文）
//!   .memory.lock           写锁（temp+rename 期间互斥）
//! ```
//!
//! 所有默认值都以「有界」为目标：召回最多 5 条、索引最多 200 行、扫描最多 200 个文件、
//! 每个文件最多读 30 行。自动抽取默认 **关闭**（`auto_extract = false`），显式配置后才
//! 会运行后台 job。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// 默认索引行上限（与 Claude Code / qwen-code 的 MEMORY.md 契约一致）。
pub const DEFAULT_MAX_INDEX_LINES: usize = 200;
/// 默认索引字节上限（25 KB）。
pub const DEFAULT_MAX_INDEX_BYTES: usize = 25_000;
/// 索引单行字符上限（含 `- [name](path) — ` 前缀）。
pub const DEFAULT_MAX_INDEX_ENTRY_CHARS: usize = 200;
/// 召回扫描的文件数量上限。
pub const DEFAULT_SCAN_MAX_FILES: usize = 200;
/// 每个文件只读前 30 行来解析 frontmatter。
pub const DEFAULT_SCAN_MAX_LINES: usize = 30;
/// 召回最多注入 5 条。
pub const DEFAULT_RECALL_MAX_SELECTED: usize = 5;
/// 召回注入的近似 token 预算。
pub const DEFAULT_RECALL_TOKEN_BUDGET: usize = 2_000;
/// side query 的超时；超时立即回退启发式，不阻断模型调用。
pub const DEFAULT_PROVIDER_TIMEOUT_MS: u64 = 800;
/// 写锁默认等待时间。
pub const DEFAULT_LOCK_TIMEOUT_MS: u64 = 2_000;
/// 整个 scope 的正文预算（字节）。
pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 2_000_000;
/// 自动抽取只处理「足够旧」的会话（默认 5 分钟）。
pub const DEFAULT_MIN_SESSION_AGE_MS: u64 = 300_000;
/// 抽取 lease 默认 TTL。
pub const DEFAULT_LEASE_TTL_MS: u64 = 120_000;
/// 每天最多抽取次数。
pub const DEFAULT_MAX_RUNS_PER_DAY: u32 = 12;
/// 每天最多写入的记忆字节数。
pub const DEFAULT_MAX_BYTES_PER_DAY: u64 = 4_000_000;
/// 单次抽取最多处理的会话数。
pub const DEFAULT_MAX_SESSIONS_PER_RUN: usize = 10;
/// 连续失败多少次后停止重试（等日期翻转或人工清理）。
pub const DEFAULT_MAX_CONSECUTIVE_FAILURES: u32 = 3;

/// 记忆 scope。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryScope {
    /// `~/.kiana/memory`。
    User,
    /// `<cwd>/.kiana/memory`。
    Project,
}

impl MemoryScope {
    /// 稳定字符串（审计/状态文件）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

/// 索引/扫描/正文的硬限制。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryLimits {
    /// 索引最多行数（包含格式头与警告行）。
    pub max_index_lines: usize,
    /// 索引最多字节数（包含警告行）。
    pub max_index_bytes: usize,
    /// 索引单条渲染后的字符上限。
    pub max_index_entry_chars: usize,
    /// 召回扫描最多文件数。
    pub scan_max_files: usize,
    /// 每个文件只读前 N 行解析 frontmatter。
    pub scan_max_lines: usize,
    /// 正文总字节预算。
    pub max_total_bytes: u64,
}

impl Default for MemoryLimits {
    fn default() -> Self {
        Self {
            max_index_lines: DEFAULT_MAX_INDEX_LINES,
            max_index_bytes: DEFAULT_MAX_INDEX_BYTES,
            max_index_entry_chars: DEFAULT_MAX_INDEX_ENTRY_CHARS,
            scan_max_files: DEFAULT_SCAN_MAX_FILES,
            scan_max_lines: DEFAULT_SCAN_MAX_LINES,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
        }
    }
}

/// 召回配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecallConfig {
    /// side query 最多选择多少条（额外再被 `max_selected` 截断）。
    pub max_selected: usize,
    /// 注入消息的近似 token 预算。
    pub token_budget: usize,
    /// provider 超时。
    pub provider_timeout: Duration,
}

impl Default for RecallConfig {
    fn default() -> Self {
        Self {
            max_selected: DEFAULT_RECALL_MAX_SELECTED,
            token_budget: DEFAULT_RECALL_TOKEN_BUDGET,
            provider_timeout: Duration::from_millis(DEFAULT_PROVIDER_TIMEOUT_MS),
        }
    }
}

/// 后台抽取配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractionConfig {
    /// 只处理修改时间早于 `now - min_session_age` 的会话（避免抽取正在写的会话）。
    pub min_session_age_ms: u64,
    /// lease TTL；过期后允许其他进程接管。
    pub lease_ttl_ms: u64,
    /// 每日运行次数上限。
    pub max_runs_per_day: u32,
    /// 每日写入字节上限。
    pub max_bytes_per_day: u64,
    /// 单次最多处理的会话数。
    pub max_sessions_per_run: usize,
    /// 连续失败上限。
    pub max_consecutive_failures: u32,
}

impl Default for ExtractionConfig {
    fn default() -> Self {
        Self {
            min_session_age_ms: DEFAULT_MIN_SESSION_AGE_MS,
            lease_ttl_ms: DEFAULT_LEASE_TTL_MS,
            max_runs_per_day: DEFAULT_MAX_RUNS_PER_DAY,
            max_bytes_per_day: DEFAULT_MAX_BYTES_PER_DAY,
            max_sessions_per_run: DEFAULT_MAX_SESSIONS_PER_RUN,
            max_consecutive_failures: DEFAULT_MAX_CONSECUTIVE_FAILURES,
        }
    }
}

/// 记忆子系统配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryConfig {
    /// 索引/扫描/正文限制。
    pub limits: MemoryLimits,
    /// 召回限制。
    pub recall: RecallConfig,
    /// 抽取限制。
    pub extraction: ExtractionConfig,
    /// **默认 false**：不默认开启自动记忆；需要显式打开。
    pub auto_extract: bool,
    /// 写锁等待时间。
    pub lock_timeout: Duration,
    /// 写入前是否脱敏（默认 true；治理要求任何写入路径都不能落下 secret）。
    pub redact_secrets: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            limits: MemoryLimits::default(),
            recall: RecallConfig::default(),
            extraction: ExtractionConfig::default(),
            auto_extract: false,
            lock_timeout: Duration::from_millis(DEFAULT_LOCK_TIMEOUT_MS),
            redact_secrets: true,
        }
    }
}

/// 一个 scope 的所有落盘路径。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopePaths {
    /// scope 类型。
    pub scope: MemoryScope,
    /// 根目录（`~/.kiana/memory` 或 `<cwd>/.kiana/memory`）。
    pub root: PathBuf,
    /// `MEMORY.md`。
    pub index_path: PathBuf,
    /// `memory_summary.md`。
    pub summary_path: PathBuf,
    /// `topics/`。
    pub topics_dir: PathBuf,
    /// `state/`。
    pub state_dir: PathBuf,
    /// `state/extraction.json`。
    pub extraction_state_path: PathBuf,
    /// `state/extraction.lock`。
    pub extraction_lock_path: PathBuf,
    /// `state/tombstones.jsonl`。
    pub tombstone_path: PathBuf,
    /// `audit.jsonl`。
    pub audit_path: PathBuf,
    /// 审计追加的跨进程锁（`audit.lock`）。
    pub audit_lock_path: PathBuf,
    /// `.memory.lock`。
    pub lock_path: PathBuf,
}

impl ScopePaths {
    /// 在给定根目录上构造（测试与 wire 层复用）。
    pub fn at_root(scope: MemoryScope, root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            scope,
            index_path: root.join("MEMORY.md"),
            summary_path: root.join("memory_summary.md"),
            topics_dir: root.join("topics"),
            state_dir: root.join("state"),
            extraction_state_path: root.join("state").join("extraction.json"),
            extraction_lock_path: root.join("state").join("extraction.lock"),
            tombstone_path: root.join("state").join("tombstones.jsonl"),
            audit_path: root.join("audit.jsonl"),
            audit_lock_path: root.join("state").join("audit.lock"),
            lock_path: root.join(".memory.lock"),
            root,
        }
    }

    /// user scope：`home/.kiana/memory`。
    pub fn user_at(home: impl AsRef<Path>) -> Self {
        Self::at_root(
            MemoryScope::User,
            home.as_ref().join(".kiana").join("memory"),
        )
    }

    /// project scope：`cwd/.kiana/memory`。
    pub fn project_at(cwd: impl AsRef<Path>) -> Self {
        Self::at_root(
            MemoryScope::Project,
            cwd.as_ref().join(".kiana").join("memory"),
        )
    }

    /// 使用当前用户 home 目录的 user scope；无法解析 home 时返回 `None`。
    pub fn user_default() -> Option<Self> {
        dirs::home_dir().map(Self::user_at)
    }

    /// 创建目录骨架（幂等）。
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::create_dir_all(&self.topics_dir)?;
        std::fs::create_dir_all(&self.state_dir)?;
        Ok(())
    }

    /// 主题文件路径：`topics/<id>.md`。
    pub fn entry_path(&self, id: &str) -> PathBuf {
        self.topics_dir.join(format!("{id}.md"))
    }

    /// 主题文件在索引里的相对路径：`topics/<id>.md`。
    pub fn entry_relative_path(id: &str) -> String {
        format!("topics/{id}.md")
    }
}
