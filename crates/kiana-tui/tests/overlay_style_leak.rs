//! `test/tui-overlay-style-leak.test.ts` 的移植（2 个 it）。
//!
//! TS 用 xterm 缓冲断言"下一行首格不是斜体"（`cell.isItalic() === 0`）。Rust 侧不做终端
//! 模拟，因此直接断言同样的语义：**行尾的 SGR 状态**——合成/补 reset 之后，第一行不能
//! 仍处于斜体状态，否则会渗到下一行。
//!
//! 管线与 `TuiMainScreen.render` 一致：`compositeOverlays` → `applyLineResets`
//! （`tui-main-screen.ts:264-275`）。

#[path = "common/mod.rs"]
mod common;

use common::TestTerminal;
use kiana_tui::text::strip_terminal_sequences;
use kiana_tui::tui::{Component, OverlayOptions, SizeValue, TuiBase, TuiStopOptions};

struct StaticLines {
    lines: Vec<String>,
}

impl StaticLines {
    fn new(lines: &[&str]) -> Self {
        StaticLines {
            lines: lines.iter().map(|line| (*line).to_string()).collect(),
        }
    }
}

impl Component for StaticLines {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.lines.clone()
    }
}

struct StaticOverlay {
    line: String,
}

impl StaticOverlay {
    fn new(line: &str) -> Self {
        StaticOverlay {
            line: line.to_string(),
        }
    }
}

impl Component for StaticOverlay {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec![self.line.clone()]
    }
}

/// 行尾是否仍处于斜体：扫描 SGR 参数（`0` 全复位、`3` 开斜体、`23` 关斜体）。
fn ends_italic(line: &str) -> bool {
    let mut italic = false;
    let mut rest = line;
    while let Some(start) = rest.find("\x1b[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('m') else {
            break;
        };
        for part in after[..end].split(';') {
            match part.parse::<u32>().unwrap_or(0) {
                0 => italic = false,
                3 => italic = true,
                23 => italic = false,
                _ => {}
            }
        }
        rest = &after[end + 1..];
    }
    italic
}

fn base(width: usize) -> (TuiBase, String) {
    let base_line = format!("\x1b[3m{}\x1b[23m", "X".repeat(width));
    let terminal = TestTerminal::with_size(width, 6);
    let mut base = TuiBase::new(Box::new(terminal), None, None);
    base.add_child(Box::new(StaticLines::new(&[&base_line, "INPUT"])));
    (base, base_line)
}

/// `should not leak styles when a trailing reset sits beyond the last visible column (no overlay)`
#[test]
fn does_not_leak_styles_with_trailing_reset_beyond_last_column() {
    let width = 20;
    let (mut base, base_line) = base(width);
    base.start();

    // 没有 overlay：`compositeOverlays` 原样返回，`applyLineResets` 补段复位。
    let lines = base.composite_overlays(vec![base_line, "INPUT".to_string()], width, 6);
    let lines = base.apply_line_resets(lines);

    assert!(!ends_italic(&lines[0]), "first line must not stay italic");
    assert_eq!(strip_terminal_sequences(&lines[1]), "INPUT");
    base.stop(TuiStopOptions::default());
}

/// `should not leak styles when overlay slicing drops trailing SGR resets`
#[test]
fn does_not_leak_styles_when_overlay_slicing_drops_trailing_resets() {
    let width = 20;
    let (mut base, base_line) = base(width);
    base.show_overlay(
        Box::new(StaticOverlay::new("OVR")),
        OverlayOptions {
            row: Some(SizeValue::Absolute(0.0)),
            col: Some(SizeValue::Absolute(5.0)),
            width: Some(SizeValue::Absolute(3.0)),
            ..OverlayOptions::default()
        },
    );
    base.start();

    let lines = base.composite_overlays(vec![base_line, "INPUT".to_string()], width, 6);
    let lines = base.apply_line_resets(lines);

    assert!(!ends_italic(&lines[0]), "first line must not stay italic");
    let first = strip_terminal_sequences(&lines[0]);
    assert_eq!(first, format!("{}OVR{}", "X".repeat(5), "X".repeat(12)));
    assert_eq!(strip_terminal_sequences(&lines[1]), "INPUT");
    base.stop(TuiStopOptions::default());
}
