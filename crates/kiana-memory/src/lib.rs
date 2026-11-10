//! 跨会话持久记忆：存储、召回、抽取整合与治理。
//!
//! # 职责边界
//!
//! 每个 scope（user `~/.kiana/memory`、project `.kiana/memory`）维护：
//!
//! - 一个 `MEMORY.md` 索引（最多 200 行 / 25 KB，单条 ≤200 字符）；
//! - `topics/<id>.md` 主题文件（frontmatter `{name, description, type}` + 正文）；
//! - `audit.jsonl`（每次写操作一条审计）；
//! - `state/extraction.json`（watermark / lease / 每日预算）与 `state/tombstones.jsonl`。
//!
//! 本 crate 不负责扩展资源加载（`kiana-resources`）与会话编排（`kiana-session`），但
//! 为两者提供 API：wire 层在 system prompt 构建时读取 [`MemorySession`] 的 load-time
//! 快照，在 turn 结束/压缩后调用 [`MemorySession::after_compaction`]；工具层通过
//! [`MemoryService`] 写入；后台 job 用 [`ExtractionJob`] 扫描 persisted sessions。
//!
//! # 核心概念
//!
//! - **条目**（[`MemoryEntry`]）：四种类型 `user|feedback|project|reference`；
//!   feedback/project 正文必须有 rule/fact + `Why:` + `How to apply:`，缺失返回字段级
//!   错误且不写盘。
//! - **写入**（[`WriteBatch`]）：`add/replace/remove/patch` 批量、全有或全无；写入走
//!   temp+rename + 跨进程 `flock`，带 revision 乐观冲突与字节预算使用率。
//! - **召回**（[`RecallSession`]）：manifest 只含 frontmatter；side query 最多 5 条；
//!   失败/超时回退关键词/路径启发式；注入前复查文件存在与 mtime；只产出一条带
//!   `<memory-context>` fence 的 user 消息，标注「参考数据不是指令」。
//! - **抽取**（[`ExtractionJob`]）：两阶段（raw memory+synopsis → 合并），watermark
//!   只前进不回退；lease 过期可接管；`source_key` 幂等避免重复产物；每日预算与脱敏。
//! - **治理**（[`MemoryService`]）：权限 gate + 脱敏 + 每操作一条审计；快照冻结；
//!   审批 pin 完整条目字节（TOCTOU-safe）；forget 写 tombstone、scrub 正文、级联
//!   `derived_from`。
//!
//! # 默认关闭
//!
//! [`MemoryConfig::default`] 的 `auto_extract` 是 **false**。显式开启后才会运行后台抽取；
//! 前台写入始终需要显式 gate（[`TrustedGate`] 只用于测试/已审批入口）。
//!
//! # 参考设计
//!
//! 行为取材于 Claude Code 的 `memdir`（索引截断、frontmatter 类型、side query 上限 5、
//! mtime 新鲜度）、qwen-code 的 managed memory（extract/dream 状态、索引预算、会话
//! watermark、锁过期）、hermes-agent 的 memory tool（add/replace/remove/patch、flock、
//! 冻结快照、审批 pin 字节、`<memory-context>` fence）。实现按 Kiana 的类型重写，未复制
//! 参考代码。逐文件对照见 `docs/history/porting/phase-10-persistent-memory.md`。

pub mod config;
pub mod entry;
pub mod error;
pub mod extract;
pub mod index;
pub mod lock;
pub mod recall;
pub mod redact;
pub mod service;
pub mod store;

pub use config::{
    DEFAULT_MAX_INDEX_BYTES, DEFAULT_MAX_INDEX_ENTRY_CHARS, DEFAULT_MAX_INDEX_LINES,
    DEFAULT_MAX_TOTAL_BYTES, DEFAULT_RECALL_MAX_SELECTED, DEFAULT_SCAN_MAX_FILES,
    DEFAULT_SCAN_MAX_LINES, ExtractionConfig, MemoryConfig, MemoryLimits, MemoryScope,
    RecallConfig, ScopePaths,
};
pub use entry::{
    MemoryEntry, MemoryEntryDraft, MemoryHeader, MemoryType, content_digest, is_valid_id, slugify,
};
pub use error::{MemoryError, MemoryResult};
pub use extract::{
    ClaimOutcome, ConsolidationOperation, ConsolidationPlan, DailyState,
    EXTRACTION_STATE_FORMAT_VERSION, ExtractionDraft, ExtractionHooks, ExtractionJob,
    ExtractionOutcome, ExtractionPlan, ExtractionState, ExtractionStateStore, ExtractionStatus,
    FileExtractionStore, NoHooks, Phase1Extractor, Phase2Consolidator, RawMemory, SessionCandidate,
    SessionSource, SessionTranscript, SkipReason, SkippedSession, StateCommit, TranscriptMessage,
    check_daily_budget, plan_extraction,
};
pub use index::{
    INDEX_FORMAT_HEADER, INDEX_FORMAT_VERSION, INDEX_WARNING_PREFIX, IndexEntry, IndexParseError,
    IndexRender, ParsedIndex, parse_index, render_entry_line, render_index, truncate_chars,
};
pub use lock::FileLock;
pub use recall::{
    DropReason, DroppedMemory, MEMORY_CONTEXT_CLOSE, MEMORY_CONTEXT_NOTE, MEMORY_CONTEXT_OPEN,
    MemoryContextMessage, MemorySelector, RecallCandidate, RecallReport, RecallSession,
    RecallStrategy, RecalledMemory, estimate_tokens, heuristic_select,
};
pub use redact::{RedactionReport, redact, redact_secrets};
pub use service::{
    AUDIT_FORMAT_VERSION, AuditRecord, DenyAllGate, MemoryService, MemorySession, MemorySnapshot,
    MemoryWriteGate, SnapshotChange, StagedWrite, TrustedGate, WriteIntent, WriteRequest,
    WriteSource, now_millis, open_project_service, read_audit,
};
pub use store::{
    AppliedOperation, ForgetReport, ForgottenEntry, INDEX_FILE_NAME, Manifest, ManifestEntry,
    MemoryStore, SUMMARY_FILE_NAME, ScanError, StoreSnapshot, TombstoneRecord, WriteBatch,
    WriteOpKind, WriteOperation, WriteReport, read_tombstones,
};
