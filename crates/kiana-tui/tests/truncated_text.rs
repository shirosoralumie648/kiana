//! `test/truncated-text.test.ts` 的 Rust 移植。

use kiana_tui::{Component, TruncatedText, strip_terminal_sequences, visible_width};

fn strip(line: &str) -> String {
    strip_terminal_sequences(line)
}

#[test]
fn pads_output_lines_to_exactly_match_width() {
    let mut text = TruncatedText::with_padding("Hello world", 1, 0);
    let lines = text.render(50);

    // 恰好一个内容行（没有纵向留白）。
    assert_eq!(lines.len(), 1);

    // 可见宽度恰好 50。
    assert_eq!(visible_width(&lines[0]), 50);
}

#[test]
fn pads_output_with_vertical_padding_lines_to_width() {
    let mut text = TruncatedText::with_padding("Hello", 0, 2);
    let lines = text.render(40);

    // 2 个上留白 + 1 个内容行 + 2 个下留白 = 5。
    assert_eq!(lines.len(), 5);

    for line in &lines {
        assert_eq!(visible_width(line), 40);
    }
}

#[test]
fn truncates_long_text_and_pads_to_width() {
    let long_text =
        "This is a very long piece of text that will definitely exceed the available width";
    let mut text = TruncatedText::with_padding(long_text, 1, 0);
    let lines = text.render(30);

    assert_eq!(lines.len(), 1);
    assert_eq!(visible_width(&lines[0]), 30);
    assert!(strip(&lines[0]).contains("..."));
}

#[test]
fn preserves_ansi_codes_in_output_and_pads_correctly() {
    // chalk.red("Hello") + " " + chalk.blue("world")（level 3 的等价转义序列）。
    let styled_text = "\x1b[31mHello\x1b[39m \x1b[34mworld\x1b[39m";
    let mut text = TruncatedText::with_padding(styled_text, 1, 0);
    let lines = text.render(40);

    assert_eq!(lines.len(), 1);
    assert_eq!(visible_width(&lines[0]), 40);
    assert!(lines[0].contains("\x1b["));
}

#[test]
fn truncates_styled_text_and_adds_reset_code_before_ellipsis() {
    let long_styled_text = "\x1b[31mThis is a very long red text that will be truncated\x1b[39m";
    let mut text = TruncatedText::with_padding(long_styled_text, 1, 0);
    let lines = text.render(20);

    assert_eq!(lines.len(), 1);
    assert_eq!(visible_width(&lines[0]), 20);
    assert!(lines[0].contains("\x1b[0m..."));
}

#[test]
fn handles_text_that_fits_exactly() {
    let mut text = TruncatedText::with_padding("Hello world", 1, 0);
    let lines = text.render(30);

    assert_eq!(lines.len(), 1);
    assert_eq!(visible_width(&lines[0]), 30);
    assert!(!strip(&lines[0]).contains("..."));
}

#[test]
fn handles_empty_text() {
    let mut text = TruncatedText::with_padding("", 1, 0);
    let lines = text.render(30);

    assert_eq!(lines.len(), 1);
    assert_eq!(visible_width(&lines[0]), 30);
}

#[test]
fn stops_at_newline_and_only_shows_first_line() {
    let mut text = TruncatedText::with_padding("First line\nSecond line\nThird line", 1, 0);
    let lines = text.render(40);

    assert_eq!(lines.len(), 1);
    assert_eq!(visible_width(&lines[0]), 40);
    let stripped = strip(&lines[0]).trim().to_string();
    assert!(stripped.contains("First line"));
    assert!(!stripped.contains("Second line"));
    assert!(!stripped.contains("Third line"));
}

#[test]
fn truncates_first_line_even_with_newlines_in_text() {
    let long_multiline_text = "This is a very long first line that needs truncation\nSecond line";
    let mut text = TruncatedText::with_padding(long_multiline_text, 1, 0);
    let lines = text.render(25);

    assert_eq!(lines.len(), 1);
    assert_eq!(visible_width(&lines[0]), 25);
    let stripped = strip(&lines[0]);
    assert!(stripped.contains("..."));
    assert!(!stripped.contains("Second line"));
}
