//! `/tasks` 后台任务面板与 headless 文本 dump。
//!
//! 设计来源：`docs/history/porting/tui-panels-design.md` §6。行为对照 qwen-code
//! `packages/cli/src/ui/commands/tasksCommand.ts` 的“三模式”做法：
//!
//! - **interactive**：本文件的 [`TasksDialog`] 是富交互面，列表 + 详情 + 取消 + 刷新；
//! - **non-interactive**（`-p` / print 文本模式）：命令层用 [`format_tasks_dump`] 输出纯文本；
//! - **acp / RPC**：同样调用 [`format_tasks_dump`]（或显式的
//!   [`format_tasks_dump_for_mode`]），因为协议消费端没有对话框。
//!
//! 三个面共享同一个数据模型 [`TasksDialogSnapshot`]，因此列表、dump 的排序与文案不会漂移。
//!
//! # 数据源
//!
//! 每一行都是 `kiana-session` 的 `BackgroundTaskRecord`——也就是 `BackgroundTaskManager`
//! 与 durable journal（`DurableTaskJournal` / `MemoryTaskJournal`）落盘的规范记录类型。
//! 本组件**不直接读 journal**：journal 的枚举 API（设计中的 `list_for_session`）由后端
//! 任务实现，命令层在打开面板时异步取回记录，再通过 [`TasksDialogSnapshot::new`] /
//! [`load_tasks_snapshot`] 交给组件。这样组件可以脱离活会话与 tokio runtime 单测，
//! 也避免在 UI 渲染路径上做 IO。
//!
//! 记录按 `created_at_ms` 升序（相同时间按 task id 稳定排序），运行中的任务**不置顶**，
//! 与 qwen 的 `startTime` 顺序一致；终态任务也会显示，因为它们仍在 journal 里。
//!
//! # 任务类别
//!
//! `BackgroundTaskRecord` 只有 `tool` 字段；类别由 [`TaskKind::from_tool`] 映射：
//!
//! | tool | 类别 |
//! |---|---|
//! | `agent` / `task` / `subagent` | [`TaskKind::Agent`] |
//! | `bash` / `shell` / `execute_bash` / `powershell` | [`TaskKind::Shell`] |
//! | `monitor` / `monitors` | [`TaskKind::Monitor`] |
//! | 其它 | [`TaskKind::Other`]（显示原始 tool） |
//!
//! Kiana 目前只有 `bash` 后台任务这个 producer（`background/tool.rs`），没有 monitor
//! producer；面板**预留** Monitor 标签与事件数字段（[`TasksDialogRow::event_count`]），
//! 但不会伪造数据。宿主要在 monitor producer 落地后填入事件数即可。
//!
//! # 列表与详情
//!
//! 列表每行：`状态图标 + 状态（含退出码/事件数） · 类别 · 描述 · task id 前缀 · 时长`。
//! 状态视觉与 §6.2 一致：
//!
//! | 状态 | 图标 | 颜色 |
//! |---|---|---|
//! | running | `●` | Warning |
//! | completed | `✓` | Success |
//! | failed / interrupted | `✗` | Error |
//! | cancelled / timed_out | `○` | Dim |
//!
//! 详情页展示 `BackgroundTaskRecord` 的全部可见字段：状态、类别、tool、描述、created /
//! updated（绝对时间 + 相对时间）、输出文件与大小/截断标记、usage、terminal（退出码、
//! reason、结束时间、acceptance）、pid。P0 不读取输出文件内容，只显示路径与大小。
//!
//! # 键盘与状态机
//!
//! ```text
//! TaskList ──Enter──> TaskDetail
//!    │  ↑↓/PageUp/PageDown      │
//!    │  / 过滤（Esc 清空、Enter 提交）
//!    │  c 取消（仅 Running）     │ c 取消 / r 刷新
//!    └──Esc（关闭面板）          └──Esc（返回列表）
//! ```
//!
//! - 过滤模式与 `/permissions` 相同：可打印字符进入 query，`r` / `c` 不再触发命令。
//! - `r` 只调用宿主回调；组件不自行拉取数据。宿主刷新成功后调用 [`TasksDialog::set_snapshot`]，
//!   组件按 task id 保留选中行。
//! - `c` 只在记录状态为 [`BackgroundTaskStatus::Running`] 时调用宿主回调：
//!   `Ok(true)` → `Cancelled <task_id>` 并请求刷新；`Ok(false)` → `Task is already finished.`；
//!   `Err(message)` → 红色错误提示并保留当前视图。
//!
//! # 失败模式与边界
//!
//! - manager 关闭：快照的 [`TasksDialogSnapshot::manager_enabled`] 为 `false`，列表显示
//!   `Background tasks are not enabled for this session.`，dump 输出同一句；
//! - 拉取失败：宿主把错误放进 [`TasksDialogSnapshot::load_error`]，面板顶部红字显示，
//!   dump 返回 `Failed to load background tasks: <清洗后的错误>`；
//! - 描述、tool、输出路径、错误文本都来自进程/用户输入，渲染与 dump 前统一经
//!   [`sanitize_task_text`] 去 ANSI 与 C0/C1 控制字符并折叠成单行，避免终端注入；
//! - 主题未初始化时颜色退化为纯文本（面板不是启动路径的一环），不会 panic；
//! - 组件只处理键盘；鼠标不在 P0 范围。
//!
//! # 与共享文件的集成
//!
//! 本文件由 Integrate 环节统一在 `components/mod.rs` 声明（`pub mod tasks_dialog;`）。
//! 在此之前，集成测试通过 `#[path = "../src/components/tasks_dialog.rs"]` 把源文件直接
//! 编进测试 crate；文件内部不引用 `crate::components::…` 的其它组件，因此测试不需要
//! 命名空间垫片。

use std::any::Any;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Local, TimeZone};
use kiana_session::background::{BackgroundTaskRecord, BackgroundTaskStatus, TaskUsage};
use kiana_theme::{THEME, ThemeColor};
use kiana_tui::keybindings::get_keybindings;
use kiana_tui::keys::decode_printable_key;
use kiana_tui::text::{truncate_to_width, truncate_to_width_default, visible_width};
use kiana_tui::tui::{Component, Focusable};

/// PageUp/PageDown 一次移动的行数（与 `/permissions` 面板一致）。
const PAGE_SIZE: usize = 8;

/// 状态列最大宽度；超长失败原因在列表里截断，完整原因在详情页。
const STATUS_COLUMN_MAX: usize = 24;

/// 类别列最小/最大宽度。
const KIND_COLUMN_MIN: usize = 5;
const KIND_COLUMN_MAX: usize = 12;

/// 描述列的最小宽度（极窄终端下仍保留一点描述文本，最终由整行截断兜底）。
const DESCRIPTION_COLUMN_MIN: usize = 8;

/// task id 超长时保留的前缀宽度；`task_8f…` 总宽 8。
const TASK_ID_PREFIX_WIDTH: usize = 7;

/// 后台任务类别。
///
/// 类别不是 journal 的持久字段，而是从 `BackgroundTaskRecord.tool` 派生的展示分类。
/// 保留 [`TaskKind::Other`] 而不是丢弃未知工具，是为了让未来新增的 producer 仍能在
/// 面板里出现（显示原始 tool 名）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskKind {
    /// 子代理任务。
    Agent,
    /// 本地 shell 命令。
    Shell,
    /// 监视器任务（Kiana 暂无 producer，字段为未来兼容保留）。
    Monitor,
    /// 未识别的工具名，原样展示。
    Other(String),
}

impl TaskKind {
    /// 按工具名映射类别。
    ///
    /// 映射表见模块文档。未知工具保留原名，绝不因为“不认识”而隐藏任务。
    pub fn from_tool(tool: &str) -> Self {
        match tool {
            "agent" | "task" | "subagent" => Self::Agent,
            "bash" | "shell" | "execute_bash" | "powershell" => Self::Shell,
            "monitor" | "monitors" => Self::Monitor,
            other => Self::Other(other.to_string()),
        }
    }

    /// 面板与 dump 里显示的类别名。
    pub fn label(&self) -> &str {
        match self {
            Self::Agent => "agent",
            Self::Shell => "shell",
            Self::Monitor => "monitor",
            Self::Other(tool) => tool,
        }
    }

    /// 是否在 headless dump 中输出 `output:` 路径。
    ///
    /// 与 qwen 一致：agent 与 shell 有输出文件；monitor 走事件流而不是文件；未知类别不猜测。
    /// 详情页不受此限制，始终展示记录里的 `output_path`。
    pub fn shows_output_path(&self) -> bool {
        matches!(self, Self::Agent | Self::Shell)
    }
}

/// 状态对应的图标与主题色（§6.2 的视觉表）。
///
/// 返回 `(icon, color)`；调用方负责把 icon 包进主题色。公开这个函数是为了让命令层
/// 或未来的 footer 指示器复用同一套视觉，而不是复制状态匹配。
pub fn status_visual(status: BackgroundTaskStatus) -> (&'static str, ThemeColor) {
    match status {
        BackgroundTaskStatus::Running => ("●", ThemeColor::Warning),
        BackgroundTaskStatus::Completed => ("✓", ThemeColor::Success),
        BackgroundTaskStatus::Failed | BackgroundTaskStatus::Interrupted => {
            ("✗", ThemeColor::Error)
        }
        BackgroundTaskStatus::Cancelled | BackgroundTaskStatus::TimedOut => ("○", ThemeColor::Dim),
    }
}

/// 面板里的一行任务：journal 记录 + 派生类别 + 宿主可选回填的统计。
///
/// `event_count` 与 `usage` 在 `BackgroundTaskRecord` 里没有（usage 只在终态 terminal 中），
/// 但 `BackgroundTaskManager` 在内存里维护 per-task usage，monitor producer 也会有事件计数。
/// 宿主要展示实时数字时用 [`TasksDialogRow::with_event_count`] / [`TasksDialogRow::with_usage`]
/// 回填；不填时终态 usage 取 `terminal.usage`，事件数不显示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TasksDialogRow {
    /// journal 的规范任务记录。
    pub record: BackgroundTaskRecord,
    /// 由 `record.tool` 派生的类别。
    pub kind: TaskKind,
    /// monitor 的事件数；`None` 表示不是 monitor 或宿主未提供。
    pub event_count: Option<u64>,
    /// 任务累计 usage；`None` 表示运行中且宿主未提供。
    pub usage: Option<TaskUsage>,
}

impl TasksDialogRow {
    /// 从 journal 记录构造一行。
    ///
    /// 类别按 [`TaskKind::from_tool`] 映射；终态记录的 `terminal.usage` 会自动填入
    /// [`TasksDialogRow::usage`]，运行中的任务保持 `None`（manager 的实时 usage 需要宿主
    /// 用 [`TasksDialogRow::with_usage`] 回填）。
    pub fn from_record(record: BackgroundTaskRecord) -> Self {
        let kind = TaskKind::from_tool(&record.tool);
        let usage = record.terminal.as_ref().map(|terminal| terminal.usage);
        Self {
            record,
            kind,
            event_count: None,
            usage,
        }
    }

    /// 回填 monitor 事件数（链式构造）。
    pub fn with_event_count(mut self, event_count: u64) -> Self {
        self.event_count = Some(event_count);
        self
    }

    /// 回填任务 usage（链式构造）。
    pub fn with_usage(mut self, usage: TaskUsage) -> Self {
        self.usage = Some(usage);
        self
    }

    /// 类别显示名（未知工具即原始 tool）。
    pub fn kind_label(&self) -> &str {
        self.kind.label()
    }

    /// 单行描述：优先 `description`，为空时退化到 tool 名。
    ///
    /// 返回值已经过去控制字符与换行折叠；直接用于渲染与 dump。
    pub fn description(&self) -> String {
        let text = self
            .record
            .description
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or(self.record.tool.as_str());
        sanitize_task_text(text)
    }

    /// 列表里显示的富状态标签。
    ///
    /// 与 qwen `statusLabel` 对齐的扩展信息：
    /// - shell/monitor 的 completed 带 `(exit N)`；
    /// - failed/timed_out/interrupted 带清洗后的 reason；
    /// - monitor 追加 `N events`，与 `(exit N)` 合并成 `(exit 0, 3 events)`。
    ///
    /// 这只是展示字符串，不改变 journal 里的 `status`。
    pub fn status_label(&self) -> String {
        let status = self.record.status;
        let terminal = self.record.terminal.as_ref();
        let exit_code = terminal.and_then(|terminal| terminal.exit_code);
        let reason = terminal
            .and_then(|terminal| terminal.reason.as_deref())
            .map(sanitize_task_text)
            .filter(|reason| !reason.is_empty());

        let mut label = match status {
            BackgroundTaskStatus::Running => "running".to_string(),
            BackgroundTaskStatus::Completed => {
                if self.kind.has_exit_code() {
                    match exit_code {
                        Some(code) => format!("completed (exit {code})"),
                        None => "completed".to_string(),
                    }
                } else {
                    "completed".to_string()
                }
            }
            BackgroundTaskStatus::Failed => match reason {
                Some(reason) => format!("failed: {reason}"),
                None => "failed".to_string(),
            },
            BackgroundTaskStatus::Cancelled => "cancelled".to_string(),
            BackgroundTaskStatus::TimedOut => match reason {
                Some(reason) => format!("timed_out: {reason}"),
                None => "timed_out".to_string(),
            },
            BackgroundTaskStatus::Interrupted => match reason {
                Some(reason) => format!("interrupted: {reason}"),
                None => "interrupted".to_string(),
            },
        };

        if self.kind == TaskKind::Monitor
            && let Some(count) = self.event_count
        {
            let events = format!("{count} event{}", if count == 1 { "" } else { "s" });
            if label.ends_with(')') && label.contains("(exit ") {
                label.pop();
                label.push_str(", ");
                label.push_str(&events);
                label.push(')');
            } else {
                label.push_str(&format!(" ({events})"));
            }
        }
        label
    }
}

impl TaskKind {
    /// shell 与 monitor 会报告进程退出码；agent 没有。
    fn has_exit_code(&self) -> bool {
        matches!(self, Self::Shell | Self::Monitor)
    }
}

/// 面板打开时的一份任务快照。
///
/// `manager_enabled = false` 表示会话的 `BackgroundTaskManager` 不存在（continuity 关闭）；
/// 这与“manager 已启用但列表为空”是两种不同状态，空态文案也不同。`load_error` 保留
/// 拉取失败的原因，便于宿主原样展示并重试。
///
/// 构造方法（[`TasksDialogSnapshot::new`] / [`TasksDialogSnapshot::with_rows`]）保证行按
/// `created_at_ms` 升序、相同时按 task id 升序；直接改 `rows` 会破坏这个不变量，渲染和
/// dump 仍会防御性排序。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TasksDialogSnapshot {
    /// 会话是否启用了后台任务 manager。
    pub manager_enabled: bool,
    /// 当前会话的任务行。
    pub rows: Vec<TasksDialogRow>,
    /// 最近一次拉取失败的原因；成功时为 `None`。
    pub load_error: Option<String>,
}

impl Default for TasksDialogSnapshot {
    fn default() -> Self {
        Self::empty()
    }
}

impl TasksDialogSnapshot {
    /// manager 已启用但没有任何任务的快照。
    pub fn empty() -> Self {
        Self {
            manager_enabled: true,
            rows: Vec::new(),
            load_error: None,
        }
    }

    /// manager 未启用（continuity 关闭）的快照。
    pub fn disabled() -> Self {
        Self {
            manager_enabled: false,
            rows: Vec::new(),
            load_error: None,
        }
    }

    /// 从 journal 记录构造快照（manager 视为已启用）。
    pub fn new(records: Vec<BackgroundTaskRecord>) -> Self {
        Self::with_rows(
            records
                .into_iter()
                .map(TasksDialogRow::from_record)
                .collect(),
        )
    }

    /// 从已构造的行构造快照（用于回填事件数/usage 的场景）。
    pub fn with_rows(rows: Vec<TasksDialogRow>) -> Self {
        let mut snapshot = Self {
            manager_enabled: true,
            rows,
            load_error: None,
        };
        snapshot.sort_rows();
        snapshot
    }

    /// 附加拉取错误（保留现有行，便于刷新失败时继续展示旧数据）。
    pub fn with_load_error(mut self, message: impl Into<String>) -> Self {
        self.load_error = Some(message.into());
        self
    }

    /// 按 `created_at_ms` 升序排序；相同时间用 task id 做稳定 tie-break。
    fn sort_rows(&mut self) {
        self.rows.sort_by(|left, right| {
            left.record
                .created_at_ms
                .cmp(&right.record.created_at_ms)
                .then_with(|| left.record.task_id.cmp(&right.record.task_id))
        });
    }
}

/// 打开 `/tasks` 面板时拉取快照的通用异步入口。
///
/// `loader` 的返回值约定：
/// - `Ok(Some(records))`：manager 已启用，记录来自 journal；
/// - `Ok(None)`：manager 未启用（命令层应先检查 `session.background_task_manager()`）；
/// - `Err(message)`：manager 已启用但读取失败，快照保留 `load_error` 并显示为错误。
///
/// 这个函数不关心具体的 session 类型，命令层可以在 `spawn_local` 里把
/// `AgentSession::list_background_tasks`（设计 §6.1 的后端 API）包成 future 传进来。
pub async fn load_tasks_snapshot<F, Fut>(loader: F) -> TasksDialogSnapshot
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<Option<Vec<BackgroundTaskRecord>>, String>>,
{
    match loader().await {
        Ok(Some(records)) => TasksDialogSnapshot::new(records),
        Ok(None) => TasksDialogSnapshot::disabled(),
        Err(message) => TasksDialogSnapshot::empty().with_load_error(message),
    }
}

/// notice 的级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TasksNoticeKind {
    /// 普通信息（灰色）。
    Info,
    /// 错误（红色）。
    Error,
}

/// 面板底部的一行提示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TasksNotice {
    /// 级别。
    pub kind: TasksNoticeKind,
    /// 文案（渲染前会经 [`sanitize_task_text`] 处理）。
    pub message: String,
}

impl TasksNotice {
    /// 信息提示。
    pub fn info(message: impl Into<String>) -> Self {
        Self {
            kind: TasksNoticeKind::Info,
            message: message.into(),
        }
    }

    /// 错误提示。
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            kind: TasksNoticeKind::Error,
            message: message.into(),
        }
    }
}

/// 面板当前视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TasksDialogView {
    /// 任务列表。
    TaskList,
    /// 单个任务的详情；按 task id 定位，刷新后仍能回到同一任务。
    TaskDetail {
        /// 正在查看的任务 id。
        task_id: String,
    },
}

/// 取消回调。
///
/// 参数是完整记录（宿主通常只需要 `task_id`，但保留记录便于日志/统计）。返回值沿用
/// `BackgroundTaskManager::cancel` 的语义：
/// - `Ok(true)`：本次调用赢得了 Running → Cancelled 的转换；
/// - `Ok(false)`：任务已经是终态（并发完成或重复取消）；
/// - `Err(message)`：journal/IO 失败，面板显示红色错误。
pub type TasksCancelCallback = Box<dyn FnMut(&BackgroundTaskRecord) -> Result<bool, String>>;

/// 宿主提供的回调面。
///
/// 组件不持有 `AgentSession`、不直接 await 异步取消：所有副作用都经这里回到命令层。
/// `r` 键只触发 [`TasksDialogCallbacks::on_refresh`]；宿主拉取完成后调用
/// [`TasksDialog::set_snapshot`]，组件按 task id 保留选中项。
pub struct TasksDialogCallbacks {
    /// `c` 键的取消入口。
    pub on_cancel: TasksCancelCallback,
    /// `r` 键的刷新入口。
    pub on_refresh: Box<dyn FnMut()>,
    /// 列表页 `Esc` 的关闭入口。
    pub on_close: Box<dyn FnMut()>,
}

impl Default for TasksDialogCallbacks {
    fn default() -> Self {
        Self {
            on_cancel: Box::new(|_| Ok(true)),
            on_refresh: Box::new(|| {}),
            on_close: Box::new(|| {}),
        }
    }
}

/// [`TasksDialog::new`] 的参数。
pub struct TasksDialogOptions {
    /// 初始快照（[`TasksDialogSnapshot::new`] / [`load_tasks_snapshot`] 的结果）。
    pub snapshot: TasksDialogSnapshot,
    /// 宿主回调。
    pub callbacks: TasksDialogCallbacks,
}

/// `/tasks` 后台任务对话框。
///
/// 实现 [`Component`] + [`Focusable`]：`handles_input()` 恒为 `true`，挂载时用
/// `SelectorFocus::Component`（与其他面板一致）。组件只保存快照、视图与过滤状态；
/// 任务数据与取消副作用都由宿主提供。
pub struct TasksDialog {
    snapshot: TasksDialogSnapshot,
    view: TasksDialogView,
    /// 当前过滤结果里的选中下标。
    selected: usize,
    /// 过滤查询串。
    query: String,
    /// 是否处于过滤输入模式。
    filter_active: bool,
    notice: Option<TasksNotice>,
    callbacks: TasksDialogCallbacks,
    focused: bool,
}

impl TasksDialog {
    /// 构造面板。初始视图是任务列表，选中第一行。
    pub fn new(options: TasksDialogOptions) -> Self {
        let mut snapshot = options.snapshot;
        snapshot.sort_rows();
        let mut dialog = Self {
            snapshot,
            view: TasksDialogView::TaskList,
            selected: 0,
            query: String::new(),
            filter_active: false,
            notice: None,
            callbacks: options.callbacks,
            focused: false,
        };
        dialog.clamp_selection();
        dialog
    }

    // -- 只读访问（宿主与测试） ---------------------------------------------

    /// 当前快照。
    pub fn snapshot(&self) -> &TasksDialogSnapshot {
        &self.snapshot
    }

    /// 当前视图。
    pub fn view(&self) -> &TasksDialogView {
        &self.view
    }

    /// 过滤结果里的选中下标。
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// 当前过滤结果（保持快照的排序）。
    pub fn filtered_rows(&self) -> Vec<&TasksDialogRow> {
        self.snapshot
            .rows
            .iter()
            .filter(|row| self.matches_query(row))
            .collect()
    }

    /// 选中行；空列表或过滤无结果时为 `None`。
    pub fn selected_row(&self) -> Option<&TasksDialogRow> {
        self.filtered_rows().get(self.selected).copied()
    }

    /// 选中行的 task id。
    pub fn selected_task_id(&self) -> Option<&str> {
        self.selected_row().map(|row| row.record.task_id.as_str())
    }

    /// 当前 notice。
    pub fn notice(&self) -> Option<&TasksNotice> {
        self.notice.as_ref()
    }

    /// 过滤查询串。
    pub fn query(&self) -> &str {
        &self.query
    }

    /// 是否处于过滤输入模式。
    pub fn is_filter_active(&self) -> bool {
        self.filter_active
    }

    // -- 宿主回填 -----------------------------------------------------------

    /// 替换快照并尽量保持选中的 task id。
    ///
    /// 刷新可能改变排序或让任务消失：
    /// - 之前选中的 task id 仍存在 → 选中它（即使排序位置变了）；
    /// - 不存在 → 把下标夹回有效范围；
    /// - 详情页正在看的任务消失 → 退回列表页。
    ///
    /// notice 不清空：宿主可以在刷新失败后继续展示上一条错误。
    pub fn set_snapshot(&mut self, snapshot: TasksDialogSnapshot) {
        let previous_task_id = self.selected_task_id().map(str::to_string);
        let viewing_task_id = match &self.view {
            TasksDialogView::TaskList => None,
            TasksDialogView::TaskDetail { task_id } => Some(task_id.clone()),
        };

        self.snapshot = snapshot;
        self.snapshot.sort_rows();

        if let Some(task_id) = previous_task_id
            && let Some(index) = self
                .filtered_rows()
                .iter()
                .position(|row| row.record.task_id == task_id)
        {
            self.selected = index;
        }
        self.clamp_selection();

        if let Some(task_id) = viewing_task_id
            && !self
                .snapshot
                .rows
                .iter()
                .any(|row| row.record.task_id == task_id)
        {
            self.view = TasksDialogView::TaskList;
        }
    }

    /// 设置 notice（宿主用于异步错误或进度提示）。
    pub fn set_notice(&mut self, notice: Option<TasksNotice>) {
        self.notice = notice;
    }

    /// 清空 notice。
    pub fn clear_notice(&mut self) {
        self.notice = None;
    }

    // -- 内部：过滤与选中 ---------------------------------------------------

    /// 行是否匹配当前过滤串。
    ///
    /// 大小写不敏感的子串匹配，字段范围：task id、tool、类别、状态、单行描述。
    /// 不匹配输出路径，避免路径片段意外命中。
    fn matches_query(&self, row: &TasksDialogRow) -> bool {
        if self.query.is_empty() {
            return true;
        }
        let needle = self.query.to_lowercase();
        let task_id = row.record.task_id.to_lowercase();
        let tool = row.record.tool.to_lowercase();
        let kind = row.kind_label().to_lowercase();
        let status = row.record.status.as_str().to_lowercase();
        let description = row.description().to_lowercase();
        [task_id, tool, kind, status, description]
            .iter()
            .any(|field| field.contains(&needle))
    }

    fn clamp_selection(&mut self) {
        let len = self.filtered_rows().len();
        self.selected = if len == 0 {
            0
        } else {
            self.selected.min(len - 1)
        };
    }

    fn move_selection(&mut self, direction: isize, page: bool) {
        let len = self.filtered_rows().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let step = if page { PAGE_SIZE as isize } else { 1 };
        let current = self.selected.min(len - 1) as isize;
        let next = (current + direction.signum() * step).clamp(0, len as isize - 1);
        self.selected = next as usize;
    }

    /// 当前活动行：列表页取选中行，详情页按 task id 查找。
    fn active_row(&self) -> Option<&TasksDialogRow> {
        match &self.view {
            TasksDialogView::TaskList => self.selected_row(),
            TasksDialogView::TaskDetail { task_id } => self
                .snapshot
                .rows
                .iter()
                .find(|row| row.record.task_id == *task_id),
        }
    }

    // -- 内部：按键 ---------------------------------------------------------

    fn is_confirm(key: &str) -> bool {
        get_keybindings().matches(key, "tui.select.confirm") || key == "\n" || key == "\r"
    }

    fn is_cancel(key: &str) -> bool {
        get_keybindings().matches(key, "tui.select.cancel")
    }

    fn is_up(key: &str) -> bool {
        get_keybindings().matches(key, "tui.select.up")
    }

    fn is_down(key: &str) -> bool {
        get_keybindings().matches(key, "tui.select.down")
    }

    fn is_page_up(key: &str) -> bool {
        get_keybindings().matches(key, "tui.select.pageUp")
    }

    fn is_page_down(key: &str) -> bool {
        get_keybindings().matches(key, "tui.select.pageDown")
    }

    fn is_backspace(key: &str) -> bool {
        get_keybindings().matches(key, "tui.editor.deleteCharBackward")
            || key == "\x7f"
            || key == "\x08"
    }

    /// 取出可打印输入；方向键、功能键等控制序列返回 `None`。
    fn printable_input(key: &str) -> Option<String> {
        if let Some(decoded) = decode_printable_key(key) {
            return Some(decoded);
        }
        if key.is_empty()
            || key.starts_with('\x1b')
            || key == "\t"
            || key == "\n"
            || key == "\r"
            || key == "\x7f"
            || key == "\x08"
            || key.chars().any(char::is_control)
        {
            return None;
        }
        Some(key.to_string())
    }

    fn handle_filter_input(&mut self, key: &str) {
        if Self::is_cancel(key) {
            self.filter_active = false;
            self.query.clear();
            self.clamp_selection();
            return;
        }
        if Self::is_confirm(key) {
            // 过滤中 Enter 只提交过滤并退出输入模式，不激活行。
            self.filter_active = false;
            return;
        }
        if Self::is_backspace(key) {
            self.query.pop();
            self.clamp_selection();
            return;
        }
        if let Some(text) = Self::printable_input(key) {
            self.query.push_str(&text);
            self.clamp_selection();
        }
    }

    fn handle_list_input(&mut self, key: &str) {
        if self.filter_active {
            self.handle_filter_input(key);
            return;
        }
        if key == "/" {
            self.filter_active = true;
            self.query.clear();
            self.selected = 0;
            return;
        }
        if Self::is_up(key) {
            self.move_selection(-1, false);
            return;
        }
        if Self::is_down(key) {
            self.move_selection(1, false);
            return;
        }
        if Self::is_page_up(key) {
            self.move_selection(-1, true);
            return;
        }
        if Self::is_page_down(key) {
            self.move_selection(1, true);
            return;
        }
        if Self::is_confirm(key) {
            if let Some(task_id) = self.selected_task_id().map(str::to_string) {
                self.notice = None;
                self.view = TasksDialogView::TaskDetail { task_id };
            }
            return;
        }
        if Self::is_cancel(key) {
            self.close();
            return;
        }
        match key {
            "c" => self.cancel_active(),
            "r" => self.refresh(),
            _ => {}
        }
    }

    fn handle_detail_input(&mut self, key: &str) {
        if Self::is_cancel(key) {
            self.notice = None;
            self.view = TasksDialogView::TaskList;
            return;
        }
        match key {
            "c" => self.cancel_active(),
            "r" => self.refresh(),
            _ => {}
        }
    }

    // -- 内部：副作用 -------------------------------------------------------

    fn close(&mut self) {
        (self.callbacks.on_close)();
    }

    fn refresh(&mut self) {
        (self.callbacks.on_refresh)();
    }

    /// 取消当前活动行；只有 Running 状态会调用宿主回调。
    fn cancel_active(&mut self) {
        let Some(row) = self.active_row().cloned() else {
            return;
        };
        if row.record.status != BackgroundTaskStatus::Running {
            self.notice = Some(TasksNotice::info("Task is already finished."));
            return;
        }
        match (self.callbacks.on_cancel)(&row.record) {
            Ok(true) => {
                self.notice = Some(TasksNotice::info(format!(
                    "Cancelled {}",
                    row.record.task_id
                )));
                // 组件不本地改状态：宿主刷新后拿到 journal 的真实终态；刷新前重复按 c
                // 由 manager 的 CAS 返回 Ok(false)，不会重复取消。
                (self.callbacks.on_refresh)();
            }
            Ok(false) => {
                self.notice = Some(TasksNotice::info("Task is already finished."));
            }
            Err(message) => {
                self.notice = Some(TasksNotice::error(message));
            }
        }
    }

    // -- 内部：渲染 ---------------------------------------------------------

    /// 按视口宽度渲染；`now_ms` 由调用方注入，便于测试固定相对时长。
    pub fn render_at(&mut self, width: usize, now_ms: u64) -> Vec<String> {
        let width = width.max(1);
        let body = match &self.view {
            TasksDialogView::TaskList => self.render_list(width, now_ms),
            TasksDialogView::TaskDetail { task_id } => self.render_detail(task_id, now_ms),
        };
        let mut lines = vec![fg(ThemeColor::BorderMuted, &"─".repeat(width))];
        for line in body {
            lines.push(truncate_to_width_default(&line, width));
        }
        lines.push(fg(ThemeColor::BorderMuted, &"─".repeat(width)));
        lines
    }

    fn render_list(&self, width: usize, now_ms: u64) -> Vec<String> {
        let mut lines = vec![bold(&format!(
            "Background tasks ({})",
            self.snapshot.rows.len()
        ))];

        if let Some(error) = &self.snapshot.load_error {
            lines.push(fg(
                ThemeColor::Error,
                &format!(
                    "Failed to load background tasks: {}",
                    sanitize_task_text(error)
                ),
            ));
        }

        if !self.snapshot.manager_enabled {
            lines.push(fg(
                ThemeColor::Dim,
                "Background tasks are not enabled for this session.",
            ));
        } else {
            if let Some(filter) = self.render_filter_line() {
                lines.push(filter);
            }
            let rows = self.filtered_rows();
            if rows.is_empty() {
                let message = if self.query.is_empty() {
                    "No background tasks."
                } else {
                    "No background tasks match the filter."
                };
                lines.push(fg(ThemeColor::Dim, message));
            } else {
                lines.extend(self.render_rows(&rows, width, now_ms));
            }
        }

        let hint = if !self.snapshot.manager_enabled {
            "Esc close"
        } else {
            "↑↓ choose · Enter details · c cancel · r refresh · / filter · Esc close"
        };
        lines.push(fg(ThemeColor::Dim, hint));
        if let Some(notice) = &self.notice {
            lines.push(render_notice(notice));
        }
        lines
    }

    fn render_filter_line(&self) -> Option<String> {
        if !self.filter_active && self.query.is_empty() {
            return None;
        }
        let cursor = if self.filter_active { "▏" } else { "" };
        Some(format!(
            " {} {}{cursor}",
            fg(ThemeColor::Accent, "filter:"),
            fg(ThemeColor::Text, &self.query)
        ))
    }

    /// 渲染任务行。
    ///
    /// 列宽按**全部**（不只过滤结果）的行计算，过滤时列不跳动。状态与类别先截断到
    /// 各自上限，剩余宽度给描述；id 与时长是固定短字段。
    fn render_rows(&self, rows: &[&TasksDialogRow], width: usize, now_ms: u64) -> Vec<String> {
        let status_texts: Vec<String> = rows
            .iter()
            .map(|row| truncate_to_width_default(&row.status_label(), STATUS_COLUMN_MAX))
            .collect();
        let kind_texts: Vec<String> = rows
            .iter()
            .map(|row| {
                truncate_to_width_default(&sanitize_task_text(row.kind_label()), KIND_COLUMN_MAX)
            })
            .collect();
        let id_texts: Vec<String> = rows
            .iter()
            .map(|row| short_task_id(&row.record.task_id))
            .collect();
        let duration_texts: Vec<String> = rows
            .iter()
            .map(|row| format_task_duration(row_duration_ms(row, now_ms)))
            .collect();

        let status_width = self
            .snapshot
            .rows
            .iter()
            .map(|row| {
                visible_width(&truncate_to_width_default(
                    &row.status_label(),
                    STATUS_COLUMN_MAX,
                ))
            })
            .max()
            .unwrap_or(0)
            .max(6);
        let kind_width = self
            .snapshot
            .rows
            .iter()
            .map(|row| {
                visible_width(&truncate_to_width_default(
                    &sanitize_task_text(row.kind_label()),
                    KIND_COLUMN_MAX,
                ))
            })
            .max()
            .unwrap_or(0)
            .max(KIND_COLUMN_MIN);
        let id_width = self
            .snapshot
            .rows
            .iter()
            .map(|row| visible_width(&short_task_id(&row.record.task_id)))
            .max()
            .unwrap_or(0);
        let duration_width = self
            .snapshot
            .rows
            .iter()
            .map(|row| visible_width(&format_task_duration(row_duration_ms(row, now_ms))))
            .max()
            .unwrap_or(0);

        // 2（选中前缀）+ 1（图标）+ 1（图标后的空格）+ 各列 + 4 个两空格分隔。
        let fixed = 4 + status_width + kind_width + id_width + duration_width + 8;
        let description_width = width.saturating_sub(fixed).max(DESCRIPTION_COLUMN_MIN);

        let mut lines = Vec::with_capacity(rows.len());
        for (index, row) in rows.iter().enumerate() {
            let (icon, color) = status_visual(row.record.status);
            let prefix = if index == self.selected { "→ " } else { "  " };
            let status = pad_visible(&status_texts[index], status_width);
            let kind = pad_visible(&kind_texts[index], kind_width);
            let description = pad_visible(
                &truncate_to_width_default(&row.description(), description_width),
                description_width,
            );
            let task_id = pad_visible(&id_texts[index], id_width);
            let duration = pad_visible(&duration_texts[index], duration_width);
            lines.push(format!(
                "{prefix}{} {status}  {}  {description}  {}  {}",
                fg(color, icon),
                fg(ThemeColor::Dim, &kind),
                fg(ThemeColor::Dim, &task_id),
                fg(ThemeColor::Dim, &duration),
            ));
        }
        lines
    }

    fn render_detail(&self, task_id: &str, now_ms: u64) -> Vec<String> {
        let Some(row) = self
            .snapshot
            .rows
            .iter()
            .find(|row| row.record.task_id == task_id)
        else {
            return vec![fg(
                ThemeColor::Error,
                "This task is no longer tracked by the background task manager.",
            )];
        };
        let record = &row.record;
        let mut lines = vec![bold(&format!(
            "Task {}",
            sanitize_task_text(&record.task_id)
        ))];
        lines.push(label_value("status", &row.status_label()));
        lines.push(label_value("kind", &sanitize_task_text(row.kind_label())));
        lines.push(label_value("tool", &sanitize_task_text(&record.tool)));
        if let Some(description) = record.description.as_deref() {
            let description = sanitize_task_text(description);
            if !description.is_empty() {
                lines.push(label_value("description", &description));
            }
        }
        lines.push(label_value(
            "created",
            &format!(
                "{} ({})",
                format_timestamp_ms(record.created_at_ms),
                format_ago(now_ms, record.created_at_ms)
            ),
        ));
        lines.push(label_value(
            "updated",
            &format!(
                "{} ({})",
                format_timestamp_ms(record.updated_at_ms),
                format_ago(now_ms, record.updated_at_ms)
            ),
        ));
        lines.push(label_value(
            "output",
            &format!(
                "{} ({}, truncated: {})",
                sanitize_task_text(&record.output_path),
                format_bytes(record.output_bytes),
                if record.output_truncated { "yes" } else { "no" }
            ),
        ));
        if let Some(usage) = row.usage {
            lines.push(label_value(
                "usage",
                &format!(
                    "in {} · out {} · total {}",
                    format_compact_count(usage.input_tokens),
                    format_compact_count(usage.output_tokens),
                    format_compact_count(usage.total_tokens)
                ),
            ));
        }
        if let Some(terminal) = &record.terminal {
            let mut parts = vec![terminal.status.as_str().to_string()];
            if let Some(exit_code) = terminal.exit_code {
                parts.push(format!("exit {exit_code}"));
            }
            if let Some(reason) = terminal.reason.as_deref() {
                let reason = sanitize_task_text(reason);
                if !reason.is_empty() {
                    parts.push(format!("reason: {reason}"));
                }
            }
            parts.push(format!(
                "finished {}",
                format_timestamp_ms(terminal.finished_at_ms)
            ));
            if let Some(acceptance) = &terminal.acceptance {
                parts.push(format!("acceptance: {}", acceptance.status.as_str()));
            }
            lines.push(label_value("terminal", &parts.join(" · ")));
        }
        if let Some(pid) = record.pid {
            lines.push(label_value("pid", &pid.to_string()));
        }
        lines.push(String::new());
        lines.push(fg(ThemeColor::Dim, "r refresh · c cancel · Esc back"));
        if let Some(notice) = &self.notice {
            lines.push(render_notice(notice));
        }
        lines
    }
}

impl Component for TasksDialog {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.render_at(width, now_millis())
    }

    fn handle_input(&mut self, key_data: &str) {
        match self.view.clone() {
            TasksDialogView::TaskList => self.handle_list_input(key_data),
            TasksDialogView::TaskDetail { .. } => self.handle_detail_input(key_data),
        }
    }

    fn handles_input(&self) -> bool {
        true
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }

    fn width(&self) -> Option<f64> {
        None
    }
}

impl Focusable for TasksDialog {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }
}

// =============================================================================
// headless 文本 dump（print / RPC / ACP）
// =============================================================================

/// 文本 dump 面对的消费端。
///
/// 对应 qwen `tasksCommand` 的三种 `executionMode`：
/// - [`TasksDumpMode::Interactive`]：TTY 里对话框不可用时的文本回退，额外给一行软提示；
/// - [`TasksDumpMode::NonInteractive`]：`-p` / print 文本模式，输出稳定文本；
/// - [`TasksDumpMode::Acp`]：IDE bridge / SDK，输出与 non-interactive 相同。
///
/// 三种模式的数据与列表正文完全一致，只有 interactive 的提示行不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TasksDumpMode {
    /// 交互式 TUI 的文本回退。
    Interactive,
    /// 非交互式 print 文本模式。
    NonInteractive,
    /// ACP / RPC 消费端。
    Acp,
}

/// 非交互模式下的任务文本 dump（print / RPC 的稳定面）。
///
/// 等价于 [`format_tasks_dump_for_mode`] 传入 [`TasksDumpMode::NonInteractive`]。输出格式
/// 与设计文档 §6.3 一致：
///
/// ```text
/// Background tasks (2):
///   [running] shell  "cargo test --all"  task_8f...  output: /path/task_8f....output
///   [completed] shell  "make build"  task_8e...  output: /path/task_8e....output
/// ```
///
/// 无任务时返回 `No background tasks.`；manager 关闭时返回
/// `Background tasks are not enabled for this session.`。描述只保留单行并去掉控制字符；
/// task id 保留完整值（与 qwen 一致）。
pub fn format_tasks_dump(snapshot: &TasksDialogSnapshot) -> String {
    format_tasks_dump_for_mode(snapshot, TasksDumpMode::NonInteractive)
}

/// 按消费端输出任务文本 dump。
///
/// 排序规则与面板相同：`created_at_ms` 升序，相同时按 task id 升序。`output:` 只对
/// [`TaskKind::Agent`] / [`TaskKind::Shell`] 输出（qwen 语义）；monitor 走事件流，
/// 未知类别不猜测。所有来自记录的自由文本都会先经 [`sanitize_task_text`] 清洗。
pub fn format_tasks_dump_for_mode(snapshot: &TasksDialogSnapshot, mode: TasksDumpMode) -> String {
    if !snapshot.manager_enabled {
        return "Background tasks are not enabled for this session.".to_string();
    }
    if let Some(error) = &snapshot.load_error {
        return format!(
            "Failed to load background tasks: {}",
            sanitize_task_text(error)
        );
    }

    let mut rows: Vec<&TasksDialogRow> = snapshot.rows.iter().collect();
    rows.sort_by(|left, right| {
        left.record
            .created_at_ms
            .cmp(&right.record.created_at_ms)
            .then_with(|| left.record.task_id.cmp(&right.record.task_id))
    });
    if rows.is_empty() {
        return "No background tasks.".to_string();
    }

    let mut lines = Vec::new();
    if mode == TasksDumpMode::Interactive {
        lines.push("Tip: open the /tasks dialog for details and cancellation.".to_string());
        lines.push(String::new());
    }
    lines.push(format!("Background tasks ({}):", rows.len()));
    for row in rows {
        let description = row.description();
        let kind = sanitize_task_text(row.kind_label());
        let task_id = sanitize_task_text(&row.record.task_id);
        let output = if row.kind.shows_output_path() {
            format!("  output: {}", sanitize_task_text(&row.record.output_path))
        } else {
            String::new()
        };
        lines.push(format!(
            "  [{}] {}  \"{}\"  {}{}",
            row.record.status.as_str(),
            kind,
            description,
            task_id,
            output
        ));
    }
    lines.join("\n")
}

// =============================================================================
// 格式化与清洗助手
// =============================================================================

/// 去掉终端不安全字符并把文本折叠成单行。
///
/// 处理四类输入（与 qwen `stripUnsafeCharacters` 的目标一致，但本函数额外折叠空白）：
/// - ANSI CSI / OSC / APC 转义序列（以 ESC 开头）；
/// - C0 控制字符（BEL、BS、VT、FF 等）；
/// - C1 控制字符（U+0080–U+009F，Rust `char::is_control` 覆盖）；
/// - 换行/回车/制表符替换成空格，连续空白折叠为一个空格。
///
/// 保留可打印 Unicode 与内部空格。为什么必须在渲染前做：`description`、`tool`、
/// `output_path`、journal 错误都可能来自用户命令或进程输出，未清洗的 ESC 会改写终端
/// 状态、伪造行，甚至覆盖面板边框。
fn sanitize_task_text(text: &str) -> String {
    let mut without_escapes = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\x1b' {
            // 跳过一整条转义序列；未知形式只丢掉 ESC 与其后的一个字符。
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') | Some('_') => {
                    chars.next();
                    while let Some(next) = chars.next() {
                        if next == '\x07' {
                            break;
                        }
                        if next == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        match character {
            '\r' | '\n' | '\t' => without_escapes.push(' '),
            other if other.is_control() => {}
            other => without_escapes.push(other),
        }
    }

    let mut collapsed = String::with_capacity(without_escapes.len());
    let mut last_was_space = false;
    for character in without_escapes.chars() {
        if character == ' ' {
            if last_was_space {
                continue;
            }
            last_was_space = true;
        } else {
            last_was_space = false;
        }
        collapsed.push(character);
    }
    collapsed.trim().to_string()
}

/// 主题前景色；主题未初始化时退化为无颜色（面板不 panic）。
fn fg(color: ThemeColor, text: &str) -> String {
    THEME.fg(color, text).unwrap_or_else(|_| text.to_string())
}

/// 主题粗体；主题未初始化时退化为原文本。
fn bold(text: &str) -> String {
    THEME.bold(text).unwrap_or_else(|_| text.to_string())
}

/// `label: value` 形式的详情行（label 灰色）。
fn label_value(label: &str, value: &str) -> String {
    format!("  {} {}", fg(ThemeColor::Dim, &format!("{label}:")), value)
}

/// 渲染 notice。
fn render_notice(notice: &TasksNotice) -> String {
    let message = sanitize_task_text(&notice.message);
    match notice.kind {
        TasksNoticeKind::Info => fg(ThemeColor::Dim, &format!("ℹ {message}")),
        TasksNoticeKind::Error => fg(ThemeColor::Error, &format!("✗ {message}")),
    }
}

/// 按可见宽度补空格；已超宽时原样返回（由整行截断兜底）。
fn pad_visible(text: &str, width: usize) -> String {
    let current = visible_width(text);
    if current >= width {
        text.to_string()
    } else {
        format!("{text}{}", " ".repeat(width - current))
    }
}

/// task id 的短形式：超过 8 列时保留 7 列前缀加省略号（`task_8f…`）。
fn short_task_id(task_id: &str) -> String {
    let sanitized = sanitize_task_text(task_id);
    if visible_width(&sanitized) > TASK_ID_PREFIX_WIDTH + 1 {
        // 必须显式传空省略号：`truncate_to_width_default` 自带 `...`，再追加 `…`
        // 会让前缀短一截并出现双重省略号。
        format!(
            "{}…",
            truncate_to_width(&sanitized, TASK_ID_PREFIX_WIDTH, "", false)
        )
    } else {
        sanitized
    }
}

/// 列表里显示的时长：终态用 `updated_at_ms`，运行中用 `now_ms`。
fn row_duration_ms(row: &TasksDialogRow, now_ms: u64) -> u64 {
    let end = if row.record.status.is_terminal() {
        row.record.updated_at_ms
    } else {
        now_ms
    };
    end.saturating_sub(row.record.created_at_ms)
}

/// 列表时长格式：`MM:SS`、`HH:MM:SS`，超过一天追加天数。
pub fn format_task_duration(duration_ms: u64) -> String {
    let seconds = duration_ms / 1000;
    if seconds < 3600 {
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    } else if seconds < 86_400 {
        format!(
            "{:02}:{:02}:{:02}",
            seconds / 3600,
            (seconds % 3600) / 60,
            seconds % 60
        )
    } else {
        format!(
            "{}d {:02}:{:02}:{:02}",
            seconds / 86_400,
            (seconds % 86_400) / 3600,
            (seconds % 3600) / 60,
            seconds % 60
        )
    }
}

/// 详情里的相对时长（`12s ago`、`1m 3s ago`、`2h 5m ago`、`3d 4h ago`）。
fn format_task_duration_words(duration_ms: u64) -> String {
    let seconds = duration_ms / 1000;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        let minutes = seconds / 60;
        let remainder = seconds % 60;
        if remainder == 0 {
            format!("{minutes}m")
        } else {
            format!("{minutes}m {remainder}s")
        }
    } else if seconds < 86_400 {
        let hours = seconds / 3600;
        let minutes = (seconds % 3600) / 60;
        if minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h {minutes}m")
        }
    } else {
        let days = seconds / 86_400;
        let hours = (seconds % 86_400) / 3600;
        if hours == 0 {
            format!("{days}d")
        } else {
            format!("{days}d {hours}h")
        }
    }
}

/// `12s ago` 形式的相对时间；时钟回拨时按 0 处理。
fn format_ago(now_ms: u64, then_ms: u64) -> String {
    format!(
        "{} ago",
        format_task_duration_words(now_ms.saturating_sub(then_ms))
    )
}

/// Unix 毫秒 → 本地时区 `YYYY-MM-DD HH:MM:SS`。
///
/// 用 [`chrono::Local`] 而不是 UTC：面板与 `tree_selector` 的时间戳一致，用户看到的是
/// 自己时区的墙上时间。时间戳超出 chrono 可表示范围时退化为原始毫秒数。
fn format_timestamp_ms(timestamp_ms: u64) -> String {
    match Local.timestamp_millis_opt(timestamp_ms as i64).single() {
        Some(datetime) => datetime.format("%Y-%m-%d %H:%M:%S").to_string(),
        None => timestamp_ms.to_string(),
    }
}

/// 字节数的紧凑展示（`512 B`、`1.2 KB`、`3.4 MB`、`1.0 GB`）。
fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;
    if bytes < KIB {
        format!("{bytes} B")
    } else if bytes < MIB {
        format!("{:.1} KB", bytes as f64 / KIB as f64)
    } else if bytes < GIB {
        format!("{:.1} MB", bytes as f64 / MIB as f64)
    } else {
        format!("{:.1} GB", bytes as f64 / GIB as f64)
    }
}

/// token 数的紧凑展示（与 footer 的 `format_tokens` 不同，这里固定一位小数以便详情对齐）。
fn format_compact_count(value: u64) -> String {
    if value < 1000 {
        value.to_string()
    } else if value < 1_000_000 {
        format!("{:.1}k", value as f64 / 1000.0)
    } else {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    }
}

/// 当前 Unix 毫秒。
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
