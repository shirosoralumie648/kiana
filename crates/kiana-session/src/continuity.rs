//! 第 11 周后半的会话级接线：执行所有权、checkpoint、agent-write ledger 与验收记录。
//!
//! 库层能力分别在 [`crate::execution`]、[`crate::checkpoint`]、[`crate::acceptance`]；
//! 本模块只负责在 `AgentSession` 的生命周期里持有它们，并提供面向上层的最小 API。
//! 默认关闭：只有显式设置 `KIANA_CONTINUITY=1`（或通过
//! [`ContinuityOptions`] 注入）时才创建状态目录、执行 claim 与 checkpoint。
//!
//! # 为什么默认关闭
//!
//! - 执行所有权会为每个会话写状态文件并持有跨进程 claim；对一次性 SDK 会话是额外
//!   负担，也会让“同一会话文件被多个只读宿主打开”变成 Busy。
//! - checkpoint 在每次 mutating tool 前调用外部 `git`；测试与只读会话不需要。
//! - 验收记录会保留完整工具输出（受工具自身上限约束），这是有意的内存成本。
//!
//! 恢复分类（`kiana-session-core::recovery`）不受此开关影响：它只读 transcript，
//! 只有发现 `InterruptedTurn` 时才写 canonical 修复。
//!
//! # 状态目录
//!
//! 默认 `<agent_dir>/continuity`，可用 `KIANA_CONTINUITY_DIR` 覆盖。子目录：
//!
//! - `executions/`：每 session 一个 JSON（[`FileExecutionStore`]）；
//! - `checkpoints/`：私有 git dir 与 ledger（[`CheckpointManager`]）；
//! - `background/`：后台任务输出文件（[`BackgroundTaskManager`]）；
//! - `work/`：Work 账本（[`crate::subagent::work_store::DurableWorkStore`]）。
//!
//! 后台任务的 journal/notifier 使用 `kiana-durable` 上的
//! [`DurableTaskJournal`]（见 `background::durable`）：每个会话一个 durable JSONL
//! 存储，记录、attempt 计数与终态通知都由 durable 事务持久化，进程重启后
//! `BackgroundTaskManager::sweep` 能按持久记录恢复或 terminalize。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use kiana_ai::types::JsonValue;

use crate::acceptance::{
    AcceptanceContext, AcceptanceReport, ExecutionRecord, evaluate_acceptance,
};
use crate::background::journal::BackgroundTaskNotification;
use crate::background::{BackgroundTaskManager, BackgroundTaskOptions, DurableTaskJournal};
use crate::checkpoint::{CheckpointManager, CheckpointOptions};
use crate::execution::{
    ExecutionController, ExecutionLease, ExecutionStatus, FileExecutionStore, RecoverResult,
};
use crate::subagent::result::ReceiptStatus;
use crate::subagent::work_store::DurableWorkStore;

/// 连续性接线的配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuityOptions {
    /// 状态根目录。
    pub state_dir: PathBuf,
    /// 启用执行所有权（每 session 一个跨进程 claim）。
    pub execution: bool,
    /// 启用私有 git checkpoint 与 agent-write ledger。
    pub checkpoints: bool,
    /// 启用后台任务管理器。
    pub background: bool,
    /// 启用 Work 账本（只在宿主注入 `WorkStore` 时生效）。
    pub work: bool,
    /// 采集验收执行记录。
    pub acceptance: bool,
    /// 执行尝试上限（默认 10）。
    pub max_execution_attempts: u32,
}

impl ContinuityOptions {
    /// 全部能力开启。
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            execution: true,
            checkpoints: true,
            background: true,
            work: true,
            acceptance: true,
            max_execution_attempts: crate::execution::DEFAULT_MAX_EXECUTION_ATTEMPTS,
        }
    }

    /// 从环境变量读取：`KIANA_CONTINUITY` 为真值时才启用。
    ///
    /// `KIANA_CONTINUITY_DIR` 覆盖状态目录；`KIANA_CONTINUITY_EXECUTION`/
    /// `KIANA_CONTINUITY_CHECKPOINTS`/`KIANA_CONTINUITY_BACKGROUND`/
    /// `KIANA_CONTINUITY_WORK`/`KIANA_CONTINUITY_ACCEPTANCE` 可单独关闭
    /// （设 `0`/`false`/`off`）。
    pub fn from_env() -> Option<Self> {
        let enabled = std::env::var("KIANA_CONTINUITY")
            .ok()
            .is_some_and(|value| env_truthy(&value));
        if !enabled {
            return None;
        }
        let state_dir = std::env::var("KIANA_CONTINUITY_DIR")
            .ok()
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| kiana_core::config::get_agent_dir().join("continuity"));
        Some(Self::new(state_dir).with_env_overrides())
    }

    /// 应用部署侧环境变量覆盖（单个能力的 kill-switch）。
    ///
    /// settings 提供 `continuity` 时仍要尊重 `KIANA_CONTINUITY_EXECUTION` 等开关：
    /// 这些是运维开关，不能因为 settings 里写了 `continuity` 就完全失效。目录与
    /// `enabled` 仍由 settings 决定（`KIANA_CONTINUITY_DIR` 只在纯环境变量启用时生效）。
    pub(crate) fn with_env_overrides(mut self) -> Self {
        self.execution = env_kill_switch("KIANA_CONTINUITY_EXECUTION", self.execution);
        self.checkpoints = env_kill_switch("KIANA_CONTINUITY_CHECKPOINTS", self.checkpoints);
        self.background = env_kill_switch("KIANA_CONTINUITY_BACKGROUND", self.background);
        self.work = env_kill_switch("KIANA_CONTINUITY_WORK", self.work);
        self.acceptance = env_kill_switch("KIANA_CONTINUITY_ACCEPTANCE", self.acceptance);
        self
    }

    /// 从合并后的 settings `continuity` 键读取。
    ///
    /// 支持两种形状（与 `KIANA_CONTINUITY` 等价，不需要环境变量）：
    ///
    /// ```json
    /// { "continuity": true }
    /// { "continuity": { "enabled": true, "dir": "/custom/state",
    ///                   "execution": true, "checkpoints": false,
    ///                   "background": true, "work": true, "acceptance": true } }
    /// ```
    ///
    /// 对象缺省 `enabled` 视为启用；`enabled: false` 返回 `None`。`dir` 缺省时与
    /// 环境变量路径一致。键不存在或类型不符时返回 `None`（由 settings 的严格诊断
    /// 负责报告类型错误）。
    /// settings 是否**显式**关闭 continuity（`false` 或对象 `enabled:false`）。
    ///
    /// `from_settings` 对“键不存在/类型不符”和“显式关闭”都返回 `None`；生产接线必须
    /// 区分两者，否则显式关闭会被 `KIANA_CONTINUITY=1` 的环境回退重新打开。
    pub fn settings_disable_continuity(value: Option<&JsonValue>) -> bool {
        match value {
            Some(JsonValue::Bool(false)) => true,
            Some(JsonValue::Object(object)) => {
                matches!(object.get("enabled"), Some(enabled) if enabled.as_bool() != Some(true))
            }
            _ => false,
        }
    }

    pub fn from_settings(value: Option<&JsonValue>) -> Option<Self> {
        let value = value?;
        if let Some(enabled) = value.as_bool() {
            return enabled
                .then(|| Self::new(kiana_core::config::get_agent_dir().join("continuity")));
        }
        let object = value.as_object()?;
        if !object
            .get("enabled")
            .map(|enabled| enabled.as_bool().unwrap_or(false))
            .unwrap_or(true)
        {
            return None;
        }
        let state_dir = object
            .get("dir")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| kiana_core::config::get_agent_dir().join("continuity"));
        let mut options = Self::new(state_dir);
        let toggle = |name: &str, default: bool| {
            object
                .get(name)
                .and_then(JsonValue::as_bool)
                .unwrap_or(default)
        };
        options.execution = toggle("execution", options.execution);
        options.checkpoints = toggle("checkpoints", options.checkpoints);
        options.background = toggle("background", options.background);
        options.work = toggle("work", options.work);
        options.acceptance = toggle("acceptance", options.acceptance);
        Some(options)
    }

    fn execution_dir(&self) -> PathBuf {
        self.state_dir.join("executions")
    }

    fn checkpoint_dir(&self) -> PathBuf {
        self.state_dir.join("checkpoints")
    }

    fn background_dir(&self) -> PathBuf {
        self.state_dir.join("background")
    }

    fn work_dir(&self) -> PathBuf {
        self.state_dir.join("work")
    }
}

/// 单个能力的部署侧开关：只能把 settings 已启用的能力关掉，不能重新打开
/// settings 显式关闭的能力（`KIANA_CONTINUITY_WORK=1` 不能覆盖 `work:false`）。
fn env_kill_switch(name: &str, enabled: bool) -> bool {
    match std::env::var(name) {
        Ok(value) => enabled && env_truthy(&value),
        Err(_) => enabled,
    }
}

fn env_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// 会话持有的连续性状态。
pub struct ContinuityRuntime {
    options: ContinuityOptions,
    execution: Option<Arc<ExecutionController<FileExecutionStore>>>,
    execution_lease: Mutex<Option<ExecutionLease<FileExecutionStore>>>,
    checkpoints: Option<CheckpointManager>,
    background: Option<Arc<BackgroundTaskManager>>,
    background_journal: Option<Arc<DurableTaskJournal>>,
    /// Work 账本；只在 `options.work` 开启时创建（构造不触碰文件系统）。
    work_store: Option<Arc<DurableWorkStore>>,
    /// 已取走、但尚未确认落盘的 terminal 通知（按 `task_id:terminal_seq` 去重）。
    ///
    /// 只有父会话把它们持久化成 `kiana.background-task` 自定义消息后才提交
    /// `consumedDeliveries`；run 在落盘前失败/进程退出时保持未确认，重启后会被
    /// 重放（at-least-once）。
    pending_background_acks: Mutex<Vec<BackgroundTaskNotification>>,
    acceptance: bool,
    records: Mutex<Vec<ExecutionRecord>>,
    next_record_seq: AtomicU64,
    diagnostics: Mutex<Vec<String>>,
}

impl std::fmt::Debug for ContinuityRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ContinuityRuntime")
            .field("state_dir", &self.options.state_dir)
            .field("execution", &self.execution.is_some())
            .field("checkpoints", &self.checkpoints.is_some())
            .field("background", &self.background.is_some())
            .field("background_journal", &self.background_journal.is_some())
            .field("work_store", &self.work_store.is_some())
            .field("acceptance", &self.acceptance)
            .finish_non_exhaustive()
    }
}

impl ContinuityRuntime {
    /// 新建运行时。目录懒创建：构造本身不触碰文件系统。
    pub fn new(options: ContinuityOptions) -> Arc<Self> {
        let execution = options.execution.then(|| {
            let controller = ExecutionController::new(Arc::new(FileExecutionStore::new(
                options.execution_dir(),
            )))
            .with_max_attempts(options.max_execution_attempts);
            Arc::new(controller)
        });
        let checkpoints = options.checkpoints.then(|| {
            CheckpointManager::new(options.checkpoint_dir(), CheckpointOptions::default())
        });
        let (background, background_journal) = if options.background {
            // 生产 journal/notifier：durable 事务存储 + 通知队列（见 `background::durable`）。
            let journal = Arc::new(DurableTaskJournal::new(options.background_dir()));
            let manager = BackgroundTaskManager::new(BackgroundTaskOptions::new(
                options.background_dir(),
                Arc::clone(&journal) as Arc<dyn crate::background::BackgroundTaskJournal>,
                Arc::clone(&journal) as Arc<dyn crate::background::BackgroundTaskNotifier>,
            ));
            (Some(manager), Some(journal))
        } else {
            (None, None)
        };
        let work_store = options
            .work
            .then(|| Arc::new(DurableWorkStore::new(options.work_dir())));
        Arc::new(Self {
            acceptance: options.acceptance,
            execution,
            execution_lease: Mutex::new(None),
            checkpoints,
            background,
            background_journal,
            work_store,
            pending_background_acks: Mutex::new(Vec::new()),
            records: Mutex::new(Vec::new()),
            next_record_seq: AtomicU64::new(1),
            diagnostics: Mutex::new(Vec::new()),
            options,
        })
    }

    /// 配置。
    pub fn options(&self) -> &ContinuityOptions {
        &self.options
    }

    /// Work 账本；`continuity.work` 关闭时为 `None`。
    pub fn work_store(&self) -> Option<&Arc<DurableWorkStore>> {
        self.work_store.as_ref()
    }

    /// 诊断（执行所有权拒绝、checkpoint 失败等）。
    pub fn diagnostics(&self) -> Vec<String> {
        self.diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(crate) fn push_diagnostic(&self, message: String) {
        let mut diagnostics = self
            .diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if diagnostics.len() >= 64 {
            diagnostics.remove(0);
        }
        diagnostics.push(message);
    }

    /// 会话启动：获取/恢复执行 claim。
    ///
    /// 返回 `Err` 表示当前进程不能执行这个 session（别的存活进程持有 claim，或尝试
    /// 次数已耗尽）；调用方应拒绝模型 run。死进程留下的 claim 会走
    /// [`ExecutionController::recover`]（durably +1 attempt）。
    pub fn begin_session(&self, session_id: &str) -> Result<(), String> {
        // 会话启动时做一次 checkpoint retention 维护：否则 `CheckpointManager::prune`
        // 在生产路径永远没有调用方，私有 git 目录与 ledger 会无界增长。
        self.prune_checkpoints();
        let Some(controller) = &self.execution else {
            return Ok(());
        };
        // recover 的 sweep 假定“上次持有者已崩溃”；两个同时存活的进程打开同一会话时，
        // 后启动者会直接接管 claim。这里先用状态文件做 owner 存活检查，把活 claim
        // 当作 Busy，避免抢占。
        match controller.state(session_id) {
            Ok(Some(state)) => {
                if let Some(claim) = &state.claim {
                    // `clean_shutdown` 是主动交接：即使进程仍存活也允许下一个会话接管
                    // （不消耗 attempt）。其余活 claim 视为 Busy，避免两个存活进程并发。
                    if !claim.clean_shutdown && claim.pid.is_none_or(process_is_alive) {
                        return Err(format!(
                            "session is already being executed by {} (pid {})",
                            claim.owner,
                            claim
                                .pid
                                .map(|pid| pid.to_string())
                                .unwrap_or_else(|| "unknown".to_string())
                        ));
                    }
                }
            }
            Ok(None) => {}
            Err(error) => {
                self.push_diagnostic(format!("execution state unreadable: {error}"));
            }
        }
        let owner = format!("kiana-session:{}", std::process::id());
        match controller.recover(session_id, owner) {
            Ok(RecoverResult::Acquired(lease) | RecoverResult::Resumed(lease)) => {
                *self
                    .execution_lease
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(lease);
                Ok(())
            }
            Ok(RecoverResult::Terminalized { reason, .. }) => {
                Err(format!("execution_attempts_exhausted: {reason}"))
            }
            Ok(RecoverResult::Busy(claim)) => Err(format!(
                "session is already being executed by {} (pid {})",
                claim.owner,
                claim
                    .pid
                    .map(|pid| pid.to_string())
                    .unwrap_or_else(|| "unknown".to_string())
            )),
            Err(error) => Err(format!("execution ownership unavailable: {error}")),
        }
    }

    /// 一轮 run 开始：确保当前进程持有该会话的 execution claim。
    ///
    /// 会话构造时的 [`Self::begin_session`] 已经取得首个 claim；一次 run 的终态会通过
    /// [`Self::finish_run`] 释放 claim，因此后续 run 在这里重新获取。与参照实现一致，
    /// claim 的生命周期是 per-turn 而不是 per-process：run 之间不阻止其他进程接手。
    pub fn begin_run(&self, session_id: &str) -> Result<(), String> {
        if self.execution.is_none() {
            return Ok(());
        }
        {
            let lease = self
                .execution_lease
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if lease.is_some() {
                return Ok(());
            }
        }
        self.begin_session(session_id)
    }

    /// 一轮 run 的终态：释放 claim 并发布一次 `Execution.Terminal`。
    ///
    /// 成功、失败、取消都走这里；进程正常退出（dispose）不走这里，而是
    /// [`Self::shutdown`] 的 clean shutdown（保留 claim、不消耗 attempt）。
    pub fn finish_run(&self, status: ExecutionStatus, reason: Option<String>) {
        let mut guard = self
            .execution_lease
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(mut lease) = guard.take() else {
            return;
        };
        if lease.is_finished() {
            return;
        }
        if let Err(error) = lease.finish(status, reason) {
            self.push_diagnostic(format!("execution lease finish failed: {error}"));
        }
    }

    /// 当前是否持有一个未结算的 execution lease（测试/诊断）。
    pub fn holds_execution_lease(&self) -> bool {
        self.execution_lease
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }

    /// 按默认 [`RetentionPolicy`](crate::checkpoint::RetentionPolicy) 清理 checkpoint。
    ///
    /// 权限错误（例如只读状态目录）只记诊断，不阻止会话启动；`too_broad` 之类的单项目
    /// 拒绝也由 `prune` 内部保留并报告。
    pub fn prune_checkpoints(&self) {
        let Some(manager) = &self.checkpoints else {
            return;
        };
        match manager.prune(&crate::checkpoint::RetentionPolicy::default()) {
            Ok(report) => {
                if !report.removed_projects.is_empty()
                    || report.removed_ledger_entries > 0
                    || report.reclaimed_bytes > 0
                {
                    self.push_diagnostic(format!(
                        "checkpoint prune: removed {} project(s), {} ledger entrie(s), {} bytes",
                        report.removed_projects.len(),
                        report.removed_ledger_entries,
                        report.reclaimed_bytes
                    ));
                }
            }
            Err(error) => self.push_diagnostic(format!("checkpoint prune failed: {error}")),
        }
    }

    /// 取走后台任务的 terminal 通知（父会话下一轮 run 开始时注入）。
    ///
    /// 取走时**不**确认消费：调用方把通知持久化成 canonical 条目后必须调用
    /// [`Self::ack_background_notification`]。这样 run 在落盘前失败时通知会在下次
    /// 启动 sweep 中重放（at-least-once），而不是永久丢失。
    pub fn take_background_notifications(
        &self,
        parent_session_id: &str,
    ) -> Vec<BackgroundTaskNotification> {
        let Some(journal) = self.background_journal.as_ref() else {
            return Vec::new();
        };
        let notifications = journal.take_notifications_for(parent_session_id);
        if notifications.is_empty() {
            return notifications;
        }
        {
            let mut pending = self
                .pending_background_acks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for notification in &notifications {
                let key = format!("{}:{}", notification.task_id, notification.terminal_seq);
                if !pending.iter().any(|existing| {
                    format!("{}:{}", existing.task_id, existing.terminal_seq) == key
                }) {
                    pending.push(notification.clone());
                }
            }
        }
        notifications
    }

    /// 确认一条通知已被持久化为 canonical 条目。
    ///
    /// 确认是异步的：关闭进程/确认失败只会让下一次启动重放一次（幂等键去重），不会
    /// 阻塞当前 turn。
    pub fn ack_background_notification(&self, task_id: &str, terminal_seq: u64) {
        let Some(journal) = self.background_journal.as_ref() else {
            return;
        };
        let acknowledged = {
            let mut pending = self
                .pending_background_acks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut acknowledged = Vec::new();
            let mut index = 0;
            while index < pending.len() {
                if pending[index].task_id == task_id && pending[index].terminal_seq == terminal_seq
                {
                    acknowledged.push(pending.remove(index));
                } else {
                    index += 1;
                }
            }
            acknowledged
        };
        if acknowledged.is_empty() {
            return;
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let journal = Arc::clone(journal);
            handle.spawn(async move {
                let _ = journal.mark_deliveries_consumed(&acknowledged).await;
            });
        }
    }

    /// 当前待投递的后台任务通知数量。
    pub fn pending_background_notifications(&self) -> usize {
        self.background_journal
            .as_ref()
            .map(|journal| journal.pending_notifications())
            .unwrap_or(0)
    }

    /// 会话正常关闭：保留 claim 但标记 clean shutdown（不消耗 attempt）。
    pub fn shutdown(&self) {
        let mut lease = self
            .execution_lease
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(lease) = lease.as_mut()
            && let Err(error) = lease.clean_shutdown()
        {
            self.push_diagnostic(format!("execution clean shutdown failed: {error}"));
        }
        lease.take();
        drop(lease);
        // 在途 Work / 后台任务仍会写账本、journal 与 transcript：此时释放 durable
        // 目录租约，等于把目录交给下一个进程，而且 `release_sessions` 会置位
        // `closed`，让在途写入直接失败。把释放留给进程退出；下一次启动的 sweep
        // 仍会按远端 pid 回收。
        if crate::subagent::work_store::any_work_is_live() {
            self.push_diagnostic("work store shutdown deferred: work is still running".to_string());
        } else if let Some(store) = &self.work_store
            && !store.release_sessions()
        {
            self.push_diagnostic(
                "work store shutdown could not release session leases".to_string(),
            );
        }
        if crate::background::any_task_is_running() {
            self.push_diagnostic(
                "background journal shutdown deferred: background tasks are still running"
                    .to_string(),
            );
        } else if let Some(journal) = &self.background_journal
            && !journal.release_sessions()
        {
            self.push_diagnostic(
                "background journal shutdown could not release session leases".to_string(),
            );
        }
    }

    /// 是否启用了执行所有权。
    pub fn execution_enabled(&self) -> bool {
        self.execution.is_some()
    }

    /// mutating tool 之前的 checkpoint（每 turn/目录最多一次）。
    pub fn checkpoint_before_tool(&self, workdir: &Path, turn_id: &str, tool_name: &str) {
        let Some(manager) = &self.checkpoints else {
            return;
        };
        match crate::checkpoint::ensure_checkpoint_before_tool(manager, workdir, turn_id, tool_name)
        {
            Ok(_) => {}
            Err(error) => self.push_diagnostic(format!("checkpoint failed: {error}")),
        }
    }

    /// 工具成功后把写入的文件记入 per-workdir ledger。
    pub fn record_agent_write(&self, workdir: &Path, path: &Path) {
        let Some(manager) = &self.checkpoints else {
            return;
        };
        let ledger = match manager.ledger(workdir) {
            Ok(ledger) => ledger,
            Err(error) => {
                self.push_diagnostic(format!("agent-write ledger unavailable: {error}"));
                return;
            }
        };
        let mut ledger = ledger;
        if let Err(error) = ledger.record_agent_write_file(path) {
            self.push_diagnostic(format!("agent-write ledger rejected {path:?}: {error}"));
            return;
        }
        if let Err(error) = ledger.save() {
            self.push_diagnostic(format!("agent-write ledger save failed: {error}"));
        }
    }

    /// 采集一条运行时执行记录（验收证据）。没启用时返回 `None`。
    pub fn record_execution(
        &self,
        tool: &str,
        arguments: JsonValue,
        output: String,
        status: ReceiptStatus,
    ) -> Option<ExecutionRecord> {
        if !self.acceptance {
            return None;
        }
        let seq = self.next_record_seq.fetch_add(1, Ordering::SeqCst);
        let record = ExecutionRecord::record(tool, arguments, output, status, seq);
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(record.clone());
        Some(record)
    }

    /// 当前采集到的全部执行记录（按完成顺序）。
    pub fn execution_records(&self) -> Vec<ExecutionRecord> {
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 用已采集的记录判定 acceptance criteria。
    pub fn evaluate(&self, criteria: &[String], workdir: &Path) -> AcceptanceReport {
        let records = self.execution_records();
        evaluate_acceptance(criteria, &AcceptanceContext::new(workdir, &records))
    }

    /// checkpoint manager（用于 restore / prune）。
    pub fn checkpoint_manager(&self) -> Option<&CheckpointManager> {
        self.checkpoints.as_ref()
    }

    /// 后台任务管理器。
    pub fn background_manager(&self) -> Option<&Arc<BackgroundTaskManager>> {
        self.background.as_ref()
    }
}

impl Drop for ContinuityRuntime {
    fn drop(&mut self) {
        // `shutdown` 在 `&self` 上取锁；这里直接取内部字段，避免递归调用。
        let mut lease = self
            .execution_lease
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(lease) = lease.as_mut() {
            let _ = lease.clean_shutdown();
        }
    }
}

/// 进程存活检查。
#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    // `pid_t` 是 i32：直接 `as` 会让超出范围的 pid 回绕成负数，而
    // `kill(-1, 0)` 探测的是“所有可见进程”，会把崩溃遗留的 claim 永久当成活的。
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // 信号 0 只做权限/存在性检查，不发送信号。
    (unsafe { libc::kill(pid, 0) == 0 })
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// 非 unix 平台没有可靠的存活探测，与 `background` / `isolation` 一致按“不存活”
/// 处理；否则崩溃遗留的 claim 会让会话永远无法接管。
#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    false
}

/// 一次工具执行是否应记录 agent 写入的路径。
pub fn written_path_from_args(tool_name: &str, args: &JsonValue) -> Option<String> {
    written_paths_from_args(tool_name, args).into_iter().next()
}

/// 一次工具执行实际写入的全部路径（顺序稳定、去重）。
///
/// - `write`/`edit`/`multi_edit` 等使用显式 `path`/`file_path` 参数；
/// - `apply_patch` 的 schema 只有 `patch` 文本，因此解析
///   `*** Add File:` / `*** Update File:` / `*** Delete File:` / `*** Move to:`
///   头。旧版 patch（无 `*** Begin Patch`）同样按行首匹配。
pub fn written_paths_from_args(tool_name: &str, args: &JsonValue) -> Vec<String> {
    let tool = tool_name.to_ascii_lowercase();
    if tool == "apply_patch" {
        let Some(patch) = args.get("patch").and_then(JsonValue::as_str) else {
            return Vec::new();
        };
        let mut paths = Vec::new();
        for line in patch.lines() {
            let trimmed = line.trim_start();
            let path = [
                "*** Add File:",
                "*** Update File:",
                "*** Delete File:",
                "*** Move to:",
            ]
            .iter()
            .find_map(|prefix| trimmed.strip_prefix(prefix));
            let Some(path) = path else {
                continue;
            };
            let path = path.trim();
            if !path.is_empty() && !paths.iter().any(|existing| existing == path) {
                paths.push(path.to_string());
            }
        }
        return paths;
    }
    if !matches!(
        tool.as_str(),
        "write" | "edit" | "multi_edit" | "write_file" | "create_file"
    ) {
        return Vec::new();
    }
    args.get("path")
        .or_else(|| args.get("file_path"))
        .or_else(|| args.get("filePath"))
        .and_then(JsonValue::as_str)
        .map(|path| vec![path.to_string()])
        .unwrap_or_default()
}

/// 工具结果的完整文本（验收/ledger 使用，UI 截断版本不算证据）。
pub fn tool_result_text(content: &[kiana_ai::types::UserContentBlock]) -> String {
    let mut out = String::new();
    for block in content {
        if let kiana_ai::types::UserContentBlock::Text(text) = block {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&text.text);
        }
    }
    out
}

/// 把工具结果状态映射为 receipt 状态。
pub fn receipt_status(is_error: bool) -> ReceiptStatus {
    if is_error {
        ReceiptStatus::Failed
    } else {
        ReceiptStatus::Succeeded
    }
}

/// 执行状态映射（供生命周期收尾使用）。
pub fn execution_status_for_result(result: &Result<(), String>, aborted: bool) -> ExecutionStatus {
    if aborted {
        ExecutionStatus::Cancelled
    } else if result.is_ok() {
        ExecutionStatus::Completed
    } else {
        ExecutionStatus::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_are_all_on_by_default_and_directories_are_lazy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let options = ContinuityOptions::new(temp.path());
        assert!(options.execution && options.checkpoints && options.background);
        assert!(options.acceptance);
        assert!(!options.execution_dir().exists());
        let runtime = ContinuityRuntime::new(options);
        assert!(!temp.path().join("executions").exists());
        assert!(!temp.path().join("background").exists());
        assert!(runtime.checkpoint_manager().is_some());
        assert!(runtime.background_manager().is_some());
    }

    /// 生产 `ContinuityRuntime` 的后台 manager 必须写 durable journal：任务还没结束时，
    /// 另一个 journal 实例（模拟新进程）就能从 state dir 读回 Running 记录。
    #[tokio::test]
    async fn background_manager_persists_records_in_the_durable_journal() {
        use crate::background::journal::{BackgroundTaskJournal, BackgroundTaskStatus};
        use crate::background::{BackgroundTaskSpec, TaskOutcome};

        let temp = tempfile::tempdir().expect("tempdir");
        let options = ContinuityOptions {
            execution: false,
            checkpoints: false,
            ..ContinuityOptions::new(temp.path())
        };
        let runtime = ContinuityRuntime::new(options.clone());
        let manager = runtime.background_manager().expect("background manager");
        // 让任务停在 Running，避免和读取方并发提交。
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        let handle = manager
            .start(
                BackgroundTaskSpec::new("session-durable", "turn-1", "bash"),
                move |_context| async move {
                    let _ = wait.await;
                    TaskOutcome::completed("done")
                },
            )
            .await
            .expect("start");

        // 另一个进程/实例只读 durable 存储：记录已经可见（start 返回前已提交）。
        let journal = DurableTaskJournal::new(options.background_dir());
        let record = journal
            .load(&handle.task_id)
            .await
            .expect("load")
            .expect("durable record");
        assert_eq!(record.status, BackgroundTaskStatus::Running);
        assert_eq!(record.parent_session_id, "session-durable");
        assert!(handle.output_path.starts_with(options.background_dir()));

        // 放行任务，让测试结束前不再遗留 pending 的会话写入。
        let _ = release.send(());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn shutdown_releases_durable_directory_leases() {
        use crate::subagent::WorkStore as _;

        let temp = tempfile::tempdir().expect("tempdir");
        let options = ContinuityOptions {
            execution: false,
            checkpoints: false,
            background: false,
            ..ContinuityOptions::new(temp.path())
        };
        let runtime = ContinuityRuntime::new(options.clone());
        let store = runtime.work_store().expect("work store").clone();
        // 打开 per-session 目录，取得 `owner.json` + `flock` 租约。
        store
            .allocate_work_id("session-lease")
            .await
            .expect("open work store");

        let owner = std::fs::read_dir(options.state_dir.join("work"))
            .expect("work root")
            .flatten()
            .map(|entry| entry.path().join("owner.json"))
            .find(|path| path.exists())
            .expect("work store must hold a directory lease");

        runtime.shutdown();
        assert!(
            !owner.exists(),
            "clean shutdown must release the durable directory lease"
        );
    }

    #[test]
    fn execution_claim_is_released_as_clean_shutdown_and_blocks_a_second_live_owner() {
        let temp = tempfile::tempdir().expect("tempdir");
        let options = ContinuityOptions {
            checkpoints: false,
            background: false,
            ..ContinuityOptions::new(temp.path())
        };
        let first = ContinuityRuntime::new(options.clone());
        first.begin_session("session-1").expect("first claim");
        // 同一进程里的第二个 runtime 看到活 claim，必须 Busy。
        let second = ContinuityRuntime::new(options.clone());
        let error = second.begin_session("session-1").expect_err("busy");
        assert!(error.contains("already being executed"), "{error}");
        first.shutdown();
        // clean shutdown 保留 claim 但不消耗 attempt；下一次 begin_session 走 recover 接管
        // （即使同一个进程仍存活）。
        let third = ContinuityRuntime::new(options);
        third
            .begin_session("session-1")
            .expect("clean shutdown claim is adoptable");
        third.shutdown();
    }

    #[test]
    fn acceptance_records_are_runtime_stamped_and_evaluable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime = ContinuityRuntime::new(ContinuityOptions {
            execution: false,
            checkpoints: false,
            background: false,
            ..ContinuityOptions::new(temp.path())
        });
        std::fs::write(temp.path().join("out.json"), "{\"ok\":true}").expect("write");
        let record = runtime.record_execution(
            "write",
            serde_json::json!({"path": "out.json"}),
            "done".to_string(),
            ReceiptStatus::Succeeded,
        );
        let record = record.expect("record");
        assert!(record.verify());
        assert_eq!(record.receipt.seq, 1);
        let report = runtime.evaluate(
            &[
                "file:out.json exists".to_string(),
                "file:out.json json-valid".to_string(),
            ],
            temp.path(),
        );
        assert!(report.all_hold(), "{report:?}");
    }

    #[test]
    fn written_paths_are_only_extracted_for_file_tools() {
        assert_eq!(
            written_path_from_args("write", &serde_json::json!({"path": "a.txt"})),
            Some("a.txt".to_string())
        );
        assert_eq!(
            written_path_from_args("bash", &serde_json::json!({"path": "a.txt"})),
            None
        );
    }

    #[test]
    fn apply_patch_headers_are_all_extracted_for_the_agent_write_ledger() {
        let patch = "*** Begin Patch\n*** Add File: src/new.rs\n+fn main() {}\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** Delete File: src/old.rs\n*** Update File: src/moved.rs\n*** Move to: src/renamed.rs\n*** End Patch\n";
        let paths = written_paths_from_args("apply_patch", &serde_json::json!({"patch": patch}));
        assert_eq!(
            paths,
            vec![
                "src/new.rs".to_string(),
                "src/lib.rs".to_string(),
                "src/old.rs".to_string(),
                "src/moved.rs".to_string(),
                "src/renamed.rs".to_string(),
            ]
        );
    }

    #[test]
    fn explicit_settings_disable_is_distinguishable_from_absence() {
        let disabled =
            |value: JsonValue| ContinuityOptions::settings_disable_continuity(Some(&value));
        assert!(disabled(serde_json::json!(false)));
        assert!(disabled(serde_json::json!({"enabled": false})));
        assert!(!disabled(serde_json::json!(true)));
        assert!(!disabled(serde_json::json!({"enabled": true})));
        assert!(!disabled(serde_json::json!({})));
        assert!(!ContinuityOptions::settings_disable_continuity(None));
    }

    #[test]
    fn continuity_settings_enable_the_runtime_without_environment_variables() {
        let temp = tempfile::tempdir().expect("tempdir");
        let options = ContinuityOptions::from_settings(Some(&serde_json::json!({
            "enabled": true,
            "dir": temp.path(),
            "execution": false,
            "checkpoints": false,
            "background": true,
            "acceptance": false,
        })))
        .expect("enabled");
        assert_eq!(options.state_dir, temp.path());
        assert!(!options.execution);
        assert!(!options.checkpoints);
        assert!(options.background);
        assert!(options.work);
        assert!(!options.acceptance);
        assert!(ContinuityOptions::from_settings(Some(&serde_json::json!(false))).is_none());
        assert!(
            ContinuityOptions::from_settings(Some(&serde_json::json!({"enabled": false})))
                .is_none()
        );
        assert!(ContinuityOptions::from_settings(None).is_none());
    }

    /// 测试期间临时设置环境变量；drop 时恢复原值。
    struct EnvVarGuard {
        name: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(name);
            // SAFETY: 只修改本用例独占的 `KIANA_CONTINUITY_*` 变量，并在 drop 时恢复。
            unsafe { std::env::set_var(name, value) };
            Self { name, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: 同上；恢复进入用例前的值。
            match &self.previous {
                Some(value) => unsafe { std::env::set_var(self.name, value) },
                None => unsafe { std::env::remove_var(self.name) },
            }
        }
    }

    /// 回归（第六轮评审 finding #8）：settings 提供 `continuity` 时，
    /// `KIANA_CONTINUITY_WORK=0` 仍必须能单独关闭 Work。
    #[test]
    fn settings_options_still_honor_environment_kill_switches() {
        let temp = tempfile::tempdir().expect("tempdir");
        let options = ContinuityOptions::from_settings(Some(&serde_json::json!({
            "enabled": true,
            "dir": temp.path(),
            "work": true,
        })))
        .expect("enabled");
        assert!(options.work);

        let guard = EnvVarGuard::set("KIANA_CONTINUITY_WORK", "0");
        let overridden = options.clone().with_env_overrides();
        assert!(
            !overridden.work,
            "the environment kill-switch must not be ignored when settings exist"
        );
        assert!(
            overridden.background,
            "unrelated capabilities keep their settings value"
        );
        drop(guard);

        assert!(options.with_env_overrides().work);

        // settings 显式关闭的能力不能被环境变量重新打开（环境只是 kill-switch）。
        let disabled = ContinuityOptions::from_settings(Some(&serde_json::json!({
            "enabled": true,
            "dir": temp.path(),
            "work": false,
        })))
        .expect("enabled");
        assert!(!disabled.work);
        let guard = EnvVarGuard::set("KIANA_CONTINUITY_WORK", "1");
        assert!(
            !disabled.with_env_overrides().work,
            "the environment must not re-enable a settings-disabled capability"
        );
        drop(guard);
    }

    #[test]
    fn work_store_is_created_only_when_work_is_enabled() {
        let temp = tempfile::tempdir().expect("tempdir");
        let enabled = ContinuityRuntime::new(ContinuityOptions {
            execution: false,
            checkpoints: false,
            background: false,
            work: true,
            acceptance: false,
            ..ContinuityOptions::new(temp.path())
        });
        assert!(enabled.work_store().is_some());

        let disabled = ContinuityRuntime::new(ContinuityOptions {
            execution: false,
            checkpoints: false,
            background: false,
            work: false,
            acceptance: false,
            ..ContinuityOptions::new(temp.path())
        });
        assert!(disabled.work_store().is_none());
    }
}
