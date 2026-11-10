//! 执行所有权：per-session 单 execution、busy period 事务、崩溃恢复与尝试计数。
//!
//! 同一会话同一时刻只能有一个执行者。开始一轮 busy period 时，`ExecutionClaim`
//! 与 `Execution.Started` 事件在**同一个事务**里落盘；正常终态释放 claim 并发布
//! terminal 事件（每个 execution 恰好一次）；clean shutdown 故意保留 claim，让下次
//! 启动能区分“正常退出”和“崩溃”，且不消耗 attempt。
//!
//! 启动 sweep 按 claim 生成 [`SweepAction::Resume`] 或 [`SweepAction::Terminalize`]：
//!
//! - claim 存在且 `clean_shutdown`：直接 Resume，attempt 不变；
//! - 否则 attempt + 1（durably）；超过 `max_attempts`（默认 10）时 terminalize，
//!   原因固定为 `execution_attempts_exhausted`；
//! - terminalize 只发布一次 terminal 事件；重复 sweep 看不到 claim，不会重复发布。
//!
//! 存储：
//! - [`MemoryExecutionStore`]：单进程参考实现（测试/嵌入）；
//! - [`FileExecutionStore`]：每 session 一个 JSON 状态文件，所有读-改-写在
//!   `flock(LOCK_EX)` 下完成，跨进程 acquire 也只有一个赢家。没有引入新依赖：
//!   `flock` 通过 `extern "C"` 声明（参考 week11a 的 `ioctl`/`mount` 做法）。
//!
//! 参考设计：
//! - `reference_project/codex/codex-rs/core/src/tasks/mod.rs` 的 turn 终态与
//!   background process 生命周期（终态只发一次）；
//! - `reference_project/qwen-code/docs/design/cancelled-turn-recovery.md` 的
//!   “重试/继续只记录必要的执行尝试，显式取消阻止恢复”；
//! - `reference_project/hermes-agent/tools/checkpoint_maintenance.py` 的
//!   持久化计数与保守清理。
//!
//! 接线说明：`kiana-durable` 的 inbox/scheduler 可以在 wire 阶段实现
//! [`ExecutionStore`]，把同一份 claim/event 放进它的任务事务里；本模块不另造队列。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::subagent::result::sha256_hex;

/// 默认最大尝试次数。
pub const DEFAULT_MAX_EXECUTION_ATTEMPTS: u32 = 10;
/// 状态文件里保留的事件上限（terminal 事件在末尾）。
const MAX_EVENTS: usize = 256;

/// 执行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    /// 运行中。
    Running,
    /// 正常完成。
    Completed,
    /// 失败。
    Failed,
    /// 取消。
    Cancelled,
    /// 超时。
    TimedOut,
}

impl ExecutionStatus {
    /// 是否是终态。
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }

    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
        }
    }
}

/// 执行所有权 claim。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionClaim {
    /// 会话 id。
    pub session_id: String,
    /// execution id（每次 acquire 生成）。
    pub execution_id: String,
    /// 拥有者（进程/主机标识）。
    pub owner: String,
    /// 进程 id（可选）。
    pub pid: Option<u32>,
    /// 开始时间（Unix 毫秒）。
    pub started_at_ms: u64,
    /// 已尝试次数（首次为 1）。
    pub attempts: u32,
    /// 是否由 clean shutdown 保留。
    pub clean_shutdown: bool,
}

/// 执行事件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionEvent {
    /// busy period 开始（与 claim 同一事务写入）。
    Started {
        /// 会话内事件序号（1-based）。
        seq: u64,
        /// 时间。
        at_ms: u64,
        /// execution id。
        execution_id: String,
        /// owner。
        owner: String,
        /// 尝试次数。
        attempt: u32,
    },
    /// 崩溃后恢复。
    Resumed {
        /// 序号。
        seq: u64,
        /// 时间。
        at_ms: u64,
        /// execution id。
        execution_id: String,
        /// 新的尝试次数。
        attempt: u32,
    },
    /// 终态（每个 execution 只发布一次）。
    Terminal {
        /// 序号。
        seq: u64,
        /// 时间。
        at_ms: u64,
        /// execution id。
        execution_id: String,
        /// 终态。
        status: ExecutionStatus,
        /// 原因（超限/失败详情）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl ExecutionEvent {
    /// 事件序号。
    pub fn seq(&self) -> u64 {
        match self {
            Self::Started { seq, .. } | Self::Resumed { seq, .. } | Self::Terminal { seq, .. } => {
                *seq
            }
        }
    }

    /// 事件时间。
    pub fn at_ms(&self) -> u64 {
        match self {
            Self::Started { at_ms, .. }
            | Self::Resumed { at_ms, .. }
            | Self::Terminal { at_ms, .. } => *at_ms,
        }
    }
}

/// 一个 session 的持久化状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionState {
    /// 会话 id。
    pub session_id: String,
    /// 当前活跃 claim。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<ExecutionClaim>,
    /// 事件历史（最多保留最后 256 条）。
    #[serde(default)]
    pub events: Vec<ExecutionEvent>,
    /// 已发布的 terminal 事件数。
    #[serde(default)]
    pub terminal_seq: u64,
}

impl ExecutionState {
    /// 空状态。
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            claim: None,
            events: Vec::new(),
            terminal_seq: 0,
        }
    }

    /// 最后一条事件。
    pub fn last_event(&self) -> Option<&ExecutionEvent> {
        self.events.last()
    }

    fn next_seq(&self) -> u64 {
        self.events.last().map(ExecutionEvent::seq).unwrap_or(0) + 1
    }

    fn push_event(&mut self, event: ExecutionEvent) {
        self.events.push(event);
        if self.events.len() > MAX_EVENTS {
            let excess = self.events.len() - MAX_EVENTS;
            self.events.drain(..excess);
        }
    }
}

/// acquire 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireOutcome {
    /// 成功获得执行权。
    Acquired(ExecutionClaim),
    /// 已有执行者。
    Busy(ExecutionClaim),
}

/// finish 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishOutcome {
    /// 是否释放了 claim。
    pub released: bool,
    /// 本次新发布的 terminal 事件（重复 finish 为 `None`）。
    pub terminal_event: Option<ExecutionEvent>,
}

/// sweep 动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepAction {
    /// 恢复执行（adopt 现有 claim）。
    Resume {
        /// 现有 claim。
        claim: ExecutionClaim,
        /// 本次生效的 attempt。
        attempt: u32,
    },
    /// 尝试次数超限，写入终态。
    Terminalize {
        /// execution id。
        execution_id: String,
        /// 原因。
        reason: String,
        /// 新发布的 terminal 事件（恰好一次）。
        terminal_event: Option<ExecutionEvent>,
    },
}

/// sweep 结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepOutcome {
    /// 本次 sweep 的动作（没有活跃 claim 时为空）。
    pub actions: Vec<SweepAction>,
    /// sweep 后的状态。
    pub state: ExecutionState,
}

/// 存储错误。
#[derive(Debug, thiserror::Error)]
pub enum ExecutionStoreError {
    /// IO 错误。
    #[error("execution store io error: {0}")]
    Io(#[from] std::io::Error),
    /// 状态文件损坏。
    #[error("execution state is corrupt: {0}")]
    Corrupt(String),
    /// claim 不存在。
    #[error("no active execution claim for session {session_id}")]
    ClaimNotFound {
        /// 会话 id。
        session_id: String,
    },
    /// claim 的 execution id 不匹配。
    #[error("execution id mismatch for session {session_id}: expected {expected}, got {actual}")]
    ExecutionMismatch {
        /// 会话 id。
        session_id: String,
        /// 期望的 id。
        expected: String,
        /// 实际 id。
        actual: String,
    },
}

/// 执行状态存储。
pub trait ExecutionStore: Send + Sync {
    /// 开始 busy period：claim + Started 在同一事务里写入。
    fn begin(&self, claim: ExecutionClaim) -> Result<AcquireOutcome, ExecutionStoreError>;
    /// 写入终态并释放 claim。
    fn finish(
        &self,
        session_id: &str,
        execution_id: &str,
        status: ExecutionStatus,
        reason: Option<String>,
    ) -> Result<FinishOutcome, ExecutionStoreError>;
    /// clean shutdown：保留 claim，并标记为 clean。
    fn keep_on_clean_shutdown(
        &self,
        session_id: &str,
        execution_id: &str,
    ) -> Result<bool, ExecutionStoreError>;
    /// 读取状态。
    fn load(&self, session_id: &str) -> Result<Option<ExecutionState>, ExecutionStoreError>;
    /// 启动 sweep：durably 计数尝试，超限 terminalize。
    fn sweep(
        &self,
        session_id: &str,
        now_ms: u64,
        max_attempts: u32,
    ) -> Result<SweepOutcome, ExecutionStoreError>;
    /// 接管一个 Resume 的 claim（更新 owner/pid，attempt 不变）。
    fn adopt(
        &self,
        session_id: &str,
        execution_id: &str,
        owner: &str,
        pid: Option<u32>,
    ) -> Result<ExecutionClaim, ExecutionStoreError>;
}

/// 执行控制器：包装一个 [`ExecutionStore`] 并提供 acquire/recover 语义。
#[derive(Debug)]
pub struct ExecutionController<S: ExecutionStore> {
    store: Arc<S>,
    max_attempts: u32,
}

impl<S: ExecutionStore> ExecutionController<S> {
    /// 新建（默认 10 次尝试）。
    pub fn new(store: Arc<S>) -> Self {
        Self {
            store,
            max_attempts: DEFAULT_MAX_EXECUTION_ATTEMPTS,
        }
    }

    /// 指定最大尝试次数。
    pub fn with_max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = max_attempts.max(1);
        self
    }

    /// 最大尝试次数。
    pub fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    /// 最近状态。
    pub fn state(&self, session_id: &str) -> Result<Option<ExecutionState>, ExecutionStoreError> {
        self.store.load(session_id)
    }

    /// 正常启动：没有活跃 claim 才成功。
    pub fn acquire(
        &self,
        session_id: &str,
        owner: impl Into<String>,
    ) -> Result<AcquireResult<S>, ExecutionStoreError> {
        let claim = ExecutionClaim {
            session_id: session_id.to_string(),
            execution_id: new_execution_id(),
            owner: owner.into(),
            pid: Some(std::process::id()),
            started_at_ms: now_millis(),
            attempts: 1,
            clean_shutdown: false,
        };
        match self.store.begin(claim.clone())? {
            AcquireOutcome::Acquired(claim) => Ok(AcquireResult::Acquired(self.lease(claim))),
            AcquireOutcome::Busy(claim) => Ok(AcquireResult::Busy(claim)),
        }
    }

    /// 启动恢复：先 sweep；`Resume` 时 adopt 现有 claim，超限则返回 terminalize。
    pub fn recover(
        &self,
        session_id: &str,
        owner: impl Into<String>,
    ) -> Result<RecoverResult<S>, ExecutionStoreError> {
        let owner = owner.into();
        let sweep = self
            .store
            .sweep(session_id, now_millis(), self.max_attempts)?;
        // `sweep_state` 对每个会话至多生成一个动作；两个分支都以返回值结束。
        if let Some(action) = sweep.actions.into_iter().next() {
            match action {
                SweepAction::Resume { claim, attempt } => {
                    let claim = self.store.adopt(
                        session_id,
                        &claim.execution_id,
                        &owner,
                        Some(std::process::id()),
                    )?;
                    let _ = attempt;
                    return Ok(RecoverResult::Resumed(self.lease(claim)));
                }
                SweepAction::Terminalize {
                    reason,
                    terminal_event,
                    ..
                } => {
                    return Ok(RecoverResult::Terminalized {
                        reason,
                        terminal_event,
                    });
                }
            }
        }
        match self.acquire(session_id, owner)? {
            AcquireResult::Acquired(lease) => Ok(RecoverResult::Acquired(lease)),
            AcquireResult::Busy(claim) => Ok(RecoverResult::Busy(claim)),
        }
    }

    fn lease(&self, claim: ExecutionClaim) -> ExecutionLease<S> {
        ExecutionLease {
            store: Arc::clone(&self.store),
            session_id: claim.session_id,
            execution_id: claim.execution_id,
            finished: false,
        }
    }
}

/// `acquire` 的返回。
#[derive(Debug)]
pub enum AcquireResult<S: ExecutionStore> {
    /// 获得执行权。
    Acquired(ExecutionLease<S>),
    /// 已有执行者。
    Busy(ExecutionClaim),
}

/// `recover` 的返回。
#[derive(Debug)]
pub enum RecoverResult<S: ExecutionStore> {
    /// 新的 execution。
    Acquired(ExecutionLease<S>),
    /// 接管崩溃/clean shutdown 留下的 execution。
    Resumed(ExecutionLease<S>),
    /// 尝试次数超限，已写入终态。
    Terminalized {
        /// 原因。
        reason: String,
        /// terminal 事件。
        terminal_event: Option<ExecutionEvent>,
    },
    /// 已有执行者（sweep 没找到 claim，但 acquire 发现 Busy 的防御分支）。
    Busy(ExecutionClaim),
}

/// 执行租约。
///
/// 不调用 [`Self::finish`] 或 [`Self::clean_shutdown`] 就 drop，等价于崩溃：claim
/// 会保留，下次启动 sweep 会计一次 attempt。
#[derive(Debug)]
pub struct ExecutionLease<S: ExecutionStore> {
    store: Arc<S>,
    session_id: String,
    execution_id: String,
    finished: bool,
}

impl<S: ExecutionStore> ExecutionLease<S> {
    /// execution id。
    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }

    /// session id。
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 正常/失败终态：释放 claim 并发布 terminal（只一次）。
    pub fn finish(
        &mut self,
        status: ExecutionStatus,
        reason: Option<String>,
    ) -> Result<FinishOutcome, ExecutionStoreError> {
        let outcome = self
            .store
            .finish(&self.session_id, &self.execution_id, status, reason)?;
        self.finished = true;
        Ok(outcome)
    }

    /// clean shutdown：故意保留 claim，不消耗 attempt。
    pub fn clean_shutdown(&mut self) -> Result<(), ExecutionStoreError> {
        self.store
            .keep_on_clean_shutdown(&self.session_id, &self.execution_id)?;
        self.finished = true;
        Ok(())
    }

    /// 是否已经结算。
    pub fn is_finished(&self) -> bool {
        self.finished
    }
}

// ---------------------------------------------------------------------------
// 内存实现
// ---------------------------------------------------------------------------

/// 单进程参考实现。
#[derive(Debug, Default)]
pub struct MemoryExecutionStore {
    states: Mutex<HashMap<String, ExecutionState>>,
}

impl MemoryExecutionStore {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }
}

impl ExecutionStore for MemoryExecutionStore {
    fn begin(&self, claim: ExecutionClaim) -> Result<AcquireOutcome, ExecutionStoreError> {
        let mut states = self
            .states
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = states
            .entry(claim.session_id.clone())
            .or_insert_with(|| ExecutionState::new(claim.session_id.clone()));
        if let Some(existing) = &state.claim {
            return Ok(AcquireOutcome::Busy(existing.clone()));
        }
        // claim 与 Started 在同一临界区（同一事务语义）写入。
        let seq = state.next_seq();
        state.push_event(ExecutionEvent::Started {
            seq,
            at_ms: claim.started_at_ms,
            execution_id: claim.execution_id.clone(),
            owner: claim.owner.clone(),
            attempt: claim.attempts,
        });
        state.claim = Some(claim.clone());
        Ok(AcquireOutcome::Acquired(claim))
    }

    fn finish(
        &self,
        session_id: &str,
        execution_id: &str,
        status: ExecutionStatus,
        reason: Option<String>,
    ) -> Result<FinishOutcome, ExecutionStoreError> {
        let mut states = self
            .states
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = states
            .entry(session_id.to_string())
            .or_insert_with(|| ExecutionState::new(session_id.to_string()));
        Ok(finish_state(state, execution_id, status, reason))
    }

    fn keep_on_clean_shutdown(
        &self,
        session_id: &str,
        execution_id: &str,
    ) -> Result<bool, ExecutionStoreError> {
        let mut states = self
            .states
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = states
            .entry(session_id.to_string())
            .or_insert_with(|| ExecutionState::new(session_id.to_string()));
        Ok(keep_state_clean(state, execution_id))
    }

    fn load(&self, session_id: &str) -> Result<Option<ExecutionState>, ExecutionStoreError> {
        Ok(self
            .states
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(session_id)
            .cloned())
    }

    fn sweep(
        &self,
        session_id: &str,
        now_ms: u64,
        max_attempts: u32,
    ) -> Result<SweepOutcome, ExecutionStoreError> {
        let mut states = self
            .states
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = states
            .entry(session_id.to_string())
            .or_insert_with(|| ExecutionState::new(session_id.to_string()));
        let actions = sweep_state(state, now_ms, max_attempts);
        Ok(SweepOutcome {
            actions,
            state: state.clone(),
        })
    }

    fn adopt(
        &self,
        session_id: &str,
        execution_id: &str,
        owner: &str,
        pid: Option<u32>,
    ) -> Result<ExecutionClaim, ExecutionStoreError> {
        let mut states = self
            .states
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state =
            states
                .get_mut(session_id)
                .ok_or_else(|| ExecutionStoreError::ClaimNotFound {
                    session_id: session_id.to_string(),
                })?;
        adopt_claim(state, session_id, execution_id, owner, pid)
    }
}

// ---------------------------------------------------------------------------
// 文件实现
// ---------------------------------------------------------------------------

/// 每 session 一个 JSON 状态文件；读-改-写在 `flock` 下完成。
#[derive(Debug)]
pub struct FileExecutionStore {
    dir: PathBuf,
    process_lock: Mutex<()>,
}

impl FileExecutionStore {
    /// 新建（目录懒创建）。
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            process_lock: Mutex::new(()),
        }
    }

    /// 状态目录。
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn state_path(&self, session_id: &str) -> PathBuf {
        let key = sha256_hex(session_id.as_bytes());
        self.dir.join(format!("{key}.json"))
    }

    fn load_locked(&self, session_id: &str) -> Result<ExecutionState, ExecutionStoreError> {
        let path = self.state_path(session_id);
        match fs::read(&path) {
            Ok(bytes) => {
                let mut state: ExecutionState = serde_json::from_slice(&bytes)
                    .map_err(|error| ExecutionStoreError::Corrupt(error.to_string()))?;
                if state.session_id.is_empty() {
                    state.session_id = session_id.to_string();
                }
                if state.session_id != session_id {
                    return Err(ExecutionStoreError::Corrupt(format!(
                        "state belongs to session {}, expected {session_id}",
                        state.session_id
                    )));
                }
                Ok(state)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(ExecutionState::new(session_id))
            }
            Err(error) => Err(ExecutionStoreError::Io(error)),
        }
    }

    fn save_locked(&self, state: &ExecutionState) -> Result<(), ExecutionStoreError> {
        fs::create_dir_all(&self.dir)?;
        let path = self.state_path(&state.session_id);
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|error| ExecutionStoreError::Corrupt(error.to_string()))?;
        let temp = temp_path(&path);
        fs::write(&temp, bytes)?;
        fs::rename(&temp, &path)?;
        Ok(())
    }

    fn with_lock<T>(
        &self,
        session_id: &str,
        operation: impl FnOnce(&mut ExecutionState) -> Result<T, ExecutionStoreError>,
    ) -> Result<T, ExecutionStoreError> {
        let _process = self
            .process_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        fs::create_dir_all(&self.dir)?;
        let lock_path = self
            .dir
            .join(format!("{}.lock", &sha256_hex(session_id.as_bytes())[..32]));
        let _file_lock = FileLock::acquire(&lock_path)?;
        let mut state = self.load_locked(session_id)?;
        let result = operation(&mut state)?;
        self.save_locked(&state)?;
        Ok(result)
    }
}

impl ExecutionStore for FileExecutionStore {
    fn begin(&self, claim: ExecutionClaim) -> Result<AcquireOutcome, ExecutionStoreError> {
        let session_id = claim.session_id.clone();
        self.with_lock(&session_id, |state| {
            if let Some(existing) = &state.claim {
                return Ok(AcquireOutcome::Busy(existing.clone()));
            }
            let seq = state.next_seq();
            state.push_event(ExecutionEvent::Started {
                seq,
                at_ms: claim.started_at_ms,
                execution_id: claim.execution_id.clone(),
                owner: claim.owner.clone(),
                attempt: claim.attempts,
            });
            state.claim = Some(claim.clone());
            Ok(AcquireOutcome::Acquired(claim))
        })
    }

    fn finish(
        &self,
        session_id: &str,
        execution_id: &str,
        status: ExecutionStatus,
        reason: Option<String>,
    ) -> Result<FinishOutcome, ExecutionStoreError> {
        self.with_lock(session_id, |state| {
            Ok(finish_state(state, execution_id, status, reason))
        })
    }

    fn keep_on_clean_shutdown(
        &self,
        session_id: &str,
        execution_id: &str,
    ) -> Result<bool, ExecutionStoreError> {
        self.with_lock(session_id, |state| {
            Ok(keep_state_clean(state, execution_id))
        })
    }

    fn load(&self, session_id: &str) -> Result<Option<ExecutionState>, ExecutionStoreError> {
        let _process = self
            .process_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = self.state_path(session_id);
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|error| ExecutionStoreError::Corrupt(error.to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(ExecutionStoreError::Io(error)),
        }
    }

    fn sweep(
        &self,
        session_id: &str,
        now_ms: u64,
        max_attempts: u32,
    ) -> Result<SweepOutcome, ExecutionStoreError> {
        self.with_lock(session_id, |state| {
            let actions = sweep_state(state, now_ms, max_attempts);
            Ok(SweepOutcome {
                actions,
                state: state.clone(),
            })
        })
    }

    fn adopt(
        &self,
        session_id: &str,
        execution_id: &str,
        owner: &str,
        pid: Option<u32>,
    ) -> Result<ExecutionClaim, ExecutionStoreError> {
        self.with_lock(session_id, |state| {
            adopt_claim(state, session_id, execution_id, owner, pid)
        })
    }
}

// ---------------------------------------------------------------------------
// 共享状态机
// ---------------------------------------------------------------------------

fn finish_state(
    state: &mut ExecutionState,
    execution_id: &str,
    status: ExecutionStatus,
    reason: Option<String>,
) -> FinishOutcome {
    let Some(claim) = state.claim.as_ref() else {
        return FinishOutcome {
            released: false,
            terminal_event: None,
        };
    };
    if claim.execution_id != execution_id {
        return FinishOutcome {
            released: false,
            terminal_event: None,
        };
    }
    state.claim = None;
    let seq = state.next_seq();
    let event = ExecutionEvent::Terminal {
        seq,
        at_ms: now_millis(),
        execution_id: execution_id.to_string(),
        status,
        reason,
    };
    state.terminal_seq = state.terminal_seq.saturating_add(1);
    state.push_event(event.clone());
    FinishOutcome {
        released: true,
        terminal_event: Some(event),
    }
}

fn keep_state_clean(state: &mut ExecutionState, execution_id: &str) -> bool {
    match state.claim.as_mut() {
        Some(claim) if claim.execution_id == execution_id => {
            claim.clean_shutdown = true;
            true
        }
        _ => false,
    }
}

fn sweep_state(state: &mut ExecutionState, now_ms: u64, max_attempts: u32) -> Vec<SweepAction> {
    let Some(claim) = state.claim.clone() else {
        return Vec::new();
    };
    if claim.clean_shutdown {
        // clean shutdown 不消耗 attempt；清除标记，避免每次启动都当 clean。
        if let Some(current) = state.claim.as_mut() {
            current.clean_shutdown = false;
        }
        let seq = state.next_seq();
        state.push_event(ExecutionEvent::Resumed {
            seq,
            at_ms: now_ms,
            execution_id: claim.execution_id.clone(),
            attempt: claim.attempts,
        });
        return vec![SweepAction::Resume {
            claim,
            attempt: state
                .claim
                .as_ref()
                .map(|claim| claim.attempts)
                .unwrap_or(1),
        }];
    }
    if claim.attempts >= max_attempts {
        let execution_id = claim.execution_id.clone();
        state.claim = None;
        let seq = state.next_seq();
        let event = ExecutionEvent::Terminal {
            seq,
            at_ms: now_ms,
            execution_id: execution_id.clone(),
            status: ExecutionStatus::Failed,
            reason: Some("execution_attempts_exhausted".to_string()),
        };
        state.terminal_seq = state.terminal_seq.saturating_add(1);
        state.push_event(event.clone());
        return vec![SweepAction::Terminalize {
            execution_id,
            reason: "execution_attempts_exhausted".to_string(),
            terminal_event: Some(event),
        }];
    }
    let attempt = claim.attempts.saturating_add(1);
    if let Some(current) = state.claim.as_mut() {
        current.attempts = attempt;
    }
    let seq = state.next_seq();
    state.push_event(ExecutionEvent::Resumed {
        seq,
        at_ms: now_ms,
        execution_id: claim.execution_id.clone(),
        attempt,
    });
    vec![SweepAction::Resume {
        claim: state.claim.clone().unwrap_or(claim),
        attempt,
    }]
}

fn adopt_claim(
    state: &mut ExecutionState,
    session_id: &str,
    execution_id: &str,
    owner: &str,
    pid: Option<u32>,
) -> Result<ExecutionClaim, ExecutionStoreError> {
    let Some(claim) = state.claim.as_mut() else {
        return Err(ExecutionStoreError::ClaimNotFound {
            session_id: session_id.to_string(),
        });
    };
    if claim.execution_id != execution_id {
        return Err(ExecutionStoreError::ExecutionMismatch {
            session_id: session_id.to_string(),
            expected: claim.execution_id.clone(),
            actual: execution_id.to_string(),
        });
    }
    claim.owner = owner.to_string();
    claim.pid = pid;
    Ok(claim.clone())
}

fn new_execution_id() -> String {
    format!("exec_{}", uuid::Uuid::new_v4())
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

#[cfg(unix)]
struct FileLock {
    file: fs::File,
}

#[cfg(unix)]
impl FileLock {
    fn acquire(path: &Path) -> Result<Self, ExecutionStoreError> {
        use std::os::unix::io::AsRawFd;
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        let result = unsafe { flock(file.as_raw_fd(), LOCK_EX) };
        if result != 0 {
            return Err(ExecutionStoreError::Io(std::io::Error::last_os_error()));
        }
        Ok(Self { file })
    }
}

#[cfg(unix)]
impl Drop for FileLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        unsafe {
            let _ = flock(self.file.as_raw_fd(), LOCK_UN);
        }
    }
}

#[cfg(not(unix))]
struct FileLock;

#[cfg(not(unix))]
impl FileLock {
    fn acquire(_path: &Path) -> Result<Self, ExecutionStoreError> {
        Ok(Self)
    }
}

#[cfg(unix)]
const LOCK_EX: i32 = 2;
#[cfg(unix)]
const LOCK_UN: i32 = 8;

#[cfg(unix)]
unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}
