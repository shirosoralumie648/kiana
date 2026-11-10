//! 后台抽取/整合：watermark、跨进程 lease、每日预算、两阶段与幂等。
//!
//! # 状态机
//!
//! ```text
//! scan persisted sessions
//!   └─ filter: current session / subagent / 太新 / <= watermark
//!        └─ claim lease (state/extraction.lock + state/extraction.json)
//!             └─ Phase 1: transcript -> raw memories + synopsis
//!                  └─ redact secrets
//!                       └─ Phase 2: manifest + draft -> upserts/removes + synopsis
//!                            └─ apply (source_key 幂等, 已存在则跳过)
//!                                 └─ commit: watermark + daily budget + clear lease
//! ```
//!
//! 关键不变量：
//!
//! - 两个进程同时启动时，`flock` + 状态文件的读改写保证**恰好一个** claim；
//! - lease 过期（崩溃）后允许接管，`attempt` 递增；
//! - 应用与 commit 之间崩溃时 watermark 不前进；重跑依据 `source_key` 跳过已写入条目，
//!   不会重复产物；
//! - 当前 session、subagent、过新 session 零抽取；
//! - 每日预算（运行次数 + 写入字节）在 claim 内检查并持久化；
//! - 抽取产物在写入前全部脱敏，summary 同样脱敏。
//!
//! 本模块**不**依赖 `kiana-durable`：lease/watermark 被抽象成
//! [`FileExtractionStore`]，wire 层可以将其替换为 durable 的 task/lease 后端（见
//! `docs/history/porting/handoffs/phase-10.md`），本模块的过滤器与幂等逻辑保持不变。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::config::{ExtractionConfig, MemoryConfig, ScopePaths};
use crate::entry::{MemoryEntry, MemoryEntryDraft, MemoryType, slugify};
use crate::error::{MemoryError, MemoryResult, io_error};
use crate::lock::FileLock;
use crate::redact::redact_secrets;
use crate::service::{MemoryService, WriteSource};
use crate::store::{ManifestEntry, WriteBatch, WriteOperation, atomic_write, backup_file};

/// 抽取状态格式版本。
pub const EXTRACTION_STATE_FORMAT_VERSION: u32 = 1;
/// 一天毫秒数。
pub const DAY_MS: u64 = 86_400_000;

/// 持久化会话候选。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCandidate {
    /// 会话 id。
    pub session_id: String,
    /// 会话文件路径。
    pub path: PathBuf,
    /// 修改时间。
    pub modified_at: SystemTime,
    /// 修改时间（Unix 毫秒）。
    pub modified_ms: u64,
    /// 是否是 subagent 会话（不抽取）。
    pub is_subagent: bool,
}

/// 转录消息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptMessage {
    /// role（user/assistant/tool）。
    pub role: String,
    /// 文本。
    pub text: String,
}

/// 会话转录（Phase 1 输入）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTranscript {
    /// 会话 id。
    pub session_id: String,
    /// 消息。
    pub messages: Vec<TranscriptMessage>,
    /// 是否 subagent。
    pub is_subagent: bool,
}

/// persisted session 来源。wire 层用 `kiana-session-core` 的 `SessionManager` 实现。
///
/// 方法声明为 async：实现可以在 `spawn_blocking` 里做文件 IO，避免大批量会话扫描
/// 阻塞 current-thread runtime。
// `async_trait` 展开的 Future 方法自带 `#[must_use]`，clippy 看不到宏内属性
// （与 kiana-ai 同类处理）。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait SessionSource: Send + Sync {
    /// 扫描所有 persisted sessions（不需要过滤；过滤由 [`plan_extraction`] 负责）。
    async fn scan_sessions(&self) -> MemoryResult<Vec<SessionCandidate>>;
    /// 读取一个会话的转录。
    async fn load_session(&self, session_id: &str) -> MemoryResult<SessionTranscript>;
}

/// Phase 1 抽取出的原始记忆。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawMemory {
    /// 名称。
    pub name: String,
    /// 一行描述。
    pub description: String,
    /// 类型。
    pub mtype: MemoryType,
    /// 正文。
    pub body: String,
    /// 派生来源。
    pub derived_from: Vec<String>,
    /// 幂等键（建议 `session_id:start-end`）。
    pub source_key: Option<String>,
}

/// Phase 1 输出。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtractionDraft {
    /// raw memories。
    pub memories: Vec<RawMemory>,
    /// 会话摘要（第二阶段会合并/重写）。
    pub synopsis: String,
}

/// Phase 2 操作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConsolidationOperation {
    /// 新增或更新（同名条目存在时由 pipeline 转成 replace）。
    Upsert(MemoryEntryDraft),
    /// 删除（例如「完成项移动」）。
    Remove {
        /// 目标 id。
        id: String,
        /// 原因。
        reason: String,
    },
}

/// Phase 2 输出。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConsolidationPlan {
    /// 操作。
    pub operations: Vec<ConsolidationOperation>,
    /// 合并后的摘要。
    pub synopsis: String,
    /// 原样保留的条目数（carry-forward；用于诊断，不影响写盘）。
    pub carried_forward: usize,
}

/// Phase 1：从会话转录抽取 raw memory + synopsis。
// `async_trait` 展开的 Future 方法自带 `#[must_use]`，clippy 看不到宏内属性。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait Phase1Extractor: Send + Sync {
    /// 执行抽取。
    async fn extract(&self, transcript: &SessionTranscript) -> MemoryResult<ExtractionDraft>;
}

/// Phase 2：结合现有 manifest 合并 draft。
///
/// 契约（对应参考设计的 consolidation 指令）：
///
/// - **carry-forward**：没有出现在 plan 里的现有条目原样保留；pipeline 不会因为一次
///   整合而清空旧条目（只执行显式 [`ConsolidationOperation::Remove`]）。
/// - **新对话优先**：同一主题/名称的新 draft 通过 `Upsert` 覆盖旧条目（pipeline 把同名
///   Upsert 转成 `replace`），旧条目的 `derived_from` 由 consolidator 决定是否保留。
/// - **完成项移动**：已完成/过期的项目记忆用 `Remove { reason }` 移出索引，摘要里记录
///   完成状态；不要靠改写正文让它“看起来消失”。
// `async_trait` 展开的 Future 方法自带 `#[must_use]`，clippy 看不到宏内属性。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait Phase2Consolidator: Send + Sync {
    /// 执行整合。
    async fn consolidate(
        &self,
        existing: &[ManifestEntry],
        draft: &ExtractionDraft,
    ) -> MemoryResult<ConsolidationPlan>;
}

/// 跳过原因。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// 当前会话正在写入，跳过。
    CurrentSession,
    /// subagent 会话不抽取。
    Subagent,
    /// 修改时间太新。
    TooNew,
    /// 不晚于 watermark。
    BeforeWatermark,
    /// 单次运行上限。
    RunLimit,
}

impl SkipReason {
    /// 稳定字符串。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CurrentSession => "current_session",
            Self::Subagent => "subagent",
            Self::TooNew => "too_new",
            Self::BeforeWatermark => "before_watermark",
            Self::RunLimit => "run_limit",
        }
    }
}

/// 被跳过的会话。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedSession {
    /// 会话 id。
    pub session_id: String,
    /// 原因。
    pub reason: SkipReason,
}

/// 抽取计划。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtractionPlan {
    /// 按 (mtime, id) 升序排列的待处理会话。
    pub selected: Vec<SessionCandidate>,
    /// 跳过记录。
    pub skipped: Vec<SkippedSession>,
}

/// 每日预算状态。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyState {
    /// UTC 天序号（`now_ms / DAY_MS`）。
    pub day: u64,
    /// 当天运行次数。
    pub runs: u32,
    /// 当天写入字节。
    pub bytes: u64,
}

/// lease 记录。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseRecord {
    /// 持有者。
    pub owner: String,
    /// 获取时间。
    pub acquired_at_ms: u64,
    /// 过期时间。
    pub expires_at_ms: u64,
    /// 第几次尝试（stale takeover 递增）。
    pub attempt: u32,
}

/// 抽取状态（`state/extraction.json`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionState {
    /// 格式版本。
    pub version: u32,
    /// 乐观并发 revision。
    pub revision: u64,
    /// watermark（Unix 毫秒）。
    pub watermark_ms: u64,
    /// watermark 同毫秒时的 tie-break 会话 id。
    pub watermark_session: Option<String>,
    /// 每日预算。
    pub daily: DailyState,
    /// 当前 lease。
    pub lease: Option<LeaseRecord>,
    /// 连续失败次数。
    pub consecutive_failures: u32,
    /// 最后一次错误。
    pub last_error: Option<String>,
    /// 累计处理会话数。
    pub processed_sessions: u64,
}

impl Default for ExtractionState {
    fn default() -> Self {
        Self {
            version: EXTRACTION_STATE_FORMAT_VERSION,
            revision: 0,
            watermark_ms: 0,
            watermark_session: None,
            daily: DailyState::default(),
            lease: None,
            consecutive_failures: 0,
            last_error: None,
            processed_sessions: 0,
        }
    }
}

/// claim 结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// 成功；`stale_takeover` 表示接管了过期 lease。
    Claimed {
        /// lease。
        lease: LeaseRecord,
        /// claim 后的状态 revision。
        revision: u64,
        /// 是否接管了过期 lease。
        stale_takeover: bool,
    },
    /// 另一个进程持有未过期 lease。
    Busy {
        /// 现有 lease。
        lease: LeaseRecord,
    },
}

/// commit 输入。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StateCommit {
    /// 新的 watermark。
    pub watermark_ms: u64,
    /// tie-break 会话 id。
    pub watermark_session: Option<String>,
    /// 本次新增运行次数。
    pub runs_delta: u32,
    /// 本次新增字节。
    pub bytes_delta: u64,
    /// 本次新增已处理会话数。
    pub processed_delta: u64,
    /// 连续失败次数。
    pub consecutive_failures: u32,
    /// 最后一次错误。
    pub last_error: Option<String>,
}

/// lease/watermark 状态存储契约。
///
/// [`FileExtractionStore`] 是独立运行/测试用的实现；wire 层可以实现一个由
/// `kiana-durable` 的任务记录（或等价的 lease 表）支撑的适配器，[`ExtractionJob`] 的
/// 过滤、两阶段、幂等与预算逻辑保持不变。
pub trait ExtractionStateStore: Send + Sync {
    /// 读取状态。
    fn load(&self) -> MemoryResult<ExtractionState>;
    /// 用当前时间 claim。
    fn claim(&self, owner: &str, ttl_ms: u64) -> MemoryResult<ClaimOutcome> {
        self.claim_at(now_millis(), owner, ttl_ms)
    }
    /// 用指定时间 claim。
    fn claim_at(&self, now_ms: u64, owner: &str, ttl_ms: u64) -> MemoryResult<ClaimOutcome>;
    /// 释放 lease。
    fn release(&self, owner: &str) -> MemoryResult<bool>;
    /// 提交 watermark/预算并清空 lease。
    fn commit(
        &self,
        owner: &str,
        expected_revision: u64,
        now_ms: u64,
        commit: StateCommit,
    ) -> MemoryResult<ExtractionState>;
}

/// 基于文件的 lease/watermark 存储。
///
/// 所有读改写都在 [`FileLock`] 内完成；文件本身用 temp+rename 原子替换。
#[derive(Clone, Debug)]
pub struct FileExtractionStore {
    state_path: PathBuf,
    lock_path: PathBuf,
    lock_timeout: Duration,
}

impl FileExtractionStore {
    /// 构造。
    pub fn new(
        state_path: impl Into<PathBuf>,
        lock_path: impl Into<PathBuf>,
        lock_timeout: Duration,
    ) -> Self {
        Self {
            state_path: state_path.into(),
            lock_path: lock_path.into(),
            lock_timeout,
        }
    }

    /// 从 scope 路径构造。
    pub fn for_scope(paths: &ScopePaths, lock_timeout: Duration) -> Self {
        Self::new(
            paths.extraction_state_path.clone(),
            paths.extraction_lock_path.clone(),
            lock_timeout,
        )
    }

    /// 状态文件路径。
    pub fn state_path(&self) -> &Path {
        &self.state_path
    }

    /// 读取状态（无锁；原子替换保证不会读到半个文件）。
    pub fn load(&self) -> MemoryResult<ExtractionState> {
        self.read_state()
    }

    /// 用当前时间 claim。
    pub fn claim(&self, owner: &str, ttl_ms: u64) -> MemoryResult<ClaimOutcome> {
        self.claim_at(now_millis(), owner, ttl_ms)
    }

    /// 用指定时间 claim（测试可注入时钟）。
    pub fn claim_at(&self, now_ms: u64, owner: &str, ttl_ms: u64) -> MemoryResult<ClaimOutcome> {
        let _lock = self.lock()?;
        let mut state = self.read_state()?;
        state.version = EXTRACTION_STATE_FORMAT_VERSION;
        let day = now_ms / DAY_MS;
        if state.daily.day != day {
            state.daily = DailyState {
                day,
                runs: 0,
                bytes: 0,
            };
            state.consecutive_failures = 0;
            state.last_error = None;
        }
        if let Some(lease) = &state.lease
            && lease.expires_at_ms > now_ms
        {
            return Ok(ClaimOutcome::Busy {
                lease: lease.clone(),
            });
        }
        let stale_takeover = state.lease.is_some();
        let attempt = state
            .lease
            .as_ref()
            .map(|lease| lease.attempt.saturating_add(1))
            .unwrap_or(1);
        let lease = LeaseRecord {
            owner: owner.to_string(),
            acquired_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(ttl_ms),
            attempt,
        };
        state.lease = Some(lease.clone());
        state.revision = state.revision.saturating_add(1);
        self.write_state(&state)?;
        Ok(ClaimOutcome::Claimed {
            lease,
            revision: state.revision,
            stale_takeover,
        })
    }

    /// 释放 lease（只有 owner 匹配时）。
    pub fn release(&self, owner: &str) -> MemoryResult<bool> {
        let _lock = self.lock()?;
        let mut state = self.read_state()?;
        let matches = state
            .lease
            .as_ref()
            .map(|lease| lease.owner == owner)
            .unwrap_or(false);
        if matches {
            state.lease = None;
            state.revision = state.revision.saturating_add(1);
            self.write_state(&state)?;
        }
        Ok(matches)
    }

    /// 提交 watermark/预算并清空 lease；`expected_revision` 做乐观并发。
    pub fn commit(
        &self,
        owner: &str,
        expected_revision: u64,
        now_ms: u64,
        commit: StateCommit,
    ) -> MemoryResult<ExtractionState> {
        let _lock = self.lock()?;
        let mut state = self.read_state()?;
        if state.revision != expected_revision {
            return Err(MemoryError::RevisionConflict {
                expected: expected_revision,
                actual: state.revision,
            });
        }
        match &state.lease {
            Some(lease) if lease.owner == owner => {}
            Some(lease) => {
                return Err(MemoryError::LeaseBusy {
                    owner: lease.owner.clone(),
                    expires_at_ms: lease.expires_at_ms,
                });
            }
            None => {
                return Err(MemoryError::InvalidArgument(
                    "cannot commit extraction state without a lease".into(),
                ));
            }
        }
        // watermark 只能前进。同一毫秒内用会话 id 字典序做 tie-break；本次没有处理
        // 任何会话（`watermark_session == None`）时必须保留已有 tie-break，否则同毫秒
        // 的旧会话会被重新判定为“未处理”并重复抽取。
        let advance = match commit.watermark_ms.cmp(&state.watermark_ms) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Less => false,
            std::cmp::Ordering::Equal => match (
                commit.watermark_session.as_deref(),
                state.watermark_session.as_deref(),
            ) {
                (Some(next), Some(current)) => next > current,
                (Some(_), None) => true,
                (None, _) => false,
            },
        };
        if advance {
            state.watermark_ms = commit.watermark_ms;
            state.watermark_session = commit.watermark_session;
        }
        let day = now_ms / DAY_MS;
        if state.daily.day != day {
            state.daily = DailyState {
                day,
                runs: 0,
                bytes: 0,
            };
        }
        state.daily.runs = state.daily.runs.saturating_add(commit.runs_delta);
        state.daily.bytes = state.daily.bytes.saturating_add(commit.bytes_delta);
        state.processed_sessions = state
            .processed_sessions
            .saturating_add(commit.processed_delta);
        state.consecutive_failures = commit.consecutive_failures;
        state.last_error = commit.last_error;
        state.lease = None;
        state.revision = state.revision.saturating_add(1);
        self.write_state(&state)?;
        Ok(state)
    }

    fn lock(&self) -> MemoryResult<FileLock> {
        FileLock::acquire(&self.lock_path, self.lock_timeout)
    }

    fn read_state(&self) -> MemoryResult<ExtractionState> {
        let bytes = match fs::read(&self.state_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ExtractionState::default());
            }
            Err(error) => return Err(io_error(&self.state_path, error)),
        };
        if bytes.is_empty() {
            return Ok(ExtractionState::default());
        }
        match serde_json::from_slice::<ExtractionState>(&bytes) {
            Ok(state) => {
                if state.version > EXTRACTION_STATE_FORMAT_VERSION {
                    return Err(MemoryError::UnsupportedFormat {
                        path: self.state_path.clone(),
                        version: state.version,
                    });
                }
                Ok(state)
            }
            Err(error) => {
                // 状态文件损坏（例如进程崩溃时写了一半）：备份后重置，后台任务不能因此
                // 永久停摆。它与用户记忆文件不同，不包含需要人工合并的内容。
                let _ = backup_file(&self.state_path);
                let _ = error;
                Ok(ExtractionState::default())
            }
        }
    }

    fn write_state(&self, state: &ExtractionState) -> MemoryResult<()> {
        let bytes = serde_json::to_vec_pretty(state)?;
        atomic_write(&self.state_path, &bytes)
    }
}

impl ExtractionStateStore for FileExtractionStore {
    fn load(&self) -> MemoryResult<ExtractionState> {
        FileExtractionStore::load(self)
    }

    fn claim_at(&self, now_ms: u64, owner: &str, ttl_ms: u64) -> MemoryResult<ClaimOutcome> {
        FileExtractionStore::claim_at(self, now_ms, owner, ttl_ms)
    }

    fn release(&self, owner: &str) -> MemoryResult<bool> {
        FileExtractionStore::release(self, owner)
    }

    fn commit(
        &self,
        owner: &str,
        expected_revision: u64,
        now_ms: u64,
        commit: StateCommit,
    ) -> MemoryResult<ExtractionState> {
        FileExtractionStore::commit(self, owner, expected_revision, now_ms, commit)
    }
}

/// 判断候选是否在 watermark 之后。
pub fn is_after_watermark(state: &ExtractionState, candidate: &SessionCandidate) -> bool {
    if candidate.modified_ms > state.watermark_ms {
        return true;
    }
    if candidate.modified_ms < state.watermark_ms {
        return false;
    }
    match (&state.watermark_session, candidate.session_id.as_str()) {
        (Some(current), next) => next > current.as_str(),
        (None, _) => true,
    }
}

/// 计算本次抽取计划。
pub fn plan_extraction(
    mut candidates: Vec<SessionCandidate>,
    state: &ExtractionState,
    now_ms: u64,
    current_session_id: Option<&str>,
    config: &ExtractionConfig,
) -> ExtractionPlan {
    candidates.sort_by(|left, right| {
        left.modified_ms
            .cmp(&right.modified_ms)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    let mut plan = ExtractionPlan::default();
    for candidate in candidates {
        let skip = if current_session_id == Some(candidate.session_id.as_str()) {
            Some(SkipReason::CurrentSession)
        } else if candidate.is_subagent {
            Some(SkipReason::Subagent)
        } else if now_ms.saturating_sub(candidate.modified_ms) < config.min_session_age_ms {
            Some(SkipReason::TooNew)
        } else if !is_after_watermark(state, &candidate) {
            Some(SkipReason::BeforeWatermark)
        } else if plan.selected.len() >= config.max_sessions_per_run {
            Some(SkipReason::RunLimit)
        } else {
            None
        };
        match skip {
            Some(reason) => plan.skipped.push(SkippedSession {
                session_id: candidate.session_id,
                reason,
            }),
            None => plan.selected.push(candidate),
        }
    }
    plan
}

/// 检查每日预算。
pub fn check_daily_budget(
    state: &ExtractionState,
    config: &ExtractionConfig,
    now_ms: u64,
) -> MemoryResult<()> {
    let day = now_ms / DAY_MS;
    if state.daily.day != day {
        return Ok(());
    }
    if state.daily.runs >= config.max_runs_per_day || state.daily.bytes >= config.max_bytes_per_day
    {
        return Err(MemoryError::DailyBudgetExhausted {
            runs: state.daily.runs,
            max_runs: config.max_runs_per_day,
            bytes: state.daily.bytes,
            max_bytes: config.max_bytes_per_day,
        });
    }
    Ok(())
}

/// 抽取拒绝/失败的测试注入点。
pub trait ExtractionHooks: Send + Sync {
    /// 在应用成功后、commit 之前调用。返回 `Err` 模拟「应用完成但进程崩溃」：
    /// lease 不释放、watermark 不前进。
    fn before_commit(&self, _session_id: &str) -> MemoryResult<()> {
        Ok(())
    }
}

/// 默认无 hook。
#[derive(Clone, Copy, Debug, Default)]
pub struct NoHooks;

impl ExtractionHooks for NoHooks {}

/// 抽取状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtractionStatus {
    /// 至少处理了一个会话。
    Extracted,
    /// 没有符合条件的会话。
    NoSessions,
    /// 配置关闭。
    SkippedDisabled,
    /// 另一个进程持有 lease。
    SkippedLeaseBusy,
    /// 每日预算用尽。
    SkippedBudget,
    /// 连续失败达到上限。
    SkippedRetry,
    /// provider/会话读取失败。
    Failed,
}

/// 一次运行的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractionOutcome {
    /// 状态。
    pub status: ExtractionStatus,
    /// 扫描到的会话数。
    pub sessions_seen: usize,
    /// 处理的会话数。
    pub sessions_processed: usize,
    /// 写入的条目数。
    pub entries_written: usize,
    /// 写入字节。
    pub bytes_written: u64,
    /// 跳过记录。
    pub skipped: Vec<SkippedSession>,
    /// 错误（provider/session）。
    pub errors: Vec<String>,
    /// commit 后的 revision（未 commit 为 `None`）。
    pub revision: Option<u64>,
    /// commit 后的 watermark。
    pub watermark_ms: u64,
}

/// 抽取 job。
pub struct ExtractionJob<'a> {
    /// 写入服务。
    pub service: &'a MemoryService,
    /// 状态存储。
    pub state: Arc<dyn ExtractionStateStore>,
    /// 会话来源。
    pub source: &'a dyn SessionSource,
    /// Phase 1。
    pub extractor: &'a dyn Phase1Extractor,
    /// Phase 2。
    pub consolidator: &'a dyn Phase2Consolidator,
    /// 配置。
    pub config: &'a MemoryConfig,
    /// 当前会话 id。
    pub current_session_id: Option<String>,
    /// lease owner（默认 `pid-nanos`）。
    pub owner: String,
    /// 测试 hook。
    pub hooks: &'a dyn ExtractionHooks,
}

impl<'a> ExtractionJob<'a> {
    /// 构造，owner 使用进程 id + 时间。
    pub fn new(
        service: &'a MemoryService,
        state: Arc<dyn ExtractionStateStore>,
        source: &'a dyn SessionSource,
        extractor: &'a dyn Phase1Extractor,
        consolidator: &'a dyn Phase2Consolidator,
        config: &'a MemoryConfig,
    ) -> Self {
        Self {
            service,
            state,
            source,
            extractor,
            consolidator,
            config,
            current_session_id: None,
            owner: default_owner(),
            hooks: &NoHooks,
        }
    }

    /// 用当前时间运行。
    pub async fn run(&self) -> MemoryResult<ExtractionOutcome> {
        self.run_at(now_millis()).await
    }

    /// 用指定时间运行（测试注入时钟）。
    pub async fn run_at(&self, now_ms: u64) -> MemoryResult<ExtractionOutcome> {
        if !self.config.auto_extract {
            return Ok(empty_outcome(ExtractionStatus::SkippedDisabled));
        }
        let extraction = &self.config.extraction;
        let claim = self
            .state
            .claim_at(now_ms, &self.owner, extraction.lease_ttl_ms)?;
        let (lease, revision, stale_takeover) = match claim {
            ClaimOutcome::Busy { lease } => {
                let mut outcome = empty_outcome(ExtractionStatus::SkippedLeaseBusy);
                outcome.errors.push(format!(
                    "lease held by {} until {}",
                    lease.owner, lease.expires_at_ms
                ));
                return Ok(outcome);
            }
            ClaimOutcome::Claimed {
                lease,
                revision,
                stale_takeover,
            } => (lease, revision, stale_takeover),
        };
        let _ = &lease;
        let state = match self.state.load() {
            Ok(state) => state,
            Err(error) => {
                // claim 已持久化 lease；load 失败也必须释放，否则后续抽取会被阻塞到
                // TTL 过期。
                let _ = self.state.release(&self.owner);
                return Err(error);
            }
        };
        if stale_takeover && state.consecutive_failures > 0 {
            // 接管本身已经递增 attempt；失败计数保留，用于 RetryExhausted。
        }
        if state.consecutive_failures >= extraction.max_consecutive_failures {
            self.state.release(&self.owner)?;
            let mut outcome = empty_outcome(ExtractionStatus::SkippedRetry);
            outcome.errors.push(
                state
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "retry limit reached".into()),
            );
            return Ok(outcome);
        }
        if let Err(error) = check_daily_budget(&state, extraction, now_ms) {
            self.state.release(&self.owner)?;
            let mut outcome = empty_outcome(ExtractionStatus::SkippedBudget);
            outcome.errors.push(error.to_string());
            return Ok(outcome);
        }

        let candidates = match self.source.scan_sessions().await {
            Ok(candidates) => candidates,
            Err(error) => {
                // 扫描错误也必须计入连续失败，否则目录权限/损坏文件会让每次启动都
                // 重新尝试，永远到不了 `max_consecutive_failures` 的退避。
                let message = redact_secrets(&error.to_string()).text;
                let commit = StateCommit {
                    watermark_ms: state.watermark_ms,
                    watermark_session: state.watermark_session.clone(),
                    runs_delta: 0,
                    bytes_delta: 0,
                    processed_delta: 0,
                    consecutive_failures: state.consecutive_failures.saturating_add(1),
                    last_error: Some(message),
                };
                if self
                    .state
                    .commit(&self.owner, revision, now_ms, commit)
                    .is_err()
                {
                    // commit 失败时 lease 仍在，不能让它泄漏到 TTL。
                    let _ = self.state.release(&self.owner);
                }
                return Err(error);
            }
        };
        let sessions_seen = candidates.len();
        let plan = plan_extraction(
            candidates,
            &state,
            now_ms,
            self.current_session_id.as_deref(),
            extraction,
        );
        let skipped = plan.skipped;
        if plan.selected.is_empty() {
            self.state.release(&self.owner)?;
            let mut outcome = empty_outcome(ExtractionStatus::NoSessions);
            outcome.sessions_seen = sessions_seen;
            outcome.skipped = skipped;
            outcome.watermark_ms = state.watermark_ms;
            return Ok(outcome);
        }

        let mut sessions_processed = 0usize;
        let mut entries_written = 0usize;
        let mut bytes_written = 0u64;
        let mut errors = Vec::new();
        let mut last_watermark_ms = state.watermark_ms;
        let mut last_watermark_session = state.watermark_session.clone();
        let mut last_synopsis = String::new();
        let mut failed = false;

        for candidate in &plan.selected {
            let transcript = match self.source.load_session(&candidate.session_id).await {
                Ok(transcript) => transcript,
                Err(error) => {
                    errors.push(
                        redact_secrets(&format!("load {}: {error}", candidate.session_id)).text,
                    );
                    failed = true;
                    break;
                }
            };
            let draft = match self.extractor.extract(&transcript).await {
                Ok(draft) => draft,
                Err(error) => {
                    errors.push(
                        redact_secrets(&format!("extract {}: {error}", candidate.session_id)).text,
                    );
                    failed = true;
                    break;
                }
            };
            let draft = redact_draft(draft);
            if !draft.synopsis.trim().is_empty() {
                last_synopsis = draft.synopsis.clone();
            }
            let existing = match self.service.store().scan_manifest() {
                Ok(manifest) => manifest.entries,
                Err(error) => {
                    errors.push(
                        redact_secrets(&format!("manifest {}: {error}", candidate.session_id)).text,
                    );
                    failed = true;
                    break;
                }
            };
            let plan = match self.consolidator.consolidate(&existing, &draft).await {
                Ok(plan) => plan,
                Err(error) => {
                    errors.push(
                        redact_secrets(&format!("consolidate {}: {error}", candidate.session_id))
                            .text,
                    );
                    failed = true;
                    break;
                }
            };
            if !plan.synopsis.trim().is_empty() {
                last_synopsis = redact_secrets(&plan.synopsis).text;
            }
            let (batch, batch_bytes) = build_batch(&existing, &plan);
            if !batch.is_empty() {
                match self.service.apply(batch, WriteSource::Extraction) {
                    Ok(report) => {
                        entries_written += report.applied.len();
                        bytes_written = bytes_written.saturating_add(batch_bytes);
                        last_watermark_ms = candidate.modified_ms;
                        last_watermark_session = Some(candidate.session_id.clone());
                        sessions_processed += 1;
                    }
                    Err(error) => {
                        errors.push(
                            redact_secrets(&format!("apply {}: {error}", candidate.session_id))
                                .text,
                        );
                        failed = true;
                        break;
                    }
                }
            } else {
                last_watermark_ms = candidate.modified_ms;
                last_watermark_session = Some(candidate.session_id.clone());
                sessions_processed += 1;
            }
            // 模拟崩溃：故意不 release lease、不 commit watermark。
            self.hooks.before_commit(&candidate.session_id)?;
        }

        if !last_synopsis.trim().is_empty()
            && let Err(error) =
                self.service
                    .write_summary(&last_synopsis, WriteSource::Extraction, now_ms)
        {
            let _ = self.state.release(&self.owner);
            return Err(error);
        }

        let commit = StateCommit {
            watermark_ms: last_watermark_ms,
            watermark_session: last_watermark_session,
            runs_delta: if sessions_processed > 0 { 1 } else { 0 },
            bytes_delta: bytes_written,
            processed_delta: sessions_processed as u64,
            consecutive_failures: if failed {
                state.consecutive_failures.saturating_add(1)
            } else {
                0
            },
            last_error: errors.last().cloned(),
        };
        let committed = match self.state.commit(&self.owner, revision, now_ms, commit) {
            Ok(state) => state,
            Err(error) => {
                let _ = self.state.release(&self.owner);
                return Err(error);
            }
        };
        Ok(ExtractionOutcome {
            status: if failed {
                ExtractionStatus::Failed
            } else {
                ExtractionStatus::Extracted
            },
            sessions_seen,
            sessions_processed,
            entries_written,
            bytes_written,
            skipped,
            errors,
            revision: Some(committed.revision),
            watermark_ms: committed.watermark_ms,
        })
    }
}

/// 与 `store::unique_id` 相同的分配规则：base 已被占用时追加 `-2`、`-3`…
///
/// `build_batch` 在写入前预测 id（source_key 幂等映射），必须与 store 实际分配一致。
fn allocate_entry_id(base: &str, known: &HashSet<String>) -> String {
    if !known.contains(base) {
        return base.to_string();
    }
    for suffix in 2..=u32::MAX {
        let candidate = format!("{base}-{suffix}");
        let candidate = if candidate.len() > 64 {
            let keep = 64 - suffix.to_string().len() - 1;
            format!("{}-{suffix}", &base[..keep.min(base.len())])
        } else {
            candidate
        };
        if !known.contains(&candidate) {
            return candidate;
        }
    }
    format!("{base}-{}", now_millis())
}

fn build_batch(existing: &[ManifestEntry], plan: &ConsolidationPlan) -> (WriteBatch, u64) {
    let mut batch = WriteBatch::new();
    let mut bytes = 0u64;
    let mut by_name: std::collections::HashMap<String, String> = existing
        .iter()
        .map(|entry| (entry.name.clone(), entry.id.clone()))
        .collect();
    let mut source_key_ids: std::collections::HashMap<String, String> = existing
        .iter()
        .filter_map(|entry| entry.source_key.clone().map(|key| (key, entry.id.clone())))
        .collect();
    let mut known_ids: HashSet<String> = existing.iter().map(|entry| entry.id.clone()).collect();
    let mut planned_names: HashSet<String> = HashSet::new();
    for operation in &plan.operations {
        match operation {
            ConsolidationOperation::Upsert(entry) => {
                if !planned_names.insert(entry.name.clone()) {
                    // 同一次计划里同名只保留第一个；consolidator 应该已经合并。
                    continue;
                }
                // 已有记忆优先按 source_key（同一段会话的幂等键）或 name 找回 id。
                let known_id = entry
                    .source_key
                    .as_ref()
                    .and_then(|key| source_key_ids.get(key).cloned())
                    .or_else(|| by_name.get(&entry.name).cloned());
                let id = match known_id {
                    Some(id) => id,
                    None => {
                        // 同一批次的 Add 也必须按 `known_ids` 预测 store 的 `unique_id`，
                        // 否则 slug 与既有条目冲突时 source_key 会指向另一个 id。
                        let id = allocate_entry_id(&slugify(&entry.name), &known_ids);
                        if let Some(key) = &entry.source_key {
                            source_key_ids.insert(key.clone(), id.clone());
                        }
                        by_name.insert(entry.name.clone(), id.clone());
                        let size = MemoryEntry::from_draft(entry.clone(), id.clone(), 1)
                            .to_file_bytes()
                            .len() as u64;
                        bytes = bytes.saturating_add(size);
                        batch.operations.push(WriteOperation::Add(entry.clone()));
                        known_ids.insert(id);
                        continue;
                    }
                };
                let size = MemoryEntry::from_draft(entry.clone(), id.clone(), 1)
                    .to_file_bytes()
                    .len() as u64;
                bytes = bytes.saturating_add(size);
                batch.operations.push(WriteOperation::Replace {
                    id: id.clone(),
                    entry: entry.clone(),
                });
                known_ids.insert(id);
            }
            ConsolidationOperation::Remove { id, .. } => {
                if known_ids.contains(id) {
                    batch
                        .operations
                        .push(WriteOperation::Remove { id: id.clone() });
                    known_ids.remove(id);
                }
            }
        }
    }
    (batch, bytes)
}

fn redact_draft(draft: ExtractionDraft) -> ExtractionDraft {
    ExtractionDraft {
        memories: draft
            .memories
            .into_iter()
            .map(|memory| {
                let name = redact_secrets(&memory.name);
                let description = redact_secrets(&memory.description);
                let body = redact_secrets(&memory.body);
                let source_key = memory.source_key.as_deref().map(redact_secrets);
                // derived_from 完全来自模型输出（会话 id / 路径等），同样可能夹带凭据。
                // 它同时受 `is_valid_id` 约束：脱敏后的 `[REDACTED]` 不是合法 id，会把
                // 整条记忆打成永久 `InvalidField`，因此替换过的项固定写成合法占位符。
                let derived_from = memory
                    .derived_from
                    .into_iter()
                    .map(|value| {
                        let report = redact_secrets(&value);
                        if report.replacements > 0 {
                            "redacted".to_string()
                        } else {
                            report.text
                        }
                    })
                    .collect();
                RawMemory {
                    name: name.text,
                    description: description.text,
                    mtype: memory.mtype,
                    body: body.text,
                    derived_from,
                    source_key: source_key.map(|report| report.text),
                }
            })
            .collect(),
        synopsis: redact_secrets(&draft.synopsis).text,
    }
}

fn empty_outcome(status: ExtractionStatus) -> ExtractionOutcome {
    ExtractionOutcome {
        status,
        sessions_seen: 0,
        sessions_processed: 0,
        entries_written: 0,
        bytes_written: 0,
        skipped: Vec::new(),
        errors: Vec::new(),
        revision: None,
        watermark_ms: 0,
    }
}

static OWNER_COUNTER: AtomicU64 = AtomicU64::new(0);

fn default_owner() -> String {
    format!(
        "{}-{}-{}",
        std::process::id(),
        now_millis(),
        OWNER_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// 当前 Unix 毫秒。
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// 系统时间转毫秒。
pub fn system_time_millis(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, ms: u64, subagent: bool) -> SessionCandidate {
        SessionCandidate {
            session_id: id.into(),
            path: PathBuf::from(format!("/tmp/{id}.jsonl")),
            modified_at: UNIX_EPOCH + Duration::from_millis(ms),
            modified_ms: ms,
            is_subagent: subagent,
        }
    }

    #[test]
    fn plan_filters_current_subagent_and_too_new() {
        let config = ExtractionConfig::default();
        let state = ExtractionState::default();
        let plan = plan_extraction(
            vec![
                candidate("current", 10_000_000, false),
                candidate("sub", 10_000_000, true),
                candidate("fresh", 20_000_000, false),
                candidate("ok", 5_000_000, false),
            ],
            &state,
            20_000_000,
            Some("current"),
            &config,
        );
        assert_eq!(plan.selected.len(), 1);
        assert_eq!(plan.selected[0].session_id, "ok");
        assert!(
            plan.skipped
                .iter()
                .any(|s| s.reason == SkipReason::CurrentSession)
        );
        assert!(
            plan.skipped
                .iter()
                .any(|s| s.reason == SkipReason::Subagent)
        );
        assert!(plan.skipped.iter().any(|s| s.reason == SkipReason::TooNew));
    }

    #[test]
    fn watermark_uses_session_tiebreak() {
        let mut state = ExtractionState {
            watermark_ms: 100,
            watermark_session: Some("b".into()),
            ..Default::default()
        };
        assert!(!is_after_watermark(&state, &candidate("a", 100, false)));
        assert!(is_after_watermark(&state, &candidate("c", 100, false)));
        state.watermark_session = None;
        assert!(is_after_watermark(&state, &candidate("a", 100, false)));
    }

    #[test]
    fn commit_keeps_the_existing_session_tiebreak_when_none_is_provided() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileExtractionStore::new(
            dir.path().join("state.json"),
            dir.path().join("state.lock"),
            Duration::from_millis(200),
        );
        let ClaimOutcome::Claimed { revision, .. } = store.claim_at(1, "owner", 1_000).unwrap()
        else {
            panic!("first claim");
        };
        store
            .commit(
                "owner",
                revision,
                2,
                StateCommit {
                    watermark_ms: 100,
                    watermark_session: Some("b".into()),
                    ..StateCommit::default()
                },
            )
            .unwrap();
        let ClaimOutcome::Claimed { revision, .. } = store.claim_at(3, "owner", 1_000).unwrap()
        else {
            panic!("second claim");
        };
        let state = store
            .commit(
                "owner",
                revision,
                4,
                StateCommit {
                    watermark_ms: 100,
                    watermark_session: None,
                    ..StateCommit::default()
                },
            )
            .unwrap();
        assert_eq!(state.watermark_ms, 100);
        assert_eq!(state.watermark_session.as_deref(), Some("b"));
    }

    #[test]
    fn same_batch_source_key_collision_targets_the_new_unique_id() {
        let existing = vec![ManifestEntry {
            id: "deploy-freeze".into(),
            name: "Unrelated".into(),
            description: String::new(),
            mtype: None,
            revision: 1,
            derived_from: Vec::new(),
            source_key: None,
            path: PathBuf::new(),
            relative_path: String::new(),
            mtime: UNIX_EPOCH,
            mtime_ms: 0,
            age_days: 0,
        }];
        let draft = |name: &str| MemoryEntryDraft {
            name: name.into(),
            description: "d".into(),
            mtype: MemoryType::User,
            body: "b".into(),
            derived_from: Vec::new(),
            source_key: Some("session:0-1".into()),
        };
        let plan = ConsolidationPlan {
            operations: vec![
                ConsolidationOperation::Upsert(draft("Deploy freeze")),
                ConsolidationOperation::Upsert(draft("Deploy freeze two")),
            ],
            synopsis: String::new(),
            carried_forward: 0,
        };
        let (batch, _) = build_batch(&existing, &plan);
        assert!(matches!(batch.operations[0], WriteOperation::Add(_)));
        match &batch.operations[1] {
            WriteOperation::Replace { id, .. } => assert_eq!(
                id, "deploy-freeze-2",
                "the second operation must not target the unrelated existing entry"
            ),
            other => panic!("unexpected operation: {other:?}"),
        }
    }

    #[test]
    fn stale_lease_is_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let state = FileExtractionStore::new(
            dir.path().join("state.json"),
            dir.path().join("state.lock"),
            Duration::from_millis(200),
        );
        let first = state.claim_at(1_000, "a", 5_000).unwrap();
        assert!(matches!(first, ClaimOutcome::Claimed { .. }));
        let busy = state.claim_at(2_000, "b", 5_000).unwrap();
        assert!(matches!(busy, ClaimOutcome::Busy { .. }));
        let takeover = state.claim_at(7_000, "b", 5_000).unwrap();
        assert!(matches!(
            takeover,
            ClaimOutcome::Claimed {
                stale_takeover: true,
                ..
            }
        ));
    }
}
