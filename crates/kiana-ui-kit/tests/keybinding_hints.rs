//! `keybinding-hints.ts` 的测试。

mod common;

use kiana_theme::ThemeColor;
use kiana_ui_kit::{
    KeyTextFormatOptions, format_key_text, key_display_text, key_hint, key_text, raw_key_hint,
};

#[test]
fn formats_keys_with_slash_and_plus() {
    let options = KeyTextFormatOptions::default();
    assert_eq!(format_key_text("ctrl+shift+up", options), "ctrl+shift+up");
    assert_eq!(
        format_key_text("ctrl+shift+up/ctrl+up", options),
        "ctrl+shift+up/ctrl+up"
    );
    assert_eq!(format_key_text("a//b", options), "a//b");
    assert_eq!(format_key_text("", options), "");
    // 大小写不在非 capitalize 模式下改变。
    assert_eq!(format_key_text("Esc", options), "Esc");
}

#[test]
fn capitalize_only_affects_the_first_character_of_each_part() {
    let options = KeyTextFormatOptions { capitalize: true };
    assert_eq!(format_key_text("escape", options), "Escape");
    assert_eq!(format_key_text("ctrl+s", options), "Ctrl+S");
    assert_eq!(format_key_text("a//b", options), "A//B");
    assert_eq!(format_key_text("", options), "");
}

#[test]
fn matches_js_format_key_text_golden_cases() {
    // 由真实 TS 模块（`formatKeyText` 从 upstream-reference 源码导入）生成的 golden：
    // 覆盖 `/`/`+` 空段、拼音/拉丁扩展的大写展开（ß→SS、ﬁ→FI、ǰ→J̌…）、
    // astral 字符（charAt(0) 是代理项，不变）与随机组合。
    #[derive(serde::Deserialize)]
    struct Case {
        key: String,
        capitalize: bool,
        expected: String,
    }

    let raw = include_str!("fixtures/keybinding_format.json");
    let cases: Vec<Case> = serde_json::from_str(raw).expect("fixture must be valid JSON");
    assert!(cases.len() > 400, "unexpected fixture size {}", cases.len());
    for case in &cases {
        let options = KeyTextFormatOptions {
            capitalize: case.capitalize,
        };
        assert_eq!(
            format_key_text(&case.key, options),
            case.expected,
            "key={:?} capitalize={}",
            case.key,
            case.capitalize
        );
    }
}

#[test]
fn reads_the_global_keybindings_manager() {
    common::setup();
    assert_eq!(key_text("tui.select.cancel"), "escape/ctrl+c");
    assert_eq!(key_display_text("tui.select.cancel"), "Escape/Ctrl+C");
    assert_eq!(key_text("app.message.copy"), "ctrl+x");
    assert_eq!(key_display_text("app.message.copy"), "Ctrl+X");
    // 未定义的键位 → 空串。
    assert_eq!(key_text("does.not.exist"), "");
    assert_eq!(key_display_text("does.not.exist"), "");
}

#[test]
fn key_hint_uses_dim_keys_and_muted_description() {
    common::setup();
    assert_eq!(
        key_hint("app.message.copy", "to copy"),
        format!(
            "{}{}",
            common::fg(ThemeColor::Dim, "ctrl+x"),
            common::fg(ThemeColor::Muted, " to copy")
        )
    );
    // 未绑定的键位：dim 段为空串，但仍有一个前导空格。
    assert_eq!(
        key_hint("does.not.exist", "hidden"),
        format!(
            "{}{}",
            common::fg(ThemeColor::Dim, ""),
            common::fg(ThemeColor::Muted, " hidden")
        )
    );
}

#[test]
fn raw_key_hint_formats_the_raw_key() {
    common::setup();
    assert_eq!(
        raw_key_hint("ctrl+shift+up", "move up"),
        format!(
            "{}{}",
            common::fg(ThemeColor::Dim, "ctrl+shift+up"),
            common::fg(ThemeColor::Muted, " move up")
        )
    );
    let expected_alt = if cfg!(target_os = "macos") {
        "option+left"
    } else {
        "alt+left"
    };
    assert_eq!(
        raw_key_hint("alt+left", "back"),
        format!(
            "{}{}",
            common::fg(ThemeColor::Dim, expected_alt),
            common::fg(ThemeColor::Muted, " back")
        )
    );
}
