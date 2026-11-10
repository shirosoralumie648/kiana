//! `test/visible-width.test.ts`：`visibleWidth` 的宽度与转义序列处理。
//!
//! TS 源：`packages/tui/test/visible-width.test.ts`（4 个用例，标题保留在测试函数文档中以便对照）。

use kiana_tui::text::visible_width;

#[test]
fn measures_styled_ascii_without_counting_escape_sequences() {
    assert_eq!(visible_width("\x1b[38;5;4mhello\x1b[39m world"), 11);
    assert_eq!(
        visible_width("\x1b]8;;https://example.com\x07link\x1b]8;;\x07"),
        4
    );
    assert_eq!(visible_width("\x1b]133;A\x1b\\prompt"), 6);
    assert_eq!(visible_width("\x1b_kiana:c\x07cursor"), 6);
}

#[test]
fn counts_tabs_as_three_columns_in_styled_text() {
    assert_eq!(visible_width("\x1b[1ma\tb\x1b[22m"), 5);
}

#[test]
fn measures_styled_non_ascii_text() {
    assert_eq!(visible_width("\x1b[31m日本\x1b[39m ok"), 7);
    assert_eq!(visible_width("\x1b[31m─→\x1b[39m"), 2);
}

#[test]
fn treats_unterminated_escape_sequences_as_zero_width_control_characters() {
    assert_eq!(visible_width("\x1b[31"), 3);
    assert_eq!(visible_width("a\x1b"), 1);
}

// ---------------------------------------------------------------------------
// 附录 B.1：`visibleWidth` 的实测数值（逐条断言）
// ---------------------------------------------------------------------------

#[test]
fn appendix_b1_matches_measured_widths() {
    let cases: &[(&str, usize)] = &[
        ("", 0),
        ("hello", 5),
        ("\t", 3),
        ("界", 2),
        ("🇨", 2),
        ("🇨🇳", 2),
        ("र्क", 2),
        ("ကို", 1),
        ("ำ", 1),
        ("กำ", 2),
        ("\r\n", 0),
        ("\x7f", 0),
        ("\x1b[31mHello\x1b[0m", 5),
        ("\x1b]133;A\x07x", 1),
        ("\x1b]8;;https://x\x07link\x1b]8;;\x07", 4),
        ("e\u{0301}", 1),
        ("\u{200B}", 0),
        ("\u{FEFF}", 0),
        ("\x1b_kiana:c\x07", 0),
        ("\x1b_kiana:/fc\x07", 0),
        ("\x1b_kiana:fc\x07", 0),
        ("\x1b[31m─→\x1b[39m", 2),
        ("\x1b[31m日本\x1b[39m ok", 7),
        ("\x1b[1ma\tb\x1b[22m", 5),
        ("\x1b]133;A\x1b\\prompt", 6),
        ("\x1b_kiana:c\x07cursor", 6),
        ("\x1b[31", 3),
        ("a\x1b", 1),
    ];
    for (input, expected) in cases {
        assert_eq!(visible_width(input), *expected, "visibleWidth({input:?})");
    }
}

// ---------------------------------------------------------------------------
// 附录 B.1 之外的边界（spec §2.1.3 的实测表）
// ---------------------------------------------------------------------------

#[test]
fn spec_2_1_3_matches_measured_widths() {
    let cases: &[(&str, usize)] = &[
        ("abc", 3),
        ("\x1b[2A", 3),
        ("\x1b[?25l", 5),
        ("\x1b[1;31mX", 1),
        ("\x1b[K", 0),
        ("\x1b[1K", 0),
        ("\x1b[G", 0),
        ("\x1b[3H", 0),
        ("\x1b[2J", 0),
        ("\x1b[1;2X", 5),
        ("\x1b]8;;u", 5),
        ("\x1b]8;;u\x07", 0),
        ("\x1b_payload\x1b\\", 0),
        ("\x1b_p", 2),
        ("\x1b[", 1),
        ("a\rb", 2),
        ("a\u{0000}b", 2),
        ("a\tb", 5),
    ];
    for (input, expected) in cases {
        assert_eq!(visible_width(input), *expected, "visibleWidth({input:?})");
    }
}

#[test]
fn appendix_b3_grapheme_width_examples() {
    // spec §2.1.2 的实测锚点（单簇时 visibleWidth == graphemeWidth）
    let cases: &[(&str, usize)] = &[
        ("a", 1),
        ("─", 1),
        ("→", 1),
        ("©", 1),
        ("界", 2),
        ("が", 2),
        ("か\u{3099}", 2),
        ("가", 2),
        ("각", 2),
        ("각", 2),
        ("网络", 4),
        ("ネットワーク", 12),
        ("🙂界", 4),
        ("👍", 2),
        ("👍🏻", 2),
        ("✅", 2),
        ("⚡", 2),
        ("⚡️", 2),
        ("👨", 2),
        ("👨‍💻", 2),
        ("🏳️‍🌈", 2),
        ("👨‍👩‍👧‍👦", 2),
        ("🏴󠁧󠁢󠁳󠁣󠁴󠁿", 2),
        ("©️", 2),
        ("1️⃣", 2),
        ("#️⃣", 2),
        ("*️⃣", 2),
        ("é", 1),
        ("čřžůú", 5),
        ("שָׁ", 1),
        ("بّ", 1),
        ("རྐ", 1),
        ("ᜠ᜴", 1),
        ("र्क", 2),
        ("র্ক", 2),
        ("ર્ક", 2),
        ("ର୍କ", 2),
        ("ర్క", 2),
        ("ര്‍ക", 2),
        ("नेटवर्क", 5),
        ("सर्वाधिकार सुरक्षित। ऑर्डर पर क्लिक करें", 33),
        ("ा", 1),
        ("ٟ", 1),
        ("ཿ", 1),
        ("ါ", 1),
        ("ေ", 1),
        ("᜴", 0),
        ("〮", 0),
        ("〯", 0),
        ("ကာ", 2),
        ("ကေ", 2),
        ("က်", 2),
        ("ကျ", 2),
        ("ကြ", 2),
        ("ကဳ", 2),
        ("ကဴ", 2),
        ("ကဵ", 2),
        ("ကး", 2),
        ("ကို", 1),
        ("က္", 1),
        ("ำ", 1),
        ("ຳ", 1),
        ("กำ", 2),
        ("ກຳ", 2),
        ("\r\n", 0),
        ("\x7f", 0),
    ];
    for (input, expected) in cases {
        assert_eq!(visible_width(input), *expected, "visibleWidth({input:?})");
    }
}
