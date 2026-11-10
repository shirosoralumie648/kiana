//! `packages/coding-agent/test/settings-diagnostics.test.ts` 的移植（3 个用例）。
//!
//! TS 的 `AgentSessionRuntimeDiagnostic` 在 `kiana-session`；这里用
//! [`SettingsDiagnostic`] + [`DiagnosticLike`] 复刻同样的数据结构与去重语义。

use std::fs;
use std::sync::Arc;

use kiana_config::settings::diagnostics::{
    DiagnosticKind, DiagnosticLike, SettingsDiagnostic, collect_settings_diagnostics,
    deduplicate_diagnostics,
};
use kiana_config::settings::{
    SettingsLockCallback, SettingsManager, SettingsScope, SettingsStorage,
};

// TS: "includes the settings file path for file-backed storage"
#[test]
fn includes_the_settings_file_path_for_file_backed_storage() {
    let temp = tempfile::tempdir().expect("tempdir");
    let agent_dir = temp.path().join("agent");
    let settings_path = agent_dir.join("settings.json");
    fs::create_dir_all(&agent_dir).expect("agent dir");
    fs::write(&settings_path, "{").expect("write settings");

    let diagnostics = collect_settings_diagnostics(&SettingsManager::create(
        temp.path().to_str().expect("utf-8"),
        agent_dir.to_str().expect("utf-8"),
    ));

    assert_eq!(diagnostics.len(), 1);
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic.kind, DiagnosticKind::Warning);
    assert_eq!(diagnostic.diagnostic_kind(), DiagnosticKind::Warning);
    assert!(
        diagnostic.message.contains(&format!(
            "Invalid settings file {}:",
            settings_path.to_str().expect("utf-8")
        )),
        "unexpected message: {}",
        diagnostic.message
    );
}

/// TS 测试里的匿名 storage：global 直接抛错，project 正常。
struct FailingGlobalStorage;

impl SettingsStorage for FailingGlobalStorage {
    fn with_lock(
        &self,
        scope: SettingsScope,
        f: &mut SettingsLockCallback<'_>,
    ) -> Result<(), kiana_config::settings::SettingsStorageError> {
        if scope == SettingsScope::Global {
            return Err(Box::new(std::io::Error::other("backend failed")));
        }
        f(None)?;
        Ok(())
    }
}

// TS: "falls back to the settings scope for storage without file paths"
#[test]
fn falls_back_to_the_settings_scope_for_storage_without_file_paths() {
    let storage: Arc<dyn SettingsStorage> = Arc::new(FailingGlobalStorage);
    let diagnostics = collect_settings_diagnostics(&SettingsManager::from_storage(storage));

    assert_eq!(
        diagnostics,
        vec![SettingsDiagnostic {
            kind: DiagnosticKind::Warning,
            message: "Invalid global settings: backend failed".to_string(),
        }]
    );
}

// TS: "deduplicates diagnostics by type and message"
#[test]
fn deduplicates_diagnostics_by_type_and_message() {
    let warning = SettingsDiagnostic {
        kind: DiagnosticKind::Warning,
        message: "Invalid settings file /tmp/settings.json".to_string(),
    };
    let error = SettingsDiagnostic {
        kind: DiagnosticKind::Error,
        ..warning.clone()
    };

    assert_eq!(
        deduplicate_diagnostics(&[warning.clone(), warning.clone(), error.clone()]),
        vec![warning, error]
    );
}
