//! `test/regression-slice-by-column-ansi-order.test.ts`：`sliceByColumn` 的 ANSI 顺序回归。
//!
//! TS 源：`packages/tui/test/regression-slice-by-column-ansi-order.test.ts`（2 个 `it`）。

use kiana_tui::text::slice_by_column;

// https://github.com/earendil-works/kiana/issues/10169
#[test]
fn keeps_a_reset_at_the_slice_start_after_earlier_style_codes() {
    let line = "\x1b[32mfoo\x1b[39m bar";
    assert_eq!(slice_by_column(line, 3, 4, true), "\x1b[32m\x1b[39m bar");
}

#[test]
fn does_not_leak_color_into_text_after_a_highlighted_token() {
    let line = "Another \x1b[35malpha\x1b[39m line with \x1b[35mbeta\x1b[39m later.";
    let after = slice_by_column(line, 13, 100, true);
    assert_eq!(
        after,
        "\x1b[35m\x1b[39m line with \x1b[35mbeta\x1b[39m later."
    );
}
