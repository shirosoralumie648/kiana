//! `kiana-logo.ts` 的测试。

mod common;

use kiana_tui::colors::{Color, TerminalColorMode, background_ansi, foreground_ansi, rgb_color};
use kiana_tui::visible_width;
use kiana_ui_kit::{kiana_logo_lines, kiana_wordmark, supports_kiana_logo};

const RESET: &str = "\x1b[0m";

fn color_mode() -> TerminalColorMode {
    kiana_theme::THEME
        .get_color_mode()
        .expect("theme initialized by common::setup")
}

fn coral() -> Color {
    rgb_color(228.0, 138.0, 122.0)
}

fn blue() -> Color {
    rgb_color(79.0, 142.0, 179.0)
}

fn yellow() -> Color {
    rgb_color(234.0, 182.0, 93.0)
}

#[test]
fn logo_lines_match_the_ts_template() {
    common::setup();
    let mode = color_mode();
    let [top, bottom] = kiana_logo_lines();

    let expected_top = format!(
        "{}{}▀{}{}▀█{} ",
        foreground_ansi(coral(), mode),
        background_ansi(blue(), mode),
        RESET,
        foreground_ansi(coral(), mode),
        RESET
    );
    let expected_bottom = format!(
        "{}█▀{} {}█{}",
        foreground_ansi(blue(), mode),
        RESET,
        foreground_ansi(yellow(), mode),
        RESET
    );
    assert_eq!(top, expected_top);
    assert_eq!(bottom, expected_bottom);

    // 两行都是 4 列宽（顶行第四个格子补空格）。
    assert_eq!(visible_width(&top), 4);
    assert_eq!(visible_width(&bottom), 4);
    // 半块字符是逐字的 U+2580 / U+2588。
    assert!(top.contains('▀'));
    assert!(top.contains('█'));
    assert!(bottom.contains('█'));
}

#[test]
fn wordmark_uses_coral_and_yellow() {
    common::setup();
    let mode = color_mode();
    assert_eq!(
        kiana_wordmark(),
        format!(
            "{}K{}{}iana{}",
            foreground_ansi(coral(), mode),
            RESET,
            foreground_ansi(yellow(), mode),
            RESET
        )
    );
    assert_eq!(visible_width(&kiana_wordmark()), 5);
}

#[test]
fn supports_logo_on_non_apple_terminals() {
    common::setup();
    // `is_apple_terminal_session()` 只在 macOS + TERM_PROGRAM=Apple_Terminal 时为真。
    let expected = !(cfg!(target_os = "macos")
        && std::env::var("TERM_PROGRAM").as_deref() == Ok("Apple_Terminal"));
    assert_eq!(supports_kiana_logo(), expected);
}
