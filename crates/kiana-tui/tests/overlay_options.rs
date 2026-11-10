//! `test/overlay-options.test.ts` 的逐条移植（26 个 it）。
//!
//! TS 用 `TuiMainScreen` + `VirtualTerminal`，从仿真屏幕的字符缓冲里断言行/列；
//! Rust 侧 `tui-main-screen` 属于另一个单元，因此这里直接驱动 **`TuiBase` 的合成管线**
//! （`composite_overlays` + `apply_line_resets`）——TS 的 `TuiMainScreen.render` 对短内容
//! 走的正是这两步（`tui-main-screen.ts:265-275`），且内容短于终端高度时 `compositeOverlays`
//! 会把结果补齐到 `termHeight` 行、`viewportStart = 0`，因此"屏幕第 N 行"与"合成结果第 N 行"
//! 一一对应。断言覆盖行号、列号与包含关系，是当前合成管线的稳定契约，改动需同步测试。
//!
//! 参考实现：`tui.ts` 的 `resolveOverlayLayout` / `compositeOverlays`（本文件不改行为）。

#[path = "common/mod.rs"]
mod common;

use std::cell::Cell;
use std::rc::Rc;

use common::TestTerminal;
use kiana_tui::text::{strip_terminal_sequences, visible_width};
use kiana_tui::tui::{
    Component, OverlayAnchor, OverlayHandle, OverlayMargin, OverlayMarginValue, OverlayOptions,
    SizeValue, TuiBase, TuiStopOptions,
};

/// 固定内容的 overlay（TS `StaticOverlay`），并记录被请求的渲染宽度。
struct StaticOverlay {
    lines: Vec<String>,
    requested_width: Rc<Cell<usize>>,
}

impl StaticOverlay {
    fn new(lines: &[&str]) -> Self {
        StaticOverlay {
            lines: lines.iter().map(|line| (*line).to_string()).collect(),
            requested_width: Rc::new(Cell::new(0)),
        }
    }

    fn width_handle(&self) -> Rc<Cell<usize>> {
        Rc::clone(&self.requested_width)
    }
}

impl Component for StaticOverlay {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.requested_width.set(width);
        self.lines.clone()
    }
}

/// 一个渲染空行的"基础内容"（TS `EmptyContent`）。
struct EmptyContent;

impl Component for EmptyContent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        Vec::new()
    }
}

/// 合成后的可见行（去掉 ANSI）。
fn visible(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .collect()
}

/// 建一个 80x24（或指定尺寸）的 `TuiBase`，并返回终端的光标可见性句柄。
fn base(columns: usize, rows: usize) -> (TuiBase, Rc<Cell<bool>>) {
    let terminal = TestTerminal::with_size(columns, rows);
    let cursor_visible = terminal.cursor_visible_handle();
    let mut base = TuiBase::new(Box::new(terminal), None, None);
    base.add_child(Box::new(EmptyContent));
    base.set_terminal_size(columns, rows);
    (base, cursor_visible)
}

/// `TuiMainScreen.render` 的等价两步：合成 overlay、补行尾 reset。
fn render(base: &mut TuiBase, columns: usize, rows: usize) -> Vec<String> {
    let lines = base.composite_overlays(Vec::new(), columns, rows);
    base.apply_line_resets(lines)
}

/// 行首到 `needle` 之间的可见列数（TS 的 `line.indexOf(needle)`）。
fn column_of(line: &str, needle: &str) -> Option<usize> {
    let text = strip_terminal_sequences(line);
    let byte_index = text.find(needle)?;
    Some(visible_width(&text[..byte_index]))
}

fn options(width: SizeValue) -> OverlayOptions {
    OverlayOptions {
        width: Some(width),
        ..OverlayOptions::default()
    }
}

// =============================================================================
// width overflow protection
// =============================================================================

/// `should truncate overlay lines that exceed declared width`
#[test]
fn should_truncate_overlay_lines_that_exceed_declared_width() {
    let (mut base, _) = base(80, 24);
    let overlay = StaticOverlay::new(&[&"X".repeat(100)]);
    base.show_overlay(Box::new(overlay), options(SizeValue::Absolute(20.0)));
    let lines = render(&mut base, 80, 24);
    assert_eq!(lines.len(), 24);
    for line in &lines {
        assert!(
            visible_width(line) <= 80,
            "line exceeds terminal width: {line:?}"
        );
    }
}

/// `should handle overlay with complex ANSI sequences without crashing`
#[test]
fn should_handle_overlay_with_complex_ansi_sequences_without_crashing() {
    let (mut base, _) = base(80, 24);
    let complex_line = format!(
        "\x1b[48;2;40;50;40m \x1b[38;2;128;128;128mSome styled content\x1b[39m\x1b[49m\
         \x1b]8;;http://example.com\x07link\x1b]8;;\x07{}",
        " more content ".repeat(10)
    );
    let overlay = StaticOverlay::new(&[&complex_line, &complex_line, &complex_line]);
    base.show_overlay(Box::new(overlay), options(SizeValue::Absolute(60.0)));
    let lines = render(&mut base, 80, 24);
    assert!(!lines.is_empty());
    for line in &lines {
        assert!(visible_width(line) <= 80);
    }
}

/// `should handle overlay composited on styled base content`
#[test]
fn should_handle_overlay_composited_on_styled_base_content() {
    /// 每行都是整宽红色粗体（TS `StyledContent`）。
    struct StyledContent;

    impl Component for StyledContent {
        fn render(&mut self, width: usize) -> Vec<String> {
            let styled = format!("\x1b[1m\x1b[38;2;255;0;0m{}\x1b[0m", "X".repeat(width));
            vec![styled.clone(), styled.clone(), styled]
        }
    }

    let terminal = TestTerminal::with_size(80, 24);
    let mut base = TuiBase::new(Box::new(terminal), None, None);
    base.add_child(Box::new(StyledContent));
    base.set_terminal_size(80, 24);
    let mut styled = StyledContent.render(80);
    let mut opts = options(SizeValue::Absolute(20.0));
    opts.anchor = Some(OverlayAnchor::Center);
    base.show_overlay(Box::new(StaticOverlay::new(&["OVERLAY"])), opts);

    let lines = base.composite_overlays(styled.clone(), 80, 24);
    styled.clear();
    assert!(
        visible(&lines).iter().any(|line| line.contains("OVERLAY")),
        "Overlay should be visible"
    );
}

/// `should handle wide characters at overlay boundary`
#[test]
fn should_handle_wide_characters_at_overlay_boundary() {
    let (mut base, _) = base(80, 24);
    let overlay = StaticOverlay::new(&["中文日本語한글テスト漢字"]);
    base.show_overlay(Box::new(overlay), options(SizeValue::Absolute(15.0)));
    let lines = render(&mut base, 80, 24);
    assert!(!lines.is_empty());
    for line in &lines {
        assert!(visible_width(line) <= 80);
    }
}

/// `should handle overlay positioned at terminal edge`
#[test]
fn should_handle_overlay_positioned_at_terminal_edge() {
    let (mut base, _) = base(80, 24);
    let overlay = StaticOverlay::new(&[&"X".repeat(50)]);
    let mut opts = options(SizeValue::Absolute(20.0));
    opts.col = Some(SizeValue::Absolute(60.0));
    base.show_overlay(Box::new(overlay), opts);
    let lines = render(&mut base, 80, 24);
    assert!(!lines.is_empty());
    for line in &lines {
        assert!(visible_width(line) <= 80);
    }
    // col 60 + width 20 正好贴右边缘：找到 overlay 所在行，前 60 列必须是空白。
    let text = visible(&lines);
    let row = text
        .iter()
        .position(|line| line.contains('X'))
        .expect("overlay should be rendered");
    assert!(
        text[row].starts_with(&" ".repeat(60)),
        "got {:?}",
        text[row]
    );
}

/// `should handle overlay on base content with OSC sequences`
#[test]
fn should_handle_overlay_on_base_content_with_osc_sequences() {
    let (mut base, _) = base(80, 24);
    let mut opts = options(SizeValue::Absolute(20.0));
    opts.anchor = Some(OverlayAnchor::Center);
    base.show_overlay(Box::new(StaticOverlay::new(&["OVERLAY-TEXT"])), opts);

    let link = "\x1b]8;;file:///path/to/file.ts\x07file.ts\x1b]8;;\x07";
    let line = format!("See {link} for details {}", "X".repeat(80 - 30));
    let lines = base.composite_overlays(vec![line.clone(), line.clone(), line], 80, 24);
    assert!(!lines.is_empty());
    assert!(
        visible(&lines)
            .iter()
            .any(|line| line.contains("OVERLAY-TEXT"))
    );
}

// =============================================================================
// width percentage
// =============================================================================

/// `should render overlay at percentage of terminal width`
#[test]
fn should_render_overlay_at_percentage_of_terminal_width() {
    let (mut base, _) = base(100, 24);
    let overlay = StaticOverlay::new(&["test"]);
    let requested_width = overlay.width_handle();
    base.show_overlay(Box::new(overlay), options(SizeValue::Percent(50.0)));
    let _ = render(&mut base, 100, 24);
    assert_eq!(requested_width.get(), 50);
}

/// `should respect minWidth when widthPercent results in smaller width`
#[test]
fn should_respect_min_width_when_width_percent_results_in_smaller_width() {
    let (mut base, _) = base(100, 24);
    let overlay = StaticOverlay::new(&["test"]);
    let requested_width = overlay.width_handle();
    let mut opts = options(SizeValue::Percent(10.0));
    opts.min_width = Some(30.0);
    base.show_overlay(Box::new(overlay), opts);
    let _ = render(&mut base, 100, 24);
    assert_eq!(requested_width.get(), 30);
}

// =============================================================================
// anchor positioning
// =============================================================================

/// `should position overlay at top-left`
#[test]
fn should_position_overlay_at_top_left() {
    let (mut base, _) = base(80, 24);
    let mut opts = options(SizeValue::Absolute(10.0));
    opts.anchor = Some(OverlayAnchor::TopLeft);
    base.show_overlay(Box::new(StaticOverlay::new(&["TOP-LEFT"])), opts);
    let lines = visible(&render(&mut base, 80, 24));
    assert!(
        lines[0].starts_with("TOP-LEFT"),
        "Expected TOP-LEFT at start, got: {:?}",
        lines[0]
    );
}

/// `should position overlay at bottom-right`
#[test]
fn should_position_overlay_at_bottom_right() {
    let (mut base, _) = base(80, 24);
    let mut opts = options(SizeValue::Absolute(10.0));
    opts.anchor = Some(OverlayAnchor::BottomRight);
    base.show_overlay(Box::new(StaticOverlay::new(&["BTM-RIGHT"])), opts);
    let lines = visible(&render(&mut base, 80, 24));
    let last_row = &lines[23];
    assert!(
        last_row.contains("BTM-RIGHT"),
        "Expected BTM-RIGHT on last row, got: {last_row:?}"
    );
    assert!(
        last_row.trim_end().ends_with("BTM-RIGHT"),
        "Expected BTM-RIGHT at end, got: {last_row:?}"
    );
}

/// `should position overlay at top-center`
#[test]
fn should_position_overlay_at_top_center() {
    let (mut base, _) = base(80, 24);
    let mut opts = options(SizeValue::Absolute(10.0));
    opts.anchor = Some(OverlayAnchor::TopCenter);
    base.show_overlay(Box::new(StaticOverlay::new(&["CENTERED"])), opts);
    let lines = visible(&render(&mut base, 80, 24));
    let first_row = &lines[0];
    assert!(
        first_row.contains("CENTERED"),
        "Expected CENTERED on first row, got: {first_row:?}"
    );
    let col = first_row.find("CENTERED").expect("contains CENTERED");
    assert!((30..=40).contains(&col), "Expected centered, got col {col}");
}

// =============================================================================
// margin
// =============================================================================

/// `should clamp negative margins to zero`
#[test]
fn should_clamp_negative_margins_to_zero() {
    let (mut base, _) = base(80, 24);
    let mut opts = options(SizeValue::Absolute(12.0));
    opts.anchor = Some(OverlayAnchor::TopLeft);
    opts.margin = Some(OverlayMarginValue::Sides(OverlayMargin {
        top: Some(-5.0),
        left: Some(-10.0),
        right: Some(0.0),
        bottom: Some(0.0),
    }));
    base.show_overlay(Box::new(StaticOverlay::new(&["NEG-MARGIN"])), opts);
    let lines = visible(&render(&mut base, 80, 24));
    assert!(
        lines[0].starts_with("NEG-MARGIN"),
        "Expected NEG-MARGIN at start of row 0, got: {:?}",
        lines[0]
    );
}

/// `should respect margin as number`
#[test]
fn should_respect_margin_as_number() {
    let (mut base, _) = base(80, 24);
    let mut opts = options(SizeValue::Absolute(10.0));
    opts.anchor = Some(OverlayAnchor::TopLeft);
    opts.margin = Some(OverlayMarginValue::All(5.0));
    base.show_overlay(Box::new(StaticOverlay::new(&["MARGIN"])), opts);
    let lines = visible(&render(&mut base, 80, 24));

    assert!(!lines[0].contains("MARGIN"), "Should not be on row 0");
    assert!(!lines[4].contains("MARGIN"), "Should not be on row 4");
    assert!(
        lines[5].contains("MARGIN"),
        "Expected MARGIN on row 5, got: {:?}",
        lines[5]
    );
    let col = lines[5].find("MARGIN").expect("contains MARGIN");
    assert_eq!(col, 5, "Expected col 5, got {col}");
}

/// `should respect margin object`
#[test]
fn should_respect_margin_object() {
    let (mut base, _) = base(80, 24);
    let mut opts = options(SizeValue::Absolute(10.0));
    opts.anchor = Some(OverlayAnchor::TopLeft);
    opts.margin = Some(OverlayMarginValue::Sides(OverlayMargin {
        top: Some(2.0),
        left: Some(3.0),
        right: Some(0.0),
        bottom: Some(0.0),
    }));
    base.show_overlay(Box::new(StaticOverlay::new(&["MARGIN"])), opts);
    let lines = visible(&render(&mut base, 80, 24));

    assert!(
        lines[2].contains("MARGIN"),
        "Expected MARGIN on row 2, got: {:?}",
        lines[2]
    );
    let col = lines[2].find("MARGIN").expect("contains MARGIN");
    assert_eq!(col, 3, "Expected col 3, got {col}");
}

// =============================================================================
// offset
// =============================================================================

/// `should apply offsetX and offsetY from anchor position`
#[test]
fn should_apply_offset_x_and_offset_y_from_anchor_position() {
    let (mut base, _) = base(80, 24);
    let mut opts = options(SizeValue::Absolute(10.0));
    opts.anchor = Some(OverlayAnchor::TopLeft);
    opts.offset_x = Some(10.0);
    opts.offset_y = Some(5.0);
    base.show_overlay(Box::new(StaticOverlay::new(&["OFFSET"])), opts);
    let lines = visible(&render(&mut base, 80, 24));

    assert!(
        lines[5].contains("OFFSET"),
        "Expected OFFSET on row 5, got: {:?}",
        lines[5]
    );
    let col = lines[5].find("OFFSET").expect("contains OFFSET");
    assert_eq!(col, 10, "Expected col 10, got {col}");
}

// =============================================================================
// percentage positioning
// =============================================================================

/// `should position with rowPercent and colPercent`
#[test]
fn should_position_with_row_percent_and_col_percent() {
    let (mut base, _) = base(80, 24);
    let mut opts = options(SizeValue::Absolute(10.0));
    opts.row = Some(SizeValue::Percent(50.0));
    opts.col = Some(SizeValue::Percent(50.0));
    base.show_overlay(Box::new(StaticOverlay::new(&["PCT"])), opts);
    let lines = visible(&render(&mut base, 80, 24));

    let found_row = lines.iter().position(|line| line.contains("PCT"));
    let found_row = found_row.expect("PCT should be rendered");
    assert!(
        (10..=13).contains(&found_row),
        "Expected centered row, got {found_row}"
    );
}

/// `rowPercent 0 should position at top`
#[test]
fn row_percent_0_should_position_at_top() {
    let (mut base, _) = base(80, 24);
    let mut opts = options(SizeValue::Absolute(10.0));
    opts.row = Some(SizeValue::Percent(0.0));
    base.show_overlay(Box::new(StaticOverlay::new(&["TOP"])), opts);
    let lines = visible(&render(&mut base, 80, 24));
    assert!(
        lines[0].contains("TOP"),
        "Expected TOP on row 0, got: {:?}",
        lines[0]
    );
}

/// `rowPercent 100 should position at bottom`
#[test]
fn row_percent_100_should_position_at_bottom() {
    let (mut base, _) = base(80, 24);
    let mut opts = options(SizeValue::Absolute(10.0));
    opts.row = Some(SizeValue::Percent(100.0));
    base.show_overlay(Box::new(StaticOverlay::new(&["BOTTOM"])), opts);
    let lines = visible(&render(&mut base, 80, 24));
    assert!(
        lines[23].contains("BOTTOM"),
        "Expected BOTTOM on last row, got: {:?}",
        lines[23]
    );
}

// =============================================================================
// maxHeight
// =============================================================================

/// `should truncate overlay to maxHeight`
#[test]
fn should_truncate_overlay_to_max_height() {
    let (mut base, _) = base(80, 24);
    let opts = OverlayOptions {
        max_height: Some(SizeValue::Absolute(3.0)),
        ..OverlayOptions::default()
    };
    base.show_overlay(
        Box::new(StaticOverlay::new(&[
            "Line 1", "Line 2", "Line 3", "Line 4", "Line 5",
        ])),
        opts,
    );
    let content = visible(&render(&mut base, 80, 24)).join("\n");
    assert!(content.contains("Line 1"), "Should include Line 1");
    assert!(content.contains("Line 2"), "Should include Line 2");
    assert!(content.contains("Line 3"), "Should include Line 3");
    assert!(!content.contains("Line 4"), "Should NOT include Line 4");
    assert!(!content.contains("Line 5"), "Should NOT include Line 5");
}

/// `should truncate overlay to maxHeightPercent`
#[test]
fn should_truncate_overlay_to_max_height_percent() {
    let (mut base, _) = base(80, 10);
    let opts = OverlayOptions {
        max_height: Some(SizeValue::Percent(50.0)),
        ..OverlayOptions::default()
    };
    base.show_overlay(
        Box::new(StaticOverlay::new(&[
            "L1", "L2", "L3", "L4", "L5", "L6", "L7", "L8", "L9", "L10",
        ])),
        opts,
    );
    let content = visible(&render(&mut base, 80, 10)).join("\n");
    assert!(content.contains("L1"), "Should include L1");
    assert!(content.contains("L5"), "Should include L5");
    assert!(!content.contains("L6"), "Should NOT include L6");
}

// =============================================================================
// absolute positioning
// =============================================================================

/// `row and col should override anchor`
#[test]
fn row_and_col_should_override_anchor() {
    let (mut base, _) = base(80, 24);
    let mut opts = options(SizeValue::Absolute(10.0));
    opts.anchor = Some(OverlayAnchor::BottomRight);
    opts.row = Some(SizeValue::Absolute(3.0));
    opts.col = Some(SizeValue::Absolute(5.0));
    base.show_overlay(Box::new(StaticOverlay::new(&["ABSOLUTE"])), opts);
    let lines = visible(&render(&mut base, 80, 24));

    assert!(
        lines[3].contains("ABSOLUTE"),
        "Expected ABSOLUTE on row 3, got: {:?}",
        lines[3]
    );
    let col = lines[3].find("ABSOLUTE").expect("contains ABSOLUTE");
    assert_eq!(col, 5, "Expected col 5, got {col}");
}

// =============================================================================
// stacked overlays
// =============================================================================

/// `should render multiple overlays with later ones on top`
#[test]
fn should_render_multiple_overlays_with_later_ones_on_top() {
    let (mut base, _) = base(80, 24);
    let mut first = options(SizeValue::Absolute(20.0));
    first.anchor = Some(OverlayAnchor::TopLeft);
    base.show_overlay(Box::new(StaticOverlay::new(&["FIRST-OVERLAY"])), first);

    let mut second = options(SizeValue::Absolute(10.0));
    second.anchor = Some(OverlayAnchor::TopLeft);
    base.show_overlay(Box::new(StaticOverlay::new(&["SECOND"])), second);

    let lines = visible(&render(&mut base, 80, 24));
    assert!(
        lines[0].contains("SECOND"),
        "Expected SECOND on row 0, got: {:?}",
        lines[0]
    );
}

/// `should handle overlays at different positions without interference`
#[test]
fn should_handle_overlays_at_different_positions_without_interference() {
    let (mut base, _) = base(80, 24);
    let mut top_left = options(SizeValue::Absolute(15.0));
    top_left.anchor = Some(OverlayAnchor::TopLeft);
    base.show_overlay(Box::new(StaticOverlay::new(&["TOP-LEFT"])), top_left);

    let mut bottom_right = options(SizeValue::Absolute(15.0));
    bottom_right.anchor = Some(OverlayAnchor::BottomRight);
    base.show_overlay(Box::new(StaticOverlay::new(&["BTM-RIGHT"])), bottom_right);

    let lines = visible(&render(&mut base, 80, 24));
    assert!(
        lines[0].contains("TOP-LEFT"),
        "Expected TOP-LEFT on row 0, got: {:?}",
        lines[0]
    );
    assert!(
        lines[23].contains("BTM-RIGHT"),
        "Expected BTM-RIGHT on row 23, got: {:?}",
        lines[23]
    );
}

/// `should properly hide overlays in stack order`
#[test]
fn should_properly_hide_overlays_in_stack_order() {
    let (mut base, _) = base(80, 24);
    let mut first = options(SizeValue::Absolute(10.0));
    first.anchor = Some(OverlayAnchor::TopLeft);
    base.show_overlay(Box::new(StaticOverlay::new(&["FIRST"])), first);

    let mut second = options(SizeValue::Absolute(10.0));
    second.anchor = Some(OverlayAnchor::TopLeft);
    base.show_overlay(Box::new(StaticOverlay::new(&["SECOND"])), second);

    let lines = visible(&render(&mut base, 80, 24));
    assert!(
        lines[0].contains("SECOND"),
        "SECOND should be visible initially"
    );

    base.hide_overlay();
    let lines = visible(&render(&mut base, 80, 24));
    assert!(
        lines[0].contains("FIRST"),
        "FIRST should be visible after hiding SECOND"
    );
}

// =============================================================================
// hiding after stop（https://github.com/earendil-works/kiana/issues/10026）
// =============================================================================

/// `hideOverlay() leaves the cursor visible`
#[test]
fn hide_overlay_leaves_the_cursor_visible() {
    let (mut base, cursor_visible) = base(80, 24);
    base.start();
    let opts = OverlayOptions {
        non_capturing: Some(true),
        ..OverlayOptions::default()
    };
    base.show_overlay(Box::new(StaticOverlay::new(&["OVERLAY"])), opts);

    base.stop(TuiStopOptions::default());
    base.hide_overlay();

    assert!(cursor_visible.get(), "cursor should stay visible");
}

/// `overlay handle hide() leaves the cursor visible`
#[test]
fn overlay_handle_hide_leaves_the_cursor_visible() {
    let (mut base, cursor_visible) = base(80, 24);
    base.start();
    let opts = OverlayOptions {
        non_capturing: Some(true),
        ..OverlayOptions::default()
    };
    let mut handle: Box<dyn OverlayHandle> =
        base.show_overlay(Box::new(StaticOverlay::new(&["OVERLAY"])), opts);

    base.stop(TuiStopOptions::default());
    handle.hide();

    assert!(cursor_visible.get(), "cursor should stay visible");
}

/// 辅助函数的自检（TS 无对应）：列号按可见宽度计算，宽字符不会被算成 2 个索引。
#[test]
fn column_of_counts_visible_columns() {
    assert_eq!(column_of("ab│X", "X"), Some(3));
    assert_eq!(column_of("中X", "X"), Some(2));
}
