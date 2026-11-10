//! `packages/coding-agent/test/suite/regressions/3616-settings-inmemory-reload.test.ts`
//! 的两个纯 `SettingsManager` 用例。
//!
//! 第三个用例（`preserves initial settings when DefaultResourceLoader reloads`）依赖
//! `DefaultResourceLoader`（`kiana-resources`，阶段 5），不属于本 crate；它只是调用
//! `SettingsManager.reload()`，语义已被前两个用例覆盖。

use kiana_ai::ModelThinkingLevel;
use kiana_config::settings::SettingsManager;
use kiana_config::settings::schema::Settings;
use serde_json::json;

fn in_memory(settings: serde_json::Value) -> SettingsManager {
    SettingsManager::in_memory(serde_json::from_value::<Settings>(settings).expect("settings"))
}

fn expected(settings: serde_json::Value) -> Settings {
    serde_json::from_value::<Settings>(settings).expect("settings")
}

// TS: "preserves initial settings after direct reload"
#[tokio::test]
async fn preserves_initial_settings_after_direct_reload() {
    let settings_manager = in_memory(json!({
        "defaultThinkingLevel": "high",
        "images": { "autoResize": false },
        "compaction": { "enabled": false },
    }));

    settings_manager.reload().await;

    assert_eq!(
        settings_manager.get_default_thinking_level(),
        Some(ModelThinkingLevel::High)
    );
    assert!(!settings_manager.get_image_auto_resize());
    assert!(!settings_manager.get_compaction_enabled());
    assert_eq!(
        settings_manager.get_global_settings(),
        expected(json!({
            "defaultThinkingLevel": "high",
            "images": { "autoResize": false },
            "compaction": { "enabled": false },
        }))
    );
}

// TS: "preserves initial settings after an unrelated setter, flush, and reload"
#[tokio::test]
async fn preserves_initial_settings_after_an_unrelated_setter_flush_and_reload() {
    let settings_manager = in_memory(json!({
        "images": { "autoResize": false },
        "compaction": { "enabled": false },
    }));

    settings_manager.set_theme("dark".to_string());
    settings_manager.flush().await;
    settings_manager.reload().await;

    assert_eq!(settings_manager.get_theme(), Some("dark".to_string()));
    assert!(!settings_manager.get_image_auto_resize());
    assert!(!settings_manager.get_compaction_enabled());
    assert_eq!(
        settings_manager.get_global_settings(),
        expected(json!({
            "images": { "autoResize": false },
            "compaction": { "enabled": false },
            "theme": "dark",
        }))
    );
}
