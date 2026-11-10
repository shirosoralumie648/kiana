//! 后台任务持久化契约与内存实现。
//!
//! [`BackgroundTaskJournal`] 是 kiana-session 与持久化层之间的**窄接口**：它不是
//! 队列，只负责保存任务记录、在终态转换时做一次 compare-and-set、持久化尝试次数，
//! 以及记录通知是否已发出。生产接线应使用 `kiana-durable` 的 inbox/scheduler
//! 实现这个 trait（见 `docs/history/porting/handoffs/phase-11b.md`），不另造一套队列。
//!
//! 终态转换的原子性由 [`BackgroundTaskJournal::finish`] 负责：只有把 Running 改成
//! terminal 的那一次调用返回 `Some(record)`；并发完成/取消的失败者得到 `None`，
//! 因此 terminal 通知最多发布一次（按 `(task_id, terminal_seq)` 去重）。
//!
//! 参考设计：
//! - `reference_project/ClaudeCode/src/tasks/LocalShellTask/LocalShellTask.tsx` 的
//!   `notified` 原子 check-and-set（取消后抑制迟到通知）；
//! - `reference_project/ClaudeCode/src/tasks/stopTask.ts` 的 “stop 先标记 notified，
//!   后到的完成不再发通知”；
//! - `reference_project/qwen-code/packages/cli/src/serve/prompt-terminal-ledger.ts`
//!   的 terminal ledger 去重思路。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// 后台任务状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTaskStatus {
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
    /// 进程崩溃后被 sweep 终止（无法恢复）。
    Interrupted,
}

impl BackgroundTaskStatus {
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
            Self::Interrupted => "interrupted",
        }
    }
}

/// 任务自身的 token 用量。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskUsage {
    /// 输入 token。
    pub input_tokens: u64,
    /// 输出 token。
    pub output_tokens: u64,
    /// 总 token。
    pub total_tokens: u64,
}

impl TaskUsage {
    /// 累加。
    pub fn add(&mut self, other: TaskUsage) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
    }
}

/// 终态详情。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTaskTerminal {
    /// 终态。
    pub status: BackgroundTaskStatus,
    /// 原因（失败/取消/超限）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 退出码。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// 任务自身 usage。
    #[serde(default)]
    pub usage: TaskUsage,
    /// 终态验收报告（criteria 为空时是显式 `Unverified`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<crate::acceptance::AcceptanceReport>,
    /// 结束时间（Unix 毫秒）。
    pub finished_at_ms: u64,
}

/// 持久化的任务记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTaskRecord {
    /// 格式版本。
    pub version: u32,
    /// task id。
    pub task_id: String,
    /// 父会话 id。
    pub parent_session_id: String,
    /// 父 turn id。
    pub parent_turn_id: String,
    /// 产生任务的服务端工具名。
    pub tool: String,
    /// 人类可读描述。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 状态。
    pub status: BackgroundTaskStatus,
    /// 终态序号：运行中为 0，进入终态后为 1。
    pub terminal_seq: u64,
    /// 终态详情。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<BackgroundTaskTerminal>,
    /// 输出文件路径。
    pub output_path: String,
    /// 输出文件字节数。
    pub output_bytes: u64,
    /// 输出是否被硬上限截断。
    pub output_truncated: bool,
    /// 是否已经成功发出 terminal 通知。
    pub notified: bool,
    /// 已尝试次数（含恢复）。
    pub attempts: u32,
    /// 创建时间。
    pub created_at_ms: u64,
    /// 更新时间。
    pub updated_at_ms: u64,
    /// 任务进程 id（若已知）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// 该 attempt 是否由 Work 账本驱动。
    ///
    /// Work 的权威终态在 `kiana.work_records`，后台记录只是这次 attempt 的输出/
    /// 通知载体。sweep 不能对这类记录做通用终态判定：Work 已提交 Success、但
    /// `finish_task` 尚未执行时，通用 sweep 会把它误报成 `process_lost`，而
    /// `finish_task` 的终态 CAS 一旦落定就无法再被 Work 对账修正。这类记录只由
    /// 所属会话的 `recover_work` 对账，sweep 一律跳过未终态的记录。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub work_backed: bool,
    /// 关联的 Work id（只对 `workBacked` 记录有意义）。
    ///
    /// 崩溃可能发生在 journal Running 写入之后、`WorkStore::begin` 之前；没有
    /// 这个映射，启动对账只能看到“永远 Running 的 workBacked attempt”。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_id: Option<i64>,
    /// 关联的 Work run id（用于区分同 id 的后续运行）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

impl BackgroundTaskRecord {
    /// 是否终态。
    pub fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }
}

/// terminal 通知。
///
/// 消费方用 `(task_id, terminal_seq)` 去重；管理器保证同一次执行不会在内存里重复
/// 发送，崩溃窗口内可能重发同一 key（at-least-once + 幂等 key）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTaskNotification {
    /// task id。
    pub task_id: String,
    /// 终态序号（当前恒为 1）。
    pub terminal_seq: u64,
    /// 父会话 id。
    pub parent_session_id: String,
    /// 父 turn id。
    pub parent_turn_id: String,
    /// 工具名。
    pub tool: String,
    /// 终态。
    pub status: BackgroundTaskStatus,
    /// 原因。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 退出码。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// 输出文件。
    pub output_path: String,
    /// 输出字节数。
    pub output_bytes: u64,
    /// 是否截断。
    pub output_truncated: bool,
    /// 任务 usage（父会话汇总见 [`super::BackgroundTaskManager::usage_for_parent`]）。
    #[serde(default)]
    pub usage: TaskUsage,
    /// 终态验收报告。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<crate::acceptance::AcceptanceReport>,
}

/// 任务最终产出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskOutcome {
    /// 终态（不能是 Running）。
    pub status: BackgroundTaskStatus,
    /// 原因。
    pub reason: Option<String>,
    /// 退出码。
    pub exit_code: Option<i32>,
    /// 追加到输出文件的最终文本。
    pub output: String,
    /// 任务 usage。
    pub usage: TaskUsage,
    /// 终态验收报告。
    pub acceptance: Option<crate::acceptance::AcceptanceReport>,
}

impl TaskOutcome {
    /// 正常完成。
    pub fn completed(output: impl Into<String>) -> Self {
        Self {
            status: BackgroundTaskStatus::Completed,
            reason: None,
            exit_code: Some(0),
            output: output.into(),
            usage: TaskUsage::default(),
            acceptance: None,
        }
    }

    /// 失败。
    pub fn failed(reason: impl Into<String>) -> Self {
        Self {
            status: BackgroundTaskStatus::Failed,
            reason: Some(reason.into()),
            exit_code: None,
            output: String::new(),
            usage: TaskUsage::default(),
            acceptance: None,
        }
    }

    /// 取消。
    pub fn cancelled() -> Self {
        Self {
            status: BackgroundTaskStatus::Cancelled,
            reason: Some("cancelled".to_string()),
            exit_code: None,
            output: String::new(),
            usage: TaskUsage::default(),
            acceptance: None,
        }
    }

    /// 超时。
    pub fn timed_out(reason: impl Into<String>) -> Self {
        Self {
            status: BackgroundTaskStatus::TimedOut,
            reason: Some(reason.into()),
            exit_code: None,
            output: String::new(),
            usage: TaskUsage::default(),
            acceptance: None,
        }
    }

    /// 附加 usage（builder）。
    pub fn with_usage(mut self, usage: TaskUsage) -> Self {
        self.usage = usage;
        self
    }

    /// 附加验收报告（builder）。
    pub fn with_acceptance(
        mut self,
        acceptance: Option<crate::acceptance::AcceptanceReport>,
    ) -> Self {
        self.acceptance = acceptance;
        self
    }

    /// 附加输出（builder）。
    pub fn with_output(mut self, output: impl Into<String>) -> Self {
        self.output = output.into();
        self
    }
}

/// 后台任务错误。
#[derive(Debug, thiserror::Error)]
pub enum BackgroundTaskError {
    /// IO 错误。
    #[error("background task io error: {0}")]
    Io(#[from] std::io::Error),
    /// 没有 tokio runtime。
    #[error("background tasks require a tokio runtime")]
    NoRuntime,
    /// 任务不存在。
    #[error("background task not found: {0}")]
    TaskNotFound(String),
    /// journal 错误。
    #[error("background task journal error: {0}")]
    Journal(String),
    /// 通知失败。
    #[error("background task notification failed: {0}")]
    Notify(String),
    /// 输出文件被拒绝（符号链接/非普通文件）。
    #[error("background task output rejected at {path}: {message}")]
    OutputRejected {
        /// 路径。
        path: String,
        /// 原因。
        message: String,
    },
    /// 非终态 outcome。
    #[error("background task outcome must be terminal, got {0}")]
    InvalidOutcome(String),
}

/// 持久化 journal（由 kiana-durable 适配实现）。
// `async_trait` 展开的 Future 方法自带 `#[must_use]`，clippy 看不到宏内属性。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait BackgroundTaskJournal: Send + Sync {
    /// 首次保存记录。
    async fn save(&self, record: BackgroundTaskRecord) -> Result<(), BackgroundTaskError>;

    /// 读取记录。
    async fn load(
        &self,
        task_id: &str,
    ) -> Result<Option<BackgroundTaskRecord>, BackgroundTaskError>;

    /// 枚举需要恢复的记录：运行中的任务，以及已终态但通知未发出的任务。
    async fn list_recoverable(&self) -> Result<Vec<BackgroundTaskRecord>, BackgroundTaskError>;

    /// 按父会话列出全部记录（运行中与终态都包含，按 `created_at_ms` 升序）。
    ///
    /// 与 [`Self::list_recoverable`] 的区别：这里不做“未通知”过滤，也不扫描其它会话；
    /// 供 `/tasks` 面板与 headless 文本输出使用。`parent_session_id` 不匹配的记录必须
    /// 被过滤掉，避免跨会话泄露。
    async fn list_for_session(
        &self,
        parent_session_id: &str,
    ) -> Result<Vec<BackgroundTaskRecord>, BackgroundTaskError>;

    /// 原子地把 Running 转成 terminal。
    ///
    /// 返回 `Some(record)` 表示本次调用赢下转换（调用方负责发通知）；`None` 表示
    /// 任务已经是终态，调用方必须抑制迟到通知。
    async fn finish(
        &self,
        task_id: &str,
        terminal: BackgroundTaskTerminal,
        output_bytes: u64,
        output_truncated: bool,
    ) -> Result<Option<BackgroundTaskRecord>, BackgroundTaskError>;

    /// 运行中任务的 attempt +1（durably）。
    async fn record_attempt(
        &self,
        task_id: &str,
    ) -> Result<Option<BackgroundTaskRecord>, BackgroundTaskError>;

    /// 标记通知已成功发出。
    async fn mark_notified(
        &self,
        task_id: &str,
        terminal_seq: u64,
    ) -> Result<(), BackgroundTaskError>;

    /// 启动 sweep 时重放“已投递但父会话尚未消费”的 terminal 通知。
    ///
    /// 默认实现无操作（内存 journal 没有跨进程投递通道）；durable 实现从持久化的
    /// `deliveries` 恢复，避免进程在投递与消费之间退出时丢失终态事件。`session_id`
    /// 限定只重放该父会话的投递，`None` 表示所有会话。
    async fn replay_pending_deliveries(
        &self,
        session_id: Option<&str>,
    ) -> Result<(), BackgroundTaskError> {
        let _ = session_id;
        Ok(())
    }
}

/// terminal 通知出口（生产实现应写入 kiana-durable 的 inbox）。
// `async_trait` 展开的 Future 方法自带 `#[must_use]`，clippy 看不到宏内属性。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait BackgroundTaskNotifier: Send + Sync {
    /// 发送通知。消费方按 `(task_id, terminal_seq)` 去重。
    async fn notify(
        &self,
        notification: &BackgroundTaskNotification,
    ) -> Result<(), BackgroundTaskError>;
}

/// sweep 时的恢复器：返回 `Some(outcome)` 表示可以恢复并直接完成，`None` 表示放弃。
// `async_trait` 展开的 Future 方法自带 `#[must_use]`，clippy 看不到宏内属性。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait BackgroundTaskResumer: Send + Sync {
    /// 尝试恢复一个运行中的任务。
    async fn resume(
        &self,
        record: &BackgroundTaskRecord,
    ) -> Result<Option<TaskOutcome>, BackgroundTaskError>;
}

/// 记录通知的测试/嵌入实现。
#[derive(Debug, Default)]
pub struct RecordingNotifier {
    notifications: Mutex<Vec<BackgroundTaskNotification>>,
}

impl RecordingNotifier {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }

    /// 已记录的通知（按发送顺序）。
    pub fn notifications(&self) -> Vec<BackgroundTaskNotification> {
        self.notifications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 通知数量。
    pub fn len(&self) -> usize {
        self.notifications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[async_trait]
impl BackgroundTaskNotifier for RecordingNotifier {
    async fn notify(
        &self,
        notification: &BackgroundTaskNotification,
    ) -> Result<(), BackgroundTaskError> {
        self.notifications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(notification.clone());
        Ok(())
    }
}

/// 内存 journal（测试/嵌入参考实现）。
#[derive(Debug, Default)]
pub struct MemoryTaskJournal {
    records: Mutex<HashMap<String, BackgroundTaskRecord>>,
}

impl MemoryTaskJournal {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前全部记录。
    pub fn all(&self) -> Vec<BackgroundTaskRecord> {
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .cloned()
            .collect()
    }
}

#[async_trait]
impl BackgroundTaskJournal for MemoryTaskJournal {
    async fn save(&self, record: BackgroundTaskRecord) -> Result<(), BackgroundTaskError> {
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(record.task_id.clone(), record);
        Ok(())
    }

    async fn load(
        &self,
        task_id: &str,
    ) -> Result<Option<BackgroundTaskRecord>, BackgroundTaskError> {
        Ok(self
            .records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(task_id)
            .cloned())
    }

    async fn list_recoverable(&self) -> Result<Vec<BackgroundTaskRecord>, BackgroundTaskError> {
        let records = self
            .records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Ok(records
            .values()
            .filter(|record| !record.is_terminal() || !record.notified)
            .cloned()
            .collect())
    }

    async fn list_for_session(
        &self,
        parent_session_id: &str,
    ) -> Result<Vec<BackgroundTaskRecord>, BackgroundTaskError> {
        let records = self
            .records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut records: Vec<BackgroundTaskRecord> = records
            .values()
            .filter(|record| record.parent_session_id == parent_session_id)
            .cloned()
            .collect();
        records.sort_by(|left, right| {
            left.created_at_ms
                .cmp(&right.created_at_ms)
                .then_with(|| left.task_id.cmp(&right.task_id))
        });
        Ok(records)
    }

    async fn finish(
        &self,
        task_id: &str,
        terminal: BackgroundTaskTerminal,
        output_bytes: u64,
        output_truncated: bool,
    ) -> Result<Option<BackgroundTaskRecord>, BackgroundTaskError> {
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = records.get_mut(task_id) else {
            return Ok(None);
        };
        if record.is_terminal() {
            return Ok(None);
        }
        record.status = terminal.status;
        record.terminal_seq = record.terminal_seq.saturating_add(1);
        record.terminal = Some(terminal);
        record.output_bytes = output_bytes;
        record.output_truncated = output_truncated;
        record.notified = false;
        record.updated_at_ms = now_millis();
        Ok(Some(record.clone()))
    }

    async fn record_attempt(
        &self,
        task_id: &str,
    ) -> Result<Option<BackgroundTaskRecord>, BackgroundTaskError> {
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = records.get_mut(task_id) else {
            return Ok(None);
        };
        if record.is_terminal() {
            return Ok(None);
        }
        record.attempts = record.attempts.saturating_add(1);
        record.updated_at_ms = now_millis();
        Ok(Some(record.clone()))
    }

    async fn mark_notified(
        &self,
        task_id: &str,
        terminal_seq: u64,
    ) -> Result<(), BackgroundTaskError> {
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(record) = records.get_mut(task_id)
            && record.terminal_seq == terminal_seq
        {
            record.notified = true;
            record.updated_at_ms = now_millis();
        }
        Ok(())
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
