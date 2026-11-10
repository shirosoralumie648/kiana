//! `test/viewport-overwrite-repro.ts` 的手工脚本移植（`#[ignore]`）。
//!
//! TS 脚本用 `ProcessTerminal` + `TuiMainScreen`，在真实终端里按三段流式追加内容
//! （PRE-TOOL → 工具调用暂停 → TOOL OUT → POST-TOOL），用于肉眼确认"内容超过视口后新行不会
//! 覆盖上方旧行"。Rust 版保留同样的阶段、行数与延时，作为需要真实 TTY 的 `#[ignore]` 演示。
//!
//! **偏差（已记录）**：
//! - 只跑一遍并断言末尾的可见行里仍能找到所有 `PRE-TOOL` 行（自动化回归由
//!   `tests/tui_render.rs` / `tests/tui_alt_screen.rs` 的视口用例覆盖）；TS 原脚本没有断言，
//!   靠人眼看终端，这里把"预期行为"落成一条保守断言。
//! - `Lines.render` 的截断按字符数而非 UTF-16 码元（演示文本全为 ASCII，结果一致）。

use std::cell::RefCell;
use std::rc::Rc;
use std::thread::sleep;
use std::time::Duration;

use kiana_tui::main_screen::TuiMainScreen;
use kiana_tui::terminal::ProcessTerminal;
use kiana_tui::tui::{Component, Tui, TuiStopOptions};

/// TS `class Lines implements Component`：外部可 `set`/`append` 的行缓冲。
#[derive(Clone)]
struct Lines {
    lines: Rc<RefCell<Vec<String>>>,
}

impl Lines {
    fn new() -> Self {
        Lines {
            lines: Rc::new(RefCell::new(Vec::new())),
        }
    }

    fn set(&self, lines: &[&str]) {
        *self.lines.borrow_mut() = lines.iter().map(|line| (*line).to_string()).collect();
    }

    fn append(&self, lines: &[&str]) {
        self.lines
            .borrow_mut()
            .extend(lines.iter().map(|line| (*line).to_string()));
    }
}

impl Component for Lines {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.lines
            .borrow()
            .iter()
            .map(|line| {
                if line.chars().count() > width {
                    line.chars().take(width).collect()
                } else {
                    format!("{line:<width$}")
                }
            })
            .collect()
    }

    fn invalidate(&mut self) {}

    fn handles_input(&self) -> bool {
        false
    }
}

/// TS `streamLines(buffer, label, count, delayMs, ui)`。
fn stream_lines(buffer: &Lines, label: &str, count: usize, delay_ms: u64, ui: &mut TuiMainScreen) {
    for i in 1..=count {
        buffer.append(&[&format!("{label} {i:02}")]);
        ui.request_render(false);
        sleep(Duration::from_millis(delay_ms));
    }
}

#[test]
#[ignore = "manual demo: needs a real TTY (ProcessTerminal + raw mode)"]
fn viewport_does_not_overwrite_earlier_content_after_tool_output() {
    let mut ui = TuiMainScreen::new(Box::new(ProcessTerminal::new()));
    let buffer = Lines::new();
    ui.add_child(Box::new(buffer.clone()));
    ui.start();

    let height = ui.terminal().borrow().rows();
    let pre_count = height + 8; // 确保内容超过视口
    let tool_count = height + 12; // 工具输出把内容推得更深
    let post_count = 6;

    buffer.set(&[
        "TUI viewport overwrite repro",
        &format!("Viewport rows detected: {height}"),
        "(Resize to ~8-12 rows for best repro)",
        "",
        "=== PRE-TOOL STREAM ===",
    ]);
    ui.request_render(false);
    sleep(Duration::from_millis(300));

    // 阶段 1：流式追加 PRE-TOOL 文本直到超过视口。
    stream_lines(&buffer, "PRE-TOOL LINE", pre_count, 30, &mut ui);

    // 阶段 2：模拟工具调用暂停与工具输出。
    buffer.append(&["", "--- TOOL CALL START ---", "(pause...)", ""]);
    ui.request_render(false);
    sleep(Duration::from_millis(700));

    stream_lines(&buffer, "TOOL OUT", tool_count, 20, &mut ui);

    // 阶段 3：工具之后的流式输出（覆盖问题最常出现的阶段）。
    buffer.append(&["", "=== POST-TOOL STREAM ==="]);
    ui.request_render(false);
    sleep(Duration::from_millis(300));
    stream_lines(&buffer, "POST-TOOL LINE", post_count, 40, &mut ui);

    // 保留输出片刻，然后恢复终端状态。
    sleep(Duration::from_millis(1500));
    ui.stop(TuiStopOptions::default());

    // 预期行为：先前的 PRE-TOOL 行没有被后到的 POST-TOOL 行覆盖。
    let lines = buffer.lines.borrow();
    let pre_tool: Vec<&String> = lines
        .iter()
        .filter(|line| line.starts_with("PRE-TOOL LINE"))
        .collect();
    assert_eq!(pre_tool.len(), pre_count, "PRE-TOOL lines must all survive");
}
