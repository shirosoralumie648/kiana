//! 后台任务运行时：`background=true` 立即返回 task id；任务不随父 turn 结束而终止。
//!
//! # 生命周期
//!
//! 1. [`BackgroundTaskManager::start`] 打开输出文件（`O_NOFOLLOW` + 硬上限）、把
//!    `Running` 记录写入 journal，然后 spawn 任务并**立即**返回
//!    [`BackgroundTaskHandle`]；
//! 2. 任务通过 [`BackgroundTaskContext`] 增量写输出、检查取消 token；
//! 3. 任务结束（或 [`BackgroundTaskManager::cancel`]）时，[`BackgroundTaskJournal::finish`]
//!    原子地把 `Running` 转成 terminal；只有赢家发布一次通知，输家（迟到完成）被抑制；
//! 4. 父 turn 只持有 handle，不拥有任务；turn 结束/被 drop 不会取消任务；
//! 5. 进程崩溃后 [`BackgroundTaskManager::sweep`] 枚举 journal 里的运行中记录：
//!    计数 attempt，有 resumer 就恢复，否则 terminalize；已终态但通知未发出的记录
//!    会用同一 `(task_id, terminal_seq)` 重发。
//!
//! # 与 kiana-durable 的关系
//!
//! 本模块**不实现队列**。持久化与通知分别通过
//! [`BackgroundTaskJournal`] / [`BackgroundTaskNotifier`] 注入，生产实现应落在
//! `kiana-durable` 的 inbox/scheduler 上。内存实现只用于测试与嵌入参考。
//!
//! # 参考设计
//!
//! - `reference_project/ClaudeCode/src/utils/task/diskOutput.ts`：O_NOFOLLOW、输出硬上限；
//! - `reference_project/ClaudeCode/src/tasks/LocalShellTask/LocalShellTask.tsx`：notified
//!   check-and-set、父 turn 结束不影响后台任务；
//! - `reference_project/ClaudeCode/src/tasks/stopTask.ts`：取消会先占位 notified，
//!   抑制迟到的 “completed” 通知；
//! - `reference_project/codex/codex-rs/core/src/tasks/mod.rs`：后台 terminal 的终态语义。

pub mod durable;
pub mod journal;
pub mod output;
pub mod tool;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub use durable::DurableTaskJournal;
pub use journal::{
    BackgroundTaskError, BackgroundTaskJournal, BackgroundTaskNotification, BackgroundTaskNotifier,
    BackgroundTaskRecord, BackgroundTaskResumer, BackgroundTaskStatus, BackgroundTaskTerminal,
    MemoryTaskJournal, RecordingNotifier, TaskOutcome, TaskUsage,
};
pub use output::{AppendOutcome, BackgroundTaskOutput, DEFAULT_MAX_OUTPUT_BYTES};
pub use tool::wrap_bash_background;

/// 默认恢复尝试上限。
pub const DEFAULT_MAX_RESUME_ATTEMPTS: u32 = 3;

/// 取消时留给任务执行协作式清理的时间；超时后 abort。
const CANCEL_CLEANUP_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

/// 后台任务配置。
pub struct BackgroundTaskOptions {
    /// 输出目录（每个 task 一个 `<task_id>.output`）。
    pub output_dir: PathBuf,
    /// 输出文件硬上限（字节）。
    pub max_output_bytes: u64,
    /// sweep 恢复尝试上限；超过后 terminalize。
    pub max_resume_attempts: u32,
    /// 持久化 journal（kiana-durable 适配）。
    pub journal: Arc<dyn BackgroundTaskJournal>,
    /// terminal 通知出口（kiana-durable inbox 适配）。
    pub notifier: Arc<dyn BackgroundTaskNotifier>,
    /// 可选的恢复器；没有时运行中记录在 sweep 时直接 terminalize。
    pub resumer: Option<Arc<dyn BackgroundTaskResumer>>,
}

impl BackgroundTaskOptions {
    /// 新建配置（默认 8 MiB 输出、3 次恢复尝试、无 resumer）。
    pub fn new(
        output_dir: impl Into<PathBuf>,
        journal: Arc<dyn BackgroundTaskJournal>,
        notifier: Arc<dyn BackgroundTaskNotifier>,
    ) -> Self {
        Self {
            output_dir: output_dir.into(),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            max_resume_attempts: DEFAULT_MAX_RESUME_ATTEMPTS,
            journal,
            notifier,
            resumer: None,
        }
    }

    /// 设置输出上限。
    pub fn with_max_output_bytes(mut self, max_output_bytes: u64) -> Self {
        self.max_output_bytes = max_output_bytes;
        self
    }

    /// 设置恢复尝试上限。
    pub fn with_max_resume_attempts(mut self, max_resume_attempts: u32) -> Self {
        self.max_resume_attempts = max_resume_attempts.max(1);
        self
    }

    /// 设置恢复器。
    pub fn with_resumer(mut self, resumer: Arc<dyn BackgroundTaskResumer>) -> Self {
        self.resumer = Some(resumer);
        self
    }
}

/// 任务创建参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundTaskSpec {
    /// 父会话 id。
    pub parent_session_id: String,
    /// 父 turn id。
    pub parent_turn_id: String,
    /// 产生任务的服务端工具名。
    pub tool: String,
    /// 人类可读描述。
    pub description: Option<String>,
    /// 任务终态要判定的 acceptance criteria；空列表在完成时仍会生成显式
    /// `Unverified` 报告（绝不静默通过）。
    pub acceptance_criteria: Vec<String>,
    /// acceptance 文件检查的根目录。
    pub workdir: Option<PathBuf>,
    /// 该 attempt 是否由 Work 账本驱动（见 [`BackgroundTaskRecord::work_backed`]）。
    pub work_backed: bool,
    /// 关联的 Work id（只对 `workBacked` 记录有意义）。
    pub work_id: Option<i64>,
    /// 关联的 Work run id。
    pub run_id: Option<String>,
}

impl BackgroundTaskSpec {
    /// 新建。
    pub fn new(
        parent_session_id: impl Into<String>,
        parent_turn_id: impl Into<String>,
        tool: impl Into<String>,
    ) -> Self {
        Self {
            parent_session_id: parent_session_id.into(),
            parent_turn_id: parent_turn_id.into(),
            tool: tool.into(),
            description: None,
            acceptance_criteria: Vec::new(),
            workdir: None,
            work_backed: false,
            work_id: None,
            run_id: None,
        }
    }

    /// 标记该任务由 Work 账本驱动：sweep 不替它宣告终态。
    pub fn with_work_backed(mut self, work_backed: bool) -> Self {
        self.work_backed = work_backed;
        self
    }

    /// 记录 attempt 关联的 Work 身份：崩溃发生在 `begin` 之前时，启动对账用它
    /// 把 journal 记录与 Work 账本配对。
    pub fn with_work_identity(mut self, work_id: i64, run_id: impl Into<String>) -> Self {
        self.work_id = Some(work_id);
        self.run_id = Some(run_id.into());
        self
    }

    /// 设置描述。
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// 设置终态验收 criteria 与检查目录。
    pub fn with_acceptance(mut self, criteria: Vec<String>, workdir: impl Into<PathBuf>) -> Self {
        self.acceptance_criteria = criteria;
        self.workdir = Some(workdir.into());
        self
    }
}

/// `start` 的返回值：立即拿到的 task id 与输出文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundTaskHandle {
    /// task id。
    pub task_id: String,
    /// 输出文件路径。
    pub output_path: PathBuf,
}

/// 输出句柄（可克隆，供任务内部增量写）。
#[derive(Debug, Clone)]
pub struct TaskOutputHandle {
    inner: Arc<Mutex<BackgroundTaskOutput>>,
}

impl TaskOutputHandle {
    /// 测试用：直接包装一个已打开的输出文件。
    #[cfg(test)]
    pub(crate) fn from_output(output: BackgroundTaskOutput) -> Self {
        Self {
            inner: Arc::new(Mutex::new(output)),
        }
    }

    /// 追加文本。
    pub fn append(&self, text: &str) -> Result<AppendOutcome, BackgroundTaskError> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .append(text)
            .map_err(BackgroundTaskError::Io)
    }

    /// 输出路径。
    pub fn path(&self) -> PathBuf {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .path()
            .to_path_buf()
    }

    /// 已写字节数。
    pub fn written(&self) -> u64 {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .written()
    }

    /// 是否截断。
    pub fn is_truncated(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_truncated()
    }
}

/// 任务上下文。
#[derive(Debug, Clone)]
pub struct BackgroundTaskContext {
    /// task id。
    pub task_id: String,
    /// 取消 token（cancel 会触发）。
    pub cancel: CancellationToken,
    /// 输出句柄。
    pub output: TaskOutputHandle,
    /// 终态 acceptance criteria。
    pub acceptance_criteria: Vec<String>,
    /// acceptance 文件检查的根目录。
    pub workdir: PathBuf,
}

impl BackgroundTaskContext {
    /// 是否已取消。
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

/// 完成报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionReport {
    /// 是否赢下 Running -> terminal 的竞争。
    pub won_race: bool,
    /// 终态。
    pub terminal: Option<BackgroundTaskTerminal>,
    /// 本次发出的通知。
    pub notification: Option<BackgroundTaskNotification>,
}

/// sweep 报告。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// 恢复并完成的任务。
    pub resumed: Vec<String>,
    /// 被 terminalize 的任务。
    pub terminalized: Vec<String>,
    /// 重发通知的任务。
    pub renotified: Vec<String>,
    /// 记录里的 pid 仍存活、被跳过的任务（属于另一个进程，或本进程里另一个会话
    /// 仍在运行的任务）。
    pub skipped_live: Vec<String>,
    /// 由 Work 账本驱动的未终态记录：通用 sweep 不碰，只由所属会话的
    /// `recover_work` 对账。
    pub skipped_work_backed: Vec<String>,
}

/// 进程是否仍存活（`kill(pid, 0)` 只做存在性与权限检查，不发送信号）。
#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    // `libc::pid_t` 是 i32；直接 `as` 会让越界 pid 回绕成负数，而 `kill(-n, 0)`
    // 检查的是进程组 n。无法表示的 pid 不能安全探测，按“不存活”处理。
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: 传 0 信号不会实际投递；全部参数都是标量。
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// 非 unix 平台无法廉价验证存活：按“不确定”处理，维持原有 sweep 行为。
#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    false
}

/// 本进程内在跑的后台任务 id。
///
/// durable journal 可以被同一进程里的多个会话共享（`/new`、`/resume` 都会按同一个
/// 状态目录打开新的 `DurableTaskJournal`）。记录里的 pid 只能区分“其它进程”，
/// 无法区分“本进程另一个会话仍然存活的任务”与“本会话里已经死掉的残留”：
/// 没有这份登记表，B 会话的启动 sweep 会把 A 会话正在运行的任务当成进程丢失而
/// 终结它，随后 A 的真实完成因为记录已终态被丢弃（`finish_task` 输家路径）。
fn live_tasks() -> &'static Mutex<HashSet<String>> {
    static LIVE_TASKS: std::sync::OnceLock<Mutex<HashSet<String>>> = std::sync::OnceLock::new();
    LIVE_TASKS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// 记录对应的任务是否仍在本进程/其它进程里运行。
///
/// 同进程的多个会话共享 pid，必须查全局活任务登记表；`kill(pid, 0)` 只用于其它
/// 进程。
pub(crate) fn record_is_live(record: &BackgroundTaskRecord) -> bool {
    match record.pid {
        Some(pid) if pid == std::process::id() => live_task_is_running(&record.task_id),
        Some(pid) => process_is_alive(pid),
        None => false,
    }
}

fn live_task_is_running(task_id: &str) -> bool {
    live_tasks()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(task_id)
}

/// 进程内是否还有后台任务在跑（干净关停前判断能否释放 durable 目录租约）。
pub(crate) fn any_task_is_running() -> bool {
    !live_tasks()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_empty()
}

/// 任务运行期间在全局登记表里的占位；drop（完成、取消、panic）时自动注销。
struct LiveTaskGuard(String);

impl LiveTaskGuard {
    fn register(task_id: &str) -> Self {
        live_tasks()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(task_id.to_string());
        Self(task_id.to_string())
    }
}

impl Drop for LiveTaskGuard {
    fn drop(&mut self) {
        live_tasks()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.0);
    }
}

/// `finish_task` 返回时移除执行槽（含所有早退路径）。
struct SlotRemoval<'a> {
    inner: &'a Inner,
    task_id: String,
}

impl Drop for SlotRemoval<'_> {
    fn drop(&mut self) {
        self.inner
            .tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.task_id);
    }
}

struct TaskSlot {
    cancel: CancellationToken,
    output: Arc<Mutex<BackgroundTaskOutput>>,
    join: Mutex<Option<JoinHandle<()>>>,
    /// 取消时是否直接 abort 任务 future。
    ///
    /// `start`（bash 等 Send 任务）保持原有行为；`start_local` 承载的是会话内的
    /// `!Send` Work，abort 会让 Work 账本停在 Running，因此只取消 token、让生命周期
    /// 自己写出终态。
    abort_on_cancel: bool,
}

/// `start` / `start_local` 共用的预备结果：输出文件、journal Running 记录与上下文
/// 都已经就绪，只差执行器不同。
struct PreparedTask {
    task_id: String,
    output_path: PathBuf,
    slot: Arc<TaskSlot>,
    context: BackgroundTaskContext,
    live_guard: LiveTaskGuard,
}

struct Inner {
    options: BackgroundTaskOptions,
    tasks: Mutex<HashMap<String, Arc<TaskSlot>>>,
    parent_usage: Mutex<HashMap<String, TaskUsage>>,
    task_usage: Mutex<HashMap<String, TaskUsage>>,
    /// 串行化“检查 notified + 发送 + 标记”，保证进程内不会重复发送同一条 terminal。
    notify_lock: tokio::sync::Mutex<()>,
    /// 串行化 finish_task：输出追加必须发生在 CAS 之前且只有一个赢家写，否则输家
    /// 会把输出写进文件、还可能抬高赢家记录的 output_bytes。
    finish_lock: tokio::sync::Mutex<()>,
    /// 最近一次后台完成路径的错误（journal/notify 失败不会让父会话崩溃，但必须可诊断）。
    last_error: Mutex<Option<String>>,
}

/// 后台任务管理器。
pub struct BackgroundTaskManager {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for BackgroundTaskManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BackgroundTaskManager")
            .field("output_dir", &self.inner.options.output_dir)
            .field("max_output_bytes", &self.inner.options.max_output_bytes)
            .finish_non_exhaustive()
    }
}

impl BackgroundTaskManager {
    /// 新建。
    pub fn new(options: BackgroundTaskOptions) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(Inner {
                options,
                tasks: Mutex::new(HashMap::new()),
                parent_usage: Mutex::new(HashMap::new()),
                task_usage: Mutex::new(HashMap::new()),
                notify_lock: tokio::sync::Mutex::new(()),
                finish_lock: tokio::sync::Mutex::new(()),
                last_error: Mutex::new(None),
            }),
        })
    }

    /// 启动后台任务并立即返回 task id。
    ///
    /// 工作闭包必须是 `Send + 'static`（`tokio::spawn` 的约束）。会话本身是
    /// `!Send` 的，需要后台执行的会话内 Work 应使用 [`Self::start_local`]。
    pub async fn start<F, Fut>(
        self: &Arc<Self>,
        spec: BackgroundTaskSpec,
        work: F,
    ) -> Result<BackgroundTaskHandle, BackgroundTaskError>
    where
        F: FnOnce(BackgroundTaskContext) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = TaskOutcome> + Send + 'static,
    {
        tokio::runtime::Handle::try_current().map_err(|_| BackgroundTaskError::NoRuntime)?;
        let prepared = self.prepare(spec, true).await?;
        let PreparedTask {
            task_id,
            output_path,
            slot,
            context,
            live_guard,
        } = prepared;
        let manager = Arc::clone(self);
        let spawn_task_id = task_id.clone();
        let join = tokio::spawn(async move {
            let _live_guard = live_guard;
            let outcome = work(context).await;
            if let Err(error) = manager.finish_task(&spawn_task_id, outcome).await {
                *manager
                    .inner
                    .last_error
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error.to_string());
            }
        });
        *slot
            .join
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(join);

        Ok(BackgroundTaskHandle {
            task_id,
            output_path,
        })
    }

    /// 在调用方的 `LocalSet` 上启动后台任务；工作闭包与 future 都不要求 `Send`。
    ///
    /// 这是会话内 `!Send` Work 的后台执行入口：`AgentSession`、rquickjs 与部分会话
    /// 状态都只能在创建它的单线程 `LocalSet` 上运行。任务被 spawn 到会话线程的
    /// **常驻** `LocalSet`（`AgentSession` 安装的派发表）上，而不是调用点恰好处于的
    /// 临时 `LocalSet`；没有常驻句柄时在写 journal / 打开输出文件之前返回
    /// [`BackgroundTaskError::NoRuntime`]，不留任何半初始化的记录。
    ///
    /// 取消是**协作式**的：`cancel` 只取消 token，不 abort future、也不抢写 journal
    /// 终态——`!Send` Work 的生命周期需要在取消后继续运行到写出自己的终态，否则
    /// Work 账本会停在 Running；task 终态则由该终态映射而来，两条账不会互相矛盾。
    pub async fn start_local<F, Fut>(
        self: &Arc<Self>,
        spec: BackgroundTaskSpec,
        work: F,
    ) -> Result<BackgroundTaskHandle, BackgroundTaskError>
    where
        F: FnOnce(BackgroundTaskContext) -> Fut + 'static,
        Fut: std::future::Future<Output = TaskOutcome> + 'static,
    {
        // 先拿到会话线程的常驻 `LocalSet`，再写 journal / 打开输出文件。临时的
        // `LocalSet` 上下文只能证明“此刻可以 spawn_local”；任务会随着宿主临时
        // executor 结束被一起丢掉。没有常驻句柄时直接失败，created record 与
        // 通知都不会产生。
        let Some(local) = crate::agent_session::session_local_set() else {
            return Err(BackgroundTaskError::NoRuntime);
        };
        // 仅“存在一个 LocalSet 句柄”不够：它可能来自不会被继续驱动的临时
        // `LocalSet`。确认 driver 心跳仍在更新后才写 journal / 打开输出文件。
        if !crate::agent_session::session_local_driver_is_live().await {
            return Err(BackgroundTaskError::NoRuntime);
        }
        let prepared = self.prepare(spec, false).await?;
        let PreparedTask {
            task_id,
            output_path,
            slot,
            context,
            live_guard,
        } = prepared;
        let manager = Arc::clone(self);
        let finish_task_id = task_id.clone();
        let join = local.spawn_local(async move {
            let _live_guard = live_guard;
            let outcome = work(context).await;
            if let Err(error) = manager.finish_task(&finish_task_id, outcome).await {
                *manager
                    .inner
                    .last_error
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error.to_string());
            }
        });
        *slot
            .join
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(join);
        Ok(BackgroundTaskHandle {
            task_id,
            output_path,
        })
    }

    /// 打开输出文件、登记活任务并写入 journal Running 记录；执行器由调用方负责。
    async fn prepare(
        &self,
        spec: BackgroundTaskSpec,
        abort_on_cancel: bool,
    ) -> Result<PreparedTask, BackgroundTaskError> {
        let task_id = format!("task_{}", uuid::Uuid::new_v4());
        // 先登记再写 Running 记录：否则另一个会话的 sweep 可能在这两步之间把记录
        // 当成“进程丢失”终结掉。早期返回时 guard 随作用域 drop，登记不会残留。
        let live_guard = LiveTaskGuard::register(&task_id);
        let acceptance_criteria = spec.acceptance_criteria.clone();
        let acceptance_workdir = spec
            .workdir
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let output_path = self
            .inner
            .options
            .output_dir
            .join(format!("{task_id}.output"));
        let output = BackgroundTaskOutput::open(&output_path, self.inner.options.max_output_bytes)
            .map_err(|error| BackgroundTaskError::OutputRejected {
                path: output_path.to_string_lossy().into_owned(),
                message: error.to_string(),
            })?;
        let output_bytes = output.written();
        let now = now_millis();
        let record = BackgroundTaskRecord {
            version: 1,
            task_id: task_id.clone(),
            parent_session_id: spec.parent_session_id,
            parent_turn_id: spec.parent_turn_id,
            tool: spec.tool,
            description: spec.description,
            status: BackgroundTaskStatus::Running,
            terminal_seq: 0,
            terminal: None,
            output_path: output_path.to_string_lossy().into_owned(),
            output_bytes,
            output_truncated: output.is_truncated(),
            notified: false,
            attempts: 1,
            created_at_ms: now,
            updated_at_ms: now,
            pid: Some(std::process::id()),
            work_backed: spec.work_backed,
            work_id: spec.work_id,
            run_id: spec.run_id,
        };
        self.inner.options.journal.save(record).await?;

        let cancel = CancellationToken::new();
        let output = Arc::new(Mutex::new(output));
        let slot = Arc::new(TaskSlot {
            cancel: cancel.clone(),
            output: Arc::clone(&output),
            join: Mutex::new(None),
            abort_on_cancel,
        });
        self.inner
            .tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(task_id.clone(), Arc::clone(&slot));

        let context = BackgroundTaskContext {
            task_id: task_id.clone(),
            cancel,
            output: TaskOutputHandle { inner: output },
            acceptance_criteria,
            workdir: acceptance_workdir,
        };
        Ok(PreparedTask {
            task_id,
            output_path,
            slot,
            context,
            live_guard,
        })
    }

    /// 取消任务。
    ///
    /// - `start` 启动的任务：取消 token、abort future，并立即抢占 terminal 通知
    ///   （迟到完成被抑制）；
    /// - `start_local` 启动的任务：只传播 token，**不** abort、也**不**抢占 terminal。
    ///   终态由任务自己的 `finish_task` 写出（后台 Work 的生命周期需要在取消后把
    ///   `Cancelled` 写进 Work 账本，再映射成 task 终态）。返回 `true` 表示取消请求
    ///   已经送达，终态稍后到达。
    pub async fn cancel(self: &Arc<Self>, task_id: &str) -> Result<bool, BackgroundTaskError> {
        let Some(record) = self.inner.options.journal.load(task_id).await? else {
            return Err(BackgroundTaskError::TaskNotFound(task_id.to_string()));
        };
        if record.is_terminal() {
            return Ok(false);
        }
        let Some(slot) = self
            .inner
            .tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(task_id)
            .cloned()
        else {
            // 本管理器里没有这个任务的执行槽：它可能属于本进程的另一个会话（共享
            // journal、各自持有自己的 manager）。擅自 CAS `Cancelled` 会抢在任务
            // 自己的终态之前落定，Work 的成功/用法/验收结果随后会被终态 no-op
            // 丢弃。没有执行槽就只报告“取消请求未送达”，不代写终态。
            return Ok(false);
        };
        slot.cancel.cancel();
        if !slot.abort_on_cancel {
            // 协作式本地任务：终态必须来自任务自己（Work 账本是权威）。这里
            // 抢写 `Cancelled` 会与“Work 已成功、task 尚未 finish”的完成路径
            // 竞争，产生 Work=success / task=cancelled 的矛盾。
            return Ok(true);
        }
        let join = slot
            .join
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(mut join) = join {
            // 取消是协作的：先给任务一个短暂窗口观察 token 并执行清理
            // （bash 的 kill_process_tree 就在取消处理里），超时后再 abort。
            // 直接 abort 会把清理 future 一起丢掉，留下仍在写输出的子进程。
            if tokio::time::timeout(CANCEL_CLEANUP_GRACE, &mut join)
                .await
                .is_err()
            {
                join.abort();
            }
        }
        let report = self.finish_task(task_id, TaskOutcome::cancelled()).await?;
        if report.won_race {
            return Ok(true);
        }
        // 任务可能在我们等待期间自己写入了 Cancelled 终态（协作式清理路径）。
        // 只有终态确实是 Cancelled 才算取消成功；自然完成不算。
        let cancelled = self
            .task(task_id)
            .await?
            .and_then(|record| record.terminal)
            .is_some_and(|terminal| terminal.status == BackgroundTaskStatus::Cancelled);
        Ok(cancelled)
    }

    /// 结算任务。并发调用只有一个赢家；输家不会发通知。
    pub async fn finish_task(
        self: &Arc<Self>,
        task_id: &str,
        outcome: TaskOutcome,
    ) -> Result<CompletionReport, BackgroundTaskError> {
        if !outcome.status.is_terminal() {
            return Err(BackgroundTaskError::InvalidOutcome(
                outcome.status.as_str().to_string(),
            ));
        }
        // 完成与取消/恢复可能并发到达同一个任务；从“读记录”到“追加输出 + CAS”必须
        // 互斥，否则输家也会把 outcome.output 写进共享输出文件（并可能抬高赢家记录的
        // output_bytes）。
        let _finish_guard = self.inner.finish_lock.lock().await;
        // 任何返回路径（journal 错误、通知失败、lost race）都必须移除执行槽：
        // 残留的 slot 会让 live_task_is_running 把已结束的任务当成仍在运行。
        let _slot_removal = SlotRemoval {
            inner: &self.inner,
            task_id: task_id.to_string(),
        };
        let Some(record) = self.inner.options.journal.load(task_id).await? else {
            return Err(BackgroundTaskError::TaskNotFound(task_id.to_string()));
        };
        if record.is_terminal() {
            return Ok(CompletionReport {
                won_race: false,
                terminal: None,
                notification: None,
            });
        }
        let slot = self
            .inner
            .tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(task_id)
            .cloned();
        let (output_bytes, output_truncated) = match slot {
            Some(slot) => {
                let mut output = slot
                    .output
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if !outcome.output.is_empty() {
                    let _ = output.append(&outcome.output)?;
                }
                (output.written(), output.is_truncated())
            }
            None => {
                let mut output = BackgroundTaskOutput::open(
                    record.output_path.as_str(),
                    self.inner.options.max_output_bytes,
                )?;
                // 崩溃可能发生在“追加终态输出”与“journal CAS”之间：记录还是
                // Running，文件却已包含上次追加。恢复到记录里的长度再追加一次，
                // 恢复路径不会把同一段输出写两遍。
                //
                // 运行中的记录从不回填 `output_bytes`（prepare 之后恒为 0），把 0
                // 当唯一可信检查点会截掉整个运行输出。只有非零检查点才可信。
                if record.output_bytes > 0 && output.written() > record.output_bytes {
                    output.truncate_to(record.output_bytes)?;
                }
                if !outcome.output.is_empty() {
                    let _ = output.append(&outcome.output)?;
                }
                (output.written(), output.is_truncated())
            }
        };
        let terminal = BackgroundTaskTerminal {
            status: outcome.status,
            reason: outcome.reason.clone(),
            exit_code: outcome.exit_code,
            usage: outcome.usage,
            acceptance: outcome.acceptance.clone(),
            finished_at_ms: now_millis(),
        };
        let Some(finished) = self
            .inner
            .options
            .journal
            .finish(task_id, terminal.clone(), output_bytes, output_truncated)
            .await?
        else {
            return Ok(CompletionReport {
                won_race: false,
                terminal: None,
                notification: None,
            });
        };
        self.accumulate_usage(&finished);
        let notification = self.notify_record(&finished).await?;
        Ok(CompletionReport {
            won_race: true,
            terminal: Some(terminal),
            notification,
        })
    }

    /// 读取任务记录。
    pub async fn task(
        &self,
        task_id: &str,
    ) -> Result<Option<BackgroundTaskRecord>, BackgroundTaskError> {
        self.inner.options.journal.load(task_id).await
    }

    /// 按父会话列出全部任务记录（运行中与终态，按 `created_at_ms` 升序）。
    ///
    /// 委托 journal 的 [`super::journal::BackgroundTaskJournal::list_for_session`]；
    /// 不暴露内部 `tasks` 表，也不跨会话返回记录。
    pub async fn list(
        &self,
        parent_session_id: &str,
    ) -> Result<Vec<BackgroundTaskRecord>, BackgroundTaskError> {
        self.inner
            .options
            .journal
            .list_for_session(parent_session_id)
            .await
    }

    /// 父会话的累计任务 usage。
    pub fn usage_for_parent(&self, parent_session_id: &str) -> TaskUsage {
        self.inner
            .parent_usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(parent_session_id)
            .copied()
            .unwrap_or_default()
    }

    /// 单个任务的累计 usage。
    pub fn usage_for_task(&self, task_id: &str) -> TaskUsage {
        self.inner
            .task_usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(task_id)
            .copied()
            .unwrap_or_default()
    }

    /// 崩溃后的启动 sweep（扫描所有会话的遗留记录；测试/嵌入用）。
    pub async fn sweep(self: &Arc<Self>) -> Result<SweepReport, BackgroundTaskError> {
        self.sweep_for_session(None).await
    }

    /// 启动 sweep：`session_id` 限定**通知重放**的范围。
    ///
    /// durable journal 根目录被所有会话共享：已投递但未消费的通知属于产生它的会话，
    /// 只能在该会话启动时重放（否则会被别的会话重复注入）。运行中/未通知的记录仍
    /// 全局扫描：进程崩溃后的 claim 接管正是由下一个启动的会话完成的，终态通知也
    /// 随之投递到接管方（`continuity_wiring` 的
    /// `startup_sweep_terminalizes_durable_records_from_a_crashed_process` 锁定了
    /// 这个行为）。
    ///
    /// 记录里的 pid 仍存活时一律跳过：另一个进程可能正在使用它；本进程的情况查
    /// 全局活任务登记表（同进程的多个会话共享 pid，只用 pid 会把别的会话正在跑的
    /// 任务误判为“进程丢失”）。
    pub async fn sweep_for_session(
        self: &Arc<Self>,
        session_id: Option<&str>,
    ) -> Result<SweepReport, BackgroundTaskError> {
        let mut report = SweepReport::default();
        // 先重放“已投递、未消费”的通知，再处理运行中/未通知的记录。
        self.inner
            .options
            .journal
            .replay_pending_deliveries(session_id)
            .await?;
        let recoverable = self.inner.options.journal.list_recoverable().await?;
        for record in recoverable {
            // 另一个进程仍在运行/仍持有这条记录：不能替它宣告终态或重发通知，也不能
            // 重复 resume（那会把同一任务跑两遍）。本进程内则查活任务登记表：只看 pid
            // 会把同进程其它会话仍在跑的任务误判为“进程丢失”。
            if record_is_live(&record) {
                report.skipped_live.push(record.task_id);
                continue;
            }
            if record.status.is_terminal() {
                if !record.notified && record.terminal.is_some() {
                    self.notify_record(&record).await?;
                    report.renotified.push(record.task_id);
                }
                continue;
            }
            if record.work_backed {
                // Work 的权威终态在账本里，不在这条 attempt 记录上。这里既不能
                // terminalize（会抢在所属会话对账之前写下错误的 process_lost），也
                // 不能交给通用 resumer（它读不到 Work 账本）。记录保持 Running，
                // 等 `recover_work` 在所属会话启动时对账。
                report.skipped_work_backed.push(record.task_id);
                continue;
            }
            let Some(updated) = self
                .inner
                .options
                .journal
                .record_attempt(&record.task_id)
                .await?
            else {
                continue;
            };
            if updated.attempts > self.inner.options.max_resume_attempts {
                self.terminalize(
                    &updated,
                    BackgroundTaskStatus::Interrupted,
                    "resume_attempts_exhausted",
                )
                .await?;
                report.terminalized.push(updated.task_id);
                continue;
            }
            let resumed = match &self.inner.options.resumer {
                Some(resumer) => resumer.resume(&updated).await?,
                None => None,
            };
            match resumed {
                Some(outcome) => {
                    self.finish_resumed(&updated, outcome).await?;
                    report.resumed.push(updated.task_id);
                }
                None => {
                    self.terminalize(&updated, BackgroundTaskStatus::Interrupted, "process_lost")
                        .await?;
                    report.terminalized.push(updated.task_id);
                }
            }
        }
        Ok(report)
    }

    async fn finish_resumed(
        self: &Arc<Self>,
        record: &BackgroundTaskRecord,
        outcome: TaskOutcome,
    ) -> Result<(), BackgroundTaskError> {
        if !outcome.status.is_terminal() {
            return Err(BackgroundTaskError::InvalidOutcome(
                outcome.status.as_str().to_string(),
            ));
        }
        let mut output = BackgroundTaskOutput::open(
            record.output_path.as_str(),
            self.inner.options.max_output_bytes,
        )?;
        // 与 `finish_task` 相同：先追加后 CAS 的窗口被崩溃打断时，重放前回到
        // 记录里的长度，避免重复追加；Running 记录的 0 不是可信检查点。
        if record.output_bytes > 0 && output.written() > record.output_bytes {
            output.truncate_to(record.output_bytes)?;
        }
        if !outcome.output.is_empty() {
            let _ = output.append(&outcome.output)?;
        }
        let terminal = BackgroundTaskTerminal {
            status: outcome.status,
            reason: outcome.reason,
            exit_code: outcome.exit_code,
            usage: outcome.usage,
            acceptance: outcome.acceptance,
            finished_at_ms: now_millis(),
        };
        if let Some(finished) = self
            .inner
            .options
            .journal
            .finish(
                &record.task_id,
                terminal,
                output.written(),
                output.is_truncated(),
            )
            .await?
        {
            self.accumulate_usage(&finished);
            let _ = self.notify_record(&finished).await?;
        }
        Ok(())
    }

    async fn terminalize(
        self: &Arc<Self>,
        record: &BackgroundTaskRecord,
        status: BackgroundTaskStatus,
        reason: &str,
    ) -> Result<(), BackgroundTaskError> {
        let terminal = BackgroundTaskTerminal {
            status,
            reason: Some(reason.to_string()),
            exit_code: None,
            usage: TaskUsage::default(),
            acceptance: None,
            finished_at_ms: now_millis(),
        };
        // Running 记录里的 output_bytes 是 prepare 时的值（通常为 0），不能代表
        // 崩溃前已经写出的输出。终态报告以文件实际长度为准。
        let (output_bytes, output_truncated) = match BackgroundTaskOutput::open(
            record.output_path.as_str(),
            self.inner.options.max_output_bytes,
        ) {
            Ok(output) => (output.written(), output.is_truncated()),
            Err(_) => (record.output_bytes, record.output_truncated),
        };
        if let Some(finished) = self
            .inner
            .options
            .journal
            .finish(&record.task_id, terminal, output_bytes, output_truncated)
            .await?
        {
            self.accumulate_usage(&finished);
            let _ = self.notify_record(&finished).await?;
        }
        Ok(())
    }

    async fn notify_record(
        self: &Arc<Self>,
        record: &BackgroundTaskRecord,
    ) -> Result<Option<BackgroundTaskNotification>, BackgroundTaskError> {
        // 锁内重新读取：完成路径与 sweep 路径可能同时看到同一条终态记录，只有
        // 第一个看到 notified=false 的调用会发送。
        let _guard = self.inner.notify_lock.lock().await;
        let Some(current) = self.inner.options.journal.load(&record.task_id).await? else {
            return Ok(None);
        };
        if current.notified {
            return Ok(None);
        }
        let Some(terminal) = &current.terminal else {
            return Ok(None);
        };
        let notification = BackgroundTaskNotification {
            task_id: current.task_id.clone(),
            terminal_seq: current.terminal_seq,
            parent_session_id: current.parent_session_id.clone(),
            parent_turn_id: current.parent_turn_id.clone(),
            tool: current.tool.clone(),
            status: terminal.status,
            reason: terminal.reason.clone(),
            exit_code: terminal.exit_code,
            output_path: current.output_path.clone(),
            output_bytes: current.output_bytes,
            output_truncated: current.output_truncated,
            usage: terminal.usage,
            acceptance: terminal.acceptance.clone(),
        };
        self.inner
            .options
            .notifier
            .notify(&notification)
            .await
            .map_err(|error| BackgroundTaskError::Notify(error.to_string()))?;
        self.inner
            .options
            .journal
            .mark_notified(&current.task_id, current.terminal_seq)
            .await?;
        Ok(Some(notification))
    }

    fn accumulate_usage(&self, record: &BackgroundTaskRecord) {
        let usage = record
            .terminal
            .as_ref()
            .map(|terminal| terminal.usage)
            .unwrap_or_default();
        self.inner
            .task_usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(record.task_id.clone())
            .or_default()
            .add(usage);
        self.inner
            .parent_usage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(record.parent_session_id.clone())
            .or_default()
            .add(usage);
    }

    /// 最近一次后台完成路径的错误（journal/通知失败）。
    pub fn last_error(&self) -> Option<String> {
        self.inner
            .last_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 输出目录。
    pub fn output_dir(&self) -> &Path {
        &self.inner.options.output_dir
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
