//! `test/overlay-short-content.test.ts` 的逐条移植（1 个 it）。
//!
//! TS 用 `TuiMainScreen` + xterm 仿真屏；Rust 侧主屏渲染器属另一单元，这里同样驱动
//! `TuiBase` 的合成管线（`composite_overlays` + `apply_line_resets`），并断言
//! "内容短于终端高度时 overlay 仍然可见"。

#[path = "common/mod.rs"]
mod common;

use kiana_tui::text::strip_terminal_sequences;
use kiana_tui::tui::{Component, OverlayOptions, TuiBase};

/// 固定若干行内容（TS `SimpleContent`）。
struct SimpleContent(Vec<String>);

impl Component for SimpleContent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.0.clone()
    }
}

/// 固定三行 overlay（TS `SimpleOverlay`）。
struct SimpleOverlay;

impl Component for SimpleOverlay {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec![
            "OVERLAY_TOP".to_string(),
            "OVERLAY_MID".to_string(),
            "OVERLAY_BOT".to_string(),
        ]
    }
}

/// `should render overlay when content is shorter than terminal height`
#[test]
fn should_render_overlay_when_content_is_shorter_than_terminal_height() {
    let terminal = common::TestTerminal::with_size(80, 24);
    let mut base = TuiBase::new(Box::new(terminal), None, None);
    base.set_terminal_size(80, 24);
    base.add_child(Box::new(SimpleContent(vec![
        "Line 1".to_string(),
        "Line 2".to_string(),
        "Line 3".to_string(),
    ])));

    base.show_overlay(Box::new(SimpleOverlay), OverlayOptions::default());
    base.start();

    // 主屏渲染：先渲染子组件，再合成 overlay（`tui-main-screen.ts:265-275`）。
    let content = vec![
        "Line 1".to_string(),
        "Line 2".to_string(),
        "Line 3".to_string(),
    ];
    let lines = base.composite_overlays(content, 80, 24);
    let has_overlay = lines
        .iter()
        .any(|line| strip_terminal_sequences(line).contains("OVERLAY"));

    assert!(
        has_overlay,
        "Overlay should be visible when content is shorter than terminal"
    );
    // 内容短于终端高度时，合成结果必须补齐到 24 行（overlay 才有屏幕坐标）。
    assert_eq!(lines.len(), 24);

    base.stop(kiana_tui::tui::TuiStopOptions::default());
}

// ---------------------------------------------------------------------------
// 端到端：经 `TuiMainScreen` 的真实渲染路径（TS 原测试用 `TuiMainScreen` + xterm 屏）
// ---------------------------------------------------------------------------

/// 与 TS 完全同构：80x24 终端、3 行内容、默认居中的 3 行 overlay，经 `TuiMainScreen`
/// 渲染后 overlay 必须出现在输出里（内容短于终端高度时由 `compositeOverlays` 补足 24 行）。
#[test]
fn tui_main_screen_renders_overlay_when_content_is_shorter_than_terminal_height() {
    use kiana_tui::main_screen::TuiMainScreen;
    use kiana_tui::tui::{Tui, TuiStopOptions};

    let terminal = common::TestTerminal::with_size(80, 24);
    let output = terminal.output_handle();
    let mut tui = TuiMainScreen::new(Box::new(terminal));
    tui.add_child(Box::new(SimpleContent(vec![
        "Line 1".to_string(),
        "Line 2".to_string(),
        "Line 3".to_string(),
    ])));
    let _overlay = tui.show_overlay(Box::new(SimpleOverlay), OverlayOptions::default());

    tui.start();
    tui.render_now(false);

    let rendered = output.borrow().clone();
    assert!(
        strip_terminal_sequences(&rendered).contains("OVERLAY_TOP"),
        "overlay should be visible when content is shorter than terminal"
    );
    // 首次全量渲染：`\x1b[?2026h` + 24 行（23 个 `\r\n` 分隔）+ `\x1b[?2026l`。
    assert_eq!(
        rendered.matches("\r\n").count(),
        23,
        "short content must be padded to the terminal height"
    );
    tui.stop(TuiStopOptions::default());
}
