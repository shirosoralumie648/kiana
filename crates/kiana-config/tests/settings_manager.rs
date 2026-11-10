//! `packages/coding-agent/test/settings-manager.test.ts` 的移植（51 个用例）。
//!
//! 每个测试上方的 `TS:` 注释保留原测试标题逐字。目录用 `tempfile::tempdir()`（不用
//! `~/.kiana`），`SettingsManager::create(projectDir, agentDir)` 的路径都是临时目录。
//!
//! 需要 `flush()` / `reload()` 的用例是 `#[tokio::test]`；写队列在没有 tokio 运行时
//! 的同步上下文里会就地执行（与 TS 的微任务效果相同）。
//!
//! 环境变量相关的用例（`$VISUAL` / `$EDITOR`）用文件内互斥锁串行化。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use kiana_ai::ModelThinkingLevel;
use kiana_config::settings::schema::{
    BooleanOrAuto, DefaultProjectTrust, FullscreenExitOutput, MermaidRenderingMode, PackageSource,
    QuietStartup, Settings, TerminalImageProtocol, TerminalImagesSetting, TerminalSettings,
    TuiMode,
};
use kiana_config::settings::{
    InMemorySettingsStorage, ResolvedRetrySettings, SettingsManager, SettingsManagerCreateOptions,
    SettingsScope, SettingsStorage,
};
use kiana_core::http_dispatcher::DEFAULT_HTTP_IDLE_TIMEOUT_MS;
use kiana_tui::terminal_image::CapabilityOverrides;
use kiana_tui::{ImageProtocol, ScrollViewScrollbar, WheelScrollLines};
use serde_json::{Value, json};

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn serial_env() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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

    fn agent(&self) -> &str {
        self.agent_dir.to_str().expect("utf-8 path")
    }

    fn project(&self) -> &str {
        self.project_dir.to_str().expect("utf-8 path")
    }

    fn global_settings_path(&self) -> PathBuf {
        self.agent_dir.join("settings.json")
    }

    fn project_settings_path(&self) -> PathBuf {
        self.project_dir.join(".kiana").join("settings.json")
    }

    fn manager(&self) -> SettingsManager {
        SettingsManager::create(self.project(), self.agent())
    }

    fn manager_with_options(&self, options: SettingsManagerCreateOptions) -> SettingsManager {
        SettingsManager::create_with_options(self.project(), self.agent(), options)
    }
}

fn write_file(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write file");
}

fn write_json(path: &Path, value: &Value) {
    write_file(path, &serde_json::to_string(value).expect("serialize"));
}

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).expect("read file")
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&read_text(path)).expect("parse json")
}

// ---------------------------------------------------------------------------
// TS: describe("preserves externally added settings")
// ---------------------------------------------------------------------------

// TS: "should preserve enabledModels when changing thinking level"
#[tokio::test]
async fn should_preserve_enabled_models_when_changing_thinking_level() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();
    write_json(
        &settings_path,
        &json!({ "theme": "dark", "defaultModel": "claude-sonnet" }),
    );

    let manager = dirs.manager();

    // 模拟用户外部编辑 settings.json，加上 enabledModels。
    let mut current = read_json(&settings_path);
    current["enabledModels"] = json!(["claude-opus-4-5", "gpt-5.2-codex"]);
    write_file(
        &settings_path,
        &serde_json::to_string_pretty(&current).expect("pretty"),
    );

    manager.set_default_thinking_level(ModelThinkingLevel::High);
    manager.flush().await;

    let saved = read_json(&settings_path);
    assert_eq!(
        saved["enabledModels"],
        json!(["claude-opus-4-5", "gpt-5.2-codex"])
    );
    assert_eq!(saved["defaultThinkingLevel"], json!("high"));
    assert_eq!(saved["theme"], json!("dark"));
    assert_eq!(saved["defaultModel"], json!("claude-sonnet"));
}

// TS: "should preserve custom settings when changing theme"
#[tokio::test]
async fn should_preserve_custom_settings_when_changing_theme() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();
    write_json(&settings_path, &json!({ "defaultModel": "claude-sonnet" }));

    let manager = dirs.manager();

    let mut current = read_json(&settings_path);
    current["shellPath"] = json!("/bin/zsh");
    current["extensions"] = json!(["/path/to/extension.ts"]);
    write_file(
        &settings_path,
        &serde_json::to_string_pretty(&current).expect("pretty"),
    );

    manager.set_theme("light".to_string());
    manager.flush().await;

    let saved = read_json(&settings_path);
    assert_eq!(saved["shellPath"], json!("/bin/zsh"));
    assert_eq!(saved["extensions"], json!(["/path/to/extension.ts"]));
    assert_eq!(saved["theme"], json!("light"));
}

// TS: "should let in-memory changes override file changes for same key"
#[tokio::test]
async fn should_let_in_memory_changes_override_file_changes_for_same_key() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();
    write_json(&settings_path, &json!({ "theme": "dark" }));

    let manager = dirs.manager();

    let mut current = read_json(&settings_path);
    current["defaultThinkingLevel"] = json!("low");
    write_file(
        &settings_path,
        &serde_json::to_string_pretty(&current).expect("pretty"),
    );

    manager.set_default_thinking_level(ModelThinkingLevel::High);
    manager.flush().await;

    assert_eq!(
        read_json(&settings_path)["defaultThinkingLevel"],
        json!("high")
    );
}

// ---------------------------------------------------------------------------
// TS: describe("deviceId")
// ---------------------------------------------------------------------------

// TS: "creates one global device ID and reuses it in later processes"
#[tokio::test]
async fn creates_one_global_device_id_and_reuses_it_in_later_processes() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();
    write_json(&settings_path, &json!({ "theme": "dark" }));
    write_json(
        &dirs.project_settings_path(),
        &json!({ "deviceId": "project-device" }),
    );
    let first = dirs.manager();

    let device_id = first.get_or_create_device_id();
    first.flush().await;

    let pattern = |value: &str| {
        let bytes = value.as_bytes();
        bytes.len() == 36
            && bytes.iter().enumerate().all(|(index, byte)| match index {
                8 | 13 | 18 | 23 => *byte == b'-',
                _ => byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase(),
            })
    };
    assert!(pattern(&device_id), "unexpected uuid: {device_id}");
    assert_eq!(first.get_or_create_device_id(), device_id);
    assert_eq!(dirs.manager().get_or_create_device_id(), device_id);
    assert_eq!(
        read_json(&settings_path),
        json!({ "theme": "dark", "deviceId": device_id })
    );
}

// ---------------------------------------------------------------------------
// TS: describe("packages migration")
// ---------------------------------------------------------------------------

// TS: "should keep local-only extensions in extensions array"
#[test]
fn should_keep_local_only_extensions_in_extensions_array() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({ "extensions": ["/local/ext.ts", "./relative/ext.ts"] }),
    );

    let manager = dirs.manager();

    assert_eq!(manager.get_packages(), Vec::<PackageSource>::new());
    assert_eq!(
        manager.get_extension_paths(),
        vec!["/local/ext.ts".to_string(), "./relative/ext.ts".to_string()]
    );
}

// TS: "should handle packages with filtering objects"
#[test]
fn should_handle_packages_with_filtering_objects() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({
            "packages": [
                "npm:simple-pkg",
                {
                    "source": "npm:shitty-extensions",
                    "extensions": ["extensions/oracle.ts"],
                    "skills": [],
                },
            ],
        }),
    );

    let manager = dirs.manager();

    let packages = manager.get_packages();
    assert_eq!(packages.len(), 2);
    assert_eq!(
        packages[0],
        PackageSource::Text("npm:simple-pkg".to_string())
    );
    let serialized = serde_json::to_value(&packages[1]).expect("serialize");
    assert_eq!(
        serialized,
        json!({
            "source": "npm:shitty-extensions",
            "extensions": ["extensions/oracle.ts"],
            "skills": [],
        })
    );
}

// ---------------------------------------------------------------------------
// TS: describe("reload")
// ---------------------------------------------------------------------------

// TS: "should reload global settings from disk"
#[tokio::test]
async fn should_reload_global_settings_from_disk() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();
    write_json(
        &settings_path,
        &json!({ "theme": "dark", "extensions": ["/before.ts"] }),
    );

    let manager = dirs.manager();

    write_json(
        &settings_path,
        &json!({
            "theme": "light",
            "extensions": ["/after.ts"],
            "defaultModel": "claude-sonnet",
        }),
    );

    manager.reload().await;

    assert_eq!(manager.get_theme(), Some("light".to_string()));
    assert_eq!(manager.get_extension_paths(), vec!["/after.ts".to_string()]);
    assert_eq!(
        manager.get_default_model(),
        Some("claude-sonnet".to_string())
    );
}

// TS: "should keep previous settings and report the file path when the file is invalid"
#[tokio::test]
async fn should_keep_previous_settings_and_report_the_file_path_when_the_file_is_invalid() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();
    write_json(&settings_path, &json!({ "theme": "dark" }));

    let manager = dirs.manager();

    write_file(&settings_path, "{ invalid json");
    manager.reload().await;

    assert_eq!(manager.get_theme(), Some("dark".to_string()));
    let errors = manager.drain_errors();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].scope, SettingsScope::Global);
    assert_eq!(
        errors[0].path.as_deref(),
        Some(settings_path.to_str().expect("utf-8"))
    );
}

// ---------------------------------------------------------------------------
// TS: describe("theme setting")
// ---------------------------------------------------------------------------

// TS: "stores slash-separated automatic theme settings separately from fixed theme names"
#[tokio::test]
async fn stores_slash_separated_automatic_theme_settings_separately_from_fixed_theme_names() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();
    write_json(&settings_path, &json!({ "theme": "light/dark" }));

    let manager = dirs.manager();

    assert_eq!(manager.get_theme(), None);
    assert_eq!(manager.get_theme_setting(), Some("light/dark".to_string()));

    manager.set_theme("solarized-light/tokyo-night".to_string());
    manager.flush().await;

    assert_eq!(
        read_json(&settings_path)["theme"],
        json!("solarized-light/tokyo-night")
    );
}

// ---------------------------------------------------------------------------
// TS: describe("error tracking")
// ---------------------------------------------------------------------------

// TS: "should collect and clear load errors via drainErrors"
#[test]
fn should_collect_and_clear_load_errors_via_drain_errors() {
    let dirs = TestDirs::new();
    let global_settings_path = dirs.global_settings_path();
    let project_settings_path = dirs.project_settings_path();
    write_file(&global_settings_path, "{ invalid global json");
    write_file(&project_settings_path, "{ invalid project json");

    let manager = dirs.manager();
    let errors = manager.drain_errors();

    assert_eq!(errors.len(), 2);
    assert_eq!(errors[0].scope, SettingsScope::Global);
    assert_eq!(
        errors[0].path.as_deref(),
        Some(global_settings_path.to_str().expect("utf-8"))
    );
    assert_eq!(errors[1].scope, SettingsScope::Project);
    assert_eq!(
        errors[1].path.as_deref(),
        Some(project_settings_path.to_str().expect("utf-8"))
    );
    assert!(manager.drain_errors().is_empty());
}

// ---------------------------------------------------------------------------
// TS: describe("project trust")
// ---------------------------------------------------------------------------

// TS: "should skip project settings when project is not trusted"
#[test]
fn should_skip_project_settings_when_project_is_not_trusted() {
    let dirs = TestDirs::new();
    write_json(&dirs.global_settings_path(), &json!({ "theme": "global" }));
    write_json(
        &dirs.project_settings_path(),
        &json!({ "theme": "project" }),
    );

    let manager = dirs.manager_with_options(SettingsManagerCreateOptions {
        project_trusted: Some(false),
    });

    assert!(!manager.is_project_trusted());
    assert_eq!(manager.get_theme(), Some("global".to_string()));
    assert_eq!(manager.get_project_settings(), Settings::default());
}

// TS: "should reload project settings after trust changes to true"
#[test]
fn should_reload_project_settings_after_trust_changes_to_true() {
    let dirs = TestDirs::new();
    write_json(&dirs.global_settings_path(), &json!({ "theme": "global" }));
    write_json(
        &dirs.project_settings_path(),
        &json!({ "theme": "project" }),
    );
    let manager = dirs.manager_with_options(SettingsManagerCreateOptions {
        project_trusted: Some(false),
    });

    manager.set_project_trusted(true);

    assert!(manager.is_project_trusted());
    assert_eq!(manager.get_theme(), Some("project".to_string()));
}

// TS: "should fail project settings writes when project is not trusted"
#[tokio::test]
async fn should_fail_project_settings_writes_when_project_is_not_trusted() {
    let dirs = TestDirs::new();
    let project_settings_path = dirs.project_settings_path();
    write_json(
        &project_settings_path,
        &json!({ "packages": ["npm:existing"] }),
    );
    let manager = dirs.manager_with_options(SettingsManagerCreateOptions {
        project_trusted: Some(false),
    });

    let error = manager
        .set_project_packages(vec![PackageSource::Text("npm:new".to_string())])
        .expect_err("project is not trusted");
    assert_eq!(
        error.to_string(),
        "Project is not trusted; refusing to write project settings"
    );
    manager.flush().await;

    assert_eq!(manager.get_project_settings(), Settings::default());
    assert_eq!(
        read_json(&project_settings_path),
        json!({ "packages": ["npm:existing"] })
    );
}

// TS: "should read default project trust from global settings only"
#[test]
fn should_read_default_project_trust_from_global_settings_only() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({ "defaultProjectTrust": "always" }),
    );
    write_json(
        &dirs.project_settings_path(),
        &json!({ "defaultProjectTrust": "never" }),
    );

    let manager = dirs.manager();

    assert_eq!(
        manager.get_default_project_trust(),
        DefaultProjectTrust::Always
    );
}

// TS: "should default invalid project trust settings to ask"
#[test]
fn should_default_invalid_project_trust_settings_to_ask() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({ "defaultProjectTrust": "sometimes" }),
    );

    let manager = dirs.manager();

    assert_eq!(
        manager.get_default_project_trust(),
        DefaultProjectTrust::Ask
    );
}

// ---------------------------------------------------------------------------
// TS: describe("project settings directory creation")
// ---------------------------------------------------------------------------

// TS: "should not create .kiana folder when only reading project settings"
#[test]
fn should_not_create_kiana_folder_when_only_reading_project_settings() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();
    write_json(&settings_path, &json!({ "theme": "dark" }));

    fs::remove_dir_all(dirs.project_dir.join(".kiana")).expect("remove .kiana");

    let manager = dirs.manager();

    assert!(!dirs.project_dir.join(".kiana").exists());
    assert_eq!(manager.get_theme(), Some("dark".to_string()));
}

// TS: "should create .kiana folder when writing project settings"
#[tokio::test]
async fn should_create_kiana_folder_when_writing_project_settings() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();
    write_json(&settings_path, &json!({ "theme": "dark" }));

    fs::remove_dir_all(dirs.project_dir.join(".kiana")).expect("remove .kiana");

    let manager = dirs.manager();

    assert!(!dirs.project_dir.join(".kiana").exists());

    manager
        .set_project_packages(vec![PackageSource::Object(
            kiana_config::settings::schema::PackageSourceObject {
                source: Some("npm:test-pkg".to_string()),
                ..Default::default()
            },
        )])
        .expect("project trusted");
    manager.flush().await;

    assert!(dirs.project_dir.join(".kiana").exists());
    assert!(
        dirs.project_dir
            .join(".kiana")
            .join("settings.json")
            .exists()
    );
}

// ---------------------------------------------------------------------------
// TS: describe("terminal capability overrides")
// ---------------------------------------------------------------------------

fn terminal_overrides(terminal: TerminalSettings) -> CapabilityOverrides {
    SettingsManager::in_memory(Settings {
        terminal: Some(terminal),
        ..Settings::default()
    })
    .get_terminal_capability_overrides()
}

// TS: "maps explicit values and omits auto values"
#[test]
fn maps_explicit_values_and_omits_auto_values() {
    assert_eq!(
        terminal_overrides(TerminalSettings {
            images: Some(TerminalImagesSetting::Disabled),
            true_color: Some(BooleanOrAuto::Boolean(false)),
            hyperlinks: Some(BooleanOrAuto::Boolean(false)),
            ..Default::default()
        }),
        CapabilityOverrides {
            images: Some(None),
            true_color: Some(false),
            hyperlinks: Some(false),
        }
    );
    assert_eq!(
        terminal_overrides(TerminalSettings {
            images: Some(TerminalImagesSetting::Protocol(
                TerminalImageProtocol::Kitty
            )),
            true_color: Some(BooleanOrAuto::Boolean(true)),
            hyperlinks: Some(BooleanOrAuto::Boolean(true)),
            ..Default::default()
        }),
        CapabilityOverrides {
            images: Some(Some(ImageProtocol::Kitty)),
            true_color: Some(true),
            hyperlinks: Some(true),
        }
    );
    assert_eq!(
        terminal_overrides(TerminalSettings {
            images: Some(TerminalImagesSetting::Protocol(TerminalImageProtocol::Auto)),
            true_color: Some(BooleanOrAuto::Auto),
            hyperlinks: Some(BooleanOrAuto::Auto),
            ..Default::default()
        }),
        CapabilityOverrides::default()
    );
}

// ---------------------------------------------------------------------------
// TS: describe("retry settings")
// ---------------------------------------------------------------------------

// TS: "defaults and overrides agent retry delay cap"
#[test]
fn defaults_and_overrides_agent_retry_delay_cap() {
    assert_eq!(
        SettingsManager::in_memory(Settings::default()).get_retry_settings(),
        ResolvedRetrySettings {
            enabled: true,
            max_retries: 3.0,
            base_delay_ms: 2000.0,
            max_agent_delay_ms: 60000.0,
        }
    );
    let settings: Settings = serde_json::from_value(json!({
        "retry": { "enabled": true, "maxRetries": 10, "baseDelayMs": 500, "maxAgentDelayMs": 5000 },
    }))
    .expect("settings");
    assert_eq!(
        SettingsManager::in_memory(settings).get_retry_settings(),
        ResolvedRetrySettings {
            enabled: true,
            max_retries: 10.0,
            base_delay_ms: 500.0,
            max_agent_delay_ms: 5000.0,
        }
    );
}

// ---------------------------------------------------------------------------
// TS: describe("httpIdleTimeoutMs")
// ---------------------------------------------------------------------------

// TS: "should default to 5 minutes"
#[test]
fn should_default_to_5_minutes() {
    let dirs = TestDirs::new();
    let manager = dirs.manager();
    assert_eq!(
        manager.get_http_idle_timeout_ms().expect("valid"),
        DEFAULT_HTTP_IDLE_TIMEOUT_MS as f64
    );
}

// TS: "should use merged global and project settings"
#[test]
fn should_use_merged_global_and_project_settings() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({ "httpIdleTimeoutMs": 300000 }),
    );
    write_json(
        &dirs.project_settings_path(),
        &json!({ "httpIdleTimeoutMs": 0 }),
    );

    let manager = dirs.manager();

    assert_eq!(manager.get_http_idle_timeout_ms().expect("valid"), 0.0);
}

// TS: "should reject invalid timeout values"
#[test]
fn should_reject_invalid_timeout_values() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({ "httpIdleTimeoutMs": -1 }),
    );
    let manager = dirs.manager();

    let error = manager
        .get_http_idle_timeout_ms()
        .expect_err("invalid timeout");
    assert!(
        error
            .to_string()
            .contains("Invalid httpIdleTimeoutMs setting")
    );
}

// ---------------------------------------------------------------------------
// TS: describe("cacheWarming")
// ---------------------------------------------------------------------------

// TS: "defaults to streaming and ignores project settings"
#[test]
fn defaults_to_streaming_and_ignores_project_settings() {
    let dirs = TestDirs::new();
    assert_eq!(
        dirs.manager().get_cache_warming_mode(),
        kiana_config::settings::schema::CacheWarmingMode::Streaming
    );

    write_json(
        &dirs.project_settings_path(),
        &json!({ "cacheWarming": "idle" }),
    );
    assert_eq!(
        dirs.manager().get_cache_warming_mode(),
        kiana_config::settings::schema::CacheWarmingMode::Streaming
    );

    write_json(
        &dirs.global_settings_path(),
        &json!({ "cacheWarming": "idle" }),
    );
    assert_eq!(
        dirs.manager().get_cache_warming_mode(),
        kiana_config::settings::schema::CacheWarmingMode::Idle
    );

    write_json(
        &dirs.global_settings_path(),
        &json!({ "cacheWarming": "bogus" }),
    );
    assert_eq!(
        dirs.manager().get_cache_warming_mode(),
        kiana_config::settings::schema::CacheWarmingMode::Streaming
    );
}

// TS: "persists the mode globally"
#[tokio::test]
async fn persists_the_mode_globally() {
    let dirs = TestDirs::new();
    let manager = dirs.manager();
    manager.set_cache_warming_mode(kiana_config::settings::schema::CacheWarmingMode::Off);
    manager.flush().await;

    assert_eq!(
        dirs.manager().get_cache_warming_mode(),
        kiana_config::settings::schema::CacheWarmingMode::Off
    );
    assert_eq!(
        read_json(&dirs.global_settings_path()),
        json!({ "cacheWarming": "off" })
    );
}

// ---------------------------------------------------------------------------
// TS: describe("externalEditor")
// ---------------------------------------------------------------------------

fn set_editor_env(visual: Option<&str>, editor: Option<&str>) {
    // SAFETY: 本文件的 editor 用例都持有 `serial_env()` 的互斥锁。
    unsafe {
        match visual {
            Some(value) => std::env::set_var("VISUAL", value),
            None => std::env::remove_var("VISUAL"),
        }
        match editor {
            Some(value) => std::env::set_var("EDITOR", value),
            None => std::env::remove_var("EDITOR"),
        }
    }
}

// TS: "should resolve editor commands by precedence"
#[test]
fn should_resolve_editor_commands_by_precedence() {
    let _guard = serial_env();
    let original_visual = std::env::var("VISUAL").ok();
    let original_editor = std::env::var("EDITOR").ok();

    set_editor_env(Some("vim"), Some("nano"));
    assert_eq!(
        SettingsManager::in_memory(Settings {
            external_editor: Some("code --wait".to_string()),
            ..Settings::default()
        })
        .get_external_editor_command(),
        "code --wait"
    );
    assert_eq!(
        SettingsManager::in_memory(Settings::default()).get_external_editor_command(),
        "vim"
    );

    set_editor_env(None, Some("emacs"));
    assert_eq!(
        SettingsManager::in_memory(Settings::default()).get_external_editor_command(),
        "emacs"
    );

    // 纯空白的配置值与未配置等价（`trim() !== ""`）。
    set_editor_env(Some("vim"), None);
    assert_eq!(
        SettingsManager::in_memory(Settings {
            external_editor: Some("   ".to_string()),
            ..Settings::default()
        })
        .get_external_editor_command(),
        "vim"
    );

    set_editor_env(original_visual.as_deref(), original_editor.as_deref());
}

// TS: "should fall back to platform defaults"
//
// TS 用 `Object.defineProperty(process, "platform", ...)` 覆盖平台；Rust 的平台是编译期
// `cfg!`，因此只能断言当前平台（Linux 上为 `nano`）。win32 的 `notepad` 分支有编译覆盖，
// 运行期无法切换。
#[test]
fn should_fall_back_to_platform_defaults() {
    let _guard = serial_env();
    let original_visual = std::env::var("VISUAL").ok();
    let original_editor = std::env::var("EDITOR").ok();

    set_editor_env(None, None);
    let expected = if cfg!(windows) { "notepad" } else { "nano" };
    assert_eq!(
        SettingsManager::in_memory(Settings::default()).get_external_editor_command(),
        expected
    );

    set_editor_env(original_visual.as_deref(), original_editor.as_deref());
}

// ---------------------------------------------------------------------------
// TS: describe("TUI mode")
// ---------------------------------------------------------------------------

// TS: "defaults to fullscreen and persists regular mode"
#[tokio::test]
async fn defaults_to_fullscreen_and_persists_regular_mode() {
    let dirs = TestDirs::new();
    let manager = dirs.manager();

    assert_eq!(manager.get_tui_mode(), TuiMode::Fullscreen);

    manager.set_tui_mode(TuiMode::Regular);
    manager.flush().await;

    assert_eq!(manager.get_tui_mode(), TuiMode::Regular);
    assert_eq!(
        read_json(&dirs.global_settings_path())["tuiMode"],
        json!("regular")
    );
}

// TS: "falls back to fullscreen for unsupported values"
#[test]
fn falls_back_to_fullscreen_for_unsupported_values() {
    let dirs = TestDirs::new();
    write_json(&dirs.global_settings_path(), &json!({ "tuiMode": "other" }));

    let manager = dirs.manager();

    assert_eq!(manager.get_tui_mode(), TuiMode::Fullscreen);
}

// TS: "does not recognize the old uiMode setting"
#[test]
fn does_not_recognize_the_old_ui_mode_setting() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({ "uiMode": "regular" }),
    );

    let manager = dirs.manager();

    assert_eq!(manager.get_tui_mode(), TuiMode::Fullscreen);
}

// TS: "validates and persists fullscreen settings"
#[tokio::test]
async fn validates_and_persists_fullscreen_settings() {
    let dirs = TestDirs::new();
    let manager = dirs.manager();
    assert_eq!(
        manager.get_fullscreen_exit_output(),
        FullscreenExitOutput::Transcript
    );
    assert_eq!(
        manager.get_fullscreen_scrollbar(),
        ScrollViewScrollbar::Auto
    );
    assert!(manager.get_fullscreen_copy_on_select());

    manager.set_fullscreen_exit_output(FullscreenExitOutput::ResumeHint);
    manager.set_fullscreen_scrollbar(ScrollViewScrollbar::Hidden);
    manager.set_fullscreen_copy_on_select(false);
    manager.flush().await;
    let saved = read_json(&dirs.global_settings_path());
    assert_eq!(saved["fullscreenExitOutput"], json!("resume-hint"));
    assert_eq!(saved["fullscreenScrollbar"], json!("hidden"));
    assert_eq!(saved["fullscreenCopyOnSelect"], json!(false));

    write_json(
        &dirs.global_settings_path(),
        &json!({ "fullscreenExitOutput": "nothing", "fullscreenScrollbar": "sometimes" }),
    );
    let reloaded = dirs.manager();
    assert_eq!(
        reloaded.get_fullscreen_exit_output(),
        FullscreenExitOutput::Transcript
    );
    assert_eq!(
        reloaded.get_fullscreen_scrollbar(),
        ScrollViewScrollbar::Auto
    );
    assert!(reloaded.get_fullscreen_copy_on_select());
}

// TS: "persists fullscreen wheel scroll lines" (#9758)
#[tokio::test]
async fn persists_fullscreen_wheel_scroll_lines() {
    let dirs = TestDirs::new();
    let manager = dirs.manager();
    assert_eq!(
        manager.get_fullscreen_wheel_scroll_lines(),
        WheelScrollLines::Auto
    );

    manager.set_fullscreen_wheel_scroll_lines(WheelScrollLines::Fixed(3.0));
    manager.flush().await;
    assert_eq!(
        read_json(&dirs.global_settings_path())["fullscreenWheelScrollLines"],
        json!(3)
    );

    for (value, expected) in [
        (json!(7.9), WheelScrollLines::Fixed(7.0)),
        (json!(0), WheelScrollLines::Fixed(1.0)),
        (json!(1000), WheelScrollLines::Fixed(100.0)),
        (json!("fast"), WheelScrollLines::Auto),
        (Value::Null, WheelScrollLines::Auto),
    ] {
        write_json(
            &dirs.global_settings_path(),
            &json!({ "fullscreenWheelScrollLines": value }),
        );
        assert_eq!(dirs.manager().get_fullscreen_wheel_scroll_lines(), expected);
    }
}

// ---------------------------------------------------------------------------
// TS: describe("outputPad")
// ---------------------------------------------------------------------------

// TS: "should default to 1 and persist binary values"
#[tokio::test]
async fn should_default_to_1_and_persist_binary_values() {
    let dirs = TestDirs::new();
    let manager = dirs.manager();

    assert_eq!(manager.get_output_pad(), 1);

    manager.set_output_pad(0);
    manager.flush().await;

    assert_eq!(manager.get_output_pad(), 0);
    assert_eq!(
        read_json(&dirs.global_settings_path())["outputPad"],
        json!(0)
    );
}

// TS: "should treat unsupported outputPad values as default padding"
#[test]
fn should_treat_unsupported_output_pad_values_as_default_padding() {
    let dirs = TestDirs::new();
    write_json(&dirs.global_settings_path(), &json!({ "outputPad": 2 }));

    let manager = dirs.manager();

    assert_eq!(manager.get_output_pad(), 1);
}

// ---------------------------------------------------------------------------
// TS: describe("markdown.mermaid")
// ---------------------------------------------------------------------------

// TS: "defaults to streaming and persists rendering modes"
#[tokio::test]
async fn defaults_to_streaming_and_persists_rendering_modes() {
    let dirs = TestDirs::new();
    let manager = dirs.manager();

    assert_eq!(
        manager.get_mermaid_rendering_mode(),
        MermaidRenderingMode::Streaming
    );

    manager
        .set_mermaid_rendering_mode(MermaidRenderingMode::Final)
        .expect("valid markdown parent");
    manager.flush().await;

    assert_eq!(
        manager.get_mermaid_rendering_mode(),
        MermaidRenderingMode::Final
    );
    assert_eq!(
        read_json(&dirs.global_settings_path())["markdown"]["mermaid"],
        json!("final")
    );
}

// TS: "falls back to streaming for unsupported values"
#[test]
fn falls_back_to_streaming_for_unsupported_values() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({ "markdown": { "mermaid": "sometimes" } }),
    );

    assert_eq!(
        dirs.manager().get_mermaid_rendering_mode(),
        MermaidRenderingMode::Streaming
    );
}

// ---------------------------------------------------------------------------
// TS: describe("shellCommandPrefix")
// ---------------------------------------------------------------------------

// TS: "should load shellCommandPrefix from settings"
#[test]
fn should_load_shell_command_prefix_from_settings() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({ "shellCommandPrefix": "shopt -s expand_aliases" }),
    );

    let manager = dirs.manager();

    assert_eq!(
        manager.get_shell_command_prefix(),
        Some("shopt -s expand_aliases".to_string())
    );
}

// TS: "should return undefined when shellCommandPrefix is not set"
#[test]
fn should_return_undefined_when_shell_command_prefix_is_not_set() {
    let dirs = TestDirs::new();
    write_json(&dirs.global_settings_path(), &json!({ "theme": "dark" }));

    let manager = dirs.manager();

    assert_eq!(manager.get_shell_command_prefix(), None);
}

// TS: "should preserve shellCommandPrefix when saving unrelated settings"
#[tokio::test]
async fn should_preserve_shell_command_prefix_when_saving_unrelated_settings() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();
    write_json(
        &settings_path,
        &json!({ "shellCommandPrefix": "shopt -s expand_aliases" }),
    );

    let manager = dirs.manager();
    manager.set_theme("light".to_string());
    manager.flush().await;

    let saved = read_json(&settings_path);
    assert_eq!(
        saved["shellCommandPrefix"],
        json!("shopt -s expand_aliases")
    );
    assert_eq!(saved["theme"], json!("light"));
}

// ---------------------------------------------------------------------------
// TS: describe("defaultTools")
// ---------------------------------------------------------------------------

// TS: "loads global defaults and lets project settings replace them"
#[test]
fn loads_global_defaults_and_lets_project_settings_replace_them() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({ "defaultTools": ["read", "bash"] }),
    );

    assert_eq!(
        dirs.manager().get_default_tools(),
        Some(vec!["read".to_string(), "bash".to_string()])
    );

    write_json(
        &dirs.project_settings_path(),
        &json!({ "defaultTools": ["grep"] }),
    );

    assert_eq!(
        dirs.manager().get_default_tools(),
        Some(vec!["grep".to_string()])
    );
}

// TS: "preserves an empty tool list"
#[test]
fn preserves_an_empty_tool_list() {
    let empty: Settings = serde_json::from_value(json!({ "defaultTools": [] })).expect("settings");
    assert_eq!(
        SettingsManager::in_memory(empty).get_default_tools(),
        Some(Vec::new())
    );
    assert_eq!(
        SettingsManager::in_memory(Settings::default()).get_default_tools(),
        None
    );
}

// TS: "applies +name and -name to the default selection"
#[test]
fn applies_name_and_name_to_the_default_selection() {
    let modifiers: Settings =
        serde_json::from_value(json!({ "defaultTools": ["+codemode", "-write"] }))
            .expect("settings");
    assert_eq!(
        SettingsManager::in_memory(modifiers).get_default_tools(),
        Some(vec![
            "read".to_string(),
            "bash".to_string(),
            "edit".to_string(),
            "codemode".to_string(),
        ])
    );
    let mixed: Settings =
        serde_json::from_value(json!({ "defaultTools": ["read", "+grep", "+read"] }))
            .expect("settings");
    assert_eq!(
        SettingsManager::in_memory(mixed).get_default_tools(),
        Some(vec!["read".to_string(), "grep".to_string()])
    );
}

// TS: "layers project modifiers on top of the global selection"
#[test]
fn layers_project_modifiers_on_top_of_the_global_selection() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({ "defaultTools": ["read", "bash", "+codemode"] }),
    );
    write_json(
        &dirs.project_settings_path(),
        &json!({ "defaultTools": ["-codemode", "+tool_search"] }),
    );

    let manager = dirs.manager();
    assert_eq!(
        manager.get_default_tools(),
        Some(vec![
            "read".to_string(),
            "bash".to_string(),
            "tool_search".to_string(),
        ])
    );

    let overrides: Settings =
        serde_json::from_value(json!({ "defaultTools": ["+codemode"] })).expect("settings");
    manager.apply_overrides(overrides);
    assert_eq!(
        manager.get_default_tools(),
        Some(vec![
            "read".to_string(),
            "bash".to_string(),
            "tool_search".to_string(),
            "codemode".to_string(),
        ])
    );
}

// TS: "applies project modifiers to the built-in defaults without a global setting"
#[test]
fn applies_project_modifiers_to_the_built_in_defaults_without_a_global_setting() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.project_settings_path(),
        &json!({ "defaultTools": ["+codemode"] }),
    );

    assert_eq!(
        dirs.manager().get_default_tools(),
        Some(vec![
            "read".to_string(),
            "bash".to_string(),
            "edit".to_string(),
            "write".to_string(),
            "codemode".to_string(),
        ])
    );
}

// ---------------------------------------------------------------------------
// TS: describe("getSessionDir")
// ---------------------------------------------------------------------------

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

// TS: "should return undefined when not set"
#[test]
fn get_session_dir_should_return_undefined_when_not_set() {
    let dirs = TestDirs::new();
    write_json(&dirs.global_settings_path(), &json!({ "theme": "dark" }));
    let manager = dirs.manager();
    assert_eq!(manager.get_session_dir(), None);
}

// TS: "should return global sessionDir"
#[test]
fn get_session_dir_should_return_global_session_dir() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({ "sessionDir": "/tmp/sessions" }),
    );
    let manager = dirs.manager();
    assert_eq!(manager.get_session_dir(), Some("/tmp/sessions".to_string()));
}

// TS: "should return project sessionDir, overriding global"
#[test]
fn get_session_dir_should_return_project_session_dir_overriding_global() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({ "sessionDir": "/global/sessions" }),
    );
    write_json(
        &dirs.project_settings_path(),
        &json!({ "sessionDir": "./sessions" }),
    );
    let manager = dirs.manager();
    assert_eq!(manager.get_session_dir(), Some("./sessions".to_string()));
}

// TS: "should expand ~ in sessionDir"
#[test]
fn get_session_dir_should_expand_tilde() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({ "sessionDir": "~/sessions" }),
    );
    let manager = dirs.manager();
    assert_eq!(
        manager.get_session_dir(),
        Some(home_dir().join("sessions").to_string_lossy().into_owned())
    );
}

// ---------------------------------------------------------------------------
// TS: describe("getShellPath")
// ---------------------------------------------------------------------------

// TS: "should return undefined when not set"
#[test]
fn get_shell_path_should_return_undefined_when_not_set() {
    let dirs = TestDirs::new();
    write_json(&dirs.global_settings_path(), &json!({ "theme": "dark" }));
    let manager = dirs.manager();
    assert_eq!(manager.get_shell_path(), None);
}

// TS: "should return an absolute shellPath unchanged"
#[test]
fn get_shell_path_should_return_an_absolute_shell_path_unchanged() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({ "shellPath": "/bin/zsh" }),
    );
    let manager = dirs.manager();
    assert_eq!(manager.get_shell_path(), Some("/bin/zsh".to_string()));
}

// TS: "should expand ~ in shellPath"
#[test]
fn get_shell_path_should_expand_tilde() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({ "shellPath": "~/.local/bin/agent-shell-sandbox" }),
    );
    let manager = dirs.manager();
    assert_eq!(
        manager.get_shell_path(),
        Some(
            home_dir()
                .join(".local/bin/agent-shell-sandbox")
                .to_string_lossy()
                .into_owned()
        )
    );
}

// TS: "should expand a bare ~ in shellPath"
#[test]
fn get_shell_path_should_expand_a_bare_tilde() {
    let dirs = TestDirs::new();
    write_json(&dirs.global_settings_path(), &json!({ "shellPath": "~" }));
    let manager = dirs.manager();
    assert_eq!(
        manager.get_shell_path(),
        Some(home_dir().to_string_lossy().into_owned())
    );
}

// ---------------------------------------------------------------------------
// 补充覆盖（TS 测试未直接触及的 accessor / 合并语义）
// ---------------------------------------------------------------------------

// 深度合并（deepMergeObjects）对嵌套对象、record（modelOverrides）与未知键的语义由
// TS 参考实现实测锁定：嵌套对象逐字段合并、record 的每个条目也逐字段合并、
// 未知键（extra）递归合并且保持 base 顺序。
#[test]
fn deep_merges_nested_objects_records_and_unknown_keys() {
    let storage = Arc::new(InMemorySettingsStorage::new());
    storage
        .with_lock(SettingsScope::Global, &mut |_| {
            Ok(Some(
                r#"{
                    "compaction": {"reserveTokens": 8192, "modelOverrides": {"a/b": {"reserveTokens": 1, "keepRecentTokens": 2}}},
                    "modelThinkingLevels": {"p/a": "high"},
                    "terminal": {"showImages": true},
                    "extensionSetting": {"a": 1, "nested": {"x": 1}},
                    "theme": "dark"
                }"#
                .to_string(),
            ))
        })
        .expect("seed global");
    storage
        .with_lock(SettingsScope::Project, &mut |_| {
            Ok(Some(
                r#"{
                    "extensionSetting": {"b": 2, "nested": {"y": 2}}
                }"#
                .to_string(),
            ))
        })
        .expect("seed project");
    let manager = SettingsManager::from_storage(storage);

    assert_eq!(
        manager.get_settings().raw("extensionSetting"),
        Some(json!({ "a": 1, "nested": { "x": 1, "y": 2 }, "b": 2 }))
    );
    assert_eq!(manager.get_theme(), Some("dark".to_string()));
    assert_eq!(
        manager.get_model_thinking_level("p", "a"),
        Some(ModelThinkingLevel::High)
    );
    assert!(manager.get_show_images());

    manager.apply_overrides(
        serde_json::from_value(json!({
            "compaction": {
                "modelOverrides": {
                    "a/b": { "keepRecentTokens": 9 },
                    "c/d": { "reserveTokens": 5 },
                },
            },
            "modelThinkingLevels": { "p/b": "low" },
            "terminal": { "clearOnShrink": true },
        }))
        .expect("overrides"),
    );

    let settings = manager.get_settings();
    assert_eq!(
        kiana_json::js_object::stringify(
            &serde_json::to_value(settings.compaction.as_ref().expect("compaction")).expect("json")
        ),
        r#"{"reserveTokens":8192,"modelOverrides":{"a/b":{"reserveTokens":1,"keepRecentTokens":9},"c/d":{"reserveTokens":5}}}"#
    );
    assert_eq!(
        settings.model_thinking_levels.as_ref(),
        Some(&indexmap::IndexMap::from([
            ("p/a".to_string(), ModelThinkingLevel::High),
            ("p/b".to_string(), ModelThinkingLevel::Low),
        ]))
    );
    assert_eq!(
        kiana_json::js_object::stringify(
            &serde_json::to_value(settings.terminal.as_ref().expect("terminal")).expect("json")
        ),
        r#"{"showImages":true,"clearOnShrink":true}"#
    );
}

// `setShellPath(undefined)` 会从磁盘删除该键（TS 实测：`{"theme":"dark"}`）。
#[tokio::test]
async fn clearing_shell_path_removes_the_key_from_the_settings_file() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();
    write_json(
        &settings_path,
        &json!({ "shellPath": "/bin/zsh", "theme": "dark" }),
    );

    let manager = dirs.manager();
    manager.set_shell_path(None);
    manager.flush().await;

    assert_eq!(read_json(&settings_path), json!({ "theme": "dark" }));
    assert_eq!(manager.get_shell_path(), None);
}

// 写盘失败（例如磁盘满）不向调用方抛错，而是进 `drainErrors()`；错误带作用域。
#[tokio::test]
async fn records_write_failures_instead_of_throwing() {
    struct FailingWriteStorage;

    impl SettingsStorage for FailingWriteStorage {
        fn with_lock(
            &self,
            _scope: SettingsScope,
            f: &mut kiana_config::settings::SettingsLockCallback<'_>,
        ) -> Result<(), kiana_config::settings::SettingsStorageError> {
            let next = f(None)?;
            match next {
                Some(_) => Err(Box::new(std::io::Error::other("disk full"))),
                None => Ok(()),
            }
        }
    }

    let manager = SettingsManager::from_storage(Arc::new(FailingWriteStorage));
    manager.set_theme("light".to_string());
    manager.flush().await;

    let errors = manager.drain_errors();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].scope, SettingsScope::Global);
    assert_eq!(errors[0].path, None);
    assert_eq!(errors[0].error, "disk full");
}

// 每个具名 setter 写全局并能在新实例里读回（TS 未逐个覆盖的 accessor）。
#[tokio::test]
async fn persists_every_named_setter_and_reads_it_back() {
    use kiana_config::settings::schema::{
        DoubleEscapeAction, SessionShareDestination, SteeringMode, TransportSetting,
        TreeFilterMode, WarningSettings,
    };

    let dirs = TestDirs::new();
    let manager = dirs.manager();

    manager.set_last_changelog_version("1.4.0".to_string());
    manager.set_default_provider("anthropic".to_string());
    manager.set_default_model("claude-sonnet".to_string());
    manager.set_default_model_and_provider("openai".to_string(), "gpt-5".to_string());
    manager.set_steering_mode(SteeringMode::All);
    manager.set_follow_up_mode(SteeringMode::All);
    manager.set_theme("dark".to_string());
    manager.set_default_thinking_level(ModelThinkingLevel::High);
    manager
        .set_model_thinking_level("anthropic", "claude", ModelThinkingLevel::Low)
        .expect("valid thinking level");
    manager.remove_model_thinking_level("other", "missing");
    manager.set_transport(TransportSetting::Websocket);
    manager.set_session_share_destination(SessionShareDestination::Gist);
    manager
        .set_compaction_enabled(false)
        .expect("valid compaction parent");
    manager
        .set_retry_enabled(false)
        .expect("valid retry parent");
    manager.set_hide_thinking_block(true);
    manager.set_show_cache_miss_notices(true);
    manager.set_shell_path(Some("/bin/zsh".to_string()));
    manager.set_quiet_startup(QuietStartup::Header);
    manager.set_default_project_trust(DefaultProjectTrust::Never);
    manager.set_shell_command_prefix(Some("shopt -s expand_aliases".to_string()));
    manager.set_npm_command(Some(vec!["npm".to_string(), "--silent".to_string()]));
    manager.set_collapse_changelog(true);
    manager.set_enable_install_telemetry(false);
    manager.set_enable_analytics(true);
    manager.set_packages(vec![PackageSource::Text("npm:x".to_string())]);
    manager.set_extension_paths(vec!["/e.ts".to_string()]);
    manager.set_skill_paths(vec!["/s".to_string()]);
    manager.set_prompt_template_paths(vec!["/p.md".to_string()]);
    manager.set_theme_paths(vec!["/t.json".to_string()]);
    manager.set_enable_skill_commands(false);
    manager
        .set_show_images(false)
        .expect("valid terminal parent");
    manager
        .set_image_width_cells(5.9)
        .expect("valid terminal parent");
    manager
        .set_clear_on_shrink(true)
        .expect("valid terminal parent");
    manager
        .set_show_terminal_progress(true)
        .expect("valid terminal parent");
    manager.set_tui_mode(TuiMode::Regular);
    manager.set_fullscreen_exit_output(FullscreenExitOutput::ResumeHint);
    manager.set_fullscreen_scrollbar(ScrollViewScrollbar::Always);
    manager.set_fullscreen_copy_on_select(false);
    manager.set_fullscreen_wheel_scroll_lines(WheelScrollLines::Fixed(7.9));
    manager
        .set_image_auto_resize(false)
        .expect("valid images parent");
    manager.set_block_images(true).expect("valid images parent");
    manager.set_enabled_models(Some(vec!["claude*".to_string()]));
    manager.set_double_escape_action(DoubleEscapeAction::None);
    manager.set_tree_filter_mode(TreeFilterMode::All);
    manager.set_show_hardware_cursor(true);
    manager.set_editor_padding_x(5.9);
    manager.set_output_pad(0);
    manager.set_autocomplete_max_visible(1.2);
    manager
        .set_mermaid_rendering_mode(MermaidRenderingMode::Off)
        .expect("valid markdown parent");
    manager.set_warnings(WarningSettings {
        anthropic_extra_usage: Some(false),
        ..Default::default()
    });
    manager
        .set_http_idle_timeout_ms(45000.5)
        .expect("valid timeout");
    manager.flush().await;

    let saved = read_json(&dirs.global_settings_path());
    assert_eq!(saved["theme"], json!("dark"));
    assert_eq!(saved["steeringMode"], json!("all"));
    assert_eq!(saved["followUpMode"], json!("all"));
    assert_eq!(saved["transport"], json!("websocket"));
    assert_eq!(saved["editorPaddingX"], json!(3));
    assert_eq!(saved["autocompleteMaxVisible"], json!(3));
    assert_eq!(saved["outputPad"], json!(0));
    assert_eq!(saved["terminal"]["imageWidthCells"], json!(5));
    assert_eq!(saved["fullscreenWheelScrollLines"], json!(7));
    assert_eq!(saved["httpIdleTimeoutMs"], json!(45000));
    assert_eq!(saved["defaultProvider"], json!("openai"));
    assert_eq!(saved["defaultModel"], json!("gpt-5"));
    assert_eq!(saved["enableInstallTelemetry"], json!(false));
    assert_eq!(saved["warnings"], json!({ "anthropicExtraUsage": false }));
    assert!(saved["trackingId"].is_string());

    let reloaded = dirs.manager();
    assert_eq!(
        reloaded.get_last_changelog_version(),
        Some("1.4.0".to_string())
    );
    assert_eq!(reloaded.get_default_provider(), Some("openai".to_string()));
    assert_eq!(reloaded.get_default_model(), Some("gpt-5".to_string()));
    assert_eq!(reloaded.get_steering_mode(), SteeringMode::All);
    assert_eq!(reloaded.get_follow_up_mode(), SteeringMode::All);
    assert_eq!(reloaded.get_theme(), Some("dark".to_string()));
    assert_eq!(
        reloaded.get_default_thinking_level(),
        Some(ModelThinkingLevel::High)
    );
    assert_eq!(
        reloaded.get_model_thinking_level("anthropic", "claude"),
        Some(ModelThinkingLevel::Low)
    );
    assert_eq!(reloaded.get_transport(), TransportSetting::Websocket);
    assert_eq!(
        reloaded.get_session_share_destination(),
        SessionShareDestination::Gist
    );
    assert!(!reloaded.get_compaction_enabled());
    assert!(!reloaded.get_retry_enabled());
    assert!(reloaded.get_hide_thinking_block());
    assert!(reloaded.get_show_cache_miss_notices());
    assert_eq!(reloaded.get_shell_path(), Some("/bin/zsh".to_string()));
    assert_eq!(reloaded.get_quiet_startup(), QuietStartup::Header);
    assert_eq!(
        reloaded.get_default_project_trust(),
        DefaultProjectTrust::Never
    );
    assert_eq!(
        reloaded.get_shell_command_prefix(),
        Some("shopt -s expand_aliases".to_string())
    );
    assert_eq!(
        reloaded.get_npm_command(),
        Some(vec!["npm".to_string(), "--silent".to_string()])
    );
    assert!(reloaded.get_collapse_changelog());
    assert!(!reloaded.get_enable_install_telemetry());
    assert!(reloaded.get_enable_analytics());
    assert!(reloaded.get_tracking_id().is_some());
    assert_eq!(
        reloaded.get_packages(),
        vec![PackageSource::Text("npm:x".to_string())]
    );
    assert_eq!(reloaded.get_extension_paths(), vec!["/e.ts".to_string()]);
    assert_eq!(reloaded.get_skill_paths(), vec!["/s".to_string()]);
    assert_eq!(
        reloaded.get_prompt_template_paths(),
        vec!["/p.md".to_string()]
    );
    assert_eq!(reloaded.get_theme_paths(), vec!["/t.json".to_string()]);
    assert!(!reloaded.get_enable_skill_commands());
    assert!(!reloaded.get_show_images());
    assert_eq!(reloaded.get_image_width_cells(), 5.0);
    assert!(reloaded.get_clear_on_shrink());
    assert!(reloaded.get_show_terminal_progress());
    assert_eq!(reloaded.get_tui_mode(), TuiMode::Regular);
    assert_eq!(
        reloaded.get_fullscreen_exit_output(),
        FullscreenExitOutput::ResumeHint
    );
    assert_eq!(
        reloaded.get_fullscreen_scrollbar(),
        ScrollViewScrollbar::Always
    );
    assert!(!reloaded.get_fullscreen_copy_on_select());
    assert_eq!(
        reloaded.get_fullscreen_wheel_scroll_lines(),
        WheelScrollLines::Fixed(7.0)
    );
    assert!(!reloaded.get_image_auto_resize());
    assert!(reloaded.get_block_images());
    assert_eq!(
        reloaded.get_enabled_models(),
        Some(vec!["claude*".to_string()])
    );
    assert_eq!(
        reloaded.get_double_escape_action(),
        DoubleEscapeAction::None
    );
    assert_eq!(reloaded.get_tree_filter_mode(), TreeFilterMode::All);
    assert!(reloaded.get_show_hardware_cursor());
    assert_eq!(reloaded.get_editor_padding_x(), 3.0);
    assert_eq!(reloaded.get_output_pad(), 0);
    assert_eq!(reloaded.get_autocomplete_max_visible(), 3.0);
    assert_eq!(
        reloaded.get_mermaid_rendering_mode(),
        MermaidRenderingMode::Off
    );
    assert_eq!(reloaded.get_warnings().anthropic_extra_usage, Some(false));
    assert_eq!(reloaded.get_http_idle_timeout_ms().expect("valid"), 45000.0);
}

// TS 实测：加载失败的 settings.json 不会被后续 setter 覆盖（`save()` 在
// `globalSettingsLoadError` 非空时提前返回），错误只在 load 时记录一次。
#[tokio::test]
async fn does_not_overwrite_a_settings_file_that_failed_to_load() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();
    write_file(&settings_path, "{ invalid json");

    let manager = dirs.manager();
    manager.set_theme("light".to_string());
    manager.flush().await;

    assert_eq!(read_text(&settings_path), "{ invalid json");
    let errors = manager.drain_errors();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].scope, SettingsScope::Global);
    // V8 的 JSON.parse 文案由 kiana-json 逐字复刻。
    assert_eq!(
        errors[0].error,
        "Expected property name or '}' in JSON at position 2 (line 1 column 3)"
    );
}

// 下游（`kiana-session` / `kiana-interactive`）会把 manager 放进 `Arc` 并跨任务共享：
// Rust 侧所有方法只借用 `&self`，因此必须 `Send + Sync`。
#[test]
fn settings_manager_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<SettingsManager>();
    assert_send_sync::<kiana_config::settings::SettingsDiagnostic>();
}

// ---------------------------------------------------------------------------
// 审计回归：类型与 schema 不符的已知设置（TS getter 直接返回原始值）
// ---------------------------------------------------------------------------
//
// 期望值全部来自 `upstream-reference` 的差分 probe：TS 读取 `JSON.parse` 的原始对象，`??` 只在
// null/undefined 时回落默认值；Rust 的类型化视图把类型不符的值放进 `extra`，getter
// 必须回落到原始值（布尔按 JS truthiness、数字按 `Number(value)`、数组按元素逐个解析）。

// TS: `getOrCreateDeviceId()` 的 `if (!this.globalSettings.deviceId)`：`""` 是 falsy。
#[tokio::test]
async fn replaces_an_empty_device_id_with_a_new_uuid() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();
    write_json(&settings_path, &json!({ "deviceId": "" }));

    let manager = dirs.manager();
    let device_id = manager.get_or_create_device_id();
    assert_eq!(device_id.len(), 36, "expected a UUID, got {device_id:?}");
    assert_ne!(device_id, "");
    manager.flush().await;

    assert_eq!(read_json(&settings_path), json!({ "deviceId": device_id }));

    // 第二次调用直接返回已持久化的值，不再改写文件。
    assert_eq!(manager.get_or_create_device_id(), device_id);
    assert_eq!(read_json(&settings_path), json!({ "deviceId": device_id }));
}

// TS: `setEnableAnalytics(true)` 的 `if (enabled && !this.globalSettings.trackingId)`：
// `""` 是 falsy，生成并持久化新的 tracking id。
#[tokio::test]
async fn replaces_an_empty_tracking_id_when_analytics_is_enabled() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();
    write_json(&settings_path, &json!({ "trackingId": "" }));

    let manager = dirs.manager();
    assert_eq!(manager.get_tracking_id(), Some(String::new()));
    manager.set_enable_analytics(true);
    let tracking_id = manager.get_tracking_id().expect("tracking id");
    assert_eq!(
        tracking_id.len(),
        36,
        "expected a UUID, got {tracking_id:?}"
    );
    manager.flush().await;

    // 已存在的键保持原位置（`trackingId` 在文件里排在 `enableAnalytics` 前面）。
    assert_eq!(
        read_text(&settings_path),
        format!("{{\n  \"trackingId\": \"{tracking_id}\",\n  \"enableAnalytics\": true\n}}")
    );
}

// 类型不符的布尔/数字：TS 的 getter 返回 `JSON.parse` 的**原始值**（例如
// `getRetrySettings().maxRetries === "4"`，类型声明是谎话），消费方随后按 truthiness /
// 算术使用。Rust 的 getter 返回值是冻结的类型化签名，无法携带原始 JSON，因此这里刻意
// 按 JS 的消费语义近似：布尔用 `Boolean(value)`，数字用 `Number(value)`（`"4"` → 4，
// 与 TS 里 `"4" * 1` 相同）。原始值本身仍保留在 `getSettings()` 的 `extra` 里。
#[test]
fn mismatched_scalar_types_follow_js_truthiness() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({
            "compaction": { "enabled": 0 },
            "retry": {
                "enabled": 0,
                "maxRetries": "4",
                "baseDelayMs": "1000",
                "maxAgentDelayMs": "2000",
                "provider": { "timeoutMs": "30000", "maxRetries": "2" },
            },
            "branchSummary": { "reserveTokens": "1024", "skipPrompt": "yes" },
            "hideThinkingBlock": "yes",
            "terminal": { "showImages": 0, "clearOnShrink": 0, "showTerminalProgress": 1 },
            "images": { "autoResize": 0, "blockImages": 0 },
            "enableInstallTelemetry": 0,
            "showCacheMissNotices": 1,
            "collapseChangelog": 0,
            "enableAnalytics": 0,
            "enableSkillCommands": "yes",
            "fullscreenCopyOnSelect": 0,
            "editorPaddingX": "2",
            "autocompleteMaxVisible": "7",
        }),
    );

    let manager = dirs.manager();
    assert!(!manager.get_compaction_enabled()); // TS: 0（falsy）
    assert!(!manager.get_retry_enabled()); // TS: 0（falsy）
    assert_eq!(
        manager.get_retry_settings(),
        ResolvedRetrySettings {
            enabled: false,
            max_retries: 4.0,
            base_delay_ms: 1000.0,
            max_agent_delay_ms: 2000.0,
        }
    );
    let provider = manager.get_provider_retry_settings();
    assert_eq!(provider.timeout_ms, Some(30000.0));
    assert_eq!(provider.max_retries, Some(2.0));
    assert_eq!(provider.max_retry_delay_ms, 60000.0);
    let branch_summary = manager.get_branch_summary_settings();
    assert_eq!(branch_summary.reserve_tokens, 1024.0);
    assert!(branch_summary.skip_prompt);
    assert!(manager.get_hide_thinking_block()); // TS: "yes"（truthy）
    assert!(!manager.get_show_images()); // TS: 0（falsy）
    assert!(!manager.get_clear_on_shrink()); // TS: 0（falsy）
    assert!(manager.get_show_terminal_progress());
    assert!(!manager.get_image_auto_resize());
    assert!(!manager.get_block_images());
    assert!(!manager.get_enable_install_telemetry());
    assert!(manager.get_show_cache_miss_notices());
    assert!(!manager.get_collapse_changelog());
    assert!(!manager.get_enable_analytics());
    assert!(manager.get_enable_skill_commands());
    assert!(!manager.get_fullscreen_copy_on_select());
    assert_eq!(manager.get_editor_padding_x(), 2.0);
    assert_eq!(manager.get_autocomplete_max_visible(), 7.0);

    // 数字字符串的超时设置：`parseHttpIdleTimeoutMs` 接受 `Number(string)`。
    let timeout_dirs = TestDirs::new();
    write_json(
        &timeout_dirs.global_settings_path(),
        &json!({ "websocketConnectTimeoutMs": " 45.9 " }),
    );
    let timeout_manager = timeout_dirs.manager();
    assert_eq!(
        timeout_manager
            .get_websocket_connect_timeout_ms()
            .expect("valid"),
        Some(45.0)
    );
}

// 父级容器本身类型不符时，TS 的可选链得到 undefined，getter 回落默认值。
#[test]
fn mismatched_container_types_fall_back_to_defaults() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({
            "compaction": 7,
            "retry": [1],
            "terminal": 5,
            "images": "x",
        }),
    );

    let manager = dirs.manager();
    assert!(manager.get_compaction_enabled());
    assert!(manager.get_retry_enabled());
    assert!(manager.get_show_images());
    assert!(manager.get_image_auto_resize());
    assert!(!manager.get_block_images());
    assert_eq!(manager.get_image_width_cells(), 60.0);
    assert_eq!(manager.get_retry_settings().max_retries, 3.0);
    assert_eq!(manager.get_provider_retry_settings().timeout_ms, None);
}

// TS 原样返回数组（包括类型不符的元素）；Rust 的类型化 Vec 只能保留能解析的元素，但不再
// 因为单个坏元素丢弃整份配置。
#[test]
fn mismatched_array_elements_keep_the_valid_entries() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({
            "packages": ["npm:x", 42],
            "extensions": ["/a", 3],
            "skills": ["/s", 7],
            "prompts": ["/p", 8],
            "themes": ["/t", 9],
            "enabledModels": ["claude*", 42],
            "npmCommand": ["npm", 5],
            "defaultTools": ["read", 5],
        }),
    );

    let manager = dirs.manager();
    assert_eq!(
        manager.get_packages(),
        vec![PackageSource::Text("npm:x".to_string())]
    );
    assert_eq!(manager.get_extension_paths(), vec!["/a".to_string()]);
    assert_eq!(manager.get_skill_paths(), vec!["/s".to_string()]);
    assert_eq!(manager.get_prompt_template_paths(), vec!["/p".to_string()]);
    assert_eq!(manager.get_theme_paths(), vec!["/t".to_string()]);
    assert_eq!(
        manager.get_enabled_models(),
        Some(vec!["claude*".to_string()])
    );
    assert_eq!(manager.get_npm_command(), Some(vec!["npm".to_string()]));
    // `getDefaultTools` 过滤非字符串后解析（TS 同款行为）。
    assert_eq!(manager.get_default_tools(), Some(vec!["read".to_string()]));
}

// 整张 `modelThinkingLevels` 因一个非法值无法解析时，合法的模型条目仍然生效；setter 在
// 原始对象上写入/删除，不会丢掉其它条目（TS 直接操作 `globalSettings.modelThinkingLevels`）。
#[tokio::test]
async fn mismatched_model_thinking_levels_keep_the_valid_entries() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({ "modelThinkingLevels": { "p/a": "high", "p/b": "bogus" } }),
    );

    let manager = dirs.manager();
    assert_eq!(
        manager.get_model_thinking_level("p", "a"),
        Some(ModelThinkingLevel::High)
    );
    // TS 原样返回 "bogus"；Rust 的枚举容纳不了非法值，按 `undefined` 处理。
    assert_eq!(manager.get_model_thinking_level("p", "b"), None);
    assert_eq!(
        manager
            .get_all_model_thinking_levels()
            .into_iter()
            .collect::<Vec<_>>(),
        vec![("p/a".to_string(), ModelThinkingLevel::High)]
    );

    manager
        .set_model_thinking_level("p", "c", ModelThinkingLevel::Low)
        .expect("valid thinking levels parent");
    manager.flush().await;
    assert_eq!(
        read_json(&dirs.global_settings_path()),
        json!({ "modelThinkingLevels": { "p/a": "high", "p/b": "bogus", "p/c": "low" } })
    );
    assert_eq!(
        manager.get_model_thinking_level("p", "c"),
        Some(ModelThinkingLevel::Low)
    );

    manager.remove_model_thinking_level("p", "a");
    manager.flush().await;
    assert_eq!(
        read_json(&dirs.global_settings_path()),
        json!({ "modelThinkingLevels": { "p/b": "bogus", "p/c": "low" } })
    );
}

// `loadFromStorage` 只做 `JSON.parse`；`deepMergeSettings` 的 `{...base}` 把顶层数组展开成
// 下标键对象（`[1,2]` → `{"0":1,"1":2}`），`getSettings()` 能看到这些键。
//
// 已知的类型级偏差：TS `getGlobalSettings()` 返回 `structuredClone` 的**数组** `[1,2]`；
// Rust 的返回类型是 `Settings`，只能表示展开后的对象形式（与 `getSettings()` 一致）。
#[test]
fn spreads_a_top_level_array_into_index_keys() {
    let dirs = TestDirs::new();
    write_file(&dirs.global_settings_path(), "[1,2]");

    let manager = dirs.manager();
    assert_eq!(
        serde_json::to_value(manager.get_settings()).expect("json"),
        json!({ "0": 1, "1": 2 })
    );
    // TS: `[1,2]`（数组）；Rust 的类型化返回只能给出同一个展开对象。
    assert_eq!(
        serde_json::to_value(manager.get_global_settings()).expect("json"),
        json!({ "0": 1, "1": 2 })
    );
    assert!(manager.drain_errors().is_empty());
}

// 写队列不依赖调用方的句柄：最后一个 `SettingsManager` 被 drop 后，已入队的写任务仍会写盘
// （TS 的 Promise 闭包捕获 `this`，GC 不会在任务完成前回收）。
#[tokio::test]
async fn queued_writes_run_after_the_last_manager_is_dropped() {
    let dirs = TestDirs::new();
    let manager = dirs.manager();
    manager.set_theme("light".to_string());
    drop(manager);
    tokio::time::sleep(Duration::from_millis(150)).await;

    assert_eq!(
        read_text(&dirs.global_settings_path()),
        "{\n  \"theme\": \"light\"\n}"
    );
}

// ---------------------------------------------------------------------------
// 并发写队列（审计回归）
// ---------------------------------------------------------------------------

/// 记录每个作用域最后一次写入的内存存储。
#[derive(Default)]
struct RecordingStorage {
    global: Mutex<Option<String>>,
    project: Mutex<Option<String>>,
    writes: Mutex<usize>,
}

impl RecordingStorage {
    fn slot(&self, scope: SettingsScope) -> &Mutex<Option<String>> {
        match scope {
            SettingsScope::Global => &self.global,
            SettingsScope::Project => &self.project,
        }
    }

    fn text(&self, scope: SettingsScope) -> Option<String> {
        self.slot(scope)
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn writes(&self) -> usize {
        *self
            .writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl SettingsStorage for RecordingStorage {
    fn with_lock(
        &self,
        scope: SettingsScope,
        f: &mut kiana_config::settings::SettingsLockCallback<'_>,
    ) -> Result<(), kiana_config::settings::SettingsStorageError> {
        let mut slot = self
            .slot(scope)
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let next = f(slot.as_deref())?;
        if let Some(next) = next {
            *slot = Some(next);
            *self
                .writes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) += 1;
        }
        Ok(())
    }
}

// 审计发现的 critical：setter 曾在两段临界区里「标记 modified」再「快照入队」，写 worker
// 的 clearModifiedScope 可能插在中间，导致这次修改以空字段集入队、永远不落盘。多线程
// runtime 下反复并发写同一个字段，最终磁盘内容必须等于内存里的最后值。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_setters_are_never_dropped_from_the_write_queue() {
    const BLOCKS: usize = 200;
    const TASKS: usize = 4;
    const SETS: usize = 400;

    for block in 0..BLOCKS {
        let storage = Arc::new(RecordingStorage::default());
        let manager = SettingsManager::from_storage(storage.clone());
        let mut handles = Vec::new();
        for task in 0..TASKS {
            let manager = manager.clone();
            handles.push(tokio::spawn(async move {
                for index in 0..SETS {
                    manager.set_theme(format!("t{task}-{index}"));
                    tokio::task::yield_now().await;
                }
            }));
        }
        for handle in handles {
            handle.await.expect("join setter task");
        }
        manager.flush().await;

        let in_memory = manager.get_theme().expect("theme after setters");
        let stored = storage
            .text(SettingsScope::Global)
            .expect("stored settings");
        let stored_value: Value = serde_json::from_str(&stored).expect("stored json");
        assert_eq!(
            stored_value["theme"],
            json!(in_memory),
            "block {block}: the last set_theme call is missing from settings.json: {stored}"
        );
        assert!(
            storage.writes() > 0,
            "block {block}: no writes were recorded"
        );
    }
}

// 同样的窗口存在于 project 变体（updateProjectSettings 先标记再 saveProjectSettings）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_project_setters_are_never_dropped_from_the_write_queue() {
    const BLOCKS: usize = 100;
    const TASKS: usize = 4;
    const SETS: usize = 200;

    for block in 0..BLOCKS {
        let storage = Arc::new(RecordingStorage::default());
        let manager = SettingsManager::from_storage(storage.clone());
        let mut handles = Vec::new();
        for task in 0..TASKS {
            let manager = manager.clone();
            handles.push(tokio::spawn(async move {
                for index in 0..SETS {
                    manager
                        .set_project_theme_paths(vec![format!("/t{task}/{index}")])
                        .expect("project settings are trusted");
                    tokio::task::yield_now().await;
                }
            }));
        }
        for handle in handles {
            handle.await.expect("join setter task");
        }
        manager.flush().await;

        let in_memory = manager.get_theme_paths();
        let stored = storage
            .text(SettingsScope::Project)
            .expect("stored settings");
        let stored_value: Value = serde_json::from_str(&stored).expect("stored json");
        assert_eq!(
            stored_value["themes"],
            json!(in_memory),
            "block {block}: the last set_project_theme_paths call is missing: {stored}"
        );
    }
}

// ---------------------------------------------------------------------------
// 深度合并：一侧整体无法解析时仍按条目合并（审计回归）
// ---------------------------------------------------------------------------

// TS `deepMergeObjects` 对嵌套对象逐键合并：project 里某个非法条目不会丢掉 global 的
// 合法条目（`modelThinkingLevels` 整张 record 无法解析时也一样）。
#[test]
fn merges_nested_objects_per_key_when_one_layer_has_invalid_entries() {
    // global 合法、project 含非法值。
    {
        let dirs = TestDirs::new();
        write_json(
            &dirs.global_settings_path(),
            &json!({ "modelThinkingLevels": { "p/a": "high" } }),
        );
        write_json(
            &dirs.project_settings_path(),
            &json!({ "modelThinkingLevels": { "p/b": "bogus" } }),
        );
        let manager = dirs.manager();
        assert_eq!(
            manager.get_model_thinking_level("p", "a"),
            Some(ModelThinkingLevel::High)
        );
        assert_eq!(
            manager
                .get_all_model_thinking_levels()
                .into_iter()
                .collect::<Vec<_>>(),
            vec![("p/a".to_string(), ModelThinkingLevel::High)]
        );
    }

    // project 合法、global 含非法值（反方向）。
    {
        let dirs = TestDirs::new();
        write_json(
            &dirs.global_settings_path(),
            &json!({ "modelThinkingLevels": { "p/x": "bogus" } }),
        );
        write_json(
            &dirs.project_settings_path(),
            &json!({ "modelThinkingLevels": { "p/a": "high" } }),
        );
        let manager = dirs.manager();
        assert_eq!(
            manager.get_model_thinking_level("p", "a"),
            Some(ModelThinkingLevel::High)
        );
    }
}

// compaction.modelOverrides 是嵌套 record：project 的非法条目（数字）不能丢掉 global 的
// 合法 override；反方向同理。默认值仍是普通值 → 16384。
#[test]
fn merges_compaction_model_overrides_per_key_when_one_side_is_invalid() {
    use kiana_config::settings::CompactionModelRef;

    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({ "compaction": { "modelOverrides": { "p/a": { "reserveTokens": 111 } } } }),
    );
    write_json(
        &dirs.project_settings_path(),
        &json!({ "compaction": { "modelOverrides": { "p/b": 7 } } }),
    );
    let manager = dirs.manager();
    assert_eq!(
        manager
            .get_compaction_reserve_tokens(Some(CompactionModelRef::new("p", "a")))
            .expect("valid override"),
        111.0
    );
    assert_eq!(
        manager
            .get_compaction_reserve_tokens(None)
            .expect("default"),
        16384.0
    );
    assert_eq!(
        manager
            .get_compaction_reserve_tokens(Some(CompactionModelRef::new("p", "b")))
            .expect_err("non-object entry")
            .to_string(),
        "Invalid compaction.modelOverrides[\"p/b\"] setting: 7. Expected an object."
    );

    // 反方向：global 的条目非法，project 的合法条目必须保留。
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({ "compaction": { "modelOverrides": { "p/x": 7 } } }),
    );
    write_json(
        &dirs.project_settings_path(),
        &json!({ "compaction": { "modelOverrides": { "p/a": { "reserveTokens": 222 } } } }),
    );
    let manager = dirs.manager();
    assert_eq!(
        manager
            .get_compaction_reserve_tokens(Some(CompactionModelRef::new("p", "a")))
            .expect("valid override"),
        222.0
    );
}

// TS 的 `[...string]` 按码点展开：数组型 getter 对字符串设置返回字符数组。
#[test]
fn spreads_string_values_for_array_settings_like_js() {
    let dirs = TestDirs::new();
    write_json(
        &dirs.global_settings_path(),
        &json!({
            "npmCommand": "npm",
            "themes": "nope",
            "extensions": "abc",
            "skills": "/s",
            "prompts": "",
            "packages": "npm:x",
            "enabledModels": "not-an-array",
        }),
    );
    let manager = dirs.manager();
    assert_eq!(
        manager.get_npm_command(),
        Some(vec!["n".to_string(), "p".to_string(), "m".to_string()])
    );
    assert_eq!(
        manager.get_theme_paths(),
        vec![
            "n".to_string(),
            "o".to_string(),
            "p".to_string(),
            "e".to_string()
        ]
    );
    assert_eq!(
        manager.get_extension_paths(),
        vec!["a".to_string(), "b".to_string(), "c".to_string()]
    );
    assert_eq!(
        manager.get_skill_paths(),
        vec!["/".to_string(), "s".to_string()]
    );
    assert!(manager.get_prompt_template_paths().is_empty());
    assert_eq!(
        manager.get_packages(),
        "npm:x"
            .chars()
            .map(|character| PackageSource::Text(character.to_string()))
            .collect::<Vec<_>>()
    );
    // TS `getEnabledModels()` 返回原始字符串 `"not-an-array"`；Rust 的
    // `Option<Vec<String>>` 表示不了非数组值（类型级偏差，见模块头注释）。
    assert_eq!(manager.get_enabled_models(), None);
}

// TS 在 truthy 的原始父值上赋值会同步抛 `TypeError`（严格模式）。Rust 的 setter 用
// `Result<(), SettingsValueError>` 表达同一个失败：错误文案逐字对齐 V8，内存与磁盘都不变
// （与 TS 抛错后 `markModified`/`save()` 都没执行一致）。upstream-reference 实测：
// `Cannot create property 'showImages' on number '5'`。
#[tokio::test]
async fn nested_setters_report_truthy_primitive_parents_as_type_errors() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();
    let original = json!({
        "terminal": 5,
        "compaction": "x",
        "retry": true,
        "images": 3.5,
        "markdown": 0,
        "modelThinkingLevels": 5,
        "theme": "dark",
    });
    write_json(&settings_path, &original);

    let manager = dirs.manager();
    let message = |result: Result<(), kiana_config::settings::SettingsValueError>| {
        result
            .expect_err("truthy primitive parent must fail")
            .message()
            .to_string()
    };

    assert_eq!(
        message(manager.set_show_images(false)),
        "Cannot create property 'showImages' on number '5'"
    );
    assert_eq!(
        message(manager.set_image_width_cells(5.9)),
        "Cannot create property 'imageWidthCells' on number '5'"
    );
    assert_eq!(
        message(manager.set_clear_on_shrink(true)),
        "Cannot create property 'clearOnShrink' on number '5'"
    );
    assert_eq!(
        message(manager.set_show_terminal_progress(true)),
        "Cannot create property 'showTerminalProgress' on number '5'"
    );
    assert_eq!(
        message(manager.set_compaction_enabled(false)),
        "Cannot create property 'enabled' on string 'x'"
    );
    assert_eq!(
        message(manager.set_retry_enabled(false)),
        "Cannot create property 'enabled' on boolean 'true'"
    );
    assert_eq!(
        message(manager.set_image_auto_resize(false)),
        "Cannot create property 'autoResize' on number '3.5'"
    );
    assert_eq!(
        message(manager.set_block_images(true)),
        "Cannot create property 'blockImages' on number '3.5'"
    );
    // `markdown ??= {}` 只替换 null/缺失，falsy 的 `0` 也会抛错（与 `if (!x)` 的写法不同）。
    assert_eq!(
        message(manager.set_mermaid_rendering_mode(MermaidRenderingMode::Off)),
        "Cannot create property 'mermaid' on number '0'"
    );
    assert_eq!(
        message(manager.set_model_thinking_level("p", "m", ModelThinkingLevel::High)),
        "Cannot create property 'p/m' on number '5'"
    );
    manager.flush().await;

    // 失败路径不写盘、不改内存（TS 的 TypeError 发生在 markModified/save 之前）。
    assert_eq!(read_json(&settings_path), original);
    assert!(manager.get_show_images());
    assert!(!manager.get_clear_on_shrink());
    assert_eq!(
        manager.get_mermaid_rendering_mode(),
        MermaidRenderingMode::Streaming
    );
    assert_eq!(manager.get_model_thinking_level("p", "m"), None);
}

// TS：`if (!this.globalSettings.terminal) this.globalSettings.terminal = {}` 会替换 falsy
// 原始值，随后赋值成功并落盘。
#[tokio::test]
async fn nested_setters_replace_falsy_primitive_parents_with_an_object() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();
    write_json(
        &settings_path,
        &json!({ "terminal": 0, "images": false, "modelThinkingLevels": "" }),
    );

    let manager = dirs.manager();
    manager
        .set_show_images(false)
        .expect("falsy terminal is replaced");
    manager
        .set_image_auto_resize(false)
        .expect("falsy images is replaced");
    manager
        .set_model_thinking_level("p", "m", ModelThinkingLevel::Low)
        .expect("falsy modelThinkingLevels is replaced");
    manager.flush().await;

    assert_eq!(
        read_json(&settings_path),
        json!({
            "terminal": { "showImages": false },
            "images": { "autoResize": false },
            "modelThinkingLevels": { "p/m": "low" },
        })
    );
}

// `markdown ??= {}` 只替换 null/缺失；对象/数组是合法赋值目标。
#[tokio::test]
async fn mermaid_rendering_mode_replaces_only_nullish_parents() {
    for (parent, expected) in [
        (json!(null), json!({ "mermaid": "off" })),
        (json!([]), json!({ "mermaid": "off" })),
        (
            json!({ "codeBlockIndent": "  " }),
            json!({ "codeBlockIndent": "  ", "mermaid": "off" }),
        ),
    ] {
        let dirs = TestDirs::new();
        let settings_path = dirs.global_settings_path();
        write_json(&settings_path, &json!({ "markdown": parent }));

        let manager = dirs.manager();
        manager
            .set_mermaid_rendering_mode(MermaidRenderingMode::Off)
            .expect("nullish/object/array parent is assignable");
        manager.flush().await;

        assert_eq!(read_json(&settings_path), json!({ "markdown": expected }));
    }
}

// TS：`modelThinkingLevels` 是数组时是真值对象，赋值只给数组加一个不可序列化的属性；
// `JSON.stringify` 后文件里仍是原数组（重载后该级别丢失）。
#[tokio::test]
async fn setting_a_model_thinking_level_keeps_an_array_container_unchanged() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();
    write_json(&settings_path, &json!({ "modelThinkingLevels": [] }));

    let manager = dirs.manager();
    manager
        .set_model_thinking_level("p", "m", ModelThinkingLevel::High)
        .expect("array parent is assignable");
    manager.flush().await;

    assert_eq!(
        read_json(&settings_path),
        json!({ "modelThinkingLevels": [] })
    );
}

// TS：`compaction: []` 是真值对象，`compaction.enabled = false` 给数组加属性；选择性持久化
// 用 `{...baseNested}` 重建，因此文件写成一个对象（upstream-reference 实测）。
#[tokio::test]
async fn compaction_array_is_rebuilt_into_an_object_on_write() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();
    write_json(&settings_path, &json!({ "compaction": [] }));

    let manager = dirs.manager();
    manager
        .set_compaction_enabled(false)
        .expect("array parent is assignable");
    manager.flush().await;

    assert_eq!(
        read_json(&settings_path),
        json!({ "compaction": { "enabled": false } })
    );
}

// `persistScopedSettings` 的嵌套基值是 `{...currentFileSettings[field]}`：当前文件里的
// 字符串按索引展开（`{..."abc"}` → {"0":"a","1":"b","2":"c"}），与 JS 对象展开一致。
#[tokio::test]
async fn persist_spreads_a_string_base_value_like_js_object_spread() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();
    write_json(
        &settings_path,
        &json!({ "terminal": { "showImages": true } }),
    );

    let manager = dirs.manager();
    manager
        .set_show_images(false)
        .expect("object parent is assignable");
    // 写任务尚未执行（current-thread runtime 只在 await 点推进 worker），模拟外部编辑
    // 把同一字段改成字符串。
    write_json(&settings_path, &json!({ "terminal": "abc" }));
    manager.flush().await;

    assert_eq!(
        read_json(&settings_path),
        json!({ "terminal": { "0": "a", "1": "b", "2": "c", "showImages": false } })
    );
}

// 存储错误文案走 Node/libuv 的 `CODE: message, syscall`（审计回归）：`settings.json` 是
// 目录时，Node 的 `readFileSync` 报 `EISDIR: illegal operation on a directory, read`
//（read 阶段、不带路径）。
#[test]
fn reports_directory_read_errors_with_node_messages() {
    let dirs = TestDirs::new();
    let settings_path = dirs.global_settings_path();
    fs::create_dir_all(&settings_path).expect("settings.json as a directory");

    let manager = dirs.manager();
    let errors = manager.drain_errors();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].scope, SettingsScope::Global);
    assert_eq!(
        errors[0].path.as_deref(),
        Some(settings_path.to_string_lossy().as_ref())
    );
    assert_eq!(
        errors[0].error,
        "EISDIR: illegal operation on a directory, read"
    );
}
