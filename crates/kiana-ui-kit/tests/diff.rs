//! `renderDiff` 的集成测试（对应 TS `diff.ts` 的行为；算法黄金样张在 `src/diff.rs` 单测里）。

mod common;

use kiana_theme::ThemeColor;
use kiana_ui_kit::{RenderDiffOptions, render_diff};

fn removed(text: &str) -> String {
    common::fg(ThemeColor::ToolDiffRemoved, text)
}

fn added(text: &str) -> String {
    common::fg(ThemeColor::ToolDiffAdded, text)
}

fn context(text: &str) -> String {
    common::fg(ThemeColor::ToolDiffContext, text)
}

#[test]
fn single_removed_added_pair_uses_intra_line_diff() {
    common::setup();
    let output = render_diff("-1 foo bar\n+1 foo baz", None);
    let expected = format!(
        "{}\n{}",
        removed(&format!("-1 foo {}", common::inverse("bar"))),
        added(&format!("+1 foo {}", common::inverse("baz")))
    );
    assert_eq!(output, expected);
}

#[test]
fn intra_line_diff_strips_leading_whitespace_of_first_part() {
    common::setup();
    let output = render_diff("-1   indented\n+1   indent", None);
    let expected = format!(
        "{}\n{}",
        removed(&format!("-1   {}", common::inverse("indented"))),
        added(&format!("+1   {}", common::inverse("indent")))
    );
    assert_eq!(output, expected);
}

#[test]
fn multi_line_blocks_show_all_removed_then_all_added() {
    common::setup();
    let output = render_diff("-1 a\n-2 b\n+1 c\n+2 d", None);
    let expected = [
        removed("-1 a"),
        removed("-2 b"),
        added("+1 c"),
        added("+2 d"),
    ]
    .join("\n");
    assert_eq!(output, expected);
}

#[test]
fn removed_block_without_added_lines() {
    common::setup();
    let output = render_diff("-1 a\n 2 b", None);
    let expected = [removed("-1 a"), context(" 2 b")].join("\n");
    assert_eq!(output, expected);
}

#[test]
fn standalone_added_line_and_context_lines() {
    common::setup();
    let output = render_diff("+2 hi\n 3 ctx", None);
    let expected = [added("+2 hi"), context(" 3 ctx")].join("\n");
    assert_eq!(output, expected);

    // 单独的 `+` 与 `-`（不满足正则，需要 lineNum 后的空格）走 context 分支。
    assert_eq!(render_diff("+", None), context("+"));
    assert_eq!(render_diff("-", None), context("-"));
}

#[test]
fn non_matching_lines_are_not_tab_replaced() {
    common::setup();
    assert_eq!(render_diff("a\tb", None), context("a\tb"));
    assert_eq!(
        render_diff("\\ No newline at end of file", None),
        context("\\ No newline at end of file")
    );
    assert_eq!(
        render_diff("--- a/file\n+++ b/file", None),
        [context("--- a/file"), context("+++ b/file")].join("\n")
    );
}

#[test]
fn matching_lines_replace_tabs() {
    common::setup();
    // context 行：前缀空白被归一为一个空格，内容里的 tab → 3 个空格。
    assert_eq!(render_diff("\t1 x", None), context(" 1 x"));
    assert_eq!(render_diff(" 1 x", None), context(" 1 x"));
    assert_eq!(render_diff(" 1 a\tb", None), context(" 1 a   b"));
    assert_eq!(render_diff("+2 a\tb", None), added("+2 a   b"));
    assert_eq!(render_diff("-1 a\tb", None), removed("-1 a   b"));
}

#[test]
fn line_numbers_are_preserved_verbatim() {
    common::setup();
    // 捕获组 2 是 `\s*\d*`：`"  12"` → lineNum `" 12"`（前导空白保留）。
    assert_eq!(render_diff("  12 abc", None), context("  12 abc"));
    // 非 ASCII 空白（NBSP）也是 JS 的 `\s`。
    assert_eq!(render_diff("\u{00a0}1\u{00a0}x", None), context(" 1 x"));
}

#[test]
fn trailing_newline_produces_an_empty_context_line() {
    common::setup();
    let output = render_diff("abc\n", None);
    assert_eq!(output, format!("{}\n{}", context("abc"), context("")));
    assert_eq!(output.split('\n').count(), 2);

    let empty = render_diff("", None);
    assert_eq!(empty, context(""));
}

#[test]
fn crlf_lines_fail_to_parse_and_stay_context_colored() {
    common::setup();
    // JS 的 `.` 不匹配 `\r`，因此 CRLF 行整体不匹配 parseDiffLine。
    let output = render_diff("-1 a\r\n+1 b\r\n", None);
    let expected = [context("-1 a\r"), context("+1 b\r"), context("")].join("\n");
    assert_eq!(output, expected);
}

#[test]
fn options_are_accepted_but_unused() {
    common::setup();
    let without = render_diff("-1 foo\n+1 bar", None);
    let with = render_diff(
        "-1 foo\n+1 bar",
        Some(&RenderDiffOptions {
            file_path: Some("file.rs".to_string()),
        }),
    );
    assert_eq!(without, with);
    assert_eq!(RenderDiffOptions::default().file_path, None);
}

#[test]
fn multiple_alternating_blocks_keep_their_order() {
    common::setup();
    let output = render_diff("-1 foo\n+1 bar\n 2 ctx\n+3 more\n-4 less", None);
    let expected = [
        removed(&format!("-1 {}", common::inverse("foo"))),
        added(&format!("+1 {}", common::inverse("bar"))),
        context(" 2 ctx"),
        added("+3 more"),
        removed("-4 less"),
    ]
    .join("\n");
    assert_eq!(output, expected);
}
