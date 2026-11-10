//! `truncateToVisualLines` / `VisualLinePreview` 的测试。

mod common;

use std::cell::Cell;
use std::rc::Rc;

use kiana_tui::{Component, truncate_to_width, visible_width};
use kiana_ui_kit::{Keep, VisualLinePreview, VisualLinePreviewOptions, truncate_to_visual_lines};

fn ten_lines() -> String {
    (1..=10)
        .map(|n| format!("l{n}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn empty_text_returns_empty_result() {
    common::setup();
    for keep in [Keep::Start, Keep::End] {
        let result = truncate_to_visual_lines("", 3, 80, 0, keep);
        assert!(result.visual_lines.is_empty());
        assert_eq!(result.skipped_count, 0);
    }
}

#[test]
fn keeps_start_or_end_lines() {
    common::setup();
    let text = ten_lines();

    let start = truncate_to_visual_lines(&text, 3, 80, 0, Keep::Start);
    assert_eq!(start.skipped_count, 7);
    assert_eq!(start.visual_lines.len(), 3);
    assert!(kiana_tui::strip_terminal_sequences(&start.visual_lines[0]).starts_with("l1"));
    assert!(kiana_tui::strip_terminal_sequences(&start.visual_lines[2]).starts_with("l3"));

    let end = truncate_to_visual_lines(&text, 3, 80, 0, Keep::End);
    assert_eq!(end.skipped_count, 7);
    assert_eq!(end.visual_lines.len(), 3);
    assert!(kiana_tui::strip_terminal_sequences(&end.visual_lines[0]).starts_with("l8"));
    assert!(kiana_tui::strip_terminal_sequences(&end.visual_lines[2]).starts_with("l10"));
}

#[test]
fn no_truncation_when_within_limit() {
    common::setup();
    let text = ten_lines();
    for max in [10usize, 11, 50] {
        let result = truncate_to_visual_lines(&text, max, 80, 0, Keep::End);
        assert_eq!(result.visual_lines.len(), 10);
        assert_eq!(result.skipped_count, 0);
    }
}

#[test]
fn max_zero_matches_js_slice_negative_zero() {
    common::setup();
    let text = ten_lines();

    // JS `slice(-0)` === `slice(0)` → 返回全部行，skippedCount = len。
    let end = truncate_to_visual_lines(&text, 0, 80, 0, Keep::End);
    assert_eq!(end.visual_lines.len(), 10);
    assert_eq!(end.skipped_count, 10);

    // `slice(0, 0)` → 空数组。
    let start = truncate_to_visual_lines(&text, 0, 80, 0, Keep::Start);
    assert!(start.visual_lines.is_empty());
    assert_eq!(start.skipped_count, 10);
}

#[test]
fn padding_x_is_applied_by_the_temporary_text() {
    common::setup();
    let text = ten_lines();
    let result = truncate_to_visual_lines(&text, 3, 20, 1, Keep::Start);
    assert_eq!(result.visual_lines.len(), 3);
    for line in &result.visual_lines {
        assert_eq!(visible_width(line), 20);
        let plain = kiana_tui::strip_terminal_sequences(line);
        assert!(plain.starts_with(' ') && plain.ends_with(' '));
    }
}

#[test]
fn long_lines_wrap_before_truncation() {
    common::setup();
    let long = "x".repeat(50);
    let result = truncate_to_visual_lines(&long, 2, 10, 0, Keep::Start);
    assert_eq!(
        result.skipped_count, 3,
        "50 chars at width 10 wrap to 5 lines"
    );
    assert_eq!(result.visual_lines.len(), 2);
    for line in &result.visual_lines {
        assert_eq!(visible_width(line), 10);
    }
}

#[test]
fn preview_inserts_hint_and_caches_per_width() {
    common::setup();
    let hint_calls = Rc::new(Cell::new(0usize));
    let counter = Rc::clone(&hint_calls);
    let mut preview = VisualLinePreview::new(VisualLinePreviewOptions {
        text: "a\nb\nc\nd".to_string(),
        max_visual_lines: 2,
        keep: Keep::End,
        format_hint: Box::new(move |hidden| {
            counter.set(counter.get() + 1);
            format!("... {hidden} more lines")
        }),
    });

    let lines = preview.render(20);
    assert_eq!(lines.len(), 3, "hint + 2 kept lines");
    assert!(kiana_tui::strip_terminal_sequences(&lines[0]).contains("... 2 more lines"));
    assert!(kiana_tui::strip_terminal_sequences(&lines[1]).starts_with('c'));
    assert!(kiana_tui::strip_terminal_sequences(&lines[2]).starts_with('d'));
    assert_eq!(hint_calls.get(), 1);

    // 同宽度：命中缓存（不再调用 formatHint）。
    let again = preview.render(20);
    assert_eq!(again, lines);
    assert_eq!(hint_calls.get(), 1);

    // 换宽度：重算。
    let _ = preview.render(40);
    assert_eq!(hint_calls.get(), 2);

    // invalidate：重算。
    preview.invalidate();
    let _ = preview.render(20);
    assert_eq!(hint_calls.get(), 3);
}

#[test]
fn preview_puts_the_hint_last_for_keep_start() {
    common::setup();
    let mut preview = VisualLinePreview::new(VisualLinePreviewOptions {
        text: "a\nb\nc\nd".to_string(),
        max_visual_lines: 2,
        keep: Keep::Start,
        format_hint: Box::new(|hidden| format!("... {hidden} more lines")),
    });
    let lines = preview.render(40);
    assert_eq!(lines.len(), 3);
    assert!(kiana_tui::strip_terminal_sequences(&lines[0]).starts_with('a'));
    assert!(kiana_tui::strip_terminal_sequences(&lines[1]).starts_with('b'));
    assert!(kiana_tui::strip_terminal_sequences(&lines[2]).contains("... 2 more lines"));
}

#[test]
fn preview_hint_is_truncated_to_width() {
    common::setup();
    let mut preview = VisualLinePreview::new(VisualLinePreviewOptions {
        text: "a\nb\nc\nd".to_string(),
        max_visual_lines: 2,
        keep: Keep::End,
        format_hint: Box::new(|_| "a very long hint line that must be truncated".to_string()),
    });
    for width in common::WIDTHS {
        let lines = preview.render(width);
        for line in &lines {
            assert!(visible_width(line) <= width, "width {width}: {line:?}");
        }
    }
    let narrow = preview.render(4);
    assert_eq!(
        narrow[0],
        truncate_to_width(
            "a very long hint line that must be truncated",
            4,
            "...",
            false
        )
    );
}

#[test]
fn preview_edge_cases() {
    common::setup();
    // 空文本：0 行、无 hint。
    let mut empty = VisualLinePreview::new(VisualLinePreviewOptions {
        text: String::new(),
        max_visual_lines: 2,
        keep: Keep::End,
        format_hint: Box::new(|hidden| format!("{hidden}")),
    });
    assert!(empty.render(20).is_empty());

    // max = 0 且 keep=end：hint + 全部行。
    let mut zero = VisualLinePreview::new(VisualLinePreviewOptions {
        text: "a\nb".to_string(),
        max_visual_lines: 0,
        keep: Keep::End,
        format_hint: Box::new(|hidden| format!("{hidden} hidden")),
    });
    let lines = zero.render(20);
    assert_eq!(lines.len(), 3);
    assert!(kiana_tui::strip_terminal_sequences(&lines[0]).contains("2 hidden"));

    // set_options 清空缓存。
    let calls = Rc::new(Cell::new(0usize));
    let counter = Rc::clone(&calls);
    let mut preview = VisualLinePreview::new(VisualLinePreviewOptions {
        text: "a\nb\nc".to_string(),
        max_visual_lines: 1,
        keep: Keep::End,
        format_hint: Box::new(move |hidden| {
            counter.set(counter.get() + 1);
            format!("{hidden}")
        }),
    });
    let _ = preview.render(20);
    assert_eq!(calls.get(), 1);
    assert_eq!(preview.text(), "a\nb\nc");
    let second_counter = Rc::clone(&calls);
    preview.set_options(VisualLinePreviewOptions {
        text: "x\ny".to_string(),
        max_visual_lines: 1,
        keep: Keep::Start,
        format_hint: Box::new(move |hidden| {
            second_counter.set(second_counter.get() + 1);
            format!("{hidden}")
        }),
    });
    let _ = preview.render(20);
    assert_eq!(calls.get(), 2);
    assert_eq!(preview.text(), "x\ny");
}

#[test]
fn preview_renders_within_every_width() {
    common::setup();
    let mut preview = VisualLinePreview::new(VisualLinePreviewOptions {
        text: "short\nanother line that wraps at small widths because it is long\nlast".to_string(),
        max_visual_lines: 1,
        keep: Keep::End,
        format_hint: Box::new(|hidden| format!("... {hidden} more lines (ctrl+o to expand)")),
    });
    common::assert_width_invariant(&mut preview, &common::WIDTHS);
    assert!(!preview.handles_input());
}

#[test]
fn preview_without_skipped_lines_returns_the_lines_verbatim() {
    common::setup();
    for keep in [Keep::Start, Keep::End] {
        let hint_calls = Rc::new(Cell::new(0usize));
        let counter = Rc::clone(&hint_calls);
        let mut preview = VisualLinePreview::new(VisualLinePreviewOptions {
            text: "a\nb\nc".to_string(),
            max_visual_lines: 3,
            keep,
            format_hint: Box::new(move |hidden| {
                counter.set(counter.get() + 1);
                format!("{hidden}")
            }),
        });
        let lines = preview.render(80);
        assert_eq!(
            lines
                .iter()
                .map(|line| kiana_tui::strip_terminal_sequences(line)
                    .trim_end()
                    .to_string())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
        // skippedCount = 0 → no hint, no formatHint call.
        assert_eq!(hint_calls.get(), 0);
    }
}

#[test]
fn preview_handles_blank_and_whitespace_only_text() {
    common::setup();
    // TS `Text.render` 对「空串或 trim 后为空」的文本返回空数组（`text.ts`），因此
    // `truncateToVisualLines` 得到 0 行、无 hint。
    for text in ["\n\n", "   ", "\t\n "] {
        let mut preview = VisualLinePreview::new(VisualLinePreviewOptions {
            text: text.to_string(),
            max_visual_lines: 1,
            keep: Keep::End,
            format_hint: Box::new(|hidden| format!("{hidden} hidden")),
        });
        assert!(preview.render(20).is_empty(), "text {text:?}");
        common::assert_width_invariant(&mut preview, &common::WIDTHS);
    }
}
