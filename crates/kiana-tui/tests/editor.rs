//! `packages/tui/test/editor.test.ts` 的 Rust 移植。
//!
//! 用例标题与分组保留在测试函数名/注释里。参考实现的 `flushAutocomplete()`（排空微任务）
//! 在 Rust 侧对应 [`flush`]（多次 `Editor::tick`）：补全请求在编辑器内是可轮询 future，
//! 请求只排队、由 `tick`/`render` 推进（对应 TS 的微任务边界），未就绪的由后续 tick 继续轮询。
//! TS 的 `t.mock.timers.tick(...)` 对应 [`FakeClock`] + `Editor::tick`。

mod common;

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::TestTerminal;
use kiana_tui::autocomplete::{
    ApplyCompletionResult, AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions,
    GetSuggestionsOptions,
};
use kiana_tui::components::editor::{
    Editor, EditorOptions, EditorTheme, EditorTui, TextSegment, word_wrap_line,
};
use kiana_tui::components::select_list::SelectListTheme;
use kiana_tui::terminal_colors::Terminal;
use kiana_tui::tui::FAKE_CURSOR_START;
use kiana_tui::{
    Component, Focusable, render_fake_cursor, strip_terminal_sequences, visible_width,
};

// ---------------------------------------------------------------------------
// 测试基础设施
// ---------------------------------------------------------------------------

/// 80x24 的测试终端（TS `createTestTUI()` 默认）。
struct Harness {
    editor: Editor,
    _renders: Rc<Cell<usize>>,
}

impl Harness {
    fn new() -> Self {
        Self::with_size(80, 24)
    }

    fn with_size(columns: usize, rows: usize) -> Self {
        let terminal: Rc<RefCell<Box<dyn Terminal>>> = Rc::new(RefCell::new(Box::new(
            TestTerminal::with_size(columns, rows),
        )));
        let renders = Rc::new(Cell::new(0usize));
        let sink = Rc::clone(&renders);
        let request_render: Rc<dyn Fn()> = Rc::new(move || sink.set(sink.get() + 1));
        let editor = Editor::new(
            EditorTui::new(terminal, request_render),
            EditorTheme::plain(),
            EditorOptions::default(),
        );
        Harness {
            editor,
            _renders: renders,
        }
    }

    fn with_theme(theme: EditorTheme) -> Self {
        Self::with_size_and_theme(80, 24, theme)
    }

    fn with_size_and_theme(columns: usize, rows: usize, theme: EditorTheme) -> Self {
        let terminal: Rc<RefCell<Box<dyn Terminal>>> = Rc::new(RefCell::new(Box::new(
            TestTerminal::with_size(columns, rows),
        )));
        let renders = Rc::new(Cell::new(0usize));
        let sink = Rc::clone(&renders);
        let request_render: Rc<dyn Fn()> = Rc::new(move || sink.set(sink.get() + 1));
        let editor = Editor::new(
            EditorTui::new(terminal, request_render),
            theme,
            EditorOptions::default(),
        );
        Harness {
            editor,
            _renders: renders,
        }
    }

    fn with_options(columns: usize, rows: usize, options: EditorOptions) -> Self {
        let terminal: Rc<RefCell<Box<dyn Terminal>>> = Rc::new(RefCell::new(Box::new(
            TestTerminal::with_size(columns, rows),
        )));
        let renders = Rc::new(Cell::new(0usize));
        let sink = Rc::clone(&renders);
        let request_render: Rc<dyn Fn()> = Rc::new(move || sink.set(sink.get() + 1));
        let editor = Editor::new(
            EditorTui::new(terminal, request_render),
            EditorTheme::plain(),
            options,
        );
        Harness {
            editor,
            _renders: renders,
        }
    }

    fn input(&mut self, data: &str) {
        self.editor.handle_input(data);
    }

    fn render(&mut self, width: usize) -> Vec<String> {
        self.editor.render(width)
    }

    fn text(&self) -> String {
        self.editor.get_text()
    }

    fn cursor(&self) -> (usize, usize) {
        self.editor.get_cursor()
    }
}

/// TS `flushAutocomplete()` 的 Rust 等价物：多次轮询（防抖到期 + 在飞请求）。
fn flush(editor: &mut Editor) {
    for _ in 0..3 {
        editor.tick();
    }
}

/// 可手动推进的假时钟（毫秒）。
#[derive(Clone)]
struct FakeClock(Rc<Cell<f64>>);

impl FakeClock {
    fn new() -> Self {
        FakeClock(Rc::new(Cell::new(0.0)))
    }

    fn advance(&self, milliseconds: f64) {
        self.0.set(self.0.get() + milliseconds);
    }

    fn as_fn(&self) -> Rc<dyn Fn() -> f64> {
        let value = Rc::clone(&self.0);
        Rc::new(move || value.get())
    }
}

/// TS 测试里的标准 `applyCompletion`（用 `item.value` 替换 `prefix`）。
fn apply_replacing_prefix(
    lines: &[String],
    cursor_line: usize,
    cursor_col: usize,
    value: &str,
    prefix: &str,
) -> ApplyCompletionResult {
    let line = lines.get(cursor_line).cloned().unwrap_or_default();
    let prefix_len = utf16_len(prefix);
    let before = utf16_slice(&line, 0, cursor_col.saturating_sub(prefix_len));
    let after = utf16_slice(&line, cursor_col, utf16_len(&line));
    let mut new_lines = lines.to_vec();
    while new_lines.len() <= cursor_line {
        new_lines.push(String::new());
    }
    new_lines[cursor_line] = format!("{before}{value}{after}");
    ApplyCompletionResult {
        lines: new_lines,
        cursor_line,
        cursor_col: cursor_col - prefix_len + utf16_len(value),
    }
}

/// 同步 mock provider：handler 立即返回结果（对应 TS 里立即 resolve 的 async 函数）。
type MockHandler = Box<
    dyn FnMut(&[String], usize, usize, &GetSuggestionsOptions) -> Option<AutocompleteSuggestions>
        + Send,
>;

struct MockProvider {
    trigger_characters: Option<Vec<char>>,
    handler: MockHandler,
}

impl MockProvider {
    fn new(
        handler: impl FnMut(
            &[String],
            usize,
            usize,
            &GetSuggestionsOptions,
        ) -> Option<AutocompleteSuggestions>
        + Send
        + 'static,
    ) -> Self {
        MockProvider {
            trigger_characters: None,
            handler: Box::new(handler),
        }
    }

    fn with_triggers(mut self, triggers: Vec<char>) -> Self {
        self.trigger_characters = Some(triggers);
        self
    }
}

impl AutocompleteProvider for MockProvider {
    fn trigger_characters(&self) -> Option<Vec<char>> {
        self.trigger_characters.clone()
    }

    fn get_suggestions<'a>(
        &'a mut self,
        lines: &'a [String],
        cursor_line: usize,
        cursor_col: usize,
        options: &'a GetSuggestionsOptions,
    ) -> Pin<Box<dyn Future<Output = Option<AutocompleteSuggestions>> + Send + 'a>> {
        let result = (self.handler)(lines, cursor_line, cursor_col, options);
        Box::pin(std::future::ready(result))
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> ApplyCompletionResult {
        apply_replacing_prefix(lines, cursor_line, cursor_col, &item.value, prefix)
    }
}

fn item(value: &str) -> AutocompleteItem {
    AutocompleteItem {
        value: value.to_string(),
        label: value.to_string(),
        description: None,
    }
}

fn suggestions(items: Vec<AutocompleteItem>, prefix: &str) -> AutocompleteSuggestions {
    AutocompleteSuggestions {
        items,
        prefix: prefix.to_string(),
    }
}

// --- UTF-16 辅助（与编辑器内部实现独立，用于断言） ---

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn utf16_slice(text: &str, start: usize, end: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let start = start.min(units.len());
    let end = end.min(units.len());
    if end <= start {
        return String::new();
    }
    String::from_utf16_lossy(&units[start..end])
}

/// TS 测试里的 `stripVTControlCharacters`。
fn strip(text: &str) -> String {
    strip_terminal_sequences(text)
}

/// 内容行（去掉上下边框）。
fn content_lines(lines: &[String]) -> &[String] {
    &lines[1..lines.len() - 1]
}

// ===========================================================================
// Prompt history navigation（15）
// ===========================================================================

#[test]
fn history_does_nothing_on_up_arrow_when_history_is_empty() {
    let mut h = Harness::new();
    h.input("\x1b[A");
    assert_eq!(h.text(), "");
}

#[test]
fn history_shows_most_recent_entry_on_up_arrow_when_editor_is_empty() {
    let mut h = Harness::new();
    h.editor.add_to_history("first prompt");
    h.editor.add_to_history("second prompt");
    h.input("\x1b[A");
    assert_eq!(h.text(), "second prompt");
}

#[test]
fn history_cycles_through_entries_on_repeated_up_arrow() {
    let mut h = Harness::new();
    h.editor.add_to_history("first");
    h.editor.add_to_history("second");
    h.editor.add_to_history("third");

    h.input("\x1b[A");
    assert_eq!(h.text(), "third");
    h.input("\x1b[A");
    assert_eq!(h.text(), "second");
    h.input("\x1b[A");
    assert_eq!(h.text(), "first");
    h.input("\x1b[A");
    assert_eq!(h.text(), "first");
}

#[test]
fn history_jumps_to_start_before_entering_history_from_a_non_empty_draft() {
    let mut h = Harness::new();
    h.editor.add_to_history("prompt");
    h.editor.set_text("draft");
    h.input("\x1b[D");
    h.input("\x1b[D");

    h.input("\x1b[A");
    assert_eq!(h.text(), "draft");
    assert_eq!(h.cursor(), (0, 0));

    h.input("\x1b[A");
    assert_eq!(h.text(), "prompt");

    h.input("\x1b[B");
    assert_eq!(h.text(), "draft");
    assert_eq!(h.cursor(), (0, 0));
}

#[test]
fn history_navigates_forward_through_history_with_down_arrow() {
    let mut h = Harness::new();
    h.editor.add_to_history("first");
    h.editor.add_to_history("second");
    h.editor.add_to_history("third");
    h.editor.set_text("draft");

    h.input("\x1b[A");
    h.input("\x1b[A");
    h.input("\x1b[A");
    h.input("\x1b[A");

    h.input("\x1b[B");
    assert_eq!(h.text(), "second");
    h.input("\x1b[B");
    assert_eq!(h.text(), "third");
    h.input("\x1b[B");
    assert_eq!(h.text(), "draft");
}

#[test]
fn history_exits_history_mode_when_typing_a_character() {
    let mut h = Harness::new();
    h.editor.add_to_history("old prompt");
    h.input("\x1b[A");
    h.input("x");
    assert_eq!(h.text(), "xold prompt");
}

#[test]
fn history_exits_history_mode_on_set_text() {
    let mut h = Harness::new();
    h.editor.add_to_history("first");
    h.editor.add_to_history("second");
    h.input("\x1b[A");
    h.editor.set_text("");
    h.input("\x1b[A");
    assert_eq!(h.text(), "second");
}

#[test]
fn history_does_not_add_empty_strings_to_history() {
    let mut h = Harness::new();
    h.editor.add_to_history("");
    h.editor.add_to_history("   ");
    h.editor.add_to_history("valid");

    h.input("\x1b[A");
    assert_eq!(h.text(), "valid");
    h.input("\x1b[A");
    assert_eq!(h.text(), "valid");
}

#[test]
fn history_does_not_add_consecutive_duplicates_to_history() {
    let mut h = Harness::new();
    h.editor.add_to_history("same");
    h.editor.add_to_history("same");
    h.editor.add_to_history("same");

    h.input("\x1b[A");
    assert_eq!(h.text(), "same");
    h.input("\x1b[A");
    assert_eq!(h.text(), "same");
}

#[test]
fn history_allows_non_consecutive_duplicates_in_history() {
    let mut h = Harness::new();
    h.editor.add_to_history("first");
    h.editor.add_to_history("second");
    h.editor.add_to_history("first");

    h.input("\x1b[A");
    assert_eq!(h.text(), "first");
    h.input("\x1b[A");
    assert_eq!(h.text(), "second");
    h.input("\x1b[A");
    assert_eq!(h.text(), "first");
}

#[test]
fn history_uses_cursor_movement_instead_of_history_when_editor_has_content() {
    let mut h = Harness::new();
    h.editor.add_to_history("history item");
    h.editor.set_text("line1\nline2");

    h.input("\x1b[A");
    h.input("X");
    assert_eq!(h.text(), "line1X\nline2");
}

#[test]
fn history_limits_history_to_100_entries() {
    let mut h = Harness::new();
    for i in 0..105 {
        h.editor.add_to_history(&format!("prompt {i}"));
    }
    for _ in 0..100 {
        h.input("\x1b[A");
    }
    assert_eq!(h.text(), "prompt 5");
    h.input("\x1b[A");
    assert_eq!(h.text(), "prompt 5");
}

#[test]
fn history_places_cursor_at_start_after_browsing_history_upward() {
    let mut h = Harness::new();
    h.editor.add_to_history("older entry");
    h.editor.add_to_history("line1\nline2\nline3");

    h.input("\x1b[A");
    assert_eq!(h.text(), "line1\nline2\nline3");
    assert_eq!(h.cursor(), (0, 0));

    h.input("\x1b[A");
    assert_eq!(h.text(), "older entry");
    assert_eq!(h.cursor(), (0, 0));
}

#[test]
fn history_places_cursor_at_end_after_browsing_history_downward() {
    let mut h = Harness::new();
    h.editor.add_to_history("older entry");
    h.editor.add_to_history("line1\nline2\nline3");
    h.editor.add_to_history("newer entry");

    h.input("\x1b[A");
    h.input("\x1b[A");
    h.input("\x1b[A");

    h.input("\x1b[B");
    assert_eq!(h.text(), "line1\nline2\nline3");
    assert_eq!(h.cursor(), (2, 5));

    h.input("\x1b[B");
    assert_eq!(h.text(), "newer entry");
}

#[test]
fn history_allows_opposite_direction_cursor_movement_within_multi_line_entry() {
    let mut h = Harness::new();
    h.editor.add_to_history("line1\nline2\nline3");

    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, 0));

    h.input("\x1b[B");
    assert_eq!(h.text(), "line1\nline2\nline3");
    assert_eq!(h.cursor(), (1, 0));

    h.input("\x1b[A");
    assert_eq!(h.text(), "line1\nline2\nline3");
    assert_eq!(h.cursor(), (0, 0));
}

// ===========================================================================
// public state accessors（2）
// ===========================================================================

#[test]
fn accessors_return_cursor_position() {
    let mut h = Harness::new();
    assert_eq!(h.cursor(), (0, 0));
    h.input("a");
    h.input("b");
    h.input("c");
    assert_eq!(h.cursor(), (0, 3));
    h.input("\x1b[D");
    assert_eq!(h.cursor(), (0, 2));
}

#[test]
fn accessors_return_lines_as_a_defensive_copy() {
    let mut h = Harness::new();
    h.editor.set_text("a\nb");
    let mut lines = h.editor.get_lines();
    assert_eq!(lines, vec!["a".to_string(), "b".to_string()]);
    lines[0] = "mutated".to_string();
    assert_eq!(h.editor.get_lines(), vec!["a".to_string(), "b".to_string()]);
}

// ===========================================================================
// Backslash+Enter newline workaround（5）
// ===========================================================================

#[test]
fn backslash_inserts_immediately_no_buffering() {
    let mut h = Harness::new();
    h.input("\\");
    assert_eq!(h.text(), "\\");
}

#[test]
fn backslash_converts_standalone_backslash_to_newline_on_enter() {
    let mut h = Harness::new();
    h.input("\\");
    h.input("\r");
    assert_eq!(h.text(), "\n");
}

#[test]
fn backslash_inserts_backslash_normally_when_followed_by_other_characters() {
    let mut h = Harness::new();
    h.input("\\");
    h.input("x");
    assert_eq!(h.text(), "\\x");
}

#[test]
fn backslash_does_not_trigger_newline_when_backslash_is_not_immediately_before_cursor() {
    let mut h = Harness::new();
    let submitted = Rc::new(Cell::new(false));
    let sink = Rc::clone(&submitted);
    h.editor.on_submit = Some(Box::new(move |_| sink.set(true)));

    h.input("\\");
    h.input("x");
    h.input("\r");
    assert!(submitted.get());
}

#[test]
fn backslash_only_removes_one_backslash_when_multiple_are_present() {
    let mut h = Harness::new();
    h.input("\\");
    h.input("\\");
    h.input("\\");
    assert_eq!(h.text(), "\\\\\\");
    h.input("\r");
    assert_eq!(h.text(), "\\\\\n");
}

// ===========================================================================
// Kitty CSI-u handling（3）
// ===========================================================================

#[test]
fn kitty_ignores_printable_csi_u_sequences_with_unsupported_modifiers() {
    let mut h = Harness::new();
    h.input("\x1b[99;9u");
    assert_eq!(h.text(), "");
}

#[test]
fn kitty_inserts_shifted_csi_u_letters_as_text() {
    let mut h = Harness::new();
    h.input("\x1b[69;2u");
    assert_eq!(h.text(), "E");
}

#[test]
fn kitty_inserts_shifted_xterm_modify_other_keys_letters_as_text() {
    let mut h = Harness::new();
    h.input("\x1b[27;2;69~");
    assert_eq!(h.text(), "E");
}

// ===========================================================================
// Unicode text editing behavior（12）
// ===========================================================================

#[test]
fn unicode_inserts_mixed_ascii_umlauts_and_emojis_as_literal_text() {
    let mut h = Harness::new();
    for ch in ["H", "e", "l", "l", "o", " ", "ä", "ö", "ü", " ", "😀"] {
        h.input(ch);
    }
    assert_eq!(h.text(), "Hello äöü 😀");
}

#[test]
fn unicode_deletes_single_code_unit_umlauts_with_backspace() {
    let mut h = Harness::new();
    h.input("ä");
    h.input("ö");
    h.input("ü");
    h.input("\x7f");
    assert_eq!(h.text(), "äö");
}

#[test]
fn unicode_deletes_multi_code_unit_emojis_with_single_backspace() {
    let mut h = Harness::new();
    h.input("😀");
    h.input("👍");
    h.input("\x7f");
    assert_eq!(h.text(), "😀");
}

#[test]
fn unicode_inserts_characters_at_correct_position_after_cursor_movement_over_umlauts() {
    let mut h = Harness::new();
    h.input("ä");
    h.input("ö");
    h.input("ü");
    h.input("\x1b[D");
    h.input("\x1b[D");
    h.input("x");
    assert_eq!(h.text(), "äxöü");
}

#[test]
fn unicode_moves_cursor_across_multi_code_unit_emojis_with_single_arrow_key() {
    let mut h = Harness::new();
    h.input("😀");
    h.input("👍");
    h.input("🎉");
    h.input("\x1b[D");
    h.input("\x1b[D");
    h.input("x");
    assert_eq!(h.text(), "😀x👍🎉");
}

#[test]
fn unicode_preserves_umlauts_across_line_breaks() {
    let mut h = Harness::new();
    for ch in ["ä", "ö", "ü", "\n", "Ä", "Ö", "Ü"] {
        h.input(ch);
    }
    assert_eq!(h.text(), "äöü\nÄÖÜ");
}

#[test]
fn unicode_replaces_entire_document_with_unicode_text_via_set_text() {
    let mut h = Harness::new();
    h.editor.set_text("Hällö Wörld! 😀 äöüÄÖÜß");
    assert_eq!(h.text(), "Hällö Wörld! 😀 äöüÄÖÜß");
}

#[test]
fn unicode_moves_cursor_to_document_start_on_ctrl_a_and_inserts_at_beginning() {
    let mut h = Harness::new();
    h.input("a");
    h.input("b");
    h.input("\x01");
    h.input("x");
    assert_eq!(h.text(), "xab");
}

#[test]
fn unicode_deletes_words_correctly_with_ctrl_w_and_alt_backspace() {
    let mut h = Harness::new();

    h.editor.set_text("foo bar baz");
    h.input("\x17");
    assert_eq!(h.text(), "foo bar ");

    h.editor.set_text("foo bar   ");
    h.input("\x17");
    assert_eq!(h.text(), "foo ");

    h.editor.set_text("foo bar...");
    h.input("\x17");
    assert_eq!(h.text(), "foo bar");

    h.editor.set_text("foo.bar");
    h.input("\x17");
    assert_eq!(h.text(), "foo.");

    h.editor.set_text("foo:bar");
    h.input("\x17");
    assert_eq!(h.text(), "foo:");

    h.editor.set_text("line one\nline two");
    h.input("\x17");
    assert_eq!(h.text(), "line one\nline ");

    h.editor.set_text("line one\n");
    h.input("\x17");
    assert_eq!(h.text(), "line one");

    h.editor.set_text("foo 😀😀 bar");
    h.input("\x17");
    assert_eq!(h.text(), "foo 😀😀 ");
    h.input("\x17");
    assert_eq!(h.text(), "foo ");

    h.editor.set_text("foo bar");
    h.input("\x1b\x7f");
    assert_eq!(h.text(), "foo ");
}

#[test]
fn unicode_navigates_words_correctly_with_ctrl_left_right() {
    let mut h = Harness::new();
    h.editor.set_text("foo bar... baz");

    h.input("\x1b[1;5D");
    assert_eq!(h.cursor(), (0, 11));
    h.input("\x1b[1;5D");
    assert_eq!(h.cursor(), (0, 7));
    h.input("\x1b[1;5D");
    assert_eq!(h.cursor(), (0, 4));

    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (0, 7));
    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (0, 10));
    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (0, 14));

    h.editor.set_text("   foo bar");
    h.input("\x01");
    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (0, 6));

    h.editor.set_text("foo.bar baz");
    h.input("\x1b[1;5D");
    assert_eq!(h.cursor(), (0, 8));
    h.input("\x1b[1;5D");
    assert_eq!(h.cursor(), (0, 4));
    h.input("\x1b[1;5D");
    assert_eq!(h.cursor(), (0, 3));

    h.input("\x01");
    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (0, 3));
    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (0, 4));
    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (0, 7));
}

#[test]
fn unicode_stops_at_fullwidth_chinese_punctuation_issue_4972() {
    let mut h = Harness::new();
    h.editor.set_text("你好，世界");

    h.input("\x1b[1;5D");
    assert_eq!(h.cursor(), (0, 3));
    h.input("\x1b[1;5D");
    assert_eq!(h.cursor(), (0, 2));
    h.input("\x1b[1;5D");
    assert_eq!(h.cursor(), (0, 0));

    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (0, 2));
    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (0, 3));
    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (0, 5));
}

#[test]
fn unicode_handles_mixed_cjk_and_ascii_word_movement() {
    let mut h = Harness::new();
    h.editor.set_text("hello你好，world世界");

    h.input("\x1b[1;5D");
    assert_eq!(h.cursor(), (0, 13));
    h.input("\x1b[1;5D");
    assert_eq!(h.cursor(), (0, 8));
    h.input("\x1b[1;5D");
    assert_eq!(h.cursor(), (0, 7));
    h.input("\x1b[1;5D");
    assert_eq!(h.cursor(), (0, 5));
    h.input("\x1b[1;5D");
    assert_eq!(h.cursor(), (0, 0));

    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (0, 5));
    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (0, 7));
    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (0, 8));
    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (0, 13));
    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (0, 15));
}

// ===========================================================================
// Scroll indicators（2）
// ===========================================================================

#[test]
fn scroll_indicators_center_on_wide_borders() {
    let width = 40;
    let mut h = Harness::with_size(width, 24);
    let text: Vec<String> = (0..20).map(|index| format!("line {index}")).collect();
    h.editor.set_text(&text.join("\n"));

    h.render(width);
    for _ in 0..10 {
        h.input("\x1b[A");
    }

    let lines = h.render(width);
    assert_eq!(
        strip(lines.first().unwrap()),
        format!("{} ↑ 9 more {}", "─".repeat(15), "─".repeat(15))
    );
    assert_eq!(
        strip(lines.last().unwrap()),
        format!("{} ↓ 4 more {}", "─".repeat(15), "─".repeat(15))
    );
}

#[test]
fn scroll_indicators_keep_truncated_indicators_within_width_and_preserve_color_issue_6962() {
    let width = 10;
    let border_color = |text: &str| format!("\x1b[35m{text}\x1b[39m");
    let theme = EditorTheme {
        border_color: Arc::new(border_color),
        select_list: SelectListTheme::plain(),
    };
    let mut h = Harness::with_theme(theme);
    let text: Vec<String> = (0..20).map(|index| format!("line {index}")).collect();
    h.editor.set_text(&text.join("\n"));

    h.render(width);
    for _ in 0..10 {
        h.input("\x1b[A");
    }

    let lines = h.render(width);
    let top_border = lines.first().unwrap().clone();
    let bottom_border = lines.last().unwrap().clone();

    assert!(strip(&top_border).starts_with("─── ↑"));
    assert!(strip(&bottom_border).starts_with("─── ↓"));
    assert_eq!(top_border, border_color(&strip(&top_border)));
    assert_eq!(bottom_border, border_color(&strip(&bottom_border)));
    for line in &lines {
        assert_eq!(
            visible_width(line),
            width,
            "line exceeds width {width}: {line:?}"
        );
    }
}

// ===========================================================================
// Grapheme-aware text wrapping（8）
// ===========================================================================

#[test]
fn grapheme_wraps_lines_correctly_when_text_contains_wide_emojis() {
    let mut h = Harness::new();
    let width = 20;
    h.editor.set_text("Hello ✅ World");
    let lines = h.render(width);
    for line in content_lines(&lines) {
        assert_eq!(
            visible_width(line),
            width,
            "line has width {}",
            visible_width(line)
        );
    }
}

#[test]
fn grapheme_wraps_long_text_with_emojis_at_correct_positions() {
    let mut h = Harness::new();
    let width = 10;
    h.editor.set_text("✅✅✅✅✅✅");
    let lines = h.render(width);
    for line in content_lines(&lines) {
        assert_eq!(
            visible_width(line),
            width,
            "line has width {}",
            visible_width(line)
        );
    }
}

#[test]
fn grapheme_renders_isolated_thai_and_lao_am_clusters_without_width_drift() {
    for text in ["ำabc", "ຳabc"] {
        let mut h = Harness::new();
        let width = 8;
        h.editor.set_text(text);
        for line in h.render(width) {
            assert_eq!(
                visible_width(&line),
                width,
                "line width drift for {text:?}: {line}"
            );
        }
    }
}

#[test]
fn grapheme_wraps_cjk_characters_correctly() {
    let mut h = Harness::new();
    let width = 10 + 1;
    h.editor.set_text("日本語テスト");
    let lines = h.render(width);

    for line in content_lines(&lines) {
        assert_eq!(visible_width(line), width);
    }

    let contents: Vec<String> = content_lines(&lines)
        .iter()
        .map(|line| strip(line).trim().to_string())
        .collect();
    assert_eq!(contents.len(), 2);
    assert_eq!(contents[0], "日本語テス");
    assert_eq!(contents[1], "ト");
}

#[test]
fn grapheme_handles_mixed_ascii_and_wide_characters_in_wrapping() {
    let mut h = Harness::new();
    let width = 15 + 1;
    h.editor.set_text("Test ✅ OK 日本");
    let lines = h.render(width);
    let contents = content_lines(&lines);
    assert_eq!(contents.len(), 1);
    assert_eq!(visible_width(&contents[0]), width);
}

#[test]
fn grapheme_renders_cursor_correctly_on_wide_characters() {
    let mut h = Harness::new();
    let width = 20;
    h.editor.set_text("A✅B");
    let lines = h.render(width);
    let content_line = &lines[1];
    assert!(
        content_line.contains(FAKE_CURSOR_START),
        "Should have fake cursor"
    );
    assert_eq!(visible_width(content_line), width);
}

#[test]
fn grapheme_does_not_exceed_terminal_width_with_emoji_at_wrap_boundary() {
    let mut h = Harness::new();
    let width = 11;
    h.editor.set_text("0123456789✅");
    let lines = h.render(width);
    for line in content_lines(&lines) {
        assert!(
            visible_width(line) <= width,
            "line has width {} exceeds max {width}",
            visible_width(line)
        );
    }
}

#[test]
fn grapheme_shows_cursor_at_end_of_line_before_wrap_and_wraps_on_next_char() {
    let width = 10;
    for padding_x in [0.0, 1.0] {
        let mut h = Harness::with_options(
            width + padding_x as usize,
            24,
            EditorOptions {
                padding_x: Some(padding_x),
                autocomplete_max_visible: None,
            },
        );
        for ch in "aaaaaaaaa".chars() {
            h.input(&ch.to_string());
        }
        let lines = h.render(width + padding_x as usize);
        let contents = content_lines(&lines);
        assert_eq!(contents.len(), 1, "Should be 1 content line before wrap");
        assert!(
            contents[0].ends_with(&render_fake_cursor(" ")),
            "Cursor should be at end of line"
        );

        h.input("a");
        let lines = h.render(width + padding_x as usize);
        assert_eq!(
            content_lines(&lines).len(),
            2,
            "Should wrap to 2 content lines"
        );
    }
}

// ===========================================================================
// Word wrapping（22）
// ===========================================================================

#[test]
fn word_wrap_wraps_at_word_boundaries_instead_of_mid_word() {
    let mut h = Harness::new();
    let width = 40;
    h.editor
        .set_text("Hello world this is a test of word wrapping functionality");
    let lines = h.render(width);
    let contents: Vec<String> = content_lines(&lines)
        .iter()
        .map(|line| strip(line).trim().to_string())
        .collect();
    assert!(!contents[0].ends_with('-'));
    for line in contents {
        let last_char = line.trim_end().chars().last().map(|ch| ch.to_string());
        let ok = match &last_char {
            None => true,
            Some(ch) => ch
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || ".,!?;:".contains(c)),
        };
        assert!(ok, "Line ends unexpectedly with: {last_char:?}");
    }
}

#[test]
fn word_wrap_does_not_start_lines_with_leading_whitespace_after_word_wrap() {
    let mut h = Harness::new();
    let width = 20;
    h.editor.set_text("Word1 Word2 Word3 Word4 Word5 Word6");
    let lines = h.render(width);
    for (i, line) in content_lines(&lines).iter().enumerate() {
        let line = strip(line);
        let trimmed_start = line.trim_start();
        if !trimmed_start.is_empty() {
            // `/^\s+\S/.test(line.trimEnd())`
            let starts_with_whitespace = line.chars().next().is_some_and(char::is_whitespace);
            assert!(
                !starts_with_whitespace,
                "Line {i} starts with unexpected whitespace before content"
            );
        }
    }
}

#[test]
fn word_wrap_breaks_long_words_urls_at_character_level() {
    let mut h = Harness::new();
    let width = 30;
    h.editor
        .set_text("Check https://example.com/very/long/path/that/exceeds/width here");
    let lines = h.render(width);
    for line in content_lines(&lines) {
        assert_eq!(visible_width(line), width);
    }
}

#[test]
fn word_wrap_preserves_multiple_spaces_within_words_on_same_line() {
    let mut h = Harness::new();
    let width = 50;
    h.editor.set_text("Word1   Word2    Word3");
    let lines = h.render(width);
    let content_line = strip(&lines[1]).trim().to_string();
    assert!(content_line.contains("Word1   Word2"));
}

#[test]
fn word_wrap_handles_empty_string() {
    let mut h = Harness::new();
    h.editor.set_text("");
    let lines = h.render(40);
    assert_eq!(lines.len(), 3);
}

#[test]
fn word_wrap_handles_single_word_that_fits_exactly() {
    let mut h = Harness::new();
    let width = 10 + 1;
    h.editor.set_text("1234567890");
    let lines = h.render(width);
    assert_eq!(lines.len(), 3);
    let content_line = strip(&lines[1]);
    assert!(content_line.contains("1234567890"));
}

#[test]
fn word_wrap_wraps_word_to_next_line_when_it_ends_exactly_at_terminal_width() {
    let chunks = word_wrap_line("hello world test", 11, None);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].text, "hello ");
    assert_eq!(chunks[1].text, "world test");
}

#[test]
fn word_wrap_keeps_whitespace_at_terminal_width_boundary_on_same_line() {
    let chunks = word_wrap_line("hello world test", 12, None);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].text, "hello world ");
    assert_eq!(chunks[1].text, "test");
}

#[test]
fn word_wrap_handles_unbreakable_word_filling_width_exactly_followed_by_space() {
    let chunks = word_wrap_line("aaaaaaaaaaaa aaaa", 12, None);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].text, "aaaaaaaaaaaa");
    assert_eq!(chunks[1].text, " aaaa");
}

#[test]
fn word_wrap_wraps_word_to_next_line_when_it_fits_width_but_not_remaining_space() {
    let chunks = word_wrap_line("      aaaaaaaaaaaa", 12, None);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].text, "      ");
    assert_eq!(chunks[1].text, "aaaaaaaaaaaa");
}

#[test]
fn word_wrap_keeps_word_with_multi_space_and_following_word_together_when_they_fit() {
    let chunks = word_wrap_line("Lorem ipsum dolor sit amet,    consectetur", 30, None);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].text, "Lorem ipsum dolor sit ");
    assert_eq!(chunks[1].text, "amet,    consectetur");
}

#[test]
fn word_wrap_keeps_word_with_multi_space_when_they_fill_width_exactly() {
    let chunks = word_wrap_line(
        "Lorem ipsum dolor sit amet,              consectetur",
        30,
        None,
    );
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].text, "Lorem ipsum dolor sit ");
    assert_eq!(chunks[1].text, "amet,              consectetur");
}

#[test]
fn word_wrap_splits_when_word_plus_multi_space_plus_word_exceeds_width() {
    let chunks = word_wrap_line(
        "Lorem ipsum dolor sit amet,               consectetur",
        30,
        None,
    );
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].text, "Lorem ipsum dolor sit ");
    assert_eq!(chunks[1].text, "amet,               ");
    assert_eq!(chunks[2].text, "consectetur");
}

#[test]
fn word_wrap_breaks_long_whitespace_at_line_boundary() {
    let chunks = word_wrap_line(
        "Lorem ipsum dolor sit amet,                         consectetur",
        30,
        None,
    );
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].text, "Lorem ipsum dolor sit ");
    assert_eq!(chunks[1].text, "amet,                         ");
    assert_eq!(chunks[2].text, "consectetur");
}

#[test]
fn word_wrap_breaks_long_whitespace_at_line_boundary_2() {
    let chunks = word_wrap_line(
        "Lorem ipsum dolor sit amet,                          consectetur",
        30,
        None,
    );
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].text, "Lorem ipsum dolor sit ");
    assert_eq!(chunks[1].text, "amet,                         ");
    assert_eq!(chunks[2].text, " consectetur");
}

#[test]
fn word_wrap_breaks_whitespace_spanning_full_lines() {
    let chunks = word_wrap_line(
        "Lorem ipsum dolor sit amet,                                     consectetur",
        30,
        None,
    );
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].text, "Lorem ipsum dolor sit ");
    assert_eq!(chunks[1].text, "amet,                         ");
    assert_eq!(chunks[2].text, "            consectetur");
}

#[test]
fn word_wrap_force_breaks_when_wide_char_after_word_boundary_wrap_still_overflows() {
    let line = format!(" {}{}", "a".repeat(186), "你");
    let chunks = word_wrap_line(&line, 187, None);
    for chunk in &chunks {
        assert!(
            visible_width(&chunk.text) <= 187,
            "chunk has visible width {} expected <= 187",
            visible_width(&chunk.text)
        );
    }
    let reconstructed: String = chunks
        .iter()
        .map(|chunk| utf16_slice(&line, chunk.start_index, chunk.end_index))
        .collect();
    assert_eq!(reconstructed, line);
}

fn inline_segment(segment: &str, index: usize) -> TextSegment {
    TextSegment {
        segment: segment.to_string(),
        index,
    }
}

#[test]
fn word_wrap_splits_oversized_atomic_segment_across_multiple_chunks() {
    let marker = "[paste #1 +20 lines]";
    let line = format!("A{marker}B");
    let segments = vec![
        inline_segment("A", 0),
        inline_segment(marker, 1),
        inline_segment("B", 1 + marker.len()),
    ];
    let chunks = word_wrap_line(&line, 10, Some(&segments));
    for chunk in &chunks {
        assert!(visible_width(&chunk.text) <= 10, "chunk {:?}", chunk.text);
    }
    let reconstructed: String = chunks
        .iter()
        .map(|chunk| utf16_slice(&line, chunk.start_index, chunk.end_index))
        .collect();
    assert_eq!(reconstructed, line);
}

#[test]
fn word_wrap_splits_oversized_atomic_segment_at_start_of_line() {
    let marker = "[paste #1 +20 lines]";
    let line = format!("{marker}B");
    let segments = vec![inline_segment(marker, 0), inline_segment("B", marker.len())];
    let chunks = word_wrap_line(&line, 10, Some(&segments));
    for chunk in &chunks {
        assert!(visible_width(&chunk.text) <= 10);
    }
    assert!(chunks.last().unwrap().text.contains('B'));
    let reconstructed: String = chunks
        .iter()
        .map(|chunk| utf16_slice(&line, chunk.start_index, chunk.end_index))
        .collect();
    assert_eq!(reconstructed, line);
}

#[test]
fn word_wrap_splits_oversized_atomic_segment_at_end_of_line() {
    let marker = "[paste #1 +20 lines]";
    let line = format!("A{marker}");
    let segments = vec![inline_segment("A", 0), inline_segment(marker, 1)];
    let chunks = word_wrap_line(&line, 10, Some(&segments));
    for chunk in &chunks {
        assert!(visible_width(&chunk.text) <= 10);
    }
    assert_eq!(chunks[0].text, "A");
    let reconstructed: String = chunks
        .iter()
        .map(|chunk| utf16_slice(&line, chunk.start_index, chunk.end_index))
        .collect();
    assert_eq!(reconstructed, line);
}

#[test]
fn word_wrap_splits_consecutive_oversized_atomic_segments() {
    let m1 = "[paste #1 +20 lines]";
    let m2 = "[paste #2 +30 lines]";
    let line = format!("{m1}{m2}");
    let segments = vec![inline_segment(m1, 0), inline_segment(m2, m1.len())];
    let chunks = word_wrap_line(&line, 10, Some(&segments));
    for chunk in &chunks {
        assert!(visible_width(&chunk.text) <= 10, "chunk {:?}", chunk.text);
    }
    let reconstructed: String = chunks
        .iter()
        .map(|chunk| utf16_slice(&line, chunk.start_index, chunk.end_index))
        .collect();
    assert_eq!(reconstructed, line);
}

#[test]
fn word_wrap_wraps_normally_after_oversized_atomic_segment() {
    let marker = "[paste #1 +20 lines]";
    let line = format!("{marker} hello world");
    let base = marker.len();
    let mut segments = vec![inline_segment(marker, 0), inline_segment(" ", base)];
    let tail = "hello world";
    for (i, ch) in tail.chars().enumerate() {
        segments.push(inline_segment(&ch.to_string(), base + 1 + i));
    }
    let chunks = word_wrap_line(&line, 10, Some(&segments));
    for chunk in &chunks {
        assert!(visible_width(&chunk.text) <= 10, "chunk {:?}", chunk.text);
    }
    assert_eq!(chunks.last().unwrap().text, "world");
    let reconstructed: String = chunks
        .iter()
        .map(|chunk| utf16_slice(&line, chunk.start_index, chunk.end_index))
        .collect();
    assert_eq!(reconstructed, line);
}

// ===========================================================================
// Kill ring（20）
// ===========================================================================

#[test]
fn kill_ring_ctrl_w_saves_deleted_text_and_ctrl_y_yanks_it() {
    let mut h = Harness::new();
    h.editor.set_text("foo bar baz");
    h.input("\x17");
    assert_eq!(h.text(), "foo bar ");
    h.input("\x01");
    h.input("\x19");
    assert_eq!(h.text(), "bazfoo bar ");
}

#[test]
fn kill_ring_ctrl_u_saves_deleted_text() {
    let mut h = Harness::new();
    h.editor.set_text("hello world");
    h.input("\x01");
    for _ in 0..6 {
        h.input("\x1b[C");
    }
    h.input("\x15");
    assert_eq!(h.text(), "world");
    h.input("\x19");
    assert_eq!(h.text(), "hello world");
}

#[test]
fn kill_ring_ctrl_k_saves_deleted_text() {
    let mut h = Harness::new();
    h.editor.set_text("hello world");
    h.input("\x01");
    h.input("\x0b");
    assert_eq!(h.text(), "");
    h.input("\x19");
    assert_eq!(h.text(), "hello world");
}

#[test]
fn kill_ring_ctrl_y_does_nothing_when_ring_is_empty() {
    let mut h = Harness::new();
    h.editor.set_text("test");
    h.input("\x19");
    assert_eq!(h.text(), "test");
}

#[test]
fn kill_ring_alt_y_cycles_through_ring_after_ctrl_y() {
    let mut h = Harness::new();
    h.editor.set_text("first");
    h.input("\x17");
    h.editor.set_text("second");
    h.input("\x17");
    h.editor.set_text("third");
    h.input("\x17");
    assert_eq!(h.text(), "");

    h.input("\x19");
    assert_eq!(h.text(), "third");
    h.input("\x1by");
    assert_eq!(h.text(), "second");
    h.input("\x1by");
    assert_eq!(h.text(), "first");
    h.input("\x1by");
    assert_eq!(h.text(), "third");
}

#[test]
fn kill_ring_alt_y_does_nothing_if_not_preceded_by_yank() {
    let mut h = Harness::new();
    h.editor.set_text("test");
    h.input("\x17");
    h.editor.set_text("other");
    h.input("x");
    assert_eq!(h.text(), "otherx");
    h.input("\x1by");
    assert_eq!(h.text(), "otherx");
}

#[test]
fn kill_ring_alt_y_does_nothing_if_ring_has_at_most_one_entry() {
    let mut h = Harness::new();
    h.editor.set_text("only");
    h.input("\x17");
    h.input("\x19");
    assert_eq!(h.text(), "only");
    h.input("\x1by");
    assert_eq!(h.text(), "only");
}

#[test]
fn kill_ring_consecutive_ctrl_w_accumulates_into_one_entry() {
    let mut h = Harness::new();
    h.editor.set_text("one two three");
    h.input("\x17");
    h.input("\x17");
    h.input("\x17");
    assert_eq!(h.text(), "");
    h.input("\x19");
    assert_eq!(h.text(), "one two three");
}

#[test]
fn kill_ring_ctrl_u_accumulates_multiline_deletes_including_newlines() {
    let mut h = Harness::new();
    h.editor.set_text("line1\nline2\nline3");

    h.input("\x15");
    assert_eq!(h.text(), "line1\nline2\n");
    h.input("\x15");
    assert_eq!(h.text(), "line1\nline2");
    h.input("\x15");
    assert_eq!(h.text(), "line1\n");
    h.input("\x15");
    assert_eq!(h.text(), "line1");
    h.input("\x15");
    assert_eq!(h.text(), "");
    h.input("\x19");
    assert_eq!(h.text(), "line1\nline2\nline3");
}

#[test]
fn kill_ring_backward_deletions_prepend_forward_deletions_append_during_accumulation() {
    let mut h = Harness::new();
    h.editor.set_text("prefix|suffix");
    h.input("\x01");
    for _ in 0..6 {
        h.input("\x1b[C");
    }
    h.input("\x0b");
    h.input("\x0b");
    assert_eq!(h.text(), "prefix");
    h.input("\x19");
    assert_eq!(h.text(), "prefix|suffix");
}

#[test]
fn kill_ring_non_delete_actions_break_kill_accumulation() {
    let mut h = Harness::new();
    h.editor.set_text("foo bar baz");
    h.input("\x17");
    assert_eq!(h.text(), "foo bar ");
    h.input("x");
    assert_eq!(h.text(), "foo bar x");
    h.input("\x17");
    assert_eq!(h.text(), "foo bar ");

    h.input("\x19");
    assert_eq!(h.text(), "foo bar x");
    h.input("\x1by");
    assert_eq!(h.text(), "foo bar baz");
}

#[test]
fn kill_ring_non_yank_actions_break_alt_y_chain() {
    let mut h = Harness::new();
    h.editor.set_text("first");
    h.input("\x17");
    h.editor.set_text("second");
    h.input("\x17");
    h.editor.set_text("");

    h.input("\x19");
    assert_eq!(h.text(), "second");
    h.input("x");
    assert_eq!(h.text(), "secondx");
    h.input("\x1by");
    assert_eq!(h.text(), "secondx");
}

#[test]
fn kill_ring_rotation_persists_after_cycling() {
    let mut h = Harness::new();
    h.editor.set_text("first");
    h.input("\x17");
    h.editor.set_text("second");
    h.input("\x17");
    h.editor.set_text("third");
    h.input("\x17");
    h.editor.set_text("");

    h.input("\x19");
    h.input("\x1by");
    assert_eq!(h.text(), "second");

    h.input("x");
    h.editor.set_text("");
    h.input("\x19");
    assert_eq!(h.text(), "second");
}

#[test]
fn kill_ring_consecutive_deletions_across_lines_coalesce_into_one_entry() {
    let mut h = Harness::new();
    h.editor.set_text("1\n2\n3");
    h.input("\x17");
    assert_eq!(h.text(), "1\n2\n");
    h.input("\x17");
    assert_eq!(h.text(), "1\n2");
    h.input("\x17");
    assert_eq!(h.text(), "1\n");
    h.input("\x17");
    assert_eq!(h.text(), "1");
    h.input("\x17");
    assert_eq!(h.text(), "");
    h.input("\x19");
    assert_eq!(h.text(), "1\n2\n3");
}

#[test]
fn kill_ring_ctrl_k_at_line_end_deletes_newline_and_coalesces() {
    let mut h = Harness::new();
    h.editor.set_text("");
    h.input("a");
    h.input("b");
    h.input("\n");
    h.input("c");
    h.input("d");
    h.input("\x1b[A");
    h.input("\x05");

    h.input("\x0b");
    assert_eq!(h.text(), "abcd");
    h.input("\x0b");
    assert_eq!(h.text(), "ab");
    h.input("\x19");
    assert_eq!(h.text(), "ab\ncd");
}

#[test]
fn kill_ring_handles_yank_in_middle_of_text() {
    let mut h = Harness::new();
    h.editor.set_text("word");
    h.input("\x17");
    h.editor.set_text("hello world");
    h.input("\x01");
    for _ in 0..6 {
        h.input("\x1b[C");
    }
    h.input("\x19");
    assert_eq!(h.text(), "hello wordworld");
}

#[test]
fn kill_ring_handles_yank_pop_in_middle_of_text() {
    let mut h = Harness::new();
    h.editor.set_text("FIRST");
    h.input("\x17");
    h.editor.set_text("SECOND");
    h.input("\x17");

    h.editor.set_text("hello world");
    h.input("\x01");
    for _ in 0..6 {
        h.input("\x1b[C");
    }
    h.input("\x19");
    assert_eq!(h.text(), "hello SECONDworld");
    h.input("\x1by");
    assert_eq!(h.text(), "hello FIRSTworld");
}

#[test]
fn kill_ring_multiline_yank_and_yank_pop_in_middle_of_text() {
    let mut h = Harness::new();
    h.editor.set_text("SINGLE");
    h.input("\x17");
    h.editor.set_text("A\nB");
    h.input("\x15");
    h.input("\x15");
    h.input("\x15");

    h.editor.set_text("hello world");
    h.input("\x01");
    for _ in 0..6 {
        h.input("\x1b[C");
    }
    h.input("\x19");
    assert_eq!(h.text(), "hello A\nBworld");
    h.input("\x1by");
    assert_eq!(h.text(), "hello SINGLEworld");
}

#[test]
fn kill_ring_alt_d_deletes_word_forward_and_saves_to_ring() {
    let mut h = Harness::new();
    h.editor.set_text("hello world test");
    h.input("\x01");
    h.input("\x1bd");
    assert_eq!(h.text(), " world test");
    h.input("\x1bd");
    assert_eq!(h.text(), " test");
    h.input("\x19");
    assert_eq!(h.text(), "hello world test");
}

#[test]
fn kill_ring_alt_d_at_end_of_line_deletes_newline() {
    let mut h = Harness::new();
    h.editor.set_text("line1\nline2");
    h.input("\x1b[A");
    h.input("\x05");
    h.input("\x1bd");
    assert_eq!(h.text(), "line1line2");
    h.input("\x19");
    assert_eq!(h.text(), "line1\nline2");
}

// ===========================================================================
// Undo（24）
// ===========================================================================

#[test]
fn undo_does_nothing_when_undo_stack_is_empty() {
    let mut h = Harness::new();
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "");
}

#[test]
fn undo_coalesces_consecutive_word_characters_into_one_undo_unit() {
    let mut h = Harness::new();
    for ch in ["h", "e", "l", "l", "o", " ", "w", "o", "r", "l", "d"] {
        h.input(ch);
    }
    assert_eq!(h.text(), "hello world");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "hello");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "");
}

#[test]
fn undo_undoes_spaces_one_at_a_time() {
    let mut h = Harness::new();
    for ch in ["h", "e", "l", "l", "o", " ", " "] {
        h.input(ch);
    }
    assert_eq!(h.text(), "hello  ");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "hello ");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "hello");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "");
}

#[test]
fn undo_undoes_newlines_and_signals_next_word_to_capture_state() {
    let mut h = Harness::new();
    for ch in ["h", "e", "l", "l", "o", "\n", "w", "o", "r", "l", "d"] {
        h.input(ch);
    }
    assert_eq!(h.text(), "hello\nworld");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "hello\n");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "hello");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "");
}

#[test]
fn undo_undoes_backspace() {
    let mut h = Harness::new();
    for ch in ["h", "e", "l", "l", "o"] {
        h.input(ch);
    }
    h.input("\x7f");
    assert_eq!(h.text(), "hell");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "hello");
}

#[test]
fn undo_undoes_forward_delete() {
    let mut h = Harness::new();
    for ch in ["h", "e", "l", "l", "o"] {
        h.input(ch);
    }
    h.input("\x01");
    h.input("\x1b[C");
    h.input("\x1b[3~");
    assert_eq!(h.text(), "hllo");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "hello");
}

#[test]
fn undo_undoes_ctrl_w_delete_word_backward() {
    let mut h = Harness::new();
    for ch in ["h", "e", "l", "l", "o", " ", "w", "o", "r", "l", "d"] {
        h.input(ch);
    }
    h.input("\x17");
    assert_eq!(h.text(), "hello ");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "hello world");
}

#[test]
fn undo_undoes_ctrl_k_delete_to_line_end() {
    let mut h = Harness::new();
    for ch in ["h", "e", "l", "l", "o", " ", "w", "o", "r", "l", "d"] {
        h.input(ch);
    }
    h.input("\x01");
    for _ in 0..6 {
        h.input("\x1b[C");
    }
    h.input("\x0b");
    assert_eq!(h.text(), "hello ");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "hello world");
    h.input("|");
    assert_eq!(h.text(), "hello |world");
}

#[test]
fn undo_undoes_ctrl_u_delete_to_line_start() {
    let mut h = Harness::new();
    for ch in ["h", "e", "l", "l", "o", " ", "w", "o", "r", "l", "d"] {
        h.input(ch);
    }
    h.input("\x01");
    for _ in 0..6 {
        h.input("\x1b[C");
    }
    h.input("\x15");
    assert_eq!(h.text(), "world");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "hello world");
}

#[test]
fn undo_undoes_yank() {
    let mut h = Harness::new();
    for ch in ["h", "e", "l", "l", "o", " "] {
        h.input(ch);
    }
    h.input("\x17");
    h.input("\x19");
    assert_eq!(h.text(), "hello ");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "");
}

#[test]
fn undo_undoes_single_line_paste_atomically() {
    let mut h = Harness::new();
    h.editor.set_text("hello world");
    h.input("\x01");
    for _ in 0..5 {
        h.input("\x1b[C");
    }
    h.input("\x1b[200~beep boop\x1b[201~");
    assert_eq!(h.text(), "hellobeep boop world");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "hello world");
    h.input("|");
    assert_eq!(h.text(), "hello| world");
}

#[test]
fn undo_does_not_trigger_autocomplete_during_single_line_paste() {
    let mut h = Harness::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&calls);
    h.editor
        .set_autocomplete_provider(Box::new(MockProvider::new(
            move |_lines, _line, _col, _options| {
                sink.fetch_add(1, Ordering::SeqCst);
                None
            },
        )));

    h.input("\x1b[200~look at @node_modules/react/index.js please\x1b[201~");
    assert_eq!(h.text(), "look at @node_modules/react/index.js please");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!h.editor.is_showing_autocomplete());
}

#[test]
fn undo_decodes_csi_u_ctrl_letter_sequences_inside_bracketed_paste() {
    let mut h = Harness::new();
    h.input("\x1b[200~line1\x1b[106;5uline2\x1b[106;5uline3\x1b[201~");
    assert_eq!(h.text(), "line1\nline2\nline3");
}

#[test]
fn undo_undoes_multi_line_paste_atomically() {
    let mut h = Harness::new();
    h.editor.set_text("hello world");
    h.input("\x01");
    for _ in 0..5 {
        h.input("\x1b[C");
    }
    h.input("\x1b[200~line1\nline2\nline3\x1b[201~");
    assert_eq!(h.text(), "helloline1\nline2\nline3 world");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "hello world");
    h.input("|");
    assert_eq!(h.text(), "hello| world");
}

#[test]
fn undo_undoes_insert_text_at_cursor_atomically() {
    let mut h = Harness::new();
    h.editor.set_text("hello world");
    h.input("\x01");
    for _ in 0..5 {
        h.input("\x1b[C");
    }
    h.editor.insert_text_at_cursor("/tmp/image.png");
    assert_eq!(h.text(), "hello/tmp/image.png world");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "hello world");
    h.input("|");
    assert_eq!(h.text(), "hello| world");
}

#[test]
fn undo_insert_text_at_cursor_handles_multiline_text() {
    let mut h = Harness::new();
    h.editor.set_text("hello world");
    h.input("\x01");
    for _ in 0..5 {
        h.input("\x1b[C");
    }
    h.editor.insert_text_at_cursor("line1\nline2\nline3");
    assert_eq!(h.text(), "helloline1\nline2\nline3 world");
    let (line, col) = h.cursor();
    assert_eq!(line, 2);
    assert_eq!(col, 5);
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "hello world");
}

#[test]
fn undo_insert_text_at_cursor_normalizes_crlf_and_cr_line_endings() {
    let mut h = Harness::new();
    h.editor.set_text("");
    h.editor.insert_text_at_cursor("a\r\nb\r\nc");
    assert_eq!(h.text(), "a\nb\nc");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "");
    h.editor.insert_text_at_cursor("x\ry\rz");
    assert_eq!(h.text(), "x\ny\nz");
}

#[test]
fn undo_undoes_set_text_to_empty_string() {
    let mut h = Harness::new();
    for ch in ["h", "e", "l", "l", "o", " ", "w", "o", "r", "l", "d"] {
        h.input(ch);
    }
    assert_eq!(h.text(), "hello world");
    h.editor.set_text("");
    assert_eq!(h.text(), "");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "hello world");
}

#[test]
fn undo_clears_undo_stack_on_submit() {
    let mut h = Harness::new();
    let submitted = Rc::new(RefCell::new(String::new()));
    let sink = Rc::clone(&submitted);
    h.editor.on_submit = Some(Box::new(move |text: &str| {
        *sink.borrow_mut() = text.to_string();
    }));

    for ch in ["h", "e", "l", "l", "o"] {
        h.input(ch);
    }
    h.input("\r");
    assert_eq!(*submitted.borrow(), "hello");
    assert_eq!(h.text(), "");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "");
}

#[test]
fn undo_exits_history_browsing_mode_on_undo() {
    let mut h = Harness::new();
    h.editor.add_to_history("hello");
    assert_eq!(h.text(), "");
    for ch in ["w", "o", "r", "l", "d"] {
        h.input(ch);
    }
    assert_eq!(h.text(), "world");
    h.input("\x17");
    assert_eq!(h.text(), "");
    h.input("\x1b[A");
    assert_eq!(h.text(), "hello");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "world");
}

#[test]
fn undo_restores_to_pre_history_state_even_after_multiple_history_navigations() {
    let mut h = Harness::new();
    h.editor.add_to_history("first");
    h.editor.add_to_history("second");
    h.editor.add_to_history("third");
    for ch in ["c", "u", "r", "r", "e", "n", "t"] {
        h.input(ch);
    }
    assert_eq!(h.text(), "current");
    h.input("\x17");
    assert_eq!(h.text(), "");
    h.input("\x1b[A");
    assert_eq!(h.text(), "third");
    h.input("\x1b[A");
    assert_eq!(h.text(), "second");
    h.input("\x1b[A");
    assert_eq!(h.text(), "first");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "current");
}

#[test]
fn undo_cursor_movement_starts_new_undo_unit() {
    let mut h = Harness::new();
    for ch in ["h", "e", "l", "l", "o", " ", "w", "o", "r", "l", "d"] {
        h.input(ch);
    }
    for _ in 0..5 {
        h.input("\x1b[D");
    }
    for ch in ["l", "o", "l"] {
        h.input(ch);
    }
    assert_eq!(h.text(), "hello lolworld");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "hello world");
    h.input("|");
    assert_eq!(h.text(), "hello |world");
}

#[test]
fn undo_no_op_delete_operations_do_not_push_undo_snapshots() {
    let mut h = Harness::new();
    for ch in ["h", "e", "l", "l", "o"] {
        h.input(ch);
    }
    assert_eq!(h.text(), "hello");
    h.input("\x17");
    assert_eq!(h.text(), "");
    h.input("\x17");
    h.input("\x17");
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "hello");
}

#[test]
fn undo_undoes_autocomplete() {
    let mut h = Harness::new();
    h.editor
        .set_autocomplete_provider(Box::new(MockProvider::new(
            |lines, _line, col, _options| {
                let text = lines.first().cloned().unwrap_or_default();
                let prefix = utf16_slice(&text, 0, col);
                if prefix == "di" {
                    Some(suggestions(vec![item("dist/")], "di"))
                } else {
                    None
                }
            },
        )));

    h.input("d");
    h.input("i");
    assert_eq!(h.text(), "di");
    h.input("\t");
    flush(&mut h.editor);
    assert_eq!(h.text(), "dist/");
    assert!(!h.editor.is_showing_autocomplete());
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "di");
}

// ===========================================================================
// Autocomplete（24）
// ===========================================================================

/// `CombinedAutocompleteProvider` 的测试命令/路径依赖。
use kiana_tui::autocomplete::{
    ArgumentCompletionsFuture, CombinedAutocompleteProvider, CommandEntry, SlashCommand,
};

/// 临时目录（`mkdtempSync` 的等价物）；`Drop` 时递归删除。
struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        use std::sync::atomic::AtomicU64;
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = format!(
            "{prefix}{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }

    fn str(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn with_fake_clock(harness: &mut Harness) -> FakeClock {
    let clock = FakeClock::new();
    harness.editor.set_clock(clock.as_fn());
    clock
}

#[test]
fn autocomplete_triggers_and_debounces_symbol_completion_after_cjk_punctuation() {
    let befores = vec![
        "查看，".to_string(),
        "\u{3000}".to_string(),
        "，．：；！？（）［］｛｝“”‘’…—。、「」『』《》【】".to_string(),
        "(".to_string(),
        "see (".to_string(),
        "`".to_string(),
        "[".to_string(),
    ];
    let triggers = ["@", "#", "$", "-"];
    for before in befores {
        for trigger in triggers {
            let mut h = Harness::new();
            let clock = with_fake_clock(&mut h);
            let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let sink = Arc::clone(&requests);
            h.editor.set_autocomplete_provider(Box::new(
                MockProvider::new(move |lines, line, col, _options| {
                    let text = lines.get(line).cloned().unwrap_or_default();
                    sink.lock().unwrap().push(utf16_slice(&text, 0, col));
                    None
                })
                .with_triggers(vec!['$', '-']),
            ));
            h.editor.set_text(&before);
            h.input(trigger);
            clock.advance(19.0);
            flush(&mut h.editor);
            assert!(
                requests.lock().unwrap().is_empty(),
                "no request before 20ms for {before:?} + {trigger:?}"
            );
            clock.advance(1.0);
            flush(&mut h.editor);
            assert_eq!(
                *requests.lock().unwrap(),
                vec![format!("{before}{trigger}")]
            );

            h.input("r");
            h.input("e");
            clock.advance(19.0);
            flush(&mut h.editor);
            assert_eq!(requests.lock().unwrap().len(), 1);
            clock.advance(1.0);
            flush(&mut h.editor);
            assert_eq!(
                *requests.lock().unwrap(),
                vec![format!("{before}{trigger}"), format!("{before}{trigger}re")]
            );
        }
    }
}

#[test]
fn autocomplete_does_not_auto_trigger_after_cjk_letters_or_for_unprefixed_paths() {
    let mut h = Harness::new();
    let clock = with_fake_clock(&mut h);
    let requests = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&requests);
    h.editor
        .set_autocomplete_provider(Box::new(MockProvider::new(
            move |_lines, _line, _col, _options| {
                sink.fetch_add(1, Ordering::SeqCst);
                None
            },
        )));

    let texts = [
        "user@example.com",
        "张三@example.com",
        "查看@src",
        "あ@src",
        "カ@src",
        "한@src",
        "ㄅ@src",
        "𠮷@src",
        "か\u{3099}@src",
        "禰\u{e0100}@src",
        "々@src",
        "Ａ@src",
        "文档@备份",
        "prefix#123",
        "foo(@src",
        "问题#123",
        "查看，/path/",
        "查看，./文档/",
        "src/index.ts",
        "./文档/说明.md",
        "文档/说明.md",
        "查看src/index.ts",
    ];
    for text in texts {
        h.editor.set_text("");
        for ch in text.chars() {
            h.input(&ch.to_string());
        }
        clock.advance(20.0);
        flush(&mut h.editor);
        assert_eq!(requests.load(Ordering::SeqCst), 0, "{text}");
    }
}

#[test]
fn autocomplete_requests_path_completion_after_cjk_punctuation_only_on_tab() {
    let mut h = Harness::new();
    let clock = with_fake_clock(&mut h);
    let requests: Arc<Mutex<Vec<(String, bool)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&requests);
    h.editor
        .set_autocomplete_provider(Box::new(MockProvider::new(
            move |lines, line, col, options| {
                let text = lines.get(line).cloned().unwrap_or_default();
                sink.lock()
                    .unwrap()
                    .push((utf16_slice(&text, 0, col), options.force));
                None
            },
        )));

    let text = "查看，/path/";
    for ch in text.chars() {
        h.input(&ch.to_string());
    }
    clock.advance(20.0);
    flush(&mut h.editor);
    assert!(requests.lock().unwrap().is_empty());

    h.input("\t");
    flush(&mut h.editor);
    assert_eq!(*requests.lock().unwrap(), vec![(text.to_string(), true)]);
}

#[test]
fn autocomplete_completes_chinese_path_prefixes_after_whitespace_or_cjk_punctuation_with_tab() {
    let base_dir = TempDir::new("kiana-editor-autocomplete-");
    std::fs::create_dir_all(base_dir.path.join("文档")).unwrap();
    std::fs::write(base_dir.path.join("文档").join("说明.md"), "text").unwrap();

    let mut h = Harness::new();
    h.editor
        .set_autocomplete_provider(Box::new(CombinedAutocompleteProvider::new(
            Vec::new(),
            base_dir.str(),
            None,
        )));

    for separator in [" ", "\t", "\u{3000}", "\u{00a0}", "，", "。"] {
        h.editor.set_text(&format!("查看{separator}"));
        let before = h.text();
        h.input("文");
        h.input("\t");
        flush(&mut h.editor);
        assert_eq!(h.text(), format!("{before}文档/"));
        h.input("说");
        h.input("\t");
        flush(&mut h.editor);
        assert_eq!(h.text(), format!("{before}文档/说明.md"));
        assert_eq!(h.cursor(), (0, utf16_len(&h.text())));
    }
}

#[test]
fn autocomplete_ends_unquoted_trigger_and_debounce_contexts_at_whitespace_or_cjk_punctuation() {
    for separator in [" ", "\u{3000}", "，", "。"] {
        for trigger in ["@", "#", "$"] {
            let mut h = Harness::new();
            let clock = with_fake_clock(&mut h);
            let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let prefix = format!("{trigger}src");
            let sink = Arc::clone(&requests);
            let expected_prefix = prefix.clone();
            h.editor.set_autocomplete_provider(Box::new(
                MockProvider::new(move |lines, line, col, _options| {
                    let text = lines.get(line).cloned().unwrap_or_default();
                    let before = utf16_slice(&text, 0, col);
                    sink.lock().unwrap().push(before.clone());
                    if before == expected_prefix {
                        Some(suggestions(
                            vec![item(&format!("{expected_prefix}/"))],
                            &expected_prefix,
                        ))
                    } else {
                        None
                    }
                })
                .with_triggers(vec!['$']),
            ));

            h.editor.set_text(&format!("{trigger}sr"));
            h.input("c");
            clock.advance(20.0);
            flush(&mut h.editor);
            assert!(
                h.editor.is_showing_autocomplete(),
                "{trigger} {separator:?}"
            );

            h.input(separator);
            flush(&mut h.editor);
            assert_eq!(
                *requests.lock().unwrap(),
                vec![prefix.clone(), format!("{prefix}{separator}")]
            );
            assert!(!h.editor.is_showing_autocomplete());

            h.input("文");
            clock.advance(20.0);
            flush(&mut h.editor);
            assert_eq!(
                *requests.lock().unwrap(),
                vec![prefix.clone(), format!("{prefix}{separator}")]
            );
        }
    }
}

#[test]
fn autocomplete_re_triggers_cjk_path_completion_after_accepting_directories_and_deleting() {
    for directory in ["文档", "我的 文档", "资料，归档"] {
        let quoted = directory != "文档";
        let initial = if quoted {
            format!("@\"{}", utf16_slice(directory, 0, 2))
        } else {
            "@文".to_string()
        };
        let directory_value = if quoted {
            format!("@\"{directory}/\"")
        } else {
            format!("@{directory}/")
        };
        let file_value = if quoted {
            format!("@\"{directory}/说明.md\"")
        } else {
            format!("@{directory}/说明.md")
        };
        let file_prefix = if quoted {
            format!("@\"{directory}/说")
        } else {
            format!("@{directory}/说")
        };

        let mut h = Harness::new();
        // 用自定义 provider 包装 CombinedAutocompleteProvider 的 applyCompletion。
        let inner = Arc::new(CombinedAutocompleteProvider::new(
            Vec::new(),
            std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            None,
        ));
        let initial_for_handler = initial.clone();
        let dir_value_for_handler = directory_value.clone();
        let file_prefix_for_handler = file_prefix.clone();
        let file_value_for_handler = file_value.clone();
        let directory_for_handler = directory.to_string();
        h.editor
            .set_autocomplete_provider(Box::new(DelegateProvider {
                inner: Arc::clone(&inner),
                handler: Box::new(
                    move |lines: &[String], line: usize, col: usize, _force: bool| {
                        let before = utf16_slice(lines.get(line).unwrap_or(&String::new()), 0, col);
                        let at = before.find('@')?;
                        let prefix = utf16_suffix_str(&before, utf16_len(&before[..at]));
                        if prefix == initial_for_handler {
                            Some(suggestions(
                                vec![AutocompleteItem {
                                    value: dir_value_for_handler.clone(),
                                    label: format!("{directory_for_handler}/"),
                                    description: None,
                                }],
                                &prefix,
                            ))
                        } else if prefix == file_prefix_for_handler {
                            Some(suggestions(
                                vec![AutocompleteItem {
                                    value: file_value_for_handler.clone(),
                                    label: "说明.md".to_string(),
                                    description: None,
                                }],
                                &prefix,
                            ))
                        } else {
                            None
                        }
                    },
                ),
            }));

        h.editor.set_text(&format!("查看：{initial}"));
        h.input("\t");
        flush(&mut h.editor);
        assert_eq!(h.text(), format!("查看：{directory_value}"));
        assert!(!h.editor.is_showing_autocomplete());

        h.input("说");
        // 无防抖等待时先推进假时钟再 flush（该场景走 20ms 防抖）。
        h.editor.tick();
        // 触发防抖要求时钟前进；使用真实 deadline 检查：推进到 deadline。
        if let Some(deadline) = h.editor.autocomplete_deadline_ms() {
            // 直接跳到截止时间。
            for _ in 0..1 {
                let _ = deadline;
            }
        }
        // 该 provider 的触发走 20ms 防抖；测试用真实时钟推进不可行，改为显式多轮 tick
        // 由下面的 `advance_deadline` 辅助完成。
        advance_past_deadline(&mut h.editor);
        flush(&mut h.editor);
        assert!(
            h.editor.is_showing_autocomplete(),
            "directory {directory:?}"
        );

        for deletion in ["\x7f", "\x1b[3~"] {
            h.input("错");
            advance_past_deadline(&mut h.editor);
            flush(&mut h.editor);
            assert!(
                !h.editor.is_showing_autocomplete(),
                "delete test {directory:?} {deletion:?}"
            );
            if deletion == "\x1b[3~" {
                h.input("\x1b[D");
            }
            h.input(deletion);
            advance_past_deadline(&mut h.editor);
            flush(&mut h.editor);
            assert!(
                h.editor.is_showing_autocomplete(),
                "delete test {directory:?} {deletion:?}"
            );
        }

        h.input("\t");
        assert_eq!(h.text(), format!("查看：{file_value} "));
        assert_eq!(h.cursor(), (0, utf16_len(&h.text())));
    }
}

/// 把真实单调时钟推进到当前防抖截止时间之后（测试里没有假时钟时用 sleep 等待）。
fn advance_past_deadline(editor: &mut Editor) {
    loop {
        editor.tick();
        let Some(deadline) = editor.autocomplete_deadline_ms() else {
            return;
        };
        let now = editor.now_ms();
        if now >= deadline {
            return;
        }
        let wait = (deadline - now).max(1.0);
        std::thread::sleep(std::time::Duration::from_millis(wait.ceil() as u64));
    }
}

/// `before.slice(index)`（UTF-16 语义）。
fn utf16_suffix_str(text: &str, start: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    String::from_utf16_lossy(&units[start.min(units.len())..])
}

/// 包装 provider：getSuggestions 由 handler 决定，applyCompletion 委托给真实 provider。
struct DelegateProvider {
    inner: Arc<CombinedAutocompleteProvider>,
    #[allow(clippy::type_complexity)]
    handler: Box<dyn Fn(&[String], usize, usize, bool) -> Option<AutocompleteSuggestions> + Send>,
}

impl AutocompleteProvider for DelegateProvider {
    fn get_suggestions<'a>(
        &'a mut self,
        lines: &'a [String],
        cursor_line: usize,
        cursor_col: usize,
        options: &'a GetSuggestionsOptions,
    ) -> Pin<Box<dyn Future<Output = Option<AutocompleteSuggestions>> + Send + 'a>> {
        let result = (self.handler)(lines, cursor_line, cursor_col, options.force);
        Box::pin(std::future::ready(result))
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> ApplyCompletionResult {
        self.inner
            .apply_completion(lines, cursor_line, cursor_col, item, prefix)
    }
}

#[test]
fn autocomplete_auto_applies_single_force_file_suggestion_without_showing_menu() {
    let mut h = Harness::new();
    h.editor
        .set_autocomplete_provider(Box::new(MockProvider::new(|lines, _line, col, options| {
            if !options.force {
                return None;
            }
            let text = lines.first().cloned().unwrap_or_default();
            let prefix = utf16_slice(&text, 0, col);
            if prefix == "Work" {
                Some(suggestions(vec![item("Workspace/")], "Work"))
            } else {
                None
            }
        })));

    for ch in ["W", "o", "r", "k"] {
        h.input(ch);
    }
    assert_eq!(h.text(), "Work");
    h.input("\t");
    flush(&mut h.editor);
    assert_eq!(h.text(), "Workspace/");
    assert!(!h.editor.is_showing_autocomplete());
    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "Work");
}

#[test]
fn autocomplete_shows_menu_when_force_file_has_multiple_suggestions() {
    let mut h = Harness::new();
    h.editor
        .set_autocomplete_provider(Box::new(MockProvider::new(|lines, _line, col, options| {
            if !options.force {
                return None;
            }
            let text = lines.first().cloned().unwrap_or_default();
            let prefix = utf16_slice(&text, 0, col);
            if prefix == "src" {
                Some(suggestions(vec![item("src/"), item("src.txt")], "src"))
            } else {
                None
            }
        })));

    for ch in ["s", "r", "c"] {
        h.input(ch);
    }
    assert_eq!(h.text(), "src");
    h.input("\t");
    flush(&mut h.editor);
    assert_eq!(h.text(), "src");
    assert!(h.editor.is_showing_autocomplete());

    h.input("\t");
    assert_eq!(h.text(), "src/");
    assert!(!h.editor.is_showing_autocomplete());
}

#[test]
fn autocomplete_keeps_suggestions_open_when_typing_in_force_mode() {
    let mut h = Harness::new();
    let all_files = ["readme.md", "package.json", "src/", "dist/"];
    h.editor
        .set_autocomplete_provider(Box::new(MockProvider::new(
            move |lines, _line, col, options| {
                let text = lines.first().cloned().unwrap_or_default();
                let prefix = utf16_slice(&text, 0, col);
                let should_match = options.force || prefix.contains('/') || prefix.starts_with('.');
                if !should_match {
                    return None;
                }
                let filtered: Vec<AutocompleteItem> = all_files
                    .iter()
                    .filter(|file| file.to_lowercase().starts_with(&prefix.to_lowercase()))
                    .map(|file| item(file))
                    .collect();
                if filtered.is_empty() {
                    None
                } else {
                    Some(suggestions(filtered, &prefix))
                }
            },
        )));

    h.input("\t");
    flush(&mut h.editor);
    assert!(h.editor.is_showing_autocomplete());

    h.input("r");
    flush(&mut h.editor);
    assert_eq!(h.text(), "r");
    assert!(h.editor.is_showing_autocomplete());

    h.input("e");
    flush(&mut h.editor);
    assert_eq!(h.text(), "re");
    assert!(h.editor.is_showing_autocomplete());

    h.input("\t");
    assert_eq!(h.text(), "readme.md");
    assert!(!h.editor.is_showing_autocomplete());
}

#[test]
fn autocomplete_debounces_at_autocomplete_while_typing() {
    let mut h = Harness::new();
    let clock = with_fake_clock(&mut h);
    let calls = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&calls);
    h.editor
        .set_autocomplete_provider(Box::new(MockProvider::new(
            move |lines, _line, col, _options| {
                sink.fetch_add(1, Ordering::SeqCst);
                let text = lines.first().cloned().unwrap_or_default();
                let prefix = utf16_slice(&text, 0, col);
                Some(suggestions(vec![item("@main.ts")], &prefix))
            },
        )));

    for ch in ["@", "m", "a", "i"] {
        h.input(ch);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!h.editor.is_showing_autocomplete());

    clock.advance(50.0);
    flush(&mut h.editor);

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(h.editor.is_showing_autocomplete());
}

#[test]
fn autocomplete_re_queries_the_picker_when_the_cursor_moves_back_into_the_command_name() {
    let mut h = Harness::new();
    h.editor
        .set_autocomplete_provider(Box::new(MockProvider::new(
            |lines, _line, col, _options| {
                let before = utf16_slice(lines.first().map(String::as_str).unwrap_or(""), 0, col);
                if !before.starts_with('/') {
                    return None;
                }
                if let Some(space) = before.find(' ') {
                    return Some(suggestions(
                        vec![item("repo"), item("message"), item("help")],
                        &utf16_suffix_str(&before, space + 1),
                    ));
                }
                Some(suggestions(vec![item("cmd")], &before))
            },
        )));

    for ch in "/cmd ".chars() {
        h.input(&ch.to_string());
        flush(&mut h.editor);
    }
    assert_eq!(h.text(), "/cmd ");
    assert!(h.editor.is_showing_autocomplete());
    let at_arg: String = h
        .render(80)
        .iter()
        .map(|line| strip(line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        at_arg.contains("repo"),
        "argument menu should be visible at `/cmd `"
    );

    h.input("\x1b[D");
    flush(&mut h.editor);

    let after_move: String = h
        .render(80)
        .iter()
        .map(|line| strip(line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !after_move.contains("repo"),
        "stale argument menu must not survive the cursor move"
    );
    assert!(
        !after_move.contains("message"),
        "stale argument menu must not survive the cursor move"
    );
}

#[test]
fn autocomplete_debounces_hash_autocomplete_while_typing() {
    let mut h = Harness::new();
    let clock = with_fake_clock(&mut h);
    let calls = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&calls);
    h.editor
        .set_autocomplete_provider(Box::new(MockProvider::new(
            move |lines, _line, col, _options| {
                sink.fetch_add(1, Ordering::SeqCst);
                let text = lines.first().cloned().unwrap_or_default();
                Some(suggestions(
                    vec![item("#2983")],
                    &utf16_slice(&text, 0, col),
                ))
            },
        )));

    for ch in ["#", "2", "9", "8"] {
        h.input(ch);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!h.editor.is_showing_autocomplete());

    clock.advance(50.0);
    flush(&mut h.editor);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(h.editor.is_showing_autocomplete());
}

#[test]
fn autocomplete_debounces_custom_trigger_characters_while_typing() {
    let mut h = Harness::new();
    let clock = with_fake_clock(&mut h);
    let calls = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&calls);
    h.editor.set_autocomplete_provider(Box::new(
        MockProvider::new(move |lines, _line, col, _options| {
            sink.fetch_add(1, Ordering::SeqCst);
            let prefix = utf16_slice(lines.first().map(String::as_str).unwrap_or(""), 0, col);
            Some(suggestions(vec![item("$skill-name")], &prefix))
        })
        .with_triggers(vec!['$']),
    ));

    h.input("$");
    h.input("s");
    h.input("k");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    clock.advance(50.0);
    flush(&mut h.editor);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(h.editor.is_showing_autocomplete());
}

/// TS 的 `autocompleteTokenStartSource` 里 `[([{<`]*` 是贪婪量词，正则引擎会把括号
/// 回溯交给触发字符分支；自定义触发字符本身是括号/反引号时必须同样命中。
#[test]
fn autocomplete_matches_bracket_trigger_characters_after_a_separator() {
    for trigger in ['(', '[', '{', '<', '`'] {
        let mut h = Harness::new();
        let clock = with_fake_clock(&mut h);
        let calls = Arc::new(AtomicUsize::new(0));
        let sink = Arc::clone(&calls);
        h.editor.set_autocomplete_provider(Box::new(
            MockProvider::new(move |lines, _line, col, _options| {
                sink.fetch_add(1, Ordering::SeqCst);
                let prefix = utf16_slice(lines.first().map(String::as_str).unwrap_or(""), 0, col);
                Some(suggestions(vec![item("match")], &prefix))
            })
            .with_triggers(vec![trigger]),
        ));

        // `x(`：前面没有分隔符，TS 也不触发。
        h.input("x");
        h.input(&trigger.to_string());
        clock.advance(50.0);
        flush(&mut h.editor);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "trigger {trigger:?} must not match without a separator"
        );
        assert!(!h.editor.is_showing_autocomplete());

        // `x，(`：CJK 标点分隔符之后，TS 触发（20ms 防抖）。
        h.input("\u{FF0C}");
        h.input(&trigger.to_string());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        clock.advance(20.0);
        flush(&mut h.editor);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "trigger {trigger:?} must match after a separator"
        );
        assert!(h.editor.is_showing_autocomplete());
    }
}

/// 回归（审阅者复现）：provider 的 triggerCharacters 包含 `(` 等括号字符时，在空编辑器里
/// 直接输入该字符，TS 的贪婪 `[([{<`]*` 会回溯把它让给触发字符分支，20ms 后调用 provider。
#[test]
fn autocomplete_matches_bracket_trigger_character_at_start_of_line() {
    for trigger in ['(', '[', '{', '<', '`'] {
        let mut h = Harness::new();
        let clock = with_fake_clock(&mut h);
        let calls = Arc::new(AtomicUsize::new(0));
        let sink = Arc::clone(&calls);
        h.editor.set_autocomplete_provider(Box::new(
            MockProvider::new(move |_lines, _line, _col, _options| {
                sink.fetch_add(1, Ordering::SeqCst);
                Some(suggestions(vec![item("match")], ""))
            })
            .with_triggers(vec![trigger]),
        ));

        h.input(&trigger.to_string());
        assert_eq!(calls.load(Ordering::SeqCst), 0, "trigger {trigger:?}");
        clock.advance(20.0);
        flush(&mut h.editor);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "trigger {trigger:?}");
        assert!(h.editor.is_showing_autocomplete(), "trigger {trigger:?}");
    }
}

#[test]
fn autocomplete_matches_cjk_punctuation_trigger_characters() {
    // `setAutocompleteTriggerCharacters(["，"])` 在 TS 里是合法的（只跳过空白与 "/"）。
    // 正则的边界分支 `(?:^|sep)` 可以消费前一个分隔符（或行首）后把这个标点当作触发字符；
    // `suffix` 为空时标点本身不属于后缀，不能被当成“后缀里的分隔符”。
    for prefix in ["", "a "] {
        let mut h = Harness::new();
        let clock = with_fake_clock(&mut h);
        let calls = Arc::new(AtomicUsize::new(0));
        let sink = Arc::clone(&calls);
        h.editor.set_autocomplete_provider(Box::new(
            MockProvider::new(move |_lines, _line, _col, _options| {
                sink.fetch_add(1, Ordering::SeqCst);
                Some(suggestions(vec![item("符号")], ""))
            })
            .with_triggers(vec!['，']),
        ));

        h.input(prefix);
        h.input("，");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "prefix={prefix:?}");
        clock.advance(20.0);
        flush(&mut h.editor);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "prefix={prefix:?}");
        assert!(h.editor.is_showing_autocomplete(), "prefix={prefix:?}");
    }
}

#[test]
fn autocomplete_resets_custom_trigger_characters_when_provider_changes() {
    let mut h = Harness::new();
    let calls = Arc::new(AtomicUsize::new(0));
    h.editor.set_autocomplete_provider(Box::new(
        MockProvider::new(|_lines, _line, _col, _options| {
            Some(suggestions(vec![item("$skill-name")], "$"))
        })
        .with_triggers(vec!['$']),
    ));
    let sink = Arc::clone(&calls);
    h.editor
        .set_autocomplete_provider(Box::new(MockProvider::new(
            move |_lines, _line, _col, _options| {
                sink.fetch_add(1, Ordering::SeqCst);
                Some(suggestions(vec![item("$skill-name")], "$"))
            },
        )));

    h.input("$");
    h.input("s");
    flush(&mut h.editor);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!h.editor.is_showing_autocomplete());
}

/// 对应 TS 测试里的 mock：promise 只在收到结果或 abort 信号时 settle。
///
/// `receiver` 永不完成（sender 被测试持有），复刻 TS 里 500ms 才 resolve 的超时；
/// 取消令牌的分支复刻 `options.signal.addEventListener("abort", ...)`（计数 + resolve(null)）。
struct DeferredProvider {
    receiver: Arc<Mutex<Option<tokio::sync::oneshot::Receiver<Option<AutocompleteSuggestions>>>>>,
    aborts: Arc<AtomicUsize>,
}

impl AutocompleteProvider for DeferredProvider {
    fn get_suggestions<'a>(
        &'a mut self,
        _lines: &'a [String],
        _cursor_line: usize,
        _cursor_col: usize,
        options: &'a GetSuggestionsOptions,
    ) -> Pin<Box<dyn Future<Output = Option<AutocompleteSuggestions>> + Send + 'a>> {
        let receiver = self.receiver.lock().unwrap().take();
        let aborts = Arc::clone(&self.aborts);
        let signal = options.signal.clone();
        Box::pin(async move {
            tokio::select! {
                result = async {
                    match receiver {
                        Some(receiver) => receiver.await.ok().flatten(),
                        None => std::future::pending().await,
                    }
                } => result,
                _ = signal.cancelled() => {
                    aborts.fetch_add(1, Ordering::SeqCst);
                    None
                }
            }
        })
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> ApplyCompletionResult {
        apply_replacing_prefix(lines, cursor_line, cursor_col, &item.value, prefix)
    }
}

#[test]
fn autocomplete_aborts_active_at_autocomplete_when_typing_continues() {
    let mut h = Harness::new();
    let clock = with_fake_clock(&mut h);
    let aborts = Arc::new(AtomicUsize::new(0));
    let (sender, receiver) = tokio::sync::oneshot::channel();
    h.editor
        .set_autocomplete_provider(Box::new(DeferredProvider {
            receiver: Arc::new(Mutex::new(Some(receiver))),
            aborts: Arc::clone(&aborts),
        }));
    // sender 保持在测试侧：请求不会自己完成，只能被 abort。
    let _sender = sender;

    for ch in ["@", "m", "a", "i"] {
        h.input(ch);
    }
    clock.advance(20.0);
    flush(&mut h.editor);

    h.input("n");
    flush(&mut h.editor);

    assert_eq!(aborts.load(Ordering::SeqCst), 1);
}

#[test]
fn autocomplete_hides_when_backspacing_slash_command_to_empty() {
    let mut h = Harness::new();
    h.editor
        .set_autocomplete_provider(Box::new(MockProvider::new(
            |lines, _line, col, _options| {
                let prefix = utf16_slice(lines.first().map(String::as_str).unwrap_or(""), 0, col);
                if prefix.starts_with('/') {
                    let query = utf16_suffix_str(&prefix, 1);
                    let commands = [
                        (item("/model"), "Change model"),
                        (item("/help"), "Show help"),
                    ];
                    let filtered: Vec<AutocompleteItem> = commands
                        .into_iter()
                        .filter(|(command, _)| command.value.starts_with(&query))
                        .map(|(mut command, description)| {
                            command.description = Some(description.to_string());
                            command
                        })
                        .collect();
                    if !filtered.is_empty() {
                        return Some(suggestions(filtered, &prefix));
                    }
                }
                None
            },
        )));

    h.input("/");
    flush(&mut h.editor);
    assert_eq!(h.text(), "/");
    assert!(h.editor.is_showing_autocomplete());

    h.input("\x7f");
    flush(&mut h.editor);
    assert_eq!(h.text(), "");
    assert!(!h.editor.is_showing_autocomplete());
}

/// 构造 `/argtest` 风格的参数补全 provider。
fn argtest_provider(
    all_arguments: Vec<&'static str>,
    filter: bool,
    command: &'static str,
) -> MockProvider {
    MockProvider::new(move |lines, _line, col, _options| {
        let text = lines.first().cloned().unwrap_or_default();
        let before = utf16_slice(&text, 0, col);
        let prefix_command = format!("/{command} ");
        let rest = before.strip_prefix(&prefix_command)?;
        if rest.is_empty() || rest.contains(' ') {
            return None;
        }
        let items: Vec<AutocompleteItem> = all_arguments
            .iter()
            .filter(|argument| !filter || argument.starts_with(rest))
            .map(|argument| item(argument))
            .collect();
        if items.is_empty() {
            None
        } else {
            Some(suggestions(items, rest))
        }
    })
}

fn type_text(harness: &mut Harness, text: &str) {
    for ch in text.chars() {
        harness.input(&ch.to_string());
    }
}

#[test]
fn autocomplete_applies_exact_typed_slash_argument_value_on_enter_even_when_first_item_highlighted()
{
    let mut h = Harness::new();
    h.editor
        .set_autocomplete_provider(Box::new(argtest_provider(
            vec!["one", "two", "three"],
            true,
            "argtest",
        )));
    type_text(&mut h, "/argtest two");
    assert_eq!(h.text(), "/argtest two");
    flush(&mut h.editor);
    assert!(h.editor.is_showing_autocomplete());
    h.input("\r");
    assert_eq!(h.text(), "/argtest two");
}

#[test]
fn autocomplete_selects_first_prefix_match_on_enter_when_typed_arg_is_not_exact_match() {
    let mut h = Harness::new();
    h.editor
        .set_autocomplete_provider(Box::new(argtest_provider(
            vec!["two", "three", "twelve"],
            true,
            "argtest",
        )));
    type_text(&mut h, "/argtest t");
    flush(&mut h.editor);
    assert!(h.editor.is_showing_autocomplete());
    h.input("\r");
    assert_eq!(h.text(), "/argtest two");
}

#[test]
fn autocomplete_highlights_unique_prefix_match_as_user_types() {
    let mut h = Harness::new();
    h.editor
        .set_autocomplete_provider(Box::new(argtest_provider(
            vec!["one", "two", "three"],
            false,
            "argtest",
        )));
    type_text(&mut h, "/argtest tw");
    assert_eq!(h.text(), "/argtest tw");
    flush(&mut h.editor);
    assert!(h.editor.is_showing_autocomplete());
    h.input("\r");
    assert_eq!(h.text(), "/argtest two");
}

#[test]
fn autocomplete_selects_first_prefix_match_when_multiple_items_match() {
    let mut h = Harness::new();
    h.editor
        .set_autocomplete_provider(Box::new(argtest_provider(
            vec!["one", "two", "three"],
            false,
            "argtest",
        )));
    type_text(&mut h, "/argtest t");
    flush(&mut h.editor);
    assert!(h.editor.is_showing_autocomplete());
    h.input("\r");
    assert_eq!(h.text(), "/argtest two");
}

#[test]
fn autocomplete_works_for_built_in_style_command_argument_completion_model_like() {
    let mut h = Harness::new();
    let all_models = ["gpt-4o", "gpt-4o-mini", "claude-sonnet"];
    h.editor
        .set_autocomplete_provider(Box::new(MockProvider::new(
            move |lines, _line, col, _options| {
                let before = utf16_slice(lines.first().map(String::as_str).unwrap_or(""), 0, col);
                let rest = before.strip_prefix("/model ")?;
                if rest.is_empty() || rest.contains(' ') {
                    return None;
                }
                let filtered: Vec<AutocompleteItem> = all_models
                    .iter()
                    .filter(|model| model.starts_with(rest))
                    .map(|model| item(model))
                    .collect();
                if filtered.is_empty() {
                    None
                } else {
                    Some(suggestions(filtered, rest))
                }
            },
        )));
    type_text(&mut h, "/model gpt-4o-mini");
    assert_eq!(h.text(), "/model gpt-4o-mini");
    flush(&mut h.editor);
    assert!(h.editor.is_showing_autocomplete());
    h.input("\r");
    assert_eq!(h.text(), "/model gpt-4o-mini");
}

#[test]
fn autocomplete_awaits_async_slash_command_argument_completions() {
    let mut h = Harness::new();
    let command = SlashCommand {
        name: "load-skills".to_string(),
        description: Some("Load skills".to_string()),
        argument_hint: None,
        get_argument_completions: Some(Box::new(|prefix: &str| -> ArgumentCompletionsFuture<'_> {
            let prefix = prefix.to_string();
            Box::pin(async move {
                if prefix.starts_with('s') {
                    Some(vec![item("skill-a")])
                } else {
                    None
                }
            })
        })),
    };
    h.editor
        .set_autocomplete_provider(Box::new(CombinedAutocompleteProvider::new(
            vec![CommandEntry::Slash(command)],
            std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            None,
        )));
    h.editor.set_text("/load-skills ");

    h.input("s");
    flush(&mut h.editor);
    assert!(h.editor.is_showing_autocomplete());

    h.input("\t");
    assert_eq!(h.text(), "/load-skills skill-a");
    assert!(!h.editor.is_showing_autocomplete());
}

#[test]
fn autocomplete_ignores_invalid_slash_command_argument_completion_results() {
    // TS 用 `as unknown as` 让 getArgumentCompletions 返回字符串 "not-an-array"；
    // Rust 的回调类型静态保证 `Option<Vec<...>>`，这里用 `None` 表达同一"非法/无结果"
    // 路径（provider 会忽略非数组结果）。
    let mut h = Harness::new();
    let command = SlashCommand {
        name: "load-skills".to_string(),
        description: Some("Load skills".to_string()),
        argument_hint: None,
        get_argument_completions: Some(Box::new(
            |_prefix: &str| -> ArgumentCompletionsFuture<'_> { Box::pin(async move { None }) },
        )),
    };
    h.editor
        .set_autocomplete_provider(Box::new(CombinedAutocompleteProvider::new(
            vec![CommandEntry::Slash(command)],
            std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            None,
        )));
    h.editor.set_text("/load-skills ");

    h.input("s");
    flush(&mut h.editor);
    assert!(!h.editor.is_showing_autocomplete());
    assert_eq!(h.text(), "/load-skills s");
}

#[test]
fn autocomplete_does_not_show_argument_completions_when_command_has_no_argument_completer() {
    let mut h = Harness::new();
    let help = SlashCommand {
        name: "help".to_string(),
        description: Some("Show help".to_string()),
        argument_hint: None,
        get_argument_completions: None,
    };
    let model = SlashCommand {
        name: "model".to_string(),
        description: Some("Switch model".to_string()),
        argument_hint: None,
        get_argument_completions: Some(Box::new(
            |_prefix: &str| -> ArgumentCompletionsFuture<'_> {
                Box::pin(async move { Some(vec![item("claude-opus")]) })
            },
        )),
    };
    h.editor
        .set_autocomplete_provider(Box::new(CombinedAutocompleteProvider::new(
            vec![CommandEntry::Slash(help), CommandEntry::Slash(model)],
            std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            None,
        )));

    h.input("/");
    h.input("h");
    h.input("e");
    flush(&mut h.editor);
    assert!(h.editor.is_showing_autocomplete());

    h.input("\t");
    assert_eq!(h.text(), "/help ");
    assert!(!h.editor.is_showing_autocomplete());
}

// ===========================================================================
// Character jump (Ctrl+])（14）
// ===========================================================================

#[test]
fn jump_forward_to_first_occurrence_of_character_on_same_line() {
    let mut h = Harness::new();
    h.editor.set_text("hello world");
    h.input("\x01");
    assert_eq!(h.cursor(), (0, 0));
    h.input("\x1d");
    h.input("o");
    assert_eq!(h.cursor(), (0, 4));
}

#[test]
fn jump_forward_to_next_occurrence_after_cursor() {
    let mut h = Harness::new();
    h.editor.set_text("hello world");
    h.input("\x01");
    for _ in 0..4 {
        h.input("\x1b[C");
    }
    assert_eq!(h.cursor(), (0, 4));
    h.input("\x1d");
    h.input("o");
    assert_eq!(h.cursor(), (0, 7));
}

#[test]
fn jump_forward_across_multiple_lines() {
    let mut h = Harness::new();
    h.editor.set_text("abc\ndef\nghi");
    h.input("\x1b[A");
    h.input("\x1b[A");
    h.input("\x01");
    assert_eq!(h.cursor(), (0, 0));
    h.input("\x1d");
    h.input("g");
    assert_eq!(h.cursor(), (2, 0));
}

#[test]
fn jump_backward_to_first_occurrence_before_cursor_on_same_line() {
    let mut h = Harness::new();
    h.editor.set_text("hello world");
    assert_eq!(h.cursor(), (0, 11));
    h.input("\x1b\x1d");
    h.input("o");
    assert_eq!(h.cursor(), (0, 7));
}

#[test]
fn jump_backward_across_multiple_lines() {
    let mut h = Harness::new();
    h.editor.set_text("abc\ndef\nghi");
    assert_eq!(h.cursor(), (2, 3));
    h.input("\x1b\x1d");
    h.input("a");
    assert_eq!(h.cursor(), (0, 0));
}

#[test]
fn jump_does_nothing_when_character_is_not_found_forward() {
    let mut h = Harness::new();
    h.editor.set_text("hello world");
    h.input("\x01");
    assert_eq!(h.cursor(), (0, 0));
    h.input("\x1d");
    h.input("z");
    assert_eq!(h.cursor(), (0, 0));
}

#[test]
fn jump_does_nothing_when_character_is_not_found_backward() {
    let mut h = Harness::new();
    h.editor.set_text("hello world");
    assert_eq!(h.cursor(), (0, 11));
    h.input("\x1b\x1d");
    h.input("z");
    assert_eq!(h.cursor(), (0, 11));
}

#[test]
fn jump_is_case_sensitive() {
    let mut h = Harness::new();
    h.editor.set_text("Hello World");
    h.input("\x01");
    assert_eq!(h.cursor(), (0, 0));

    h.input("\x1d");
    h.input("h");
    assert_eq!(h.cursor(), (0, 0));

    h.input("\x1d");
    h.input("W");
    assert_eq!(h.cursor(), (0, 6));
}

#[test]
fn jump_cancels_jump_mode_when_ctrl_bracket_is_pressed_again() {
    let mut h = Harness::new();
    h.editor.set_text("hello world");
    h.input("\x01");
    assert_eq!(h.cursor(), (0, 0));
    h.input("\x1d");
    h.input("\x1d");
    h.input("o");
    assert_eq!(h.text(), "ohello world");
}

#[test]
fn jump_cancels_jump_mode_on_escape_and_processes_the_escape() {
    let mut h = Harness::new();
    h.editor.set_text("hello world");
    h.input("\x01");
    assert_eq!(h.cursor(), (0, 0));
    h.input("\x1d");
    h.input("\x1b");
    assert_eq!(h.cursor(), (0, 0));
    h.input("o");
    assert_eq!(h.text(), "ohello world");
}

#[test]
fn jump_cancels_backward_jump_mode_when_ctrl_alt_bracket_is_pressed_again() {
    let mut h = Harness::new();
    h.editor.set_text("hello world");
    assert_eq!(h.cursor(), (0, 11));
    h.input("\x1b\x1d");
    h.input("\x1b\x1d");
    h.input("o");
    assert_eq!(h.text(), "hello worldo");
}

#[test]
fn jump_searches_for_special_characters() {
    let mut h = Harness::new();
    h.editor.set_text("foo(bar) = baz;");
    h.input("\x01");
    assert_eq!(h.cursor(), (0, 0));

    h.input("\x1d");
    h.input("(");
    assert_eq!(h.cursor(), (0, 3));

    h.input("\x1d");
    h.input("=");
    assert_eq!(h.cursor(), (0, 9));
}

#[test]
fn jump_handles_empty_text_gracefully() {
    let mut h = Harness::new();
    h.editor.set_text("");
    assert_eq!(h.cursor(), (0, 0));
    h.input("\x1d");
    h.input("x");
    assert_eq!(h.cursor(), (0, 0));
}

#[test]
fn jump_resets_last_action_when_jumping() {
    let mut h = Harness::new();
    h.editor.set_text("hello world");
    h.input("\x01");
    h.input("x");
    assert_eq!(h.text(), "xhello world");

    h.input("\x1d");
    h.input("o");
    h.input("Y");
    assert_eq!(h.text(), "xhellYo world");

    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "xhello world");
}

// ===========================================================================
// Sticky column（19）
// ===========================================================================

/// `positionCursor(editor, line, col)`：先回到第 0 行，再下移到目标行，Ctrl+A 后右移。
fn position_cursor(h: &mut Harness, line: usize, col: usize) {
    for _ in 0..20 {
        h.input("\x1b[A");
    }
    for _ in 0..line {
        h.input("\x1b[B");
    }
    h.input("\x01");
    for _ in 0..col {
        h.input("\x1b[C");
    }
}

#[test]
fn sticky_preserves_target_column_when_moving_up_through_a_shorter_line() {
    let mut h = Harness::new();
    h.editor
        .set_text("2222222222x222\n\n1111111111_111111111111");
    assert_eq!(h.cursor(), (2, 23));
    h.input("\x01");
    for _ in 0..10 {
        h.input("\x1b[C");
    }
    assert_eq!(h.cursor(), (2, 10));

    h.input("\x1b[A");
    assert_eq!(h.cursor(), (1, 0));
    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, 10));
}

#[test]
fn sticky_preserves_target_column_when_moving_down_through_a_shorter_line() {
    let mut h = Harness::new();
    h.editor
        .set_text("1111111111_111\n\n2222222222x222222222222");

    h.input("\x1b[A");
    h.input("\x1b[A");
    h.input("\x01");
    for _ in 0..10 {
        h.input("\x1b[C");
    }
    assert_eq!(h.cursor(), (0, 10));

    h.input("\x1b[B");
    assert_eq!(h.cursor(), (1, 0));
    h.input("\x1b[B");
    assert_eq!(h.cursor(), (2, 10));
}

#[test]
fn sticky_resets_on_horizontal_movement_left_arrow() {
    let mut h = Harness::new();
    h.editor.set_text("1234567890\n\n1234567890");

    h.input("\x01");
    for _ in 0..5 {
        h.input("\x1b[C");
    }
    assert_eq!(h.cursor(), (2, 5));

    h.input("\x1b[A");
    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, 5));

    h.input("\x1b[D");
    assert_eq!(h.cursor(), (0, 4));

    h.input("\x1b[B");
    h.input("\x1b[B");
    assert_eq!(h.cursor(), (2, 4));
}

#[test]
fn sticky_resets_on_horizontal_movement_right_arrow() {
    let mut h = Harness::new();
    h.editor.set_text("1234567890\n\n1234567890");

    h.input("\x1b[A");
    h.input("\x1b[A");
    h.input("\x01");
    for _ in 0..5 {
        h.input("\x1b[C");
    }
    assert_eq!(h.cursor(), (0, 5));

    h.input("\x1b[B");
    h.input("\x1b[B");
    assert_eq!(h.cursor(), (2, 5));

    h.input("\x1b[C");
    assert_eq!(h.cursor(), (2, 6));

    h.input("\x1b[A");
    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, 6));
}

#[test]
fn sticky_resets_on_typing() {
    let mut h = Harness::new();
    h.editor.set_text("1234567890\n\n1234567890");
    h.input("\x01");
    for _ in 0..8 {
        h.input("\x1b[C");
    }
    h.input("\x1b[A");
    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, 8));

    h.input("X");
    assert_eq!(h.cursor(), (0, 9));

    h.input("\x1b[B");
    h.input("\x1b[B");
    assert_eq!(h.cursor(), (2, 9));
}

#[test]
fn sticky_resets_on_backspace() {
    let mut h = Harness::new();
    h.editor.set_text("1234567890\n\n1234567890");
    h.input("\x01");
    for _ in 0..8 {
        h.input("\x1b[C");
    }
    h.input("\x1b[A");
    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, 8));

    h.input("\x7f");
    assert_eq!(h.cursor(), (0, 7));

    h.input("\x1b[B");
    h.input("\x1b[B");
    assert_eq!(h.cursor(), (2, 7));
}

#[test]
fn sticky_resets_on_ctrl_a_move_to_line_start() {
    let mut h = Harness::new();
    h.editor.set_text("1234567890\n\n1234567890");
    h.input("\x01");
    for _ in 0..8 {
        h.input("\x1b[C");
    }
    h.input("\x1b[A");
    h.input("\x01");
    assert_eq!(h.cursor(), (1, 0));

    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, 0));
}

#[test]
fn sticky_resets_on_ctrl_e_move_to_line_end() {
    let mut h = Harness::new();
    h.editor.set_text("12345\n\n1234567890");
    h.input("\x01");
    for _ in 0..3 {
        h.input("\x1b[C");
    }
    h.input("\x1b[A");
    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, 3));

    h.input("\x05");
    assert_eq!(h.cursor(), (0, 5));

    h.input("\x1b[B");
    h.input("\x1b[B");
    assert_eq!(h.cursor(), (2, 5));
}

#[test]
fn sticky_resets_on_word_movement_ctrl_left() {
    let mut h = Harness::new();
    h.editor.set_text("hello world\n\nhello world");
    assert_eq!(h.cursor(), (2, 11));

    h.input("\x1b[A");
    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, 11));

    h.input("\x1b[1;5D");
    assert_eq!(h.cursor(), (0, 6));

    h.input("\x1b[B");
    h.input("\x1b[B");
    assert_eq!(h.cursor(), (2, 6));
}

#[test]
fn sticky_resets_on_word_movement_ctrl_right() {
    let mut h = Harness::new();
    h.editor.set_text("hello world\n\nhello world");
    h.input("\x1b[A");
    h.input("\x1b[A");
    h.input("\x01");
    assert_eq!(h.cursor(), (0, 0));

    h.input("\x1b[B");
    h.input("\x1b[B");
    assert_eq!(h.cursor(), (2, 0));

    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (2, 5));

    h.input("\x1b[A");
    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, 5));
}

#[test]
fn sticky_resets_on_undo() {
    let mut h = Harness::new();
    h.editor.set_text("1234567890\n\n1234567890");
    h.input("\x1b[A");
    h.input("\x1b[A");
    h.input("\x01");
    for _ in 0..8 {
        h.input("\x1b[C");
    }
    assert_eq!(h.cursor(), (0, 8));

    h.input("\x1b[B");
    h.input("\x1b[B");
    assert_eq!(h.cursor(), (2, 8));

    h.input("X");
    assert_eq!(h.text(), "1234567890\n\n12345678X90");
    assert_eq!(h.cursor(), (2, 9));

    h.input("\x1b[A");
    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, 9));

    h.input("\x1b[45;5u");
    assert_eq!(h.text(), "1234567890\n\n1234567890");
    assert_eq!(h.cursor(), (2, 8));

    h.input("\x1b[A");
    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, 8));
}

#[test]
fn sticky_handles_multiple_consecutive_up_down_movements() {
    let mut h = Harness::new();
    h.editor.set_text("1234567890\nab\ncd\nef\n1234567890");
    h.input("\x01");
    for _ in 0..7 {
        h.input("\x1b[C");
    }
    assert_eq!(h.cursor(), (4, 7));

    h.input("\x1b[A");
    assert_eq!(h.cursor(), (3, 2));
    h.input("\x1b[A");
    assert_eq!(h.cursor(), (2, 2));
    h.input("\x1b[A");
    assert_eq!(h.cursor(), (1, 2));
    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, 7));

    h.input("\x1b[B");
    assert_eq!(h.cursor(), (1, 2));
    h.input("\x1b[B");
    assert_eq!(h.cursor(), (2, 2));
    h.input("\x1b[B");
    assert_eq!(h.cursor(), (3, 2));
    h.input("\x1b[B");
    assert_eq!(h.cursor(), (4, 7));
}

#[test]
fn sticky_moves_correctly_through_wrapped_visual_lines_without_getting_stuck() {
    let mut h = Harness::with_size(15, 24);
    h.editor.set_text("short\n123456789012345678901234567890");
    h.render(15);
    assert_eq!(h.cursor(), (1, 30));

    h.input("\x1b[A");
    assert_eq!(h.cursor().0, 1);
    h.input("\x1b[A");
    assert_eq!(h.cursor().0, 1);
    h.input("\x1b[A");
    assert_eq!(h.cursor().0, 0);
}

#[test]
fn sticky_handles_set_text_resetting_sticky_column() {
    let mut h = Harness::new();
    h.editor.set_text("1234567890\n\n1234567890");
    h.input("\x01");
    for _ in 0..8 {
        h.input("\x1b[C");
    }
    h.input("\x1b[A");

    h.editor.set_text("abcdefghij\n\nabcdefghij");
    assert_eq!(h.cursor(), (2, 10));

    h.input("\x1b[A");
    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, 10));
}

#[test]
fn sticky_sets_preferred_visual_col_when_pressing_right_at_end_of_prompt_last_line() {
    let mut h = Harness::new();
    h.editor.set_text("111111111x1111111111\n\n333333333_");

    h.input("\x1b[A");
    h.input("\x1b[A");
    h.input("\x05");
    assert_eq!(h.cursor(), (0, 20));

    h.input("\x1b[B");
    h.input("\x1b[B");
    assert_eq!(h.cursor(), (2, 10));

    h.input("\x1b[C");
    assert_eq!(h.cursor(), (2, 10));

    h.input("\x1b[A");
    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, 10));
}

#[test]
fn sticky_handles_editor_resizes_when_preferred_visual_col_is_on_the_same_line() {
    let mut h = Harness::with_size(80, 24);
    h.editor
        .set_text("12345678901234567890\n\n12345678901234567890");

    h.input("\x01");
    for _ in 0..15 {
        h.input("\x1b[C");
    }

    h.input("\x1b[A");
    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, 15));

    h.render(12);

    h.input("\x1b[B");
    h.input("\x1b[B");
    assert_eq!(h.cursor().1, 4);
}

#[test]
fn sticky_handles_editor_resizes_when_preferred_visual_col_is_on_a_different_line() {
    let mut h = Harness::with_size(80, 24);
    h.editor.set_text("short\n12345678901234567890");

    h.input("\x01");
    for _ in 0..15 {
        h.input("\x1b[C");
    }
    assert_eq!(h.cursor(), (1, 15));

    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, 5));

    h.render(10);

    h.input("\x1b[B");
    assert_eq!(h.cursor(), (1, 8));

    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, 5));

    h.render(80);

    h.input("\x1b[B");
    assert_eq!(h.cursor(), (1, 15));
}

#[test]
fn sticky_rewrapped_lines_target_fits_current_visual_column() {
    let mut h = Harness::with_size(80, 24);
    h.editor.set_text("abcdefghijklmnopqr\n123456789012345678");

    position_cursor(&mut h, 0, 18);
    assert_eq!(h.cursor(), (0, 18));

    h.render(10);
    h.input("\x1b[B");
    assert_eq!(h.cursor(), (1, 8));

    h.render(80);
    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, 8));

    h.input("\x1b[B");
    assert_eq!(h.cursor(), (1, 8));
}

#[test]
fn sticky_rewrapped_lines_target_shorter_than_current_visual_column() {
    let mut h = Harness::with_size(80, 24);
    h.editor
        .set_text("abcdefghijklmnopqr\n123456789012345678\nab");

    position_cursor(&mut h, 0, 18);
    assert_eq!(h.cursor(), (0, 18));

    h.render(10);
    h.input("\x1b[B");
    assert_eq!(h.cursor(), (1, 8));

    h.render(80);
    h.input("\x1b[B");
    assert_eq!(h.cursor(), (2, 2));

    h.input("\x1b[A");
    assert_eq!(h.cursor(), (1, 8));
}

// ===========================================================================
// Paste marker atomic behavior（22）
// ===========================================================================

use regex::Regex;

/// 模拟一次大粘贴并返回编辑器文本（`pasteWithMarker` 辅助）。
fn paste_with_marker(h: &mut Harness) -> String {
    let big_content = "line\n".repeat(20);
    let big_content = big_content.trim_end();
    h.input(&format!("\x1b[200~{big_content}\x1b[201~"));
    h.text()
}

/// 12 行、带区分标签的粘贴内容。
fn big_paste(tag: &str) -> String {
    (0..12)
        .map(|i| format!("{tag}{i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn marker_regex() -> Regex {
    Regex::new(r"\[paste #[0-9]+ \+[0-9]+ lines\]").unwrap()
}

fn first_marker(text: &str) -> String {
    marker_regex()
        .find(text)
        .map(|m| m.as_str().to_string())
        .expect("paste marker present")
}

fn all_markers(text: &str) -> Vec<String> {
    marker_regex()
        .find_iter(text)
        .map(|m| m.as_str().to_string())
        .collect()
}

#[test]
fn paste_creates_a_marker_for_large_pastes() {
    let mut h = Harness::new();
    let text = paste_with_marker(&mut h);
    assert!(marker_regex().is_match(&text), "text: {text}");
}

#[test]
fn paste_treats_marker_as_single_unit_for_right_arrow() {
    let mut h = Harness::new();
    h.input("A");
    paste_with_marker(&mut h);
    h.input("B");

    h.input("\x01");
    assert_eq!(h.cursor(), (0, 0));

    h.input("\x1b[C");
    assert_eq!(h.cursor(), (0, 1));

    h.input("\x1b[C");
    let marker = first_marker(&h.text());
    assert_eq!(h.cursor(), (0, 1 + utf16_len(&marker)));

    h.input("\x1b[C");
    assert_eq!(h.cursor(), (0, 1 + utf16_len(&marker) + 1));
}

#[test]
fn paste_treats_marker_as_single_unit_for_left_arrow() {
    let mut h = Harness::new();
    h.input("A");
    paste_with_marker(&mut h);
    h.input("B");

    h.input("\x1b[D");
    let marker = first_marker(&h.text());
    assert_eq!(h.cursor(), (0, 1 + utf16_len(&marker)));

    h.input("\x1b[D");
    assert_eq!(h.cursor(), (0, 1));

    h.input("\x1b[D");
    assert_eq!(h.cursor(), (0, 0));
}

#[test]
fn paste_treats_marker_as_single_unit_for_backspace() {
    let mut h = Harness::new();
    h.input("A");
    paste_with_marker(&mut h);
    h.input("B");
    let marker = first_marker(&h.text());

    h.input("\x01");
    h.input("\x1b[C");
    h.input("\x1b[C");
    assert_eq!(h.cursor(), (0, 1 + utf16_len(&marker)));

    h.input("\x7f");
    assert_eq!(h.text(), "AB");
    assert_eq!(h.cursor(), (0, 1));
}

#[test]
fn paste_treats_marker_as_single_unit_for_forward_delete() {
    let mut h = Harness::new();
    h.input("A");
    paste_with_marker(&mut h);
    h.input("B");

    h.input("\x01");
    h.input("\x1b[C");
    h.input("\x1b[3~");
    assert_eq!(h.text(), "AB");
    assert_eq!(h.cursor(), (0, 1));
}

#[test]
fn paste_treats_marker_as_single_unit_for_word_movement() {
    let mut h = Harness::new();
    h.input("X");
    h.input(" ");
    paste_with_marker(&mut h);
    h.input(" ");
    h.input("Y");
    let marker = first_marker(&h.text());

    h.input("\x01");
    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (0, 1));

    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (0, 2 + utf16_len(&marker)));
}

#[test]
fn paste_undo_restores_marker_after_backspace_deletion() {
    let mut h = Harness::new();
    h.input("A");
    paste_with_marker(&mut h);
    h.input("B");
    let text_before = h.text();

    h.input("\x01");
    h.input("\x1b[C");
    h.input("\x1b[C");
    h.input("\x7f");
    assert_eq!(h.text(), "AB");

    h.input("\x1b[45;5u");
    assert_eq!(h.text(), text_before);
}

#[test]
fn paste_undo_after_marker_deletion_restores_the_paste_registry() {
    let mut h = Harness::new();
    let submitted = Rc::new(RefCell::new(String::new()));
    let sink = Rc::clone(&submitted);
    h.editor.on_submit = Some(Box::new(move |text: &str| {
        *sink.borrow_mut() = text.to_string();
    }));

    let paste = big_paste("alpha");
    h.input(&format!("\x1b[200~{paste}\x1b[201~"));
    h.input("\x7f");
    h.input("\x1b[45;5u");
    h.input("\r");
    assert_eq!(*submitted.borrow(), paste);
}

#[test]
fn paste_undo_after_deleting_the_first_of_two_markers_restores_both_registry_entries() {
    let mut h = Harness::new();
    let submitted = Rc::new(RefCell::new(String::new()));
    let sink = Rc::clone(&submitted);
    h.editor.on_submit = Some(Box::new(move |text: &str| {
        *sink.borrow_mut() = text.to_string();
    }));

    let paste_a = big_paste("alpha");
    let paste_b = big_paste("beta");
    h.input(&format!("\x1b[200~{paste_a}\x1b[201~"));
    h.input(&format!("\x1b[200~{paste_b}\x1b[201~"));
    h.input("\x01");
    h.input("\x1b[C");
    h.input("\x7f");
    h.input("\x1b[45;5u");
    h.input("\r");
    assert_eq!(*submitted.borrow(), format!("{paste_a}{paste_b}"));
}

#[test]
fn paste_renumbers_the_registry_in_ascending_id_order_when_markers_are_out_of_order_in_text() {
    let mut h = Harness::new();
    let submitted = Rc::new(RefCell::new(String::new()));
    let sink = Rc::clone(&submitted);
    h.editor.on_submit = Some(Box::new(move |text: &str| {
        *sink.borrow_mut() = text.to_string();
    }));

    let paste_a = big_paste("alpha");
    let paste_b = big_paste("beta");
    let paste_c = big_paste("gamma");
    h.input(&format!("\x1b[200~{paste_a}\x1b[201~"));
    h.input("\x01");
    h.input(&format!("\x1b[200~{paste_b}\x1b[201~"));
    h.input("\x01");
    h.input(&format!("\x1b[200~{paste_c}\x1b[201~"));
    h.input("\x05");
    h.input("\x7f");
    h.input("\r");
    assert_eq!(*submitted.borrow(), format!("{paste_c}{paste_b}"));
}

/// TS 的 marker 重编号回调直接拼接未参与匹配的可选组：`suffixGroup` 是 `undefined`
/// 时模板字符串会写出字面 "undefined"（`[paste #1undefined]`）。这是当前稳定契约，改动需同步测试。
#[test]
fn paste_renumbering_appends_undefined_for_marker_without_suffix() {
    let mut h = Harness::new();
    let block = big_paste("line");
    h.input(&format!("\x1b[200~{block}\x1b[201~"));
    h.input("[paste #2]");
    assert_eq!(h.text(), "[paste #1 +12 lines][paste #2]");

    h.input("\x01");
    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (0, utf16_len("[paste #1 +12 lines]")));

    h.input("\x7f");
    assert_eq!(h.text(), "[paste #1undefined]");
    assert_eq!(h.editor.get_expanded_text(), "[paste #1undefined]");
}

#[test]
fn paste_undo_after_set_text_restores_markers_and_registry() {
    let mut h = Harness::new();
    let submitted = Rc::new(RefCell::new(String::new()));
    let sink = Rc::clone(&submitted);
    h.editor.on_submit = Some(Box::new(move |text: &str| {
        *sink.borrow_mut() = text.to_string();
    }));

    let paste = big_paste("alpha");
    h.input(&format!("\x1b[200~{paste}\x1b[201~"));
    h.editor.set_text("replacement");
    h.input("\x1b[45;5u");
    h.input("\r");
    assert_eq!(*submitted.borrow(), paste);
}

#[test]
fn paste_handles_multiple_markers_in_same_line() {
    let mut h = Harness::new();
    paste_with_marker(&mut h);
    h.input(" ");
    paste_with_marker(&mut h);

    let markers = all_markers(&h.text());
    assert_eq!(markers.len(), 2);

    h.input("\x01");
    h.input("\x1b[C");
    assert_eq!(h.cursor(), (0, utf16_len(&markers[0])));
    h.input("\x1b[C");
    assert_eq!(h.cursor(), (0, utf16_len(&markers[0]) + 1));
    h.input("\x1b[C");
    assert_eq!(
        h.cursor(),
        (0, utf16_len(&markers[0]) + 1 + utf16_len(&markers[1]))
    );
}

#[test]
fn paste_does_not_treat_manually_typed_marker_like_text_as_atomic() {
    let mut h = Harness::new();
    let fake_marker = "[paste #99 +5 lines]";
    for ch in fake_marker.chars() {
        h.input(&ch.to_string());
    }
    assert_eq!(h.text(), fake_marker);

    h.input("\x01");
    h.input("\x1b[C");
    assert_eq!(h.cursor(), (0, 1));
}

#[test]
fn paste_does_not_crash_when_marker_is_wider_than_terminal_width() {
    let mut h = Harness::new();
    let big_content = "line\n".repeat(47);
    let big_content = big_content.trim_end();
    h.input(&format!("\x1b[200~{big_content}\x1b[201~"));

    let text = h.text();
    let marker = first_marker(&text);
    assert!(visible_width(&marker) > 8);

    let lines = h.render(8);
    for line in &lines {
        assert!(
            visible_width(line) <= 8,
            "line exceeds width 8: visible={} text={line:?}",
            visible_width(line)
        );
    }
}

#[test]
fn paste_does_not_crash_when_text_and_marker_exceed_width_with_cursor_on_marker() {
    let mut h = Harness::new();
    for _ in 0..35 {
        h.input("b");
    }
    let big_content = "line\n".repeat(27);
    let big_content = big_content.trim_end();
    h.input(&format!("\x1b[200~{big_content}\x1b[201~"));
    for _ in 0..4 {
        h.input("b");
    }
    for _ in 0..5 {
        h.input("\x1b[D");
    }

    let render_width = 54;
    let lines = h.render(render_width);
    for line in &lines {
        assert!(
            visible_width(line) <= render_width,
            "line exceeds width {render_width}: visible={} text={line:?}",
            visible_width(line)
        );
    }
}

#[test]
fn paste_word_wrap_re_checks_overflow_after_backtracking_to_wrap_opportunity() {
    let mut h = Harness::new();
    h.input(" ");
    for _ in 0..35 {
        h.input("b");
    }
    let big_content = "line\n".repeat(27);
    let big_content = big_content.trim_end();
    h.input(&format!("\x1b[200~{big_content}\x1b[201~"));
    for _ in 0..4 {
        h.input("b");
    }

    let render_width = 54;
    let lines = h.render(render_width);
    for line in &lines {
        assert!(
            visible_width(line) <= render_width,
            "line exceeds width {render_width}: visible={} text={line:?}",
            visible_width(line)
        );
    }
}

#[test]
fn paste_expands_large_pasted_content_literally_in_get_expanded_text() {
    let mut h = Harness::new();
    let pasted_text = [
        "line 1",
        "line 2",
        "line 3",
        "line 4",
        "line 5",
        "line 6",
        "line 7",
        "line 8",
        "line 9",
        "line 10",
        "tokens $1 $2 $& $$ $` $' end",
    ]
    .join("\n");
    h.input(&format!("\x1b[200~{pasted_text}\x1b[201~"));

    assert!(marker_regex().is_match(&h.text()));
    assert_eq!(h.editor.get_expanded_text(), pasted_text);
}

#[test]
fn paste_snaps_to_the_marker_start_when_navigating_down_into_it() {
    let mut h = Harness::new();
    h.editor.set_text("12345678901234567890\n\nhello ");
    let big_content = "x".repeat(2000);
    h.input(&format!("\x1b[200~{big_content}\x1b[201~"));
    h.render(80);

    h.input("\x1b[A");
    h.input("\x1b[A");
    h.input("\x01");
    for _ in 0..10 {
        h.input("\x1b[C");
    }
    assert_eq!(h.cursor(), (0, 10));

    h.input("\x1b[B");
    assert_eq!(h.cursor(), (1, 0));

    h.input("\x1b[B");
    assert_eq!(h.cursor(), (2, 6));
}

#[test]
fn paste_preserves_sticky_column_when_navigating_through_marker_line() {
    let mut h = Harness::with_size(30, 24);
    for ch in "1234567890123456".chars() {
        h.input(&ch.to_string());
    }
    h.input("\n");
    h.input("\n");
    h.input(&format!("\x1b[200~{}\x1b[201~", "x".repeat(2000)));
    h.input("\n");
    h.input("\n");
    for ch in "abcdefghijklmnop".chars() {
        h.input(&ch.to_string());
    }
    h.render(30);

    for _ in 0..4 {
        h.input("\x1b[A");
    }
    h.input("\x01");
    for _ in 0..10 {
        h.input("\x1b[C");
    }
    assert_eq!(h.cursor(), (0, 10));

    h.input("\x1b[B");
    assert_eq!(h.cursor(), (1, 0));
    h.input("\x1b[B");
    assert_eq!(h.cursor(), (2, 0));
    h.input("\x1b[B");
    assert_eq!(h.cursor(), (3, 0));
    h.input("\x1b[B");
    assert_eq!(h.cursor(), (4, 10));
}

#[test]
fn paste_does_not_get_stuck_moving_down_from_a_multi_visual_line_marker() {
    let mut h = Harness::with_size(20, 24);
    for ch in "abcdefgh".chars() {
        h.input(&ch.to_string());
    }
    let big_content = "line\n".repeat(100);
    let big_content = big_content.trim_end();
    h.input(&format!("\x1b[200~{big_content}\x1b[201~"));
    for ch in "ijklmnopqr".chars() {
        h.input(&ch.to_string());
    }
    h.input("\n");
    for ch in "123456789012345678".chars() {
        h.input(&ch.to_string());
    }
    h.render(20);

    let text = h.text();
    let marker = first_marker(&text);
    let marker_len = utf16_len(&marker);
    assert!(marker_len > 20);
    let marker_start = 8;
    let marker_end = marker_start + marker_len;

    h.input("\x1b[A");
    h.input("\x01");
    for _ in 0..6 {
        h.input("\x1b[C");
    }
    assert_eq!(h.cursor(), (0, 6));

    h.input("\x1b[B");
    assert_eq!(h.cursor(), (0, marker_start));

    h.input("\x1b[B");
    assert_eq!(h.cursor().0, 0);
    assert_eq!(h.cursor().1, marker_end);

    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, marker_start));

    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, 6));
}

#[test]
fn paste_skips_marker_continuation_visual_lines_when_preferred_col_falls_in_marker_tail() {
    let mut h = Harness::with_size(20, 24);
    for ch in "abcdefgh".chars() {
        h.input(&ch.to_string());
    }
    let big_content = "line\n".repeat(100);
    let big_content = big_content.trim_end();
    h.input(&format!("\x1b[200~{big_content}\x1b[201~"));
    for ch in "ijklmnopqr".chars() {
        h.input(&ch.to_string());
    }
    h.input("\n");
    for ch in "123456789012345678".chars() {
        h.input(&ch.to_string());
    }
    h.render(20);

    h.input("\x1b[A");
    h.input("\x01");
    for _ in 0..3 {
        h.input("\x1b[C");
    }
    assert_eq!(h.cursor(), (0, 3));

    h.input("\x1b[B");
    assert_eq!(h.cursor().1, 8);

    h.input("\x1b[B");
    assert_eq!(h.cursor(), (1, 3));

    h.input("\x1b[A");
    assert_eq!(h.cursor().1, 8);
    h.input("\x1b[A");
    assert_eq!(h.cursor(), (0, 3));
}

#[test]
fn paste_submits_large_pasted_content_literally() {
    let mut h = Harness::new();
    let pasted_text = [
        "line 1",
        "line 2",
        "line 3",
        "line 4",
        "line 5",
        "line 6",
        "line 7",
        "line 8",
        "line 9",
        "line 10",
        "tokens $1 $2 $& $$ $` $' end",
    ]
    .join("\n");
    let submitted = Rc::new(RefCell::new(String::new()));
    let sink = Rc::clone(&submitted);
    h.editor.on_submit = Some(Box::new(move |text: &str| {
        *sink.borrow_mut() = text.to_string();
    }));

    h.input(&format!("\x1b[200~{pasted_text}\x1b[201~"));
    h.input("\r");
    assert_eq!(*submitted.borrow(), pasted_text);
}

// ===========================================================================
// 额外：鼠标、边框回调、padding、访问器（spec §2.15.7/2.15.8、§2.15.6）
// ===========================================================================

use kiana_tui::tui::{CURSOR_MARKER, TuiMouseButton, TuiMouseEvent, TuiMouseEventType};

fn click_event(x: isize, y: isize, width: usize) -> TuiMouseEvent {
    TuiMouseEvent {
        event_type: TuiMouseEventType::Click,
        button: TuiMouseButton::Left,
        x,
        y,
        screen_x: x,
        screen_y: y,
        width,
        height: 24,
        shift: false,
        alt: false,
        ctrl: false,
        wheel_delta: None,
        click_count: None,
    }
}

#[test]
fn mouse_click_positions_cursor_on_clicked_grapheme() {
    let mut h = Harness::new();
    h.editor.set_text("hello\nworld");
    h.render(40);

    let result = h.editor.handle_mouse(&click_event(3, 1, 40));
    assert!(result.is_some());
    assert_eq!(h.cursor(), (0, 3));

    let result = h.editor.handle_mouse(&click_event(2, 2, 40));
    assert!(result.is_some());
    assert_eq!(h.cursor(), (1, 2));
}

#[test]
fn mouse_click_on_border_only_focuses() {
    let mut h = Harness::new();
    h.editor.set_text("hello");
    h.render(40);

    let result = h.editor.handle_mouse(&click_event(0, 0, 40)).unwrap();
    assert_eq!(result.handled, Some(true));
    assert_eq!(result.focus, Some(true));

    let result = h.editor.handle_mouse(&click_event(0, 3, 40)).unwrap();
    assert_eq!(result.handled, Some(true));
    assert_eq!(result.focus, Some(true));
}

#[test]
fn mouse_non_click_events_are_not_handled() {
    let mut h = Harness::new();
    h.editor.set_text("hello");
    h.render(40);

    let mut event = click_event(2, 1, 40);
    event.event_type = TuiMouseEventType::Press;
    assert!(h.editor.handle_mouse(&event).is_none());
}

#[test]
fn mouse_click_on_autocomplete_row_uses_rendered_count_plus_two_and_applies_selection() {
    let mut h = Harness::new();
    h.editor
        .set_autocomplete_provider(Box::new(MockProvider::new(
            |lines, _line, col, _options| {
                let prefix = utf16_slice(lines.first().map(String::as_str).unwrap_or(""), 0, col);
                if prefix == "/" {
                    Some(suggestions(vec![item("/model")], "/"))
                } else {
                    None
                }
            },
        )));
    h.input("/");
    flush(&mut h.editor);
    assert!(h.editor.is_showing_autocomplete());
    h.render(40);

    let start_row = h.editor.rendered_visible_line_count() as isize + 2;
    assert_eq!(start_row, 3);
    let result = h.editor.handle_mouse(&click_event(0, start_row, 40));
    assert!(result.is_some());
    // mock provider 的 applyCompletion 是"用 item.value 替换 prefix"（同 TS 测试 helper）。
    assert_eq!(h.text(), "/model");
    assert!(!h.editor.is_showing_autocomplete());
}

#[test]
fn border_render_hooks_override_top_and_bottom_borders() {
    let mut h = Harness::new();
    h.editor
        .set_top_border_renderer(Some(Arc::new(|width, hidden, _color, _base| {
            format!("TOP {width} {hidden}")
        })));
    h.editor
        .set_bottom_border_renderer(Some(Arc::new(|width, hidden, _color, _base| {
            format!("BOTTOM {width} {hidden}")
        })));
    h.editor.set_text("hello");
    let lines = h.render(40);
    assert_eq!(lines.first().unwrap(), "TOP 40 0");
    assert_eq!(lines.last().unwrap(), "BOTTOM 40 0");
}

#[test]
fn padding_x_adds_left_and_right_padding_and_keeps_total_width() {
    let mut h = Harness::with_options(
        20,
        24,
        EditorOptions {
            padding_x: Some(2.0),
            autocomplete_max_visible: None,
        },
    );
    h.editor.set_text("hi");
    let lines = h.render(20);
    for line in &lines {
        assert_eq!(visible_width(line), 20, "line {line:?}");
    }
    assert!(lines[1].starts_with("  hi"));
    assert!(lines[1].ends_with("  "));
    assert_eq!(h.editor.get_padding_x(), 2);
}

#[test]
fn focused_render_emits_hardware_cursor_marker() {
    let mut h = Harness::new();
    h.editor.set_text("abc");
    h.editor.set_focused(true);
    let lines = h.render(40);
    assert!(lines[1].contains(CURSOR_MARKER));

    h.editor.set_focused(false);
    let lines = h.render(40);
    assert!(!lines[1].contains(CURSOR_MARKER));
}

#[test]
fn autocomplete_max_visible_is_clamped_and_triggers_render() {
    let mut h = Harness::new();
    assert_eq!(h.editor.get_autocomplete_max_visible(), 5);

    let before = h._renders.get();
    h.editor.set_autocomplete_max_visible(100.0);
    assert_eq!(h.editor.get_autocomplete_max_visible(), 20);
    assert!(h._renders.get() > before);

    h.editor.set_autocomplete_max_visible(1.0);
    assert_eq!(h.editor.get_autocomplete_max_visible(), 3);
    h.editor.set_autocomplete_max_visible(f64::NAN);
    assert_eq!(h.editor.get_autocomplete_max_visible(), 5);
    h.editor.set_autocomplete_max_visible(7.9);
    assert_eq!(h.editor.get_autocomplete_max_visible(), 7);
}

#[test]
fn padding_x_is_normalized_and_triggers_render() {
    let mut h = Harness::new();
    let before = h._renders.get();
    h.editor.set_padding_x(3.9);
    assert_eq!(h.editor.get_padding_x(), 3);
    assert!(h._renders.get() > before);

    h.editor.set_padding_x(-5.0);
    assert_eq!(h.editor.get_padding_x(), 0);
    h.editor.set_padding_x(f64::INFINITY);
    assert_eq!(h.editor.get_padding_x(), 0);
}

#[test]
fn render_clamps_padding_to_half_the_width() {
    let mut h = Harness::with_options(
        10,
        24,
        EditorOptions {
            padding_x: Some(9.0),
            autocomplete_max_visible: None,
        },
    );
    h.editor.set_text("hello");
    let lines = h.render(10);
    // maxPadding = floor((10 - 1) / 2) = 4
    for line in &lines {
        assert_eq!(visible_width(line), 10);
    }
    assert!(lines[1].starts_with("    "));
}

#[test]
fn default_render_has_three_lines_for_single_line_text() {
    let mut h = Harness::new();
    h.editor.set_text("hello");
    let lines = h.render(20);
    assert_eq!(lines.len(), 3);
    assert_eq!(strip(&lines[0]), "─".repeat(20));
    assert_eq!(strip(&lines[2]), "─".repeat(20));
    assert_eq!(h.editor.rendered_visible_line_count(), 1);
    assert_eq!(h.editor.rendered_autocomplete_height(), 0);
}

#[test]
fn render_scrolls_to_keep_cursor_visible() {
    let mut h = Harness::with_size(80, 24);
    let text: Vec<String> = (0..30).map(|index| format!("line {index}")).collect();
    h.editor.set_text(&text.join("\n"));
    // 终端 24 行 → maxVisibleLines = max(5, floor(7.2)) = 7
    let lines = h.render(40);
    assert_eq!(content_lines(&lines).len(), 7);
    assert!(h.editor.scroll_offset() > 0);
    // 首行是顶边框（有隐藏行指示）。
    assert!(strip(&lines[0]).starts_with('─'));
    assert!(strip(&lines[0]).contains("↑"));
}

// ===========================================================================
// 额外：单字素超宽保护 / 归一化 / disableSubmit
// ===========================================================================

#[test]
fn word_wrap_single_wide_grapheme_wider_than_max_width_fails_like_ts() {
    // TS 在这里对自身无限递归（`RangeError: Maximum call stack size exceeded`）。
    // Rust 用同文案的 panic 复现（模块文档第 6 条）。
    for (line, width) in [("你", 1usize), ("😀", 1), ("\t", 2), ("a你b", 1)] {
        let result = std::panic::catch_unwind(|| word_wrap_line(line, width, None));
        let error = result.expect_err(&format!("line {line:?} width {width} should fail"));
        let message = error
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| error.downcast_ref::<&str>().copied())
            .unwrap_or("");
        assert!(
            message.contains("RangeError: Maximum call stack size exceeded"),
            "line {line:?} width {width}: {message}"
        );
    }

    // 不触发单字素分支时仍然正常切分（`a你b` 宽 2：`a`/`你`/`b`）。
    let chunks = word_wrap_line("a你b", 2, None);
    let reconstructed: String = chunks
        .iter()
        .map(|chunk| utf16_slice("a你b", chunk.start_index, chunk.end_index))
        .collect();
    assert_eq!(reconstructed, "a你b");
    assert_eq!(chunks.len(), 3);
}

#[test]
fn render_wide_text_at_tiny_widths_fails_like_ts() {
    // TS 实测（`new Editor(createTestTUI(), theme).setText("日本\n😀")`）：
    // 视口宽度 0/1/2 抛 `RangeError: Maximum call stack size exceeded`，3 起正常
    // （行数 5/5/4）。
    for width in 0..=2 {
        let mut h = Harness::new();
        h.editor.set_text("日本\n😀");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h.render(width)));
        assert!(result.is_err(), "width {width} should fail");
    }
    for (width, expected) in [(3usize, 5usize), (4, 5), (5, 4)] {
        let mut h = Harness::new();
        h.editor.set_text("日本\n😀");
        assert_eq!(h.render(width).len(), expected, "width {width}");
    }
}

#[test]
fn set_text_expands_tabs_to_four_spaces() {
    let mut h = Harness::new();
    h.editor.set_text("a\tb\r\nc\rd");
    assert_eq!(h.text(), "a    b\nc\nd");
}

#[test]
fn insert_text_at_cursor_expands_tabs_to_four_spaces() {
    let mut h = Harness::new();
    h.editor.insert_text_at_cursor("x\ty");
    assert_eq!(h.text(), "x    y");
}

#[test]
fn paste_expands_tabs_to_four_spaces() {
    let mut h = Harness::new();
    h.input("\x1b[200~a\tb\x1b[201~");
    assert_eq!(h.text(), "a    b");
}

#[test]
fn disable_submit_prevents_submission_but_allows_newlines() {
    let mut h = Harness::new();
    let submitted = Rc::new(Cell::new(false));
    let sink = Rc::clone(&submitted);
    h.editor.on_submit = Some(Box::new(move |_| sink.set(true)));
    h.editor.disable_submit = true;

    h.input("hi");
    h.input("\r");
    assert!(!submitted.get());
    assert_eq!(h.text(), "hi");

    // disableSubmit 时 Enter 在"反斜杠工作区"之前就返回（TS 同）：既不提交也不换行。
    h.input("\\");
    h.input("\r");
    assert!(!submitted.get());
    assert_eq!(h.text(), "hi\\");
}

#[test]
fn escape_cancels_autocomplete() {
    let mut h = Harness::new();
    h.editor
        .set_autocomplete_provider(Box::new(MockProvider::new(
            |lines, _line, col, _options| {
                let prefix = utf16_slice(lines.first().map(String::as_str).unwrap_or(""), 0, col);
                if prefix.starts_with('/') {
                    Some(suggestions(vec![item("/help")], &prefix))
                } else {
                    None
                }
            },
        )));
    h.input("/");
    flush(&mut h.editor);
    assert!(h.editor.is_showing_autocomplete());
    h.input("\x1b");
    assert!(!h.editor.is_showing_autocomplete());
    assert_eq!(h.text(), "/");
}

// ===========================================================================
// 额外：自动补全的串行化/在飞窗口回归（审阅发现 #1/#2/#3/#4/#5）
// ===========================================================================

use std::sync::atomic::AtomicBool;

use kiana_tui::editor_component::EditorComponent;

/// 手动放行的 provider 的共享观测状态。
#[derive(Clone, Default)]
struct GatedState {
    calls: Arc<Mutex<Vec<String>>>,
    gates: Arc<Mutex<Vec<Arc<AtomicBool>>>>,
    completions: Arc<AtomicUsize>,
}

impl GatedState {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn completions(&self) -> usize {
        self.completions.load(Ordering::SeqCst)
    }

    /// 放行此刻已有的请求；之后新建的请求仍是 pending（需要再次调用）。
    fn release(&self) {
        for gate in self.gates.lock().unwrap().iter() {
            gate.store(true, Ordering::SeqCst);
        }
    }
}

/// 请求只在 `release()` 之后才会 settle 的 provider（TS 测试里未 settle 的 Promise）。
struct GatedProvider {
    state: GatedState,
    items: Vec<AutocompleteItem>,
    allow_file_completion: bool,
}

impl GatedProvider {
    fn new(state: GatedState, items: Vec<AutocompleteItem>) -> Self {
        GatedProvider {
            state,
            items,
            allow_file_completion: true,
        }
    }

    fn without_file_completion(mut self) -> Self {
        self.allow_file_completion = false;
        self
    }
}

impl AutocompleteProvider for GatedProvider {
    fn get_suggestions<'a>(
        &'a mut self,
        lines: &'a [String],
        cursor_line: usize,
        cursor_col: usize,
        options: &'a GetSuggestionsOptions,
    ) -> Pin<Box<dyn Future<Output = Option<AutocompleteSuggestions>> + Send + 'a>> {
        let text = lines.get(cursor_line).cloned().unwrap_or_default();
        self.state
            .calls
            .lock()
            .unwrap()
            .push(format!("{cursor_line}:{cursor_col}:{}", options.force));
        let prefix = utf16_slice(&text, 0, cursor_col);
        let gate = Arc::new(AtomicBool::new(false));
        self.state.gates.lock().unwrap().push(Arc::clone(&gate));
        let result = suggestions(self.items.clone(), &prefix);
        Box::pin(async move {
            std::future::poll_fn(move |_| {
                if gate.load(Ordering::SeqCst) {
                    std::task::Poll::Ready(())
                } else {
                    std::task::Poll::Pending
                }
            })
            .await;
            Some(result)
        })
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> ApplyCompletionResult {
        self.state.completions.fetch_add(1, Ordering::SeqCst);
        apply_replacing_prefix(lines, cursor_line, cursor_col, &item.value, prefix)
    }

    fn should_trigger_file_completion(
        &self,
        _lines: &[String],
        _cursor_line: usize,
        _cursor_col: usize,
    ) -> bool {
        self.allow_file_completion
    }
}

/// 输入 `@a` 并让第一个请求进入在飞状态；`release` + `flush` 后候选菜单可见。
fn show_picker_then_start_pending_refresh(h: &mut Harness, clock: &FakeClock, state: &GatedState) {
    h.input("@");
    h.input("a");
    clock.advance(30.0);
    flush(&mut h.editor);
    assert_eq!(state.calls(), vec!["0:2:false".to_string()]);
    state.release();
    flush(&mut h.editor);
    assert!(h.editor.is_showing_autocomplete());

    // 再输入一个字符：刷新请求进入在飞窗口，旧菜单仍然可见。
    h.input("b");
    clock.advance(30.0);
    flush(&mut h.editor);
    assert!(h.editor.is_showing_autocomplete());
    assert_eq!(
        state.calls(),
        vec!["0:2:false".to_string(), "0:3:false".to_string()]
    );
}

#[test]
fn enter_while_autocomplete_refresh_is_in_flight_applies_completion_instead_of_submitting() {
    let mut h = Harness::new();
    let clock = with_fake_clock(&mut h);
    let state = GatedState::default();
    h.editor
        .set_autocomplete_provider(Box::new(GatedProvider::new(
            state.clone(),
            vec![item("CAND1"), item("CAND2")],
        )));
    let changes: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&changes);
    h.editor.on_change = Some(Box::new(move |text: &str| {
        sink.borrow_mut().push(text.to_string());
    }));
    let submitted: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&submitted);
    h.editor.on_submit = Some(Box::new(move |text: &str| {
        sink.borrow_mut().push(text.to_string());
    }));

    show_picker_then_start_pending_refresh(&mut h, &clock, &state);

    h.input("\r");
    assert_eq!(h.text(), "@CAND1");
    assert!(!h.editor.is_showing_autocomplete());
    assert!(submitted.borrow().is_empty(), "Enter 不应提交未补全的文本");
    assert_eq!(changes.borrow().last().map(String::as_str), Some("@CAND1"));
    assert_eq!(state.completions(), 1);
}

#[test]
fn tab_while_autocomplete_refresh_is_in_flight_applies_completion() {
    let mut h = Harness::new();
    let clock = with_fake_clock(&mut h);
    let state = GatedState::default();
    h.editor
        .set_autocomplete_provider(Box::new(GatedProvider::new(
            state.clone(),
            vec![item("CAND1"), item("CAND2")],
        )));
    let submitted: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&submitted);
    h.editor.on_submit = Some(Box::new(move |text: &str| {
        sink.borrow_mut().push(text.to_string());
    }));

    show_picker_then_start_pending_refresh(&mut h, &clock, &state);

    h.input("\t");
    assert_eq!(h.text(), "@CAND1");
    assert!(!h.editor.is_showing_autocomplete());
    assert!(submitted.borrow().is_empty());
    assert_eq!(state.completions(), 1);
}

#[test]
fn mouse_click_on_autocomplete_row_while_refresh_is_in_flight_applies_completion() {
    let mut h = Harness::new();
    let clock = with_fake_clock(&mut h);
    let state = GatedState::default();
    h.editor
        .set_autocomplete_provider(Box::new(GatedProvider::new(
            state.clone(),
            vec![item("CAND1"), item("CAND2")],
        )));

    show_picker_then_start_pending_refresh(&mut h, &clock, &state);
    h.render(40);
    let start_row = h.editor.rendered_visible_line_count() as isize + 2;
    let result = h.editor.handle_mouse(&click_event(0, start_row, 40));
    assert!(result.is_some());
    assert_eq!(h.text(), "@CAND1");
    assert_eq!(state.completions(), 1);
}

#[test]
fn autocomplete_coalesces_tab_presses_within_one_synchronous_batch() {
    let mut h = Harness::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&calls);
    h.editor
        .set_autocomplete_provider(Box::new(MockProvider::new(
            move |lines, _line, col, _options| {
                sink.fetch_add(1, Ordering::SeqCst);
                let prefix = utf16_slice(lines.first().map(String::as_str).unwrap_or(""), 0, col);
                Some(suggestions(vec![item("CAND1"), item("CAND2")], &prefix))
            },
        )));
    h.editor.set_text("x ");

    // 同一同步批次里的两次 Tab：TS 的请求链只让最后一个请求到达 provider。
    h.input("\t");
    h.input("\t");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    flush(&mut h.editor);

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(h.text(), "x ");
    assert!(h.editor.is_showing_autocomplete());
}

#[test]
fn autocomplete_coalesces_typing_burst_into_single_request() {
    // "/" 触发不带防抖的请求；同一同步批次后续字符只保留最后一个请求。
    let mut h = Harness::new();
    let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&calls);
    h.editor
        .set_autocomplete_provider(Box::new(MockProvider::new(
            move |_lines, line, col, _options| {
                sink.lock().unwrap().push(format!("{line}:{col}"));
                None
            },
        )));

    h.input("/");
    h.input("m");
    assert!(calls.lock().unwrap().is_empty());
    flush(&mut h.editor);

    assert_eq!(*calls.lock().unwrap(), vec!["0:2".to_string()]);
}

#[test]
fn declined_force_completion_keeps_in_flight_request_alive() {
    // TS：shouldTriggerFileCompletion() === false 时 requestAutocomplete 直接返回，
    // 在飞请求不被取消，其结果随后照常应用。
    let mut h = Harness::new();
    let clock = with_fake_clock(&mut h);
    let state = GatedState::default();
    h.editor.set_autocomplete_provider(Box::new(
        GatedProvider::new(state.clone(), vec![item("CAND1"), item("CAND2")])
            .without_file_completion(),
    ));

    h.input("@");
    h.input("a");
    clock.advance(30.0);
    flush(&mut h.editor);
    assert!(!h.editor.is_showing_autocomplete());
    assert_eq!(state.calls(), vec!["0:2:false".to_string()]);

    h.input("\t"); // force 被谓词拒绝
    assert!(!h.editor.is_showing_autocomplete());
    assert_eq!(state.calls().len(), 1);

    state.release();
    flush(&mut h.editor);
    assert!(h.editor.is_showing_autocomplete());
    assert_eq!(state.calls().len(), 1, "拒绝的 force 请求不应调用 provider");
}

#[test]
fn force_completion_during_in_flight_request_supersedes_its_result() {
    let mut h = Harness::new();
    let clock = with_fake_clock(&mut h);
    let state = GatedState::default();
    h.editor
        .set_autocomplete_provider(Box::new(GatedProvider::new(
            state.clone(),
            vec![item("CAND1"), item("CAND2")],
        )));

    h.input("@");
    h.input("a");
    clock.advance(30.0);
    flush(&mut h.editor);
    assert_eq!(state.calls(), vec!["0:2:false".to_string()]);

    h.input("\t"); // force：排队等待在飞请求结束
    state.release();
    flush(&mut h.editor);

    // 在飞请求的结果被丢弃（TS 在请求时已 abort 它），新的 force 请求已启动。
    assert!(!h.editor.is_showing_autocomplete());
    assert_eq!(
        state.calls(),
        vec!["0:2:false".to_string(), "0:2:true".to_string()]
    );

    state.release();
    flush(&mut h.editor);
    assert!(h.editor.is_showing_autocomplete());
    assert_eq!(state.calls().len(), 2);
}

#[test]
fn is_showing_autocomplete_is_a_pure_query() {
    let mut h = Harness::new();
    let clock = with_fake_clock(&mut h);
    let calls = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&calls);
    h.editor
        .set_autocomplete_provider(Box::new(MockProvider::new(
            move |_lines, _line, _col, _options| {
                sink.fetch_add(1, Ordering::SeqCst);
                None
            },
        )));

    h.input("@");
    h.input("a");
    clock.advance(30.0); // 防抖已到期，但 getter 不应推进请求
    assert!(!h.editor.is_showing_autocomplete());
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    flush(&mut h.editor);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn editor_can_be_used_as_editor_component_interface() {
    let terminal: Rc<RefCell<Box<dyn Terminal>>> =
        Rc::new(RefCell::new(Box::new(TestTerminal::with_size(80, 24))));
    let editor = Editor::new(
        EditorTui::new(terminal, Rc::new(|| {})),
        EditorTheme::plain(),
        EditorOptions::default(),
    );
    let mut editor: Box<dyn EditorComponent> = Box::new(editor);

    editor.set_text("hello");
    assert_eq!(editor.get_text(), "hello");
    editor.insert_text_at_cursor("!");
    assert_eq!(editor.get_text(), "hello!");
    editor.set_padding_x(2);
    editor.set_autocomplete_max_visible(9);
    let changed = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&changed);
    editor.set_on_change(Some(Box::new(move |text: &str| {
        sink.borrow_mut().push(text.to_string());
    })));
    editor.set_on_submit(Some(Box::new(|_text: &str| {})));
    editor.add_to_history("hello!");
    assert_eq!(editor.get_expanded_text(), "hello!");

    // 类型擦除的 provider 载荷：`Box<Box<dyn AutocompleteProvider>>`。
    let calls = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&calls);
    editor.set_autocomplete_provider(Box::new(Box::new(MockProvider::new(
        move |lines, _line, col, _options| {
            sink.fetch_add(1, Ordering::SeqCst);
            let prefix = utf16_slice(lines.first().map(String::as_str).unwrap_or(""), 0, col);
            Some(suggestions(vec![item("CAND1")], &prefix))
        },
    )) as Box<dyn AutocompleteProvider>));

    // borderColor：Some 覆盖、None 恢复恒等。
    editor.set_border_color(Some(Box::new(|text: &str| format!("<{text}>"))));
    let rendered = editor.render(40);
    assert!(rendered.first().unwrap().starts_with('<'));

    editor.handle_input("\t");
    let _ = editor.render(40);
    // mock 的 applyCompletion 用前缀（整行 "hello!"）替换为候选值。
    assert_eq!(editor.get_text(), "CAND1");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!changed.borrow().is_empty());

    editor.set_border_color(None);
    let rendered = editor.render(40);
    assert!(!rendered.first().unwrap().starts_with('<'));
}

/// 定时器待触发回调的槽位。
type TimerCallbackSlot = Rc<RefCell<Option<Box<dyn FnOnce()>>>>;

/// 可手动触发的定时器替身（TS 测试里的 `t.mock.timers`）。
#[derive(Clone, Default)]
struct FakeTimerState {
    delays: Rc<RefCell<Vec<u64>>>,
    pending: TimerCallbackSlot,
}

impl FakeTimerState {
    fn delays(&self) -> Vec<u64> {
        self.delays.borrow().clone()
    }

    fn has_pending(&self) -> bool {
        self.pending.borrow().is_some()
    }

    fn fire(&self) {
        let callback = self.pending.borrow_mut().take();
        if let Some(callback) = callback {
            callback();
        }
    }
}

struct FakeTimer(FakeTimerState);

impl kiana_tui::tui::Timer for FakeTimer {
    fn set(&mut self, delay_ms: u64, callback: Box<dyn FnOnce()>) {
        self.0.delays.borrow_mut().push(delay_ms);
        *self.0.pending.borrow_mut() = Some(callback);
    }

    fn cancel(&mut self) {
        *self.0.pending.borrow_mut() = None;
    }
}

#[test]
fn debounce_timer_requests_render_when_it_fires() {
    let mut h = Harness::new();
    let clock = with_fake_clock(&mut h);
    let timer = FakeTimerState::default();
    h.editor.set_timer(Box::new(FakeTimer(timer.clone())));
    let calls = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&calls);
    h.editor
        .set_autocomplete_provider(Box::new(MockProvider::new(
            move |_lines, _line, _col, _options| {
                sink.fetch_add(1, Ordering::SeqCst);
                None
            },
        )));

    h.input("@");
    h.input("a");
    assert_eq!(timer.delays(), vec![20, 20]);
    assert!(timer.has_pending());

    clock.advance(20.0);
    let renders_before = h._renders.get();
    timer.fire();
    assert_eq!(
        h._renders.get(),
        renders_before + 1,
        "到期的防抖定时器应请求一次重绘（TS 的 setTimeout）"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0, "定时器本身不调用 provider");

    // 宿主响应重绘请求渲染一次：pump 启动到期的请求。
    h.render(40);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!timer.has_pending(), "到期后定时器不应再次触发");
}

#[test]
fn new_request_cancels_pending_debounce_timer() {
    let mut h = Harness::new();
    let timer = FakeTimerState::default();
    h.editor.set_timer(Box::new(FakeTimer(timer.clone())));
    h.editor
        .set_autocomplete_provider(Box::new(MockProvider::new(
            |_lines, _line, _col, _options| None,
        )));

    h.input("@");
    assert!(timer.has_pending());
    h.input("a");
    assert!(timer.has_pending());
    assert_eq!(timer.delays(), vec![20, 20]);

    // 立即请求（Tab → force，无防抖）会取消尚未触发的防抖定时器。
    h.input("\t");
    assert!(!timer.has_pending());
}

// ---------------------------------------------------------------------------
// 评审回归：单字素宽于 maxWidth 时与 TS 一样失败（RangeError: Maximum call stack size exceeded）
// ---------------------------------------------------------------------------

/// TS 的 `wordWrapLine("中", 1)` 在"超宽原子段"分支里对自身无限递归，抛
/// `RangeError: Maximum call stack size exceeded`；Rust 用同文案的 panic 复现。
#[test]
#[should_panic(expected = "RangeError: Maximum call stack size exceeded")]
fn word_wrap_line_single_grapheme_wider_than_max_width_fails_like_ts() {
    let _ = word_wrap_line("中", 1, None);
}

/// `Editor::render(2)`（内容宽度 1）里的宽字符同样失败。
#[test]
#[should_panic(expected = "RangeError: Maximum call stack size exceeded")]
fn editor_render_at_content_width_one_with_wide_char_fails_like_ts() {
    let mut harness = Harness::with_size(2, 4);
    harness.editor.set_text("中");
    let _ = harness.editor.render(2);
}

// ---------------------------------------------------------------------------
// 评审回归（major #1）：Ctrl+Right 的词边界取自 Node ICU 78.3 的 `Intl.Segmenter`
// ---------------------------------------------------------------------------

/// `ひらがな` 的 Node 分段是 `ひ`/`ら`/`が`/`な`：Ctrl+A 后两次 Ctrl+Right 落在 1、2 列。
#[test]
fn unicode_ctrl_right_uses_node_hiragana_segments() {
    let mut h = Harness::new();
    h.editor.set_text("ひらがな");
    h.input("\x01");
    assert_eq!(h.cursor(), (0, 0));
    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (0, 1));
    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (0, 2));
}

/// `中文字` 的 Node 分段是 `中`/`文字`：第一次 Ctrl+Right 落在第 1 列。
#[test]
fn unicode_ctrl_right_uses_node_chinese_segments() {
    let mut h = Harness::new();
    h.editor.set_text("中文字");
    h.input("\x01");
    h.input("\x1b[1;5C");
    assert_eq!(h.cursor(), (0, 1));
}
