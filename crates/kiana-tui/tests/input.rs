//! `test/input.test.ts` 的 Rust 移植。

use std::cell::RefCell;
use std::rc::Rc;

use kiana_tui::components::input::InputOptions;
use kiana_tui::{Component, Focusable, Input, strip_terminal_sequences, visible_width};

fn new_input() -> Input {
    Input::new(InputOptions::default())
}

#[test]
fn submits_value_including_backslash_on_enter() {
    let mut input = new_input();
    let submitted: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&submitted);
    input.on_submit = Some(Box::new(move |value: &str| {
        *sink.borrow_mut() = Some(value.to_string());
    }));

    // 输入 hello、反斜杠，然后 Enter。
    for character in ["h", "e", "l", "l", "o", "\\"] {
        input.handle_input(character);
    }
    input.handle_input("\r");

    // 单行输入，没有反斜杠 + Enter 的特殊处理。
    assert_eq!(*submitted.borrow(), Some("hello\\".to_string()));
}

#[test]
fn inserts_backslash_as_regular_character() {
    let mut input = new_input();

    input.handle_input("\\");
    input.handle_input("x");

    assert_eq!(input.get_value(), "\\x");
}

// ============================================================================
// render
// ============================================================================

#[test]
fn supports_a_custom_prompt_and_styled_placeholder() {
    let mut input = Input::new(InputOptions {
        prompt: Some(String::new()),
        placeholder: Some("Find transcript".to_string()),
        placeholder_style: Some(std::sync::Arc::new(|text: &str| {
            format!("\x1b[2m{text}\x1b[22m")
        })),
    });
    input.set_focused(true);

    let empty = input.render(20);
    assert!(empty[0].contains("\x1b[2m"));
    assert_eq!(
        strip_terminal_sequences(&empty[0]).trim_end(),
        "Find transcript"
    );

    input.handle_input("n");
    let populated = input.render(20);
    assert_eq!(strip_terminal_sequences(&populated[0]).trim_end(), "n");
}

#[test]
fn does_not_overflow_with_wide_cjk_and_fullwidth_text() {
    let width = 93;
    let cases = [
        "가나다라마바사아자차카타파하 한글 텍스트가 터미널 너비를 초과하면 크래시가 발생합니다 이것은 재현용 테스트입니다",
        "これはテスト文章です。日本語のテキストが正しく表示されるかどうかを確認するためのサンプルテキストです。あいうえお",
        "这是一段测试文本，用于验证中文字符在终端中的显示宽度是否被正确计算，如果不正确就会导致用户界面崩溃的问题",
        "ＡＢＣＤＥＦＧＨＩＪＫＬＭＮＯＰＱＲＳＴＵＶＷＸＹＺ０１２３４５６７８９ａｂｃｄｅｆｇｈｉｊｋｌｍ",
    ];
    let cursor_positions = ["start", "middle", "end"];

    for text in cases {
        for position in cursor_positions {
            let mut input = new_input();
            input.set_value(text);
            input.set_focused(true);
            match position {
                "start" => {}
                "middle" => {
                    for _ in 0..10 {
                        input.handle_input("\x1b[C");
                    }
                }
                "end" => input.handle_input("\x05"),
                _ => unreachable!(),
            }

            let line = &input.render(width)[0];
            assert!(
                visible_width(line) <= width,
                "rendered line overflowed for {text} at {position}"
            );
        }
    }
}

#[test]
fn keeps_the_cursor_visible_when_horizontally_scrolling_wide_text() {
    let mut input = new_input();
    let width = 20;
    let text = "가나다라마바사아자차카타파하";
    input.set_value(text);
    input.set_focused(true);
    input.handle_input("\x01");
    for _ in 0..5 {
        input.handle_input("\x1b[C");
    }

    let line = &input.render(width)[0];
    assert!(visible_width(line) <= width);
}

// ============================================================================
// Kill ring
// ============================================================================

#[test]
fn ctrl_w_saves_deleted_text_to_kill_ring_and_ctrl_y_yanks_it() {
    let mut input = new_input();

    input.set_value("foo bar baz");
    input.handle_input("\x05"); // Ctrl+E - 移到末尾

    input.handle_input("\x17"); // Ctrl+W - 删除 "baz"
    assert_eq!(input.get_value(), "foo bar ");

    input.handle_input("\x01"); // Ctrl+A - 移到开头
    input.handle_input("\x19"); // Ctrl+Y
    assert_eq!(input.get_value(), "bazfoo bar ");
}

#[test]
fn ctrl_w_preserves_ascii_punctuation_boundaries() {
    let mut input = new_input();

    input.set_value("foo.bar");
    input.handle_input("\x05");
    input.handle_input("\x17"); // 删除 "bar"
    assert_eq!(input.get_value(), "foo.");

    input.set_value("foo:bar");
    input.handle_input("\x05");
    input.handle_input("\x17"); // 删除 "bar"
    assert_eq!(input.get_value(), "foo:");
}

#[test]
fn ctrl_w_handles_unicode_word_boundaries() {
    let mut input = new_input();

    // "你好世界。你好，世界" 分段：你好|世界|。|你好|，|世界
    input.set_value("你好世界。你好，世界");
    input.handle_input("\x05");
    input.handle_input("\x17"); // 删除 "世界"
    assert_eq!(input.get_value(), "你好世界。你好，");
    input.handle_input("\x17"); // 删除 "，"
    assert_eq!(input.get_value(), "你好世界。你好");
    input.handle_input("\x17"); // 删除 "你好"
    assert_eq!(input.get_value(), "你好世界。");
    input.handle_input("\x17"); // 删除 "。"
    assert_eq!(input.get_value(), "你好世界");
    input.handle_input("\x17"); // 删除 "世界"
    assert_eq!(input.get_value(), "你好");
    input.handle_input("\x17"); // 删除 "你好"
    assert_eq!(input.get_value(), "");
}

#[test]
fn ctrl_u_saves_deleted_text_to_kill_ring() {
    let mut input = new_input();

    input.set_value("hello world");
    input.handle_input("\x01");
    for _ in 0..6 {
        input.handle_input("\x1b[C");
    }

    input.handle_input("\x15"); // Ctrl+U - 删除 "hello "
    assert_eq!(input.get_value(), "world");

    input.handle_input("\x19"); // Ctrl+Y
    assert_eq!(input.get_value(), "hello world");
}

#[test]
fn ctrl_k_saves_deleted_text_to_kill_ring() {
    let mut input = new_input();

    input.set_value("hello world");
    input.handle_input("\x01");
    input.handle_input("\x0b"); // Ctrl+K

    assert_eq!(input.get_value(), "");

    input.handle_input("\x19"); // Ctrl+Y
    assert_eq!(input.get_value(), "hello world");
}

#[test]
fn ctrl_y_does_nothing_when_kill_ring_is_empty() {
    let mut input = new_input();

    input.set_value("test");
    input.handle_input("\x05");
    input.handle_input("\x19");
    assert_eq!(input.get_value(), "test");
}

#[test]
fn alt_y_cycles_through_kill_ring_after_ctrl_y() {
    let mut input = new_input();

    for word in ["first", "second", "third"] {
        input.set_value(word);
        input.handle_input("\x05");
        input.handle_input("\x17");
    }

    assert_eq!(input.get_value(), "");

    input.handle_input("\x19"); // 取出 "third"
    assert_eq!(input.get_value(), "third");

    input.handle_input("\x1by"); // 轮换到 "second"
    assert_eq!(input.get_value(), "second");

    input.handle_input("\x1by"); // 轮换到 "first"
    assert_eq!(input.get_value(), "first");

    input.handle_input("\x1by"); // 轮回到 "third"
    assert_eq!(input.get_value(), "third");
}

#[test]
fn alt_y_does_nothing_if_not_preceded_by_yank() {
    let mut input = new_input();

    input.set_value("test");
    input.handle_input("\x05");
    input.handle_input("\x17");
    input.set_value("other");
    input.handle_input("\x05");

    // 输入字符打断 yank 链。
    input.handle_input("x");
    assert_eq!(input.get_value(), "otherx");

    input.handle_input("\x1by");
    assert_eq!(input.get_value(), "otherx");
}

#[test]
fn alt_y_does_nothing_if_kill_ring_has_one_entry() {
    let mut input = new_input();

    input.set_value("only");
    input.handle_input("\x05");
    input.handle_input("\x17");

    input.handle_input("\x19");
    assert_eq!(input.get_value(), "only");

    input.handle_input("\x1by");
    assert_eq!(input.get_value(), "only");
}

#[test]
fn consecutive_ctrl_w_accumulates_into_one_kill_ring_entry() {
    let mut input = new_input();

    input.set_value("one two three");
    input.handle_input("\x05");
    input.handle_input("\x17");
    input.handle_input("\x17");
    input.handle_input("\x17");

    assert_eq!(input.get_value(), "");

    input.handle_input("\x19");
    assert_eq!(input.get_value(), "one two three");
}

#[test]
fn non_delete_actions_break_kill_accumulation() {
    let mut input = new_input();

    input.set_value("foo bar baz");
    input.handle_input("\x05");
    input.handle_input("\x17");
    assert_eq!(input.get_value(), "foo bar ");

    input.handle_input("x"); // 打字打断累积
    assert_eq!(input.get_value(), "foo bar x");

    input.handle_input("\x17"); // 删除 "x"（独立条目）
    assert_eq!(input.get_value(), "foo bar ");

    input.handle_input("\x19"); // 最近的是 "x"
    assert_eq!(input.get_value(), "foo bar x");

    input.handle_input("\x1by"); // 轮换到 "baz"
    assert_eq!(input.get_value(), "foo bar baz");
}

#[test]
fn non_yank_actions_break_alt_y_chain() {
    let mut input = new_input();

    input.set_value("first");
    input.handle_input("\x05");
    input.handle_input("\x17");
    input.set_value("second");
    input.handle_input("\x05");
    input.handle_input("\x17");
    input.set_value("");

    input.handle_input("\x19"); // 取出 "second"
    assert_eq!(input.get_value(), "second");

    input.handle_input("x"); // 打断 yank 链
    assert_eq!(input.get_value(), "secondx");

    input.handle_input("\x1by");
    assert_eq!(input.get_value(), "secondx");
}

#[test]
fn kill_ring_rotation_persists_after_cycling() {
    let mut input = new_input();

    for word in ["first", "second", "third"] {
        input.set_value(word);
        input.handle_input("\x05");
        input.handle_input("\x17");
    }
    input.set_value("");

    input.handle_input("\x19"); // 取出 "third"
    input.handle_input("\x1by"); // 轮换到 "second"
    assert_eq!(input.get_value(), "second");

    // 打断链条后重新开始。
    input.handle_input("x");
    input.set_value("");

    // 新的 yank 得到 "second"（轮换后在环顶）。
    input.handle_input("\x19");
    assert_eq!(input.get_value(), "second");
}

#[test]
fn backward_deletions_prepend_forward_deletions_append_during_accumulation() {
    let mut input = new_input();

    input.set_value("prefix|suffix");
    input.handle_input("\x01");
    for _ in 0..6 {
        input.handle_input("\x1b[C");
    }

    input.handle_input("\x0b"); // Ctrl+K - 删除 "|suffix"（向前）
    assert_eq!(input.get_value(), "prefix");

    input.handle_input("\x19");
    assert_eq!(input.get_value(), "prefix|suffix");
}

#[test]
fn alt_d_deletes_word_forward_and_saves_to_kill_ring() {
    let mut input = new_input();

    input.set_value("hello world test");
    input.handle_input("\x01");

    input.handle_input("\x1bd"); // 删除 "hello"
    assert_eq!(input.get_value(), " world test");

    input.handle_input("\x1bd"); // 删除 " world"
    assert_eq!(input.get_value(), " test");

    input.handle_input("\x19");
    assert_eq!(input.get_value(), "hello world test");
}

#[test]
fn alt_d_preserves_ascii_punctuation_boundaries() {
    let mut input = new_input();

    input.set_value("foo.bar baz");
    input.handle_input("\x01");
    input.handle_input("\x1bd"); // 删除 "foo"
    assert_eq!(input.get_value(), ".bar baz");
    input.handle_input("\x1bd"); // 删除 "."
    assert_eq!(input.get_value(), "bar baz");
    input.handle_input("\x1bd"); // 删除 "bar"
    assert_eq!(input.get_value(), " baz");
}

#[test]
fn alt_d_handles_unicode_word_boundaries() {
    let mut input = new_input();

    input.set_value("你好世界。你好，世界");
    input.handle_input("\x01");
    input.handle_input("\x1bd"); // 删除 "你好"
    assert_eq!(input.get_value(), "世界。你好，世界");
    input.handle_input("\x1bd"); // 删除 "世界"
    assert_eq!(input.get_value(), "。你好，世界");
    input.handle_input("\x1bd"); // 删除 "。"
    assert_eq!(input.get_value(), "你好，世界");
    input.handle_input("\x1bd"); // 删除 "你好"
    assert_eq!(input.get_value(), "，世界");
    input.handle_input("\x1bd"); // 删除 "，"
    assert_eq!(input.get_value(), "世界");
    input.handle_input("\x1bd"); // 删除 "世界"
    assert_eq!(input.get_value(), "");
}

#[test]
fn handles_yank_in_middle_of_text() {
    let mut input = new_input();

    input.set_value("word");
    input.handle_input("\x05");
    input.handle_input("\x17");
    input.set_value("hello world");
    input.handle_input("\x01");
    for _ in 0..6 {
        input.handle_input("\x1b[C");
    }

    input.handle_input("\x19");
    assert_eq!(input.get_value(), "hello wordworld");
}

#[test]
fn handles_yank_pop_in_middle_of_text() {
    let mut input = new_input();

    input.set_value("FIRST");
    input.handle_input("\x05");
    input.handle_input("\x17");
    input.set_value("SECOND");
    input.handle_input("\x05");
    input.handle_input("\x17");

    input.set_value("hello world");
    input.handle_input("\x01");
    for _ in 0..6 {
        input.handle_input("\x1b[C");
    }

    input.handle_input("\x19"); // 取出 "SECOND"
    assert_eq!(input.get_value(), "hello SECONDworld");

    input.handle_input("\x1by"); // 替换成 "FIRST"
    assert_eq!(input.get_value(), "hello FIRSTworld");
}

// ============================================================================
// Undo
// ============================================================================

#[test]
fn does_nothing_when_undo_stack_is_empty() {
    let mut input = new_input();

    input.handle_input("\x1b[45;5u"); // Ctrl+-
    assert_eq!(input.get_value(), "");
}

#[test]
fn coalesces_consecutive_word_characters_into_one_undo_unit() {
    let mut input = new_input();

    for character in "hello world".chars() {
        input.handle_input(&character.to_string());
    }
    assert_eq!(input.get_value(), "hello world");

    input.handle_input("\x1b[45;5u"); // 撤销 " world"
    assert_eq!(input.get_value(), "hello");

    input.handle_input("\x1b[45;5u"); // 撤销 "hello"
    assert_eq!(input.get_value(), "");
}

#[test]
fn undoes_spaces_one_at_a_time() {
    let mut input = new_input();

    for character in "hello  ".chars() {
        input.handle_input(&character.to_string());
    }
    assert_eq!(input.get_value(), "hello  ");

    input.handle_input("\x1b[45;5u");
    assert_eq!(input.get_value(), "hello ");

    input.handle_input("\x1b[45;5u");
    assert_eq!(input.get_value(), "hello");

    input.handle_input("\x1b[45;5u");
    assert_eq!(input.get_value(), "");
}

#[test]
fn undoes_backspace() {
    let mut input = new_input();

    for character in "hello".chars() {
        input.handle_input(&character.to_string());
    }
    input.handle_input("\x7f"); // Backspace
    assert_eq!(input.get_value(), "hell");

    input.handle_input("\x1b[45;5u");
    assert_eq!(input.get_value(), "hello");
}

#[test]
fn undoes_forward_delete() {
    let mut input = new_input();

    for character in "hello".chars() {
        input.handle_input(&character.to_string());
    }
    input.handle_input("\x01");
    input.handle_input("\x1b[C");
    input.handle_input("\x1b[3~"); // Delete
    assert_eq!(input.get_value(), "hllo");

    input.handle_input("\x1b[45;5u");
    assert_eq!(input.get_value(), "hello");
}

#[test]
fn undoes_ctrl_w_delete_word_backward() {
    let mut input = new_input();

    for character in "hello world".chars() {
        input.handle_input(&character.to_string());
    }
    assert_eq!(input.get_value(), "hello world");

    input.handle_input("\x17");
    assert_eq!(input.get_value(), "hello ");

    input.handle_input("\x1b[45;5u");
    assert_eq!(input.get_value(), "hello world");
}

#[test]
fn undoes_ctrl_k_delete_to_line_end() {
    let mut input = new_input();

    for character in "hello world".chars() {
        input.handle_input(&character.to_string());
    }
    input.handle_input("\x01");
    for _ in 0..6 {
        input.handle_input("\x1b[C");
    }

    input.handle_input("\x0b");
    assert_eq!(input.get_value(), "hello ");

    input.handle_input("\x1b[45;5u");
    assert_eq!(input.get_value(), "hello world");
}

#[test]
fn undoes_ctrl_u_delete_to_line_start() {
    let mut input = new_input();

    for character in "hello world".chars() {
        input.handle_input(&character.to_string());
    }
    input.handle_input("\x01");
    for _ in 0..6 {
        input.handle_input("\x1b[C");
    }

    input.handle_input("\x15");
    assert_eq!(input.get_value(), "world");

    input.handle_input("\x1b[45;5u");
    assert_eq!(input.get_value(), "hello world");
}

#[test]
fn undoes_yank() {
    let mut input = new_input();

    for character in "hello ".chars() {
        input.handle_input(&character.to_string());
    }
    input.handle_input("\x17"); // 删除 "hello "
    input.handle_input("\x19"); // 取出
    assert_eq!(input.get_value(), "hello ");

    input.handle_input("\x1b[45;5u");
    assert_eq!(input.get_value(), "");
}

#[test]
fn undoes_paste_atomically() {
    let mut input = new_input();

    input.set_value("hello world");
    input.handle_input("\x01");
    for _ in 0..5 {
        input.handle_input("\x1b[C");
    }

    // 模拟括号粘贴。
    input.handle_input("\x1b[200~beep boop\x1b[201~");
    assert_eq!(input.get_value(), "hellobeep boop world");

    // 一次撤销恢复整个粘贴前的状态。
    input.handle_input("\x1b[45;5u");
    assert_eq!(input.get_value(), "hello world");
}

#[test]
fn undoes_alt_d_delete_word_forward() {
    let mut input = new_input();

    input.set_value("hello world");
    input.handle_input("\x01");

    input.handle_input("\x1bd"); // 删除 "hello"
    assert_eq!(input.get_value(), " world");

    input.handle_input("\x1b[45;5u");
    assert_eq!(input.get_value(), "hello world");
}

#[test]
fn cursor_movement_starts_new_undo_unit() {
    let mut input = new_input();

    for character in "abc".chars() {
        input.handle_input(&character.to_string());
    }
    input.handle_input("\x01"); // 移动打断合并
    input.handle_input("\x05");
    for character in "de".chars() {
        input.handle_input(&character.to_string());
    }
    assert_eq!(input.get_value(), "abcde");

    // 撤销删除 "de"。
    input.handle_input("\x1b[45;5u");
    assert_eq!(input.get_value(), "abc");

    // 撤销删除 "abc"。
    input.handle_input("\x1b[45;5u");
    assert_eq!(input.get_value(), "");
}

// ---------------------------------------------------------------------------
// test/mouse-components.test.ts
// ---------------------------------------------------------------------------

use kiana_tui::{TuiMouseButton, TuiMouseEvent, TuiMouseEventType};

fn mouse(
    event_type: TuiMouseEventType,
    x: isize,
    y: isize,
    width: usize,
    height: usize,
) -> TuiMouseEvent {
    TuiMouseEvent {
        event_type,
        button: TuiMouseButton::Left,
        x,
        y,
        screen_x: x,
        screen_y: y,
        width,
        height,
        shift: false,
        alt: false,
        ctrl: false,
        wheel_delta: None,
        click_count: (event_type == TuiMouseEventType::Click).then_some(1),
    }
}

#[test]
fn positions_a_single_line_input_cursor_on_press() {
    let mut input = new_input();
    input.set_value("hello");
    input.render(20);

    let result = input.handle_mouse(&mouse(TuiMouseEventType::Press, 4, 0, 20, 1));
    assert_eq!(result.and_then(|result| result.handled), Some(true));
    input.handle_input("X");
    assert_eq!(input.get_value(), "heXllo");
}

// ============================================================================
// 回归（minor）：`yankPop` 的负下标语义
//
// TS 直接写 `value.slice(0, this.cursor - prevText.length)` / `value.slice(this.cursor)`；
// 负下标在 JS 里表示"从末尾倒数"。`setValue()` 不重置 `lastAction`，所以缩短值之后
// Alt+Y 仍会进入 yankPop。Rust 旧实现用 `saturating_sub` 把负下标夹到 0，光标落在行尾
// （文本也可能不同）。以下两个探针的期望值由 TS 参考实现生成。
// ============================================================================

fn fill_kill_ring_with_first_and_second(input: &mut Input) {
    for word in ["first", "second"] {
        input.set_value(word);
        input.handle_input("\x05"); // Ctrl+E
        input.handle_input("\x17"); // Ctrl+W
    }
}

#[test]
fn yank_pop_handles_a_negative_index_after_set_value_shortens_the_value() {
    let mut input = new_input();
    fill_kill_ring_with_first_and_second(&mut input);

    input.handle_input("\x19"); // Ctrl+Y -> "second"
    assert_eq!(input.get_value(), "second");
    input.set_value("x");
    assert_eq!(input.cursor(), 1);

    input.handle_input("\x1by"); // Alt+Y
    assert_eq!(input.get_value(), "first");
    assert_eq!(input.cursor(), 0);

    input.handle_input("Z");
    assert_eq!(input.get_value(), "Zfirst");
}

#[test]
fn yank_pop_counts_a_negative_slice_end_from_the_end_of_the_value() {
    let mut input = new_input();
    fill_kill_ring_with_first_and_second(&mut input);

    input.handle_input("\x19"); // Ctrl+Y -> "second"
    input.set_value("abcd");
    assert_eq!(input.cursor(), 4);

    input.handle_input("\x1by"); // Alt+Y
    // TS: slice(0, 4-6=-2) = "ab"; slice(4) = ""；插入 "first" 时 slice(0,-2) 仍是开头。
    assert_eq!(input.get_value(), "firstab");
    assert_eq!(input.cursor(), 3);

    input.handle_input("Z");
    assert_eq!(input.get_value(), "firZstab");
    assert_eq!(input.cursor(), 4);
}
