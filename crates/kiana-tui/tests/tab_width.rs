//! `packages/tui/test/tab-width.test.ts` 的移植（4 个 it）。
//!
//! 前三个用例只依赖 `utils.ts` 的切片/分段与 `normalizeTerminalOutput`，TS 版同样是纯字符串
//! 断言。第四个用例用 `TuiMainScreen` + 16x3 的捕获终端，断言"含 tab 的 overlay 只占一个物理
//! 终端行"：Rust 侧直接检查**写入流**——按 `\r\n` 切开后每行的可见文本必须与预期
//! 视口一致（三行内容都短于终端宽度，不存在滚动），并且写入流里不能出现
//! 字面 tab。

use std::cell::RefCell;
use std::rc::Rc;

use kiana_tui::main_screen::TuiMainScreen;
use kiana_tui::terminal_colors::{ProgramStatus, Terminal};
use kiana_tui::text::{
    extract_segments, normalize_terminal_output, slice_with_width, strip_terminal_sequences,
    visible_width,
};
use kiana_tui::tui::{Component, OverlayOptions, SizeValue, Tui, TuiStopOptions};

/// `keeps slice helper widths consistent with visible width`
#[test]
fn keeps_slice_helper_widths_consistent_with_visible_width() {
    let text = "out 192M\t.kiana/skill-tests/results-ha";
    let slice = slice_with_width(text, 0, 10, true);

    assert_eq!(slice.text, "out 192M");
    assert_eq!(slice.width, 8);
    assert_eq!(visible_width(&slice.text), slice.width);
}

/// `keeps overlay segment widths consistent with visible width`
#[test]
fn keeps_overlay_segment_widths_consistent_with_visible_width() {
    let text = "out 192M\t.kiana/skill-tests/results-ha";
    let segments = extract_segments(text, 10, 13, 10, true);

    assert_eq!(segments.before, "out 192M");
    assert_eq!(segments.before_width, 8);
    assert_eq!(visible_width(&segments.before), segments.before_width);

    let tab_fits = extract_segments(text, 11, 13, 10, true);
    assert_eq!(tab_fits.before, "out 192M\t");
    assert_eq!(tab_fits.before_width, 11);
    assert_eq!(visible_width(&tab_fits.before), tab_fits.before_width);
}

/// `keeps tabs inside terminal control sequences byte-identical`
#[test]
fn keeps_tabs_inside_terminal_control_sequences_byte_identical() {
    let control_sequences = [
        "\x1b]8;;https://example.test/a\tb\x07",
        "\x1b]0;window\ttitle\x1b\\",
        "\x1b_payload\tdata\x1b\\",
    ];

    for control_sequence in control_sequences {
        assert_eq!(
            normalize_terminal_output(&format!("{control_sequence}label\ttext")),
            format!("{control_sequence}label   text")
        );
    }
}

/// 满视口内容：每行补到终端宽度（TS `FullViewportContent`）。
struct FullViewportContent;

impl Component for FullViewportContent {
    fn render(&mut self, width: usize) -> Vec<String> {
        ["base 0", "base 1", "base 2"]
            .iter()
            .map(|line| format!("{line:<width$}"))
            .collect()
    }

    fn handles_input(&self) -> bool {
        false
    }
}

/// 单行 overlay（TS `TabStatusOverlay`）。
struct TabStatusOverlay;

impl Component for TabStatusOverlay {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec!["\tX".to_string()]
    }

    fn handles_input(&self) -> bool {
        false
    }
}

/// 记录写入的终端（不做屏幕仿真）。
struct CapturingTerminal {
    output: Rc<RefCell<String>>,
    columns: usize,
    rows: usize,
}

impl Terminal for CapturingTerminal {
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
        self.output.borrow_mut().push_str(data);
    }
    fn columns(&self) -> usize {
        self.columns
    }
    fn rows(&self) -> usize {
        self.rows
    }
    fn kitty_protocol_active(&self) -> bool {
        true
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

/// `keeps tab-containing overlays on one physical terminal row`
#[test]
fn keeps_tab_containing_overlays_on_one_physical_terminal_row() {
    let output = Rc::new(RefCell::new(String::new()));
    let terminal = CapturingTerminal {
        output: Rc::clone(&output),
        columns: 16,
        rows: 3,
    };
    let mut tui = TuiMainScreen::new(Box::new(terminal));
    tui.add_child(Box::new(FullViewportContent));
    let _overlay = tui.show_overlay(
        Box::new(TabStatusOverlay),
        OverlayOptions {
            width: Some(SizeValue::Absolute(4.0)),
            row: Some(SizeValue::Absolute(1.0)),
            col: Some(SizeValue::Absolute(4.0)),
            ..OverlayOptions::default()
        },
    );
    tui.start();
    tui.render_now(false);

    let writes = output.borrow().clone();
    assert!(
        !writes.contains('\t'),
        "tab must be normalized before it reaches the terminal"
    );
    // 首次渲染：`\x1b[?2026h` + 3 行 + `\x1b[?2026l` + 光标定位。
    // 注意：逐行 strip（`strip_terminal_sequences` 对整段里的私有模式 CSI 不可靠）。
    let body = writes
        .strip_prefix("\x1b[?2026h")
        .expect("first render starts with synchronized output");
    let body = &body[..body
        .find("\x1b[?2026l")
        .expect("first render ends synchronized output")];
    let lines: Vec<String> = body.split("\r\n").map(strip_terminal_sequences).collect();
    assert_eq!(
        lines,
        vec![
            "base 0          ".to_string(),
            "base   X        ".to_string(),
            "base 2          ".to_string(),
        ],
        "unexpected rendered lines"
    );
    tui.stop(TuiStopOptions::default());
}
