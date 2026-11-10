//! `packages/coding-agent/test/settings-manager-compaction.test.ts` 的移植（12 个 `it`，
//! 参数化展开后 60+ 个断言组）。
//!
//! 每个测试上方的 `TS:` 注释保留原测试标题逐字。模型键是精确的 `${provider}/${id}`。

use std::sync::Arc;

use indexmap::IndexMap;
use kiana_config::settings::schema::{CompactionModelOverride, CompactionSettings, Settings};
use kiana_config::settings::{
    CompactionModelRef, InMemorySettingsStorage, ResolvedCompactionSettings, SettingsManager,
    SettingsScope, SettingsStorage,
};
use serde_json::{Value, json};

const MODEL: CompactionModelRef<'static> = CompactionModelRef {
    provider: "provider",
    id: "family/model",
};
const MODEL_KEY: &str = "provider/family/model";
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

fn defaults() -> ResolvedCompactionSettings {
    ResolvedCompactionSettings {
        enabled: true,
        reserve_tokens: 16384.0,
        keep_recent_tokens: 20000.0,
    }
}

fn storage_with_global(value: Value) -> Arc<dyn SettingsStorage> {
    let storage = InMemorySettingsStorage::new();
    let text = serde_json::to_string(&value).expect("serialize");
    storage
        .with_lock(SettingsScope::Global, &mut |_| Ok(Some(text.clone())))
        .expect("seed storage");
    Arc::new(storage)
}

fn storage_with_scopes(global: Value, project: Value) -> Arc<dyn SettingsStorage> {
    let storage = InMemorySettingsStorage::new();
    let global_text = serde_json::to_string(&global).expect("serialize");
    let project_text = serde_json::to_string(&project).expect("serialize");
    storage
        .with_lock(SettingsScope::Global, &mut |_| {
            Ok(Some(global_text.clone()))
        })
        .expect("seed global");
    storage
        .with_lock(SettingsScope::Project, &mut |_| {
            Ok(Some(project_text.clone()))
        })
        .expect("seed project");
    Arc::new(storage)
}

fn string_of(value: &Value) -> String {
    kiana_json::js_string::js_string(Some(value))
}

fn map(entries: Vec<(&str, Value)>) -> Value {
    let mut map = serde_json::Map::new();
    for (key, value) in entries {
        map.insert(key.to_string(), value);
    }
    Value::Object(map)
}

/// `{ compaction: { modelOverrides: { MODEL_KEY: entry } } }`。
fn model_override_settings(entry: Value) -> Value {
    map(vec![(
        "compaction",
        map(vec![("modelOverrides", map(vec![(MODEL_KEY, entry)]))]),
    )])
}

/// `{ compaction: { modelOverrides: { MODEL_KEY: { field: value } } } }`。
fn model_override_field_settings(field: &str, value: Value) -> Value {
    model_override_settings(map(vec![(field, value)]))
}

// TS: "uses defaults without compaction settings"
#[test]
fn uses_defaults_without_compaction_settings() {
    let manager = SettingsManager::in_memory(Settings::default());
    assert_eq!(
        manager.get_compaction_settings(None).expect("valid"),
        defaults()
    );
    assert_eq!(
        manager.get_compaction_settings(Some(MODEL)).expect("valid"),
        defaults()
    );
}

// TS: "resolves each field independently and keeps individual getters consistent"
#[test]
fn resolves_each_field_independently_and_keeps_individual_getters_consistent() {
    let manager = SettingsManager::in_memory(
        serde_json::from_value(map(vec![(
            "compaction",
            map(vec![
                ("reserveTokens", json!(8192)),
                ("keepRecentTokens", json!(10000)),
                (
                    "modelOverrides",
                    map(vec![(
                        MODEL_KEY,
                        map(vec![("reserveTokens", json!(400000))]),
                    )]),
                ),
            ]),
        )]))
        .expect("settings"),
    );
    assert_eq!(
        manager.get_compaction_settings(Some(MODEL)).expect("valid"),
        ResolvedCompactionSettings {
            enabled: true,
            reserve_tokens: 400000.0,
            keep_recent_tokens: 10000.0,
        }
    );
    assert_eq!(
        manager
            .get_compaction_reserve_tokens(Some(MODEL))
            .expect("valid"),
        400000.0
    );
    assert_eq!(
        manager
            .get_compaction_keep_recent_tokens(Some(MODEL))
            .expect("valid"),
        10000.0
    );
    assert_eq!(
        manager.get_compaction_settings(None).expect("valid"),
        ResolvedCompactionSettings {
            enabled: true,
            reserve_tokens: 8192.0,
            keep_recent_tokens: 10000.0,
        }
    );

    manager.apply_overrides(
        serde_json::from_value(model_override_field_settings(
            "keepRecentTokens",
            json!(30000),
        ))
        .expect("overrides"),
    );
    assert_eq!(
        manager
            .get_compaction_keep_recent_tokens(Some(MODEL))
            .expect("valid"),
        30000.0
    );
    assert_eq!(
        manager
            .get_compaction_reserve_tokens(Some(MODEL))
            .expect("valid"),
        400000.0
    );
}

// TS: "falls back to built-in defaults for missing fields"
#[test]
fn falls_back_to_built_in_defaults_for_missing_fields() {
    let manager = SettingsManager::in_memory(
        serde_json::from_value(model_override_field_settings(
            "keepRecentTokens",
            json!(1024),
        ))
        .expect("settings"),
    );
    assert_eq!(
        manager.get_compaction_settings(Some(MODEL)).expect("valid"),
        ResolvedCompactionSettings {
            keep_recent_tokens: 1024.0,
            ..defaults()
        }
    );
}

// TS: "matches exact provider/model IDs, including IDs containing slashes"
#[test]
fn matches_exact_provider_model_ids_including_ids_containing_slashes() {
    let manager = SettingsManager::in_memory(
        serde_json::from_value(map(vec![(
            "compaction",
            map(vec![(
                "modelOverrides",
                map(vec![
                    (MODEL_KEY, map(vec![("reserveTokens", json!(400000))])),
                    ("provider/*", map(vec![("reserveTokens", json!(1))])),
                    ("family/model", map(vec![("reserveTokens", json!(2))])),
                ]),
            )]),
        )]))
        .expect("settings"),
    );
    assert_eq!(
        manager
            .get_compaction_reserve_tokens(Some(MODEL))
            .expect("valid"),
        400000.0
    );
    for other in [
        CompactionModelRef::new("other", MODEL.id),
        CompactionModelRef::new(MODEL.provider, "other"),
        CompactionModelRef::new(MODEL.provider, "family/Model"),
    ] {
        assert_eq!(
            manager.get_compaction_settings(Some(other)).expect("valid"),
            defaults()
        );
    }
}

// TS: "merges project model overrides per field before resolving fallbacks"
#[tokio::test]
async fn merges_project_model_overrides_per_field_before_resolving_fallbacks() {
    let storage = storage_with_scopes(
        map(vec![(
            "compaction",
            map(vec![
                ("reserveTokens", json!(8192)),
                (
                    "modelOverrides",
                    map(vec![
                        (
                            MODEL_KEY,
                            map(vec![
                                ("reserveTokens", json!(400000)),
                                ("keepRecentTokens", json!(30000)),
                            ]),
                        ),
                        (
                            "provider/other",
                            map(vec![("keepRecentTokens", json!(4096))]),
                        ),
                    ]),
                ),
            ]),
        )]),
        map(vec![(
            "compaction",
            map(vec![
                ("reserveTokens", json!(1024)),
                (
                    "modelOverrides",
                    map(vec![(
                        MODEL_KEY,
                        map(vec![("keepRecentTokens", json!(2000))]),
                    )]),
                ),
            ]),
        )]),
    );
    let manager = SettingsManager::from_storage(storage);
    assert_eq!(
        manager.get_compaction_settings(Some(MODEL)).expect("valid"),
        ResolvedCompactionSettings {
            enabled: true,
            reserve_tokens: 400000.0,
            keep_recent_tokens: 2000.0,
        }
    );
    assert_eq!(
        manager
            .get_compaction_settings(Some(CompactionModelRef::new("provider", "other")))
            .expect("valid"),
        ResolvedCompactionSettings {
            enabled: true,
            reserve_tokens: 1024.0,
            keep_recent_tokens: 4096.0,
        }
    );
    manager.reload().await;
    assert_eq!(
        manager
            .get_compaction_keep_recent_tokens(Some(MODEL))
            .expect("valid"),
        2000.0
    );
    manager.set_project_trusted(false);
    assert_eq!(
        manager
            .get_compaction_keep_recent_tokens(Some(MODEL))
            .expect("valid"),
        30000.0
    );
}

// TS: "keeps enabled global and preserves overrides when saving the toggle"
#[tokio::test]
async fn keeps_enabled_global_and_preserves_overrides_when_saving_the_toggle() {
    let storage = storage_with_global(map(vec![(
        "compaction",
        map(vec![(
            "modelOverrides",
            map(vec![(
                MODEL_KEY,
                map(vec![
                    ("enabled", json!(false)),
                    ("reserveTokens", json!(400000)),
                ]),
            )]),
        )]),
    )]));
    let _ = "";

    let manager = SettingsManager::from_storage(storage);
    assert!(
        manager
            .get_compaction_settings(Some(MODEL))
            .expect("valid")
            .enabled
    );
    assert!(manager.get_compaction_enabled());
    manager
        .set_compaction_enabled(false)
        .expect("valid compaction parent");
    manager.flush().await;
    manager.reload().await;
    assert_eq!(
        manager.get_compaction_settings(Some(MODEL)).expect("valid"),
        ResolvedCompactionSettings {
            enabled: false,
            reserve_tokens: 400000.0,
            keep_recent_tokens: 20000.0,
        }
    );
}

// TS: describe.each(["reserveTokens", "keepRecentTokens"])("model override %s")
//     it.each([null, -1, 1.5, "400000", true, {}, [], MAX_SAFE_INTEGER + 1])
//     "reports invalid token values: %j"
#[test]
fn reports_invalid_model_override_token_values() {
    for field in ["reserveTokens", "keepRecentTokens"] {
        for value in [
            Value::Null,
            json!(-1),
            json!(1.5),
            json!("400000"),
            json!(true),
            json!({}),
            json!([]),
            json!(MAX_SAFE_INTEGER + 1.0),
        ] {
            let storage = storage_with_global(model_override_field_settings(field, value.clone()));
            let manager = SettingsManager::from_storage(storage);
            let error = manager
                .get_compaction_settings(Some(MODEL))
                .expect_err("invalid token");
            assert_eq!(
                error.to_string(),
                format!(
                    "Invalid compaction.modelOverrides[\"{MODEL_KEY}\"].{field} setting: {}. Expected a non-negative safe integer.",
                    string_of(&value)
                )
            );
            assert_eq!(
                manager.get_compaction_settings(None).expect("valid"),
                defaults()
            );
            assert_eq!(
                manager
                    .get_compaction_settings(Some(CompactionModelRef::new("other", MODEL.id)))
                    .expect("valid"),
                defaults()
            );
        }
    }
}

// TS: it.each([NaN, Infinity, -Infinity]) "reports non-finite runtime values: %s"
#[test]
fn reports_non_finite_runtime_values() {
    for field in ["reserveTokens", "keepRecentTokens"] {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let manager = SettingsManager::in_memory(Settings::default());
            let mut entry = CompactionModelOverride::default();
            match field {
                "reserveTokens" => entry.reserve_tokens = Some(value),
                _ => entry.keep_recent_tokens = Some(value),
            }
            let mut overrides = IndexMap::new();
            overrides.insert(MODEL_KEY.to_string(), entry);
            manager.apply_overrides(Settings {
                compaction: Some(CompactionSettings {
                    model_overrides: Some(overrides),
                    ..Default::default()
                }),
                ..Settings::default()
            });
            let error = manager
                .get_compaction_settings(Some(MODEL))
                .expect_err("non-finite token");
            let expected_value = match value {
                v if v.is_nan() => "NaN".to_string(),
                v if v == f64::INFINITY => "Infinity".to_string(),
                _ => "-Infinity".to_string(),
            };
            assert_eq!(
                error.to_string(),
                format!(
                    "Invalid compaction.modelOverrides[\"{MODEL_KEY}\"].{field} setting: {expected_value}. Expected a non-negative safe integer."
                )
            );
        }
    }
}

// TS: describe.each(["reserveTokens", "keepRecentTokens"])("ordinary compaction.%s")
//     it.each([...]) "reports invalid values even when a valid model override exists: %j"
#[test]
fn reports_invalid_ordinary_values_even_when_a_valid_model_override_exists() {
    for field in ["reserveTokens", "keepRecentTokens"] {
        for value in [
            Value::Null,
            json!(-1),
            json!(1.5),
            json!("400000"),
            json!(true),
            json!({}),
            json!([]),
            json!(MAX_SAFE_INTEGER + 1.0),
        ] {
            let mut compaction = serde_json::Map::new();
            compaction.insert(field.to_string(), value.clone());
            compaction.insert(
                "modelOverrides".to_string(),
                map(vec![(MODEL_KEY, map(vec![(field, json!(4096))]))]),
            );
            let storage = storage_with_global(map(vec![("compaction", Value::Object(compaction))]));
            let manager = SettingsManager::from_storage(storage);
            let expected = format!(
                "Invalid compaction.{field} setting: {}. Expected a non-negative safe integer.",
                string_of(&value)
            );
            assert_eq!(
                manager
                    .get_compaction_settings(None)
                    .expect_err("invalid")
                    .to_string(),
                expected
            );
            assert_eq!(
                manager
                    .get_compaction_settings(Some(MODEL))
                    .expect_err("invalid")
                    .to_string(),
                expected
            );
        }
    }
}

// TS: it.each([NaN, Infinity, -Infinity]) "reports non-finite runtime values: %s"
#[test]
fn reports_non_finite_ordinary_runtime_values() {
    for field in ["reserveTokens", "keepRecentTokens"] {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let manager = SettingsManager::in_memory(Settings::default());
            let compaction = match field {
                "reserveTokens" => CompactionSettings {
                    reserve_tokens: Some(value),
                    ..Default::default()
                },
                _ => CompactionSettings {
                    keep_recent_tokens: Some(value),
                    ..Default::default()
                },
            };
            manager.apply_overrides(Settings {
                compaction: Some(compaction),
                ..Settings::default()
            });
            let error = manager
                .get_compaction_settings(None)
                .expect_err("non-finite ordinary");
            let expected_value = match value {
                v if v.is_nan() => "NaN".to_string(),
                v if v == f64::INFINITY => "Infinity".to_string(),
                _ => "-Infinity".to_string(),
            };
            assert_eq!(
                error.to_string(),
                format!(
                    "Invalid compaction.{field} setting: {expected_value}. Expected a non-negative safe integer."
                )
            );
        }
    }
}

// TS: it.each([null, false, 42, "invalid", []]) "reports malformed model entries: %j"
#[test]
fn reports_malformed_model_entries() {
    for entry in [
        Value::Null,
        json!(false),
        json!(42),
        json!("invalid"),
        json!([]),
    ] {
        let storage = storage_with_global(model_override_settings(entry.clone()));
        let error = SettingsManager::from_storage(storage)
            .get_compaction_settings(Some(MODEL))
            .expect_err("malformed entry");
        assert_eq!(
            error.to_string(),
            format!(
                "Invalid compaction.modelOverrides[\"{MODEL_KEY}\"] setting: {}. Expected an object.",
                string_of(&entry)
            )
        );
    }
}

// TS: "accepts zero in ordinary settings and model overrides"
#[test]
fn accepts_zero_in_ordinary_settings_and_model_overrides() {
    let manager = SettingsManager::in_memory(
        serde_json::from_value(map(vec![(
            "compaction",
            map(vec![
                ("reserveTokens", json!(0)),
                ("keepRecentTokens", json!(0)),
            ]),
        )]))
        .expect("settings"),
    );
    assert_eq!(
        manager.get_compaction_settings(Some(MODEL)).expect("valid"),
        ResolvedCompactionSettings {
            enabled: true,
            reserve_tokens: 0.0,
            keep_recent_tokens: 0.0,
        }
    );
    manager.apply_overrides(
        serde_json::from_value(map(vec![(
            "compaction",
            map(vec![
                ("reserveTokens", json!(1000)),
                ("keepRecentTokens", json!(1000)),
                (
                    "modelOverrides",
                    map(vec![(
                        MODEL_KEY,
                        map(vec![
                            ("reserveTokens", json!(0)),
                            ("keepRecentTokens", json!(0)),
                        ]),
                    )]),
                ),
            ]),
        )]))
        .expect("overrides"),
    );
    assert_eq!(
        manager.get_compaction_settings(Some(MODEL)).expect("valid"),
        ResolvedCompactionSettings {
            enabled: true,
            reserve_tokens: 0.0,
            keep_recent_tokens: 0.0,
        }
    );
}

// 补充：`modelOverrides` 中只要有一个坏条目，整张 map 会退回到原始 JSON；读取其它
// 合法条目时仍要正确解析数字。TS 的按需读取（`modelOverrides?.[modelKey]`）不会因为
// 同级的坏条目而失败。
#[test]
fn reads_valid_entries_alongside_malformed_model_overrides() {
    let storage = storage_with_global(map(vec![(
        "compaction",
        map(vec![(
            "modelOverrides",
            map(vec![
                ("provider/good", map(vec![("reserveTokens", json!(400000))])),
                ("provider/bad", Value::Null),
            ]),
        )]),
    )]));
    let manager = SettingsManager::from_storage(storage);
    assert_eq!(
        manager
            .get_compaction_reserve_tokens(Some(CompactionModelRef::new("provider", "good")))
            .expect("valid entry"),
        400000.0
    );
    let error = manager
        .get_compaction_settings(Some(CompactionModelRef::new("provider", "bad")))
        .expect_err("malformed entry");
    assert_eq!(
        error.to_string(),
        "Invalid compaction.modelOverrides[\"provider/bad\"] setting: null. Expected an object."
    );
}
