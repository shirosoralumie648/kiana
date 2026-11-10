//! `test/word-navigation.test.ts`：按词移动光标（`Intl.Segmenter` 语义）。
//!
//! TS 源：`packages/tui/test/word-navigation.test.ts`（19 个用例，标题保留在测试函数文档中以便对照）。
//! 光标位置一律是 **UTF-16** 偏移，与 TS 的 `string.length` / `slice` 同单位。

use kiana_tui::text::{WordNavigationOptions, WordSegment, find_word_backward, find_word_forward};

// ---------------------------------------------------------------------------
// describe("findWordBackward")
// ---------------------------------------------------------------------------

#[test]
fn backward_basic_words_hello_world() {
    let text = "hello world";
    assert_eq!(find_word_backward(text, 11, None), 6);
    assert_eq!(find_word_backward(text, 6, None), 0);
}

#[test]
fn backward_dotted_foo_bar() {
    let text = "foo.bar";
    assert_eq!(find_word_backward(text, 7, None), 4);
    assert_eq!(find_word_backward(text, 4, None), 3);
    assert_eq!(find_word_backward(text, 3, None), 0);
}

#[test]
fn backward_colon_foo_bar() {
    let text = "foo:bar";
    assert_eq!(find_word_backward(text, 7, None), 4);
    assert_eq!(find_word_backward(text, 4, None), 3);
    assert_eq!(find_word_backward(text, 3, None), 0);
}

#[test]
fn backward_path_path_to_file() {
    let text = "path/to/file";
    assert_eq!(find_word_backward(text, 12, None), 8);
    assert_eq!(find_word_backward(text, 8, None), 7);
    // "/to" 是一个 word-like 段，"/" 是段内的标点边界
    assert_eq!(find_word_backward(text, 7, None), 5);
    assert_eq!(find_word_backward(text, 5, None), 4);
    assert_eq!(find_word_backward(text, 4, None), 0);
}

#[test]
fn backward_cjk_mixed() {
    let text = "你好世界 test";
    assert_eq!(
        find_word_backward(text, text.encode_utf16().count(), None),
        5
    );
    // Intl.Segmenter 把每个 CJK 字符视为独立的 word-like 段
    assert_eq!(find_word_backward(text, 5, None), 2);
    assert_eq!(find_word_backward(text, 2, None), 0);
}

#[test]
fn backward_whitespace_at_boundaries() {
    let text = "  hello  ";
    assert_eq!(find_word_backward(text, 9, None), 2);
    assert_eq!(find_word_backward(text, 2, None), 0);
}

#[test]
fn backward_punctuation_run_foo_bar() {
    let text = "foo...bar";
    assert_eq!(find_word_backward(text, 9, None), 6);
    assert_eq!(find_word_backward(text, 6, None), 3);
    assert_eq!(find_word_backward(text, 3, None), 0);
}

#[test]
fn backward_cursor_at_0_returns_0() {
    assert_eq!(find_word_backward("hello", 0, None), 0);
}

// ---------------------------------------------------------------------------
// describe("findWordForward")
// ---------------------------------------------------------------------------

#[test]
fn forward_basic_words_hello_world() {
    let text = "hello world";
    assert_eq!(find_word_forward(text, 0, None), 5);
    assert_eq!(find_word_forward(text, 5, None), 11);
}

#[test]
fn forward_dotted_foo_bar() {
    let text = "foo.bar";
    assert_eq!(find_word_forward(text, 0, None), 3);
    assert_eq!(find_word_forward(text, 3, None), 4);
    assert_eq!(find_word_forward(text, 4, None), 7);
}

#[test]
fn forward_colon_foo_bar() {
    let text = "foo:bar";
    assert_eq!(find_word_forward(text, 0, None), 3);
    assert_eq!(find_word_forward(text, 3, None), 4);
    assert_eq!(find_word_forward(text, 4, None), 7);
}

#[test]
fn forward_path_path_to_file() {
    let text = "path/to/file";
    assert_eq!(find_word_forward(text, 0, None), 4);
    assert_eq!(find_word_forward(text, 4, None), 5);
    assert_eq!(find_word_forward(text, 5, None), 7);
    assert_eq!(find_word_forward(text, 7, None), 8);
    assert_eq!(find_word_forward(text, 8, None), 12);
}

#[test]
fn forward_cjk_mixed() {
    let text = "你好世界 test";
    let text_len = text.encode_utf16().count();
    let first_end = find_word_forward(text, 0, None);
    assert!(first_end > 0);
    assert!(first_end <= 4);
    // 一路走到末尾
    let mut pos = 0;
    while pos < text_len {
        let next = find_word_forward(text, pos, None);
        if next == pos {
            break;
        }
        pos = next;
    }
    assert_eq!(pos, text_len);
}

#[test]
fn forward_whitespace_at_boundaries() {
    let text = "  hello  ";
    assert_eq!(find_word_forward(text, 0, None), 7);
    assert_eq!(find_word_forward(text, 7, None), 9);
}

#[test]
fn forward_punctuation_run_foo_bar() {
    let text = "foo...bar";
    assert_eq!(find_word_forward(text, 0, None), 3);
    assert_eq!(find_word_forward(text, 3, None), 6);
    assert_eq!(find_word_forward(text, 6, None), 9);
}

#[test]
fn forward_cursor_at_end_returns_end() {
    assert_eq!(find_word_forward("hello", 5, None), 5);
}

// ---------------------------------------------------------------------------
// describe("atomic segments")
// ---------------------------------------------------------------------------

const MARKER: &str = "[paste #1 +5 lines]";
const ATOMIC_TEXT: &str = "hello [paste #1 +5 lines] world";

/// `[{segment, index, isWordLike}]` 的便捷构造（`input` 统一取整段文本，与 TS 测试一致）。
fn segments(input: &str, items: &[(&str, usize, bool)]) -> Vec<WordSegment> {
    items
        .iter()
        .map(|&(segment, index, is_word_like)| WordSegment {
            segment: segment.to_string(),
            index,
            input: input.to_string(),
            is_word_like,
        })
        .collect()
}

/// TS 的 `segmentMap`：函数会先切片再调用 `segment()`，因此按切片后的字符串查表。
fn atomic_segments(input: &str) -> Vec<WordSegment> {
    match input {
        // 整段（backward 从末尾开始时用的切片）
        "hello [paste #1 +5 lines] world" => segments(
            input,
            &[
                ("hello", 0, true),
                (" ", 5, false),
                (MARKER, 6, true),
                (" ", 25, false),
                ("world", 26, true),
            ],
        ),
        // backward from 26: slice(0, 26)
        "hello [paste #1 +5 lines] " => segments(
            input,
            &[
                ("hello", 0, true),
                (" ", 5, false),
                (MARKER, 6, true),
                (" ", 25, false),
            ],
        ),
        // forward from 6: slice(6)
        "[paste #1 +5 lines] world" => segments(
            input,
            &[(MARKER, 0, true), (" ", 19, false), ("world", 20, true)],
        ),
        _ => Vec::new(),
    }
}

fn is_atomic(segment: &str) -> bool {
    segment == MARKER
}

#[test]
fn backward_skips_word_then_stops_before_atomic_marker() {
    let options = WordNavigationOptions {
        segment: Some(&atomic_segments),
        is_atomic_segment: Some(&is_atomic),
    };
    assert_eq!(
        find_word_backward(
            ATOMIC_TEXT,
            ATOMIC_TEXT.encode_utf16().count(),
            Some(&options)
        ),
        26
    );
}

#[test]
fn backward_skips_whitespace_then_atomic_marker_as_one_unit() {
    let options = WordNavigationOptions {
        segment: Some(&atomic_segments),
        is_atomic_segment: Some(&is_atomic),
    };
    assert_eq!(find_word_backward(ATOMIC_TEXT, 26, Some(&options)), 6);
}

#[test]
fn forward_skips_atomic_marker_as_one_unit() {
    let options = WordNavigationOptions {
        segment: Some(&atomic_segments),
        is_atomic_segment: Some(&is_atomic),
    };
    assert_eq!(
        find_word_forward(ATOMIC_TEXT, 6, Some(&options)),
        6 + MARKER.len()
    );
}

// ---------------------------------------------------------------------------
// 评审回归：代理对中间的偏移 / 非 ASCII 词边界（期望值均取自 Node 26.12.1）
// ---------------------------------------------------------------------------

/// 光标落在代理对**中间**时，TS 的 `slice` 会切出孤立代理（ICU 把它当 U+FFFD 处理），
/// 因此 `findWordForward("😀abc", 1)` 返回 2（跨过 `😀`）；旧实现的
/// `floor_char_boundary_utf16` 会把光标回退到代理对之前，得到 3。
#[test]
fn cursor_inside_a_surrogate_pair_rounds_like_ts() {
    // TS：slice(1) = "\uDE00" + "abc"；"\uDE00" 非词、非空白 → 跳过它 → 1 + 1 = 2
    assert_eq!(find_word_forward("😀abc", 1, None), 2);
    // TS：slice(0, 4) 以孤立高位代理结尾 → 跳过整个 "abc😀" → 3
    assert_eq!(find_word_backward("abc😀", 4, None), 3);
    // TS：slice(0, 2) = "a" + 孤立高位代理 → 两段都跳过 → 1
    assert_eq!(find_word_backward("a😀b", 2, None), 1);
    // TS：slice(0, 1) = 孤立高位代理 → 非词段整体跳过 → 0
    assert_eq!(find_word_backward("🇺🇸ab", 1, None), 0);
}

/// `_` / `__` 的 `isWordLike` 差异（critical #2）：`__` 是一个 word-like 段，
/// 所以 `findWordForward("/__/", 0)` 停在该段内的第一个标点前（1），而不是跨过整段（4）。
#[test]
fn underscore_runs_stop_at_the_segment_not_past_it() {
    assert_eq!(find_word_forward("/__/", 0, None), 1);
    assert_eq!(find_word_forward("__", 0, None), 2);
    assert_eq!(find_word_backward("/__/", 2, None), 0);
}

/// `コンピュータ` 是一整段（critical #3）：向前一次到 6，向后从 6 回到 0。
#[test]
fn katakana_word_with_prolonged_mark_moves_as_one_unit() {
    assert_eq!(find_word_forward("コンピュータ", 0, None), 6);
    assert_eq!(find_word_backward("コンピュータ", 6, None), 0);
    assert_eq!(find_word_forward("キーボード", 0, None), 5);
    assert_eq!(find_word_backward("コーヒー", 4, None), 0);
}

/// 泰文按词典段移动（critical #1）：`จากนั้น` 的两次回退分别是 3 与 0，
/// 向前从 0 到 3（旧实现因为后处理把 `จาก`/`นั้น` 拼回整段，一次跨过 7）。
#[test]
fn thai_words_move_by_dictionary_segments() {
    assert_eq!(find_word_forward("จากนั้น", 0, None), 3);
    assert_eq!(find_word_backward("จากนั้น", 7, None), 3);
    assert_eq!(find_word_backward("จากนั้น", 3, None), 0);
    // `สวัสดีครับ` = `สวัสดี`(0..6) + `ครับ`(6..10)
    assert_eq!(find_word_forward("สวัสดีครับ", 0, None), 6);
    assert_eq!(find_word_backward("สวัสดีครับ", 10, None), 6);
}

/// `한́` 不是 word-like（critical #2），所以向前会跨过整段（2）。
#[test]
fn non_word_like_hangul_segment_is_skipped_whole() {
    assert_eq!(find_word_forward("한́", 0, None), 2);
    assert_eq!(find_word_backward("한́", 2, None), 0);
}

// ---------------------------------------------------------------------------
// 评审回归（major #1）：与 Node 26.12.1 / ICU 78.3 `Intl.Segmenter("word")` 逐段一致
// ---------------------------------------------------------------------------
//
// 修复前用的是 `icu_segmenter`（ICU4X）的词典数据，日文/泰文的词边界与 Node 不同
// （例如 ICU4X 把 `ひらがな` 切成 `ひらが`/`な`）。现在改用 Node 内置 ICU 78.3 的
// `cjdict`/`thaidict` 等词典 + ICU4C 引擎移植，下列期望值均在 Node 上实测。

#[test]
fn forward_thai_uses_the_icu78_dictionary_boundary() {
    // TS 实测（`packages/tui/src/word-navigation.ts`）：`findWordForward("าษาไทย", 1) = 3`
    // （评审报告写的是 4，与实际 TS 源码不符）。
    assert_eq!(find_word_forward("าษาไทย", 1, None), 3);
}

#[test]
fn backward_thai_uses_the_icu78_dictionary_boundary() {
    assert_eq!(find_word_backward("จากนั้น", 5, None), 2);
}

#[test]
fn forward_japanese_hiragana_follows_node_segments() {
    // Node：`ひ`/`ら`/`が`/`な`（ICU4X 是 `ひらが`/`な`）。
    assert_eq!(find_word_forward("ひらがな", 0, None), 1);
    assert_eq!(find_word_forward("ひらがな", 1, None), 2);
    assert_eq!(find_word_forward("ひらがな", 2, None), 3);
}

#[test]
fn forward_chinese_uses_the_icu78_dictionary_boundary() {
    // Node：`中`/`文字`。
    assert_eq!(find_word_forward("中文字", 0, None), 1);
}
