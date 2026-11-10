//! `packages/tui/test/tui-render.test.ts` 的逐条移植（40 个 it）。
//!
//! TS 用 `@xterm/headless` 的 `VirtualTerminal` 做屏幕仿真，再从缓冲区断言行内容；
//! Rust 不引入 VT 模拟器依赖，本文件实现了一台**够用的**迷你仿真屏 [`VtScreen`]：
//!
//! - 只覆盖渲染器实际产出的序列：`\r` `\n`、CSI `A`/`B`/`G`/`H`/`K`/`J`、SGR（只跟踪斜体，
//!   用于 `resets styles after each rendered line`）、OSC/APC（整体跳过，含 Kitty 图形）；
//! - `getViewport()` 与 xterm 的 `translateToString(true)` 一致：**被显式写过的尾随空格保留**，
//!   从未写过的单元被裁掉（见 `probe1` 对照）；
//! - 支持滚动缓冲区：视口恒为缓冲区末尾 `rows` 行，resize 增加/减少行数时从滚动区补/取
//!   （与 xterm 的 viewportY 行为一致）。
//!
//! TS 的 `LoggingVirtualTerminal` 只覆写 `write()`，因此 `hideCursor`/`showCursor`/`stop()`
//! 等**不进入**写入日志；Rust 的 [`VtTerminal`] 保持同一语义（这些方法仍然作用于仿真屏）。
//!
//! 超大输出分块测试（`BoundedWriteTerminal`）不关心屏幕内容，只用写入日志。

use std::cell::RefCell;
use std::rc::Rc;

use kiana_tui::components::image::{Image, ImageOptions, ImageTheme};
use kiana_tui::components::input::{Input, InputOptions};
use kiana_tui::main_screen::TuiMainScreen;
use kiana_tui::terminal_colors::{ProgramStatus, Terminal};
use kiana_tui::terminal_image::{
    CellDimensions, ImageDimensions, ImageProtocol, KittyEncodeOptions, TerminalCapabilities,
    delete_kitty_image, encode_kitty, reset_capabilities_cache, set_capabilities,
    set_cell_dimensions,
};
use kiana_tui::text::slice_by_column;
use kiana_tui::tui::{
    CURSOR_MARKER, Component, MouseFocusTarget, OverlayOptions, RenderDecision, SizeValue, Tui,
    TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType, TuiStopOptions,
    render_fake_cursor,
};
use unicode_width::UnicodeWidthChar;

// ============================================================================
// 迷你 VT 仿真屏
// ============================================================================

/// 一个单元：字符 + 是否被写过 + 写入时的斜体状态。
#[derive(Clone, Copy, Default)]
struct VtCell {
    ch: char,
    set: bool,
    italic: bool,
}

/// 屏幕的一行。
#[derive(Clone, Default)]
struct VtLine {
    cells: Vec<VtCell>,
}

impl VtLine {
    fn ensure(&mut self, columns: usize) {
        if self.cells.len() < columns {
            self.cells.resize(columns, VtCell::default());
        }
    }

    /// xterm `translateToString(true)`：保留写过的空格，裁掉从未写过的单元。
    fn text(&self) -> String {
        let mut text = String::new();
        for cell in &self.cells {
            if !cell.set {
                break;
            }
            text.push(cell.ch);
        }
        text
    }

    fn clear(&mut self) {
        for cell in &mut self.cells {
            *cell = VtCell::default();
        }
    }
}

/// 一台够用的 VT 仿真屏（见模块文档）。
struct VtScreen {
    columns: usize,
    rows: usize,
    /// 完整缓冲区（含滚动区）；视口恒为末尾 `rows` 行。
    lines: Vec<VtLine>,
    cursor_row: usize,
    cursor_col: usize,
    /// xterm 的延迟换行：写满最后一列后光标停在 `columns`，下一个可打印字符才换行。
    pending_wrap: bool,
    italic: bool,
}

impl VtScreen {
    fn new(columns: usize, rows: usize) -> Self {
        VtScreen {
            columns,
            rows,
            lines: vec![VtLine::default(); rows],
            cursor_row: 0,
            cursor_col: 0,
            pending_wrap: false,
            italic: false,
        }
    }

    fn viewport_top(&self) -> usize {
        self.lines.len().saturating_sub(self.rows)
    }

    fn resize(&mut self, columns: usize, rows: usize) {
        self.columns = columns;
        self.rows = rows;
        if self.lines.len() < rows {
            self.lines.resize(rows, VtLine::default());
        }
        for line in &mut self.lines {
            line.ensure(columns);
        }
        if self.cursor_row >= self.lines.len() {
            self.cursor_row = self.lines.len() - 1;
        }
        self.cursor_col = self.cursor_col.min(columns);
    }

    fn viewport(&self) -> Vec<String> {
        let top = self.viewport_top();
        (0..self.rows)
            .map(|index| {
                self.lines
                    .get(top + index)
                    .map(VtLine::text)
                    .unwrap_or_default()
            })
            .collect()
    }

    fn cell_italic(&self, row: usize, col: usize) -> bool {
        let top = self.viewport_top();
        self.lines
            .get(top + row)
            .and_then(|line| line.cells.get(col))
            .is_some_and(|cell| cell.italic)
    }

    fn line_feed(&mut self) {
        if self.cursor_row + 1 >= self.lines.len() {
            self.lines.push(VtLine::default());
        }
        self.cursor_row += 1;
        if let Some(line) = self.lines.get_mut(self.cursor_row) {
            line.ensure(self.columns);
        }
    }

    fn put_char(&mut self, ch: char) {
        if self.pending_wrap {
            self.pending_wrap = false;
            self.cursor_col = 0;
            self.line_feed();
        }
        let width = ch.width().unwrap_or(0).max(1);
        let row = self.cursor_row;
        let col = self.cursor_col;
        let line = &mut self.lines[row];
        line.ensure(self.columns);
        if col < self.columns {
            line.cells[col] = VtCell {
                ch,
                set: true,
                italic: self.italic,
            };
            // 宽字符吃掉第二个单元（用一个"已写"的空格占位，输出时不再额外产生字符）。
            for extra in 1..width {
                if col + extra < self.columns {
                    line.cells[col + extra] = VtCell {
                        ch: ' ',
                        set: true,
                        italic: self.italic,
                    };
                }
            }
        }
        self.cursor_col += width;
        if self.cursor_col >= self.columns {
            self.cursor_col = self.columns;
            self.pending_wrap = true;
        }
    }

    fn erase_line(&mut self, mode: usize) {
        let row = self.cursor_row;
        let col = self.cursor_col;
        let line = &mut self.lines[row];
        line.ensure(self.columns);
        match mode {
            2 => line.clear(),
            1 => {
                let end = (col + 1).min(line.cells.len());
                for cell in line.cells.iter_mut().take(end) {
                    *cell = VtCell::default();
                }
            }
            _ => {
                if col < line.cells.len() {
                    for cell in line.cells.iter_mut().skip(col) {
                        *cell = VtCell::default();
                    }
                }
            }
        }
    }

    fn erase_display(&mut self, mode: usize) {
        match mode {
            2 => {
                let top = self.viewport_top();
                for line in self.lines.iter_mut().skip(top) {
                    line.clear();
                }
            }
            3 => {
                // 清滚动区：只保留视口内的行，光标行号相应左移。
                let top = self.viewport_top();
                self.lines.drain(..top);
                self.cursor_row -= top;
            }
            _ => {
                self.erase_line(2);
                let row = self.cursor_row;
                for line in self.lines.iter_mut().skip(row + 1) {
                    line.clear();
                }
            }
        }
    }

    fn move_up(&mut self, count: usize) {
        self.pending_wrap = false;
        let top = self.viewport_top();
        self.cursor_row = self.cursor_row.saturating_sub(count).max(top);
    }

    fn move_down(&mut self, count: usize) {
        self.pending_wrap = false;
        self.cursor_row = (self.cursor_row + count).min(self.lines.len().saturating_sub(1));
    }

    fn move_column(&mut self, column: usize) {
        self.pending_wrap = false;
        self.cursor_col = column.saturating_sub(1).min(self.columns.saturating_sub(1));
    }

    fn cursor_position(&mut self, row: usize, column: usize) {
        self.pending_wrap = false;
        let top = self.viewport_top();
        self.cursor_row = (top + row.saturating_sub(1)).min(self.lines.len().saturating_sub(1));
        self.cursor_col = column.saturating_sub(1).min(self.columns.saturating_sub(1));
    }

    fn apply_sgr(&mut self, params: &str) {
        for param in params.split(';') {
            match param {
                "" | "0" => self.italic = false,
                "3" => self.italic = true,
                "23" => self.italic = false,
                _ => {}
            }
        }
    }

    /// 解析并执行一段终端输出。
    fn write(&mut self, data: &str) {
        let bytes: Vec<char> = data.chars().collect();
        let mut index = 0;
        while index < bytes.len() {
            let ch = bytes[index];
            match ch {
                '\x1b' => {
                    index += 1;
                    let Some(&next) = bytes.get(index) else { break };
                    match next {
                        '[' => {
                            index += 1;
                            let start = index;
                            while index < bytes.len()
                                && !(('\x40'..='\x7e').contains(&bytes[index]))
                            {
                                index += 1;
                            }
                            let params: String = bytes[start..index].iter().collect();
                            if index < bytes.len() {
                                let final_byte = bytes[index];
                                index += 1;
                                self.dispatch_csi(final_byte, &params);
                            }
                        }
                        ']' | '_' | 'P' | '^' => {
                            // OSC/APC/PM：直到 BEL 或 ST（ESC \）。
                            index += 1;
                            while index < bytes.len() {
                                if bytes[index] == '\x07' {
                                    index += 1;
                                    break;
                                }
                                if bytes[index] == '\x1b' {
                                    index += 1;
                                    if index < bytes.len() {
                                        index += 1;
                                    }
                                    break;
                                }
                                index += 1;
                            }
                        }
                        _ => {
                            index += 1;
                        }
                    }
                }
                '\r' => {
                    self.cursor_col = 0;
                    self.pending_wrap = false;
                    index += 1;
                }
                '\n' => {
                    self.line_feed();
                    self.pending_wrap = false;
                    index += 1;
                }
                '\t' => {
                    let next_stop = (self.cursor_col / 8 + 1) * 8;
                    self.cursor_col = next_stop.min(self.columns.saturating_sub(1));
                    index += 1;
                }
                '\x08' => {
                    self.cursor_col = self.cursor_col.saturating_sub(1);
                    index += 1;
                }
                control if control.is_control() => {
                    index += 1;
                }
                printable => {
                    self.put_char(printable);
                    index += 1;
                }
            }
        }
    }

    fn dispatch_csi(&mut self, final_byte: char, params: &str) {
        let private = params.starts_with('?');
        let body = params.trim_start_matches('?');
        let numbers: Vec<usize> = body
            .split(';')
            .map(|value| value.parse::<usize>().unwrap_or(0))
            .collect();
        let first = numbers.first().copied().unwrap_or(0);
        match final_byte {
            // 私有模式（`?2026h` / `?25l` / `?2004h` …）：对屏幕内容无影响。
            'h' | 'l' if private => {}
            'm' => self.apply_sgr(body),
            'A' => self.move_up(first.max(1)),
            'B' => self.move_down(first.max(1)),
            'C' => {
                let count = first.max(1);
                self.cursor_col = (self.cursor_col + count).min(self.columns.saturating_sub(1));
            }
            'D' => {
                let count = first.max(1);
                self.cursor_col = self.cursor_col.saturating_sub(count);
            }
            'G' => self.move_column(first.max(1)),
            'H' | 'f' => {
                let row = numbers.first().copied().unwrap_or(1).max(1);
                let column = numbers.get(1).copied().unwrap_or(1).max(1);
                self.cursor_position(row, column);
            }
            'K' => self.erase_line(first),
            'J' => self.erase_display(first),
            _ => {}
        }
    }
}

// ============================================================================
// 测试终端
// ============================================================================

/// TS `VirtualTerminal` + `LoggingVirtualTerminal` 的 Rust 等价物。
/// 终端输入回调（`Terminal::start` 的 `on_input`）。
type InputHandler = Box<dyn FnMut(&str)>;
/// resize 回调。
type ResizeHandler = Box<dyn FnMut()>;

struct VtShared {
    columns: usize,
    rows: usize,
    screen: VtScreen,
    /// 只记录 `write()` 的写入（TS 的 `LoggingVirtualTerminal` 只覆写 `write`）。
    writes: Vec<String>,
    input_handler: Option<InputHandler>,
    resize_handler: Option<ResizeHandler>,
}

struct VtTerminal {
    shared: Rc<RefCell<VtShared>>,
}

impl VtTerminal {
    fn new(columns: usize, rows: usize) -> Self {
        VtTerminal {
            shared: Rc::new(RefCell::new(VtShared {
                columns,
                rows,
                screen: VtScreen::new(columns, rows),
                writes: Vec::new(),
                input_handler: None,
                resize_handler: None,
            })),
        }
    }

    fn handle(&self) -> Rc<RefCell<VtShared>> {
        Rc::clone(&self.shared)
    }
}

/// 测试侧的观察入口。
struct Screen(Rc<RefCell<VtShared>>);

impl Screen {
    fn viewport(&self) -> Vec<String> {
        self.0.borrow().screen.viewport()
    }

    fn writes(&self) -> String {
        self.0.borrow().writes.join("")
    }

    fn clear_writes(&self) {
        self.0.borrow_mut().writes.clear();
    }

    fn cell_italic(&self, row: usize, col: usize) -> bool {
        self.0.borrow().screen.cell_italic(row, col)
    }

    fn rows(&self) -> usize {
        self.0.borrow().rows
    }

    fn send_input(&self, data: &str) {
        let mut shared = self.0.borrow_mut();
        if let Some(handler) = shared.input_handler.as_mut() {
            handler(data);
        }
    }

    fn resize(&self, columns: usize, rows: usize) {
        let mut shared = self.0.borrow_mut();
        shared.columns = columns;
        shared.rows = rows;
        shared.screen.resize(columns, rows);
        if let Some(handler) = shared.resize_handler.as_mut() {
            handler();
        }
    }

    /// TS `waitForRender()` 的等价物：等排队的帧到期后渲染它。
    ///
    /// TS 的实现是 `await nextTick(); await sleep(20); await flush()`：**只有确实被排队**
    /// （`start` / `requestRender` / overlay 变更等）的帧才会被渲染。这里同样只在
    /// [`TuiMainScreen::render_decision`] 给出 `Now`（立即路径）或 `Wait`（节流路径到期）时
    /// 渲染；`Idle` 时什么也不做——这样测试能发现"该请求的帧没有请求"的回归，而不是用一个
    /// 无条件的 `renderNow()` 把它掩盖过去。
    fn wait_for_render(&self, tui: &mut TuiMainScreen) {
        match tui.base_mut().render_decision() {
            RenderDecision::Idle => {}
            RenderDecision::Now => tui.render_now(false),
            RenderDecision::Wait(delay_ms) => {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms.ceil() as u64 + 1));
                tui.render_now(false);
            }
        }
    }
}

impl Terminal for VtTerminal {
    fn start(&mut self, on_input: Box<dyn FnMut(&str)>, on_resize: Box<dyn FnMut()>) {
        let mut shared = self.shared.borrow_mut();
        shared.input_handler = Some(on_input);
        shared.resize_handler = Some(on_resize);
        // TS `VirtualTerminal.start` 写 bracketed paste，但那是 `xterm.write()`，不进写入日志。
        shared.screen.write("\x1b[?2004h");
    }

    fn stop(&mut self) {
        let mut shared = self.shared.borrow_mut();
        shared.screen.write("\x1b[?2004l");
        shared.input_handler = None;
        shared.resize_handler = None;
    }

    fn drain_input(
        &mut self,
        _max_ms: u64,
        _idle_ms: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>> {
        Box::pin(async {})
    }

    fn write(&mut self, data: &str) {
        let mut shared = self.shared.borrow_mut();
        shared.writes.push(data.to_string());
        shared.screen.write(data);
    }

    fn columns(&self) -> usize {
        self.shared.borrow().columns
    }

    fn rows(&self) -> usize {
        self.shared.borrow().rows
    }

    fn kitty_protocol_active(&self) -> bool {
        true
    }

    fn move_by(&mut self, lines: isize) {
        if lines > 0 {
            self.shared
                .borrow_mut()
                .screen
                .write(&format!("\x1b[{lines}B"));
        } else if lines < 0 {
            self.shared
                .borrow_mut()
                .screen
                .write(&format!("\x1b[{}A", -lines));
        }
    }

    fn hide_cursor(&mut self) {
        self.shared.borrow_mut().screen.write("\x1b[?25l");
    }

    fn show_cursor(&mut self) {
        self.shared.borrow_mut().screen.write("\x1b[?25h");
    }

    fn clear_line(&mut self) {
        self.shared.borrow_mut().screen.write("\x1b[K");
    }

    fn clear_from_cursor(&mut self) {
        self.shared.borrow_mut().screen.write("\x1b[J");
    }

    fn clear_screen(&mut self) {
        self.shared.borrow_mut().screen.write("\x1b[2J\x1b[H");
    }

    fn set_title(&mut self, title: &str) {
        self.shared
            .borrow_mut()
            .screen
            .write(&format!("\x1b]0;{title}\x07"));
    }

    fn set_progress(&mut self, _active: bool) {}

    fn set_program_status(&mut self, _status: &ProgramStatus) {}
}

/// TS `BoundedWriteTerminal`：只记录写入，不做屏幕仿真。
struct BoundedWriteTerminal {
    writes: Rc<RefCell<Vec<String>>>,
    columns: usize,
    rows: usize,
}

impl BoundedWriteTerminal {
    fn new(columns: usize, rows: usize) -> Self {
        BoundedWriteTerminal {
            writes: Rc::new(RefCell::new(Vec::new())),
            columns,
            rows,
        }
    }

    fn handle(&self) -> Rc<RefCell<Vec<String>>> {
        Rc::clone(&self.writes)
    }
}

impl Terminal for BoundedWriteTerminal {
    fn start(&mut self, _on_input: Box<dyn FnMut(&str)>, _on_resize: Box<dyn FnMut()>) {}
    fn stop(&mut self) {}
    fn drain_input(
        &mut self,
        _max_ms: u64,
        _idle_ms: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>> {
        Box::pin(async {})
    }

    fn write(&mut self, data: &str) {
        self.writes.borrow_mut().push(data.to_string());
    }

    fn columns(&self) -> usize {
        self.columns
    }

    fn rows(&self) -> usize {
        self.rows
    }

    fn kitty_protocol_active(&self) -> bool {
        false
    }

    fn move_by(&mut self, _lines: isize) {}
    fn hide_cursor(&mut self) {}
    fn show_cursor(&mut self) {}
    fn clear_line(&mut self) {}
    fn clear_from_cursor(&mut self) {}
    fn clear_screen(&mut self) {}
    fn set_title(&mut self, _title: &str) {}
    fn set_progress(&mut self, _active: bool) {}
    fn set_program_status(&mut self, _status: &ProgramStatus) {}
}

// ============================================================================
// 组件桩
// ============================================================================

/// TS `TestComponent`：固定行，可随时替换。
struct TestComponent {
    lines: Vec<String>,
}

impl TestComponent {
    fn new(lines: &[&str]) -> Self {
        TestComponent {
            lines: lines.iter().map(|line| (*line).to_string()).collect(),
        }
    }
}

impl Component for TestComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.lines.clone()
    }

    fn handles_input(&self) -> bool {
        false
    }
}

/// TS `InputComponent`：记录渲染次数，收到输入后把输入当作唯一一行。
struct InputComponent {
    lines: Vec<String>,
    render_count: usize,
}

impl InputComponent {
    fn new(lines: &[&str]) -> Self {
        InputComponent {
            lines: lines.iter().map(|line| (*line).to_string()).collect(),
            render_count: 0,
        }
    }
}

impl Component for InputComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.render_count += 1;
        self.lines.clone()
    }

    fn handle_input(&mut self, data: &str) {
        self.lines = vec![data.to_string()];
    }
}

/// 用 `Rc<RefCell<…>>` 共享给测试观察的可变组件。
struct SharedComponent {
    lines: Rc<RefCell<Vec<String>>>,
}

impl Component for SharedComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.lines.borrow().clone()
    }

    fn handles_input(&self) -> bool {
        false
    }
}

// ============================================================================
// 辅助
// ============================================================================

const MAX_RENDER_WRITE_CHARS: usize = 1024 * 1024;

fn lines_of(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

// ============================================================================
// 测试：渲染调度 / 调试日志 / 分块输出
// ============================================================================

#[test]
fn renders_keyboard_input_without_waiting_for_a_throttled_frame() {
    let terminal = VtTerminal::new(40, 10);
    let screen = Screen(terminal.handle());
    let mut tui = TuiMainScreen::new(Box::new(terminal));
    // 固定时钟：任何节流窗口都"未到期"，只有立即路径能渲染。
    tui.base_mut().set_clock(Box::new(|| 0.0));
    let component = Rc::new(RefCell::new(InputComponent::new(&["initial"])));
    tui.add_child(Box::new(SharedInputComponent(Rc::clone(&component))));
    tui.set_focus(Some(kiana_tui::tui::FocusTarget::Component { index: 0 }));
    tui.start();
    tui.render_now(false);
    let render_count_before_input = component.borrow().render_count;

    // 先排一个普通（节流）渲染，键盘输入必须抢占它。
    component.borrow_mut().lines = vec!["pending".to_string()];
    tui.request_render(false);
    screen.send_input("first");
    screen.send_input("second");
    screen.send_input("typed");
    tui.base_mut().pump_pending_input();

    // 输入走立即路径：调度决策为 Now（不是 Wait），且只渲染一次。固定时钟下节流窗口
    // 永远未到期，`flush_pending_render()` 返回 true 就证明输入没有等节流定时器。
    assert!(
        tui.flush_pending_render(),
        "keyboard input must render immediately, not wait for the throttle"
    );
    assert_eq!(
        component.borrow().render_count,
        render_count_before_input + 1
    );
    assert_eq!(component.borrow().lines, vec!["typed".to_string()]);
    // 三次输入 + 一个已排队的节流帧必须合并成这一帧，不能留下第二个待渲染请求。
    assert!(
        !tui.flush_pending_render(),
        "input must coalesce into a single frame (TS nextTick renders once)"
    );
    tui.stop(TuiStopOptions::default());
}

/// `InputComponent` 的共享句柄包装（只用于上面的调度测试）。
struct SharedInputComponent(Rc<RefCell<InputComponent>>);

impl Component for SharedInputComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    fn handle_input(&mut self, data: &str) {
        self.0.borrow_mut().handle_input(data);
    }
}

#[test]
fn writes_redraw_logs_to_the_provided_directory() {
    let log_dir = std::env::temp_dir().join(format!(
        "kiana-tui-log-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&log_dir).unwrap();
    // 环境变量在进程内是全局的：用互斥锁隔离（Rust 测试并行跑）。
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _guard = EnvGuard::set(&[("KIANA_TUI_DEBUG_REDRAW", Some("1"))]);

    let terminal = VtTerminal::new(40, 10);
    let screen = Screen(terminal.handle());
    let mut tui = TuiMainScreen::with_options(
        Box::new(terminal),
        None,
        Some(log_dir.to_string_lossy().into_owned()),
    );
    tui.add_child(Box::new(TestComponent::new(&["test"])));
    tui.start();
    screen.wait_for_render(&mut tui);

    let log = std::fs::read_to_string(log_dir.join("kiana-tui-debug.log")).unwrap();
    assert!(
        log.contains("fullRender: first render"),
        "unexpected log: {log}"
    );
    tui.stop(TuiStopOptions::default());

    let _ = std::fs::remove_dir_all(&log_dir);
}

#[test]
fn splits_a_large_full_render_without_changing_its_output() {
    let terminal = BoundedWriteTerminal::new(80, 24);
    let writes = terminal.handle();
    let mut tui = TuiMainScreen::new(Box::new(terminal));
    let kitty_line = format!("\x1b_Ga=T,f=100;{}\x1b\\", "A".repeat(1_200_000));
    tui.add_child(Box::new(TestComponent {
        lines: vec![kitty_line.clone(), kitty_line.clone()],
    }));

    tui.render_now(false);

    let writes = writes.borrow();
    assert!(
        writes.len() > 2,
        "large output should be split across terminal writes"
    );
    assert!(
        writes
            .iter()
            .all(|write| write.encode_utf16().count() <= MAX_RENDER_WRITE_CHARS),
        "each terminal write should stay below the configured limit"
    );
    assert_eq!(
        writes.join(""),
        format!("\x1b[?2026h{kitty_line}\r\n{kitty_line}\x1b[?2026l"),
        "chunking must preserve the synchronized render output"
    );
}

#[test]
fn splits_large_differential_updates_without_a_full_redraw() {
    let terminal = BoundedWriteTerminal::new(80, 24);
    let writes = terminal.handle();
    let mut tui = TuiMainScreen::new(Box::new(terminal));
    let component = SharedComponent {
        lines: Rc::new(RefCell::new(lines_of(&["before"]))),
    };
    let handle = Rc::clone(&component.lines);
    tui.add_child(Box::new(component));
    tui.render_now(false);
    writes.borrow_mut().clear();

    let kitty_line = format!("\x1b_Ga=T,f=100;{}\x1b\\", "A".repeat(1_200_000));
    *handle.borrow_mut() = vec!["before".to_string(), kitty_line.clone(), kitty_line.clone()];
    tui.render_now(false);

    let writes = writes.borrow();
    assert!(
        writes.len() > 2,
        "large output should be split across terminal writes"
    );
    assert!(
        writes
            .iter()
            .all(|write| write.encode_utf16().count() <= MAX_RENDER_WRITE_CHARS)
    );
    let output = writes.join("");
    assert!(output.starts_with("\x1b[?2026h"));
    assert!(output.ends_with("\x1b[?2026l"));
    assert!(
        !output.contains("\x1b[2J"),
        "the update should stay on the differential render path"
    );
}

// ============================================================================
// 辅助：环境变量、图片与 overlay
// ============================================================================

/// 环境变量在进程内是全局的：会改环境（或被环境影响）的测试都在此锁下串行。
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 设置若干环境变量，`Drop` 时还原（TS `withEnv` / `overrideEnv`）。
struct EnvGuard {
    restore: Vec<(String, Option<String>)>,
}

impl EnvGuard {
    fn set(updates: &[(&str, Option<&str>)]) -> Self {
        let mut restore = Vec::new();
        for (name, value) in updates {
            restore.push(((*name).to_string(), std::env::var(name).ok()));
            match value {
                Some(value) => unsafe { std::env::set_var(name, value) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
        EnvGuard { restore }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in self.restore.drain(..) {
            match value {
                Some(value) => unsafe { std::env::set_var(&name, value) },
                None => unsafe { std::env::remove_var(&name) },
            }
        }
    }
}

/// 建一个带屏幕观察入口的 `TuiMainScreen`。
fn make_tui(columns: usize, rows: usize) -> (TuiMainScreen, Screen) {
    let terminal = VtTerminal::new(columns, rows);
    let screen = Screen(terminal.handle());
    (TuiMainScreen::new(Box::new(terminal)), screen)
}

fn make_tui_with(
    columns: usize,
    rows: usize,
    show_hardware_cursor: Option<bool>,
    log_directory: Option<String>,
) -> (TuiMainScreen, Screen) {
    let terminal = VtTerminal::new(columns, rows);
    let screen = Screen(terminal.handle());
    (
        TuiMainScreen::with_options(Box::new(terminal), show_hardware_cursor, log_directory),
        screen,
    )
}

fn with_kitty_image_env(run: impl FnOnce()) {
    set_capabilities(TerminalCapabilities {
        images: Some(ImageProtocol::Kitty),
        true_color: true,
        hyperlinks: true,
    });
    set_cell_dimensions(CellDimensions {
        width_px: 10.0,
        height_px: 10.0,
    });
    run();
    reset_capabilities_cache();
    set_cell_dimensions(CellDimensions {
        width_px: 9.0,
        height_px: 18.0,
    });
}

fn kitty_image(max_width_cells: f64, width_px: f64, height_px: f64) -> Image {
    Image::new(
        "AAAA",
        "image/png",
        ImageTheme::plain(),
        ImageOptions {
            max_width_cells: Some(max_width_cells),
            ..ImageOptions::default()
        },
        Some(ImageDimensions {
            width_px,
            height_px,
        }),
    )
}

fn overlay_at(row: f64, col: f64, width: f64) -> OverlayOptions {
    OverlayOptions {
        row: Some(SizeValue::Absolute(row)),
        col: Some(SizeValue::Absolute(col)),
        width: Some(SizeValue::Absolute(width)),
        ..OverlayOptions::default()
    }
}

// ============================================================================
// 测试：未配置日志目录时的 crash 转储
// ============================================================================

#[test]
fn writes_the_crash_dump_to_the_os_temp_directory() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let crash_dir = std::env::temp_dir().join(format!(
        "kiana-tui-crash-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&crash_dir).unwrap();
    let crash_log_path = crash_dir.join("kiana-tui-crash.log");
    let crash_dir_text = crash_dir.to_string_lossy().into_owned();
    let _guard = EnvGuard::set(&[
        ("TMPDIR", Some(crash_dir_text.as_str())),
        ("TEMP", Some(crash_dir_text.as_str())),
        ("TMP", Some(crash_dir_text.as_str())),
    ]);

    let (mut tui, screen) = make_tui(40, 10);
    let component = SharedComponent {
        lines: Rc::new(RefCell::new(lines_of(&["ok"]))),
    };
    let handle = Rc::clone(&component.lines);
    tui.add_child(Box::new(component));
    tui.start();
    screen.wait_for_render(&mut tui);

    // 超宽行在差分渲染路径上被发现。
    *handle.borrow_mut() = lines_of(&["ok", &"x".repeat(60)]);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tui.render_now(false);
    }));
    let payload = result.expect_err("render should panic on an over-wide line");
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
        .expect("panic payload should be a string");
    assert!(
        message.contains(&crash_log_path.to_string_lossy().into_owned()),
        "error message should reference {}: {message}",
        crash_log_path.display()
    );
    let crash_log = std::fs::read_to_string(&crash_log_path).unwrap();
    assert!(
        crash_log.contains("Terminal width: 40"),
        "unexpected crash log: {crash_log}"
    );
    // TS 把 `[...lines, ""]` 用 "\n" join：文件以**一个**换行结束（不是空行）。
    assert!(
        crash_log.ends_with('\n'),
        "crash log must end with a newline"
    );
    assert!(
        !crash_log.ends_with("\n\n"),
        "crash log must not end with a blank line: {:?}",
        &crash_log[crash_log.len().saturating_sub(8)..]
    );
    let _ = std::fs::remove_dir_all(&crash_dir);
}

/// `logDirectory` 走 `path.join`（折叠 `.` 与重复分隔符），错误消息里的路径也是规范化后的。
#[test]
fn crash_log_path_joins_log_directory_like_node() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let base = std::env::temp_dir().join(format!(
        "kiana-tui-crash-join-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&base).unwrap();
    // Node：path.join("<base>/./logs//", "kiana-tui-crash.log") = "<base>/logs/kiana-tui-crash.log"。
    let log_directory = format!("{}/./logs//", base.display());
    let expected = format!("{}/logs/kiana-tui-crash.log", base.display());

    let (mut tui, screen) = make_tui_with(40, 10, None, Some(log_directory.clone()));
    let component = SharedComponent {
        lines: Rc::new(RefCell::new(lines_of(&["ok"]))),
    };
    let handle = Rc::clone(&component.lines);
    tui.add_child(Box::new(component));
    tui.start();
    screen.wait_for_render(&mut tui);
    *handle.borrow_mut() = lines_of(&["ok", &"x".repeat(60)]);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tui.render_now(false);
    }));
    let payload = result.expect_err("render should panic on an over-wide line");
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
        .expect("panic payload should be a string");
    assert!(
        message.contains(&format!("Debug log written to: {expected}")),
        "error message should reference the normalized path {expected}: {message}"
    );
    let crash_log = std::fs::read_to_string(&expected).unwrap();
    assert!(crash_log.ends_with('\n') && !crash_log.ends_with("\n\n"));
    let _ = std::fs::remove_dir_all(&base);
}

// ============================================================================
// 测试：Kitty 图片清理
// ============================================================================

#[test]
fn clears_reserved_kitty_image_rows_before_drawing_appended_image_placements() {
    with_kitty_image_env(|| {
        let (mut tui, screen) = make_tui(40, 10);
        let component = SharedComponent {
            lines: Rc::new(RefCell::new(lines_of(&["before"]))),
        };
        let handle = Rc::clone(&component.lines);
        tui.add_child(Box::new(component));

        tui.start();
        screen.wait_for_render(&mut tui);
        screen.clear_writes();

        let mut image = kitty_image(2.0, 20.0, 20.0);
        let image_lines = image.render(40);
        let image_sequence = image_lines[0].clone();
        let mut next = lines_of(&["before"]);
        next.extend(image_lines.iter().cloned());
        next.push("after".to_string());
        *handle.borrow_mut() = next;
        tui.request_render(false);
        screen.wait_for_render(&mut tui);

        let writes = screen.writes();
        assert!(
            writes.contains(&format!("\x1b[2K\r\n\x1b[2K\x1b[1A{image_sequence}\x1b[1B")),
            "reserved rows should be cleared before the image placement is drawn: {writes:?}"
        );
        assert!(
            !writes.contains(&format!("{image_sequence}\r\n\x1b[2K")),
            "reserved row clears must not run after the image placement is drawn"
        );
        tui.stop(TuiStopOptions::default());
    });
}

#[test]
fn falls_back_to_full_redraw_when_kitty_image_pre_clear_would_scroll() {
    with_kitty_image_env(|| {
        let (mut tui, screen) = make_tui(40, 2);
        let component = SharedComponent {
            lines: Rc::new(RefCell::new(lines_of(&["before"]))),
        };
        let handle = Rc::clone(&component.lines);
        tui.add_child(Box::new(component));

        tui.start();
        screen.wait_for_render(&mut tui);
        let redraws_before_image = tui.full_redraws();
        screen.clear_writes();

        let mut image = kitty_image(3.0, 30.0, 30.0);
        let mut next = lines_of(&["before"]);
        next.extend(image.render(40));
        next.push("after".to_string());
        *handle.borrow_mut() = next;
        tui.request_render(false);
        screen.wait_for_render(&mut tui);

        assert!(
            tui.full_redraws() > redraws_before_image,
            "unsafe image pre-clear should force a full redraw"
        );
        assert!(
            screen.writes().contains("\x1b[2J"),
            "fallback should clear and fully redraw"
        );
        tui.stop(TuiStopOptions::default());
    });
}

#[test]
fn reserves_kitty_image_rows_before_drawing_during_full_redraw_fallbacks() {
    with_kitty_image_env(|| {
        let (mut tui, screen) = make_tui(40, 5);
        let component = SharedComponent {
            lines: Rc::new(RefCell::new(lines_of(&["l0", "l1", "l2", "l3", "l4"]))),
        };
        let handle = Rc::clone(&component.lines);
        tui.add_child(Box::new(component));

        tui.start();
        screen.wait_for_render(&mut tui);
        let redraws_before_image = tui.full_redraws();
        screen.clear_writes();

        let mut image = kitty_image(3.0, 30.0, 30.0);
        let image_lines = image.render(40);
        let image_sequence = image_lines[0].clone();
        let mut next = lines_of(&["l0", "l1", "l2", "l3", "l4"]);
        next.extend(image_lines.iter().cloned());
        next.push("after".to_string());
        *handle.borrow_mut() = next;
        tui.request_render(false);
        screen.wait_for_render(&mut tui);

        let writes = screen.writes();
        assert!(
            tui.full_redraws() > redraws_before_image,
            "scrolling image append should force a full redraw"
        );
        assert!(
            writes.contains(&format!("\r\n\r\n\x1b[2A{image_sequence}\x1b[2B")),
            "full redraw should reserve visible image rows before drawing the placement: {writes:?}"
        );
        assert!(
            !writes.contains(&format!("{image_sequence}\r\n\x1b[0m")),
            "full redraw must not write reserved padding rows after drawing the placement"
        );
        tui.stop(TuiStopOptions::default());
    });
}

#[test]
fn does_not_use_cursor_up_placement_for_kitty_images_taller_than_the_viewport() {
    with_kitty_image_env(|| {
        let (mut tui, screen) = make_tui(40, 5);
        let component = SharedComponent {
            lines: Rc::new(RefCell::new(lines_of(&["before"]))),
        };
        let handle = Rc::clone(&component.lines);
        tui.add_child(Box::new(component));

        tui.start();
        screen.wait_for_render(&mut tui);
        screen.clear_writes();

        let mut image = kitty_image(6.0, 60.0, 60.0);
        let image_lines = image.render(40);
        let image_sequence = image_lines[0].clone();
        assert!(
            image_lines.len() > screen.rows(),
            "test image should exceed the viewport height"
        );

        let mut next = lines_of(&["before"]);
        next.extend(image_lines.iter().cloned());
        next.push("after".to_string());
        *handle.borrow_mut() = next;
        tui.request_render(true);
        screen.wait_for_render(&mut tui);

        let writes = screen.writes();
        assert!(
            writes.contains(&image_sequence),
            "image placement should be drawn"
        );
        assert!(
            !writes.contains(&format!("\x1b[{}A{image_sequence}", image_lines.len() - 1)),
            "taller-than-viewport images must keep the #4461 first-row placement path"
        );
        tui.stop(TuiStopOptions::default());
    });
}

#[test]
fn deletes_changed_image_ids_before_drawing_moved_placements() {
    let (mut tui, screen) = make_tui(40, 10);
    let component = SharedComponent {
        lines: Rc::new(RefCell::new(Vec::new())),
    };
    let handle = Rc::clone(&component.lines);
    tui.add_child(Box::new(component));

    let old_image = encode_kitty(
        "AAAA",
        KittyEncodeOptions {
            columns: Some(2.0),
            rows: Some(2.0),
            image_id: Some(42.0),
            move_cursor: Some(false),
        },
    );
    *handle.borrow_mut() = vec!["top".to_string(), old_image];
    tui.start();
    screen.wait_for_render(&mut tui);
    screen.clear_writes();

    let new_image = encode_kitty(
        "BBBB",
        KittyEncodeOptions {
            columns: Some(2.0),
            rows: Some(1.0),
            image_id: Some(42.0),
            move_cursor: Some(false),
        },
    );
    *handle.borrow_mut() = vec![new_image.clone(), String::new()];
    tui.request_render(false);
    screen.wait_for_render(&mut tui);

    let writes = screen.writes();
    let delete_index = writes.find(&delete_kitty_image(42));
    let draw_index = writes.find(&new_image);
    assert!(
        delete_index.is_some(),
        "changed old image should be deleted"
    );
    assert!(draw_index.is_some(), "new image should be drawn");
    assert!(
        delete_index.unwrap() < draw_index.unwrap(),
        "old image must be deleted before the new placement is drawn"
    );
    tui.stop(TuiStopOptions::default());
}

#[test]
fn redraws_image_lines_when_an_earlier_reserved_image_row_changes() {
    let (mut tui, screen) = make_tui(40, 10);
    let component = SharedComponent {
        lines: Rc::new(RefCell::new(Vec::new())),
    };
    let handle = Rc::clone(&component.lines);
    tui.add_child(Box::new(component));

    let image = encode_kitty(
        "AAAA",
        KittyEncodeOptions {
            columns: Some(2.0),
            rows: Some(2.0),
            image_id: Some(88.0),
            move_cursor: Some(false),
        },
    );
    *handle.borrow_mut() = vec![String::new(), image.clone()];
    tui.start();
    screen.wait_for_render(&mut tui);
    screen.clear_writes();

    *handle.borrow_mut() = vec!["covered".to_string(), image.clone()];
    tui.request_render(false);
    screen.wait_for_render(&mut tui);

    let writes = screen.writes();
    let delete_index = writes.find(&delete_kitty_image(88));
    let draw_index = writes.find(&image);
    assert!(
        delete_index.is_some(),
        "image should be deleted when a reserved row changes"
    );
    assert!(
        draw_index.is_some(),
        "unchanged image line should be redrawn after deleting the placement"
    );
    assert!(
        delete_index.unwrap() < draw_index.unwrap(),
        "old placement must be deleted before the image line is redrawn"
    );
    assert!(
        !writes.contains("\x1b[2J"),
        "reserved row changes should not force a full redraw"
    );
    tui.stop(TuiStopOptions::default());
}

#[test]
fn deletes_previously_rendered_image_ids_during_full_redraws() {
    let (mut tui, screen) = make_tui(40, 10);
    let component = SharedComponent {
        lines: Rc::new(RefCell::new(Vec::new())),
    };
    let handle = Rc::clone(&component.lines);
    tui.add_child(Box::new(component));

    *handle.borrow_mut() = vec![encode_kitty(
        "AAAA",
        KittyEncodeOptions {
            columns: Some(2.0),
            rows: Some(2.0),
            image_id: Some(77.0),
            move_cursor: Some(false),
        },
    )];
    tui.start();
    screen.wait_for_render(&mut tui);
    screen.clear_writes();

    *handle.borrow_mut() = lines_of(&["plain text"]);
    tui.request_render(true);
    screen.wait_for_render(&mut tui);

    let writes = screen.writes();
    let delete_index = writes.find(&delete_kitty_image(77));
    let clear_index = writes.find("\x1b[2J");
    assert!(
        delete_index.is_some(),
        "previous image should be deleted during full redraw"
    );
    assert!(clear_index.is_some(), "full redraw should clear the screen");
    assert!(
        delete_index.unwrap() < clear_index.unwrap(),
        "old image should be deleted before the screen is cleared"
    );
    tui.stop(TuiStopOptions::default());
}

// ============================================================================
// 测试：resize
// ============================================================================

#[test]
fn triggers_full_re_render_when_terminal_height_changes() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _guard = EnvGuard::set(&[("TERMUX_VERSION", None)]);

    let (mut tui, screen) = make_tui(40, 10);
    tui.add_child(Box::new(TestComponent::new(&[
        "Line 0", "Line 1", "Line 2",
    ])));
    tui.start();
    screen.wait_for_render(&mut tui);

    let initial_redraws = tui.full_redraws();
    screen.resize(40, 15);
    screen.wait_for_render(&mut tui);

    assert!(
        tui.full_redraws() > initial_redraws,
        "Height change should trigger full redraw"
    );
    let viewport = screen.viewport();
    assert!(
        viewport[0].contains("Line 0"),
        "Content preserved after height change"
    );
    tui.stop(TuiStopOptions::default());
}

#[test]
fn skips_full_re_render_on_height_changes_in_termux() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _guard = EnvGuard::set(&[("TERMUX_VERSION", Some("1"))]);

    let (mut tui, screen) = make_tui(40, 10);
    let long_lines: Vec<String> = (0..20).map(|i| format!("Line {i}")).collect();
    let component = SharedComponent {
        lines: Rc::new(RefCell::new(long_lines)),
    };
    tui.add_child(Box::new(component));
    tui.start();
    screen.wait_for_render(&mut tui);
    screen.clear_writes();

    let initial_redraws = tui.full_redraws();
    for height in [15, 8, 14, 11] {
        screen.resize(40, height);
        screen.wait_for_render(&mut tui);
    }

    assert_eq!(
        tui.full_redraws(),
        initial_redraws,
        "Height change should not trigger full redraw"
    );
    assert!(
        !screen.writes().contains("\x1b[2J"),
        "Height change should not clear the screen"
    );
    assert!(
        !screen.writes().contains("\x1b[3J"),
        "Height change should not clear scrollback"
    );
    let viewport = screen.viewport();
    assert!(
        viewport.join("\n").contains("Line 19"),
        "Latest content remains visible after resize"
    );
    tui.stop(TuiStopOptions::default());
}

#[test]
fn triggers_full_re_render_when_terminal_width_changes() {
    let (mut tui, screen) = make_tui(40, 10);
    tui.add_child(Box::new(TestComponent::new(&[
        "Line 0", "Line 1", "Line 2",
    ])));
    tui.start();
    screen.wait_for_render(&mut tui);

    let initial_redraws = tui.full_redraws();
    screen.resize(60, 10);
    screen.wait_for_render(&mut tui);

    assert!(
        tui.full_redraws() > initial_redraws,
        "Width change should trigger full redraw"
    );
    tui.stop(TuiStopOptions::default());
}

// ============================================================================
// 测试：内容收缩
// ============================================================================

#[test]
fn clears_empty_rows_when_content_shrinks_significantly() {
    let (mut tui, screen) = make_tui(40, 10);
    tui.base_mut().set_clear_on_shrink(true);
    let component = SharedComponent {
        lines: Rc::new(RefCell::new(lines_of(&[
            "Line 0", "Line 1", "Line 2", "Line 3", "Line 4", "Line 5",
        ]))),
    };
    let handle = Rc::clone(&component.lines);
    tui.add_child(Box::new(component));
    tui.start();
    screen.wait_for_render(&mut tui);

    let initial_redraws = tui.full_redraws();
    *handle.borrow_mut() = lines_of(&["Line 0", "Line 1"]);
    tui.request_render(false);
    screen.wait_for_render(&mut tui);

    assert!(
        tui.full_redraws() > initial_redraws,
        "Content shrinkage should trigger full redraw"
    );
    let viewport = screen.viewport();
    assert!(viewport[0].contains("Line 0"), "First line preserved");
    assert!(viewport[1].contains("Line 1"), "Second line preserved");
    assert_eq!(viewport[2].trim(), "", "Line 2 should be cleared");
    assert_eq!(viewport[3].trim(), "", "Line 3 should be cleared");
    tui.stop(TuiStopOptions::default());
}

#[test]
fn handles_shrink_to_single_line() {
    let (mut tui, screen) = make_tui(40, 10);
    tui.base_mut().set_clear_on_shrink(true);
    let component = SharedComponent {
        lines: Rc::new(RefCell::new(lines_of(&[
            "Line 0", "Line 1", "Line 2", "Line 3",
        ]))),
    };
    let handle = Rc::clone(&component.lines);
    tui.add_child(Box::new(component));
    tui.start();
    screen.wait_for_render(&mut tui);

    *handle.borrow_mut() = lines_of(&["Only line"]);
    tui.request_render(false);
    screen.wait_for_render(&mut tui);

    let viewport = screen.viewport();
    assert!(viewport[0].contains("Only line"), "Single line rendered");
    assert_eq!(viewport[1].trim(), "", "Line 1 should be cleared");
    tui.stop(TuiStopOptions::default());
}

#[test]
fn handles_shrink_to_empty() {
    let (mut tui, screen) = make_tui(40, 10);
    tui.base_mut().set_clear_on_shrink(true);
    let component = SharedComponent {
        lines: Rc::new(RefCell::new(lines_of(&["Line 0", "Line 1", "Line 2"]))),
    };
    let handle = Rc::clone(&component.lines);
    tui.add_child(Box::new(component));
    tui.start();
    screen.wait_for_render(&mut tui);

    handle.borrow_mut().clear();
    tui.request_render(false);
    screen.wait_for_render(&mut tui);

    let viewport = screen.viewport();
    assert_eq!(viewport[0].trim(), "", "Line 0 should be cleared");
    assert_eq!(viewport[1].trim(), "", "Line 1 should be cleared");
    tui.stop(TuiStopOptions::default());
}

// ============================================================================
// 测试：差分渲染
// ============================================================================

#[test]
fn tracks_cursor_correctly_when_content_shrinks_with_unchanged_remaining_lines() {
    let (mut tui, screen) = make_tui(40, 10);
    let component = SharedComponent {
        lines: Rc::new(RefCell::new(lines_of(&[
            "Line 0", "Line 1", "Line 2", "Line 3", "Line 4",
        ]))),
    };
    let handle = Rc::clone(&component.lines);
    tui.add_child(Box::new(component));
    tui.start();
    screen.wait_for_render(&mut tui);

    // 缩到 3 行，剩下的行内容完全没变（没有内容变化）。
    *handle.borrow_mut() = lines_of(&["Line 0", "Line 1", "Line 2"]);
    tui.request_render(false);
    screen.wait_for_render(&mut tui);

    // 再改第 1 行：证明游标跟踪正确。
    *handle.borrow_mut() = lines_of(&["Line 0", "CHANGED", "Line 2"]);
    tui.request_render(false);
    screen.wait_for_render(&mut tui);

    let viewport = screen.viewport();
    assert!(
        viewport[1].contains("CHANGED"),
        "Expected \"CHANGED\" on line 1, got: {}",
        viewport[1]
    );
    tui.stop(TuiStopOptions::default());
}

#[test]
fn renders_correctly_when_only_a_middle_line_changes() {
    let (mut tui, screen) = make_tui(40, 10);
    let component = SharedComponent {
        lines: Rc::new(RefCell::new(lines_of(&["Header", "Working...", "Footer"]))),
    };
    let handle = Rc::clone(&component.lines);
    tui.add_child(Box::new(component));
    tui.start();
    screen.wait_for_render(&mut tui);

    for frame in ["|", "/", "-", "\\"] {
        *handle.borrow_mut() = lines_of(&["Header", &format!("Working {frame}"), "Footer"]);
        tui.request_render(false);
        screen.wait_for_render(&mut tui);

        let viewport = screen.viewport();
        assert!(
            viewport[0].contains("Header"),
            "Header preserved: {}",
            viewport[0]
        );
        assert!(
            viewport[1].contains(&format!("Working {frame}")),
            "Spinner updated: {}",
            viewport[1]
        );
        assert!(
            viewport[2].contains("Footer"),
            "Footer preserved: {}",
            viewport[2]
        );
    }
    tui.stop(TuiStopOptions::default());
}

#[test]
fn resets_styles_after_each_rendered_line() {
    let (mut tui, screen) = make_tui(20, 6);
    tui.add_child(Box::new(TestComponent::new(&["\x1b[3mItalic", "Plain"])));
    tui.start();
    screen.wait_for_render(&mut tui);

    assert!(!screen.cell_italic(1, 0));
    tui.stop(TuiStopOptions::default());
}

#[test]
fn renders_correctly_when_first_line_changes_but_rest_stays_same() {
    let (mut tui, screen) = make_tui(40, 10);
    let component = SharedComponent {
        lines: Rc::new(RefCell::new(lines_of(&[
            "Line 0", "Line 1", "Line 2", "Line 3",
        ]))),
    };
    let handle = Rc::clone(&component.lines);
    tui.add_child(Box::new(component));
    tui.start();
    screen.wait_for_render(&mut tui);

    *handle.borrow_mut() = lines_of(&["CHANGED", "Line 1", "Line 2", "Line 3"]);
    tui.request_render(false);
    screen.wait_for_render(&mut tui);

    let viewport = screen.viewport();
    assert!(
        viewport[0].contains("CHANGED"),
        "First line changed: {}",
        viewport[0]
    );
    assert!(
        viewport[1].contains("Line 1"),
        "Line 1 preserved: {}",
        viewport[1]
    );
    assert!(
        viewport[2].contains("Line 2"),
        "Line 2 preserved: {}",
        viewport[2]
    );
    assert!(
        viewport[3].contains("Line 3"),
        "Line 3 preserved: {}",
        viewport[3]
    );
    tui.stop(TuiStopOptions::default());
}

#[test]
fn renders_correctly_when_last_line_changes_but_rest_stays_same() {
    let (mut tui, screen) = make_tui(40, 10);
    let component = SharedComponent {
        lines: Rc::new(RefCell::new(lines_of(&[
            "Line 0", "Line 1", "Line 2", "Line 3",
        ]))),
    };
    let handle = Rc::clone(&component.lines);
    tui.add_child(Box::new(component));
    tui.start();
    screen.wait_for_render(&mut tui);

    *handle.borrow_mut() = lines_of(&["Line 0", "Line 1", "Line 2", "CHANGED"]);
    tui.request_render(false);
    screen.wait_for_render(&mut tui);

    let viewport = screen.viewport();
    assert!(
        viewport[0].contains("Line 0"),
        "Line 0 preserved: {}",
        viewport[0]
    );
    assert!(
        viewport[1].contains("Line 1"),
        "Line 1 preserved: {}",
        viewport[1]
    );
    assert!(
        viewport[2].contains("Line 2"),
        "Line 2 preserved: {}",
        viewport[2]
    );
    assert!(
        viewport[3].contains("CHANGED"),
        "Last line changed: {}",
        viewport[3]
    );
    tui.stop(TuiStopOptions::default());
}

#[test]
fn renders_correctly_when_multiple_non_adjacent_lines_change() {
    let (mut tui, screen) = make_tui(40, 10);
    let component = SharedComponent {
        lines: Rc::new(RefCell::new(lines_of(&[
            "Line 0", "Line 1", "Line 2", "Line 3", "Line 4",
        ]))),
    };
    let handle = Rc::clone(&component.lines);
    tui.add_child(Box::new(component));
    tui.start();
    screen.wait_for_render(&mut tui);

    *handle.borrow_mut() = lines_of(&["Line 0", "CHANGED 1", "Line 2", "CHANGED 3", "Line 4"]);
    tui.request_render(false);
    screen.wait_for_render(&mut tui);

    let viewport = screen.viewport();
    assert!(
        viewport[0].contains("Line 0"),
        "Line 0 preserved: {}",
        viewport[0]
    );
    assert!(
        viewport[1].contains("CHANGED 1"),
        "Line 1 changed: {}",
        viewport[1]
    );
    assert!(
        viewport[2].contains("Line 2"),
        "Line 2 preserved: {}",
        viewport[2]
    );
    assert!(
        viewport[3].contains("CHANGED 3"),
        "Line 3 changed: {}",
        viewport[3]
    );
    assert!(
        viewport[4].contains("Line 4"),
        "Line 4 preserved: {}",
        viewport[4]
    );
    tui.stop(TuiStopOptions::default());
}

#[test]
fn handles_transition_from_content_to_empty_and_back_to_content() {
    let (mut tui, screen) = make_tui(40, 10);
    let component = SharedComponent {
        lines: Rc::new(RefCell::new(lines_of(&["Line 0", "Line 1", "Line 2"]))),
    };
    let handle = Rc::clone(&component.lines);
    tui.add_child(Box::new(component));
    tui.start();
    screen.wait_for_render(&mut tui);

    let viewport = screen.viewport();
    assert!(viewport[0].contains("Line 0"), "Initial content rendered");

    handle.borrow_mut().clear();
    tui.request_render(false);
    screen.wait_for_render(&mut tui);

    *handle.borrow_mut() = lines_of(&["New Line 0", "New Line 1"]);
    tui.request_render(false);
    screen.wait_for_render(&mut tui);

    let viewport = screen.viewport();
    assert!(
        viewport[0].contains("New Line 0"),
        "New content rendered: {}",
        viewport[0]
    );
    assert!(
        viewport[1].contains("New Line 1"),
        "New content line 1: {}",
        viewport[1]
    );
    tui.stop(TuiStopOptions::default());
}

#[test]
fn full_re_renders_when_deleted_lines_move_the_viewport_upward() {
    let (mut tui, screen) = make_tui(20, 5);
    let long_lines: Vec<String> = (0..12).map(|i| format!("Line {i}")).collect();
    let component = SharedComponent {
        lines: Rc::new(RefCell::new(long_lines)),
    };
    let handle = Rc::clone(&component.lines);
    tui.add_child(Box::new(component));
    tui.start();
    screen.wait_for_render(&mut tui);

    let initial_redraws = tui.full_redraws();
    *handle.borrow_mut() = (0..7).map(|i| format!("Line {i}")).collect();
    tui.request_render(false);
    screen.wait_for_render(&mut tui);

    assert!(
        tui.full_redraws() > initial_redraws,
        "Shrink should trigger a full redraw"
    );
    assert_eq!(
        screen.viewport(),
        lines_of(&["Line 2", "Line 3", "Line 4", "Line 5", "Line 6"])
    );
    tui.stop(TuiStopOptions::default());
}

#[test]
fn appends_after_a_shrink_without_another_full_redraw_once_the_viewport_is_reset() {
    let (mut tui, screen) = make_tui(20, 5);
    let long_lines: Vec<String> = (0..8).map(|i| format!("Line {i}")).collect();
    let component = SharedComponent {
        lines: Rc::new(RefCell::new(long_lines)),
    };
    let handle = Rc::clone(&component.lines);
    tui.add_child(Box::new(component));
    tui.start();
    screen.wait_for_render(&mut tui);

    let initial_redraws = tui.full_redraws();
    *handle.borrow_mut() = lines_of(&["Line 0", "Line 1"]);
    tui.request_render(false);
    screen.wait_for_render(&mut tui);

    assert!(
        tui.full_redraws() > initial_redraws,
        "Shrink should reset the viewport with a full redraw"
    );
    let redraws_after_shrink = tui.full_redraws();

    *handle.borrow_mut() = lines_of(&["Line 0", "Line 1", "Line 2"]);
    tui.request_render(false);
    screen.wait_for_render(&mut tui);

    assert_eq!(
        tui.full_redraws(),
        redraws_after_shrink,
        "Append should stay on the differential path"
    );
    assert_eq!(
        screen.viewport(),
        lines_of(&["Line 0", "Line 1", "Line 2", "", ""])
    );
    tui.stop(TuiStopOptions::default());
}

#[test]
fn clears_stale_content_when_max_lines_rendered_was_inflated_by_a_transient_component() {
    let (mut tui, screen) = make_tui(40, 10);
    let chat_lines = Rc::new(RefCell::new(
        (0..15).map(|i| format!("Chat {i}")).collect::<Vec<_>>(),
    ));
    let editor_lines = Rc::new(RefCell::new(
        (0..3).map(|i| format!("Editor {i}")).collect::<Vec<_>>(),
    ));
    tui.add_child(Box::new(SharedComponent {
        lines: Rc::clone(&chat_lines),
    }));
    tui.add_child(Box::new(SharedComponent {
        lines: Rc::clone(&editor_lines),
    }));

    tui.start();
    screen.wait_for_render(&mut tui);

    *editor_lines.borrow_mut() = (0..8).map(|i| format!("Selector {i}")).collect();
    tui.request_render(false);
    screen.wait_for_render(&mut tui);

    *editor_lines.borrow_mut() = (0..3).map(|i| format!("Editor {i}")).collect();
    tui.request_render(false);
    screen.wait_for_render(&mut tui);

    let redraws_before_switch = tui.full_redraws();
    *chat_lines.borrow_mut() = (0..12).map(|i| format!("Chat {i}")).collect();
    tui.request_render(false);
    screen.wait_for_render(&mut tui);

    assert!(
        tui.full_redraws() > redraws_before_switch,
        "Branch switch should trigger a full redraw"
    );

    let viewport = screen.viewport();
    for (index, line) in viewport.iter().enumerate().take(10) {
        assert!(
            !line.contains("Chat 12"),
            "Stale \"Chat 12\" at viewport row {index}"
        );
        assert!(
            !line.contains("Chat 13"),
            "Stale \"Chat 13\" at viewport row {index}"
        );
        assert!(
            !line.contains("Chat 14"),
            "Stale \"Chat 14\" at viewport row {index}"
        );
    }
    assert_eq!(
        viewport,
        lines_of(&[
            "Chat 5", "Chat 6", "Chat 7", "Chat 8", "Chat 9", "Chat 10", "Chat 11", "Editor 0",
            "Editor 1", "Editor 2",
        ])
    );
    tui.stop(TuiStopOptions::default());
}

// ============================================================================
// 测试：伪光标
// ============================================================================

#[test]
fn draws_the_focused_fake_cursor_in_reverse_video_and_omits_it_when_hardware_cursor_shown() {
    let (mut tui, screen) = make_tui(20, 5);
    let mut input = Input::new(InputOptions::default());
    input.set_value("abc");
    tui.add_child(Box::new(input));
    tui.set_focus(Some(kiana_tui::tui::FocusTarget::Component { index: 0 }));
    tui.start();
    screen.wait_for_render(&mut tui);
    assert!(
        screen.writes().contains("> \x1b[7ma\x1b[27mbc"),
        "unexpected writes: {:?}",
        screen.writes()
    );

    screen.clear_writes();
    tui.base_mut().set_show_hardware_cursor(true);
    screen.wait_for_render(&mut tui);
    assert!(!screen.writes().contains("\x1b[7m"));
    assert!(!screen.writes().contains("\x1b_kiana:"));
    tui.stop(TuiStopOptions::default());
}

#[test]
fn keeps_the_fake_cursor_of_an_unfocused_input_when_the_hardware_cursor_is_shown() {
    let (mut tui, screen) = make_tui_with(20, 5, Some(true), None);
    let mut input = Input::new(InputOptions::default());
    input.set_value("abc");
    tui.add_child(Box::new(input));
    tui.start();
    screen.wait_for_render(&mut tui);
    assert!(
        screen.writes().contains("> \x1b[7ma\x1b[27mbc"),
        "unexpected writes: {:?}",
        screen.writes()
    );
    tui.stop(TuiStopOptions::default());
}

#[test]
fn omits_the_focused_fake_cursor_when_truncation_dropped_its_end_marker() {
    let (mut tui, screen) = make_tui_with(20, 5, Some(true), None);
    let line = slice_by_column(
        &format!("ab{CURSOR_MARKER}{}def", render_fake_cursor("c")),
        0,
        3,
        true,
    );
    tui.add_child(Box::new(TestComponent { lines: vec![line] }));
    tui.start();
    screen.wait_for_render(&mut tui);
    assert!(!screen.writes().contains("\x1b[7m"));
    assert!(
        screen.writes().ends_with("\x1b[3G"),
        "unexpected writes: {:?}",
        screen.writes()
    );
    tui.stop(TuiStopOptions::default());
}

#[test]
fn keeps_the_cursor_when_an_overlay_ends_directly_left_of_it() {
    for show_hardware_cursor in [false, true] {
        let (mut tui, screen) = make_tui_with(20, 5, Some(show_hardware_cursor), None);
        let base_line = format!("abc{CURSOR_MARKER}{}ef", render_fake_cursor("d"));
        tui.add_child(Box::new(TestComponent {
            lines: vec![base_line],
        }));
        let _overlay = tui.show_overlay(
            Box::new(TestComponent::new(&["XXX"])),
            overlay_at(0.0, 0.0, 3.0),
        );
        tui.start();
        screen.wait_for_render(&mut tui);

        let writes = screen.writes();
        assert!(writes.contains("XXX"));
        assert_eq!(writes.contains("\x1b[7md"), !show_hardware_cursor);
        // 两种模式都定位到第 4 列。
        assert!(
            writes.ends_with("\x1b[4G"),
            "unexpected writes end: {:?}",
            &writes[writes.len().saturating_sub(40)..]
        );
        tui.stop(TuiStopOptions::default());
    }
}

// ============================================================================
// 测试：overlay handle 的焦点变更在同一帧生效
// ============================================================================

/// 回归：TS 的 `OverlayHandle.hide()` / `setHidden()` / `unfocus()` / `focus()` 经
/// `setFocusInternal` **同步**写 `component.focused`，因此紧随其后的 `doRender()` 必须看到
/// 新焦点（`CURSOR_MARKER`、伪光标、硬件光标定位）。Rust 的组合实现把标志刷新挂在
/// `composite_overlays` 里，而那次刷新发生在 `render_children` **之后**，且 overlay 栈为空时
/// 根本不会执行——曾导致关闭对话框后的第一帧（以及之后的空闲帧）沿用旧焦点。
#[test]
fn overlay_handle_focus_changes_apply_before_the_next_frame() {
    for show_hardware_cursor in [false, true] {
        let (mut tui, screen) = make_tui_with(20, 5, Some(show_hardware_cursor), None);
        let mut input = Input::new(InputOptions::default());
        input.set_value("abc");
        tui.add_child(Box::new(input));
        tui.set_focus(Some(kiana_tui::tui::FocusTarget::Component { index: 0 }));
        tui.start();
        screen.wait_for_render(&mut tui);

        // 捕获型 overlay 抢走焦点：Input 不再产出 `CURSOR_MARKER`，因此这一帧不写光标定位。
        let mut handle = tui.show_overlay(
            Box::new(TestComponent::new(&["XX"])),
            overlay_at(0.0, 0.0, 2.0),
        );
        screen.wait_for_render(&mut tui);
        screen.clear_writes();
        tui.render_now(false);
        assert!(
            !screen.writes().contains("\x1b[3G"),
            "the overlay should own focus: {:?}",
            screen.writes()
        );

        // handle.hide() 把焦点还给 Input：这一帧就要画出标记并定位硬件光标。
        screen.clear_writes();
        handle.hide();
        tui.render_now(false);
        let writes = screen.writes();
        // 硬件光标模式下 `resolve_fake_cursors` 只在 `CURSOR_MARKER` 紧邻伪光标时剥掉反显；
        // 焦点没恢复（没有标记）时反显会残留，因此它也指示焦点状态。
        assert_eq!(
            writes.contains("\x1b[7m"),
            !show_hardware_cursor,
            "the fake cursor must follow the restored focus (show_hardware_cursor={show_hardware_cursor}): {writes:?}"
        );
        assert!(
            writes.ends_with("\x1b[3G"),
            "the first frame after hide() must position the hardware cursor: {writes:?}"
        );

        // 之后的空闲帧（无内容变化）仍会重新定位硬件光标（TS 同样写 `\x1b[3G`）。
        screen.clear_writes();
        tui.render_now(false);
        assert!(
            screen.writes().ends_with("\x1b[3G"),
            "idle frames must keep positioning the hardware cursor: {:?}",
            screen.writes()
        );
        tui.stop(TuiStopOptions::default());
    }
}

// ============================================================================
// 测试：`Container.handleMouse` 语义（TS `TuiBase` 没有 `handleInput`）
// ============================================================================

/// 只响应鼠标的探针：记录局部坐标与渲染次数，并在命中时请求焦点。
struct MouseProbe {
    seen: Rc<RefCell<Vec<(isize, isize)>>>,
    renders: Rc<std::cell::Cell<usize>>,
}

impl MouseProbe {
    fn new() -> Self {
        MouseProbe {
            seen: Rc::new(RefCell::new(Vec::new())),
            renders: Rc::new(std::cell::Cell::new(0)),
        }
    }
}

impl Component for MouseProbe {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.renders.set(self.renders.get() + 1);
        vec!["m".to_string()]
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.seen.borrow_mut().push((event.y, event.x));
        Some(TuiMouseEventResult {
            focus: Some(true),
            ..TuiMouseEventResult::default()
        })
    }

    fn handles_input(&self) -> bool {
        true
    }
}

fn mouse_event(y: isize, width: usize, height: usize) -> TuiMouseEvent {
    TuiMouseEvent {
        event_type: TuiMouseEventType::Press,
        button: TuiMouseButton::Left,
        x: 0,
        y,
        screen_x: 0,
        screen_y: y,
        width,
        height,
        shift: false,
        alt: false,
        ctrl: false,
        wheel_delta: None,
        click_count: None,
    }
}

/// TS 的 `TuiBase` 没有 `handleInput`，`Container.handleMouse` 不会把焦点收回给容器：
/// `focusTarget` 保持为**命中的子控件**。
#[test]
fn main_screen_mouse_focus_target_stays_on_the_clicked_child() {
    let (mut tui, _screen) = make_tui(20, 5);
    let probe = MouseProbe::new();
    let seen = Rc::clone(&probe.seen);
    tui.add_child(Box::new(probe));
    let child_address = tui.children()[0].as_ref() as *const dyn Component as *const () as usize;

    let result = Component::handle_mouse(&mut tui, &mouse_event(0, 20, 5))
        .expect("the child should handle the event");
    assert_eq!(result.focus, Some(true));
    assert_eq!(
        result.focus_target,
        Some(MouseFocusTarget::Address(child_address)),
        "focus must stay on the clicked child (TuiBase has no handleInput)"
    );
    assert_eq!(seen.borrow().as_slice(), &[(0, 0)]);
}

/// `mouseLayout` 按组件身份缓存（不是按槽位下标）：渲染后插入/重排子项、下一次渲染之前
/// 到达的事件仍路由给原组件；`invalidate()` 与 TS 一样**不清**缓存。
#[test]
fn main_screen_mouse_layout_keeps_component_identity_across_child_changes() {
    let (mut tui, _screen) = make_tui(20, 5);
    let probe = MouseProbe::new();
    let seen = Rc::clone(&probe.seen);
    let renders = Rc::clone(&probe.renders);
    tui.add_child(Box::new(probe));
    tui.render_children(20);
    assert_eq!(renders.get(), 1);

    // 在数组最前面插入新子项（不渲染）：TS 的缓存仍指向原组件。
    tui.children()
        .insert(0, Box::new(TestComponent::new(&["new"])));
    let result = Component::handle_mouse(&mut tui, &mouse_event(0, 20, 5));
    assert!(
        result.is_some(),
        "the cached entry must still route to the original child"
    );
    assert_eq!(seen.borrow().as_slice(), &[(0, 0)]);
    assert_eq!(renders.get(), 1, "a cache hit must not re-render children");

    // `invalidate()` 只向下传递，不清 `mouseLayout`（TS `Container.invalidate`）。
    tui.invalidate();
    let result = Component::handle_mouse(&mut tui, &mouse_event(0, 20, 5));
    assert!(result.is_some());
    assert_eq!(
        renders.get(),
        1,
        "invalidate() must not drop the cached mouse layout"
    );
}

// ============================================================================
// 测试：`mouseLayout` 的引用语义（REMOVE 之后、下一次渲染之前）
// ============================================================================

/// `Container.removeChild` 只 `splice` 掉 `children`，`mouseLayout` 里的组件引用仍然有效：
/// 下一次渲染之前到达的事件必须交给**已移除**的组件（`tui.ts:366-371`、`383-403`）。
///
/// 旧实现按堆地址在当前 `children` 里查找，找不到就丢弃事件；这里回归验证 TS 的引用语义。
#[test]
fn main_screen_mouse_reaches_a_removed_child_before_the_next_render() {
    let (mut tui, _screen) = make_tui(20, 5);
    let first = MouseProbe::new();
    let first_seen = Rc::clone(&first.seen);
    let second = MouseProbe::new();
    let second_seen = Rc::clone(&second.seen);
    tui.add_child(Box::new(first));
    tui.add_child(Box::new(second));
    tui.render_children(20);

    // `remove_child` 需要 `&dyn Component`，而借用检查器不允许在同一个调用里两次可变借用
    // `tui`。这里只取 `children[0]` 的裸指针：`remove_child` 在移动该 `Box` 之前只按引用读取
    // 身份（不 deref 字段），且被移除的组件会被保活到下一次渲染，指针不会悬垂。
    let first_ref: *const dyn Component = tui.children()[0].as_ref();
    // SAFETY: 见上；`Box` 在调用期间仍归 `children` 所有。
    tui.remove_child(unsafe { &*first_ref });

    // 移除后立刻加入新子项（不渲染）：TS 的缓存仍指向被移除的组件；旧实现若让新分配复用了
    // 已释放的堆地址，还会把事件误路由给新组件。
    tui.add_child(Box::new(MouseProbe::new()));

    // 屏幕第 0 行在缓存里仍属于已移除的组件（TS 会调用它的 handleMouse，返回值照常传出）。
    let result = Component::handle_mouse(&mut tui, &mouse_event(0, 20, 5))
        .expect("the cached entry must still dispatch to the removed child");
    assert_eq!(first_seen.borrow().as_slice(), &[(0, 0)]);
    assert_eq!(result.focus, Some(true));
    assert!(
        matches!(result.focus_target, Some(MouseFocusTarget::Address(_))),
        "focusTarget must still point at the removed (kept-alive) component"
    );

    // 第 1 行仍路由到第二个子项，并使用局部坐标。
    let result = Component::handle_mouse(&mut tui, &mouse_event(1, 20, 5))
        .expect("the second child must still receive its row");
    assert_eq!(second_seen.borrow().as_slice(), &[(0, 0)]);
    assert_eq!(result.handled, Some(true));

    // 下一次渲染替换 `mouseLayout`（保活的组件随之释放）：第 0 行现在属于第二个子项。
    tui.render_children(20);
    Component::handle_mouse(&mut tui, &mouse_event(0, 20, 5)).expect("second child now owns row 0");
    assert_eq!(second_seen.borrow().as_slice(), &[(0, 0), (0, 0)]);
    assert_eq!(
        first_seen.borrow().len(),
        1,
        "the removed child must not receive events after the next render"
    );
}

/// 两个**不同具体类型**的零大小组件：`Box::new(Zst)` 的数据指针都是 `NonNull::dangling()`，
/// 只比较数据指针会把屏幕第 1 行误路由给第一个组件。身份必须带上 vtable 指针。
struct ZstFirstChild;

static ZST_FIRST_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl Component for ZstFirstChild {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec!["a".to_string()]
    }

    fn handle_mouse(&mut self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        ZST_FIRST_HITS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some(TuiMouseEventResult {
            handled: Some(true),
            ..TuiMouseEventResult::default()
        })
    }
}

struct ZstSecondChild;

static ZST_SECOND_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl Component for ZstSecondChild {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec!["b".to_string()]
    }

    fn handle_mouse(&mut self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        ZST_SECOND_HITS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some(TuiMouseEventResult {
            handled: Some(true),
            ..TuiMouseEventResult::default()
        })
    }
}

#[test]
fn main_screen_mouse_distinguishes_zero_sized_children_by_type() {
    use std::sync::atomic::Ordering;
    ZST_FIRST_HITS.store(0, Ordering::SeqCst);
    ZST_SECOND_HITS.store(0, Ordering::SeqCst);

    let (mut tui, _screen) = make_tui(20, 5);
    tui.add_child(Box::new(ZstFirstChild));
    tui.add_child(Box::new(ZstSecondChild));
    tui.render_children(20);

    // TS 的引用相等：第 1 行属于第二个组件（本地 y = 0）。
    assert!(
        Component::handle_mouse(&mut tui, &mouse_event(1, 20, 5)).is_some(),
        "row 1 must reach the second zero-sized child"
    );
    assert_eq!(
        ZST_FIRST_HITS.load(Ordering::SeqCst),
        0,
        "row 1 must not be misrouted to the first ZST child"
    );
    assert_eq!(ZST_SECOND_HITS.load(Ordering::SeqCst), 1);

    // 第 0 行仍然是第一个组件。
    assert!(Component::handle_mouse(&mut tui, &mouse_event(0, 20, 5)).is_some());
    assert_eq!(ZST_FIRST_HITS.load(Ordering::SeqCst), 1);
    assert_eq!(ZST_SECOND_HITS.load(Ordering::SeqCst), 1);
}

/// TS 的 `fs.mkdirSync` / `fs.appendFileSync` 不捕获错误：`KIANA_TUI_DEBUG_REDRAW=1` 且日志目录
/// 不可写时，`doRender()` 会抛出并中止本帧（不写任何终端输出）。旧实现用 `let _ =` 吞掉错误。
#[test]
fn redraw_log_filesystem_errors_abort_the_frame() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _guard = EnvGuard::set(&[("KIANA_TUI_DEBUG_REDRAW", Some("1"))]);

    let base = std::env::temp_dir().join(format!(
        "kiana-tui-log-error-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&base).unwrap();
    // 日志目录位置是一个**普通文件**：`create_dir_all` 失败（TS `mkdirSync` 抛 EEXIST）。
    let log_dir = base.join("not-a-directory");
    std::fs::write(&log_dir, "x").unwrap();

    let terminal = VtTerminal::new(40, 10);
    let screen = Screen(terminal.handle());
    let mut tui = TuiMainScreen::with_options(
        Box::new(terminal),
        None,
        Some(log_dir.to_string_lossy().into_owned()),
    );
    tui.add_child(Box::new(TestComponent::new(&["test"])));

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tui.render_now(false);
    }));
    assert!(
        result.is_err(),
        "an unwritable redraw log directory must abort the render like TS"
    );
    assert!(
        screen.writes().is_empty(),
        "the frame must abort before any terminal write: {:?}",
        screen.writes()
    );

    let _ = std::fs::remove_dir_all(&base);
}
