//! 无人值守 cron 调度器：tick、pending 槽位恢复、execution 账本与 delivery。
//!
//! 设计参照：
//! - `reference_project/deer-flow/backend/packages/harness/deerflow/persistence/scheduled_tasks/sql.py`
//!   的“先推进 next_run_at，再 claim/launch”顺序、lease/owner 存活判断；
//! - `reference_project/deer-flow/backend/packages/harness/deerflow/persistence/scheduled_task_runs/finalization.py`
//!   的终态推进；
//! - `reference_project/ClaudeCode/src/utils/cronScheduler.ts` 的启动追赶与 missed 记录；
//! - `reference_project/cline/sdk/packages/core/src/cron/runner/cron-runner.ts` 的
//!   claim token / 终态只结算一次。
//!
//! # tick 的顺序（不可交换）
//!
//! 1. 扫描 active execution：owner 指纹已死 → `unknown`（delivery 若已 claim 也标 unknown，
//!    **永不重试**）；
//! 2. 对每个 job：
//!    - `pending_slot` 存在：查同 `(job_id, slot)` 的 execution。
//!      - 有 active execution 且 owner 存活 → 跳过（别的进程在跑）；
//!      - 有 active execution 且 owner 已死 → unknown，清 pending、推进调度，**不重派**；
//!      - 有任何终态 execution → 清 pending、推进调度（崩溃在 dispatch 之后、清 pending 之前）；
//!      - 没有任何 execution（崩在持久化 pending 之后、写 execution 之前）→ 在 grace 内
//!        claim 并 dispatch **恰好一次**，超过 grace 记 `missed`；
//!    - 没有 pending 且 `next_run_at <= now`：
//!      - 超过 `next_run_at + grace_minutes` → `missed`；
//!      - 否则先 CAS 写入 `next_run_at`（已推进）与 `pending_slot`，再写 execution 并 dispatch。
//!
//! # catch-up
//!
//! 恢复时若推进后的时间仍落在过去，下一次时间取 `now + clamp(半周期, 120s, 2h)`，
//! 避免停机很久后连续补跑。
//!
//! # delivery
//!
//! [`CronScheduler::claim_delivery`] 委托 store 的原子 claim；每行最多一次。claim 后若
//! owner 死亡，`recover_orphaned_executions` 把 execution 标为 `unknown` 并设置
//! `delivery.unknown = true`，之后 `mark_delivered` 拒绝执行。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::schedule::CompiledSchedule;
use super::store::{
    CronError, CronExecution, CronJob, CronStore, DeliveryRecord, ExecutionStatus,
    ProcessFingerprint,
};
use super::time::now_secs;

/// catch-up 半周期下限（秒）。
pub const CATCH_UP_MIN_SECS: u64 = 120;
/// catch-up 半周期上限（秒）。
pub const CATCH_UP_MAX_SECS: u64 = 7_200;

/// 调度器配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronConfig {
    /// 受管脚本目录；设置了 `script` 的 job 只能是该目录内的相对路径。
    pub managed_script_dir: Option<PathBuf>,
    /// catch-up 下限。
    pub catch_up_min_secs: u64,
    /// catch-up 上限。
    pub catch_up_max_secs: u64,
}

impl Default for CronConfig {
    fn default() -> Self {
        Self {
            managed_script_dir: None,
            catch_up_min_secs: CATCH_UP_MIN_SECS,
            catch_up_max_secs: CATCH_UP_MAX_SECS,
        }
    }
}

/// 交给 agent 的一次调度。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchRequest {
    /// job 快照（含 prompt/skills/model/script/context_from/workdir/timezone）。
    pub job: CronJob,
    /// 计划槽位。
    pub slot: i64,
    /// 对应的 execution id。
    pub execution_id: String,
}

/// 执行器契约；生产实现把请求交给 AgentSession，测试用记录替身。
// `async_trait` 展开的 Future 方法自带 `#[must_use]`，clippy 看不到宏内属性。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait JobDispatcher: Send + Sync {
    /// 执行一次；`Err` 会写进 execution 的 `error` 并标 `failed`。
    async fn dispatch(&self, request: DispatchRequest) -> Result<(), String>;
}

/// 一次 tick 的摘要。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TickReport {
    /// 首次初始化 `next_run_at` 的 job。
    pub initialized: Vec<String>,
    /// 实际 dispatch 的 job（含 pending 恢复）。
    pub dispatched: Vec<String>,
    /// 从 pending_slot 恢复的 job。
    pub recovered: Vec<String>,
    /// 因 owner 死亡标 unknown 的 execution id。
    pub unknown: Vec<String>,
    /// 超过 grace 记 missed 的 job。
    pub missed: Vec<String>,
    /// dispatch 失败的 job。
    pub failed: Vec<String>,
    /// 因并发竞争/其他 owner 持有而跳过的 job。
    pub skipped: Vec<String>,
}

/// cron 调度器。
pub struct CronScheduler<S: CronStore> {
    store: Arc<S>,
    owner: ProcessFingerprint,
    config: CronConfig,
    dispatcher: Arc<dyn JobDispatcher>,
    counter: AtomicU64,
}

impl<S: CronStore> CronScheduler<S> {
    /// 构造；`owner` 通常是 [`ProcessFingerprint::current`]。
    pub fn new(
        store: Arc<S>,
        owner: ProcessFingerprint,
        config: CronConfig,
        dispatcher: Arc<dyn JobDispatcher>,
    ) -> Self {
        Self {
            store,
            owner,
            config,
            dispatcher,
            counter: AtomicU64::new(0),
        }
    }

    /// 当前配置。
    pub fn config(&self) -> &CronConfig {
        &self.config
    }

    /// 执行一次 tick。
    pub async fn tick(&self, now: i64) -> Result<TickReport, CronError> {
        let mut report = TickReport::default();
        report
            .unknown
            .extend(self.recover_orphaned_executions(now)?);

        for job in self.store.list_jobs()? {
            if !job.enabled {
                report.skipped.push(job.id);
                continue;
            }
            let compiled = match job.compile() {
                Ok(compiled) => compiled,
                Err(error) => {
                    report.skipped.push(format!("{}: {error}", job.id));
                    continue;
                }
            };
            if job.pending_slot.is_some() {
                self.recover_pending(job, &compiled, now, &mut report)
                    .await?;
                continue;
            }
            let Some(due) = job.next_run_at else {
                let mut updated = job.clone();
                updated.next_run_at = compiled.initial_next(now);
                let expected = job.version;
                if self.store.compare_and_swap_job(expected, &updated)? {
                    report.initialized.push(job.id);
                } else {
                    report.skipped.push(job.id);
                }
                continue;
            };
            if due > now {
                continue;
            }
            if Self::exceeded_grace(&job, due, now) {
                self.record_missed(&job, due, now, "next_run_at exceeded grace", &mut report)?;
                continue;
            }
            self.claim_and_dispatch(job, compiled, due, now, &mut report, false)
                .await?;
        }
        Ok(report)
    }

    /// CAS 推进 `next_run_at`、写入 `pending_slot`，然后 dispatch。
    async fn claim_and_dispatch(
        &self,
        job: CronJob,
        compiled: CompiledSchedule,
        slot: i64,
        now: i64,
        report: &mut TickReport,
        recovered: bool,
    ) -> Result<(), CronError> {
        let expected = job.version;
        let mut updated = job.clone();
        updated.next_run_at = self.advance(&compiled, slot, now);
        updated.pending_slot = Some(slot);
        updated.pending_owner = Some(self.owner.clone());
        if !self.store.compare_and_swap_job(expected, &updated)? {
            report.skipped.push(job.id);
            return Ok(());
        }
        updated.version = expected + 1;
        self.dispatch_claimed(&updated, slot, now, report, recovered)
            .await
    }

    async fn dispatch_claimed(
        &self,
        job: &CronJob,
        slot: i64,
        now: i64,
        report: &mut TickReport,
        recovered: bool,
    ) -> Result<(), CronError> {
        let execution_id = self.execution_id(&job.id, slot, now);
        let mut execution =
            CronExecution::claimed(execution_id.clone(), job, slot, self.owner.clone(), now);
        self.store.save_execution(&execution)?;
        execution.status = ExecutionStatus::Running;
        // 真实开始时间（tick 的 `now` 只是调度槽位时间，长任务用它会让耗时为 0）。
        execution.started_at = Some(now_secs());
        execution.attempts = 1;
        self.store.save_execution(&execution)?;

        report.dispatched.push(job.id.clone());
        if recovered {
            report.recovered.push(job.id.clone());
        }

        let request = DispatchRequest {
            job: job.clone(),
            slot,
            execution_id: execution_id.clone(),
        };
        let result = self.dispatcher.dispatch(request).await;
        let mut finished = execution;
        match result {
            Ok(()) => {
                finished.status = ExecutionStatus::Completed;
            }
            Err(error) => {
                finished.status = ExecutionStatus::Failed;
                finished.error = Some(error);
                report.failed.push(job.id.clone());
            }
        }
        finished.finished_at = Some(now_secs());
        self.store.save_execution(&finished)?;

        let mut cleared = job.clone();
        cleared.pending_slot = None;
        cleared.pending_owner = None;
        cleared.last_run_at = Some(now);
        // one-shot 完成后停用，否则下一次 tick 会把过去的 at 重新初始化为 next_run_at，
        // 在 grace 窗口内再次触发。
        if job
            .compile()
            .map(|compiled| compiled.is_one_shot())
            .unwrap_or(false)
        {
            cleared.enabled = false;
        }
        let expected = cleared.version;
        self.store.compare_and_swap_job(expected, &cleared)?;
        Ok(())
    }

    async fn recover_pending(
        &self,
        job: CronJob,
        compiled: &CompiledSchedule,
        now: i64,
        report: &mut TickReport,
    ) -> Result<(), CronError> {
        let Some(slot) = job.pending_slot else {
            return Ok(());
        };
        let executions = self.store.list_executions()?;
        let existing = executions
            .iter()
            .filter(|execution| execution.job_id == job.id && execution.slot == slot)
            .max_by_key(|execution| execution.claimed_at);

        if let Some(existing) = existing {
            let mut execution = existing.clone();
            if execution.status.is_active() {
                if execution.owner.is_alive() {
                    report.skipped.push(job.id);
                    return Ok(());
                }
                execution.status = ExecutionStatus::Unknown;
                execution.finished_at = Some(now);
                execution.error = Some("owner process died".to_string());
                if let Some(delivery) = execution.delivery.as_mut() {
                    delivery.unknown = true;
                }
                self.store.save_execution(&execution)?;
                report.unknown.push(execution.id.clone());
            }
            // 已有 execution（无论刚才标 unknown 还是终态）都绝不重派。
            let expected = job.version;
            let mut updated = job.clone();
            updated.pending_slot = None;
            updated.pending_owner = None;
            updated.next_run_at = self.advance(compiled, slot, now);
            if compiled.is_one_shot() {
                updated.enabled = false;
            }
            if self.store.compare_and_swap_job(expected, &updated)? {
                report.recovered.push(job.id);
            }
            return Ok(());
        }

        if Self::exceeded_grace(&job, slot, now) {
            self.record_missed(&job, slot, now, "pending slot exceeded grace", report)?;
            return Ok(());
        }
        if let Some(owner) = &job.pending_owner
            && owner.is_alive()
        {
            report.skipped.push(job.id);
            return Ok(());
        }
        // 崩在“持久化 pending”与“写 execution”之间：claim owner 后恰好 dispatch 一次。
        let expected = job.version;
        let mut updated = job.clone();
        updated.pending_owner = Some(self.owner.clone());
        if !self.store.compare_and_swap_job(expected, &updated)? {
            report.skipped.push(job.id);
            return Ok(());
        }
        updated.version = expected + 1;
        self.dispatch_claimed(&updated, slot, now, report, true)
            .await
    }

    fn record_missed(
        &self,
        job: &CronJob,
        slot: i64,
        now: i64,
        reason: &str,
        report: &mut TickReport,
    ) -> Result<(), CronError> {
        let execution_id = self.execution_id(&job.id, slot, now);
        let mut execution =
            CronExecution::claimed(execution_id, job, slot, self.owner.clone(), now);
        execution.status = ExecutionStatus::Missed;
        execution.finished_at = Some(now);
        execution.error = Some(reason.to_string());
        self.store.save_execution(&execution)?;

        let compiled = job.compile()?;
        let expected = job.version;
        let mut updated = job.clone();
        updated.pending_slot = None;
        updated.pending_owner = None;
        updated.next_run_at = self.advance(&compiled, slot, now);
        if compiled.is_one_shot() {
            updated.enabled = false;
        }
        if self.store.compare_and_swap_job(expected, &updated)? {
            report.missed.push(job.id.clone());
        } else {
            report.skipped.push(job.id.clone());
        }
        Ok(())
    }

    /// 把 owner 已死亡的 active execution 标为 `unknown`；返回 execution id。
    ///
    /// delivery 已 claim 的同样标 `unknown`，调用方不得再投递。
    pub fn recover_orphaned_executions(&self, now: i64) -> Result<Vec<String>, CronError> {
        let mut recovered = Vec::new();
        for mut execution in self.store.list_executions()? {
            if !execution.status.is_active() || execution.owner.is_alive() {
                continue;
            }
            execution.status = ExecutionStatus::Unknown;
            execution.finished_at = Some(now);
            execution.error = Some("owner process died".to_string());
            if let Some(delivery) = execution.delivery.as_mut() {
                delivery.unknown = true;
            }
            self.store.save_execution(&execution)?;
            recovered.push(execution.id);
        }
        Ok(recovered)
    }

    /// 原子 claim 一次 delivery；每行最多一次。
    pub fn claim_delivery(
        &self,
        execution_id: &str,
        token: impl Into<String>,
    ) -> Result<DeliveryRecord, CronError> {
        let now = now_secs();
        match self
            .store
            .claim_delivery(execution_id, &token.into(), now)?
        {
            Some(record) => Ok(record),
            None => {
                if self.store.get_execution(execution_id)?.is_none() {
                    Err(CronError::ExecutionNotFound(execution_id.to_string()))
                } else {
                    Err(CronError::DeliveryAlreadyClaimed(execution_id.to_string()))
                }
            }
        }
    }

    /// 标记 delivery 已投递；token 必须匹配，unknown 一律拒绝。
    pub fn mark_delivered(
        &self,
        execution_id: &str,
        token: &str,
        now: i64,
    ) -> Result<(), CronError> {
        let mut execution = self
            .store
            .get_execution(execution_id)?
            .ok_or_else(|| CronError::ExecutionNotFound(execution_id.to_string()))?;
        let delivery = execution
            .delivery
            .as_mut()
            .ok_or_else(|| CronError::DeliveryAlreadyClaimed(execution_id.to_string()))?;
        if delivery.unknown {
            return Err(CronError::DeliveryUnknown(execution_id.to_string()));
        }
        if delivery.claim_token != token {
            return Err(CronError::DeliveryTokenMismatch(execution_id.to_string()));
        }
        delivery.delivered_at = Some(now);
        self.store.save_execution(&execution)?;
        Ok(())
    }

    /// 显式取消一次执行；终态保持不变。
    pub fn cancel_execution(&self, execution_id: &str, now: i64) -> Result<(), CronError> {
        let mut execution = self
            .store
            .get_execution(execution_id)?
            .ok_or_else(|| CronError::ExecutionNotFound(execution_id.to_string()))?;
        if execution.status.is_active() {
            execution.status = ExecutionStatus::Cancelled;
            execution.finished_at = Some(now);
            execution.error = Some("cancelled".to_string());
            self.store.save_execution(&execution)?;
        }
        Ok(())
    }

    fn exceeded_grace(job: &CronJob, slot: i64, now: i64) -> bool {
        let grace = job.grace_minutes.saturating_mul(60) as i64;
        now > slot.saturating_add(grace)
    }

    /// 计算下一槽位；若仍在过去则按半周期 clamp 到未来。
    fn advance(&self, compiled: &CompiledSchedule, slot: i64, now: i64) -> Option<i64> {
        if compiled.is_one_shot() {
            return None;
        }
        let next = compiled.next_after(slot).unwrap_or(now);
        if next > now {
            return Some(next);
        }
        let half = compiled
            .half_period_secs(now)
            .unwrap_or(self.config.catch_up_min_secs);
        let minimum = self.config.catch_up_min_secs;
        let maximum = self.config.catch_up_max_secs.max(minimum);
        let delay = half.clamp(minimum, maximum) as i64;
        Some(now.saturating_add(delay))
    }

    fn execution_id(&self, job_id: &str, slot: i64, now: i64) -> String {
        let counter = self.counter.fetch_add(1, Ordering::Relaxed);
        format!("{job_id}-{slot}-{now}-{counter}")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::super::schedule::ScheduleSpec;
    use super::super::store::{MemoryCronStore, resolve_script_path};
    use super::*;

    fn at(text: &str) -> i64 {
        super::super::time::parse_iso8601(text).expect("timestamp")
    }

    #[derive(Default)]
    struct RecordingDispatcher {
        seen: Mutex<Vec<DispatchRequest>>,
        fail: bool,
    }

    #[async_trait]
    impl JobDispatcher for RecordingDispatcher {
        async fn dispatch(&self, request: DispatchRequest) -> Result<(), String> {
            self.seen.lock().expect("seen").push(request);
            if self.fail {
                Err("dispatch failed".to_string())
            } else {
                Ok(())
            }
        }
    }

    struct InspectingDispatcher {
        store: Arc<MemoryCronStore>,
        seen: Mutex<Vec<(Option<i64>, Option<i64>)>>,
    }

    #[async_trait]
    impl JobDispatcher for InspectingDispatcher {
        async fn dispatch(&self, _request: DispatchRequest) -> Result<(), String> {
            let jobs = self.store.list_jobs().expect("jobs");
            let job = jobs.first().expect("job");
            self.seen
                .lock()
                .expect("seen")
                .push((job.next_run_at, job.pending_slot));
            Ok(())
        }
    }

    fn job(id: &str, spec: ScheduleSpec, next_run_at: i64) -> CronJob {
        let mut job = CronJob::new(id, "prompt", spec);
        job.next_run_at = Some(next_run_at);
        job.grace_minutes = 5;
        job
    }

    fn scheduler(
        store: Arc<MemoryCronStore>,
        dispatcher: Arc<dyn JobDispatcher>,
    ) -> CronScheduler<MemoryCronStore> {
        CronScheduler::new(
            store,
            ProcessFingerprint::current(),
            CronConfig::default(),
            dispatcher,
        )
    }

    #[tokio::test]
    async fn one_shot_fires_once_and_stops() {
        let store = Arc::new(MemoryCronStore::new());
        let dispatcher = Arc::new(RecordingDispatcher::default());
        let now = at("2026-11-07T10:00:00Z");
        store
            .save_job(&job(
                "once",
                ScheduleSpec::at("2026-11-07T09:59:00Z"),
                now - 60,
            ))
            .expect("save");
        let scheduler = scheduler(store.clone(), dispatcher.clone());
        let report = scheduler.tick(now).await.expect("tick");
        assert_eq!(report.dispatched, vec!["once"]);
        assert_eq!(dispatcher.seen.lock().expect("seen").len(), 1);
        let stored = store.get_job("once").expect("get").expect("some");
        assert_eq!(stored.pending_slot, None);
        assert_eq!(stored.next_run_at, None);
        assert!(!stored.enabled, "one-shot must disable itself after firing");

        let report = scheduler.tick(now + 10).await.expect("tick again");
        assert!(report.dispatched.is_empty());
        let report = scheduler.tick(now + 10_000).await.expect("tick much later");
        assert!(report.dispatched.is_empty());
        assert_eq!(dispatcher.seen.lock().expect("seen").len(), 1);
        assert!(
            store
                .list_executions()
                .expect("executions")
                .iter()
                .all(|execution| { execution.status != ExecutionStatus::Missed })
        );
    }

    #[tokio::test]
    async fn next_run_at_and_pending_slot_are_persisted_before_dispatch() {
        let store = Arc::new(MemoryCronStore::new());
        let dispatcher = Arc::new(InspectingDispatcher {
            store: store.clone(),
            seen: Mutex::new(Vec::new()),
        });
        let now = at("2026-11-07T10:00:00Z");
        store
            .save_job(&job("hourly", ScheduleSpec::duration("1h"), now))
            .expect("save");
        let scheduler = scheduler(store.clone(), dispatcher.clone());
        let report = scheduler.tick(now).await.expect("tick");
        assert_eq!(report.dispatched, vec!["hourly"]);
        assert_eq!(
            dispatcher.seen.lock().expect("seen").as_slice(),
            &[(Some(now + 3_600), Some(now))]
        );
        let stored = store.get_job("hourly").expect("get").expect("some");
        assert_eq!(stored.next_run_at, Some(now + 3_600));
        assert_eq!(stored.pending_slot, None);
        assert_eq!(stored.pending_owner, None);
        assert_eq!(stored.last_run_at, Some(now));
        let executions = store.list_executions().expect("executions");
        assert_eq!(executions.len(), 1);
        assert_eq!(executions[0].status, ExecutionStatus::Completed);
    }

    #[tokio::test]
    async fn pending_slot_without_execution_is_recovered_exactly_once() {
        let store = Arc::new(MemoryCronStore::new());
        let dispatcher = Arc::new(RecordingDispatcher::default());
        let now = at("2026-11-07T10:00:00Z");
        let mut job = job("recover", ScheduleSpec::duration("1h"), now + 3_600);
        job.pending_slot = Some(now - 10);
        job.pending_owner = Some(ProcessFingerprint::for_pid(999_999_999));
        store.save_job(&job).expect("save");
        let scheduler = scheduler(store.clone(), dispatcher.clone());
        let report = scheduler.tick(now).await.expect("tick");
        assert_eq!(report.dispatched, vec!["recover"]);
        assert_eq!(report.recovered, vec!["recover"]);
        assert_eq!(dispatcher.seen.lock().expect("seen").len(), 1);

        let stored = store.get_job("recover").expect("get").expect("some");
        assert_eq!(stored.pending_slot, None);
        assert_eq!(stored.next_run_at, Some(now + 3_600));

        let second = scheduler.tick(now + 1).await.expect("second tick");
        assert!(second.dispatched.is_empty());
        assert_eq!(dispatcher.seen.lock().expect("seen").len(), 1);
    }

    #[tokio::test]
    async fn expired_pending_slot_is_missed_not_dispatched() {
        let store = Arc::new(MemoryCronStore::new());
        let dispatcher = Arc::new(RecordingDispatcher::default());
        let now = at("2026-11-07T10:00:00Z");
        let mut job = job("expired", ScheduleSpec::duration("1h"), now + 3_600);
        job.grace_minutes = 0;
        job.pending_slot = Some(now - 1_000);
        job.pending_owner = Some(ProcessFingerprint::for_pid(999_999_999));
        store.save_job(&job).expect("save");
        let scheduler = scheduler(store.clone(), dispatcher.clone());
        let report = scheduler.tick(now).await.expect("tick");
        assert!(report.dispatched.is_empty());
        assert_eq!(report.missed, vec!["expired"]);
        assert_eq!(dispatcher.seen.lock().expect("seen").len(), 0);
        let executions = store.list_executions().expect("executions");
        assert_eq!(executions[0].status, ExecutionStatus::Missed);
    }

    #[tokio::test]
    async fn dead_owner_execution_becomes_unknown_and_is_never_redelivered() {
        let store = Arc::new(MemoryCronStore::new());
        let dispatcher = Arc::new(RecordingDispatcher::default());
        let now = at("2026-11-07T10:00:00Z");
        let slot = now - 10;
        let mut job = job("dead", ScheduleSpec::duration("1h"), now + 3_600);
        job.pending_slot = Some(slot);
        job.pending_owner = Some(ProcessFingerprint::for_pid(999_999_999));
        store.save_job(&job).expect("save");
        let mut execution = CronExecution::claimed(
            "run-dead",
            &job,
            slot,
            ProcessFingerprint::for_pid(999_999_999),
            slot,
        );
        execution.status = ExecutionStatus::Running;
        execution.delivery = Some(DeliveryRecord {
            claim_token: "token".to_string(),
            claimed_at: slot,
            delivered_at: None,
            unknown: false,
        });
        store.save_execution(&execution).expect("save execution");

        let scheduler = scheduler(store.clone(), dispatcher.clone());
        let report = scheduler.tick(now).await.expect("tick");
        assert!(report.dispatched.is_empty());
        assert_eq!(report.unknown, vec!["run-dead"]);
        let stored = store.get_execution("run-dead").expect("get").expect("some");
        assert_eq!(stored.status, ExecutionStatus::Unknown);
        assert!(stored.delivery.as_ref().expect("delivery").unknown);

        let second = scheduler.tick(now + 1).await.expect("second");
        assert!(second.dispatched.is_empty());
        assert_eq!(dispatcher.seen.lock().expect("seen").len(), 0);
        let stored = store.get_job("dead").expect("get").expect("some");
        assert_eq!(stored.pending_slot, None);
    }

    #[tokio::test]
    async fn live_owner_execution_is_left_alone() {
        let store = Arc::new(MemoryCronStore::new());
        let dispatcher = Arc::new(RecordingDispatcher::default());
        let now = at("2026-11-07T10:00:00Z");
        let slot = now - 10;
        let mut job = job("live", ScheduleSpec::duration("1h"), now + 3_600);
        job.pending_slot = Some(slot);
        job.pending_owner = Some(ProcessFingerprint::current());
        store.save_job(&job).expect("save");
        let mut execution =
            CronExecution::claimed("run-live", &job, slot, ProcessFingerprint::current(), slot);
        execution.status = ExecutionStatus::Running;
        store.save_execution(&execution).expect("save");

        let scheduler = scheduler(store.clone(), dispatcher);
        let report = scheduler.tick(now).await.expect("tick");
        assert!(report.dispatched.is_empty());
        assert_eq!(
            store
                .get_execution("run-live")
                .expect("get")
                .expect("some")
                .status,
            ExecutionStatus::Running
        );
    }

    #[tokio::test]
    async fn overdue_jobs_are_missed_and_next_run_uses_catch_up_clamp() {
        let store = Arc::new(MemoryCronStore::new());
        let dispatcher = Arc::new(RecordingDispatcher::default());
        let now = at("2026-11-07T10:00:00Z");
        let mut short = job("short", ScheduleSpec::duration("10s"), now - 100_000);
        short.grace_minutes = 0;
        let mut long = job("long", ScheduleSpec::duration("8h"), now - 100_000);
        long.grace_minutes = 0;
        store.save_job(&short).expect("save");
        store.save_job(&long).expect("save");
        let scheduler = scheduler(store.clone(), dispatcher.clone());
        let report = scheduler.tick(now).await.expect("tick");
        assert_eq!(report.missed.len(), 2);
        assert!(report.dispatched.is_empty());

        let short = store.get_job("short").expect("get").expect("some");
        assert_eq!(short.next_run_at, Some(now + 120), "half 5s clamps to 120s");
        let long = store.get_job("long").expect("get").expect("some");
        assert_eq!(long.next_run_at, Some(now + 7_200), "half 4h clamps to 2h");
    }

    #[tokio::test]
    async fn missing_next_run_is_initialized_on_first_tick() {
        let store = Arc::new(MemoryCronStore::new());
        let dispatcher = Arc::new(RecordingDispatcher::default());
        let now = at("2026-11-07T10:00:00Z");
        let mut job = CronJob::new("cron", "prompt", ScheduleSpec::cron("0 9 * * *"));
        job.next_run_at = None;
        store.save_job(&job).expect("save");
        let scheduler = scheduler(store.clone(), dispatcher);
        let report = scheduler.tick(now).await.expect("tick");
        assert_eq!(report.initialized, vec!["cron"]);
        let stored = store.get_job("cron").expect("get").expect("some");
        assert_eq!(stored.next_run_at, Some(at("2026-11-08T09:00:00Z")));
    }

    #[tokio::test]
    async fn dispatch_failure_is_recorded_as_failed() {
        let store = Arc::new(MemoryCronStore::new());
        let dispatcher = Arc::new(RecordingDispatcher {
            seen: Mutex::new(Vec::new()),
            fail: true,
        });
        let now = at("2026-11-07T10:00:00Z");
        store
            .save_job(&job("fails", ScheduleSpec::duration("1h"), now))
            .expect("save");
        let scheduler = scheduler(store.clone(), dispatcher);
        let report = scheduler.tick(now).await.expect("tick");
        assert_eq!(report.failed, vec!["fails"]);
        let executions = store.list_executions().expect("executions");
        assert_eq!(executions[0].status, ExecutionStatus::Failed);
        assert_eq!(executions[0].error.as_deref(), Some("dispatch failed"));
    }

    #[tokio::test]
    async fn delivery_is_claimed_once_and_unknown_after_owner_death() {
        let store = Arc::new(MemoryCronStore::new());
        let now = at("2026-11-07T10:00:00Z");
        let job = job("delivery", ScheduleSpec::duration("1h"), now);
        store.save_job(&job).expect("save");
        let execution = CronExecution::claimed(
            "run-delivery",
            &job,
            now,
            ProcessFingerprint::for_pid(999_999_999),
            now,
        );
        store.save_execution(&execution).expect("save");
        let scheduler = scheduler(store.clone(), Arc::new(RecordingDispatcher::default()));

        let record = scheduler
            .claim_delivery("run-delivery", "token")
            .expect("claim");
        assert_eq!(record.claim_token, "token");
        let error = scheduler
            .claim_delivery("run-delivery", "second")
            .expect_err("second claim");
        assert!(matches!(error, CronError::DeliveryAlreadyClaimed(_)));

        scheduler
            .recover_orphaned_executions(now + 1)
            .expect("recover");
        let stored = store
            .get_execution("run-delivery")
            .expect("get")
            .expect("some");
        assert_eq!(stored.status, ExecutionStatus::Unknown);
        assert!(stored.delivery.as_ref().expect("delivery").unknown);
        let error = scheduler
            .mark_delivered("run-delivery", "token", now + 2)
            .expect_err("unknown delivery");
        assert!(matches!(error, CronError::DeliveryUnknown(_)));
    }

    #[tokio::test]
    async fn script_escaping_managed_directory_is_rejected_by_validation() {
        let dir = std::env::temp_dir().join(format!(
            "kiana-cron-scripts-{}-{}",
            std::process::id(),
            now_secs()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let mut job = CronJob::new("script", "prompt", ScheduleSpec::duration("1h"));
        job.script = Some("../escape.sh".to_string());
        assert!(job.validate(Some(&dir)).is_err());
        assert!(resolve_script_path(&dir, "../escape.sh").is_err());
    }
}
