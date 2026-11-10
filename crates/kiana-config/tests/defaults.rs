//! `core/defaults.ts` / `core/radius.ts` / `core/telemetry.ts` 的移植测试
//! （`ca-config-models.md` §2.1、§2.3、§2.4）。
//!
//! TS 侧没有与 §2.1/§2.3 直接对应的独立测试文件（`DEFAULT_THINKING_LEVEL` 与
//! `THINKING_LEVEL_OPTIONS` 通过 UI 与 model-resolver 间接覆盖）；这里按 spec 与 TS 源码
//! 逐项锁定。`radius.ts` 的 `KIANA_RADIUS_GATEWAY` 覆盖来自 `getRadiusGatewayUrl()` 的实现，
//! 由本文件覆盖（环境变量用例串行化）。
//!
//! `telemetry.ts` 的优先级来自 `isInstallTelemetryEnabled`：`KIANA_TELEMETRY` 只要已定义
//! （含空串）就完全覆盖 settings。

use std::sync::{Mutex, MutexGuard};

use kiana_agent_core::ThinkingLevel;
use kiana_config::defaults::{
    DEFAULT_THINKING_LEVEL, ENV_RADIUS_GATEWAY, RADIUS_MCP_URL, RADIUS_PROVIDER_ID,
    THINKING_LEVEL_OPTIONS, get_radius_gateway_url, is_install_telemetry_enabled,
    is_install_telemetry_enabled_with_env,
};
use kiana_config::settings::SettingsManager;
use kiana_config::settings::schema::Settings;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn serial_env() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn set_env(name: &str, value: Option<&str>) {
    // SAFETY: 本文件所有读写环境变量的用例都持有 `serial_env()` 的互斥锁。
    unsafe {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }
}

fn manager_with_telemetry(enabled: bool) -> SettingsManager {
    SettingsManager::in_memory(Settings {
        enable_install_telemetry: Some(enabled),
        ..Settings::default()
    })
}

// ---------------------------------------------------------------------------
// defaults.ts
// ---------------------------------------------------------------------------

#[test]
fn default_thinking_level_is_medium() {
    assert_eq!(DEFAULT_THINKING_LEVEL, ThinkingLevel::Medium);
}

#[test]
fn thinking_level_options_keep_ui_order() {
    assert_eq!(
        THINKING_LEVEL_OPTIONS,
        [
            ThinkingLevel::Off,
            ThinkingLevel::Minimal,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
            ThinkingLevel::Xhigh,
            ThinkingLevel::Max,
        ]
    );
    assert_eq!(THINKING_LEVEL_OPTIONS.len(), 7);
}

// ---------------------------------------------------------------------------
// radius.ts
// ---------------------------------------------------------------------------

#[test]
fn radius_constants_match_typescript() {
    assert_eq!(RADIUS_PROVIDER_ID, "radius");
    assert_eq!(ENV_RADIUS_GATEWAY, "KIANA_RADIUS_GATEWAY");
    assert_eq!(&*RADIUS_MCP_URL, "https://radius.kiana.dev/mcp");
}

#[test]
fn radius_gateway_url_defaults_and_honors_env_override() {
    let _guard = serial_env();
    let original = std::env::var(ENV_RADIUS_GATEWAY).ok();

    set_env(ENV_RADIUS_GATEWAY, None);
    assert_eq!(get_radius_gateway_url(), "https://radius.kiana.dev");

    set_env(ENV_RADIUS_GATEWAY, Some("https://example.test/"));
    assert_eq!(get_radius_gateway_url(), "https://example.test");

    // 没有 scheme 时补 `https://`，并去掉所有结尾 `/`。
    set_env(ENV_RADIUS_GATEWAY, Some("gateway.test//"));
    assert_eq!(get_radius_gateway_url(), "https://gateway.test");

    // 每次调用都重新读环境变量（不缓存）。
    set_env(ENV_RADIUS_GATEWAY, Some("http://other.test"));
    assert_eq!(get_radius_gateway_url(), "http://other.test");

    set_env(ENV_RADIUS_GATEWAY, original.as_deref());
}

// ---------------------------------------------------------------------------
// telemetry.ts
// ---------------------------------------------------------------------------

#[test]
fn telemetry_falls_back_to_settings_when_env_is_undefined() {
    assert!(is_install_telemetry_enabled_with_env(
        &manager_with_telemetry(true),
        None
    ));
    assert!(!is_install_telemetry_enabled_with_env(
        &manager_with_telemetry(false),
        None
    ));
}

#[test]
fn telemetry_env_fully_overrides_settings() {
    // 设置开着，但环境变量已定义 → 环境变量说了算。
    assert!(!is_install_telemetry_enabled_with_env(
        &manager_with_telemetry(true),
        Some("0")
    ));
    // 设置关着，但环境变量已定义 → 环境变量说了算。
    assert!(is_install_telemetry_enabled_with_env(
        &manager_with_telemetry(false),
        Some("1")
    ));
}

#[test]
fn telemetry_accepts_truthy_env_flags_case_insensitively() {
    for value in ["1", "true", "TRUE", "True", "yes", "YES", "Yes"] {
        assert!(
            is_install_telemetry_enabled_with_env(&manager_with_telemetry(false), Some(value)),
            "expected {value:?} to enable telemetry"
        );
    }
    for value in ["", "0", "false", "FALSE", "no", "2", "on"] {
        assert!(
            !is_install_telemetry_enabled_with_env(&manager_with_telemetry(true), Some(value)),
            "expected {value:?} to disable telemetry"
        );
    }
}

#[test]
fn telemetry_reads_kiana_telemetry_from_the_environment() {
    let _guard = serial_env();
    let original = std::env::var("KIANA_TELEMETRY").ok();

    set_env("KIANA_TELEMETRY", None);
    assert!(is_install_telemetry_enabled(&manager_with_telemetry(true)));

    // 空串也算“已定义” → false。
    set_env("KIANA_TELEMETRY", Some(""));
    assert!(!is_install_telemetry_enabled(&manager_with_telemetry(true)));

    set_env("KIANA_TELEMETRY", Some("yes"));
    assert!(is_install_telemetry_enabled(&manager_with_telemetry(false)));

    set_env("KIANA_TELEMETRY", original.as_deref());
}
