//! `scrollbar-theme.test.ts` 的逐条移植（`it.each` 展开成两条用例）。

mod common;

use common::{builtin_json, install_default_validator, lock, reset_theme_state, write_theme_file};
use std::sync::Arc;

use kiana_theme::{Theme, load_theme_from_path};
use kiana_tui::TerminalColorMode;
use tempfile::TempDir;

fn write_theme(theme: &serde_json::Value) -> (TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let name = theme["name"].as_str().expect("theme name");
    let path = write_theme_file(
        dir.path(),
        &format!("{name}.json"),
        &serde_json::to_string(theme).expect("serialize"),
    );
    (dir, path)
}

fn load(theme: &serde_json::Value) -> (TempDir, Arc<Theme>) {
    let (dir, path) = write_theme(theme);
    let loaded =
        load_theme_from_path(&path, Some(TerminalColorMode::Truecolor)).expect("theme loads");
    (dir, loaded)
}

fn missing_token_theme(token: &str) -> serde_json::Value {
    let mut theme_json = builtin_json("dark");
    theme_json["name"] = serde_json::json!(format!("missing-{token}-theme"));
    if let Some(colors) = theme_json["colors"].as_object_mut() {
        colors.remove(token);
    }
    theme_json
}

// 用例标题逐字取自 TS（`it.each` 的 `$token` 展开），因此保留驼峰。
#[allow(non_snake_case)]
#[test]
fn falls_back_to_scrollbarTrack_when_omitted() {
    let _lock = lock();
    reset_theme_state();
    install_default_validator();
    let (_dir, theme) = load(&missing_token_theme("scrollbarTrack"));
    assert_eq!(
        theme.get_fg_ansi_named("scrollbarTrack").expect("resolved"),
        theme.get_fg_ansi_named("muted").expect("resolved")
    );
}

// 同上：标题逐字取自 TS。
#[allow(non_snake_case)]
#[test]
fn falls_back_to_scrollbarThumb_when_omitted() {
    let _lock = lock();
    reset_theme_state();
    install_default_validator();
    let (_dir, theme) = load(&missing_token_theme("scrollbarThumb"));
    assert_eq!(
        theme.get_fg_ansi_named("scrollbarThumb").expect("resolved"),
        theme.get_fg_ansi_named("text").expect("resolved")
    );
}

#[test]
fn uses_explicitly_configured_scrollbar_colors() {
    let _lock = lock();
    reset_theme_state();
    install_default_validator();
    let mut theme_json = builtin_json("dark");
    theme_json["name"] = serde_json::json!("custom-scrollbar-theme");
    theme_json["colors"]["scrollbarTrack"] = serde_json::json!("#654321");
    theme_json["colors"]["scrollbarThumb"] = serde_json::json!("#123456");

    let (_dir, theme) = load(&theme_json);
    assert_eq!(
        theme.get_fg_ansi_named("scrollbarTrack").expect("resolved"),
        "\x1b[38;2;101;67;33m"
    );
    assert_eq!(
        theme.get_fg_ansi_named("scrollbarThumb").expect("resolved"),
        "\x1b[38;2;18;52;86m"
    );
}

#[test]
fn falls_back_to_existing_selection_and_text_colors_for_search_highlights() {
    let _lock = lock();
    reset_theme_state();
    install_default_validator();
    let mut theme_json = builtin_json("dark");
    theme_json["name"] = serde_json::json!("legacy-search-theme");
    if let Some(colors) = theme_json["colors"].as_object_mut() {
        colors.remove("searchMatchBg");
        colors.remove("searchMatchText");
    }

    let (_dir, theme) = load(&theme_json);
    assert_eq!(
        theme.get_bg_ansi_named("searchMatchBg").expect("resolved"),
        theme.get_bg_ansi_named("selectedBg").expect("resolved")
    );
    assert_eq!(
        theme
            .get_fg_ansi_named("searchMatchText")
            .expect("resolved"),
        theme.get_fg_ansi_named("text").expect("resolved")
    );
}

#[test]
fn uses_explicitly_configured_search_highlight_colors() {
    let _lock = lock();
    reset_theme_state();
    install_default_validator();
    let mut theme_json = builtin_json("dark");
    theme_json["name"] = serde_json::json!("custom-search-theme");
    theme_json["colors"]["searchMatchBg"] = serde_json::json!("#112233");
    theme_json["colors"]["searchMatchText"] = serde_json::json!("#223344");

    let (_dir, theme) = load(&theme_json);
    assert_eq!(
        theme.get_bg_ansi_named("searchMatchBg").expect("resolved"),
        "\x1b[48;2;17;34;51m"
    );
    assert_eq!(
        theme
            .get_fg_ansi_named("searchMatchText")
            .expect("resolved"),
        "\x1b[38;2;34;51;68m"
    );
}
