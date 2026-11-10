//! 记忆子系统的错误契约。
//!
//! 所有可失败入口返回 [`MemoryError`]。错误按「调用方该做什么」分组：
//!
//! - **输入错误**（[`MemoryError::InvalidField`]、[`MemoryError::MissingField`]、
//!   [`MemoryError::EntryExists`]、[`MemoryError::EntryNotFound`]、
//!   [`MemoryError::AmbiguousMatch`]、[`MemoryError::NoMatch`]、
//!   [`MemoryError::RepeatedPatch`]）：调用方改参数后重试；不会留下任何磁盘副作用。
//! - **并发/一致性**（[`MemoryError::RevisionConflict`]、[`MemoryError::LockTimeout`]、
//!   [`MemoryError::PinnedEntryChanged`]）：重新读取后重试；
//!   [`MemoryError::LockTimeout`] 表示另一个进程持有写锁，本次没有写入。
//! - **外部破坏**（[`MemoryError::CorruptIndex`]、[`MemoryError::CorruptEntry`]、
//!   [`MemoryError::UnsupportedFormat`]）：磁盘上的文件无法被本版本往返解析。实现会先
//!   备份为 `.bak` 再拒绝覆盖；调用方需要人工修复。
//! - **预算**（[`MemoryError::BudgetExceeded`]、[`MemoryError::DailyBudgetExhausted`]）：
//!   清理旧条目或等第二天。
//! - **治理**（[`MemoryError::PermissionDenied`]、[`MemoryError::StagedApprovalNotFound`]）：
//!   写被权限层或审批状态拒绝；记忆内容不会被修改。
//! - **后台任务**（[`MemoryError::ExtractionDisabled`]、[`MemoryError::LeaseBusy`]、
//!   [`MemoryError::RetryExhausted`]）：抽取是可选的（默认关闭），这些错误/跳过状态不应
//!   阻断前台对话。

use std::path::PathBuf;

use thiserror::Error;

/// 记忆子系统的统一错误类型。
#[derive(Debug, Error)]
pub enum MemoryError {
    /// 文件系统错误（附带出错路径）。
    #[error("memory i/o error at {path}: {source}")]
    Io {
        /// 出错路径。
        path: PathBuf,
        /// 底层错误。
        #[source]
        source: std::io::Error,
    },

    /// 条目字段级校验失败；`entry` 是条目标识（name 或 id），`field` 用点号表示层级
    /// （如 `body.why`）。
    #[error("memory entry `{entry}` has invalid field `{field}`: {message}")]
    InvalidField {
        /// 条目标识。
        entry: String,
        /// 字段路径。
        field: &'static str,
        /// 说明。
        message: String,
    },

    /// feedback/project 条目缺少必需字段（`body.why` / `body.how_to_apply`）。
    #[error("memory entry `{entry}` is missing required field `{field}`")]
    MissingField {
        /// 条目标识。
        entry: String,
        /// 字段路径。
        field: &'static str,
    },

    /// 同名/同 id 条目已存在。
    #[error("memory entry `{0}` already exists")]
    EntryExists(String),

    /// 条目不存在。
    #[error("memory entry `{0}` not found")]
    EntryNotFound(String),

    /// `old_text` 命中多个条目，调用方必须提供更精确的文本。
    #[error("old_text `{old_text}` matches {matches} entries; make it unique")]
    AmbiguousMatch {
        /// 调用方提供的文本。
        old_text: String,
        /// 命中数量。
        matches: usize,
    },

    /// `old_text` 在目标条目中不存在。
    #[error("old_text `{old_text}` not found in entry `{entry}`")]
    NoMatch {
        /// 条目 id。
        entry: String,
        /// 调用方提供的文本。
        old_text: String,
    },

    /// patch 的旧片段在条目中出现多次。
    #[error("patch span occurs {occurrences} times in entry `{entry}`")]
    RepeatedPatch {
        /// 条目 id。
        entry: String,
        /// 出现次数。
        occurrences: usize,
    },

    /// 索引文件无法往返解析；`backup` 是写前快照（若备份也失败则为 `None`）。
    #[error(
        "memory index at {path} cannot be parsed as kiana-memory v1: {message} (backup: {backup:?})"
    )]
    CorruptIndex {
        /// 索引路径。
        path: PathBuf,
        /// 解析错误。
        message: String,
        /// `.bak` 路径。
        backup: Option<PathBuf>,
    },

    /// 主题文件无法往返解析；写前快照在 `backup`。
    #[error(
        "memory file at {path} cannot be parsed as a kiana-memory entry: {message} (backup: {backup:?})"
    )]
    CorruptEntry {
        /// 主题文件路径。
        path: PathBuf,
        /// 解析错误。
        message: String,
        /// `.bak` 路径。
        backup: Option<PathBuf>,
    },

    /// 期望的 revision 与磁盘上的 revision 不一致（乐观并发控制）。
    #[error("memory revision conflict: expected {expected}, actual {actual}")]
    RevisionConflict {
        /// 调用方读取到的 revision。
        expected: u64,
        /// 当前磁盘上的 revision。
        actual: u64,
    },

    /// 获取跨进程写锁超时；本次没有写入任何字节。
    #[error("memory lock timed out after {waited_ms} ms at {path}")]
    LockTimeout {
        /// 锁文件路径。
        path: PathBuf,
        /// 已等待毫秒数。
        waited_ms: u64,
    },

    /// 批次会让记忆总量超过字节预算。
    #[error("memory byte budget exceeded: {used}/{limit} bytes, operation would add {extra}")]
    BudgetExceeded {
        /// 当前已用字节。
        used: u64,
        /// 预算上限。
        limit: u64,
        /// 本次新增字节。
        extra: u64,
    },

    /// 记忆写入被权限层拒绝。
    #[error("memory write denied: {reason}")]
    PermissionDenied {
        /// 拒绝原因（脱敏后的文本）。
        reason: String,
    },

    /// 审批阶段 pin 的条目字节已经变化（TOCTOU）；拒绝按新内容应用，且不写盘。
    #[error("pinned memory entry `{entry}` changed since the write was staged")]
    PinnedEntryChanged {
        /// 条目 id。
        entry: String,
    },

    /// 审批凭据不存在（已应用或从未创建）。
    #[error("staged memory write `{0}` not found")]
    StagedApprovalNotFound(String),

    /// 磁盘上的格式版本高于本实现支持的版本。
    #[error("unsupported memory format version {version} at {path}")]
    UnsupportedFormat {
        /// 文件路径。
        path: PathBuf,
        /// 文件里的版本。
        version: u32,
    },

    /// 自动记忆抽取被配置关闭（默认关闭）。
    #[error("memory extraction is disabled by configuration")]
    ExtractionDisabled,

    /// 另一个进程持有抽取 lease。
    #[error("memory extraction lease is held by `{owner}` until {expires_at_ms}")]
    LeaseBusy {
        /// 持有者 id。
        owner: String,
        /// 过期时间（Unix 毫秒）。
        expires_at_ms: u64,
    },

    /// 当天的抽取预算用尽。
    #[error(
        "memory extraction daily budget exhausted: {runs}/{max_runs} runs, {bytes}/{max_bytes} bytes"
    )]
    DailyBudgetExhausted {
        /// 当天已运行次数。
        runs: u32,
        /// 次数上限。
        max_runs: u32,
        /// 当天累计写入字节。
        bytes: u64,
        /// 字节上限。
        max_bytes: u64,
    },

    /// 连续失败次数超过上限；在日期翻转前不再重试。
    #[error("memory extraction retry limit exceeded after {attempts} attempts: {last_error}")]
    RetryExhausted {
        /// 连续失败次数。
        attempts: u32,
        /// 最后一次错误。
        last_error: String,
    },

    /// 会话来源（扫描/读取 persisted session）失败。
    #[error("memory session source error: {0}")]
    Source(String),

    /// 抽取/召回 provider 失败。
    #[error("memory provider error: {0}")]
    Provider(String),

    /// 参数非法（例如空 id、路径穿越）。
    #[error("invalid memory argument: {0}")]
    InvalidArgument(String),

    /// JSON 状态文件错误。
    #[error("memory json error: {0}")]
    Json(#[from] serde_json::Error),

    /// 配置的模式编译失败（实现内部的静态模式，正常不会发生）。
    #[error("memory pattern error: {0}")]
    Pattern(String),
}

/// 记忆子系统的结果别名。
pub type MemoryResult<T> = Result<T, MemoryError>;

/// 把 `io::Error` 附上路径。
pub(crate) fn io_error(path: impl Into<PathBuf>, source: std::io::Error) -> MemoryError {
    MemoryError::Io {
        path: path.into(),
        source,
    }
}
