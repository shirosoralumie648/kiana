//! `theme-export.test.ts` 的逐条移植。

mod common;

use std::path::PathBuf;

use common::{EnvGuard, builtin_json, lock, reset_theme_state, write_theme_file};
use kiana_theme::{ThemeExportColors, get_theme_export_colors};
use kiana_tui::{color_to_hex, indexed_color, okhsl_color};
use tempfile::TempDir;

/// 建立 `KIANA_CODING_AGENT_DIR=<root>/agent`（含 `themes/`），返回 root 与 guard。
fn agent_dir() -> (TempDir, PathBuf, EnvGuard) {
    let root = tempfile::tempdir().expect("temp dir");
    let agent_dir = root.path().join("agent");
    std::fs::create_dir_all(agent_dir.join("themes")).expect("themes dir");
    let guard = EnvGuard::set("KIANA_CODING_AGENT_DIR", &agent_dir);
    (root, agent_dir, guard)
}

#[test]
fn resolves_export_variable_references_using_the_same_syntax_as_colors() {
    let _lock = lock();
    reset_theme_state();
    let (_root, agent_dir, _env) = agent_dir();

    let dark_theme = builtin_json("dark");
    let mut custom_theme = dark_theme.clone();
    custom_theme["name"] = serde_json::json!("custom-export-vars");
    custom_theme["vars"]["pageBgVar"] = serde_json::json!("#112233");
    custom_theme["vars"]["pageBgAlias"] = serde_json::json!("pageBgVar");
    custom_theme["vars"]["infoBgVar"] = serde_json::json!("#445566");
    custom_theme["vars"]["cardBgVar"] = serde_json::json!("#223344");
    custom_theme["export"] = serde_json::json!({
        "pageBg": "pageBgAlias",
        "cardBg": "cardBgVar",
        "infoBg": "infoBgVar",
    });
    write_theme_file(
        &agent_dir.join("themes"),
        "custom-export-vars.json",
        &serde_json::to_string_pretty(&custom_theme).expect("serialize"),
    );

    assert_eq!(
        get_theme_export_colors(Some("custom-export-vars")),
        ThemeExportColors {
            page_bg: Some("#112233".to_string()),
            card_bg: Some("#223344".to_string()),
            info_bg: Some("#445566".to_string()),
        }
    );
}

#[test]
fn converts_okhsl_export_colors_to_hex_because_css_does_not_support_them() {
    let _lock = lock();
    reset_theme_state();
    let (_root, agent_dir, _env) = agent_dir();

    let mut custom_theme = builtin_json("dark");
    custom_theme["name"] = serde_json::json!("custom-export-okhsl");
    custom_theme["vars"] = serde_json::json!({ "card": "okhsl(250 20% 20%)" });
    custom_theme["export"] = serde_json::json!({
        "pageBg": "okhsl(250 20% 15%)",
        "cardBg": "card",
        "infoBg": "oklch(30% 0.05 80)",
    });
    write_theme_file(
        &agent_dir.join("themes"),
        "custom-export-okhsl.json",
        &serde_json::to_string_pretty(&custom_theme).expect("serialize"),
    );

    assert_eq!(
        get_theme_export_colors(Some("custom-export-okhsl")),
        ThemeExportColors {
            page_bg: Some(color_to_hex(okhsl_color(250.0, 0.2, 0.15))),
            card_bg: Some(color_to_hex(okhsl_color(250.0, 0.2, 0.2))),
            info_bg: Some("oklch(30% 0.05 80)".to_string()),
        }
    );
}

#[test]
fn resolves_recursive_vars_and_converts_256_color_export_values_to_hex() {
    let _lock = lock();
    reset_theme_state();
    let (_root, agent_dir, _env) = agent_dir();

    let mut custom_theme = builtin_json("dark");
    custom_theme["name"] = serde_json::json!("custom-export-recursive");
    custom_theme["vars"]["deepPageBg"] = serde_json::json!("#abcdef");
    custom_theme["vars"]["pageBgAlias"] = serde_json::json!("deepPageBg");
    custom_theme["vars"]["cardBgAnsi"] = serde_json::json!(24);
    custom_theme["export"] = serde_json::json!({
        "pageBg": "pageBgAlias",
        "cardBg": "cardBgAnsi",
        "infoBg": "",
    });
    write_theme_file(
        &agent_dir.join("themes"),
        "custom-export-recursive.json",
        &serde_json::to_string_pretty(&custom_theme).expect("serialize"),
    );

    assert_eq!(
        get_theme_export_colors(Some("custom-export-recursive")),
        ThemeExportColors {
            page_bg: Some("#abcdef".to_string()),
            card_bg: Some(color_to_hex(indexed_color(24))),
            info_bg: None,
        }
    );
    assert_eq!(color_to_hex(indexed_color(24)), "#005f87");
}

#[test]
fn returns_no_export_colors_when_any_field_cannot_be_resolved() {
    let _lock = lock();
    reset_theme_state();
    let (_root, agent_dir, _env) = agent_dir();

    // okhsl 字面量在加载时原样保留；`getThemeExportColors()` 的 CSS 转换失败时，TS 的
    // 整个对象字面量求值抛错并被 catch，返回 `{}`（不是部分结果）。
    let mut custom_theme = builtin_json("dark");
    custom_theme["name"] = serde_json::json!("custom-export-broken-okhsl");
    custom_theme["export"] = serde_json::json!({
        "pageBg": "okhsl(bad)",
        "cardBg": "#112233",
        "infoBg": "#445566",
    });
    write_theme_file(
        &agent_dir.join("themes"),
        "custom-export-broken-okhsl.json",
        &serde_json::to_string_pretty(&custom_theme).expect("serialize"),
    );
    assert_eq!(
        get_theme_export_colors(Some("custom-export-broken-okhsl")),
        ThemeExportColors::default()
    );

    // 未定义变量的引用同样让整段导出失败（变量在这条路径上惰性解析）。
    let mut custom_theme = builtin_json("dark");
    custom_theme["name"] = serde_json::json!("custom-export-missing-var");
    custom_theme["export"] = serde_json::json!({
        "pageBg": "missing-var",
        "cardBg": "#112233",
    });
    write_theme_file(
        &agent_dir.join("themes"),
        "custom-export-missing-var.json",
        &serde_json::to_string_pretty(&custom_theme).expect("serialize"),
    );
    assert_eq!(
        get_theme_export_colors(Some("custom-export-missing-var")),
        ThemeExportColors::default()
    );
}
