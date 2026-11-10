//! `packages/tui/test/markdown.test.ts`：`Markdown` 组件的 92 个用例。
//!
//! TS 用 `VirtualTerminal`（`@xterm/headless`）做单元格级断言；Rust 侧不引入 VT 模拟器，
//! 改为：
//! * 纯文本断言——[`strip_sgr`] 去掉 SGR 序列后与 TS 的 `stripAnsi` 等价；
//! * 单元格属性断言——[`scan`] 把渲染行按可见列解析成 SGR 状态，等价于 xterm 的
//!   `getCell(...).isFgDefault()/isBold()/isDim()/isItalic()/isUnderline()/getFgColor()`。
//!
//! 主题用与 `test-themes.ts` 相同的 `chalk`（level 3）闭包；[`chalk_apply`] 逐行复刻
//! chalk 的 `applyStyle`（close → close+open、换行包裹、openAll/closeAll）。

use std::sync::{Arc, Mutex};

use kiana_tui::components::markdown::StyleFn;
use kiana_tui::markdown::{Lexer, Token};
use kiana_tui::terminal_image::{TerminalCapabilities, reset_capabilities_cache, set_capabilities};
use kiana_tui::text::{is_js_whitespace, trim_end_js, trim_js};
use kiana_tui::{Component, DefaultTextStyle, Markdown, MarkdownOptions, MarkdownTheme};

// ---------------------------------------------------------------------------
// chalk（level 3）等价物
// ---------------------------------------------------------------------------

/// chalk 的 `stringReplaceAll(string, substring, substring + postfix)`。
fn replace_all_with_postfix(text: &str, substring: &str, postfix: &str) -> String {
    if !text.contains(substring) {
        return text.to_string();
    }
    let mut result = String::with_capacity(text.len() + postfix.len());
    let mut cursor = 0usize;
    while let Some(found) = text[cursor..].find(substring) {
        let index = cursor + found;
        result.push_str(&text[cursor..index]);
        result.push_str(substring);
        result.push_str(postfix);
        cursor = index + substring.len();
    }
    result.push_str(&text[cursor..]);
    result
}

/// chalk 的 `stringEncaseCRLFWithFirstIndex`：每个 `\n` / `\r\n` 包上 `prefix`/`postfix`。
fn encase_newlines(text: &str, prefix: &str, postfix: &str) -> String {
    let bytes = text.as_bytes();
    let mut result = String::with_capacity(text.len());
    let mut start = 0usize;
    let mut search = 0usize;
    while let Some(found) = text[search..].find('\n') {
        let index = search + found;
        let has_cr = index > 0 && bytes[index - 1] == b'\r';
        let end = if has_cr { index - 1 } else { index };
        result.push_str(&text[start..end]);
        result.push_str(prefix);
        result.push_str(if has_cr { "\r\n" } else { "\n" });
        result.push_str(postfix);
        start = index + 1;
        search = start;
    }
    result.push_str(&text[start..]);
    result
}

/// chalk 的 `applyStyle`：`pairs` 从外到内。
fn chalk_apply(text: &str, pairs: &[(&str, &str)]) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut opens = String::new();
    for (open, _) in pairs {
        opens.push_str(open);
    }
    let mut closes = String::new();
    for (_, close) in pairs.iter().rev() {
        closes.push_str(close);
    }
    let mut body = text.to_string();
    if body.contains('\x1b') {
        // chalk：从最内层 styler 开始，把每个 close 替换成 close + open
        for (open, close) in pairs.iter().rev() {
            body = replace_all_with_postfix(&body, close, open);
        }
    }
    let body = encase_newlines(&body, &closes, &opens);
    format!("{opens}{body}{closes}")
}

fn chalk_bold(text: &str) -> String {
    chalk_apply(text, &[("\x1b[1m", "\x1b[22m")])
}

fn chalk_dim(text: &str) -> String {
    chalk_apply(text, &[("\x1b[2m", "\x1b[22m")])
}

fn chalk_italic(text: &str) -> String {
    chalk_apply(text, &[("\x1b[3m", "\x1b[23m")])
}

fn chalk_underline(text: &str) -> String {
    chalk_apply(text, &[("\x1b[4m", "\x1b[24m")])
}

fn chalk_strikethrough(text: &str) -> String {
    chalk_apply(text, &[("\x1b[9m", "\x1b[29m")])
}

fn chalk_green(text: &str) -> String {
    chalk_apply(text, &[("\x1b[32m", "\x1b[39m")])
}

fn chalk_yellow(text: &str) -> String {
    chalk_apply(text, &[("\x1b[33m", "\x1b[39m")])
}

fn chalk_blue(text: &str) -> String {
    chalk_apply(text, &[("\x1b[34m", "\x1b[39m")])
}

fn chalk_magenta(text: &str) -> String {
    chalk_apply(text, &[("\x1b[35m", "\x1b[39m")])
}

fn chalk_cyan(text: &str) -> String {
    chalk_apply(text, &[("\x1b[36m", "\x1b[39m")])
}

fn chalk_gray(text: &str) -> String {
    chalk_apply(text, &[("\x1b[90m", "\x1b[39m")])
}

/// `chalk.bold.cyan`（`defaultMarkdownTheme.heading`）。
fn chalk_heading(text: &str) -> String {
    chalk_apply(text, &[("\x1b[1m", "\x1b[22m"), ("\x1b[36m", "\x1b[39m")])
}

fn styler(function: fn(&str) -> String) -> StyleFn {
    let arc: StyleFn = Arc::new(function);
    arc
}

/// `test-themes.ts` 的 `defaultMarkdownTheme`。
fn default_markdown_theme() -> MarkdownTheme {
    MarkdownTheme {
        heading: styler(chalk_heading),
        link: styler(chalk_blue),
        link_url: styler(chalk_dim),
        code: styler(chalk_yellow),
        code_block: styler(chalk_green),
        code_block_border: styler(chalk_dim),
        quote: styler(chalk_italic),
        quote_border: styler(chalk_dim),
        hr: styler(chalk_dim),
        list_bullet: styler(chalk_cyan),
        bold: styler(chalk_bold),
        italic: styler(chalk_italic),
        strikethrough: styler(chalk_strikethrough),
        underline: styler(chalk_underline),
        highlight_code: None,
        code_block_indent: None,
    }
}

fn md(source: &str, padding_x: usize, padding_y: usize) -> Markdown {
    Markdown::new(source, padding_x, padding_y, default_markdown_theme())
}

// ---------------------------------------------------------------------------
// ANSI 工具（stripAnsi / 单元格扫描）
// ---------------------------------------------------------------------------

/// `stripAnsi`：只移除 SGR 序列（`/\x1b\[[0-9;]*m/g`），其余转义序列（如 OSC 超链接）原样保留。
fn strip_sgr(line: &str) -> String {
    let mut result = String::with_capacity(line.len());
    let mut chars = line.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if ch == '\x1b' && line[index..].starts_with("\x1b[") {
            let rest = &line[index + 2..];
            let params_len = rest
                .find(|c: char| !(c.is_ascii_digit() || c == ';'))
                .unwrap_or(rest.len());
            if rest[params_len..].starts_with('m') {
                // 跳过 ESC [ params m
                for _ in 0..2 + params_len {
                    chars.next();
                }
                continue;
            }
        }
        result.push(ch);
    }
    result
}

/// 去掉 OSC 8 超链接序列（`\x1b]8;;...\x1b\\`）。
fn strip_osc8(line: &str) -> String {
    let mut result = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find("\x1b]8;;") {
        result.push_str(&rest[..start]);
        let after = &rest[start + 4..];
        match after.find("\x1b\\") {
            Some(end) => rest = &after[end + 2..],
            None => {
                rest = "";
                break;
            }
        }
    }
    result.push_str(rest);
    result
}

fn plain(lines: &[String]) -> Vec<String> {
    lines.iter().map(|line| strip_sgr(line)).collect()
}

fn plain_trimmed(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|line| trim_end_js(&strip_sgr(line)).to_string())
        .collect()
}

/// `lines.map(l => stripAnsi(l)).join(sep)`
fn joined_plain(lines: &[String], separator: &str) -> String {
    plain(lines).join(separator)
}

fn expect(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

fn chars_len(text: &str) -> usize {
    text.chars().count()
}

fn any_contains(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|line| line.contains(needle))
}

fn find_line<'a>(lines: &'a [String], needle: &str) -> Option<&'a String> {
    lines.iter().find(|line| line.contains(needle))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FgColor {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy)]
struct CellStyle {
    fg: FgColor,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
}

impl Default for CellStyle {
    fn default() -> Self {
        CellStyle {
            fg: FgColor::Default,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
        }
    }
}

fn parse_sgr(state: &mut CellStyle, params: &str) {
    let numbers: Vec<i64> = if params.is_empty() {
        vec![0]
    } else {
        params
            .split(';')
            .map(|part| part.parse::<i64>().unwrap_or(0))
            .collect()
    };
    let mut index = 0usize;
    while index < numbers.len() {
        match numbers[index] {
            0 => *state = CellStyle::default(),
            1 => state.bold = true,
            2 => state.dim = true,
            3 => state.italic = true,
            4 => state.underline = true,
            22 => {
                state.bold = false;
                state.dim = false;
            }
            23 => state.italic = false,
            24 => state.underline = false,
            38 => {
                if index + 1 < numbers.len() {
                    if numbers[index + 1] == 5 && index + 2 < numbers.len() {
                        state.fg = FgColor::Indexed(numbers[index + 2].clamp(0, 255) as u8);
                        index += 2;
                    } else if numbers[index + 1] == 2 && index + 4 < numbers.len() {
                        state.fg = FgColor::Rgb(
                            numbers[index + 2].clamp(0, 255) as u8,
                            numbers[index + 3].clamp(0, 255) as u8,
                            numbers[index + 4].clamp(0, 255) as u8,
                        );
                        index += 4;
                    }
                }
            }
            39 => state.fg = FgColor::Default,
            30..=37 => state.fg = FgColor::Indexed((numbers[index] - 30) as u8),
            90..=97 => state.fg = FgColor::Indexed((numbers[index] - 90 + 8) as u8),
            _ => {}
        }
        index += 1;
    }
}

/// 把一行渲染文本解析成（可见文本, 每个可见列的 SGR 状态）。
fn scan(line: &str) -> (String, Vec<CellStyle>) {
    let mut text = String::new();
    let mut cells: Vec<CellStyle> = Vec::new();
    let mut state = CellStyle::default();
    let chars: Vec<char> = line.chars().collect();
    let mut index = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\x1b' {
            if index + 1 < chars.len() && chars[index + 1] == '[' {
                let mut end = index + 2;
                while end < chars.len() && !('\u{40}'..='\u{7e}').contains(&chars[end]) {
                    end += 1;
                }
                if end >= chars.len() {
                    break;
                }
                if chars[end] == 'm' {
                    let params: String = chars[index + 2..end].iter().collect();
                    parse_sgr(&mut state, &params);
                }
                index = end + 1;
                continue;
            }
            if index + 1 < chars.len() && chars[index + 1] == ']' {
                let mut end = index + 2;
                while end < chars.len() {
                    if chars[end] == '\u{7}' {
                        end += 1;
                        break;
                    }
                    if chars[end] == '\x1b' && end + 1 < chars.len() && chars[end + 1] == '\\' {
                        end += 2;
                        break;
                    }
                    end += 1;
                }
                index = end;
                continue;
            }
            index += 2;
            continue;
        }
        text.push(ch);
        cells.push(state);
        index += 1;
    }
    (text, cells)
}

/// 可见文本中从 `from`（字符下标）开始查找 `needle` 的字符下标。
fn find_chars(text: &str, needle: &str, from: usize) -> Option<usize> {
    let chars: Vec<char> = text.chars().collect();
    let needle_chars: Vec<char> = needle.chars().collect();
    if needle_chars.is_empty() || from + needle_chars.len() > chars.len() {
        return None;
    }
    let mut index = from;
    while index + needle_chars.len() <= chars.len() {
        if chars[index..index + needle_chars.len()] == needle_chars[..] {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// 扫描完整文本后的 SGR 状态（等价于 xterm 在下一行行首的字符状态）。
fn state_after(text: &str) -> CellStyle {
    let (_, cells) = scan(text);
    cells.last().copied().unwrap_or_default()
}

fn is_numbered_item(line: &str) -> bool {
    let mut digits = 0usize;
    for ch in line.chars() {
        if ch.is_ascii_digit() {
            digits += 1;
            continue;
        }
        return digits > 0 && ch == '.';
    }
    false
}

fn with_capabilities(hyperlinks: bool, true_color: bool) {
    set_capabilities(TerminalCapabilities {
        images: None,
        true_color,
        hyperlinks,
    });
}

// ---------------------------------------------------------------------------
// Transforms
// ---------------------------------------------------------------------------

#[test]
fn caches_transformed_markdown_by_source_and_available_width() {
    let calls: Arc<Mutex<Vec<(String, usize)>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let mut markdown = md("source", 2, 0).with_options(MarkdownOptions {
        transform: Some(Arc::new(move |source: &str, available_width: usize| {
            recorded
                .lock()
                .expect("calls lock")
                .push((source.to_string(), available_width));
            format!("{source} {available_width}")
        })),
        ..Default::default()
    });

    assert_eq!(
        markdown
            .render(80)
            .iter()
            .map(|line| trim_js(&strip_sgr(line)).to_string())
            .collect::<Vec<_>>(),
        expect(&["source 76"])
    );
    markdown.render(80);
    assert_eq!(
        markdown
            .render(60)
            .iter()
            .map(|line| trim_js(&strip_sgr(line)).to_string())
            .collect::<Vec<_>>(),
        expect(&["source 56"])
    );
    assert_eq!(
        *calls.lock().expect("calls lock"),
        vec![
            ("source".to_string(), 76usize),
            ("source".to_string(), 56usize),
        ]
    );

    markdown.set_text("updated");
    assert_eq!(
        markdown
            .render(60)
            .iter()
            .map(|line| trim_js(&strip_sgr(line)).to_string())
            .collect::<Vec<_>>(),
        expect(&["updated 56"])
    );
    assert_eq!(
        calls.lock().expect("calls lock").last().cloned(),
        Some(("updated".to_string(), 56usize))
    );

    markdown.invalidate();
    markdown.render(60);
    assert_eq!(
        calls.lock().expect("calls lock").last().cloned(),
        Some(("updated".to_string(), 56usize))
    );
    assert_eq!(calls.lock().expect("calls lock").len(), 4);
}

// ---------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------

#[test]
fn render_simple_nested_list() {
    let mut markdown = md("- Item 1\n  - Nested 1.1\n  - Nested 1.2\n- Item 2", 0, 0);
    let lines = markdown.render(80);
    let plain_lines = plain(&lines);

    assert!(!lines.is_empty());
    assert!(any_contains(&plain_lines, "- Item 1"));
    assert!(any_contains(&plain_lines, "    - Nested 1.1"));
    assert!(any_contains(&plain_lines, "    - Nested 1.2"));
    assert!(any_contains(&plain_lines, "- Item 2"));
}

#[test]
fn render_deeply_nested_list() {
    let mut markdown = md(
        "- Level 1\n  - Level 2\n    - Level 3\n      - Level 4",
        0,
        0,
    );
    let lines = markdown.render(80);
    let plain_lines = plain(&lines);

    assert!(any_contains(&plain_lines, "- Level 1"));
    assert!(any_contains(&plain_lines, "    - Level 2"));
    assert!(any_contains(&plain_lines, "        - Level 3"));
    assert!(any_contains(&plain_lines, "            - Level 4"));
}

#[test]
fn render_ordered_nested_list() {
    let mut markdown = md(
        "1. First\n   1. Nested first\n   2. Nested second\n2. Second",
        0,
        0,
    );
    let lines = markdown.render(80);
    let plain_lines = plain(&lines);

    assert!(any_contains(&plain_lines, "1. First"));
    assert!(any_contains(&plain_lines, "    1. Nested first"));
    assert!(any_contains(&plain_lines, "    2. Nested second"));
    assert!(any_contains(&plain_lines, "2. Second"));
}

#[test]
fn normalize_ordered_list_markers_by_default() {
    let mut markdown = md("1. alpha\n1. beta\n1. gamma", 0, 0);
    let lines = markdown.render(80);
    assert_eq!(
        plain_trimmed(&lines),
        expect(&["1. alpha", "2. beta", "3. gamma"])
    );
}

#[test]
fn preserve_source_list_markers_when_configured() {
    let mut markdown = md(
        "  4. forth\n  3. third\n\n10) ten\n7) seven\n\n+ plus\n* star\n- minus\n+",
        0,
        0,
    )
    .with_options(MarkdownOptions {
        preserve_ordered_list_markers: true,
        ..Default::default()
    });
    let lines = markdown.render(80);
    assert_eq!(
        plain_trimmed(&lines),
        expect(&[
            "4. forth", "3. third", "", "10) ten", "7) seven", "", "+ plus", "* star", "- minus",
            "+",
        ])
    );
}

#[test]
fn render_mixed_ordered_and_unordered_nested_lists() {
    let mut markdown = md(
        "1. Ordered item\n   - Unordered nested\n   - Another nested\n2. Second ordered\n   - More nested",
        0,
        0,
    );
    let lines = markdown.render(80);
    let plain_lines = plain(&lines);

    assert!(any_contains(&plain_lines, "1. Ordered item"));
    assert!(any_contains(&plain_lines, "    - Unordered nested"));
    assert!(any_contains(&plain_lines, "2. Second ordered"));
}

#[test]
fn render_blank_lines_between_loose_list_items() {
    let mut markdown = md(
        "1. Lorem ipsum dolor sit amet.\n\n   Ut enim ad minim veniam.\n\n2. Duis aute irure dolor.\n\n   Excepteur sint occaecat cupidatat.\n\n3. Beep boop",
        0,
        0,
    );
    let lines = markdown.render(80);
    assert_eq!(
        plain_trimmed(&lines),
        expect(&[
            "1. Lorem ipsum dolor sit amet.",
            "",
            "   Ut enim ad minim veniam.",
            "",
            "2. Duis aute irure dolor.",
            "",
            "   Excepteur sint occaecat cupidatat.",
            "",
            "3. Beep boop",
        ])
    );
}

#[test]
fn render_task_list_markers() {
    let mut markdown = md("- [ ] beep\n- [x] boop", 0, 0);
    let lines = markdown.render(80);
    assert_eq!(plain_trimmed(&lines), expect(&["- [ ] beep", "- [x] boop"]));
}

#[test]
fn maintain_numbering_when_code_blocks_are_not_indented() {
    let mut markdown = md(
        "1. First item\n\n```typescript\n// code block\n```\n\n2. Second item\n\n```typescript\n// another code block\n```\n\n3. Third item",
        0,
        0,
    );
    let lines = markdown.render(80);
    let plain_lines: Vec<String> = lines
        .iter()
        .map(|line| trim_js(&strip_sgr(line)).to_string())
        .collect();

    let numbered: Vec<&String> = plain_lines
        .iter()
        .filter(|line| is_numbered_item(line))
        .collect();
    assert_eq!(
        numbered.len(),
        3,
        "Expected 3 numbered items, got: {numbered:?}"
    );
    assert!(
        numbered[0].starts_with("1."),
        "First item should be \"1.\", got: {}",
        numbered[0]
    );
    assert!(
        numbered[1].starts_with("2."),
        "Second item should be \"2.\", got: {}",
        numbered[1]
    );
    assert!(
        numbered[2].starts_with("3."),
        "Third item should be \"3.\", got: {}",
        numbered[2]
    );
}

#[test]
fn indent_wrapped_unordered_list_lines() {
    let mut markdown = md("- alpha beta gamma delta epsilon", 0, 0);
    let lines = markdown.render(20);
    assert_eq!(
        plain_trimmed(&lines),
        expect(&["- alpha beta gamma", "  delta epsilon"])
    );
}

#[test]
fn indent_wrapped_ordered_list_lines() {
    let mut markdown = md("1. alpha beta gamma delta epsilon", 0, 0);
    let lines = markdown.render(20);
    assert_eq!(
        plain_trimmed(&lines),
        expect(&["1. alpha beta gamma", "   delta epsilon"])
    );
}

#[test]
fn indent_wrapped_ordered_list_lines_with_multi_digit_markers() {
    let mut markdown = md("10. alpha beta gamma delta epsilon", 0, 0);
    let lines = markdown.render(21);
    assert_eq!(
        plain_trimmed(&lines),
        expect(&["10. alpha beta gamma", "    delta epsilon"])
    );
}

#[test]
fn indent_wrapped_nested_list_lines() {
    let mut markdown = md("- parent\n  - alpha beta gamma delta epsilon", 0, 0);
    let lines = markdown.render(24);
    assert_eq!(
        plain_trimmed(&lines),
        expect(&["- parent", "    - alpha beta gamma", "      delta epsilon"])
    );
}

#[test]
fn indent_wrapped_nested_list_lines_under_ordered_parents() {
    let mut markdown = md("1. parent\n   - alpha beta gamma delta epsilon", 0, 0);
    let lines = markdown.render(24);
    assert_eq!(
        plain_trimmed(&lines),
        expect(&["1. parent", "    - alpha beta gamma", "      delta epsilon"])
    );
}

#[test]
fn render_and_wrap_blockquotes_inside_list_items() {
    let mut markdown = md("- > alpha beta gamma delta epsilon zeta", 0, 0);
    let lines = markdown.render(24);
    assert_eq!(
        plain_trimmed(&lines),
        expect(&["- │ alpha beta gamma", "  │ delta epsilon zeta"])
    );
}

#[test]
fn render_and_wrap_code_blocks_inside_list_items() {
    let mut markdown = md(
        "- ```ts\n  alpha beta gamma delta epsilon zeta\n  ```",
        0,
        0,
    );
    let lines = markdown.render(24);
    assert_eq!(
        plain_trimmed(&lines),
        expect(&[
            "- ```ts",
            "    alpha beta gamma",
            "  delta epsilon zeta",
            "  ```",
        ])
    );
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

#[test]
fn render_simple_table() {
    let mut markdown = md(
        "| Name | Age |\n| --- | --- |\n| Alice | 30 |\n| Bob | 25 |",
        0,
        0,
    );
    let lines = markdown.render(80);
    let plain_lines = plain(&lines);

    assert!(any_contains(&plain_lines, "Name"));
    assert!(any_contains(&plain_lines, "Age"));
    assert!(any_contains(&plain_lines, "Alice"));
    assert!(any_contains(&plain_lines, "Bob"));
    assert!(any_contains(&plain_lines, "│"));
    assert!(any_contains(&plain_lines, "─"));
}

#[test]
fn render_row_dividers_between_data_rows() {
    let mut markdown = md(
        "| Name | Age |\n| --- | --- |\n| Alice | 30 |\n| Bob | 25 |",
        0,
        0,
    );
    let lines = markdown.render(80);
    let plain_lines = plain(&lines);
    let divider_lines: Vec<&String> = plain_lines
        .iter()
        .filter(|line| line.contains('┼'))
        .collect();

    assert_eq!(divider_lines.len(), 2, "Expected header + row divider");
}

#[test]
fn keep_column_width_at_least_the_longest_word() {
    let longest_word = "superlongword";
    let source = format!(
        "| Column One | Column Two |\n| --- | --- |\n| {longest_word} short | otherword |\n| small | tiny |"
    );
    let mut markdown = md(&source, 0, 0);
    let lines = markdown.render(32);
    let plain_lines = plain(&lines);
    let data_line =
        find_line(&plain_lines, longest_word).expect("Expected data row containing longest word");

    let segments: Vec<&str> = data_line.split('│').collect();
    let first_segment = segments[1];
    let first_column_width = chars_len(first_segment) - 2;

    assert!(
        first_column_width >= longest_word.len(),
        "Expected first column width >= {}, got {first_column_width}",
        longest_word.len()
    );
}

#[test]
fn render_table_with_alignment() {
    let mut markdown = md(
        "| Left | Center | Right |\n| :--- | :---: | ---: |\n| A | B | C |\n| Long text | Middle | End |",
        0,
        0,
    );
    let lines = markdown.render(80);
    let plain_lines = plain(&lines);

    assert!(any_contains(&plain_lines, "Left"));
    assert!(any_contains(&plain_lines, "Center"));
    assert!(any_contains(&plain_lines, "Right"));
    assert!(any_contains(&plain_lines, "Long text"));
}

#[test]
fn handle_tables_with_varying_column_widths() {
    let mut markdown = md(
        "| Short | Very long column header |\n| --- | --- |\n| A | This is a much longer cell content |\n| B | Short |",
        0,
        0,
    );
    let lines = markdown.render(80);

    assert!(!lines.is_empty());
    let plain_lines = plain(&lines);
    assert!(any_contains(&plain_lines, "Very long column header"));
    assert!(any_contains(
        &plain_lines,
        "This is a much longer cell content"
    ));
}

#[test]
fn wrap_table_cells_when_table_exceeds_available_width() {
    let mut markdown = md(
        "| Command | Description | Example |\n| --- | --- | --- |\n| npm install | Install all dependencies | npm install |\n| npm run build | Build the project | npm run build |",
        0,
        0,
    );
    let lines = markdown.render(50);
    let plain_lines = plain_trimmed(&lines);

    for line in &plain_lines {
        assert!(
            chars_len(line) <= 50,
            "Line exceeds width 50: \"{line}\" (length: {})",
            chars_len(line)
        );
    }

    let all_text = plain_lines.join(" ");
    assert!(all_text.contains("Command"), "Should contain 'Command'");
    assert!(
        all_text.contains("Description"),
        "Should contain 'Description'"
    );
    assert!(
        all_text.contains("npm install"),
        "Should contain 'npm install'"
    );
    assert!(all_text.contains("Install"), "Should contain 'Install'");
}

#[test]
fn not_leak_wrapped_link_styles_into_table_borders_or_plain_cells() {
    let source = "| Link | Plain |\n| --- | --- |\n| [**one two three four five six**](https://example.com) | normal text |";

    for hyperlinks in [true, false] {
        with_capabilities(hyperlinks, false);
        let mut markdown = md(source, 0, 0);
        let lines = markdown.render(24);
        let viewport = plain_trimmed(&lines);
        let row = viewport
            .iter()
            .position(|line| line.contains("one") && line.contains("norm"))
            .unwrap_or_else(|| panic!("Missing wrapped table row: {viewport:?}"));
        let (text, cells) = scan(&lines[row]);
        let link_col = find_chars(&text, "one", 0).expect("link text");
        let separator_col = find_chars(&text, "│", link_col + 1).expect("separator");
        let plain_col = find_chars(&text, "norm", 0).expect("plain text");
        assert!(link_col < separator_col && separator_col < plain_col);
        assert_ne!(cells[link_col].fg, FgColor::Default);
        assert_eq!(cells[separator_col].fg, FgColor::Default);
        assert_eq!(cells[plain_col].fg, FgColor::Default);
        assert!(cells[link_col].bold);
        assert!(!cells[separator_col].bold);
        assert!(!cells[plain_col].bold);

        if !hyperlinks {
            let url_row = viewport
                .iter()
                .position(|line| line.contains("https"))
                .unwrap_or_else(|| panic!("Missing fallback URL row: {viewport:?}"));
            let (url_text, url_cells) = scan(&lines[url_row]);
            let url_col = find_chars(&url_text, "https", 0).expect("url");
            let url_separator_col = find_chars(&url_text, "│", url_col + 1).expect("url separator");
            let url_chars: Vec<char> = url_text.chars().collect();
            let url_border_col = url_chars
                .iter()
                .rposition(|ch| *ch == '│')
                .expect("url border");
            assert!(url_col < url_separator_col && url_separator_col < url_border_col);
            assert!(url_cells[url_col].dim);
            assert!(!url_cells[url_separator_col].dim);
            assert!(!url_cells[url_border_col].dim);
        }
        reset_capabilities_cache();
    }
    reset_capabilities_cache();
}

#[test]
fn restore_enclosing_style_after_wrapped_table_link() {
    let mut theme = default_markdown_theme();
    theme.quote = Arc::new(|text: &str| format!("\x1b[38;2;18;52;86m{text}\x1b[39m"));
    theme.link = Arc::new(|text: &str| format!("\x1b[38;2;129;162;190m{text}\x1b[39m"));
    let source = "> | Link | Plain |\n> | --- | --- |\n> | [one two three four five six](https://example.com) | normal text |";

    with_capabilities(true, true);
    let mut markdown = Markdown::new(source, 0, 0, theme);
    let lines = markdown.render(28);
    let viewport = plain_trimmed(&lines);
    let row = viewport
        .iter()
        .position(|line| line.contains("one") && line.contains("normal"))
        .unwrap_or_else(|| panic!("Missing wrapped blockquote table row: {viewport:?}"));
    let (text, cells) = scan(&lines[row]);
    let link_col = find_chars(&text, "one", 0).expect("link text");
    let separator_col = find_chars(&text, "│", link_col + 1).expect("separator");
    let plain_col = find_chars(&text, "normal", 0).expect("plain text");
    assert!(link_col < separator_col && separator_col < plain_col);

    let quote_color = FgColor::Rgb(18, 52, 86);
    assert_ne!(cells[link_col].fg, quote_color);
    assert_eq!(cells[separator_col].fg, quote_color);
    assert_eq!(cells[plain_col].fg, quote_color);

    let final_row = viewport
        .iter()
        .position(|line| line.contains("five six"))
        .unwrap_or_else(|| panic!("Missing final wrapped link row: {viewport:?}"));
    let (final_text, final_cells) = scan(&lines[final_row]);
    let final_link_col = find_chars(&final_text, "five six", 0).expect("final link text");
    let final_separator_col =
        find_chars(&final_text, "│", final_link_col + 1).expect("final separator");
    let final_chars: Vec<char> = final_text.chars().collect();
    let final_border_col = final_chars
        .iter()
        .rposition(|ch| *ch == '│')
        .expect("final border");
    assert!(final_link_col < final_separator_col && final_separator_col < final_border_col);
    assert_eq!(final_cells[final_separator_col].fg, quote_color);
    assert_eq!(final_cells[final_border_col].fg, quote_color);

    reset_capabilities_cache();
}

#[test]
fn wrap_long_cell_content_to_multiple_lines() {
    let mut markdown = md(
        "| Header |\n| --- |\n| This is a very long cell content that should wrap |",
        0,
        0,
    );
    let lines = markdown.render(25);
    let plain_lines = plain_trimmed(&lines);

    let data_rows: Vec<&String> = plain_lines
        .iter()
        .filter(|line| line.starts_with('│') && !line.contains('─'))
        .collect();
    assert!(
        data_rows.len() > 2,
        "Expected wrapped rows, got {} rows",
        data_rows.len()
    );

    let all_text = plain_lines.join(" ");
    assert!(
        all_text.contains("very long"),
        "Should preserve 'very long'"
    );
    assert!(
        all_text.contains("cell content"),
        "Should preserve 'cell content'"
    );
    assert!(
        all_text.contains("should wrap"),
        "Should preserve 'should wrap'"
    );
}

#[test]
fn wrap_long_unbroken_tokens_inside_table_cells() {
    with_capabilities(false, false);
    let url = "https://example.com/this/is/a/very/long/url/that/should/wrap";
    let source = format!("| Value |\n| --- |\n| prefix {url} |");
    let mut markdown = md(&source, 0, 0);
    let width = 30;
    let lines = markdown.render(width);
    reset_capabilities_cache();
    let plain_lines = plain_trimmed(&lines);

    for line in &plain_lines {
        assert!(
            chars_len(line) <= width,
            "Line exceeds width {width}: \"{line}\" (length: {})",
            chars_len(line)
        );
    }

    let table_lines: Vec<&String> = plain_lines
        .iter()
        .filter(|line| line.starts_with('│'))
        .collect();
    assert!(!table_lines.is_empty(), "Expected table rows to render");
    for line in &table_lines {
        let border_count = line.matches('│').count();
        assert_eq!(
            border_count, 2,
            "Expected 2 borders, got {border_count}: \"{line}\""
        );
    }

    let extracted: String = plain_lines
        .join("")
        .chars()
        .filter(|ch| !matches!(ch, '│' | '├' | '┤' | '─') && !is_js_whitespace(*ch))
        .collect();
    assert!(extracted.contains("prefix"), "Should preserve 'prefix'");
    assert!(extracted.contains(url), "Should preserve URL");
}

#[test]
fn wrap_styled_inline_code_inside_table_cells_without_breaking_borders() {
    let mut markdown = md(
        "| Code |\n| --- |\n| `averyveryveryverylongidentifier` |",
        0,
        0,
    );
    let width = 20;
    let lines = markdown.render(width);
    let joined_output = lines.join("\n");
    assert!(
        joined_output.contains("\x1b[33m"),
        "Inline code should be styled (yellow)"
    );

    let plain_lines = plain_trimmed(&lines);
    for line in &plain_lines {
        assert!(
            chars_len(line) <= width,
            "Line exceeds width {width}: \"{line}\" (length: {})",
            chars_len(line)
        );
    }

    let table_lines: Vec<&String> = plain_lines
        .iter()
        .filter(|line| line.starts_with('│'))
        .collect();
    for line in &table_lines {
        let border_count = line.matches('│').count();
        assert_eq!(
            border_count, 2,
            "Expected 2 borders, got {border_count}: \"{line}\""
        );
    }
}

#[test]
fn handle_extremely_narrow_width_gracefully() {
    let mut markdown = md("| A | B | C |\n| --- | --- | --- |\n| 1 | 2 | 3 |", 0, 0);
    let lines = markdown.render(15);
    let plain_lines = plain_trimmed(&lines);

    assert!(!lines.is_empty(), "Should produce output");
    for line in &plain_lines {
        assert!(
            chars_len(line) <= 15,
            "Line exceeds width 15: \"{line}\" (length: {})",
            chars_len(line)
        );
    }
}

#[test]
fn render_table_correctly_when_it_fits_naturally() {
    let mut markdown = md("| A | B |\n| --- | --- |\n| 1 | 2 |", 0, 0);
    let lines = markdown.render(80);
    let plain_lines = plain_trimmed(&lines);

    let header_line = plain_lines
        .iter()
        .find(|line| line.contains('A') && line.contains('B'))
        .expect("Should have header row");
    assert!(header_line.contains('│'), "Header should have borders");

    let separator_line = plain_lines
        .iter()
        .find(|line| line.contains('├') && line.contains('┼'))
        .expect("Should have separator row");
    assert!(separator_line.contains('─'));

    let data_line = plain_lines
        .iter()
        .find(|line| line.contains('1') && line.contains('2'))
        .expect("Should have data row");
    assert!(!data_line.is_empty());
}

#[test]
fn respect_padding_x_when_calculating_table_width() {
    let mut markdown = md(
        "| Column One | Column Two |\n| --- | --- |\n| Data 1 | Data 2 |",
        2,
        0,
    );
    let lines = markdown.render(40);
    let plain_lines = plain_trimmed(&lines);

    for line in &plain_lines {
        assert!(
            chars_len(line) <= 40,
            "Line exceeds width 40: \"{line}\" (length: {})",
            chars_len(line)
        );
    }

    let table_row = plain_lines
        .iter()
        .find(|line| line.contains('│'))
        .expect("Should have table row");
    assert!(
        table_row.starts_with("  "),
        "Table should have left padding"
    );
}

#[test]
fn not_add_trailing_blank_line_when_table_is_last_block() {
    let mut markdown = md("| Name |\n| --- |\n| Alice |", 0, 0);
    let lines = markdown.render(80);
    let plain_lines = plain_trimmed(&lines);

    assert_ne!(
        plain_lines.last().map(String::as_str),
        Some(""),
        "Expected table to end without a blank line: {plain_lines:?}"
    );
}

// ---------------------------------------------------------------------------
// Combined features
// ---------------------------------------------------------------------------

#[test]
fn render_lists_and_tables_together() {
    let mut markdown = md(
        "# Test Document\n\n- Item 1\n  - Nested item\n- Item 2\n\n| Col1 | Col2 |\n| --- | --- |\n| A | B |",
        0,
        0,
    );
    let lines = markdown.render(80);
    let plain_lines = plain(&lines);

    assert!(any_contains(&plain_lines, "Test Document"));
    assert!(any_contains(&plain_lines, "- Item 1"));
    assert!(any_contains(&plain_lines, "    - Nested item"));
    assert!(any_contains(&plain_lines, "Col1"));
    assert!(any_contains(&plain_lines, "│"));
}

// ---------------------------------------------------------------------------
// LaTeX math
// ---------------------------------------------------------------------------

#[test]
fn renders_inline_dollar_and_parenthesis_delimiters() {
    let mut markdown = md(
        r"A map $\mathbb{C}^3 \to \mathbb{C}^3$, $xy$, $x-y$, $-x$, $\frac{1}{2}$, $\rightarrow$, and \(s \to \infty\).",
        0,
        0,
    );
    let lines = plain_trimmed(&markdown.render(80));

    assert_eq!(
        lines,
        expect(&["A map ℂ³ → ℂ³, xy, x-y, -x, 1/2, →, and s → ∞."])
    );
}

#[test]
fn renders_display_dollar_delimiters_without_markdown_escape_corruption() {
    let mut markdown = md(
        "Before\n\n$$\\{3x+2y,\\; x \\in \\{0, \\pm 1\\}\\}$$\n\nafter",
        0,
        0,
    );
    let lines = plain_trimmed(&markdown.render(80));

    assert_eq!(
        lines,
        expect(&["Before", "", "{3x+2y, x ∈ {0, ± 1}}", "", "after"])
    );
}

#[test]
fn renders_display_bracket_delimiters() {
    let mut markdown = md(
        "Before\n\n\\[\nE \\approx \\frac{0.1\\ \\text{lux}}{100\\ \\text{lm/W}}\n\\]\n\nafter",
        0,
        0,
    );
    let lines = plain_trimmed(&markdown.render(80));

    assert_eq!(
        lines,
        expect(&[
            "Before",
            "",
            "    0.1 lux",
            "E ≈ ────────",
            "    100 lm/W",
            "",
            "after",
        ])
    );
}

#[test]
fn aligns_matrix_rows_with_the_opening_delimiter() {
    let mut markdown = md(
        "Consider the matrix\n\n\\[\nA=\n\\begin{pmatrix}\n\\pi & 0\\\\\n0 & \\frac{1}{\\pi}\n\\end{pmatrix}.\n\\]",
        0,
        0,
    );
    let lines = plain_trimmed(&markdown.render(80));

    assert_eq!(
        lines,
        expect(&[
            "Consider the matrix",
            "",
            "A = ⎛ π │ 0   ⎞",
            "    ⎝ 0 │ 1/π ⎠.",
        ])
    );
}

#[test]
fn renders_lower_limits_beneath_display_operators() {
    let mut markdown = md(
        "\\[\n\\lim_{x\\to 0}\\frac{\\frac{\\sin x}{x}-1}{\\frac{e^x-1}{x}-1}=0\n\\]",
        0,
        0,
    );
    let lines = plain_trimmed(&markdown.render(80));

    assert_eq!(
        lines,
        expect(&[
            "     (sin x)/x-1",
            "lim  ─────────── = 0",
            "x→0  (eˣ-1)/x-1",
        ])
    );
}

#[test]
fn renders_math_inside_lists_and_tables() {
    let mut markdown = md(
        "- Formula: $F_1 = u^2$\n\n| Value |\n| --- |\n| $\\mathbb{C}^3$ |",
        0,
        0,
    );
    let lines = plain_trimmed(&markdown.render(80));
    let output = lines.join("\n");

    assert!(output.contains("- Formula: F₁ = u²"));
    assert!(output.contains("│ ℂ³"));
}

#[test]
fn does_not_treat_currency_shell_variables_or_code_spans_as_math() {
    let source = "Costs $5 and $10 or $8k–$12k; use `$x$`, $HOME, and ${PATH}.";
    let mut markdown = md(source, 0, 0);
    let lines = plain_trimmed(&markdown.render(80));

    assert_eq!(
        lines,
        expect(&["Costs $5 and $10 or $8k–$12k; use $x$, $HOME, and ${PATH}."])
    );

    let shell_variables = "Paths: $HOME/$USER and $XDG_CONFIG_HOME/$APP_CONFIG";
    let mut shell_markdown = md(shell_variables, 0, 0);
    let shell_lines = plain_trimmed(&shell_markdown.render(80));
    assert_eq!(shell_lines, expect(&[shell_variables]));
}

#[test]
fn preserves_unsupported_and_incomplete_latex_exactly() {
    let cases = [
        r"Unknown $x + \unknown{y}$ after",
        r"Streaming $\mathbb{C}^3",
    ];

    for source in cases {
        let mut markdown = md(source, 0, 0);
        let lines = plain_trimmed(&markdown.render(80));
        assert_eq!(lines, expect(&[source]));
    }
}

#[test]
fn preserves_incomplete_backslash_delimiters_while_streaming() {
    let mut inline = md(r"Map \(\mathbb{C}^3", 0, 0);
    assert_eq!(
        plain_trimmed(&inline.render(80)),
        expect(&[r"Map \(\mathbb{C}^3"])
    );

    let mut display = md("\\[\nx^2", 0, 0);
    assert_eq!(plain_trimmed(&display.render(80)), expect(&["\\[", "x^2"]));
}

#[test]
fn does_not_render_latex_inside_escaped_delimiters_or_code_fences() {
    let source = [r"Escaped \$x-y\$.", "", "```text", r"$\mathbb{C}^3$", "```"].join("\n");
    let mut markdown = md(&source, 0, 0);
    let lines = plain_trimmed(&markdown.render(80));

    assert_eq!(
        lines,
        expect(&["Escaped $x-y$.", "", "```text", r"  $\mathbb{C}^3$", "```",])
    );
}

#[test]
fn allows_latex_rendering_to_be_disabled() {
    let source =
        "$$\n\\widetilde Y_{sf}\n=\n(1-w_{sf})\\mu_{sf}^{\\mathrm{MAR}}\n$$\n\nInline \\(A_{sf}\\)";
    let mut markdown = md(source, 0, 0).with_options(MarkdownOptions {
        render_latex: false,
        ..Default::default()
    });

    assert_eq!(
        plain_trimmed(&markdown.render(80)),
        expect(&[
            "$$",
            r"\widetilde Y_{sf}",
            "=",
            r"(1-w_{sf})\mu_{sf}^{\mathrm{MAR}}",
            "$$",
            "",
            r"Inline \(A_{sf}\)",
        ])
    );
}

// --- Regression tests for #11159 --------------------------------------------

const TABLE_RENDERED: [&str; 5] = [
    "┌───┬───┐",
    "│ A │ B │",
    "├───┼───┤",
    "│ 1 │ 2 │",
    "└───┴───┘",
];

fn render_table_case(source: &str) -> Vec<String> {
    let mut markdown = md(source, 0, 0);
    plain_trimmed(&markdown.render(80))
}

fn table_source(parts: &[&str]) -> String {
    parts.join("\n")
}

#[test]
fn renders_table_after_orphan_dollar_opener() {
    let source = table_source(&[
        "Intro",
        "",
        "$$",
        "",
        "| A | B |",
        "|---|---|",
        "| 1 | 2 |",
        "",
        "After",
    ]);
    let mut expected = expect(&["Intro", "", "$$", ""]);
    expected.extend(expect(&TABLE_RENDERED));
    expected.extend(expect(&["", "After"]));
    assert_eq!(render_table_case(&source), expected);
}

#[test]
fn renders_table_after_orphan_dollar_opener_with_math_like_content() {
    let source = table_source(&["$$", "x = 1", "", "| A | B |", "|---|---|", "| 1 | 2 |"]);
    let mut expected = expect(&["$$", "x = 1", ""]);
    expected.extend(expect(&TABLE_RENDERED));
    assert_eq!(render_table_case(&source), expected);
}

#[test]
fn does_not_pair_unclosed_dollar_with_later_dollar_across_table() {
    let source = table_source(&[
        "Intro",
        "",
        "$$",
        "x = 1",
        "",
        "| A | B |",
        "|---|---|",
        "| 1 | 2 |",
        "",
        "$$",
        "y^2",
        "$$",
    ]);
    let mut expected = expect(&["Intro", "", "$$", "x = 1", ""]);
    expected.extend(expect(&TABLE_RENDERED));
    expected.extend(expect(&["", "y²"]));
    assert_eq!(render_table_case(&source), expected);
}

#[test]
fn renders_single_column_table_with_pipe_free_aligned_delimiter_after_orphan_dollar() {
    let source = table_source(&["$$", "", "| A |", ":---:", "| 1 |"]);
    assert_eq!(
        render_table_case(&source),
        expect(&["$$", "", "┌───┐", "│ A │", "├───┤", "│ 1 │", "└───┘"])
    );
}

#[test]
fn renders_table_after_orphan_bracket_opener() {
    let source = table_source(&["\\\\[", "x^2", "", "| A | B |", "|---|---|", "| 1 | 2 |"]);
    let mut expected = expect(&["", "\\[", "x^2", ""]);
    expected.extend(expect(&TABLE_RENDERED));
    assert_eq!(render_table_case(&source), expected);
}

#[test]
fn does_not_pair_unclosed_bracket_opener_with_later_opener_across_table() {
    let source = table_source(&[
        "\\\\[",
        "x = 1",
        "",
        "| A | B |",
        "|---|---|",
        "| 1 | 2 |",
        "",
        "\\\\[",
        "y^2",
        "\\\\]",
    ]);
    let mut expected = expect(&["", "\\[", "x = 1", ""]);
    expected.extend(expect(&TABLE_RENDERED));
    expected.extend(expect(&["", "\\", "", "\\[", "y^2", "\\\\]"]));
    assert_eq!(render_table_case(&source), expected);
}

#[test]
fn renders_table_with_pipe_free_header_after_orphan_dollar() {
    let source = table_source(&["$$", "x^2", "", "Value", "| --- |", "1"]);
    assert_eq!(
        render_table_case(&source),
        expect(&[
            "$$",
            "x^2",
            "",
            "┌───────┐",
            "│ Value │",
            "├───────┤",
            "│ 1     │",
            "└───────┘",
        ])
    );
}

#[test]
fn still_renders_closed_multiline_display_math_followed_by_table() {
    let source = table_source(&[
        "$$",
        "a",
        "+ b",
        "$$",
        "",
        "| A | B |",
        "|---|---|",
        "| 1 | 2 |",
    ]);
    let mut expected = expect(&["a + b", ""]);
    expected.extend(expect(&TABLE_RENDERED));
    assert_eq!(render_table_case(&source), expected);
}

#[test]
fn switches_from_raw_to_rendered_math_when_streamed_delimiter_closes() {
    let mut markdown = md(r"Map $\mathbb{C}^3", 0, 0);
    assert_eq!(
        plain_trimmed(&markdown.render(80)),
        expect(&[r"Map $\mathbb{C}^3"])
    );

    markdown.set_text(r"Map $\mathbb{C}^3$");

    assert_eq!(plain_trimmed(&markdown.render(80)), expect(&["Map ℂ³"]));
}

// ---------------------------------------------------------------------------
// Backslash escapes
// ---------------------------------------------------------------------------

#[test]
fn normalize_escaped_punctuation_by_default() {
    let mut markdown = md(r#""\""#, 0, 0);
    let lines = plain_trimmed(&markdown.render(80));

    assert_eq!(lines, expect(&["\"\""]));
}

#[test]
fn preserve_source_backslash_escapes_when_configured() {
    let mut markdown = md(r#""\""#, 0, 0).with_options(MarkdownOptions {
        preserve_backslash_escapes: true,
        ..Default::default()
    });
    let lines = plain_trimmed(&markdown.render(80));

    assert_eq!(lines, expect(&[r#""\""#]));
}

// ---------------------------------------------------------------------------
// Pre-styled text (thinking traces)
// ---------------------------------------------------------------------------

fn gray_italic_style() -> DefaultTextStyle {
    DefaultTextStyle {
        color: Some(styler(chalk_gray)),
        italic: true,
        ..Default::default()
    }
}

#[test]
fn preserve_gray_italic_styling_after_inline_code() {
    let mut markdown = md(
        "This is thinking with `inline code` and more text after",
        1,
        0,
    )
    .with_default_text_style(gray_italic_style());

    let lines = markdown.render(80);
    let joined_output = lines.join("\n");

    assert!(joined_output.contains("inline code"));
    assert!(
        joined_output.contains("\x1b[90m"),
        "Should have gray color code"
    );
    assert!(joined_output.contains("\x1b[3m"), "Should have italic code");
    assert!(
        joined_output.contains("\x1b[33m"),
        "Should style inline code"
    );
}

#[test]
fn preserve_gray_italic_styling_after_bold_text() {
    let mut markdown = md("This is thinking with **bold text** and more after", 1, 0)
        .with_default_text_style(gray_italic_style());

    let lines = markdown.render(80);
    let joined_output = lines.join("\n");

    assert!(joined_output.contains("bold text"));
    assert!(
        joined_output.contains("\x1b[90m"),
        "Should have gray color code"
    );
    assert!(joined_output.contains("\x1b[3m"), "Should have italic code");
    assert!(joined_output.contains("\x1b[1m"), "Should have bold code");
}

#[test]
fn not_leak_styles_into_following_lines_when_rendered_in_tui() {
    let mut markdown = md("This is thinking with `inline code`", 1, 0)
        .with_default_text_style(gray_italic_style());

    let lines = markdown.render(80);
    assert!(!lines.is_empty());

    // TUI 把组件行（已按宽度补空格）逐行写出，再写输入行；SGR 状态跨行延续。
    let mut stream = String::new();
    for line in &lines {
        stream.push_str(line);
        stream.push_str("\r\n");
    }
    let state = state_after(&stream);
    assert!(!state.italic, "Input row should not inherit italic");
}

// ---------------------------------------------------------------------------
// Spacing after code blocks
// ---------------------------------------------------------------------------

#[test]
fn only_one_blank_line_between_code_block_and_following_paragraph() {
    let mut markdown = md(
        "hello world\n\n```js\nconst hello = \"world\";\n```\n\nagain, hello world",
        0,
        0,
    );
    let lines = plain_trimmed(&markdown.render(80));

    let closing_index = lines
        .iter()
        .position(|line| line == "```")
        .expect("Should have closing backticks");
    let after_backticks = &lines[closing_index + 1..];
    let empty_line_count = after_backticks
        .iter()
        .position(|line| !line.is_empty())
        .expect("Should have following text");

    assert_eq!(
        empty_line_count,
        1,
        "Expected 1 empty line after code block, but found {empty_line_count}. Lines after backticks: {:?}",
        &after_backticks[..after_backticks.len().min(5)]
    );
}

#[test]
fn normalize_paragraph_and_code_block_spacing_to_one_blank_line() {
    let cases = [
        "hello this is text\n```\ncode block\n```\nmore text",
        "hello this is text\n\n```\ncode block\n```\n\nmore text",
    ];
    let expected_lines = expect(&[
        "hello this is text",
        "",
        "```",
        "  code block",
        "```",
        "",
        "more text",
    ]);

    for text in cases {
        let mut markdown = md(text, 0, 0);
        let lines = plain_trimmed(&markdown.render(80));
        assert_eq!(
            lines, expected_lines,
            "Unexpected spacing for markdown: {text:?}"
        );
    }
}

#[test]
fn not_add_trailing_blank_line_when_code_block_is_last_block() {
    let cases = [
        "```js\nconst hello = 'world';\n```",
        "hello world\n\n```js\nconst hello = 'world';\n```",
    ];

    for text in cases {
        let mut markdown = md(text, 0, 0);
        let lines = plain_trimmed(&markdown.render(80));
        assert_ne!(
            lines.last().map(String::as_str),
            Some(""),
            "Expected code block to end without a blank line: {lines:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Spacing after dividers
// ---------------------------------------------------------------------------

#[test]
fn only_one_blank_line_between_divider_and_following_paragraph() {
    let mut markdown = md("hello world\n\n---\n\nagain, hello world", 0, 0);
    let lines = plain_trimmed(&markdown.render(80));

    let divider_index = lines
        .iter()
        .position(|line| line.contains('─'))
        .expect("Should have divider");
    let after_divider = &lines[divider_index + 1..];
    let empty_line_count = after_divider
        .iter()
        .position(|line| !line.is_empty())
        .expect("Should have following text");

    assert_eq!(
        empty_line_count,
        1,
        "Expected 1 empty line after divider, but found {empty_line_count}. Lines after divider: {:?}",
        &after_divider[..after_divider.len().min(5)]
    );
}

#[test]
fn not_add_trailing_blank_line_when_divider_is_last_block() {
    let mut markdown = md("---", 0, 0);
    let lines = plain_trimmed(&markdown.render(80));

    assert_ne!(
        lines.last().map(String::as_str),
        Some(""),
        "Expected divider to end without a blank line: {lines:?}"
    );
}

// ---------------------------------------------------------------------------
// Spacing after headings
// ---------------------------------------------------------------------------

#[test]
fn only_one_blank_line_between_heading_and_following_paragraph() {
    let mut markdown = md("# Hello\n\nThis is a paragraph", 0, 0);
    let lines = plain_trimmed(&markdown.render(80));

    let heading_index = lines
        .iter()
        .position(|line| line.contains("Hello"))
        .expect("Should have heading");
    let after_heading = &lines[heading_index + 1..];
    let empty_line_count = after_heading
        .iter()
        .position(|line| !line.is_empty())
        .expect("Should have following text");

    assert_eq!(
        empty_line_count,
        1,
        "Expected 1 empty line after heading, but found {empty_line_count}. Lines after heading: {:?}",
        &after_heading[..after_heading.len().min(5)]
    );
}

#[test]
fn not_add_trailing_blank_line_when_heading_is_last_block() {
    let mut markdown = md("# Hello", 0, 0);
    let lines = plain_trimmed(&markdown.render(80));

    assert_ne!(
        lines.last().map(String::as_str),
        Some(""),
        "Expected heading to end without a blank line: {lines:?}"
    );
}

// ---------------------------------------------------------------------------
// Spacing after blockquotes
// ---------------------------------------------------------------------------

#[test]
fn only_one_blank_line_between_blockquote_and_following_paragraph() {
    let mut markdown = md(
        "hello world\n\n> This is a quote\n\nagain, hello world",
        0,
        0,
    );
    let lines = plain_trimmed(&markdown.render(80));

    let quote_index = lines
        .iter()
        .position(|line| line.contains("This is a quote"))
        .expect("Should have blockquote");
    let after_quote = &lines[quote_index + 1..];
    let empty_line_count = after_quote
        .iter()
        .position(|line| !line.is_empty())
        .expect("Should have following text");

    assert_eq!(
        empty_line_count,
        1,
        "Expected 1 empty line after blockquote, but found {empty_line_count}. Lines after quote: {:?}",
        &after_quote[..after_quote.len().min(5)]
    );
}

#[test]
fn not_add_trailing_blank_line_when_blockquote_is_last_block() {
    let mut markdown = md("> This is a quote", 0, 0);
    let lines = plain_trimmed(&markdown.render(80));

    assert_ne!(
        lines.last().map(String::as_str),
        Some(""),
        "Expected blockquote to end without a blank line: {lines:?}"
    );
}

// ---------------------------------------------------------------------------
// Deeply nested blockquotes (#11272)
// ---------------------------------------------------------------------------

#[test]
fn render_quotes_nested_deeper_than_width_allows_without_exploding() {
    let source = format!("{}x", ">".repeat(60));
    let mut markdown = md(&source, 1, 0);
    let lines = plain_trimmed(&markdown.render(80));

    assert_eq!(lines.len(), 1, "Expected a single line: {lines:?}");
    assert!(
        lines[0].ends_with('x'),
        "Expected content to be kept: {lines:?}"
    );
}

// ---------------------------------------------------------------------------
// Blockquotes with multiline content
// ---------------------------------------------------------------------------

#[test]
fn apply_consistent_styling_to_all_lines_in_lazy_continuation_blockquote() {
    let mut markdown = md(">Foo\nbar", 0, 0).with_default_text_style(DefaultTextStyle {
        color: Some(styler(chalk_magenta)),
        ..Default::default()
    });

    let lines = markdown.render(80);

    let plain_lines = plain(&lines);
    let quoted_lines: Vec<&String> = plain_lines
        .iter()
        .filter(|line| line.starts_with("│ "))
        .collect();
    assert_eq!(
        quoted_lines.len(),
        2,
        "Expected 2 quoted lines, got: {plain_lines:?}"
    );

    let foo_line = find_line(&lines, "Foo").expect("Should have Foo line");
    let bar_line = find_line(&lines, "bar").expect("Should have bar line");

    assert!(
        foo_line.contains("\x1b[3m"),
        "Foo line should have italic: {foo_line}"
    );
    assert!(
        bar_line.contains("\x1b[3m"),
        "bar line should have italic: {bar_line}"
    );
    assert!(
        !foo_line.contains("\x1b[35m"),
        "Foo line should NOT have magenta color: {foo_line}"
    );
    assert!(
        !bar_line.contains("\x1b[35m"),
        "bar line should NOT have magenta color: {bar_line}"
    );
}

#[test]
fn apply_consistent_styling_to_explicit_multiline_blockquote() {
    let mut markdown = md(">Foo\n>bar", 0, 0).with_default_text_style(DefaultTextStyle {
        color: Some(styler(chalk_cyan)),
        ..Default::default()
    });

    let lines = markdown.render(80);

    let plain_lines = plain(&lines);
    let quoted_lines: Vec<&String> = plain_lines
        .iter()
        .filter(|line| line.starts_with("│ "))
        .collect();
    assert_eq!(
        quoted_lines.len(),
        2,
        "Expected 2 quoted lines, got: {plain_lines:?}"
    );

    let foo_line = find_line(&lines, "Foo").expect("Should have Foo line");
    let bar_line = find_line(&lines, "bar").expect("Should have bar line");

    assert!(foo_line.contains("\x1b[3m"), "Foo line should have italic");
    assert!(bar_line.contains("\x1b[3m"), "bar line should have italic");
    assert!(
        !foo_line.contains("\x1b[36m"),
        "Foo line should NOT have cyan color: {foo_line}"
    );
    assert!(
        !bar_line.contains("\x1b[36m"),
        "bar line should NOT have cyan color: {bar_line}"
    );
}

#[test]
fn render_list_content_inside_blockquotes() {
    let mut markdown = md("> 1. bla bla\n> - nested bullet", 0, 0);
    let lines = markdown.render(80);
    let plain_lines = plain(&lines);
    let quoted_lines: Vec<&String> = plain_lines
        .iter()
        .filter(|line| line.starts_with("│ "))
        .collect();

    assert!(
        quoted_lines.iter().any(|line| line.contains("1. bla bla")),
        "Missing ordered list item: {quoted_lines:?}"
    );
    assert!(
        quoted_lines
            .iter()
            .any(|line| line.contains("- nested bullet")),
        "Missing unordered list item: {quoted_lines:?}"
    );
}

#[test]
fn wrap_long_blockquote_lines_and_add_border_to_each_wrapped_line() {
    let long_text =
        "This is a very long blockquote line that should wrap to multiple lines when rendered";
    let source = format!("> {long_text}");
    let mut markdown = md(&source, 0, 0);
    let lines = plain_trimmed(&markdown.render(30));

    let content_lines: Vec<&String> = lines.iter().filter(|line| !line.is_empty()).collect();
    assert!(
        content_lines.len() > 1,
        "Expected multiple wrapped lines, got: {content_lines:?}"
    );

    for line in &content_lines {
        assert!(
            line.starts_with("│ "),
            "Wrapped line should have quote border: \"{line}\""
        );
    }

    let all_text = content_lines
        .iter()
        .map(|line| line.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        all_text.contains("very long"),
        "Should preserve 'very long'"
    );
    assert!(
        all_text.contains("blockquote"),
        "Should preserve 'blockquote'"
    );
    assert!(all_text.contains("multiple"), "Should preserve 'multiple'");
}

#[test]
fn properly_indent_wrapped_blockquote_lines_with_styling() {
    let mut markdown = md("> This is styled text that is long enough to wrap", 0, 0)
        .with_default_text_style(DefaultTextStyle {
            color: Some(styler(chalk_yellow)),
            italic: true,
            ..Default::default()
        });

    let lines = markdown.render(25);
    let plain_lines = plain_trimmed(&lines);

    let content_lines: Vec<&String> = plain_lines.iter().filter(|line| !line.is_empty()).collect();
    for line in &content_lines {
        assert!(
            line.starts_with("│ "),
            "Line should have quote border: \"{line}\""
        );
    }

    let all_output = lines.join("\n");
    assert!(all_output.contains("\x1b[3m"), "Should have italic");
    assert!(
        !all_output.contains("\x1b[33m"),
        "Should NOT have yellow color from default style"
    );
}

#[test]
fn render_inline_formatting_inside_blockquotes_and_reapply_quote_styling_after() {
    let mut markdown = md("> Quote with **bold** and `code`", 0, 0);
    let lines = markdown.render(80);
    let plain_lines = plain(&lines);

    assert!(
        plain_lines.iter().any(|line| line.starts_with("│ ")),
        "Should have quote border"
    );

    let all_plain = plain_lines.join(" ");
    assert!(
        all_plain.contains("Quote with"),
        "Should preserve 'Quote with'"
    );
    assert!(all_plain.contains("bold"), "Should preserve 'bold'");
    assert!(all_plain.contains("code"), "Should preserve 'code'");

    let all_output = lines.join("\n");
    assert!(all_output.contains("\x1b[1m"), "Should have bold styling");
    assert!(
        all_output.contains("\x1b[33m"),
        "Should have code styling (yellow)"
    );
    assert!(
        all_output.contains("\x1b[3m"),
        "Should have italic from quote styling"
    );
}

// ---------------------------------------------------------------------------
// Heading with inline code
// ---------------------------------------------------------------------------

/// JS 的 `slice(max(0, index - 40), index)`（此处文本均为 ASCII/ANSI）。
fn preceding_chunk(text: &str, needle: &str) -> Option<String> {
    let index = text.find(needle)?;
    let chars: Vec<char> = text[..index].chars().collect();
    let start = chars.len().saturating_sub(40);
    Some(chars[start..].iter().collect())
}

#[test]
fn preserve_heading_styling_after_inline_code() {
    let mut markdown = md("### Why `sourceInfo` should not be optional", 0, 0);
    let lines = markdown.render(80);
    let joined_output = lines.join("\n");

    assert!(
        joined_output.contains("\x1b[33m"),
        "Should have yellow for inline code"
    );

    let chunk = preceding_chunk(&joined_output, "should not be optional")
        .expect("Should contain text after inline code");
    assert!(
        chunk.contains("\x1b[1m"),
        "Should re-apply bold before text after code: {chunk}"
    );
    assert!(
        chunk.contains("\x1b[36m"),
        "Should re-apply cyan before text after code: {chunk}"
    );
}

#[test]
fn preserve_heading_styling_after_inline_code_for_h1() {
    let mut markdown = md("# Title with `code` inside", 0, 0);
    let lines = markdown.render(80);
    let joined_output = lines.join("\n");

    let chunk =
        preceding_chunk(&joined_output, "inside").expect("Should contain text after inline code");
    assert!(
        chunk.contains("\x1b[1m"),
        "Should re-apply bold for h1: {chunk}"
    );
    assert!(
        chunk.contains("\x1b[36m"),
        "Should re-apply cyan for h1: {chunk}"
    );
    assert!(
        chunk.contains("\x1b[4m"),
        "Should re-apply underline for h1: {chunk}"
    );
}

#[test]
fn not_leak_h1_underline_into_padding_when_inline_code_is_last_token() {
    let mut markdown = md("# Important distinction from `open()`", 0, 0);
    let lines = markdown.render(80);
    let rendered_line = lines.first().expect("Should render heading line");
    let content_width = chars_len(trim_end_js(&strip_sgr(rendered_line)));
    assert!(content_width > 0, "Should have visible heading content");

    let (_, cells) = scan(rendered_line);
    for column in content_width..80 {
        let style = cells.get(column).copied().unwrap_or_default();
        assert!(
            !style.underline,
            "Expected no underline in padding at col {column}"
        );
    }
}

#[test]
fn preserve_heading_styling_after_bold_text() {
    let mut markdown = md("## Heading with **bold** and more", 0, 0);
    let lines = markdown.render(80);
    let joined_output = lines.join("\n");

    let chunk =
        preceding_chunk(&joined_output, "and more").expect("Should contain text after bold");
    assert!(
        chunk.contains("\x1b[1m"),
        "Should re-apply bold for h2: {chunk}"
    );
    assert!(
        chunk.contains("\x1b[36m"),
        "Should re-apply cyan for h2: {chunk}"
    );
}

// ---------------------------------------------------------------------------
// Strikethrough syntax
// ---------------------------------------------------------------------------

#[test]
fn render_strikethrough_text() {
    let mut markdown = md("Use ~~strikethrough~~ here", 0, 0);
    let lines = markdown.render(80);
    let joined_output = lines.join("\n");
    let joined_plain = plain(&lines).join(" ");

    assert!(
        joined_output.contains("\x1b[9m"),
        "Should apply strikethrough styling"
    );
    assert!(
        joined_plain.contains("strikethrough"),
        "Should include struck text content"
    );
    assert!(
        !joined_plain.contains("~~strikethrough~~"),
        "Should not render delimiters as text"
    );
}

#[test]
fn keep_single_tilde_as_plain_text() {
    let mut markdown = md("Use ~strikethrough~ literally", 0, 0);
    let lines = markdown.render(80);
    let joined_output = lines.join("\n");
    let joined_plain = plain(&lines).join(" ");

    assert!(
        joined_plain.contains("~strikethrough~"),
        "Single-tilde delimiters should remain visible"
    );
    assert!(
        !joined_output.contains("\x1b[9m"),
        "Single-tilde text should not use strikethrough styling"
    );
}

// ---------------------------------------------------------------------------
// CJK emphasis (#10154)
// ---------------------------------------------------------------------------

fn cjk_theme() -> MarkdownTheme {
    let mut theme = default_markdown_theme();
    theme.bold = Arc::new(|text: &str| format!("<b>{text}</b>"));
    theme.italic = Arc::new(|text: &str| format!("<i>{text}</i>"));
    theme
}

fn render_cjk(source: &str) -> Vec<String> {
    let mut markdown = Markdown::new(source, 0, 0, cjk_theme());
    markdown
        .render(80)
        .iter()
        .map(|line| trim_end_js(line).to_string())
        .collect()
}

#[test]
fn renders_emphasis_delimited_next_to_cjk_punctuation() {
    assert_eq!(
        render_cjk("**这是测试。**后面"),
        expect(&["<b>这是测试。</b>后面"])
    );
    assert_eq!(
        render_cjk("**一句话说清：**一台机器"),
        expect(&["<b>一句话说清：</b>一台机器"])
    );
    assert_eq!(
        render_cjk("**要点到此为止。**20 小时"),
        expect(&["<b>要点到此为止。</b>20 小时"])
    );
    assert_eq!(
        render_cjk("字**「引用」**字"),
        expect(&["字<b>「引用」</b>字"])
    );
    assert_eq!(render_cjk("*斜体。*后面"), expect(&["<i>斜体。</i>后面"]));
}

#[test]
fn keeps_commonmark_flanking_rules_for_non_cjk_text() {
    assert_eq!(
        render_cjk("**end.**Then next"),
        expect(&["**end.**Then next"])
    );
}

// ---------------------------------------------------------------------------
// Links
// ---------------------------------------------------------------------------

#[test]
fn not_duplicate_url_for_autolinked_emails() {
    with_capabilities(false, false);
    let mut markdown = md("Contact user@example.com for help", 0, 0);
    let lines = markdown.render(80);
    let joined = joined_plain(&lines, " ");

    assert!(joined.contains("user@example.com"), "Should contain email");
    assert!(
        !joined.contains("mailto:"),
        "Should not show mailto: prefix for autolinked emails"
    );
    reset_capabilities_cache();
}

#[test]
fn not_duplicate_url_for_bare_urls() {
    with_capabilities(false, false);
    let mut markdown = md("Visit https://example.com for more", 0, 0);
    let lines = markdown.render(80);
    let joined = joined_plain(&lines, " ");

    let url_count = joined.matches("https://example.com").count();
    assert_eq!(url_count, 1, "URL should appear exactly once");
    reset_capabilities_cache();
}

#[test]
fn show_url_in_parentheses_when_hyperlinks_are_not_supported() {
    with_capabilities(false, false);
    let mut markdown = md("[click here](https://example.com)", 0, 0);
    let lines = markdown.render(80);
    let joined = joined_plain(&lines, " ");

    assert!(joined.contains("click here"), "Should contain link text");
    assert!(
        joined.contains("(https://example.com)"),
        "Should show URL in parentheses"
    );
    reset_capabilities_cache();
}

#[test]
fn show_mailto_url_in_parentheses_when_hyperlinks_are_not_supported() {
    with_capabilities(false, false);
    let mut markdown = md("[Email me](mailto:test@example.com)", 0, 0);
    let lines = markdown.render(80);
    let joined = joined_plain(&lines, " ");

    assert!(joined.contains("Email me"), "Should contain link text");
    assert!(
        joined.contains("(mailto:test@example.com)"),
        "Should show mailto URL in parentheses"
    );
    reset_capabilities_cache();
}

#[test]
fn emit_osc8_hyperlink_sequence_when_terminal_supports_hyperlinks() {
    with_capabilities(true, false);
    let mut markdown = md("[click here](https://example.com)", 0, 0);
    let lines = markdown.render(80);
    let joined = lines.join("");

    assert!(
        joined.contains("\x1b]8;;https://example.com\x1b\\"),
        "Should contain OSC 8 open sequence"
    );
    assert!(
        joined.contains("\x1b]8;;\x1b\\"),
        "Should contain OSC 8 close sequence"
    );

    let visible = lines
        .iter()
        .map(|line| strip_sgr(&strip_osc8(line)))
        .collect::<Vec<_>>()
        .join("");
    assert!(visible.contains("click here"), "Should contain link text");
    assert!(
        !visible.contains("(https://example.com)"),
        "URL should not appear inline in parentheses"
    );
    reset_capabilities_cache();
}

#[test]
fn use_osc8_for_mailto_links_when_terminal_supports_hyperlinks() {
    with_capabilities(true, false);
    let mut markdown = md("[Email me](mailto:test@example.com)", 0, 0);
    let lines = markdown.render(80);
    let joined = lines.join("");

    assert!(
        joined.contains("\x1b]8;;mailto:test@example.com\x1b\\"),
        "Should contain OSC 8 open with mailto URL"
    );
    assert!(
        joined.contains("\x1b]8;;\x1b\\"),
        "Should contain OSC 8 close sequence"
    );
    reset_capabilities_cache();
}

#[test]
fn use_osc8_for_bare_urls_when_terminal_supports_hyperlinks() {
    with_capabilities(true, false);
    let mut markdown = md("Visit https://example.com for more", 0, 0);
    let lines = markdown.render(80);
    let joined = lines.join("");

    assert!(
        joined.contains("\x1b]8;;https://example.com\x1b\\"),
        "Should contain OSC 8 hyperlink"
    );

    let visible = lines
        .iter()
        .map(|line| strip_sgr(&strip_osc8(line)))
        .collect::<Vec<_>>()
        .join("");
    assert!(
        !visible.contains("(https://example.com)"),
        "URL should not appear twice"
    );
    reset_capabilities_cache();
}

/// marked 的 gfm `url` 规则有两个分支：`protocol://|www.` 与 `^email`。裸邮箱命中第二个
/// 分支时 group 1 不参与匹配；TS 只在非邮箱分支读 `cap[1]`，所以 `foo@bar.com` 会生成
/// 一个 Link token（href = `mailto:foo@bar.com`）。回归：Rust 曾因 `cap.get(1)?` 直接
/// 返回 None，把裸邮箱当普通文本渲染。
#[test]
fn render_bare_email_as_link() {
    with_capabilities(false, false);
    let mut markdown = md("foo@bar.com", 0, 0);
    let lines = markdown.render(40);
    // 期望值取自 TS 参考实现（defaultMarkdownTheme，width 40）。
    assert_eq!(
        lines,
        vec![format!(
            "\x1b[34m\x1b[4mfoo@bar.com\x1b[24m\x1b[39m{}",
            " ".repeat(40 - chars_len("foo@bar.com"))
        )]
    );

    let plain_line = strip_sgr(&lines[0]);
    assert!(
        !plain_line.contains("mailto:"),
        "Fallback should not print the mailto URL inline: {plain_line:?}"
    );
    reset_capabilities_cache();
}

/// 与 [`render_bare_email_as_link`] 相同，但开启 hyperlinks capability：TS 会用 OSC 8
/// `mailto:` 序列包裹链接文本。
#[test]
fn use_osc8_for_bare_emails_when_terminal_supports_hyperlinks() {
    with_capabilities(true, false);
    let mut markdown = md("foo@bar.com", 0, 0);
    let lines = markdown.render(40);
    let joined = lines.join("");

    assert!(
        joined.contains("\x1b]8;;mailto:foo@bar.com\x1b\\"),
        "Should contain OSC 8 open with mailto URL: {joined:?}"
    );
    assert!(
        joined.contains("\x1b]8;;\x1b\\"),
        "Should contain OSC 8 close sequence"
    );

    let visible = lines
        .iter()
        .map(|line| strip_sgr(&strip_osc8(line)))
        .collect::<Vec<_>>()
        .join("");
    assert_eq!(trim_end_js(&visible), "foo@bar.com");
    reset_capabilities_cache();
}

/// marked 的块循环用 `src.substring(token.raw.length)` 推进源码，`substring` 会把越界
/// 下界夹到字符串长度。blockquote 的 `raw` 由行拼接重建，懒续行（最后一行没有换行）时
/// 可能比剩余源码长一字节；Rust 曾直接按字节切片导致 panic（`> - a\nb` 等）。
#[test]
fn blockquote_lazy_continuation_does_not_panic() {
    with_capabilities(false, false);
    let cases: [(&str, Vec<String>); 5] = [
        ("> - a\nb", expect(&["│ - a", "│   b"])),
        (">-\n ", expect(&["│ -"])),
        ("> 1. a\nb", expect(&["│ 1. a", "│    b"])),
        ("- a\n> - b\nc", expect(&["- a", "│ - b", "│   c"])),
        ("\t> 2) \n  `2) ", expect(&["│ 2.", "│", "│   `2)"])),
    ];

    for (input, expected) in cases {
        let mut markdown = md(input, 0, 0);
        let lines = plain_trimmed(&markdown.render(40));
        assert_eq!(lines, expected, "input: {input:?}");
    }
    reset_capabilities_cache();
}

// ---------------------------------------------------------------------------
// HTML-like tags in text
// ---------------------------------------------------------------------------

#[test]
fn render_content_with_html_like_tags_as_text() {
    let mut markdown = md(
        "This is text with <thinking>hidden content</thinking> that should be visible",
        0,
        0,
    );
    let lines = markdown.render(80);
    let joined_plain = plain(&lines).join(" ");

    assert!(
        joined_plain.contains("hidden content") || joined_plain.contains("<thinking>"),
        "Should render HTML-like tags or their content as text, not hide them"
    );
}

#[test]
fn render_html_tags_in_code_blocks_correctly() {
    let mut markdown = md("```html\n<div>Some HTML</div>\n```", 0, 0);
    let lines = markdown.render(80);
    let joined_plain = plain(&lines).join("\n");

    assert!(
        joined_plain.contains("<div>") && joined_plain.contains("</div>"),
        "Should render HTML in code blocks"
    );
}

// ---------------------------------------------------------------------------
// Streaming code fences
// ---------------------------------------------------------------------------

#[test]
fn stabilizes_partial_closing_fence_rendering() {
    let cases: [(&str, Vec<String>); 6] = [
        (
            "```ts\nconst x = 1;\n``",
            expect(&["```ts", "  const x = 1;", "```"]),
        ),
        (
            "```md\nnot a closing fence:\n``\n```",
            expect(&["```md", "  not a closing fence:", "  ``", "```"]),
        ),
        ("```ts\n``", expect(&["```ts", "", "```"])),
        ("````\n```", expect(&["```", "", "```"])),
        ("~~~~~\n~~~~", expect(&["```", "", "```"])),
        (
            "```md\nnot a closing fence:\n``\n```\n\nafter",
            expect(&[
                "```md",
                "  not a closing fence:",
                "  ``",
                "```",
                "",
                "after",
            ]),
        ),
    ];

    for (input, expected) in cases {
        let mut markdown = md(input, 0, 0);
        let lines = plain_trimmed(&markdown.render(80));
        assert_eq!(lines, expected, "input: {input:?}");
    }

    let mut partial = md("```ts\nconst x = 1;\n``", 0, 0);
    let mut complete = md("```ts\nconst x = 1;\n```", 0, 0);
    assert_eq!(partial.render(80).len(), complete.render(80).len());
}

// ---------------------------------------------------------------------------
// 复查修复：astral 首字符的扩展裁剪 / 单个空白 token / 未闭合 \[ / expandTabs
// ---------------------------------------------------------------------------

/// 行内/块级的扩展起点裁剪按 UTF-16 码元换算：JS 的 `src.slice(1)` 对 astral 首字符只切掉
/// 一个码元（留下孤立低代理），扩展起点下标回填原串时要补上 2 个码元，且 `^` 锚点不能命中
/// 切片起点。修复前这些输入会丢掉定界符之后（或整行）的内容；期望值取自 TS 参考实现。
#[test]
fn keeps_text_after_astral_char_before_latex_delimiter() {
    let cases: [(&str, Vec<String>); 13] = [
        ("😀$100", expect(&["😀$100"])),
        ("😀$", expect(&["😀$"])),
        ("😀$x$", expect(&["😀x"])),
        ("😀\\(x\\)", expect(&["😀x"])),
        ("😀\\(x\\) tail", expect(&["😀x tail"])),
        ("😀$ tail", expect(&["😀$ tail"])),
        ("**a**😀$x$ tail", expect(&["a😀x tail"])),
        ("**a**😀\\[x\\] tail", expect(&["a😀x tail"])),
        ("`c`😀\\[x\\]", expect(&["c😀x"])),
        // 块级扩展起点：`e.slice(1)` 的孤立低代理让 `^ {0,3}(?:\$\$|\\\[)` 无法命中。
        ("😀$$x$$", expect(&["😀x"])),
        ("😀\\[x\\]", expect(&["😀x"])),
        (
            "|😀$x$|b|\n|---|---|\n|c|d|",
            expect(&[
                "┌─────┬───┐",
                "│ 😀x │ b │",
                "├─────┼───┤",
                "│ c   │ d │",
                "└─────┴───┘",
            ]),
        ),
        ("> 😀$q", expect(&["│ 😀$q"])),
    ];

    for (input, expected) in cases {
        let mut markdown = md(input, 0, 0);
        let lines = plain_trimmed(&markdown.render(40));
        assert_eq!(lines, expected, "input: {input:?}");
    }
}

/// marked 的 space 合并条件是 `token.raw.length === 1`（UTF-16 码元数），不只是单个换行：
/// 输入末尾的单个空格/制表符也会折进上一个 token 的 raw，不产生额外的空行。
/// 这正是用户刚敲下列表符号时的流式状态。
#[test]
fn merges_single_unit_space_token_into_previous_token() {
    for input in ["- ", "1. ", "* "] {
        let mut markdown = md(input, 0, 0);
        let lines = plain_trimmed(&markdown.render(40));
        assert_eq!(lines.len(), 1, "input: {input:?} => {lines:?}");
    }

    // 两个及以上空格是长度 > 1 的 space token，保留为一行空行（TS 行为）。
    let mut markdown = md("-  ", 0, 0);
    assert_eq!(plain_trimmed(&markdown.render(40)), expect(&["-", ""]));

    let mut markdown = md("---\n\n- ", 0, 0);
    assert_eq!(
        plain_trimmed(&markdown.render(40)),
        vec!["─".repeat(40), String::new(), "-".to_string()]
    );
}

/// `pendingBracket?.[1]` 为空串时是假值：`   \[`（后面没有内容）走普通 paragraph/text 裁剪
/// 路径，保留前导空行，而不是生成一个 raw 之后被 trim 的 pending latexBlock。
#[test]
fn renders_pending_bracket_without_content_as_paragraph() {
    let cases: [(&str, Vec<String>); 5] = [
        ("   \\[", expect(&["", "", "", "\\["])),
        (" \\[", expect(&["", "\\["])),
        ("   \\[   ", expect(&["", "", "", "\\["])),
        ("   \\[\n", expect(&["", "", "", "\\["])),
        (">   \\[", expect(&["│", "│", "│ \\["])),
    ];

    for (input, expected) in cases {
        let mut markdown = md(input, 0, 0);
        let lines = plain_trimmed(&markdown.render(40));
        assert_eq!(lines, expected, "input: {input:?}");
    }
}

/// marked 的 `expandTabs` 用 `for (const char of line)`（按码点）并 `col++`，astral 字符占 1 列。
/// 期望值取自 `marked@18.0.11` 的 `new Marked().lexer(source)`。
#[test]
fn expand_tabs_counts_code_points() {
    let cases: [(&str, &str); 5] = [
        ("- 😀\tfoo", "😀 foo"),
        ("- 😀😀\tfoo", "😀😀    foo"),
        ("- a\tb", "a b"),
        ("- ab\tc", "ab    c"),
        ("- 😀 \tfoo", "😀     foo"),
    ];

    for (input, expected) in cases {
        let mut lexer = Lexer::new();
        let tokens = lexer.lex(input);
        match tokens.first() {
            Some(Token::List(list)) => {
                assert_eq!(list.items[0].text, expected, "input: {input:?}");
            }
            other => panic!("expected list for {input:?}, got {other:?}"),
        }
    }
}

/// marked gfm `url` 的裸邮箱分支（group 1 不参与）生成 `mailto:` Link token。
#[test]
fn bare_email_is_lexed_as_mailto_link() {
    let mut lexer = Lexer::new();
    let tokens = lexer.lex("foo@bar.com");
    let inline = match tokens.first() {
        Some(Token::Paragraph { tokens, .. }) => tokens,
        other => panic!("expected paragraph, got {other:?}"),
    };
    match inline.first() {
        Some(Token::Link {
            raw,
            href,
            text,
            image,
            ..
        }) => {
            assert_eq!(raw, "foo@bar.com");
            assert_eq!(text, "foo@bar.com");
            assert_eq!(href, "mailto:foo@bar.com");
            assert!(!image);
        }
        other => panic!("expected bare email link token, got {other:?}"),
    }
}

/// blockquote 的 `raw` 是重建出来的，可能比源串长（懒续行 + 末尾换行）。token 树保持
/// processed 结果，消费时按 `substring` 语义夹到末尾。
#[test]
fn blockquote_reconstructed_raw_may_exceed_source() {
    let mut lexer = Lexer::new();
    let tokens = lexer.lex("> - a\nb");
    let blockquote = tokens.first().expect("a token");
    assert_eq!(blockquote.type_name(), "blockquote");
    // TS：`new Marked().lexer("> - a\nb")[0].raw === "> - a\nb\n"`（源串只有 7 个码元）。
    assert_eq!(blockquote.raw(), "> - a\nb\n");
}

/// 审查报告给出的复现输入：松散任务项的 paragraph 若丢掉 checkbox，token 树会与 TS 不同
/// （渲染结果不变，因为渲染器忽略 checkbox）。
#[test]
fn loose_task_item_checkbox_survives_inline_queue_repro() {
    let mut lexer = Lexer::new();
    let tokens = lexer.lex("- [ ] `=_\n\n  - `]'2)");
    let list = match tokens.first() {
        Some(Token::List(list)) => list,
        other => panic!("expected list, got {other:?}"),
    };
    assert!(list.loose, "list should be loose");
    let paragraph_tokens = match list.items[0].tokens.first() {
        Some(Token::Paragraph { tokens, .. }) => tokens,
        other => panic!("expected paragraph, got {other:?}"),
    };
    match paragraph_tokens.first() {
        Some(Token::Checkbox { raw, checked }) => {
            assert_eq!(raw, "[ ] ");
            assert!(!checked);
        }
        other => panic!("expected checkbox first, got {other:?}"),
    }
    match paragraph_tokens.get(1) {
        Some(Token::Text { raw, text, .. }) => {
            assert_eq!(raw, "`=_");
            assert_eq!(text, "`=_");
        }
        other => panic!("expected inline text after checkbox, got {other:?}"),
    }
}

/// 松散任务项：marked 在第二遍把 checkbox `unshift` 进 paragraph 的 tokens 数组，而
/// inlineQueue 的填充是 append；因此 paragraph.tokens 以 checkbox 开头，后面才是行内 token。
#[test]
fn loose_task_item_keeps_checkbox_token_in_paragraph() {
    let mut lexer = Lexer::new();
    let tokens = lexer.lex("- [ ] a\n\n- [x] b");
    let list = match tokens.first() {
        Some(Token::List(list)) => list,
        other => panic!("expected list, got {other:?}"),
    };
    assert!(list.loose, "list should be loose");
    assert_eq!(list.items.len(), 2);

    for (index, expected_checked) in [(0usize, false), (1, true)] {
        let item = &list.items[index];
        assert_eq!(item.checked, Some(expected_checked), "item {index} checked");
        let (paragraph_raw, paragraph_text, paragraph_tokens) = match item.tokens.first() {
            Some(Token::Paragraph {
                raw, text, tokens, ..
            }) => (raw.clone(), text.clone(), tokens),
            other => panic!("expected paragraph for item {index}, got {other:?}"),
        };
        // TS：item0 paragraph raw/text 为 "[ ] a\n" / "[ ] a"，item1 为 "[x] b"。
        let expected_body = if expected_checked { "[x] b" } else { "[ ] a" };
        assert_eq!(paragraph_text, expected_body, "item {index} paragraph text");
        let expected_raw = if expected_checked {
            "[x] b".to_string()
        } else {
            "[ ] a\n".to_string()
        };
        assert_eq!(paragraph_raw, expected_raw, "item {index} paragraph raw");
        let expected_raw = if expected_checked { "[x] " } else { "[ ] " };
        match paragraph_tokens.first() {
            Some(Token::Checkbox { raw, checked }) => {
                assert_eq!(raw, expected_raw, "item {index} checkbox raw");
                assert_eq!(*checked, expected_checked, "item {index} checkbox");
            }
            other => panic!("expected checkbox first for item {index}, got {other:?}"),
        }
        assert!(
            matches!(paragraph_tokens.get(1), Some(Token::Text { .. })),
            "inline tokens should follow the checkbox: {paragraph_tokens:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 评审回归：TS 的 `Markdown` 没有 `handleInput` 成员
// ---------------------------------------------------------------------------

/// 作为聚焦 overlay 时，`handleTerminalInput` 既不调用它也不请求立即渲染
/// （`tui.ts:1147`）；Rust 侧通过 `handles_input() == false` 表达。
#[test]
fn markdown_component_does_not_handle_input() {
    let markdown = Markdown::new("# hi", 0, 0, default_markdown_theme());
    assert!(!markdown.handles_input());
}
