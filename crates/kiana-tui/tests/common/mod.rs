//! 共享测试终端：`packages/tui/test/virtual-terminal.ts` 的 Rust 等价物。
//!
//! TS 的 `VirtualTerminal` 用 `@xterm/headless` 渲染屏幕；Rust 侧不引入 VT 模拟器
//! （`vt100` 未在 `[workspace.dependencies]` 中固定版本），改为**捕获写入的字节流**，
//! 断言由各测试自己用 `contains`/前缀比较完成。以下是当前稳定契约（改动需同步测试）：
//!
//! - `start` 写 `\x1b[?2004h`（开启 bracketed paste），`stop` 写 `\x1b[?2004l`；
//! - `kittyProtocolActive` 恒为 `true`；
//! - `sendInput`/`resize` 触发 `start` 时注册的回调；
//! - `columns`/`rows` 可运行时改变。
//!
//! 与 TS 的差异：没有终端模拟，因此没有"屏幕内容"概念；测试断言的是原始输出。

#![allow(
    dead_code,
    reason = "共享测试基础设施：各测试 crate 只用到自己需要的部分"
)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use kiana_tui::terminal_colors::{ProgramStatus, Terminal};

/// `Terminal::start` 注册的输入回调（`Box<dyn FnMut(&str)>`）。
type InputHandler = Box<dyn FnMut(&str)>;
/// `Terminal::start` 注册的 resize 回调。
type ResizeHandler = Box<dyn FnMut()>;

/// 捕获写入的测试终端。
pub struct TestTerminal {
    columns: usize,
    rows: usize,
    /// 累计写入的字节流。
    pub output: Rc<RefCell<String>>,
    input_handler: Option<InputHandler>,
    resize_handler: Option<ResizeHandler>,
    started: bool,
    stopped: bool,
    /// 光标可见性（TS `CursorTrackingTerminal.cursorVisible`）：`hide_cursor` 置 false，
    /// `show_cursor` 置 true；共享给测试观察。
    cursor_visible: Rc<Cell<bool>>,
    /// `restore_on_exit(before, after)` 的调用记录（TS `terminal.restoreOnExit`）。
    restore_calls: Rc<RefCell<Vec<(String, String)>>>,
}

impl TestTerminal {
    /// 80x24 的默认尺寸（与 TS 的 `new VirtualTerminal()` 一致）。
    pub fn new() -> Self {
        TestTerminal::with_size(80, 24)
    }

    /// 指定尺寸。
    pub fn with_size(columns: usize, rows: usize) -> Self {
        TestTerminal {
            columns,
            rows,
            output: Rc::new(RefCell::new(String::new())),
            input_handler: None,
            resize_handler: None,
            started: false,
            stopped: false,
            cursor_visible: Rc::new(Cell::new(true)),
            restore_calls: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// `restore_on_exit` 的调用记录（按调用顺序）。
    pub fn restore_calls(&self) -> Vec<(String, String)> {
        self.restore_calls.borrow().clone()
    }

    /// 共享的 `restore_on_exit` 记录（供 `TuiBase` 之外的观察者读取）。
    pub fn restore_calls_handle(&self) -> Rc<RefCell<Vec<(String, String)>>> {
        Rc::clone(&self.restore_calls)
    }

    /// 共享的光标可见性句柄（TS 的 `CursorTrackingTerminal.cursorVisible`）。
    pub fn cursor_visible_handle(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.cursor_visible)
    }

    /// 共享输出缓冲（供 `TuiBase` 之外的观察者读取）。
    pub fn output_handle(&self) -> Rc<RefCell<String>> {
        Rc::clone(&self.output)
    }

    /// 取出并清空累计输出。
    pub fn take_output(&self) -> String {
        std::mem::take(&mut *self.output.borrow_mut())
    }

    /// 当前累计输出（不清空）。
    pub fn output(&self) -> String {
        self.output.borrow().clone()
    }

    /// `sendInput(data)`：触发输入回调。
    pub fn send_input(&mut self, data: &str) {
        if let Some(handler) = self.input_handler.as_mut() {
            handler(data);
        }
    }

    /// `resize(columns, rows)`：改尺寸并触发 resize 回调。
    pub fn resize(&mut self, columns: usize, rows: usize) {
        self.columns = columns;
        self.rows = rows;
        if let Some(handler) = self.resize_handler.as_mut() {
            handler();
        }
    }

    /// 是否调用过 `start`。
    pub fn is_started(&self) -> bool {
        self.started
    }

    /// 是否调用过 `stop`。
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }
}

impl Default for TestTerminal {
    fn default() -> Self {
        TestTerminal::new()
    }
}

impl Terminal for TestTerminal {
    fn restore_on_exit(&mut self, before: &str, after: &str) {
        self.restore_calls
            .borrow_mut()
            .push((before.to_string(), after.to_string()));
    }

    fn start(&mut self, on_input: Box<dyn FnMut(&str)>, on_resize: Box<dyn FnMut()>) {
        self.input_handler = Some(on_input);
        self.resize_handler = Some(on_resize);
        self.started = true;
        // 与 ProcessTerminal 一致：开启 bracketed paste。
        self.write("\x1b[?2004h");
    }

    fn stop(&mut self) {
        self.write("\x1b[?2004l");
        self.input_handler = None;
        self.resize_handler = None;
        self.stopped = true;
    }

    fn drain_input(
        &mut self,
        _max_ms: u64,
        _idle_ms: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>> {
        Box::pin(async {})
    }

    fn write(&mut self, data: &str) {
        self.output.borrow_mut().push_str(data);
    }

    fn columns(&self) -> usize {
        self.columns
    }

    fn rows(&self) -> usize {
        self.rows
    }

    fn kitty_protocol_active(&self) -> bool {
        // 测试终端恒为 true（与 TS 的 `VirtualTerminal` 一致）。
        true
    }

    fn move_by(&mut self, lines: isize) {
        if lines > 0 {
            self.write(&format!("\x1b[{lines}B"));
        } else if lines < 0 {
            self.write(&format!("\x1b[{}A", -lines));
        }
    }

    fn hide_cursor(&mut self) {
        self.cursor_visible.set(false);
        self.write("\x1b[?25l");
    }

    fn show_cursor(&mut self) {
        self.cursor_visible.set(true);
        self.write("\x1b[?25h");
    }

    fn clear_line(&mut self) {
        self.write("\x1b[K");
    }

    fn clear_from_cursor(&mut self) {
        self.write("\x1b[J");
    }

    fn clear_screen(&mut self) {
        self.write("\x1b[2J\x1b[H");
    }

    fn set_title(&mut self, title: &str) {
        self.write(&format!("\x1b]0;{title}\x07"));
    }

    fn set_progress(&mut self, _active: bool) {}

    fn set_program_status(&mut self, _status: &ProgramStatus) {}
}

/// 把写入流里可见的文本行取出来（去掉 ANSI/OSC/APC 与 `\r`，按 `\n` 切分）。
///
/// 只用于断言"输出的可读内容"；需要逐字节比较的测试直接读 [`TestTerminal::output`]。
pub fn visible_output_lines(output: &str) -> Vec<String> {
    kiana_tui::text::strip_terminal_sequences(output)
        .split('\n')
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect()
}
