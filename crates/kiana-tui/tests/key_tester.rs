//! `packages/tui/test/key-tester.ts` 的手工脚本移植（`#[ignore]`）。
//!
//! TS 脚本是交互式按键码查看器：`ProcessTerminal` + `TuiMainScreen`，`KeyLogger` 组件把每次
//! 按键的十六进制/码点/转义表示打印出来（Ctrl+C 退出），并每 100ms 刷新一次协议协商状态。
//!
//! Rust 版保留同一套渲染与格式化逻辑（标题、协议行、`Hex: … | Chars: […] | Repr: "…"`、
//! 填充到 25 行、页脚提示），但作为一个 `#[ignore]` 测试只渲染一帧就退出——交互循环需要真实
//! TTY，测试环境里没有。两个已知偏差：
//!
//! - `modifyOtherKeysActive` 只存在于 `ProcessTerminal` 的具体类型上，而 `TuiMainScreen`
//!   持有的是 `Box<dyn Terminal>`，因此协议名只能区分 `kitty` / `legacy`；
//! - 组件不能反向持有 `&mut TuiMainScreen`（Rust 所有权），Ctrl+C 只置退出标志，由宿主
//!   （这里是测试）决定何时 `stop()`——与 TS 里 `tui.stop(); process.exit(0)` 的可观察结果
//!   相同。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use kiana_tui::keys::matches_key;
use kiana_tui::main_screen::TuiMainScreen;
use kiana_tui::terminal::ProcessTerminal;
use kiana_tui::text::truncate_to_width_default;
use kiana_tui::tui::{Component, FocusTarget, Tui, TuiStopOptions};

/// 按键码记录组件（TS `KeyLogger`）。
struct KeyLogger {
    log: RefCell<Vec<String>>,
    max_lines: usize,
    protocol: String,
    exit_requested: Rc<Cell<bool>>,
}

impl KeyLogger {
    fn new(protocol: impl Into<String>, exit_requested: Rc<Cell<bool>>) -> Self {
        KeyLogger {
            log: RefCell::new(Vec::new()),
            max_lines: 20,
            protocol: protocol.into(),
            exit_requested,
        }
    }

    /// `fit(line, width)`：`truncateToWidth(line, width).padEnd(width)`。
    ///
    /// `truncateToWidth` 的默认省略号是 `"..."`；`padEnd` 按 **UTF-16 code unit** 数补齐
    /// （JS 的 `string.length`），因此含 ANSI 转义的行在 TS 里更容易超出宽度而不再补空格。
    fn fit(&self, line: &str, width: usize) -> String {
        let mut padded = truncate_to_width_default(line, width);
        let length = padded.encode_utf16().count();
        padded.push_str(&" ".repeat(width.saturating_sub(length)));
        padded
    }

    fn protocol_name(&self) -> &str {
        &self.protocol
    }
}

impl Component for KeyLogger {
    fn handle_input(&mut self, data: &str) {
        // Ctrl+C（裸键或 Kitty 协议）退出。
        if matches_key(data, "ctrl+c") {
            self.exit_requested.set(true);
            return;
        }

        let hex = data
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        // TS：`Array.from(data).map((c) => c.charCodeAt(0))` —— `Array.from` 按**码点**迭代，
        // 但 `charCodeAt(0)` 取的是第一个 **UTF-16 code unit**（emoji → 55357，不是 128512）。
        let char_codes = data
            .chars()
            .map(|ch| {
                let mut units = [0u16; 2];
                ch.encode_utf16(&mut units)[0].to_string()
            })
            .collect::<Vec<_>>()
            .join(", ");
        let repr = data
            .replace('\x1b', "\\x1b")
            .replace('\r', "\\r")
            .replace('\n', "\\n")
            .replace('\t', "\\t")
            .replace('\x7f', "\\x7f");

        let log_line = format!(
            "Hex: {:<20} | Chars: [{:<15}] | Repr: \"{}\"",
            hex, char_codes, repr
        );

        let mut log = self.log.borrow_mut();
        log.push(log_line);
        if log.len() > self.max_lines {
            log.remove(0);
        }
    }

    fn handles_input(&self) -> bool {
        true
    }

    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();

        // 标题。
        lines.push("=".repeat(width));
        lines.push(self.fit(
            "Key Code Tester - Press keys to see their codes (Ctrl+C to exit)",
            width,
        ));
        lines.push(self.fit(&format!("Protocol: {}", self.protocol_name()), width));
        lines.push("=".repeat(width));
        lines.push(String::new());

        // 日志条目。
        for entry in self.log.borrow().iter() {
            lines.push(self.fit(entry, width));
        }

        // 填充剩余空间（TS：`"".padEnd(width)`，即 width 个空格）。
        let remaining = 25usize.saturating_sub(lines.len());
        for _ in 0..remaining {
            lines.push(" ".repeat(width));
        }

        // 页脚。
        lines.push("=".repeat(width));
        lines.push(self.fit("Test these:", width));
        lines.push(self.fit(
            "  - Shift + Enter (should show: \\x1b[13;2u with Kitty protocol)",
            width,
        ));
        lines.push(self.fit("  - Alt/Option + Enter", width));
        lines.push(self.fit("  - Option/Alt + Backspace", width));
        lines.push(self.fit("  - Cmd/Ctrl + Backspace", width));
        lines.push(self.fit("  - Regular Backspace", width));
        lines.push("=".repeat(width));

        lines
    }
}

#[test]
#[ignore = "manual demo: needs a real TTY (ProcessTerminal + raw mode)"]
fn key_code_tester_renders_one_frame() {
    let terminal = ProcessTerminal::new();
    let mut tui = TuiMainScreen::new(Box::new(terminal));
    let protocol = if tui.base().terminal.borrow().kitty_protocol_active() {
        "kitty"
    } else {
        "legacy"
    };
    let exit_requested = Rc::new(Cell::new(false));
    let logger = KeyLogger::new(protocol, Rc::clone(&exit_requested));
    tui.add_child(Box::new(logger));
    tui.set_focus(Some(FocusTarget::Component { index: 0 }));

    tui.start();
    tui.render_now(false);
    tui.stop(TuiStopOptions::default());
}

/// `KeyLogger` 的格式化辅助函数（不需要 TTY，随普通测试运行）。
#[test]
fn key_logger_formats_codes_and_lines_like_the_ts_script() {
    let exit_requested = Rc::new(Cell::new(false));
    let mut logger = KeyLogger::new("legacy", Rc::clone(&exit_requested));

    // `Array.from(data).map((c) => c.charCodeAt(0))`：emoji 取第一个 UTF-16 code unit。
    logger.handle_input("😀a");
    {
        let log = logger.log.borrow();
        assert!(
            log[0].contains("Chars: [55357, 97"),
            "unexpected log line: {}",
            log[0]
        );
    }
    logger.handle_input("x");
    assert_eq!(logger.log.borrow().len(), 2);

    // `fit`：默认省略号 "..."，`padEnd` 按 UTF-16 长度补空格。
    assert_eq!(logger.fit("ab", 5), "ab   ");
    // `truncateToWidth` 自己会在被截断的前缀与省略号两侧补 `\x1b[0m`（node 26 实测）。
    assert_eq!(logger.fit("abcdefgh", 5), "ab\x1b[0m...\x1b[0m");
    let styled = "\x1b[31mab\x1b[0m";
    assert_eq!(
        logger.fit(styled, 5),
        styled,
        "ANSI counts toward padEnd length"
    );

    // Ctrl+C 只置退出标志（见模块文档的偏差说明）。
    logger.handle_input("\x03");
    assert!(exit_requested.get());
}
