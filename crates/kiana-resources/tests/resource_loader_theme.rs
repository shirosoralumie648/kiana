//! 移植 `packages/coding-agent/test/resource-loader-theme.test.ts`（3 个测试）。
//!
//! `setThemeJsonValidator(validateThemeJson)` 对应 `kiana_theme::set_theme_json_validator`；
//! 主题颜色模式从 `settingsManager.getTerminalCapabilityOverrides()` 叠加到
//! `detectCapabilities(() => false)` 的结果上（固定为 false，保证结果与终端无关）。

#[path = "resource_loader_support/mod.rs"]
mod support;

use std::path::Path;

use kiana_config::settings::SettingsManager;
use kiana_config::settings::schema::{BooleanOrAuto, Settings, TerminalSettings};
use kiana_theme::ThemeBg;
use kiana_tui::{
    detect_capabilities, get_terminal_color_mode, reset_capabilities_cache,
    set_capability_overrides,
};

use support::{TestDirs, new_loader, write_file};

/// 串行化改动 `KIANA_TRUE_COLOR` 的测试（同一测试二进制内的线程共享进程环境）。
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn s(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn theme_json(name: &str) -> serde_json::Value {
    let mut theme: serde_json::Value =
        serde_json::from_str(kiana_theme::DARK_JSON).expect("parse dark theme");
    theme["name"] = serde_json::Value::String(name.to_string());
    theme["colors"]["userMessageBg"] = serde_json::Value::String("#3c3544".to_string());
    theme
}

struct ThemeFixture {
    dirs: TestDirs,
    theme_path: std::path::PathBuf,
    theme_json: serde_json::Value,
}

fn fixture() -> ThemeFixture {
    let dirs = TestDirs::new();
    let theme_json = theme_json("capability-test");
    let theme_path = write_file(
        dirs.temp.path(),
        "capability-test.json",
        &serde_json::to_string(&theme_json).expect("serialize theme"),
    );
    ThemeFixture {
        dirs,
        theme_path,
        theme_json,
    }
}

fn terminal_settings(true_color: bool) -> Settings {
    Settings {
        terminal: Some(TerminalSettings {
            true_color: Some(BooleanOrAuto::Boolean(true_color)),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn uses_the_setting_setting_over_a_256_color_environment() {
    let _env = ENV_LOCK.lock().await;
    // 环境变量落后于显式设置：`KIANA_TRUE_COLOR=0` 不应影响主题颜色。
    unsafe {
        std::env::set_var("KIANA_TRUE_COLOR", "0");
    }
    set_capability_overrides(Default::default());
    reset_capabilities_cache();

    let fixture = fixture();
    let loader = new_loader(&fixture.dirs, |options| {
        options.settings_manager = Some(SettingsManager::in_memory(terminal_settings(true)));
        options.additional_theme_paths = vec![s(&fixture.theme_path)];
        options.no_extensions = true;
        options.no_skills = true;
        options.no_prompt_templates = true;
        options.no_context_files = true;
    });
    loader.reload(None).await.expect("reload");

    let theme = loader
        .get_themes()
        .themes
        .into_iter()
        .find(|theme| theme.name() == Some("capability-test"))
        .expect("capability-test theme");
    assert_eq!(
        theme.bg(ThemeBg::UserMessageBg, "x"),
        "\x1b[48;2;60;53;68mx\x1b[49m"
    );
    // 与 TS 的 `getTerminalColorMode({...detectCapabilities(() => false), ...overrides})` 同源。
    let _ = get_terminal_color_mode(detect_capabilities(&mut || false));
    unsafe {
        std::env::remove_var("KIANA_TRUE_COLOR");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn uses_the_setting_setting_over_a_truecolor_environment() {
    let _env = ENV_LOCK.lock().await;
    // 环境变量领先于显式设置：`KIANA_TRUE_COLOR=1` 不应覆盖 `trueColor: false`。
    unsafe {
        std::env::set_var("KIANA_TRUE_COLOR", "1");
    }
    set_capability_overrides(Default::default());
    reset_capabilities_cache();

    let fixture = fixture();
    let loader = new_loader(&fixture.dirs, |options| {
        options.settings_manager = Some(SettingsManager::in_memory(terminal_settings(false)));
        options.additional_theme_paths = vec![s(&fixture.theme_path)];
        options.no_extensions = true;
        options.no_skills = true;
        options.no_prompt_templates = true;
        options.no_context_files = true;
    });
    loader.reload(None).await.expect("reload");

    let theme = loader
        .get_themes()
        .themes
        .into_iter()
        .find(|theme| theme.name() == Some("capability-test"))
        .expect("capability-test theme");
    assert_eq!(
        theme.bg(ThemeBg::UserMessageBg, "x"),
        "\x1b[48;5;59mx\x1b[49m"
    );
    unsafe {
        std::env::remove_var("KIANA_TRUE_COLOR");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn returns_to_automatic_detection_after_an_explicit_setting_is_removed() {
    let _env = ENV_LOCK.lock().await;
    unsafe {
        std::env::set_var("KIANA_TRUE_COLOR", "1");
    }
    set_capability_overrides(Default::default());
    reset_capabilities_cache();

    let fixture = fixture();
    let settings_path = fixture.dirs.agent_dir.join("settings.json");
    write_file(
        &fixture.dirs.agent_dir,
        "settings.json",
        r#"{"terminal":{"trueColor":false}}"#,
    );
    let settings_manager =
        SettingsManager::create(&s(&fixture.dirs.cwd), &s(&fixture.dirs.agent_dir));
    let loader = new_loader(&fixture.dirs, |options| {
        options.settings_manager = Some(settings_manager.clone());
        options.additional_theme_paths = vec![s(&fixture.theme_path)];
        options.no_extensions = true;
        options.no_skills = true;
        options.no_prompt_templates = true;
        options.no_context_files = true;
    });
    loader.reload(None).await.expect("reload");
    set_capability_overrides(settings_manager.get_terminal_capability_overrides());

    std::fs::write(&settings_path, "{}").expect("write empty settings");
    loader.reload(None).await.expect("reload");

    let theme = loader
        .get_themes()
        .themes
        .into_iter()
        .find(|theme| theme.name() == Some("capability-test"))
        .expect("capability-test theme");
    assert_eq!(
        theme.bg(ThemeBg::UserMessageBg, "x"),
        "\x1b[48;2;60;53;68mx\x1b[49m"
    );
    unsafe {
        std::env::remove_var("KIANA_TRUE_COLOR");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn applies_strict_validation_on_initial_load_and_reload() {
    kiana_theme::set_theme_json_validator(Box::new(kiana_theme::validate_theme_json));

    let fixture = fixture();
    let theme_path = fixture.theme_path.clone();
    let mut theme_json = fixture.theme_json;
    let loader = new_loader(&fixture.dirs, |options| {
        options.settings_manager = Some(SettingsManager::in_memory(Settings::default()));
        options.additional_theme_paths = vec![s(&theme_path)];
        options.no_extensions = true;
        options.no_skills = true;
        options.no_prompt_templates = true;
        options.no_context_files = true;
    });

    theme_json["unsupported"] = serde_json::Value::Bool(true);
    std::fs::write(
        &theme_path,
        serde_json::to_string(&theme_json).expect("serialize"),
    )
    .expect("write theme");
    loader.reload(None).await.expect("reload");
    let initial_diagnostics = loader.get_themes().diagnostics;
    assert_eq!(initial_diagnostics.len(), 1);
    assert_eq!(
        initial_diagnostics[0].kind,
        kiana_core::diagnostics::DiagnosticKind::Warning
    );
    assert_eq!(
        initial_diagnostics[0].path.as_deref(),
        Some(s(&theme_path).as_str())
    );
    assert!(
        initial_diagnostics[0].message.contains("/unsupported"),
        "{}",
        initial_diagnostics[0].message
    );

    theme_json
        .as_object_mut()
        .expect("object")
        .remove("unsupported");
    std::fs::write(
        &theme_path,
        serde_json::to_string(&theme_json).expect("serialize"),
    )
    .expect("write theme");
    loader.reload(None).await.expect("reload");
    assert_eq!(loader.get_themes().diagnostics, Vec::new());
    assert!(
        loader
            .get_themes()
            .themes
            .iter()
            .any(|theme| theme.name() == Some("capability-test"))
    );

    theme_json["unsupported"] = serde_json::Value::Bool(true);
    std::fs::write(
        &theme_path,
        serde_json::to_string(&theme_json).expect("serialize"),
    )
    .expect("write theme");
    loader.reload(None).await.expect("reload");
    assert_eq!(loader.get_themes().diagnostics, initial_diagnostics);

    kiana_theme::clear_theme_json_validator();
}

#[test]
fn themes_load_in_node_readdir_name_order() {
    // Node 的 `readdirSync` 按文件名字节序返回（libuv uv_fs_scandir）；主题顺序
    // 以及同名主题去重的胜者都依赖它。按非字典序创建文件，确认按名字排序。
    let dirs = TestDirs::new();
    let themes_dir = dirs.temp_path("themes");
    for name in ["zeta", "alpha", "Mike", "beta", "010", "aaa", "ZZZ"] {
        let json = theme_json(name);
        write_file(
            &themes_dir,
            &format!("{name}.json"),
            &serde_json::to_string(&json).expect("serialize"),
        );
    }
    let mut themes = Vec::new();
    let mut diagnostics = Vec::new();
    kiana_resources::loader::themes::load_themes_from_dir(
        &s(&themes_dir),
        &mut themes,
        &mut diagnostics,
        kiana_tui::TerminalColorMode::Truecolor,
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(
        themes
            .iter()
            .map(|theme| theme.name().unwrap_or_default())
            .collect::<Vec<_>>(),
        vec!["010", "Mike", "ZZZ", "aaa", "alpha", "beta", "zeta"]
    );
}
