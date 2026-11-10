//! `TasksDialog` 的渲染/按键/取消回调/过滤/headless dump 测试。
//!
//! 组件文件通过 `#[path]` 直接编进测试 crate：`components/mod.rs` 由 Integrate 环节
//! 统一声明，本任务不修改共享文件。组件内部不引用 `crate::components::…` 的其它组件，
//! 因此这里不需要 `components` 命名空间垫片（与 `tests/components_mcp_dialog.rs` 相同）。
//!
//! 锁定行为：
//! - 空态 / manager 关闭 / 拉取错误的三种文案；
//! - 列表的 `created_at_ms` 升序、状态图标与颜色、退出码/事件数、时长与 task id 短形式；
//! - 列表 → 详情 → 返回的 Esc 语义（只有列表页 Esc 关闭面板）；
//! - 详情页展示记录的全部可见字段（输出路径、大小、usage、terminal、pid）；
//! - `c` 只对 Running 调回调，`Ok(true)` / `Ok(false)` / `Err` 三种结果；
//! - `r` 在列表与详情都会调用刷新回调，`set_snapshot` 按 task id 保持选中；
//! - `/` 过滤模式吞掉 `r`/`c`，Enter 提交、Esc 清空；
//! - headless dump 的稳定文本、类别映射、控制字符清洗与三模式提示差异；
//! - 极窄宽度不越界。

#[path = "../src/components/tasks_dialog.rs"]
pub mod tasks_dialog;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard};

use kiana_session::background::{
    BackgroundTaskRecord, BackgroundTaskStatus, BackgroundTaskTerminal, TaskUsage,
};
use kiana_theme::{THEME, ThemeColor};
use kiana_tui::text::visible_width;
use kiana_tui::tui::{Component, Focusable};
use tasks_dialog::{
    TaskKind, TasksDialog, TasksDialogCallbacks, TasksDialogOptions, TasksDialogRow,
    TasksDialogSnapshot, TasksDialogView, TasksDumpMode, TasksNoticeKind, format_task_duration,
    format_tasks_dump, format_tasks_dump_for_mode, load_tasks_snapshot, status_visual,
};

const NOW: u64 = 1_700_000_000_000;

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

/// 主题是进程级全局状态；测试并行运行，用一把锁串行化初始化与渲染。
fn setup() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    kiana_theme::init_theme(Some("dark"), false);
    guard
}

/// 去掉 CSI/OSC/APC 转义序列（测试只需比较可见文本）。
fn strip_ansi(text: &str) -> String {
    let mut output = String::new();
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\x1b' {
            output.push(character);
            continue;
        }
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
                for next in chars.by_ref() {
                    if next == '\x07' {
                        break;
                    }
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    output
}

fn render_plain(dialog: &mut TasksDialog, width: usize, now_ms: u64) -> Vec<String> {
    dialog
        .render_at(width, now_ms)
        .iter()
        .map(|line| strip_ansi(line))
        .collect()
}

fn joined(dialog: &mut TasksDialog, width: usize, now_ms: u64) -> String {
    render_plain(dialog, width, now_ms).join("\n")
}

fn base_record(
    task_id: &str,
    tool: &str,
    description: &str,
    status: BackgroundTaskStatus,
    created_at_ms: u64,
) -> BackgroundTaskRecord {
    BackgroundTaskRecord {
        version: 1,
        task_id: task_id.to_string(),
        parent_session_id: "session-test".to_string(),
        parent_turn_id: format!("tool:{task_id}"),
        tool: tool.to_string(),
        description: Some(description.to_string()),
        status,
        terminal_seq: if status.is_terminal() { 1 } else { 0 },
        terminal: None,
        output_path: format!("/tmp/kiana-tasks/{task_id}.output"),
        output_bytes: 1234,
        output_truncated: false,
        notified: status.is_terminal(),
        attempts: 1,
        created_at_ms,
        updated_at_ms: created_at_ms + 12_000,
        pid: Some(12345),
        work_backed: false,
        work_id: None,
        run_id: None,
    }
}

fn finish(
    mut record: BackgroundTaskRecord,
    status: BackgroundTaskStatus,
    exit_code: Option<i32>,
    reason: Option<&str>,
    finished_at_ms: u64,
) -> BackgroundTaskRecord {
    assert!(status.is_terminal(), "finish() 只接受终态");
    record.status = status;
    record.terminal_seq = 1;
    record.notified = true;
    record.updated_at_ms = finished_at_ms;
    record.terminal = Some(BackgroundTaskTerminal {
        status,
        reason: reason.map(str::to_string),
        exit_code,
        usage: TaskUsage {
            input_tokens: 1200,
            output_tokens: 340,
            total_tokens: 1540,
        },
        acceptance: None,
        finished_at_ms,
    });
    record
}

fn running_shell(created_at_ms: u64) -> BackgroundTaskRecord {
    base_record(
        "task_aaaaaaaa",
        "bash",
        "cargo test --all",
        BackgroundTaskStatus::Running,
        created_at_ms,
    )
}

fn completed_shell(created_at_ms: u64, finished_at_ms: u64) -> BackgroundTaskRecord {
    finish(
        base_record(
            "task_bbbbbbbb",
            "bash",
            "make build",
            BackgroundTaskStatus::Running,
            created_at_ms,
        ),
        BackgroundTaskStatus::Completed,
        Some(0),
        None,
        finished_at_ms,
    )
}

fn failed_agent(created_at_ms: u64, finished_at_ms: u64) -> BackgroundTaskRecord {
    finish(
        base_record(
            "task_cccccccc",
            "agent",
            "review the diff",
            BackgroundTaskStatus::Running,
            created_at_ms,
        ),
        BackgroundTaskStatus::Failed,
        Some(1),
        Some("exit status 1"),
        finished_at_ms,
    )
}

fn snapshot(records: Vec<BackgroundTaskRecord>) -> TasksDialogSnapshot {
    TasksDialogSnapshot::new(records)
}

fn dialog(snapshot: TasksDialogSnapshot, callbacks: TasksDialogCallbacks) -> TasksDialog {
    TasksDialog::new(TasksDialogOptions {
        snapshot,
        callbacks,
    })
}

#[derive(Debug)]
struct Recorder {
    closed: usize,
    refreshed: usize,
    cancelled: Vec<String>,
    cancel_result: Result<bool, String>,
}

impl Default for Recorder {
    fn default() -> Self {
        Self {
            closed: 0,
            refreshed: 0,
            cancelled: Vec::new(),
            cancel_result: Ok(true),
        }
    }
}

fn recording_callbacks(recorder: Rc<RefCell<Recorder>>) -> TasksDialogCallbacks {
    TasksDialogCallbacks {
        on_cancel: Box::new({
            let recorder = Rc::clone(&recorder);
            move |record| {
                recorder.borrow_mut().cancelled.push(record.task_id.clone());
                recorder.borrow().cancel_result.clone()
            }
        }),
        on_refresh: Box::new({
            let recorder = Rc::clone(&recorder);
            move || recorder.borrow_mut().refreshed += 1
        }),
        on_close: Box::new({
            let recorder = Rc::clone(&recorder);
            move || recorder.borrow_mut().closed += 1
        }),
    }
}

fn down(dialog: &mut TasksDialog) {
    dialog.handle_input("\x1b[B");
}

fn up(dialog: &mut TasksDialog) {
    dialog.handle_input("\x1b[A");
}

fn page_down(dialog: &mut TasksDialog) {
    dialog.handle_input("\x1b[6~");
}

fn page_up(dialog: &mut TasksDialog) {
    dialog.handle_input("\x1b[5~");
}

fn enter(dialog: &mut TasksDialog) {
    dialog.handle_input("\r");
}

fn escape(dialog: &mut TasksDialog) {
    dialog.handle_input("\x1b");
}

// ---------------------------------------------------------------------------
// 空态 / manager 状态
// ---------------------------------------------------------------------------

#[test]
fn empty_snapshot_renders_empty_state() {
    let _guard = setup();
    let mut dialog = dialog(
        TasksDialogSnapshot::empty(),
        TasksDialogCallbacks::default(),
    );
    let text = joined(&mut dialog, 120, NOW);

    assert!(text.contains("Background tasks (0)"));
    assert!(text.contains("No background tasks."));
    assert!(!text.contains("Background tasks are not enabled for this session."));
    assert!(text.contains("Enter details"));
}

#[test]
fn disabled_snapshot_renders_disabled_message() {
    let _guard = setup();
    let mut dialog = dialog(
        TasksDialogSnapshot::disabled(),
        TasksDialogCallbacks::default(),
    );
    let text = joined(&mut dialog, 120, NOW);

    assert!(text.contains("Background tasks (0)"));
    assert!(text.contains("Background tasks are not enabled for this session."));
    assert!(!text.contains("No background tasks."));
    assert!(text.contains("Esc close"));
}

#[test]
fn load_error_is_rendered_and_precedes_rows() {
    let _guard = setup();
    let snapshot =
        snapshot(vec![running_shell(NOW - 5_000)]).with_load_error("journal offline\x1b[31m");
    let mut dialog = dialog(snapshot, TasksDialogCallbacks::default());
    let text = joined(&mut dialog, 160, NOW);

    assert!(text.contains("Failed to load background tasks: journal offline"));
    // 旧数据仍展示，方便刷新失败时继续操作。
    assert!(text.contains("cargo test --all"));
    assert!(!text.contains('\x1b'));
}

// ---------------------------------------------------------------------------
// 列表渲染
// ---------------------------------------------------------------------------

#[test]
fn list_sorts_by_created_at_and_renders_status_exit_code_duration() {
    let _guard = setup();
    let rows = vec![
        TasksDialogRow::from_record(running_shell(NOW - 12_000)),
        TasksDialogRow::from_record(completed_shell(NOW - 63_000, NOW)),
        TasksDialogRow::from_record(failed_agent(NOW - 41_000, NOW)),
    ];
    let mut dialog = dialog(
        TasksDialogSnapshot::with_rows(rows),
        TasksDialogCallbacks::default(),
    );
    let text = joined(&mut dialog, 160, NOW);
    let lines: Vec<&str> = text.lines().collect();

    // created_at_ms 升序：completed(-63s) → failed(-41s) → running(-12s)，运行中不置顶。
    let completed = text.find("completed (exit 0)").unwrap();
    let failed = text.find("failed: exit status 1").unwrap();
    let running = text.find("running").unwrap();
    assert!(completed < failed && failed < running);

    assert!(
        lines
            .iter()
            .any(|line| line.contains("→ ✓ completed (exit 0)"))
    );
    assert!(text.contains("✓ completed (exit 0)"));
    assert!(text.contains("✗ failed: exit status 1"));
    assert!(text.contains("● running"));
    assert!(text.contains("01:03"));
    assert!(text.contains("00:41"));
    assert!(text.contains("00:12"));
    assert!(text.contains("shell"));
    assert!(text.contains("agent"));
}

#[test]
fn list_renders_monitor_event_count_and_short_task_id() {
    let _guard = setup();
    let monitor = base_record(
        "task_dddddddd",
        "monitor",
        "watch app logs",
        BackgroundTaskStatus::Running,
        NOW - 5_000,
    );
    let rows = vec![
        TasksDialogRow::from_record(running_shell(NOW - 12_000)),
        TasksDialogRow::from_record(monitor).with_event_count(3),
    ];
    let mut dialog = dialog(
        TasksDialogSnapshot::with_rows(rows),
        TasksDialogCallbacks::default(),
    );
    let text = joined(&mut dialog, 160, NOW);

    assert!(text.contains("monitor"));
    assert!(text.contains("running (3 events)"));
    // task id 前缀 7 列 + 省略号。
    assert!(text.contains("task_aa…"));
    assert!(text.contains("task_dd…"));
}

#[test]
fn status_visual_matches_design_colors() {
    assert_eq!(
        status_visual(BackgroundTaskStatus::Running),
        ("●", ThemeColor::Warning)
    );
    assert_eq!(
        status_visual(BackgroundTaskStatus::Completed),
        ("✓", ThemeColor::Success)
    );
    assert_eq!(
        status_visual(BackgroundTaskStatus::Failed),
        ("✗", ThemeColor::Error)
    );
    assert_eq!(
        status_visual(BackgroundTaskStatus::Interrupted),
        ("✗", ThemeColor::Error)
    );
    assert_eq!(
        status_visual(BackgroundTaskStatus::Cancelled),
        ("○", ThemeColor::Dim)
    );
    assert_eq!(
        status_visual(BackgroundTaskStatus::TimedOut),
        ("○", ThemeColor::Dim)
    );
}

#[test]
fn rendered_icons_use_theme_colors() {
    let _guard = setup();
    let cancelled = finish(
        base_record(
            "task_eeeeeeee",
            "bash",
            "old command",
            BackgroundTaskStatus::Running,
            NOW - 30_000,
        ),
        BackgroundTaskStatus::Cancelled,
        None,
        None,
        NOW - 20_000,
    );
    let rows = vec![
        TasksDialogRow::from_record(running_shell(NOW - 12_000)),
        TasksDialogRow::from_record(completed_shell(NOW - 63_000, NOW)),
        TasksDialogRow::from_record(failed_agent(NOW - 41_000, NOW)),
        TasksDialogRow::from_record(cancelled),
    ];
    let mut dialog = dialog(
        TasksDialogSnapshot::with_rows(rows),
        TasksDialogCallbacks::default(),
    );
    let raw = dialog.render_at(160, NOW).join("\n");

    assert!(raw.contains(&THEME.fg(ThemeColor::Warning, "●").unwrap()));
    assert!(raw.contains(&THEME.fg(ThemeColor::Success, "✓").unwrap()));
    assert!(raw.contains(&THEME.fg(ThemeColor::Error, "✗").unwrap()));
    assert!(raw.contains(&THEME.fg(ThemeColor::Dim, "○").unwrap()));
}

#[test]
fn render_is_width_bounded_at_every_width() {
    let _guard = setup();
    let mut dialog = dialog(
        snapshot(vec![
            running_shell(NOW - 12_000),
            completed_shell(NOW - 63_000, NOW),
        ]),
        TasksDialogCallbacks::default(),
    );
    for width in [1, 8, 16, 24, 80] {
        let lines = render_plain(&mut dialog, width, NOW);
        assert!(
            lines.iter().all(|line| visible_width(line) <= width),
            "width={width} 有越界行: {lines:?}"
        );
        assert!(lines.len() >= 2);
    }
}

#[test]
fn task_kind_mapping_covers_all_three_categories() {
    assert_eq!(TaskKind::from_tool("agent"), TaskKind::Agent);
    assert_eq!(TaskKind::from_tool("task"), TaskKind::Agent);
    assert_eq!(TaskKind::from_tool("subagent"), TaskKind::Agent);
    assert_eq!(TaskKind::from_tool("bash"), TaskKind::Shell);
    assert_eq!(TaskKind::from_tool("shell"), TaskKind::Shell);
    assert_eq!(TaskKind::from_tool("execute_bash"), TaskKind::Shell);
    assert_eq!(TaskKind::from_tool("powershell"), TaskKind::Shell);
    assert_eq!(TaskKind::from_tool("monitor"), TaskKind::Monitor);
    assert_eq!(TaskKind::from_tool("monitors"), TaskKind::Monitor);
    assert_eq!(
        TaskKind::from_tool("custom_tool"),
        TaskKind::Other("custom_tool".to_string())
    );
    assert_eq!(TaskKind::Agent.label(), "agent");
    assert_eq!(TaskKind::Shell.label(), "shell");
    assert_eq!(TaskKind::Monitor.label(), "monitor");
    assert_eq!(TaskKind::Other("x".into()).label(), "x");
    assert!(TaskKind::Agent.shows_output_path());
    assert!(TaskKind::Shell.shows_output_path());
    assert!(!TaskKind::Monitor.shows_output_path());
    assert!(!TaskKind::Other("x".into()).shows_output_path());
}

// ---------------------------------------------------------------------------
// 详情与导航
// ---------------------------------------------------------------------------

#[test]
fn enter_opens_detail_and_escape_returns_then_closes() {
    let _guard = setup();
    let recorder = Rc::new(RefCell::new(Recorder::default()));
    let mut dialog = dialog(
        snapshot(vec![running_shell(NOW - 12_000)]),
        recording_callbacks(Rc::clone(&recorder)),
    );

    enter(&mut dialog);
    assert_eq!(
        dialog.view(),
        &TasksDialogView::TaskDetail {
            task_id: "task_aaaaaaaa".to_string()
        }
    );
    assert_eq!(recorder.borrow().closed, 0);

    escape(&mut dialog);
    assert_eq!(dialog.view(), &TasksDialogView::TaskList);
    assert_eq!(recorder.borrow().closed, 0);

    escape(&mut dialog);
    assert_eq!(recorder.borrow().closed, 1);
}

#[test]
fn detail_renders_record_fields() {
    let _guard = setup();
    let record = completed_shell(NOW - 12_000, NOW);
    let mut dialog = dialog(snapshot(vec![record]), TasksDialogCallbacks::default());
    enter(&mut dialog);
    let text = joined(&mut dialog, 160, NOW);

    assert!(text.contains("Task task_bbbbbbbb"));
    assert!(text.contains("status: completed (exit 0)"));
    assert!(text.contains("kind: shell"));
    assert!(text.contains("tool: bash"));
    assert!(text.contains("description: make build"));
    assert!(text.contains("(12s ago)"));
    assert!(text.contains("(0s ago)"));
    assert!(text.contains("output: /tmp/kiana-tasks/task_bbbbbbbb.output (1.2 KB, truncated: no)"));
    assert!(text.contains("usage: in 1.2k · out 340 · total 1.5k"));
    assert!(text.contains("terminal: completed · exit 0 · finished "));
    assert!(text.contains("pid: 12345"));
    assert!(text.contains("r refresh · c cancel · Esc back"));
}

#[test]
fn running_detail_omits_terminal_and_usage_lines() {
    let _guard = setup();
    let mut dialog = dialog(
        snapshot(vec![running_shell(NOW - 12_000)]),
        TasksDialogCallbacks::default(),
    );
    enter(&mut dialog);
    let text = joined(&mut dialog, 160, NOW);

    assert!(text.contains("status: running"));
    assert!(!text.contains("terminal:"));
    assert!(!text.contains("usage:"));
}

#[test]
fn set_snapshot_keeps_the_selected_task_id() {
    let _guard = setup();
    let rows = vec![
        TasksDialogRow::from_record(running_shell(NOW - 30_000)),
        TasksDialogRow::from_record(completed_shell(NOW - 20_000, NOW)),
        TasksDialogRow::from_record(failed_agent(NOW - 10_000, NOW)),
    ];
    let mut dialog = dialog(
        TasksDialogSnapshot::with_rows(rows),
        TasksDialogCallbacks::default(),
    );

    down(&mut dialog);
    assert_eq!(dialog.selected_task_id(), Some("task_bbbbbbbb"));
    enter(&mut dialog);

    // 刷新后顺序变化（created_at 不变，只是输入顺序不同），仍按 task id 选中。
    let refreshed_rows = vec![
        TasksDialogRow::from_record(failed_agent(NOW - 10_000, NOW)),
        TasksDialogRow::from_record(completed_shell(NOW - 20_000, NOW)),
        TasksDialogRow::from_record(running_shell(NOW - 30_000)),
    ];
    dialog.set_snapshot(TasksDialogSnapshot::with_rows(refreshed_rows));
    assert_eq!(dialog.selected_task_id(), Some("task_bbbbbbbb"));
    assert_eq!(
        dialog.view(),
        &TasksDialogView::TaskDetail {
            task_id: "task_bbbbbbbb".to_string()
        }
    );

    // 正在查看的任务消失：退回列表页。
    let without_selected = vec![
        TasksDialogRow::from_record(running_shell(NOW - 30_000)),
        TasksDialogRow::from_record(failed_agent(NOW - 10_000, NOW)),
    ];
    dialog.set_snapshot(TasksDialogSnapshot::with_rows(without_selected));
    assert_eq!(dialog.view(), &TasksDialogView::TaskList);
    assert!(dialog.selected_task_id().is_some());
}

#[test]
fn refresh_key_invokes_callback_in_list_and_detail() {
    let recorder = Rc::new(RefCell::new(Recorder::default()));
    let mut dialog = dialog(
        snapshot(vec![running_shell(NOW - 12_000)]),
        recording_callbacks(Rc::clone(&recorder)),
    );

    dialog.handle_input("r");
    assert_eq!(recorder.borrow().refreshed, 1);

    enter(&mut dialog);
    dialog.handle_input("r");
    assert_eq!(recorder.borrow().refreshed, 2);
}

// ---------------------------------------------------------------------------
// 取消
// ---------------------------------------------------------------------------

#[test]
fn cancel_running_task_invokes_callback_and_requests_refresh() {
    let recorder = Rc::new(RefCell::new(Recorder::default()));
    let mut dialog = dialog(
        snapshot(vec![running_shell(NOW - 12_000)]),
        recording_callbacks(Rc::clone(&recorder)),
    );

    dialog.handle_input("c");

    assert_eq!(recorder.borrow().cancelled, vec!["task_aaaaaaaa"]);
    assert_eq!(recorder.borrow().refreshed, 1);
    let notice = dialog.notice().unwrap();
    assert_eq!(notice.kind, TasksNoticeKind::Info);
    assert_eq!(notice.message, "Cancelled task_aaaaaaaa");
    // 组件不本地改状态；真实终态等宿主刷新。
    assert_eq!(
        dialog.snapshot().rows[0].record.status,
        BackgroundTaskStatus::Running
    );
}

#[test]
fn cancel_terminal_task_does_not_invoke_callback() {
    let recorder = Rc::new(RefCell::new(Recorder::default()));
    let mut dialog = dialog(
        snapshot(vec![completed_shell(NOW - 63_000, NOW)]),
        recording_callbacks(Rc::clone(&recorder)),
    );

    dialog.handle_input("c");

    assert!(recorder.borrow().cancelled.is_empty());
    assert_eq!(recorder.borrow().refreshed, 0);
    let notice = dialog.notice().unwrap();
    assert_eq!(notice.kind, TasksNoticeKind::Info);
    assert_eq!(notice.message, "Task is already finished.");
}

#[test]
fn cancel_false_and_error_results_set_the_expected_notice() {
    let recorder = Rc::new(RefCell::new(Recorder::default()));
    recorder.borrow_mut().cancel_result = Ok(false);
    let mut dialog = dialog(
        snapshot(vec![running_shell(NOW - 12_000)]),
        recording_callbacks(Rc::clone(&recorder)),
    );

    dialog.handle_input("c");
    assert_eq!(recorder.borrow().cancelled.len(), 1);
    assert_eq!(recorder.borrow().refreshed, 0);
    assert_eq!(
        dialog.notice().unwrap().message,
        "Task is already finished."
    );

    recorder.borrow_mut().cancel_result = Err("journal unavailable".to_string());
    dialog.handle_input("c");
    assert_eq!(recorder.borrow().cancelled.len(), 2);
    assert_eq!(recorder.borrow().refreshed, 0);
    let notice = dialog.notice().unwrap();
    assert_eq!(notice.kind, TasksNoticeKind::Error);
    assert_eq!(notice.message, "journal unavailable");
}

#[test]
fn detail_view_can_cancel_the_open_task() {
    let recorder = Rc::new(RefCell::new(Recorder::default()));
    let mut dialog = dialog(
        snapshot(vec![
            running_shell(NOW - 30_000),
            completed_shell(NOW - 20_000, NOW),
        ]),
        recording_callbacks(Rc::clone(&recorder)),
    );

    // sorted：running(-30s, task_a) 在前，completed(-20s, task_b) 在后；Enter 打开第一行。
    assert_eq!(dialog.selected_task_id(), Some("task_aaaaaaaa"));
    enter(&mut dialog);
    assert_eq!(
        dialog.view(),
        &TasksDialogView::TaskDetail {
            task_id: "task_aaaaaaaa".to_string()
        }
    );

    dialog.handle_input("c");
    assert_eq!(recorder.borrow().cancelled, vec!["task_aaaaaaaa"]);
    assert_eq!(recorder.borrow().refreshed, 1);
}

#[test]
fn cancel_on_empty_list_is_a_noop() {
    let recorder = Rc::new(RefCell::new(Recorder::default()));
    let mut dialog = dialog(
        TasksDialogSnapshot::empty(),
        recording_callbacks(Rc::clone(&recorder)),
    );

    dialog.handle_input("c");
    assert!(recorder.borrow().cancelled.is_empty());
    assert_eq!(recorder.borrow().refreshed, 0);
    assert!(dialog.notice().is_none());
}

// ---------------------------------------------------------------------------
// 过滤与导航
// ---------------------------------------------------------------------------

#[test]
fn filter_limits_rows_and_swallows_command_keys() {
    let recorder = Rc::new(RefCell::new(Recorder::default()));
    let mut dialog = dialog(
        snapshot(vec![
            running_shell(NOW - 30_000),
            completed_shell(NOW - 20_000, NOW),
            failed_agent(NOW - 10_000, NOW),
        ]),
        recording_callbacks(Rc::clone(&recorder)),
    );

    dialog.handle_input("/");
    assert!(dialog.is_filter_active());
    dialog.handle_input("make");
    assert_eq!(dialog.query(), "make");
    assert_eq!(dialog.filtered_rows().len(), 1);
    assert_eq!(dialog.selected_task_id(), Some("task_bbbbbbbb"));

    // 过滤模式下 r/c 是普通字符，不触发命令。
    dialog.handle_input("r");
    dialog.handle_input("c");
    assert_eq!(dialog.query(), "makerc");
    assert!(recorder.borrow().cancelled.is_empty());
    assert_eq!(recorder.borrow().refreshed, 0);
    assert_eq!(dialog.filtered_rows().len(), 0);
}

#[test]
fn filter_enter_commits_and_escape_clears() {
    let recorder = Rc::new(RefCell::new(Recorder::default()));
    let mut dialog = dialog(
        snapshot(vec![
            running_shell(NOW - 30_000),
            completed_shell(NOW - 20_000, NOW),
            failed_agent(NOW - 10_000, NOW),
        ]),
        recording_callbacks(Rc::clone(&recorder)),
    );

    dialog.handle_input("/");
    dialog.handle_input("make");
    enter(&mut dialog);

    // Enter 只提交过滤，不打开详情。
    assert!(!dialog.is_filter_active());
    assert_eq!(dialog.query(), "make");
    assert_eq!(dialog.filtered_rows().len(), 1);
    assert_eq!(dialog.view(), &TasksDialogView::TaskList);

    // 提交后命令键恢复。
    dialog.handle_input("r");
    assert_eq!(recorder.borrow().refreshed, 1);

    // 再次进入过滤，Esc 清空查询并退出输入模式。
    dialog.handle_input("/");
    dialog.handle_input("zzz");
    assert_eq!(dialog.filtered_rows().len(), 0);
    escape(&mut dialog);
    assert!(!dialog.is_filter_active());
    assert_eq!(dialog.query(), "");
    assert_eq!(dialog.filtered_rows().len(), 3);
}

#[test]
fn page_keys_move_within_filtered_rows() {
    let records: Vec<BackgroundTaskRecord> = (0..10)
        .map(|index| {
            base_record(
                &format!("task_{index:08}"),
                "bash",
                &format!("command {index}"),
                BackgroundTaskStatus::Running,
                NOW - 100_000 + index as u64 * 1_000,
            )
        })
        .collect();
    let mut dialog = dialog(snapshot(records), TasksDialogCallbacks::default());

    page_down(&mut dialog);
    assert_eq!(dialog.selected(), 8);
    page_down(&mut dialog);
    assert_eq!(dialog.selected(), 9);
    page_down(&mut dialog);
    assert_eq!(dialog.selected(), 9);
    page_up(&mut dialog);
    assert_eq!(dialog.selected(), 1);
    up(&mut dialog);
    assert_eq!(dialog.selected(), 0);

    // 过滤后仍按过滤结果长度夹取。
    dialog.handle_input("/");
    dialog.handle_input("command 1");
    enter(&mut dialog);
    assert_eq!(dialog.filtered_rows().len(), 1);
    assert_eq!(dialog.selected(), 0);
}

#[test]
fn filtering_matches_task_id_tool_kind_and_status() {
    let mut dialog = dialog(
        snapshot(vec![
            running_shell(NOW - 30_000),
            completed_shell(NOW - 20_000, NOW),
            failed_agent(NOW - 10_000, NOW),
        ]),
        TasksDialogCallbacks::default(),
    );

    for (query, expected) in [
        ("task_cccc", 1usize),
        ("agent", 1),
        ("completed", 1),
        ("bash", 2),
        ("missing", 0),
    ] {
        dialog.handle_input("/");
        dialog.handle_input(query);
        enter(&mut dialog);
        assert_eq!(dialog.filtered_rows().len(), expected, "query={query}");
        dialog.handle_input("/");
        escape(&mut dialog);
    }
}

// ---------------------------------------------------------------------------
// headless 文本 dump
// ---------------------------------------------------------------------------

#[test]
fn headless_dump_matches_the_design_shape() {
    let dump = format_tasks_dump(&snapshot(vec![
        completed_shell(NOW - 63_000, NOW),
        running_shell(NOW - 12_000),
    ]));

    let expected = concat!(
        "Background tasks (2):\n",
        "  [completed] shell  \"make build\"  task_bbbbbbbb  output: /tmp/kiana-tasks/task_bbbbbbbb.output\n",
        "  [running] shell  \"cargo test --all\"  task_aaaaaaaa  output: /tmp/kiana-tasks/task_aaaaaaaa.output",
    );
    assert_eq!(dump, expected);
}

#[test]
fn headless_dump_empty_disabled_and_error() {
    assert_eq!(
        format_tasks_dump(&TasksDialogSnapshot::empty()),
        "No background tasks."
    );
    assert_eq!(
        format_tasks_dump(&TasksDialogSnapshot::disabled()),
        "Background tasks are not enabled for this session."
    );
    assert_eq!(
        format_tasks_dump(&TasksDialogSnapshot::empty().with_load_error("journal\x1b[31m offline")),
        "Failed to load background tasks: journal offline"
    );
}

#[test]
fn headless_dump_omits_output_for_monitor_and_other() {
    let monitor = base_record(
        "task_dddddddd",
        "monitor",
        "watch app logs",
        BackgroundTaskStatus::Running,
        NOW - 2_000,
    );
    let other = base_record(
        "task_eeeeeeee",
        "custom_tool",
        "custom work",
        BackgroundTaskStatus::Running,
        NOW - 1_000,
    );
    let rows = vec![
        TasksDialogRow::from_record(monitor).with_event_count(3),
        TasksDialogRow::from_record(other),
    ];
    let dump = format_tasks_dump(&TasksDialogSnapshot::with_rows(rows));

    let expected = concat!(
        "Background tasks (2):\n",
        "  [running] monitor  \"watch app logs\"  task_dddddddd\n",
        "  [running] custom_tool  \"custom work\"  task_eeeeeeee",
    );
    assert_eq!(dump, expected);
}

#[test]
fn headless_dump_sanitizes_control_characters_and_ansi() {
    let mut record = base_record(
        "task_ffffffff",
        "bash",
        "bad\x1b[31mred\x1b[0m\nnext\tline\x07",
        BackgroundTaskStatus::Running,
        NOW - 1_000,
    );
    record.output_path = "/tmp/\x1b]0;evil\x07out.output".to_string();
    let dump = format_tasks_dump(&snapshot(vec![record]));

    assert!(!dump.contains('\x1b'));
    assert!(!dump.contains('\x07'));
    assert_eq!(dump.lines().count(), 2);
    assert!(dump.contains("\"badred next line\""));
    assert!(dump.contains("output: /tmp/out.output"));
}

#[test]
fn interactive_dump_adds_a_tip_but_other_modes_do_not() {
    let snapshot = snapshot(vec![running_shell(NOW - 12_000)]);

    let interactive = format_tasks_dump_for_mode(&snapshot, TasksDumpMode::Interactive);
    let non_interactive = format_tasks_dump_for_mode(&snapshot, TasksDumpMode::NonInteractive);
    let acp = format_tasks_dump_for_mode(&snapshot, TasksDumpMode::Acp);

    assert!(interactive.starts_with("Tip: open the /tasks dialog"));
    assert!(interactive.contains("Background tasks (1):"));
    assert_eq!(non_interactive, format_tasks_dump(&snapshot));
    assert_eq!(non_interactive, acp);
    assert!(!non_interactive.contains("Tip:"));
}

#[test]
fn duration_and_status_label_formats_are_locked() {
    assert_eq!(format_task_duration(0), "00:00");
    assert_eq!(format_task_duration(12_000), "00:12");
    assert_eq!(format_task_duration(63_000), "01:03");
    assert_eq!(format_task_duration(3_661_000), "01:01:01");
    assert_eq!(format_task_duration(90_000_000), "1d 01:00:00");

    let monitor = base_record(
        "task_dddddddd",
        "monitor",
        "watch app logs",
        BackgroundTaskStatus::Running,
        NOW,
    );
    let running = TasksDialogRow::from_record(monitor).with_event_count(1);
    assert_eq!(running.status_label(), "running (1 event)");

    let completed = finish(
        base_record(
            "task_dddddddd",
            "monitor",
            "watch app logs",
            BackgroundTaskStatus::Running,
            NOW,
        ),
        BackgroundTaskStatus::Completed,
        Some(0),
        None,
        NOW,
    );
    let completed = TasksDialogRow::from_record(completed).with_event_count(3);
    assert_eq!(completed.status_label(), "completed (exit 0, 3 events)");
}

#[test]
fn snapshot_constructor_sorts_rows_and_maps_kinds() {
    let snapshot = snapshot(vec![
        failed_agent(NOW - 10_000, NOW),
        running_shell(NOW - 30_000),
        completed_shell(NOW - 20_000, NOW),
    ]);

    let ids: Vec<&str> = snapshot
        .rows
        .iter()
        .map(|row| row.record.task_id.as_str())
        .collect();
    assert_eq!(ids, vec!["task_aaaaaaaa", "task_bbbbbbbb", "task_cccccccc"]);
    assert_eq!(snapshot.rows[0].kind, TaskKind::Shell);
    assert_eq!(snapshot.rows[2].kind, TaskKind::Agent);
    assert!(snapshot.manager_enabled);
    assert!(snapshot.load_error.is_none());
}

#[tokio::test]
async fn load_snapshot_maps_disabled_loaded_and_error() {
    let disabled =
        load_tasks_snapshot(|| async { Ok::<Option<Vec<BackgroundTaskRecord>>, String>(None) })
            .await;
    assert!(!disabled.manager_enabled);
    assert!(disabled.rows.is_empty());

    let loaded = load_tasks_snapshot(|| async {
        Ok::<Option<Vec<BackgroundTaskRecord>>, String>(Some(vec![running_shell(NOW - 12_000)]))
    })
    .await;
    assert!(loaded.manager_enabled);
    assert_eq!(loaded.rows.len(), 1);

    let failed = load_tasks_snapshot(|| async {
        Err::<Option<Vec<BackgroundTaskRecord>>, String>("journal offline".to_string())
    })
    .await;
    assert!(failed.manager_enabled);
    assert!(failed.rows.is_empty());
    assert_eq!(failed.load_error.as_deref(), Some("journal offline"));
}

#[test]
fn focusable_and_component_flags_match_other_panels() {
    let mut dialog = dialog(
        snapshot(vec![running_shell(NOW - 12_000)]),
        TasksDialogCallbacks::default(),
    );
    assert!(dialog.handles_input());
    assert!(!dialog.focused());
    dialog.set_focused(true);
    assert!(dialog.focused());
    assert!(dialog.as_focusable().is_some());
    assert!(dialog.as_any().is_some());
    assert!(dialog.width().is_none());

    // Component::render 走真实时钟路径，只要求不 panic 且有边框/正文。
    let lines = dialog.render(80);
    assert!(lines.len() >= 3);
}
