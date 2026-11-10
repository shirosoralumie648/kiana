//! `test/regression-overlay-cjk-boundary.test.ts` 的逐条移植（4 个 it）。
//!
//! 这四个用例只依赖 `compositeTuiLine` 与 `utils.ts` 的分段/切片，不需要终端仿真，
//! 因此可以直接断言字符串（TS 版同样如此）。

use kiana_tui::text::{extract_segments, get_grapheme_cell_range, slice_by_column, visible_width};
use kiana_tui::tui::composite_tui_line;

/// `excludes a wide grapheme from before when overlay starts inside it`
#[test]
fn excludes_a_wide_grapheme_from_before_when_overlay_starts_inside_it() {
    let segments = extract_segments("abcd让EFGH", 5, 9, 11, true);
    assert_eq!(segments.before, "abcd");
    assert_eq!(segments.before_width, 4);
    assert_eq!(visible_width(&segments.before), segments.before_width);
    assert_eq!(segments.after, "H");
    assert_eq!(segments.after_width, 1);
}

/// `keeps ASCII before-segment behavior at the same boundary`
#[test]
fn keeps_ascii_before_segment_behavior_at_the_same_boundary() {
    let segments = extract_segments("abcdG EFGH", 5, 9, 11, true);
    assert_eq!(segments.before, "abcdG");
    assert_eq!(segments.before_width, 5);
    assert_eq!(visible_width(&segments.before), segments.before_width);
}

/// `composites an overlay at the requested column when it starts inside a wide grapheme`
#[test]
fn composites_an_overlay_at_the_requested_column_when_it_starts_inside_a_wide_grapheme() {
    let out = composite_tui_line("abcd让EFGH", "│XX│", 5, 4, 20);
    let prefix = slice_by_column(&out, 0, 5, true);
    let overlay = slice_by_column(&out, 5, 4, true);

    assert!(!out.contains("让"));
    assert_eq!(visible_width(&out), 20);
    assert_eq!(visible_width(&prefix), 5);
    assert_eq!(visible_width(&overlay), 4);
    assert!(overlay.contains("│XX│"));
}

/// `composites an overlay when it starts at a wide grapheme boundary`
#[test]
fn composites_an_overlay_when_it_starts_at_a_wide_grapheme_boundary() {
    let out = composite_tui_line("abcd让EFGH", "│XX│", 4, 4, 20);
    let overlay = slice_by_column(&out, 4, 4, true);

    assert!(!out.contains("让"));
    assert_eq!(visible_width(&out), 20);
    assert_eq!(visible_width(&overlay), 4);
    assert!(overlay.contains("│XX│"));
}

/// 附加自检：`getGraphemeCellRange` 按列工作（上面四条依赖它的列语义）。
///
/// TS 版没有这条断言，但在 Rust 里单独固定一次便于定位回归。
#[test]
fn grapheme_cell_range_reports_wide_grapheme_span() {
    let range = get_grapheme_cell_range("abcd让EFGH", 4).expect("列 4 落在 `让` 上");
    assert_eq!((range.start, range.end), (4, 6));
}
