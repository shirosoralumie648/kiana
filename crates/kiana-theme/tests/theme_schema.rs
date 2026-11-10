//! `config-schemas.test.ts` 中主题相关的唯一用例
//! （`accepts $schema in themes and rejects malformed themes`）。
//!
//! 另外 12 个用例属于 `kiana-config`；`kiana-config` 不能 dev-depend on `kiana-theme`（正常依赖方向
//! 是 `kiana-theme -> kiana-config`），所以这里只移植主题部分。

mod common;

use common::{builtin_json, lock};
use kiana_theme::{ThemeJson, validate_theme_json};
use serde_json::{Value, json};

fn schema_url(name: &str) -> String {
    format!("https://kiana.dev/schemas/{name}.schema.json")
}

fn with(mut value: Value, edit: impl FnOnce(&mut serde_json::Map<String, Value>)) -> Value {
    if let Value::Object(object) = &mut value {
        edit(object);
    }
    value
}

#[test]
fn accepts_schema_in_themes_and_rejects_malformed_themes() {
    let _guard = lock();
    let built_in = builtin_json("dark");

    let accepted: ThemeJson = validate_theme_json(
        "custom",
        &with(built_in.clone(), |object| {
            object.insert("$schema".to_string(), json!(schema_url("theme")));
            object.insert("name".to_string(), json!("custom"));
        }),
    )
    .expect("$schema is accepted");
    assert_eq!(accepted.name.as_deref(), Some("custom"));

    let error = validate_theme_json(
        "invalid",
        &with(built_in.clone(), |object| {
            object.insert("name".to_string(), json!("invalid/name"));
        }),
    )
    .expect_err("slash names are rejected");
    assert!(
        error.to_string().contains("theme names cannot contain"),
        "{error}"
    );

    let error = validate_theme_json(
        "invalid",
        &with(built_in.clone(), |object| {
            object.insert("colors".to_string(), json!({}));
        }),
    )
    .expect_err("missing colors are rejected");
    assert!(
        error.to_string().contains("Missing required color tokens"),
        "{error}"
    );

    let invalids = [
        with(built_in.clone(), |object| {
            object.insert("unexpected".to_string(), json!(true));
        }),
        with(built_in.clone(), |object| {
            let colors = object
                .get_mut("colors")
                .and_then(Value::as_object_mut)
                .expect("colors");
            colors.insert("scrollbarThmb".to_string(), json!(""));
        }),
        with(built_in, |object| {
            object.insert("export".to_string(), json!({ "unexpected": "" }));
        }),
    ];
    for invalid in invalids {
        let error = validate_theme_json("invalid", &invalid).expect_err("additional properties");
        let message = error.to_string().to_lowercase();
        assert!(message.contains("additional properties"), "{error}");
    }
}
