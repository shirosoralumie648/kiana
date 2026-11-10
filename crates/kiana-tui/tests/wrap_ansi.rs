//! `test/wrap-ansi.test.ts`：`wrapTextWithAnsi` 的换行与 ANSI 状态延续。
//!
//! TS 源：`packages/tui/test/wrap-ansi.test.ts`（19 个用例，标题保留在测试函数文档中以便对照）。

use kiana_tui::text::{visible_width, wrap_text_with_ansi};
use regex::Regex;

// ---------------------------------------------------------------------------
// describe("wrapTextWithAnsi") / describe("underline styling")
// ---------------------------------------------------------------------------

#[test]
fn should_not_apply_underline_style_before_the_styled_text() {
    let underline_on = "\x1b[4m";
    let underline_off = "\x1b[24m";
    let url = "https://example.com/very/long/path/that/will/wrap";
    let text = format!("read this thread {underline_on}{url}{underline_off}");

    let wrapped = wrap_text_with_ansi(&text, 40);

    // 第一行只有 "read this thread"，不应带下划线
    assert_eq!(wrapped[0], "read this thread");

    // 第二行应以下划线开头并包含 URL 内容
    assert!(
        wrapped[1].starts_with(underline_on),
        "line = {:?}",
        wrapped[1]
    );
    assert!(wrapped[1].contains("https://"), "line = {:?}", wrapped[1]);
}

#[test]
fn should_not_have_whitespace_before_underline_reset_code() {
    let underline_on = "\x1b[4m";
    let underline_off = "\x1b[24m";
    let text = format!("{underline_on}underlined text here {underline_off}more");

    let wrapped = wrap_text_with_ansi(&text, 18);

    assert!(
        !wrapped[0].contains(&format!(" {underline_off}")),
        "line = {:?}",
        wrapped[0]
    );
}

#[test]
fn should_not_bleed_underline_to_padding_each_line_should_end_with_reset_for_underline_only() {
    let underline_on = "\x1b[4m";
    let underline_off = "\x1b[24m";
    let url = "https://example.com/very/long/path/that/will/definitely/wrap";
    let text = format!("prefix {underline_on}{url}{underline_off} suffix");

    let wrapped = wrap_text_with_ansi(&text, 30);

    // 含下划线内容的时间行应以 underline-off 结尾，而不是全量 reset
    for line in &wrapped[1..wrapped.len() - 1] {
        if line.contains(underline_on) {
            assert!(line.ends_with(underline_off), "line = {line:?}");
            assert!(!line.ends_with("\x1b[0m"), "line = {line:?}");
        }
    }
}

// ---------------------------------------------------------------------------
// describe("background color preservation")
// ---------------------------------------------------------------------------

#[test]
fn should_preserve_background_color_across_wrapped_lines_without_full_reset() {
    let bg_blue = "\x1b[44m";
    let reset = "\x1b[0m";
    let text = format!("{bg_blue}hello world this is blue background text{reset}");

    let wrapped = wrap_text_with_ansi(&text, 15);

    for line in &wrapped {
        assert!(line.contains(bg_blue), "line = {line:?}");
    }

    // 中间行不应以全量 reset 结尾（那会连带清掉 padding 的背景色）
    for line in &wrapped[..wrapped.len() - 1] {
        assert!(!line.ends_with("\x1b[0m"), "line = {line:?}");
    }
}

#[test]
fn should_reset_underline_but_preserve_background_when_wrapping_underlined_text_inside_background()
{
    let underline_on = "\x1b[4m";
    let underline_off = "\x1b[24m";
    let reset = "\x1b[0m";

    let text = format!(
        "\x1b[41mprefix {underline_on}UNDERLINED_CONTENT_THAT_WRAPS{underline_off} suffix{reset}"
    );

    let wrapped = wrap_text_with_ansi(&text, 20);

    // 每一行都应带背景色 41（可能是 \x1b[41m，也可能是 \x1b[4;41m 这样的合并码）
    for line in &wrapped {
        let has_bg_color = line.contains("[41m") || line.contains(";41m") || line.contains("[41;");
        assert!(has_bg_color, "line = {line:?}");
    }

    // 含下划线内容的时间行应以 underline-off 结尾，而不是全量 reset
    for line in &wrapped[..wrapped.len() - 1] {
        let has_underline = line.contains("[4m") || line.contains("[4;") || line.contains(";4m");
        if has_underline && !line.contains(underline_off) {
            assert!(line.ends_with(underline_off), "line = {line:?}");
            assert!(!line.ends_with("\x1b[0m"), "line = {line:?}");
        }
    }
}

// ---------------------------------------------------------------------------
// describe("basic wrapping")
// ---------------------------------------------------------------------------

#[test]
fn should_handle_lf_crlf_and_cr_line_endings() {
    assert_eq!(
        wrap_text_with_ansi("first\nsecond\r\nthird\rfourth", 80),
        vec!["first", "second", "third", "fourth"]
    );
}

#[test]
fn should_preserve_ansi_state_across_crlf_and_cr_line_endings() {
    let red = "\x1b[31m";
    let reset = "\x1b[0m";

    assert_eq!(
        wrap_text_with_ansi(&format!("{red}first\r\nsecond\rthird{reset}"), 80),
        vec![
            format!("{red}first"),
            format!("{red}second"),
            format!("{red}third{reset}"),
        ]
    );
}

#[test]
fn should_wrap_plain_text_correctly() {
    let text = "hello world this is a test";
    let wrapped = wrap_text_with_ansi(text, 10);

    assert!(wrapped.len() > 1);
    for line in &wrapped {
        assert!(visible_width(line) <= 10, "line = {line:?}");
    }
}

#[test]
fn should_break_cjk_runs_at_grapheme_boundaries_after_latin_text() {
    let text = "This is an example 中文汉字测试段落内容中文汉字测试段落内容.";
    let wrapped = wrap_text_with_ansi(text, 40);

    assert_eq!(
        wrapped,
        vec![
            "This is an example 中文汉字测试段落内容",
            "中文汉字测试段落内容."
        ]
    );
    for line in &wrapped {
        assert!(visible_width(line) <= 40, "line = {line:?}");
    }
}

#[test]
fn should_preserve_color_codes_when_wrapping_cjk_runs() {
    let red = "\x1b[31m";
    let reset = "\x1b[0m";
    let text = format!("{red}This is an example 中文汉字测试段落内容中文汉字测试段落内容.{reset}");
    let wrapped = wrap_text_with_ansi(&text, 40);

    assert_eq!(wrapped.len(), 2);
    assert_eq!(
        wrapped[0],
        format!("{red}This is an example 中文汉字测试段落内容")
    );
    assert_eq!(wrapped[1], format!("{red}中文汉字测试段落内容.{reset}"));
    for line in &wrapped {
        assert!(visible_width(line) <= 40, "line = {line:?}");
    }
}

#[test]
fn should_ignore_osc_133_semantic_markers_in_visible_width() {
    let text = "\x1b]133;A\x07hello\x1b]133;B\x07";
    assert_eq!(visible_width(text), 5);
}

#[test]
fn should_ignore_osc_sequences_terminated_with_st_in_visible_width() {
    let text = "\x1b]133;A\x1b\\hello\x1b]133;B\x1b\\";
    assert_eq!(visible_width(text), 5);
}

#[test]
fn should_treat_isolated_regional_indicators_as_width_2() {
    assert_eq!(visible_width("🇨"), 2);
    assert_eq!(visible_width("🇨🇳"), 2);
}

#[test]
fn should_truncate_trailing_whitespace_that_exceeds_width() {
    let two_spaces_wrapped_to_width_1 = wrap_text_with_ansi("  ", 1);
    assert!(visible_width(&two_spaces_wrapped_to_width_1[0]) <= 1);
}

#[test]
fn should_preserve_color_codes_across_wraps() {
    let red = "\x1b[31m";
    let reset = "\x1b[0m";
    let text = format!("{red}hello world this is red{reset}");

    let wrapped = wrap_text_with_ansi(&text, 10);

    // 每个续行都应以红色码开头
    for line in &wrapped[1..] {
        assert!(line.starts_with(red), "line = {line:?}");
    }

    // 中间行不应以全量 reset 结尾
    for line in &wrapped[..wrapped.len() - 1] {
        assert!(!line.ends_with("\x1b[0m"), "line = {line:?}");
    }
}

// ---------------------------------------------------------------------------
// describe("wrapTextWithAnsi with OSC 8 hyperlinks")
// ---------------------------------------------------------------------------

#[test]
fn re_emits_osc_8_open_at_the_start_of_continuation_lines() {
    // 超链接文本长到需要换行
    let url = "https://example.com";
    // OSC 8 open + 10 个可见字符 + OSC 8 close
    let input = format!("\x1b]8;;{url}\x1b\\0123456789\x1b]8;;\x1b\\");
    let lines = wrap_text_with_ansi(&input, 6);

    let osc8_st = Regex::new(r"\x1b\]8;;[^\x1b\x07]*\x1b\\").unwrap();
    let sgr = Regex::new(r"\x1b\[[0-9;]*m").unwrap();
    let open = format!("\x1b]8;;{url}\x1b\\");

    for line in &lines {
        // 去掉两种序列后若还有可见文本，这一行就必须带 OSC 8 re-open
        let stripped = sgr
            .replace_all(&osc8_st.replace_all(line, ""), "")
            .into_owned();
        if !stripped.trim().is_empty() {
            assert!(
                line.starts_with(&open) || line.contains(&open),
                "Line {line:?} has visible text but no OSC 8 re-open"
            );
        }
    }
}

#[test]
fn closes_osc_8_before_each_line_break() {
    let url = "https://example.com";
    let input = format!("\x1b]8;;{url}\x1b\\0123456789\x1b]8;;\x1b\\");
    let lines = wrap_text_with_ansi(&input, 6);

    for line in &lines[..lines.len() - 1] {
        // 每个仍在超链接里的非末行都应以 close 结尾
        if line.contains(&format!("\x1b]8;;{url}\x1b\\")) {
            assert!(
                line.ends_with("\x1b]8;;\x1b\\"),
                "Non-final line {line:?} is inside a hyperlink but does not close it"
            );
        }
    }
}

#[test]
fn preserves_bel_terminators_when_wrapping_oauth_style_hyperlinks() {
    let url = format!("https://example.com/oauth/{}", "a".repeat(32));
    let input = format!("\x1b]8;;{url}\x07{url}\x1b]8;;\x07");
    let lines = wrap_text_with_ansi(&input, 20);

    assert!(lines.len() > 1);
    for line in &lines {
        assert!(
            line.contains(&format!("\x1b]8;;{url}\x07")),
            "Line {line:?} does not reopen the hyperlink with BEL"
        );
        assert!(
            !line.contains(&format!("\x1b]8;;{url}\x1b\\")),
            "Line {line:?} reopens the hyperlink with ST"
        );
    }
    for line in &lines[..lines.len() - 1] {
        assert!(
            line.ends_with("\x1b]8;;\x07"),
            "Line {line:?} does not close the hyperlink with BEL"
        );
    }
}

#[test]
fn does_not_emit_osc_8_sequences_on_lines_that_are_outside_the_hyperlink() {
    let url = "https://example.com";
    let input = format!("before \x1b]8;;{url}\x1b\\link\x1b]8;;\x1b\\ after");
    let lines = wrap_text_with_ansi(&input, 80);

    // 宽度 80 放得下一行：应恰好一个 OSC 8 open、一个 OSC 8 close
    assert_eq!(lines.len(), 1);
    let open = Regex::new(r"\x1b\]8;;https:[^\x1b]+\x1b\\").unwrap();
    let close = Regex::new(r"\x1b\]8;;\x1b\\").unwrap();
    assert_eq!(open.find_iter(&lines[0]).count(), 1);
    assert_eq!(close.find_iter(&lines[0]).count(), 1);
}

// ---------------------------------------------------------------------------
// 回归：`AnsiCodeTracker::process` 的 SGR 参数正则**未锚定**
// ---------------------------------------------------------------------------

/// TS 的 `process()` 用 `ansiCode.match(/\x1b\[([\d;]*)m/)`，没有 `^`。
/// `ansiCodeLength` 有时会把"一个 CSI 开头 + 后面的可见文本 + 另一个 SGR"整段当成一个序列
/// （`\x1b[?25l…\x1b[4m` 里第一个 `m`?? —— `?25l` 以 `l` 结尾，不是终结符，
/// 于是扫到后面 `\x1b[4m` 的 `m` 才结束），此时正则仍应命中其中第一个可用的 SGR。
#[test]
fn tracker_finds_sgr_inside_a_span_that_swallowed_following_text() {
    // 换行后第二行本该带上下划线（TS 从被吞掉的整段里认出 `\x1b[4m`）
    let text = "\x1b[?25l가〮*\u{FEFF}\u{301}\x1b[4m\x00Z";
    let wrapped = wrap_text_with_ansi(text, 0);
    assert_eq!(
        wrapped,
        vec![
            "\x1b[?25l가〮*\u{FEFF}\u{301}\x1b[4m\x00\x1b[24m".to_string(),
            "\x1b[4mZ".to_string(),
        ]
    );
}
