//! `test/regression-regional-indicator-width.test.ts`：区域指示符宽度回归。
//!
//! TS 源：`packages/tui/test/regression-regional-indicator-width.test.ts`（5 个 `it`）。

use kiana_tui::text::{visible_width, wrap_text_with_ansi};

#[test]
fn treats_partial_flag_grapheme_as_full_width_to_avoid_streaming_render_drift() {
    // 复现背景：流式输出时 "🇨🇳" 常常先以中间态 "🇨" 出现。
    // 若 "🇨" 被算成宽 1 而终端按宽 2 渲染，差分渲染会漂移，留下残影。
    let partial_flag = "🇨";
    let list_line = "      - 🇨";

    assert_eq!(visible_width(partial_flag), 2);
    assert_eq!(visible_width(list_line), 10);
}

#[test]
fn wraps_intermediate_partial_flag_list_line_before_overflow() {
    // 宽度 9 装不下 "      - 🇨"（8 + 2 = 10），必须换行以避免终端自动换行不一致。
    let wrapped = wrap_text_with_ansi("      - 🇨", 9);

    assert_eq!(wrapped.len(), 2);
    assert_eq!(
        visible_width(wrapped.first().map(String::as_str).unwrap_or("")),
        7
    );
    assert_eq!(
        visible_width(wrapped.get(1).map(String::as_str).unwrap_or("")),
        2
    );
}

#[test]
fn treats_all_regional_indicator_singleton_graphemes_as_width_2() {
    for cp in 0x1f1e6..=0x1f1ffu32 {
        let regional_indicator = char::from_u32(cp).unwrap().to_string();
        assert_eq!(
            visible_width(&regional_indicator),
            2,
            "Expected {regional_indicator} (U+{cp:X}) to be width 2"
        );
    }
}

#[test]
fn keeps_full_flag_pairs_at_width_2() {
    let samples = ["🇯🇵", "🇺🇸", "🇬🇧", "🇨🇳", "🇩🇪", "🇫🇷"];
    for flag in samples {
        assert_eq!(visible_width(flag), 2, "Expected {flag} to be width 2");
    }
}

#[test]
fn keeps_common_streaming_emoji_intermediates_at_stable_width() {
    let samples = ["👍", "👍🏻", "✅", "⚡", "⚡️", "👨", "👨‍💻", "🏳️‍🌈"];
    for sample in samples {
        assert_eq!(visible_width(sample), 2, "Expected {sample} to be width 2");
    }
}
