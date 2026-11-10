//! settings 迁移（`migrateSettings`）的独立覆盖。
//!
//! 审计指出：没有任何 TS 测试直接调用 `migrateSettings`（它只经 `loadFromStorage` /
//! `persistScopedSettings` 间接执行）。本文件按 `ca-config-models.md` §2.9.7 逐条锁定四个
//! 迁移的**固定顺序**、`in` 语义（`contains_key`，显式 `null` 也算存在）与幂等性，并验证
//! 迁移在**加载**和**写盘前重读当前文件**两条路径上都执行。
//!
//! 另含 `config-schemas.test.ts` 中依赖 `SettingsManager` / 运行期 accessor 的两个用例。

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use kiana_config::settings::migrations::{SettingsMigrationError, migrate_settings};
use kiana_config::settings::schema::{
    Settings, SteeringMode, TransportSetting, settings_schema_accepts, validate_settings_schema,
};
use kiana_config::settings::{
    InMemorySettingsStorage, SettingsManager, SettingsScope, SettingsStorage,
};
use serde_json::{Value, json};

fn migrate(mut value: Value) -> Value {
    migrate_settings(&mut value).expect("object migrations cannot fail");
    value
}

fn keys(value: &Value) -> Vec<&str> {
    value
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect()
}

// ---------------------------------------------------------------------------
// 1. queueMode -> steeringMode
// ---------------------------------------------------------------------------

#[test]
fn migrates_queue_mode_only_when_steering_mode_is_absent() {
    assert_eq!(
        migrate(json!({ "queueMode": "all" })),
        json!({ "steeringMode": "all" })
    );
    assert_eq!(
        migrate(json!({ "queueMode": "one-at-a-time" })),
        json!({ "steeringMode": "one-at-a-time" })
    );
    // 目标键存在时两键都保留（不删除 queueMode）。
    assert_eq!(
        migrate(json!({ "queueMode": "all", "steeringMode": "one-at-a-time" })),
        json!({ "queueMode": "all", "steeringMode": "one-at-a-time" })
    );
    // `in` 语义：显式 null 也算“存在”。
    assert_eq!(
        migrate(json!({ "queueMode": "all", "steeringMode": null })),
        json!({ "queueMode": "all", "steeringMode": null })
    );
    // queueMode 为 null 时同样迁移（值原样带过去）。
    assert_eq!(
        migrate(json!({ "queueMode": null })),
        json!({ "steeringMode": null })
    );
}

// ---------------------------------------------------------------------------
// 2. websockets -> transport
// ---------------------------------------------------------------------------

#[test]
fn migrates_websockets_only_when_transport_is_absent_and_websockets_is_boolean() {
    assert_eq!(
        migrate(json!({ "websockets": true })),
        json!({ "transport": "websocket" })
    );
    assert_eq!(
        migrate(json!({ "websockets": false })),
        json!({ "transport": "sse" })
    );
    // transport 已存在 → 两键都不动（websockets 保留）。
    assert_eq!(
        migrate(json!({ "websockets": true, "transport": "sse" })),
        json!({ "websockets": true, "transport": "sse" })
    );
    // 非布尔值 → 不迁移。
    for value in [json!("true"), json!(0), json!(null), json!({}), json!([])] {
        assert_eq!(
            migrate(json!({ "websockets": value.clone() })),
            json!({ "websockets": value }),
            "websockets = {value} must not migrate"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. skills object -> array
// ---------------------------------------------------------------------------

#[test]
fn migrates_skills_object_to_array() {
    assert_eq!(
        migrate(json!({
            "skills": { "enableSkillCommands": false, "customDirectories": ["/a", "/b"] },
        })),
        json!({ "enableSkillCommands": false, "skills": ["/a", "/b"] })
    );
    // 空 customDirectories → 删除 skills（enableSkillCommands 仍复制）。
    assert_eq!(
        migrate(json!({
            "skills": { "enableSkillCommands": true, "customDirectories": [] },
        })),
        json!({ "enableSkillCommands": true })
    );
    // customDirectories 不是数组 → 删除 skills。
    assert_eq!(
        migrate(json!({ "skills": { "customDirectories": "/a" } })),
        json!({})
    );
    // 目标 enableSkillCommands 已存在 → 不复制。
    assert_eq!(
        migrate(json!({
            "skills": { "enableSkillCommands": false, "customDirectories": ["/a"] },
            "enableSkillCommands": true,
        })),
        json!({ "enableSkillCommands": true, "skills": ["/a"] })
    );
    // enableSkillCommands 为 null 也算“已定义”，会被复制。
    assert_eq!(
        migrate(json!({ "skills": { "enableSkillCommands": null } })),
        json!({ "enableSkillCommands": null })
    );
    // Kiana 扩展：对象形式里的技能激活 TTL 迁移到顶层，避免对象被折叠/删除时丢失。
    assert_eq!(
        migrate(json!({
            "skills": {
                "customDirectories": ["/a"],
                "activationTtlSeconds": 60,
            }
        })),
        json!({
            "skillActivationTtlSeconds": 60,
            "skills": ["/a"],
        })
    );
    assert_eq!(
        migrate(json!({ "skills": { "activationTtl": 30 } })),
        json!({ "skillActivationTtl": 30 })
    );
    // 顶层已定义时不覆盖。
    assert_eq!(
        migrate(json!({
            "skills": { "activationTtlSeconds": 60 },
            "skillActivationTtlSeconds": 5,
        })),
        json!({ "skillActivationTtlSeconds": 5 })
    );
    // 数组 / null / 字符串的 skills 保持原样。
    assert_eq!(
        migrate(json!({ "skills": ["/a"] })),
        json!({ "skills": ["/a"] })
    );
    assert_eq!(
        migrate(json!({ "skills": null })),
        json!({ "skills": null })
    );
    assert_eq!(
        migrate(json!({ "skills": "/a" })),
        json!({ "skills": "/a" })
    );
}

// ---------------------------------------------------------------------------
// 4. retry.maxDelayMs -> retry.provider.maxRetryDelayMs
// ---------------------------------------------------------------------------

#[test]
fn migrates_retry_max_delay_ms() {
    // provider 缺失 → 新对象。
    assert_eq!(
        migrate(json!({ "retry": { "maxDelayMs": 5000 } })),
        json!({ "retry": { "provider": { "maxRetryDelayMs": 5000 } } })
    );
    // provider 存在 → 逐字段合并（保持原有键）。
    assert_eq!(
        migrate(
            json!({ "retry": { "enabled": false, "maxDelayMs": 5000, "provider": { "maxRetries": 2 } } })
        ),
        json!({ "retry": { "enabled": false, "provider": { "maxRetries": 2, "maxRetryDelayMs": 5000 } } })
    );
    // provider.maxRetryDelayMs 为 null → 视为缺失，替换。
    assert_eq!(
        migrate(
            json!({ "retry": { "maxDelayMs": 5000, "provider": { "maxRetryDelayMs": null } } })
        ),
        json!({ "retry": { "provider": { "maxRetryDelayMs": 5000 } } })
    );
    // 目标已有值 → 不复制，但 maxDelayMs 仍被删除。
    assert_eq!(
        migrate(
            json!({ "retry": { "maxDelayMs": 5000, "provider": { "maxRetryDelayMs": 1000 } } })
        ),
        json!({ "retry": { "provider": { "maxRetryDelayMs": 1000 } } })
    );
    // maxDelayMs 不是数字 → 仍被删除。
    for value in [
        json!("5000"),
        json!(null),
        json!(true),
        json!([1]),
        json!({}),
    ] {
        assert_eq!(
            migrate(json!({ "retry": { "maxDelayMs": value.clone(), "enabled": true } })),
            json!({ "retry": { "enabled": true } }),
            "maxDelayMs = {value} must be deleted"
        );
    }
    // provider 是数组：JS 的 `typeof [] === "object"`，展开成下标键。
    assert_eq!(
        migrate(json!({ "retry": { "maxDelayMs": 9, "provider": [1, 2] } })),
        json!({ "retry": { "provider": { "0": 1, "1": 2, "maxRetryDelayMs": 9 } } })
    );
    // retry 是数组 / 原始值 → 完全不动。
    let retry_array = json!({ "retry": [{ "maxDelayMs": 5000 }] });
    assert_eq!(migrate(retry_array.clone()), retry_array);
    let retry_string = json!({ "retry": "maxDelayMs" });
    assert_eq!(migrate(retry_string.clone()), retry_string);
}

// ---------------------------------------------------------------------------
// 顺序与幂等性
// ---------------------------------------------------------------------------

#[test]
fn runs_migrations_in_fixed_order_and_is_idempotent() {
    let input = json!({
        "queueMode": "all",
        "websockets": true,
        "skills": { "enableSkillCommands": false, "customDirectories": ["/skills"] },
        "retry": { "maxDelayMs": 5000, "provider": { "maxRetries": 2 } },
        "theme": "dark",
    });
    let migrated = migrate(input.clone());

    assert_eq!(
        migrated,
        json!({
            "skills": ["/skills"],
            "retry": { "provider": { "maxRetries": 2, "maxRetryDelayMs": 5000 } },
            "theme": "dark",
            "steeringMode": "all",
            "transport": "websocket",
            "enableSkillCommands": false,
        })
    );
    // 键序：原文件的键（skills / retry / theme）在前，迁移新增的键按迁移顺序追加。
    assert_eq!(
        keys(&migrated),
        [
            "skills",
            "retry",
            "theme",
            "steeringMode",
            "transport",
            "enableSkillCommands",
        ]
    );

    // 幂等：再跑一次不改变任何东西。
    assert_eq!(migrate(migrated.clone()), migrated);
}

#[test]
fn rejects_top_level_scalars_with_the_javascript_in_operator_message() {
    for (value, rendered) in [
        (Value::Null, "null"),
        (json!("abc"), "abc"),
        (json!(42), "42"),
        (json!(1.5), "1.5"),
        (json!(true), "true"),
    ] {
        let mut value = value;
        let error: SettingsMigrationError =
            migrate_settings(&mut value).expect_err("scalars must be rejected");
        assert_eq!(
            error.to_string(),
            format!("Cannot use 'in' operator to search for 'queueMode' in {rendered}")
        );
    }
    // 顶层数组合法（`\"queueMode\" in []` 为 false），原样返回。
    let mut array = json!([1, 2, 3]);
    migrate_settings(&mut array).expect("top-level arrays pass through");
    assert_eq!(array, json!([1, 2, 3]));
}

// ---------------------------------------------------------------------------
// 迁移在加载路径上执行
// ---------------------------------------------------------------------------

fn storage_with_global(value: Value) -> Arc<dyn SettingsStorage> {
    let storage = InMemorySettingsStorage::new();
    let text = serde_json::to_string(&value).expect("serialize");
    storage
        .with_lock(SettingsScope::Global, &mut |_| Ok(Some(text.clone())))
        .expect("seed storage");
    Arc::new(storage)
}

#[test]
fn applies_migrations_when_loading_settings() {
    let manager = SettingsManager::from_storage(storage_with_global(json!({
        "queueMode": "all",
        "websockets": true,
        "skills": { "enableSkillCommands": false, "customDirectories": ["/a"] },
        "retry": { "maxDelayMs": 5000, "enabled": false },
    })));

    assert_eq!(manager.get_steering_mode(), SteeringMode::All);
    assert_eq!(manager.get_transport(), TransportSetting::Websocket);
    assert_eq!(manager.get_skill_paths(), vec!["/a".to_string()]);
    assert!(!manager.get_enable_skill_commands());
    assert_eq!(
        manager.get_provider_retry_settings().max_retry_delay_ms,
        5000.0
    );
}

// ---------------------------------------------------------------------------
// 迁移在 persistScopedSettings 重读当前文件时执行（逐字节）
// ---------------------------------------------------------------------------

// 期望字节由 TS 参考实现生成（`fixtures/settings/persist_golden.json` 的
// "migrates legacy keys when persisting" 用例）。
#[tokio::test]
async fn applies_migrations_when_persisting_and_keeps_key_order() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/settings/persist_golden.json"))
            .expect("fixture");
    let case = fixture["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .find(|case| case["name"] == json!("migrates legacy keys when persisting"))
        .expect("fixture case");
    let expected_global = case["expectedGlobal"]
        .as_str()
        .expect("expected")
        .to_string();

    let temp = tempfile::tempdir().expect("tempdir");
    let agent_dir = temp.path().join("agent");
    let project_dir = temp.path().join("project");
    fs::create_dir_all(&agent_dir).expect("agent dir");
    fs::create_dir_all(project_dir.join(".kiana")).expect("project dir");
    let settings_path: PathBuf = agent_dir.join("settings.json");
    fs::write(&settings_path, case["initial"].as_str().expect("initial")).expect("write");

    let manager = SettingsManager::create(
        project_dir.to_str().expect("utf-8"),
        agent_dir.to_str().expect("utf-8"),
    );
    manager.set_theme("light".to_string());
    manager.flush().await;

    let written = fs::read_to_string(&settings_path).expect("read");
    assert_eq!(written, expected_global);
    // 无结尾换行。
    assert!(!written.ends_with('\n'));
    // 迁移后的键都在。
    assert_eq!(
        keys(&serde_json::from_str::<Value>(&written).expect("json")),
        [
            "retry",
            "skills",
            "theme",
            "steeringMode",
            "transport",
            "enableSkillCommands",
        ]
    );
}

// ---------------------------------------------------------------------------
// config-schemas.test.ts 的 SettingsManager 相关用例
// ---------------------------------------------------------------------------

// TS: "rejects settings values that runtime accessors reject"
#[test]
fn rejects_settings_values_that_runtime_accessors_reject() {
    for invalid in [
        json!({ "compaction": { "reserveTokens": -1 } }),
        json!({ "compaction": { "keepRecentTokens": 1.5 } }),
        json!({
            "compaction": {
                "modelOverrides": { "provider/model": { "reserveTokens": 9_007_199_254_740_992.0 } }
            }
        }),
        json!({ "httpIdleTimeoutMs": -1 }),
        json!({ "websocketConnectTimeoutMs": -1 }),
        json!({ "httpIdleTimeoutMs": "bogus" }),
        json!({ "websocketConnectTimeoutMs": "30000" }),
        json!({ "codemode": { "inlineBudget": -1 } }),
    ] {
        assert!(
            !settings_schema_accepts(&invalid),
            "schema must reject {invalid}"
        );
    }

    let valid = json!({
        "compaction": {
            "reserveTokens": 0,
            "keepRecentTokens": 9_007_199_254_740_991.0,
            "modelOverrides": { "provider/model": { "reserveTokens": 0 } },
        },
        "httpIdleTimeoutMs": 0,
        "websocketConnectTimeoutMs": "disabled",
        "codemode": { "inlineBudget": 0 },
    });
    assert!(validate_settings_schema(&valid).is_ok());

    // 运行期 accessor 对同一批值抛错。
    let manager = |value: Value| {
        SettingsManager::in_memory(serde_json::from_value::<Settings>(value).expect("settings"))
    };
    assert!(
        manager(json!({ "compaction": { "reserveTokens": -1 } }))
            .get_compaction_settings(None)
            .is_err()
    );
    assert!(
        manager(json!({ "compaction": { "keepRecentTokens": 1.5 } }))
            .get_compaction_settings(None)
            .is_err()
    );
    assert!(
        manager(json!({
            "compaction": { "modelOverrides": { "provider/model": { "reserveTokens": 9_007_199_254_740_992.0 } } }
        }))
        .get_compaction_settings(Some(kiana_config::settings::CompactionModelRef::new(
            "provider", "model"
        )))
        .is_err()
    );
    assert!(
        manager(json!({ "httpIdleTimeoutMs": -1 }))
            .get_http_idle_timeout_ms()
            .is_err()
    );
    assert!(
        manager(json!({ "httpIdleTimeoutMs": "bogus" }))
            .get_http_idle_timeout_ms()
            .is_err()
    );
    assert!(
        manager(json!({ "websocketConnectTimeoutMs": -1 }))
            .get_websocket_connect_timeout_ms()
            .is_err()
    );
}

// TS: "accepts $schema in settings.json and preserves unknown settings"
#[test]
fn accepts_schema_in_settings_json_and_preserves_unknown_settings() {
    let schema_url = "https://kiana.dev/schemas/settings.schema.json";
    let temp = tempfile::tempdir().expect("tempdir");
    let directory = temp.path().to_path_buf();
    fs::write(
        directory.join("settings.json"),
        serde_json::to_string(&json!({
            "$schema": schema_url,
            "theme": "light",
            "extensionSetting": { "enabled": true },
        }))
        .expect("serialize"),
    )
    .expect("write settings");

    let manager = SettingsManager::create(
        directory.to_str().expect("utf-8"),
        directory.to_str().expect("utf-8"),
    );
    assert_eq!(manager.get_theme(), Some("light".to_string()));
    let global = manager.get_global_settings();
    assert_eq!(global.raw("$schema"), Some(json!(schema_url)));
    assert_eq!(
        global.raw("extensionSetting"),
        Some(json!({ "enabled": true }))
    );
}
