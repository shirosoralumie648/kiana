//! `SettingsManager::update_project_permissions` 的选择性持久化。
//!
//! 第 1 周的「总是允许（项目）」写回只能改 `permissions` 一个键；磁盘上的其它字段与
//! 键序必须保留，否则会覆盖用户/外部工具的修改。

use std::sync::Arc;

use kiana_config::settings::{
    FileSettingsStorage, SettingsManager, SettingsScope, SettingsStorage,
};
use serde_json::json;

/// 项目 settings 文件加载失败（损坏 JSON）时，写回必须失败而不是「报告成功但没写」。
#[tokio::test]
async fn project_permission_writeback_fails_when_the_project_file_cannot_be_loaded() {
    let dir = tempfile::tempdir().expect("temp");
    let agent_dir = dir.path().join("agent");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    std::fs::create_dir_all(project.join(".kiana")).expect("project config dir");
    let path = project.join(".kiana/settings.json");
    std::fs::write(&path, "{ not valid json").expect("seed corrupt project settings");

    let storage = Arc::new(FileSettingsStorage::new(
        &project.to_string_lossy(),
        &agent_dir.to_string_lossy(),
    ));
    let manager = SettingsManager::from_storage(storage.clone());

    let error = manager
        .update_project_permissions(|current| {
            let mut permissions = current.cloned().unwrap_or_else(|| json!({}));
            kiana_config::settings::permissions::append_rule_to_permissions(
                &mut permissions,
                &json!({"tool": "edit", "decision": "allow"}),
            )
            .expect("append rule");
            permissions
        })
        .expect_err("a corrupt project file must fail the writeback");
    assert!(
        error.message().contains("failed to load"),
        "unexpected error: {error}"
    );

    // 损坏的文件不能被覆盖。
    assert_eq!(
        std::fs::read_to_string(&path).expect("read project settings"),
        "{ not valid json"
    );
    // 内存里也不能出现这条规则（否则调用方会误以为已持久化）。
    assert!(manager.project_permissions().is_none());

    // 模拟重启：规则不存在，仍然需要重新批准。
    let restarted = SettingsManager::from_storage(storage);
    assert!(restarted.project_permissions().is_none());
    assert!(!restarted.drain_errors().is_empty());
}

#[tokio::test]
async fn project_permission_writeback_preserves_other_fields_and_key_order() {
    let dir = tempfile::tempdir().expect("temp");
    let agent_dir = dir.path().join("agent");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    std::fs::create_dir_all(project.join(".kiana")).expect("project config dir");
    let path = project.join(".kiana/settings.json");
    std::fs::write(
        &path,
        r#"{
  "theme": "dark",
  "permissions": {
    "mode": "default",
    "rules": [{"tool": "read", "decision": "allow"}]
  },
  "lastChangelogVersion": "1.0.0"
}"#,
    )
    .expect("seed project settings");

    let storage = Arc::new(FileSettingsStorage::new(
        &project.to_string_lossy(),
        &agent_dir.to_string_lossy(),
    ));
    let manager = SettingsManager::from_storage(storage);
    manager
        .update_project_permissions(|current| {
            let mut permissions = current.cloned().unwrap_or_else(|| json!({}));
            kiana_config::settings::permissions::append_rule_to_permissions(
                &mut permissions,
                &json!({"command": ["git", "status"], "decision": "allow"}),
            )
            .expect("append rule");
            permissions
        })
        .expect("write project permissions");
    manager.flush().await;

    let text = std::fs::read_to_string(&path).expect("read project settings");
    let theme_index = text.find("\"theme\"").expect("theme");
    let permissions_index = text.find("\"permissions\"").expect("permissions");
    let changelog_index = text.find("\"lastChangelogVersion\"").expect("changelog");
    assert!(
        theme_index < permissions_index && permissions_index < changelog_index,
        "key order changed:\n{text}"
    );
    let value: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(value["permissions"]["mode"], json!("default"));
    assert_eq!(
        value["permissions"]["rules"]
            .as_array()
            .expect("rules")
            .len(),
        2
    );
    assert_eq!(
        manager.project_permissions().expect("project permissions")["rules"]
            .as_array()
            .expect("rules")
            .len(),
        2
    );

    // 额外确认 storage 的 project scope 与 manager 看到的是同一份文件（避免测试误用
    // global 作用域）。
    let mut seen = String::new();
    FileSettingsStorage::new(&project.to_string_lossy(), &agent_dir.to_string_lossy())
        .with_lock(SettingsScope::Project, &mut |current| {
            seen = current.unwrap_or_default().to_string();
            Ok(None)
        })
        .expect("read project scope");
    assert_eq!(seen, text);
}

/// `/permissions` 面板的 User 作用域：全局写回只改 `permissions`，保留其它键与键序。
#[tokio::test]
async fn global_permission_writeback_preserves_other_fields_and_key_order() {
    let dir = tempfile::tempdir().expect("temp");
    let agent_dir = dir.path().join("agent");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    std::fs::create_dir_all(&project).expect("project");
    let global_path = agent_dir.join("settings.json");
    std::fs::write(
        &global_path,
        r#"{
  "theme": "dark",
  "permissions": {
    "mode": "read-only"
  },
  "lastChangelogVersion": "1.0.0"
}"#,
    )
    .expect("seed global settings");

    let storage = Arc::new(FileSettingsStorage::new(
        &project.to_string_lossy(),
        &agent_dir.to_string_lossy(),
    ));
    let manager = SettingsManager::from_storage(storage);
    manager
        .update_global_permissions(|current| {
            let mut permissions = current.cloned().unwrap_or_else(|| json!({}));
            permissions["rules"] = json!([{"tool": "read", "decision": "allow"}]);
            permissions
        })
        .expect("write global permissions");
    manager.flush().await;

    let text = std::fs::read_to_string(&global_path).expect("read global settings");
    let theme_index = text.find("\"theme\"").expect("theme");
    let permissions_index = text.find("\"permissions\"").expect("permissions");
    let changelog_index = text.find("\"lastChangelogVersion\"").expect("changelog");
    assert!(
        theme_index < permissions_index && permissions_index < changelog_index,
        "key order changed:\n{text}"
    );
    let value: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(value["permissions"]["mode"], json!("read-only"));
    assert_eq!(value["permissions"]["rules"][0]["tool"], json!("read"));
}

/// Local 作用域的读改写：`update_permissions_file` 只在有改动时写盘，并保留其它键。
#[test]
fn local_permission_update_preserves_other_fields_and_skips_noop_writes() {
    let dir = tempfile::tempdir().expect("temp");
    let path = dir.path().join("settings.local.json");
    std::fs::write(
        &path,
        r#"{
  "theme": "dark",
  "permissions": {
    "addDirs": ["/tmp/extra"]
  }
}"#,
    )
    .expect("seed local settings");

    let changed =
        kiana_config::settings::permissions::update_permissions_file(&path, |permissions| {
            let object = permissions.as_object_mut().expect("permissions object");
            object.insert("mode".to_string(), json!("accept-edits"));
            Ok(true)
        })
        .expect("update local permissions");
    assert!(changed);

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
    assert_eq!(value["theme"], json!("dark"));
    assert_eq!(value["permissions"]["mode"], json!("accept-edits"));
    assert_eq!(value["permissions"]["addDirs"][0], json!("/tmp/extra"));

    let before = std::fs::read_to_string(&path).expect("read");
    let changed =
        kiana_config::settings::permissions::update_permissions_file(&path, |_permissions| {
            Ok(false)
        })
        .expect("no-op update");
    assert!(!changed);
    assert_eq!(std::fs::read_to_string(&path).expect("read"), before);
}
