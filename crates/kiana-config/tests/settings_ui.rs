//! `ui.locale`（TUI 界面语言）的配置模型与 `SettingsManager` 行为。
//!
//! 覆盖：getter 回退链（`ui.locale` → 旧键 `language` → `"en"`）、project 覆盖
//! global、setter 保留其它 ui 字段并写盘、严格模式不把 `ui` 当作未知键。

use std::fs;
use std::path::{Path, PathBuf};

use kiana_config::settings::SettingsManager;
use kiana_config::settings::schema::Settings;
use kiana_config::settings::strict::{parse_strict_settings, strict_settings_schema};
use serde_json::{Value, json};

fn settings_from(value: Value) -> Settings {
    serde_json::from_value(value).expect("settings parse")
}

fn manager_from(value: Value) -> SettingsManager {
    SettingsManager::in_memory(settings_from(value))
}

struct TestDirs {
    _temp: tempfile::TempDir,
    agent_dir: PathBuf,
    project_dir: PathBuf,
}

impl TestDirs {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_path_buf();
        let agent_dir = root.join("agent");
        let project_dir = root.join("project");
        fs::create_dir_all(&agent_dir).expect("agent dir");
        fs::create_dir_all(project_dir.join(".kiana")).expect("project dir");
        Self {
            _temp: temp,
            agent_dir,
            project_dir,
        }
    }

    fn global_settings_path(&self) -> PathBuf {
        self.agent_dir.join("settings.json")
    }

    fn project_settings_path(&self) -> PathBuf {
        self.project_dir.join(".kiana").join("settings.json")
    }

    fn manager(&self) -> SettingsManager {
        SettingsManager::create(
            self.project_dir.to_str().expect("utf-8 path"),
            self.agent_dir.to_str().expect("utf-8 path"),
        )
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).expect("read settings")).expect("json")
}

// ---------------------------------------------------------------------------
// getter 回退链：ui.locale → language → "en"
// ---------------------------------------------------------------------------

#[test]
fn get_ui_locale_prefers_merged_ui_locale() {
    let manager = manager_from(json!({ "ui": { "locale": "zh-CN" }, "language": "fr" }));
    assert_eq!(manager.get_ui_locale(), "zh-CN");
}

#[test]
fn get_ui_locale_falls_back_to_legacy_language() {
    let manager = manager_from(json!({ "language": "zh-CN" }));
    assert_eq!(manager.get_ui_locale(), "zh-CN");

    // `ui.locale` 不是字符串（保留在 ui.extra）时也算未设置。
    let manager = manager_from(json!({ "ui": { "locale": 5 }, "language": "fr" }));
    assert_eq!(manager.get_ui_locale(), "fr");
}

#[test]
fn get_ui_locale_falls_back_to_en_and_skips_blank_values() {
    assert_eq!(manager_from(json!({})).get_ui_locale(), "en");

    // 空串/纯空白视为未设置：先回退 language，再回退 "en"。
    assert_eq!(
        manager_from(json!({ "ui": { "locale": "  " }, "language": "ja" })).get_ui_locale(),
        "ja"
    );
    assert_eq!(
        manager_from(json!({ "ui": { "locale": "" }, "language": " " })).get_ui_locale(),
        "en"
    );
    assert_eq!(
        manager_from(json!({ "ui": "bogus", "language": "" })).get_ui_locale(),
        "en"
    );
}

// ---------------------------------------------------------------------------
// project 覆盖 global
// ---------------------------------------------------------------------------

#[test]
fn project_ui_locale_overrides_global_and_keeps_other_ui_fields() {
    let dirs = TestDirs::new();
    fs::write(
        dirs.global_settings_path(),
        r#"{"ui":{"locale":"en","futureUiOption":true}}"#,
    )
    .expect("write global");
    fs::write(dirs.project_settings_path(), r#"{"ui":{"locale":"zh-CN"}}"#).expect("write project");

    let manager = dirs.manager();
    assert_eq!(manager.get_ui_locale(), "zh-CN");

    let merged = manager.get_settings();
    let ui = merged.ui.as_ref().expect("merged ui");
    assert_eq!(ui.locale.as_deref(), Some("zh-CN"));
    // 深度合并保留 global 的其它 ui 字段。
    assert_eq!(ui.extra.get("futureUiOption"), Some(&json!(true)));

    // global 快照本身不被 project 覆盖。
    let global = manager.get_global_settings();
    assert_eq!(
        global.ui.as_ref().and_then(|ui| ui.locale.as_deref()),
        Some("en")
    );
}

// ---------------------------------------------------------------------------
// setter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn set_ui_locale_preserves_other_ui_fields_and_persists() {
    let dirs = TestDirs::new();
    fs::write(
        dirs.global_settings_path(),
        r#"{"theme":"dark","ui":{"locale":"en","futureUiOption":true}}"#,
    )
    .expect("write settings");

    let manager = dirs.manager();
    manager.set_ui_locale("zh-CN");

    assert_eq!(manager.get_ui_locale(), "zh-CN");
    let ui = manager.get_settings().ui.expect("ui settings");
    assert_eq!(ui.locale.as_deref(), Some("zh-CN"));
    assert_eq!(ui.extra.get("futureUiOption"), Some(&json!(true)));

    manager.flush().await;

    let saved = read_json(&dirs.global_settings_path());
    assert_eq!(
        saved["ui"],
        json!({ "locale": "zh-CN", "futureUiOption": true })
    );
    assert_eq!(saved["theme"], json!("dark"));
}

#[tokio::test]
async fn set_ui_locale_replaces_stale_raw_ui_value() {
    let dirs = TestDirs::new();
    fs::write(dirs.global_settings_path(), r#"{"ui":"bogus"}"#).expect("write settings");

    let manager = dirs.manager();
    assert_eq!(manager.get_ui_locale(), "en");
    manager.set_ui_locale("ja");

    // 旧的非对象原始值随类型化写入一起清掉。
    assert!(manager.get_settings().extra.get("ui").is_none());
    assert_eq!(manager.get_ui_locale(), "ja");

    manager.flush().await;

    let saved = read_json(&dirs.global_settings_path());
    assert_eq!(saved["ui"], json!({ "locale": "ja" }));
}

// ---------------------------------------------------------------------------
// 严格模式：ui 是已知键
// ---------------------------------------------------------------------------

#[test]
fn strict_mode_does_not_treat_ui_as_unknown() {
    let schema = strict_settings_schema();
    let text = r#"{"ui":{"locale":"zh-CN","futureUiOption":true}}"#;
    let (_, diagnostics) =
        parse_strict_settings(Path::new("managed.json"), text, &schema).expect("valid JSON");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}
