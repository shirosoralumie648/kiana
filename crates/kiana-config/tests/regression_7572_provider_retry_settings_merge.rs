//! `packages/coding-agent/test/suite/regressions/7572-provider-retry-settings-merge.test.ts`
//! 的移植（1 个用例）。
//!
//! 嵌套的 `retry.provider` 在 global/project 两层的合并必须逐字段进行：
//! project 只写 `maxRetries` 时，global 的 `timeoutMs` / `maxRetryDelayMs` 保留。

use std::sync::Arc;

use kiana_config::settings::{
    InMemorySettingsStorage, ResolvedProviderRetrySettings, SettingsManager, SettingsScope,
    SettingsStorage,
};

// TS: "preserves global provider settings not overridden by the project"
#[test]
fn preserves_global_provider_settings_not_overridden_by_the_project() {
    let storage = InMemorySettingsStorage::new();
    storage
        .with_lock(SettingsScope::Global, &mut |_| {
            Ok(Some(
                r#"{"retry":{"provider":{"timeoutMs":30000,"maxRetryDelayMs":45000}}}"#.to_string(),
            ))
        })
        .expect("seed global");
    storage
        .with_lock(SettingsScope::Project, &mut |_| {
            Ok(Some(
                r#"{"retry":{"provider":{"maxRetries":2}}}"#.to_string(),
            ))
        })
        .expect("seed project");

    let settings_manager =
        SettingsManager::from_storage(Arc::new(storage) as Arc<dyn SettingsStorage>);

    assert_eq!(
        settings_manager.get_provider_retry_settings(),
        ResolvedProviderRetrySettings {
            timeout_ms: Some(30000.0),
            max_retries: Some(2.0),
            max_retry_delay_ms: 45000.0,
        }
    );
}
