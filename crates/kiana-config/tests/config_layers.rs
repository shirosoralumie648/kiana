//! 分层配置的公开 API 集成测试。

use std::path::Path;

use kiana_config::settings::layers::{
    ArrayMergeMode, ConfigLayer, ConfigLayerSource, ConfigLayers, LayerLoadError, LayerLoadMode,
    ManagedPolicy, McpServerDecision,
};
use serde_json::json;

fn layer(source: ConfigLayerSource, value: serde_json::Value) -> ConfigLayer {
    ConfigLayer::new(source, value)
}

#[test]
fn higher_layer_wins_for_every_source() {
    let mut layers = ConfigLayers::new();
    for source in ConfigLayerSource::ALL {
        layers.push(layer(
            source,
            json!({"theme": source.as_str(), "permissions": {"mode": source.as_str()}}),
        ));
    }
    let merged = layers.resolve();
    assert_eq!(merged.value()["theme"], json!("session"));
    assert_eq!(merged.value()["permissions"]["mode"], json!("session"));
    assert_eq!(
        merged.origin("permissions.mode"),
        Some(ConfigLayerSource::Session)
    );
    assert_eq!(merged.origin("theme"), Some(ConfigLayerSource::Session));
}

#[test]
fn origins_identify_the_actual_winner() {
    let merged = ConfigLayers::new()
        .with_layer(layer(
            ConfigLayerSource::Builtin,
            json!({"compaction": {"enabled": true, "reserveTokens": 100}}),
        ))
        .with_layer(layer(
            ConfigLayerSource::User,
            json!({"compaction": {"reserveTokens": 200}}),
        ))
        .with_layer(layer(
            ConfigLayerSource::Project,
            json!({"compaction": {"enabled": false}}),
        ))
        .resolve();
    assert_eq!(
        merged.origin("compaction.enabled"),
        Some(ConfigLayerSource::Project)
    );
    assert_eq!(
        merged.origin("compaction.reserveTokens"),
        Some(ConfigLayerSource::User)
    );
    assert_eq!(merged.value()["compaction"]["reserveTokens"], json!(200));
}

#[test]
fn fingerprints_are_order_insensitive_and_value_sensitive() {
    let first = ConfigLayers::new()
        .with_layer(layer(
            ConfigLayerSource::Project,
            json!({"a": {"x": 1, "y": [1, 2]}, "b": true}),
        ))
        .resolve();
    let reordered = ConfigLayers::new()
        .with_layer(layer(
            ConfigLayerSource::Project,
            json!({"b": true, "a": {"y": [1, 2], "x": 1}}),
        ))
        .resolve();
    assert_eq!(first.fingerprint(), reordered.fingerprint());

    let changed = ConfigLayers::new()
        .with_layer(layer(
            ConfigLayerSource::Project,
            json!({"a": {"x": 1, "y": [1, 3]}, "b": true}),
        ))
        .resolve();
    assert_ne!(first.fingerprint(), changed.fingerprint());

    // 数组顺序是有效值的一部分，不能被视为键序。
    let reordered_array = ConfigLayers::new()
        .with_layer(layer(
            ConfigLayerSource::Project,
            json!({"a": {"x": 1, "y": [2, 1]}, "b": true}),
        ))
        .resolve();
    assert_ne!(first.fingerprint(), reordered_array.fingerprint());
}

#[test]
fn array_merge_is_table_driven() {
    assert_eq!(
        kiana_config::settings::layers::array_merge_mode("permissions.deny"),
        ArrayMergeMode::ConcatDedup
    );
    assert_eq!(
        kiana_config::settings::layers::array_merge_mode("permissions.rules"),
        ArrayMergeMode::ConcatDedup
    );
    assert_eq!(
        kiana_config::settings::layers::array_merge_mode("hooks.PreToolUse"),
        ArrayMergeMode::ConcatDedup
    );
    assert_eq!(
        kiana_config::settings::layers::array_merge_mode("npmCommand"),
        ArrayMergeMode::Replace
    );

    let merged = ConfigLayers::new()
        .with_layer(layer(
            ConfigLayerSource::User,
            json!({"permissions": {"deny": ["read:/etc", "bash:rm"]}}),
        ))
        .with_layer(layer(
            ConfigLayerSource::Local,
            json!({"permissions": {"deny": ["bash:rm", "write:/etc"]}}),
        ))
        .resolve();
    assert_eq!(
        merged.value()["permissions"]["deny"],
        json!(["read:/etc", "bash:rm", "write:/etc"])
    );
}

#[test]
fn managed_policy_cannot_be_overridden_by_project() {
    let merged = ConfigLayers::new()
        .with_layer(layer(
            ConfigLayerSource::Managed,
            json!({
                "allowManagedPermissionRulesOnly": true,
                "allowManagedHooksOnly": true,
                "disableBypassPermissionsMode": "disable",
                "allowedMcpServers": [{"serverName": "docs"}],
                "deniedMcpServers": [{"serverName": "evil"}]
            }),
        ))
        .with_layer(layer(
            ConfigLayerSource::Project,
            json!({
                "allowManagedPermissionRulesOnly": false,
                "allowManagedHooksOnly": false,
                "permissions": {"disableBypassPermissionsMode": false},
                "allowedMcpServers": [{"serverName": "user"}]
            }),
        ))
        .resolve();
    let policy = merged.policy();
    assert!(policy.allow_managed_permission_rules_only);
    assert!(policy.allow_managed_hooks_only);
    assert!(policy.disable_bypass_permissions_mode);
    assert_eq!(
        merged.value()["permissions"]["disableBypassPermissionsMode"],
        json!(true)
    );
    assert_eq!(
        merged.value()["allowManagedPermissionRulesOnly"],
        json!(true)
    );
    assert_eq!(merged.value()["allowManagedHooksOnly"], json!(true));
    assert_eq!(
        merged.value()["allowed_mcp_servers"],
        json!([{"serverName": "docs"}])
    );
    assert_eq!(
        policy.mcp_server_decision("docs", None, None),
        McpServerDecision::Allowed
    );
    assert_eq!(
        policy.mcp_server_decision("user", None, None),
        McpServerDecision::Denied
    );
}

#[test]
fn user_deny_still_applies_under_a_managed_allowlist() {
    let merged = ConfigLayers::new()
        .with_layer(layer(
            ConfigLayerSource::Managed,
            json!({"allowedMcpServers": [{"serverName": "shared"}]}),
        ))
        .with_layer(layer(
            ConfigLayerSource::User,
            json!({"deniedMcpServers": [{"serverName": "shared"}]}),
        ))
        .resolve();
    assert_eq!(
        merged.policy().mcp_server_decision("shared", None, None),
        McpServerDecision::Denied,
        "deny 永远合并：用户 deny 不能被 managed allowlist 覆盖"
    );
    assert_eq!(merged.value()["allowed_mcp_servers"], json!([]));
}

#[test]
fn managed_permission_rules_only_keeps_all_deny_rules() {
    let merged = ConfigLayers::new()
        .with_layer(layer(
            ConfigLayerSource::User,
            json!({"permissions": {
                "rules": [
                    {"tool": "read", "decision": "allow"},
                    {"command": ["curl"], "decision": "deny"}
                ],
                "allow": ["read"],
                "deny": ["curl"]
            }}),
        ))
        .with_layer(layer(
            ConfigLayerSource::Managed,
            json!({
                "allow_managed_permission_rules_only": true,
                "permissions": {
                    "rules": [{"tool": "bash", "decision": "allow"}],
                    "allow": ["bash"]
                }
            }),
        ))
        .resolve();
    let permissions = &merged.value()["permissions"];
    assert_eq!(permissions["allow"], json!(["bash"]));
    assert_eq!(permissions["deny"], json!(["curl"]));
    let rules = permissions["rules"].as_array().expect("rules");
    assert!(rules.iter().any(|rule| rule["tool"] == json!("bash")));
    assert!(rules.iter().any(|rule| rule["command"] == json!(["curl"])));
    assert!(!rules.iter().any(|rule| rule["tool"] == json!("read")));
}

#[test]
fn strict_diagnostics_target_managed_files_only() {
    let dir = tempfile::tempdir().expect("temp");
    let managed_path = dir.path().join("managed-settings.json");
    std::fs::write(
        &managed_path,
        "{\n  \"permissions\": {\n    \"mode\": \"default\",\n    \"allowed\": []\n  }\n}",
    )
    .expect("write");
    let managed = ConfigLayer::load_file(
        ConfigLayerSource::Managed,
        &managed_path,
        LayerLoadMode::default_for(ConfigLayerSource::Managed),
    )
    .expect("load")
    .expect("layer");
    assert_eq!(managed.diagnostics.len(), 1);
    let diagnostic = &managed.diagnostics[0];
    assert_eq!(diagnostic.file, managed_path);
    assert_eq!(diagnostic.line, 4);
    assert_eq!(diagnostic.column, 5);
    assert_eq!(diagnostic.key_path, "permissions.allowed");

    let legacy_path = dir.path().join("settings.json");
    std::fs::write(
        &legacy_path,
        "{\"permissions\": {\"allowed\": []}, \"customKey\": true}",
    )
    .expect("write");
    let legacy = ConfigLayer::load_file(
        ConfigLayerSource::User,
        &legacy_path,
        LayerLoadMode::default_for(ConfigLayerSource::User),
    )
    .expect("load")
    .expect("layer");
    assert!(legacy.diagnostics.is_empty());
}

#[test]
fn syntax_error_carries_file_line_and_column() {
    let dir = tempfile::tempdir().expect("temp");
    let path = dir.path().join("managed-settings.json");
    std::fs::write(&path, "{\n  \"a\": }").expect("write");
    let error = ConfigLayer::load_file(ConfigLayerSource::Managed, &path, LayerLoadMode::Strict)
        .expect_err("syntax error");
    match error {
        LayerLoadError::Parse(diagnostic) => {
            assert_eq!(diagnostic.file, path);
            assert_eq!(diagnostic.line, 2);
            assert!(diagnostic.column > 1);
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn standard_paths_point_at_project_and_local_files() {
    let cwd = Path::new("/workspace");
    assert_eq!(
        ConfigLayerSource::Project.standard_path(cwd),
        Some(Path::new("/workspace/.kiana/settings.json").to_path_buf())
    );
    assert_eq!(
        ConfigLayerSource::Local.standard_path(cwd),
        Some(Path::new("/workspace/.kiana/settings.local.json").to_path_buf())
    );
    assert_eq!(ConfigLayerSource::Session.standard_path(cwd), None);
}

#[test]
fn managed_policy_parses_snake_and_camel_case() {
    let snake = ManagedPolicy::from_value(&json!({
        "allow_managed_permission_rules_only": true,
        "allow_managed_hooks_only": true,
        "disable_bypass_permissions_mode": true,
        "allowed_mcp_servers": [{"server_name": "docs"}],
        "denied_mcp_servers": [{"server_url": "https://*.evil.example/*"}]
    }));
    assert!(snake.allow_managed_permission_rules_only);
    assert!(snake.allow_managed_hooks_only);
    assert!(snake.disable_bypass_permissions_mode);
    assert_eq!(
        snake.mcp_server_decision("docs", None, None),
        McpServerDecision::Allowed
    );
    assert_eq!(
        snake.mcp_server_decision("remote", None, Some("https://a.evil.example/x")),
        McpServerDecision::Denied
    );

    let camel = ManagedPolicy::from_value(&json!({
        "allowManagedPermissionRulesOnly": true,
        "permissions": {"disableBypassPermissionsMode": "disable"}
    }));
    assert!(camel.allow_managed_permission_rules_only);
    assert!(camel.disable_bypass_permissions_mode);
}
