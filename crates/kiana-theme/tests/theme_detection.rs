//! `theme-detection.test.ts` 的逐条移植。

mod common;

use common::{EnvGuard, lock, reset_theme_state, set_truecolor};
use kiana_theme::{
    ThemeColor, detect_color_fg_bg_theme_from, detect_terminal_theme_with_env, get_theme_by_name,
    parse_auto_theme_setting, resolve_theme_setting,
};
use kiana_tui::{RgbColor, TerminalColorScheme, TerminalColors};

fn rgb(r: u8, g: u8, b: u8) -> RgbColor {
    RgbColor { r, g, b }
}

#[test]
fn classifies_the_last_field_by_palette_index_like_vim() {
    assert_eq!(
        detect_color_fg_bg_theme_from(Some("15;0")),
        Some(TerminalColorScheme::Dark)
    );
    assert_eq!(
        detect_color_fg_bg_theme_from(Some("0;7;15")),
        Some(TerminalColorScheme::Light)
    );
    // Solarized Dark's background is bright black.
    assert_eq!(
        detect_color_fg_bg_theme_from(Some("12;8")),
        Some(TerminalColorScheme::Dark)
    );
    // rxvt writes "default" when the background is not a palette color.
    assert_eq!(detect_color_fg_bg_theme_from(Some("15;default")), None);
    assert_eq!(detect_color_fg_bg_theme_from(None), None);
}

#[test]
fn prefers_the_background_then_the_reported_scheme_then_colorfgbg_then_dark() {
    let env = Some("0;15");
    assert_eq!(
        detect_terminal_theme_with_env(
            &TerminalColors {
                background: Some(rgb(8, 8, 8)),
                ..TerminalColors::default()
            },
            Some(TerminalColorScheme::Light),
            env,
        ),
        TerminalColorScheme::Dark
    );
    assert_eq!(
        detect_terminal_theme_with_env(
            &TerminalColors::default(),
            Some(TerminalColorScheme::Dark),
            env,
        ),
        TerminalColorScheme::Dark
    );
    assert_eq!(
        detect_terminal_theme_with_env(&TerminalColors::default(), None, env),
        TerminalColorScheme::Light
    );
    assert_eq!(
        detect_terminal_theme_with_env(&TerminalColors::default(), None, None),
        TerminalColorScheme::Dark
    );
}

#[test]
fn follows_the_foreground_when_text_is_readable_that_way() {
    let background = rgb(118, 118, 118);
    assert_eq!(
        detect_terminal_theme_with_env(
            &TerminalColors {
                background: Some(background),
                ..TerminalColors::default()
            },
            None,
            None,
        ),
        TerminalColorScheme::Light
    );
    assert_eq!(
        detect_terminal_theme_with_env(
            &TerminalColors {
                background: Some(background),
                foreground: Some(rgb(255, 255, 255)),
                ..TerminalColors::default()
            },
            None,
            None,
        ),
        TerminalColorScheme::Dark
    );
    // White text cannot reach 4.5:1 on mid-gray.
    let mid_gray = rgb(128, 128, 128);
    assert_eq!(
        detect_terminal_theme_with_env(
            &TerminalColors {
                background: Some(mid_gray),
                foreground: Some(rgb(255, 255, 255)),
                ..TerminalColors::default()
            },
            None,
            None,
        ),
        TerminalColorScheme::Light
    );
}

#[test]
fn uses_terminal_capabilities() {
    let _guard = lock();
    reset_theme_state();

    set_truecolor(false);
    let ansi256_theme = get_theme_by_name("dark").expect("dark theme not found");
    assert_eq!(
        ansi256_theme.get_color_mode(),
        kiana_tui::TerminalColorMode::Palette256
    );
    let ansi = ansi256_theme.get_fg_ansi(ThemeColor::Accent);
    let pattern = regex::Regex::new(r"^\x1b\[38;5;\d+m$").expect("pattern");
    assert!(pattern.is_match(&ansi), "{ansi:?}");

    set_truecolor(true);
    let truecolor_theme = get_theme_by_name("dark").expect("dark theme not found");
    assert_eq!(
        truecolor_theme.get_color_mode(),
        kiana_tui::TerminalColorMode::Truecolor
    );
    let ansi = truecolor_theme.get_fg_ansi(ThemeColor::Accent);
    let pattern = regex::Regex::new(r"^\x1b\[38;2;\d+;\d+;\d+m$").expect("pattern");
    assert!(pattern.is_match(&ansi), "{ansi:?}");
}

#[test]
fn parses_and_resolves_automatic_theme_settings() {
    let _guard = lock();
    reset_theme_state();
    let _env = EnvGuard::unset("COLORFGBG");

    let auto = parse_auto_theme_setting(Some("light/dark")).expect("auto theme");
    assert_eq!(auto.light_theme, "light");
    assert_eq!(auto.dark_theme, "dark");

    assert_eq!(
        resolve_theme_setting(Some("dark"), TerminalColorScheme::Light),
        Some("dark".to_string())
    );
    assert_eq!(
        resolve_theme_setting(Some("light/dark"), TerminalColorScheme::Light),
        Some("light".to_string())
    );
    assert_eq!(
        resolve_theme_setting(Some("light/dark"), TerminalColorScheme::Dark),
        Some("dark".to_string())
    );
    assert_eq!(
        resolve_theme_setting(Some("light/dark/extra"), TerminalColorScheme::Dark),
        None
    );
}
