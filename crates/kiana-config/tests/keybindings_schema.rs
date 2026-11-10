//! `core/keybindings-schema.ts` 与 `core/key-id-schema.ts` 的移植测试。
//!
//! `keybindings-schema.ts` 没有直接对应的 TS 测试；这里移植
//! `packages/coding-agent/test/config-schemas.test.ts` 中的两个键位用例：
//! - "validates keybinding syntax"
//! - "accepts $schema in keybindings.json without treating it as an action"
//!
//! 另外用 TS 参考实现生成的 `tests/fixtures/keybindings_schema.json`
//! （`JSON.stringify(KeybindingsSchema, null, 2)`，未经过 `addDefinitions`）逐字节校验
//! 属性集、顺序、描述与 §2.25 的正则模式（`spec/ca-config-models.md` §4.8）。

use std::fs;
use std::path::PathBuf;

use kiana_config::keybindings::key_id::{
    BASE_KEYS, KEY_ID_DESCRIPTION, KEY_ID_PATTERN, KEYBINDING_VALUE_SCHEMA, is_valid_key_id,
    is_valid_keybinding_value, key_id_schema, keybinding_value_schema,
};
use kiana_config::keybindings::schema::{KEYBINDINGS_SCHEMA, keybindings_schema};
use kiana_config::keybindings::{KeybindingsManager, keybindings};
use serde_json::Value;
use tempfile::TempDir;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn read_fixture(name: &str) -> String {
    fs::read_to_string(fixture(name)).expect("fixture exists")
}

fn read_fixture_json(name: &str) -> Value {
    kiana_json::js_parse::parse(&read_fixture(name)).expect("fixture is valid JSON")
}

// TS: "validates keybinding syntax"
#[test]
fn validates_keybinding_syntax() {
    let cases = read_fixture_json("key_id_cases.json");
    let cases = cases.get("values").and_then(Value::as_array).unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let value = case.get("value").expect("value");
        let expected = case.get("valid").and_then(Value::as_bool).expect("valid");
        assert_eq!(
            is_valid_keybinding_value(value),
            expected,
            "KeybindingValueSchema.Check({value})"
        );
    }
}

/// 与 TS `Compile(KeybindingValueSchema).Check` 的随机化差分语料（867 例，
/// 由 `/home/ctyun/kiana-work/upstream-reference` 的参考实现生成）。
#[test]
fn matches_the_typescript_validator_on_generated_corpus() {
    let cases = read_fixture_json("key_id_fuzz_cases.json");
    let cases = cases.get("values").and_then(Value::as_array).unwrap();
    assert!(cases.len() >= 800);
    for case in cases {
        let value = case.get("value").and_then(Value::as_str).expect("value");
        let expected = case.get("valid").and_then(Value::as_bool).expect("valid");
        assert_eq!(is_valid_key_id(value), expected, "{value:?}");
    }
}

#[test]
fn validates_the_documented_syntax_examples() {
    for binding in [
        "a",
        "9",
        "pageUp",
        "+",
        "ctrl+shift+x",
        "alt+ctrl+?",
        "ctrl+shift+alt+super+f12",
        "ctrl++",
    ] {
        assert!(is_valid_key_id(binding), "{binding} should be valid");
    }
    for binding in [
        "",
        "Ctrl+x",
        "control+x",
        "ctrl+not-a-key",
        "ctrl+ctrl+x",
        "ctrl+shift+alt+super+ctrl+x",
        "ctrl+",
        "++++",
        "ctrl+a\n",
    ] {
        assert!(!is_valid_key_id(binding), "{binding} should be invalid");
    }
}

#[test]
fn keybinding_value_accepts_strings_and_arrays_only() {
    assert!(is_valid_keybinding_value(&Value::String("ctrl+a".into())));
    assert!(is_valid_keybinding_value(&Value::Array(vec![])));
    assert!(is_valid_keybinding_value(&Value::Array(vec![
        Value::String("a".into()),
        Value::String("ctrl+b".into())
    ])));
    assert!(!is_valid_keybinding_value(&Value::Array(vec![
        Value::String("a".into()),
        Value::Number(1.into())
    ])));
    for value in [
        Value::Null,
        Value::Bool(true),
        Value::Number(1.into()),
        Value::Object(Default::default()),
    ] {
        assert!(!is_valid_keybinding_value(&value), "{value:?}");
    }
}

#[test]
fn base_keys_match_the_generated_fixture() {
    let expected = read_fixture_json("key_id_base_keys.json");
    let expected: Vec<&str> = expected
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert_eq!(BASE_KEYS.to_vec(), expected);
}

#[test]
fn pattern_and_description_are_verbatim() {
    // §2.25 的最终正则（343 字符，取自生成的 keybindings.schema.json）。
    assert_eq!(KEY_ID_PATTERN.len(), 343);
    assert!(KEY_ID_PATTERN.ends_with(")$"));
    assert!(KEY_ID_PATTERN.contains("(?!.*(?:ctrl\\+.*ctrl\\+"));
    assert_eq!(
        KEY_ID_DESCRIPTION,
        "Key identifier with optional ctrl, shift, alt, or super modifiers."
    );
    let schema = key_id_schema();
    assert_eq!(schema["type"], "string");
    assert_eq!(schema["pattern"], KEY_ID_PATTERN);
    assert_eq!(schema["description"], KEY_ID_DESCRIPTION);
}

#[test]
fn keybinding_value_schema_matches_the_ts_union_shape() {
    let schema = keybinding_value_schema();
    let any_of = schema["anyOf"].as_array().unwrap();
    assert_eq!(any_of.len(), 2);
    assert_eq!(any_of[0]["type"], "string");
    assert_eq!(any_of[1]["type"], "array");
    assert_eq!(any_of[1]["items"], any_of[0]);
    assert_eq!(&*KEYBINDING_VALUE_SCHEMA, &schema);
}

#[test]
fn whole_schema_matches_the_typescript_golden() {
    // `JSON.stringify(KeybindingsSchema, null, 2)` 的逐字节输出：
    // 覆盖属性顺序、描述、模式与 additionalProperties。
    let expected = read_fixture("keybindings_schema.json");
    let actual = kiana_json::js_object::stringify_pretty(&keybindings_schema(), 2);
    assert_eq!(actual, expected);
    assert_eq!(&*KEYBINDINGS_SCHEMA, &keybindings_schema());
}

#[test]
fn schema_has_ninety_one_properties_in_keybindings_order() {
    let schema = read_fixture_json("keybindings_schema.json");
    let properties = schema["properties"].as_object().unwrap();
    assert_eq!(properties.len(), 91);
    let keys: Vec<&str> = properties.keys().map(String::as_str).collect();
    assert_eq!(keys[0], "$schema");
    let expected: Vec<&str> = keybindings().keys().map(String::as_str).collect();
    assert_eq!(&keys[1..], expected.as_slice());
}

#[test]
fn schema_descriptions_come_from_keybindings() {
    let schema = keybindings_schema();
    let properties = schema["properties"].as_object().unwrap();
    for (id, definition) in keybindings() {
        assert_eq!(
            properties[id]["description"].as_str(),
            definition.description.as_deref(),
            "{id}"
        );
    }
    assert_eq!(properties["$schema"]["type"], "string");
}

// TS: "accepts $schema in keybindings.json without treating it as an action"
#[test]
fn accepts_schema_in_keybindings_json_without_treating_it_as_an_action() {
    let directory = TempDir::new().unwrap();
    fs::write(
        directory.path().join("keybindings.json"),
        serde_json::json!({
            "$schema": "https://kiana.dev/schemas/keybindings.schema.json",
            "app.session.new": "ctrl+n",
            "extension.invalid": "ctrl+not-a-key",
        })
        .to_string(),
    )
    .unwrap();

    let manager = KeybindingsManager::create(directory.path());
    let bindings = manager.get_user_bindings();
    assert_eq!(bindings.len(), 1);
    assert_eq!(
        bindings["app.session.new"],
        kiana_config::keybindings::KeyIdList::One("ctrl+n".to_string())
    );
    assert_eq!(
        manager.get_effective_config()["app.session.new"],
        kiana_config::keybindings::KeyIdList::One("ctrl+n".to_string())
    );
}
