//! cron 的持久化账本与进程指纹。
//!
//! 设计参照：
//! - `reference_project/ClaudeCode/src/utils/cronTasks.ts` / `cronScheduler.ts`：job 的
//!   `nextRunAt`、one-shot 自动删除；
//! - `reference_project/deer-flow/backend/packages/harness/deerflow/persistence/scheduled_tasks/**`
//!   与 `scheduled_task_runs/**`：`next_run_at` 先推进、`pending` 槽位、execution 账本的
//!   claimed/running/terminal 状态、lease/owner 证明；
//! - `reference_project/cline/sdk/packages/core/src/cron/{runner,store}`：claim token、
//!   终态只结算一次。
//!
//! # 事务原语来自 kiana-durable
//!
//! 跨进程 `flock(LOCK_EX)`、临时文件 + rename、`version` 乐观并发与锁内读改写由
//! [`kiana_durable::harness::record_store::JsonRecordStore`] 提供（ADR
//! `docs/adr/durable.md`）。本文件只定义 cron 的 job/execution 记录形状与调度字段，
//! 不再维护第二套锁或事务账本。`MemoryCronStore` 是测试/嵌入参考实现。
//!
//! # 进程死亡证明
//!
//! [`ProcessFingerprint`] 记录 `pid` 与 Linux `/proc/<pid>/stat` 的 `starttime`（第 22
//! 字段）。只比较 pid 无法区分 PID 复用；start-time 不同即判定旧 owner 已死亡。非
//! Linux 没有 `/proc` 时退回 `kill(pid, 0)`（精度较低，文档中标注）。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use kiana_durable::harness::record_store::{JsonRecordStore, JsonRecordStoreError};

use super::schedule::{CompiledSchedule, ScheduleError, ScheduleSpec};
use super::time::now_secs;

/// cron 错误。
#[derive(Debug, Error)]
pub enum CronError {
    /// 调度表达式错误。
    #[error("schedule error: {0}")]
    Schedule(#[from] ScheduleError),
    /// 持久化错误。
    #[error("cron store error: {0}")]
    Store(#[from] CronStoreError),
    /// job 字段不合法。
    #[error("invalid cron job: {0}")]
    InvalidJob(String),
    /// job 不存在。
    #[error("cron job {0:?} does not exist")]
    JobNotFound(String),
    /// execution 不存在。
    #[error("cron execution {0:?} does not exist")]
    ExecutionNotFound(String),
    /// 同一行 delivery 已被 claim。
    #[error("delivery for execution {0:?} was already claimed")]
    DeliveryAlreadyClaimed(String),
    /// delivery 的 owner 已死亡，结果永久 unknown。
    #[error("delivery for execution {0:?} is permanently unknown")]
    DeliveryUnknown(String),
    /// delivery claim token 不匹配。
    #[error("delivery claim token mismatch for execution {0:?}")]
    DeliveryTokenMismatch(String),
    /// 脚本逃逸受管目录。
    #[error("script {0:?} escapes the managed script directory")]
    ScriptEscape(String),
}

/// store 错误。
#[derive(Debug, Error)]
pub enum CronStoreError {
    /// I/O 失败。
    #[error("cron store I/O error at {path}: {source}")]
    Io {
        /// 路径。
        path: PathBuf,
        /// 底层错误。
        source: std::io::Error,
    },
    /// JSON 解析失败。
    #[error("cron store JSON error at {path}: {source}")]
    Json {
        /// 路径。
        path: PathBuf,
        /// 底层错误。
        source: serde_json::Error,
    },
    /// 记录损坏。
    #[error("corrupt cron record {path}: {message}")]
    Corrupt {
        /// 路径。
        path: PathBuf,
        /// 原因。
        message: String,
    },
    /// `kiana-durable` 的事务存储错误。
    #[error("durable cron store error: {0}")]
    Durable(#[from] JsonRecordStoreError),
}

impl CronStoreError {
    fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// 进程指纹：pid + 启动时间（Linux `/proc/<pid>/stat` 的 `starttime`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessFingerprint {
    /// 进程 id。
    pub pid: u32,
    /// 启动时间（clock ticks since boot）；非 Linux 为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<u64>,
}

impl ProcessFingerprint {
    /// 当前进程指纹。
    pub fn current() -> Self {
        let pid = std::process::id();
        Self {
            pid,
            start_time: process_start_time(pid),
        }
    }

    /// 构造一个带指定 pid 的指纹（测试用）。
    pub fn for_pid(pid: u32) -> Self {
        Self {
            pid,
            start_time: process_start_time(pid),
        }
    }

    /// owner 是否仍存活：start-time 必须完全一致；没有 start-time 时退回 `kill(pid,0)`。
    pub fn is_alive(&self) -> bool {
        match self.start_time {
            Some(expected) => process_start_time(self.pid) == Some(expected),
            None => pid_exists(self.pid),
        }
    }
}

/// 读取 `/proc/<pid>/stat` 的 starttime（第 22 字段）。
#[cfg(target_os = "linux")]
pub fn process_start_time(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm 可能包含空格/括号；以最后一个 ')' 为界，其后从 state（第 3 字段）开始。
    let close = stat.rfind(')')?;
    let fields: Vec<&str> = stat[close + 1..].split_whitespace().collect();
    // fields[0] = state（字段 3），starttime 是字段 22 → 下标 19。
    fields.get(19)?.parse::<u64>().ok()
}

/// 非 Linux 平台没有 `/proc`。
#[cfg(not(target_os = "linux"))]
pub fn process_start_time(_pid: u32) -> Option<u64> {
    None
}

#[cfg(unix)]
fn pid_exists(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    // SAFETY: `kill` 只读取 pid。
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn pid_exists(_pid: u32) -> bool {
    true
}

/// 一个 cron job（落盘形状）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CronJob {
    /// 稳定 id。
    pub id: String,
    /// 交给 agent 的提示词。
    pub prompt: String,
    /// 调度表达式（用户原始形状）。
    pub schedule: ScheduleSpec,
    /// 是否启用。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 要启用的 skills。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    /// 覆盖模型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 受管目录内的脚本文件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    /// 从哪些会话/上下文继承（id 列表；接线层解释）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_from: Vec<String>,
    /// 工作目录。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    /// 时区（UTC/Local/±HH:MM）。
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// 超过 `next_run_at + grace` 就记 missed。
    #[serde(default)]
    pub grace_minutes: u64,
    /// 下一次计划时间（epoch 秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<i64>,
    /// 已推进但尚未 dispatch 的槽位。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_slot: Option<i64>,
    /// 占用 pending 槽位的进程。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_owner: Option<ProcessFingerprint>,
    /// 最近一次开始执行的时间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<i64>,
    /// 创建时间。
    #[serde(default)]
    pub created_at: i64,
    /// 乐观并发版本；每次 CAS 成功 +1。
    #[serde(default)]
    pub version: u64,
}

fn default_true() -> bool {
    true
}

fn default_timezone() -> String {
    "UTC".to_string()
}

impl CronJob {
    /// 新建 job（`next_run_at = None`，由 scheduler 初始化）。
    pub fn new(id: impl Into<String>, prompt: impl Into<String>, schedule: ScheduleSpec) -> Self {
        Self {
            id: id.into(),
            prompt: prompt.into(),
            schedule,
            enabled: true,
            skills: Vec::new(),
            model: None,
            script: None,
            context_from: Vec::new(),
            workdir: None,
            timezone: default_timezone(),
            grace_minutes: 0,
            next_run_at: None,
            pending_slot: None,
            pending_owner: None,
            last_run_at: None,
            created_at: now_secs(),
            version: 0,
        }
    }

    /// 编译调度表达式。
    pub fn compile(&self) -> Result<CompiledSchedule, ScheduleError> {
        CompiledSchedule::from_spec(&self.schedule, &self.timezone)
    }

    /// 首次调度：`next_run_at` 为空时按 now 计算。
    pub fn initialize_schedule(&mut self, now: i64) -> Result<(), CronError> {
        if self.next_run_at.is_none() {
            let compiled = self.compile()?;
            self.next_run_at = compiled.initial_next(now);
        }
        Ok(())
    }

    /// 校验 id、prompt、调度、脚本与 workdir。
    pub fn validate(&self, managed_script_dir: Option<&Path>) -> Result<(), CronError> {
        if self.id.trim().is_empty() {
            return Err(CronError::InvalidJob("id is empty".to_string()));
        }
        if self.id.contains('/') || self.id.contains('\\') || self.id.contains("..") {
            return Err(CronError::InvalidJob(format!(
                "id {:?} contains a path separator",
                self.id
            )));
        }
        if self.prompt.trim().is_empty() {
            return Err(CronError::InvalidJob("prompt is empty".to_string()));
        }
        self.compile()?;
        if let Some(script) = &self.script {
            let Some(directory) = managed_script_dir else {
                return Err(CronError::InvalidJob(
                    "script is set but no managed script directory is configured".to_string(),
                ));
            };
            resolve_script_path(directory, script)?;
        }
        if let Some(workdir) = &self.workdir
            && !Path::new(workdir).is_absolute()
        {
            return Err(CronError::InvalidJob(format!(
                "workdir {workdir:?} is not absolute"
            )));
        }
        Ok(())
    }
}

/// 解析受管脚本路径：拒绝绝对路径、`..`、以及 canonicalize 后逃逸的符号链接。
pub fn resolve_script_path(managed_dir: &Path, script: &str) -> Result<PathBuf, CronError> {
    let script = script.trim();
    if script.is_empty() {
        return Err(CronError::ScriptEscape(script.to_string()));
    }
    let candidate = Path::new(script);
    if candidate.is_absolute()
        || candidate.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(CronError::ScriptEscape(script.to_string()));
    }
    let base = fs::canonicalize(managed_dir).unwrap_or_else(|_| managed_dir.to_path_buf());
    let joined = base.join(candidate);
    if let Ok(canonical) = fs::canonicalize(&joined) {
        if !canonical.starts_with(&base) {
            return Err(CronError::ScriptEscape(script.to_string()));
        }
        return Ok(canonical);
    }
    if !joined.starts_with(&base) {
        return Err(CronError::ScriptEscape(script.to_string()));
    }
    Ok(joined)
}

/// execution 状态账本。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    /// 已被某进程占有，尚未开始。
    Claimed,
    /// 正在执行。
    Running,
    /// 成功。
    Completed,
    /// 失败。
    Failed,
    /// owner 死亡，结果不可知；永不重试。
    Unknown,
    /// 显式取消。
    Cancelled,
    /// 超过 grace 未执行。
    Missed,
}

impl ExecutionStatus {
    /// 是否仍在进行中。
    pub fn is_active(self) -> bool {
        matches!(self, ExecutionStatus::Claimed | ExecutionStatus::Running)
    }

    /// 是否终态。
    pub fn is_terminal(self) -> bool {
        !self.is_active()
    }
}

/// delivery claim 记录；每行最多 claim 一次。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryRecord {
    /// claim token（调用方生成）。
    pub claim_token: String,
    /// claim 时间。
    pub claimed_at: i64,
    /// 投递完成时间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<i64>,
    /// owner 在 claim 后死亡：永久 unknown，永不重试。
    #[serde(default)]
    pub unknown: bool,
}

/// 一次执行尝试。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CronExecution {
    /// 唯一 id。
    pub id: String,
    /// 所属 job。
    pub job_id: String,
    /// 计划槽位（epoch 秒）。
    pub slot: i64,
    /// 状态。
    pub status: ExecutionStatus,
    /// 占有者指纹。
    pub owner: ProcessFingerprint,
    /// claim 时间。
    pub claimed_at: i64,
    /// 开始执行时间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    /// 结束时间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<i64>,
    /// 失败原因。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// delivery 记录。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<DeliveryRecord>,
    /// 尝试次数。
    #[serde(default)]
    pub attempts: u32,
}

impl CronExecution {
    /// 新建 claimed 记录。
    pub fn claimed(
        id: impl Into<String>,
        job: &CronJob,
        slot: i64,
        owner: ProcessFingerprint,
        now: i64,
    ) -> Self {
        Self {
            id: id.into(),
            job_id: job.id.clone(),
            slot,
            status: ExecutionStatus::Claimed,
            owner,
            claimed_at: now,
            started_at: None,
            finished_at: None,
            error: None,
            delivery: None,
            attempts: 0,
        }
    }
}

/// cron 持久化契约。
///
/// `compare_and_swap_job` 是跨进程唯一的 job 写入口：只有 `job.version == expected`
/// 时才写入并把版本 +1，否则返回 `false`。
pub trait CronStore: Send + Sync {
    /// 所有 job（按 id）。
    fn list_jobs(&self) -> Result<Vec<CronJob>, CronStoreError>;
    /// 读取单个 job。
    fn get_job(&self, id: &str) -> Result<Option<CronJob>, CronStoreError>;
    /// 管理员写入（覆盖）；不参与并发争用。
    fn save_job(&self, job: &CronJob) -> Result<(), CronStoreError>;
    /// 乐观并发写；成功时把 `job.version` 设为 `expected + 1`。
    fn compare_and_swap_job(&self, expected: u64, job: &CronJob) -> Result<bool, CronStoreError>;
    /// 写入 execution（覆盖）。
    fn save_execution(&self, execution: &CronExecution) -> Result<(), CronStoreError>;
    /// 读取 execution。
    fn get_execution(&self, id: &str) -> Result<Option<CronExecution>, CronStoreError>;
    /// 所有 execution。
    fn list_executions(&self) -> Result<Vec<CronExecution>, CronStoreError>;
    /// 原子 claim delivery：只有 `delivery == None` 时成功；否则 `None`。
    fn claim_delivery(
        &self,
        execution_id: &str,
        token: &str,
        now: i64,
    ) -> Result<Option<DeliveryRecord>, CronStoreError>;
}

/// 内存实现（测试/嵌入参考）。
#[derive(Default)]
pub struct MemoryCronStore {
    jobs: Mutex<BTreeMap<String, CronJob>>,
    executions: Mutex<BTreeMap<String, CronExecution>>,
}

impl MemoryCronStore {
    /// 空 store。
    pub fn new() -> Self {
        Self::default()
    }
}

impl CronStore for MemoryCronStore {
    fn list_jobs(&self) -> Result<Vec<CronJob>, CronStoreError> {
        Ok(self.jobs.lock().expect("jobs").values().cloned().collect())
    }

    fn get_job(&self, id: &str) -> Result<Option<CronJob>, CronStoreError> {
        Ok(self.jobs.lock().expect("jobs").get(id).cloned())
    }

    fn save_job(&self, job: &CronJob) -> Result<(), CronStoreError> {
        self.jobs
            .lock()
            .expect("jobs")
            .insert(job.id.clone(), job.clone());
        Ok(())
    }

    fn compare_and_swap_job(&self, expected: u64, job: &CronJob) -> Result<bool, CronStoreError> {
        let mut jobs = self.jobs.lock().expect("jobs");
        let current = jobs.get(&job.id).map(|job| job.version).unwrap_or(0);
        if current != expected {
            return Ok(false);
        }
        let mut updated = job.clone();
        updated.version = expected + 1;
        jobs.insert(job.id.clone(), updated);
        Ok(true)
    }

    fn save_execution(&self, execution: &CronExecution) -> Result<(), CronStoreError> {
        self.executions
            .lock()
            .expect("executions")
            .insert(execution.id.clone(), execution.clone());
        Ok(())
    }

    fn get_execution(&self, id: &str) -> Result<Option<CronExecution>, CronStoreError> {
        Ok(self.executions.lock().expect("executions").get(id).cloned())
    }

    fn list_executions(&self) -> Result<Vec<CronExecution>, CronStoreError> {
        Ok(self
            .executions
            .lock()
            .expect("executions")
            .values()
            .cloned()
            .collect())
    }

    fn claim_delivery(
        &self,
        execution_id: &str,
        token: &str,
        now: i64,
    ) -> Result<Option<DeliveryRecord>, CronStoreError> {
        let mut executions = self.executions.lock().expect("executions");
        let Some(execution) = executions.get_mut(execution_id) else {
            return Ok(None);
        };
        if execution.delivery.is_some() {
            return Ok(None);
        }
        let record = DeliveryRecord {
            claim_token: token.to_string(),
            claimed_at: now,
            delivered_at: None,
            unknown: false,
        };
        execution.delivery = Some(record.clone());
        Ok(Some(record))
    }
}

/// 文件实现：`<root>/jobs/<id>.json`、`<root>/executions/<id>.json`、`<root>/.lock`。
///
/// 事务原语来自 `kiana_durable::harness::record_store::JsonRecordStore`（ADR
/// `docs/adr/durable.md`：kiana-durable 是 cron 的唯一事务存储底座）。本类型只做
/// `CronJob`/`CronExecution` 与通用 JSON 记录之间的字段映射，不再自带锁/临时文件逻辑。
pub struct FileCronStore {
    root: PathBuf,
    records: JsonRecordStore,
}

impl FileCronStore {
    /// 创建目录与锁文件。
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, CronStoreError> {
        let root = root.into();
        let records = JsonRecordStore::new(&root)?;
        for collection in ["jobs", "executions"] {
            let directory = root.join(collection);
            fs::create_dir_all(&directory)
                .map_err(|error| CronStoreError::io(&directory, error))?;
            records.ensure_collection(collection)?;
        }
        Ok(Self { root, records })
    }

    /// 根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl CronStore for FileCronStore {
    fn list_jobs(&self) -> Result<Vec<CronJob>, CronStoreError> {
        let mut jobs: Vec<CronJob> = self.records.list("jobs")?;
        jobs.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(jobs)
    }

    fn get_job(&self, id: &str) -> Result<Option<CronJob>, CronStoreError> {
        Ok(self.records.get("jobs", id)?)
    }

    fn save_job(&self, job: &CronJob) -> Result<(), CronStoreError> {
        Ok(self.records.put("jobs", &job.id, job)?)
    }

    fn compare_and_swap_job(&self, expected: u64, job: &CronJob) -> Result<bool, CronStoreError> {
        let mut candidate = job.clone();
        candidate.version = expected;
        let value = serde_json::to_value(&candidate).map_err(|error| CronStoreError::Json {
            path: self.root.join("jobs").join(format!("{}.json", job.id)),
            source: error,
        })?;
        Ok(self
            .records
            .compare_and_swap("jobs", &job.id, expected, &value)?)
    }

    fn save_execution(&self, execution: &CronExecution) -> Result<(), CronStoreError> {
        Ok(self.records.put("executions", &execution.id, execution)?)
    }

    fn get_execution(&self, id: &str) -> Result<Option<CronExecution>, CronStoreError> {
        Ok(self.records.get("executions", id)?)
    }

    fn list_executions(&self) -> Result<Vec<CronExecution>, CronStoreError> {
        let mut executions: Vec<CronExecution> = self.records.list("executions")?;
        executions.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(executions)
    }

    fn claim_delivery(
        &self,
        execution_id: &str,
        token: &str,
        now: i64,
    ) -> Result<Option<DeliveryRecord>, CronStoreError> {
        let token = token.to_string();
        Ok(self.records.update(
            "executions",
            execution_id,
            move |execution: &mut CronExecution| {
                if execution.delivery.is_some() {
                    return None;
                }
                let record = DeliveryRecord {
                    claim_token: token,
                    claimed_at: now,
                    delivered_at: None,
                    unknown: false,
                };
                execution.delivery = Some(record.clone());
                Some(record)
            },
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kiana-cron-store-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn job(id: &str) -> CronJob {
        CronJob::new(id, "do work", ScheduleSpec::duration("1h"))
    }

    #[test]
    fn memory_store_cas_advances_version_and_rejects_stale_writes() {
        let store = MemoryCronStore::new();
        let mut job = job("a");
        store.save_job(&job).expect("save");
        assert_eq!(store.get_job("a").expect("get").expect("some").version, 0);

        job.next_run_at = Some(100);
        assert!(store.compare_and_swap_job(0, &job).expect("cas"));
        let stored = store.get_job("a").expect("get").expect("some");
        assert_eq!(stored.version, 1);
        assert_eq!(stored.next_run_at, Some(100));

        job.next_run_at = Some(200);
        assert!(!store.compare_and_swap_job(0, &job).expect("stale"));
        assert_eq!(
            store.get_job("a").expect("get").expect("some").next_run_at,
            Some(100)
        );
    }

    #[test]
    fn file_store_persists_across_instances_and_cas() {
        let dir = temp_dir("persist");
        let store = FileCronStore::new(&dir).expect("store");
        let job = job("daily");
        store.save_job(&job).expect("save");
        assert_eq!(store.list_jobs().expect("list").len(), 1);

        drop(store);
        let reopened = FileCronStore::new(&dir).expect("reopen");
        let mut stored = reopened.get_job("daily").expect("get").expect("some");
        assert_eq!(stored.prompt, "do work");
        stored.next_run_at = Some(42);
        assert!(reopened.compare_and_swap_job(0, &stored).expect("cas"));
        assert!(!reopened.compare_and_swap_job(0, &stored).expect("stale"));
        let stored = reopened.get_job("daily").expect("get").expect("some");
        assert_eq!(stored.version, 1);
        assert_eq!(stored.next_run_at, Some(42));
    }

    #[test]
    fn file_store_rejects_path_like_ids() {
        let dir = temp_dir("safe-id");
        let store = FileCronStore::new(&dir).expect("store");
        let mut job = job("safe");
        job.id = "../escape".to_string();
        assert!(store.save_job(&job).is_err());
        assert!(store.get_job("../escape").is_err());
        let mut execution =
            CronExecution::claimed("../run", &job, 1, ProcessFingerprint::current(), 1);
        assert!(store.save_execution(&execution).is_err());
        execution.id = "run".to_string();
        assert!(store.save_execution(&execution).is_ok());
        assert!(store.get_execution("../run").is_err());
        assert!(store.claim_delivery("../run", "t", 1).is_err());
    }

    #[test]
    fn file_store_delivery_is_claimed_at_most_once() {
        let dir = temp_dir("delivery");
        let store = FileCronStore::new(&dir).expect("store");
        let job = job("a");
        store.save_job(&job).expect("save");
        let execution =
            CronExecution::claimed("run-1", &job, 10, ProcessFingerprint::current(), 100);
        store.save_execution(&execution).expect("save exec");
        let first = store
            .claim_delivery("run-1", "token", 101)
            .expect("claim")
            .expect("first");
        assert_eq!(first.claim_token, "token");
        assert!(
            store
                .claim_delivery("run-1", "other", 102)
                .expect("second")
                .is_none()
        );
        assert!(
            store
                .claim_delivery("missing", "x", 103)
                .expect("missing")
                .is_none()
        );
    }

    #[test]
    fn process_fingerprint_detects_pid_reuse_via_start_time() {
        let current = ProcessFingerprint::current();
        assert!(current.is_alive());
        if current.start_time.is_some() {
            let reused = ProcessFingerprint {
                pid: current.pid,
                start_time: current.start_time.map(|value| value + 1),
            };
            assert!(!reused.is_alive(), "different start time must be dead");
        }
        assert!(!ProcessFingerprint::for_pid(999_999_999).is_alive());
    }

    #[test]
    fn script_paths_cannot_escape_managed_directory() {
        let dir = temp_dir("scripts");
        fs::create_dir_all(dir.join("nested")).expect("nested");
        fs::write(dir.join("run.sh"), "#!/bin/sh\n").expect("write");
        let resolved = resolve_script_path(&dir, "run.sh").expect("valid");
        assert!(resolved.starts_with(fs::canonicalize(&dir).expect("canonical")));
        assert_eq!(
            resolve_script_path(&dir, "nested/missing.sh").expect("lexical"),
            dir.join("nested/missing.sh")
        );
        assert!(resolve_script_path(&dir, "../escape.sh").is_err());
        assert!(resolve_script_path(&dir, "/etc/passwd").is_err());
    }

    #[test]
    fn job_validation_rejects_bad_ids_and_relative_workdirs() {
        let mut job = job("a/b");
        assert!(job.validate(None).is_err());
        job.id = "a".to_string();
        job.prompt = "  ".to_string();
        assert!(job.validate(None).is_err());
        job.prompt = "work".to_string();
        job.workdir = Some("relative".to_string());
        assert!(job.validate(None).is_err());
        job.workdir = Some("/abs".to_string());
        assert!(job.validate(None).is_ok());
    }
}
