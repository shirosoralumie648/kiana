//! 移植自 `packages/durable/test/env-truncate.test.ts`。
//!
//! 原测试用子进程验证“没有 `Buffer` 的运行时”也按 UTF-8 字节计数；Rust 没有这个运行期分支，
//! 该用例改成本地直接断言同样的字节长度与截断结果。
//!
//! 偏差：TS 用 `"\ud83d"` / `"\ude42"` 这样的孤立代理项作为输入；Rust `String` 无法表示，
//! 因此这两项不参与（见交付说明）。

use kiana_durable::truncate::{
    TruncatedBy, TruncationOptions, format_size, truncate_head, utf8_byte_length,
};

fn truncate(
    content: &str,
    max_lines: usize,
    max_bytes: usize,
) -> kiana_durable::truncate::TruncationResult {
    truncate_head(
        content,
        &TruncationOptions::new()
            .with_max_lines(max_lines)
            .with_max_bytes(max_bytes),
    )
}

#[test]
fn reports_utf8_byte_counts_in_truncation_results() {
    let content = "aé🙂\nb";
    let result = truncate(content, 100, 10);

    assert!(!result.truncated);
    assert_eq!(result.total_bytes, utf8_byte_length(content));
    assert_eq!(result.output_bytes, utf8_byte_length(content));
    assert_eq!(result.total_bytes, 9);
}

#[test]
fn counts_utf8_bytes_without_a_buffer_api() {
    let inputs = [
        "",
        "ascii",
        "é",
        "中",
        "🙂",
        "a🙂b",
        "\u{7ff}\u{800}\u{ffff}",
    ];
    let expected = [0usize, 5, 2, 3, 4, 6, 8];
    let lengths: Vec<usize> = inputs.iter().map(|input| utf8_byte_length(input)).collect();
    assert_eq!(lengths, expected);

    let head = truncate("aé🙂\nb", 10, 7);
    assert_eq!(head.content, "aé🙂");
    assert_eq!(head.output_bytes, 7);
    assert_eq!(head.truncated_by, Some(TruncatedBy::Bytes));
}

#[test]
fn does_not_count_a_trailing_newline_as_an_extra_line() {
    let content = "line\nline\nline\n";
    let head = truncate(content, 3, 100);

    assert!(!head.truncated);
    assert_eq!(head.total_lines, 3);
    assert_eq!(head.output_lines, 3);
}

#[test]
fn truncates_head_by_line_limits() {
    let content = "one\ntwo\nthree\nfour";
    let head = truncate(content, 2, 100);

    assert_eq!(head.content, "one\ntwo");
    assert!(head.truncated);
    assert_eq!(head.truncated_by, Some(TruncatedBy::Lines));
    assert_eq!(head.total_lines, 4);
    assert_eq!(head.output_lines, 2);
}

#[test]
fn reports_bytes_when_only_a_trailing_newline_exceeds_limits_at_the_line_cap() {
    let head = truncate("hello\nworld\n", 2, 11);

    assert_eq!(head.content, "hello\nworld");
    assert!(head.truncated);
    assert_eq!(head.truncated_by, Some(TruncatedBy::Bytes));
    assert_eq!(head.total_lines, 2);
    assert_eq!(head.output_lines, 2);
}

#[test]
fn truncates_head_on_utf8_byte_limits_without_partial_lines() {
    let content = "éé\nabc";
    let result = truncate(content, 10, 4);

    assert_eq!(result.content, "éé");
    assert!(result.truncated);
    assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
    assert_eq!(result.output_bytes, 4);
    assert!(!result.first_line_exceeds_limit);
}

#[test]
fn reports_head_truncation_when_the_first_line_exceeds_the_byte_limit() {
    let result = truncate("éé\nabc", 10, 3);

    assert_eq!(result.content, "");
    assert!(result.truncated);
    assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
    assert!(result.first_line_exceeds_limit);
}

#[test]
fn formats_sizes() {
    assert_eq!(format_size(1023), "1023B");
    assert_eq!(format_size(1536), "1.5KB");
    assert_eq!(format_size(3 * 1024 * 1024), "3.0MB");
}
