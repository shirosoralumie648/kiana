//! `test/first-time-setup.test.ts` 的 Rust 版本。
//!
//! - `shouldRunFirstTimeSetup` 的 4 个环境组合；
//! - `analytics settings`（`SettingsManager` 的默认值/生成/保留 tracking id）。
//!
//! `first-time-setup-fork.test.ts`（`vi.mock` 改写 `PACKAGE_NAME`）在
//! [`first_time_setup_fork`](first_time_setup_fork.rs) 里用可注入输入覆盖。

use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};

use kiana_coding_agent::cli::startup_ui::should_run_first_time_setup;
use kiana_config::settings::SettingsManager;
use kiana_core::config::ENV_AGENT_DIR;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn new() -> Self {
        let lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Self {
            _lock: lock,
            saved: Vec::new(),
        }
    }

    fn set(&mut self, key: &'static str, value: Option<&str>) {
        if !self.saved.iter().any(|(saved, _)| *saved == key) {
            self.saved.push((key, std::env::var_os(key)));
        }
        match value {
            Some(value) => unsafe { std::env::set_var(key, value) },
            None => unsafe { std::env::remove_var(key) },
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..).rev() {
            match value {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}

#[test]
fn returns_true_when_experimental_default_agent_dir_and_no_settings_json() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut guard = EnvGuard::new();
    guard.set("KIANA_EXPERIMENTAL", Some("1"));
    guard.set(ENV_AGENT_DIR, None);
    let settings_path = temp.path().join("settings.json");
    assert!(should_run_first_time_setup(&settings_path));
}

#[test]
fn returns_false_when_experimental_features_are_disabled() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut guard = EnvGuard::new();
    guard.set("KIANA_EXPERIMENTAL", None);
    guard.set(ENV_AGENT_DIR, None);
    let settings_path = temp.path().join("settings.json");
    assert!(!should_run_first_time_setup(&settings_path));
}

#[test]
fn returns_false_when_a_custom_agent_dir_is_set() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut guard = EnvGuard::new();
    guard.set("KIANA_EXPERIMENTAL", Some("1"));
    guard.set(
        ENV_AGENT_DIR,
        Some(temp.path().to_str().expect("utf-8 temp path")),
    );
    let settings_path = temp.path().join("settings.json");
    assert!(!should_run_first_time_setup(&settings_path));
}

#[test]
fn returns_false_when_settings_json_already_exists() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut guard = EnvGuard::new();
    guard.set("KIANA_EXPERIMENTAL", Some("1"));
    guard.set(ENV_AGENT_DIR, None);
    let settings_path = temp.path().join("settings.json");
    std::fs::write(&settings_path, "{}").expect("write settings");
    assert!(!should_run_first_time_setup(&settings_path));
}

// ---------------------------------------------------------------------------
// analytics settings
// ---------------------------------------------------------------------------

#[test]
fn analytics_defaults_to_disabled_with_no_tracking_identifier() {
    let manager = SettingsManager::in_memory(kiana_config::settings::Settings::default());
    assert!(!manager.get_enable_analytics());
    assert!(manager.get_tracking_id().is_none());
}

#[test]
fn generates_a_tracking_identifier_on_opt_in() {
    let manager = SettingsManager::in_memory(kiana_config::settings::Settings::default());
    manager.set_enable_analytics(true);
    assert!(manager.get_enable_analytics());
    let tracking_id = manager.get_tracking_id().expect("tracking id");
    assert_eq!(tracking_id.len(), 36, "{tracking_id}");
    assert!(
        tracking_id
            .chars()
            .all(|character| character == '-' || character.is_ascii_hexdigit()),
        "{tracking_id}"
    );
}

#[test]
fn does_not_generate_a_tracking_identifier_on_opt_out() {
    let manager = SettingsManager::in_memory(kiana_config::settings::Settings::default());
    manager.set_enable_analytics(false);
    assert!(!manager.get_enable_analytics());
    assert!(manager.get_tracking_id().is_none());
}

#[test]
fn keeps_the_tracking_identifier_when_toggling_analytics() {
    let manager = SettingsManager::in_memory(kiana_config::settings::Settings::default());
    manager.set_enable_analytics(true);
    let tracking_id = manager.get_tracking_id();
    manager.set_enable_analytics(false);
    manager.set_enable_analytics(true);
    assert_eq!(manager.get_tracking_id(), tracking_id);
}
