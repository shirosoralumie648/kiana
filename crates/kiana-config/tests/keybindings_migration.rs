//! `keybindings-migration.test.ts` 的移植测试。
//!
//! TS 源：`packages/coding-agent/test/keybindings-migration.test.ts`（3 个 `it(...)`）。
//!
//! 重要（审计修正）：`keybindings.ts` **从不**写 `keybindings.json`；真正的文件重写在
//! `ca-cli` 的 `runMigrations`（`packages/coding-agent/src/migrations.ts`）。因此：
//! - case 1–2 在 Rust 侧直接断言纯函数 [`migrate_keybindings_config`] 的输出
//!   （TS 测试通过 `runMigrations` 间接观察同一个纯函数的结果）；
//! - case 3 直接移植：`KeybindingsManager.create` 在文件被重写前就在内存中加载旧名。

use std::fs;
use std::path::PathBuf;

use indexmap::IndexMap;
use kiana_config::keybindings::{
    KeyIdList, KeybindingsConfig, KeybindingsManager, migrate_keybindings_config,
};
use serde_json::{Value, json};
use tempfile::TempDir;

fn config(pairs: Vec<(&str, Value)>) -> IndexMap<String, Value> {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

fn keys<V>(map: &IndexMap<String, V>) -> Vec<&str> {
    map.keys().map(String::as_str).collect()
}

/// 与 TS `createAgentDir` 一致：`JSON.stringify(config, null, 2) + "\n"`。
fn create_agent_dir(config: &Value) -> TempDir {
    let dir = TempDir::new().expect("temp dir");
    let text = format!("{}\n", kiana_json::js_object::stringify_pretty(config, 2));
    fs::write(dir.path().join("keybindings.json"), text).expect("write keybindings.json");
    dir
}

// TS: "rewrites old key names to namespaced ids"
#[test]
fn rewrites_old_key_names_to_namespaced_ids() {
    let raw = config(vec![
        ("cursorUp", json!(["up", "ctrl+p"])),
        ("expandTools", json!("ctrl+x")),
    ]);

    let result = migrate_keybindings_config(&raw);

    assert!(result.migrated);
    // 输出顺序 = KEYBINDINGS 顺序：`tui.editor.cursorUp` 在 `app.tools.expand` 之前。
    assert_eq!(
        keys(&result.config),
        vec!["tui.editor.cursorUp", "app.tools.expand"]
    );
    assert_eq!(
        result.config["tui.editor.cursorUp"],
        json!(["up", "ctrl+p"])
    );
    assert_eq!(result.config["app.tools.expand"], json!("ctrl+x"));
}

// TS: "keeps the namespaced value when old and new names both exist"
#[test]
fn keeps_the_namespaced_value_when_old_and_new_names_both_exist() {
    let raw = config(vec![
        ("expandTools", json!("ctrl+x")),
        ("app.tools.expand", json!("ctrl+y")),
    ]);

    let result = migrate_keybindings_config(&raw);

    assert!(result.migrated);
    assert_eq!(result.config.len(), 1);
    assert_eq!(result.config["app.tools.expand"], json!("ctrl+y"));
    assert_eq!(
        kiana_json::js_object::stringify(&Value::Object(
            result
                .config
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        )),
        r#"{"app.tools.expand":"ctrl+y"}"#
    );
}

// TS: "loads old key names in memory before the file is rewritten"
#[test]
fn loads_old_key_names_in_memory_before_the_file_is_rewritten() {
    let agent_dir = create_agent_dir(&json!({
        "selectConfirm": "enter",
        "interrupt": "ctrl+x",
    }));

    let keybindings = KeybindingsManager::create(agent_dir.path());

    let user_bindings = keybindings.get_user_bindings();
    assert_eq!(
        keys(&user_bindings),
        vec!["tui.select.confirm", "app.interrupt"]
    );
    assert_eq!(
        user_bindings["tui.select.confirm"],
        kiana_config::keybindings::KeyIdList::One("enter".to_string())
    );
    assert_eq!(
        user_bindings["app.interrupt"],
        kiana_config::keybindings::KeyIdList::One("ctrl+x".to_string())
    );

    let effective = keybindings.get_effective_config();
    assert_eq!(
        effective["tui.select.confirm"],
        kiana_config::keybindings::KeyIdList::One("enter".to_string())
    );
    assert_eq!(
        effective["app.interrupt"],
        kiana_config::keybindings::KeyIdList::One("ctrl+x".to_string())
    );

    // 文件本身未被改写（本单元没有 writer）。
    let text = fs::read_to_string(agent_dir.path().join("keybindings.json")).unwrap();
    assert!(text.contains("selectConfirm"));
}

// ---------------------------------------------------------------------------
// 额外的纯函数边界用例
// ---------------------------------------------------------------------------

#[test]
fn extras_are_appended_in_js_string_order() {
    let raw = config(vec![
        ("zzz.custom", json!("a")),
        ("app.clear", json!("ctrl+c")),
        ("aaa.custom", json!("b")),
        ("tui.input.tab", json!("tab")),
    ]);

    let result = migrate_keybindings_config(&raw);
    assert_eq!(
        keys(&result.config),
        vec!["tui.input.tab", "app.clear", "aaa.custom", "zzz.custom"]
    );
    assert!(!result.migrated);
}

#[test]
fn migration_does_not_touch_schema_or_unknown_keys() {
    let raw = config(vec![
        (
            "$schema",
            json!("https://kiana.dev/schemas/keybindings.schema.json"),
        ),
        ("extension.action", json!("ctrl+shift+x")),
        ("notAKeybinding", json!("ctrl+y")),
    ]);

    let result = migrate_keybindings_config(&raw);
    assert!(!result.migrated);
    assert_eq!(
        keys(&result.config),
        vec!["$schema", "extension.action", "notAKeybinding"]
    );
}

// ---------------------------------------------------------------------------
// 审计回归：JS 对象自有键枚举序
// ---------------------------------------------------------------------------

/// `JSON.parse` 之后的原始配置（`kiana_json::js_parse` 会做与 JS 相同的键重排）。
fn parse_config(text: &str) -> IndexMap<String, Value> {
    match kiana_json::js_parse::parse(text).expect("valid JSON") {
        Value::Object(map) => map.into_iter().collect(),
        other => panic!("expected object: {other:?}"),
    }
}

// TS: `Object.keys(migrateKeybindingsConfig(JSON.parse(
//   '{"10":"ctrl+a","2":"ctrl+b","app.clear":"ctrl+c"}')).config)` === ["2","10","app.clear"]
#[test]
fn integer_index_keys_are_enumerated_first_in_ascending_order() {
    let raw = parse_config(r#"{"10":"ctrl+a","2":"ctrl+b","app.clear":"ctrl+c"}"#);
    let result = migrate_keybindings_config(&raw);

    assert!(!result.migrated);
    assert_eq!(keys(&result.config), vec!["2", "10", "app.clear"]);
    // 序列化结果同样按 JS 键序（stringify 会再次提升整数键，此处验证值未错位）。
    assert_eq!(
        kiana_json::js_object::stringify(&Value::Object(
            result
                .config
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        )),
        r#"{"2":"ctrl+b","10":"ctrl+a","app.clear":"ctrl+c"}"#
    );

    // 同一文件经 KeybindingsManager::create 读取后，getUserBindings 的键序一致。
    let agent_dir = create_agent_dir(&json!({
        "10": "ctrl+a",
        "2": "ctrl+b",
        "app.clear": "ctrl+c",
    }));
    let manager = KeybindingsManager::create(agent_dir.path());
    let bindings = manager.get_user_bindings();
    assert_eq!(keys(&bindings), vec!["2", "10", "app.clear"]);
    assert_eq!(
        bindings["10"],
        kiana_config::keybindings::KeyIdList::One("ctrl+a".to_string())
    );
}

// 顶层数组的 12 个元素：TS `Object.keys` 为 ["0","1","2",...,"11"]。
#[test]
fn top_level_array_keys_enumerate_in_numeric_order() {
    let agent_dir = TempDir::new().unwrap();
    fs::write(
        agent_dir.path().join("keybindings.json"),
        r#"["ctrl+a","ctrl+b","ctrl+c","ctrl+d","ctrl+e","ctrl+f","ctrl+g","ctrl+h","ctrl+i","ctrl+j","ctrl+k","ctrl+l"]"#,
    )
    .unwrap();

    let manager = KeybindingsManager::create(agent_dir.path());
    let bindings = manager.get_user_bindings();
    let expected: Vec<String> = (0..12).map(|index| index.to_string()).collect();
    assert_eq!(keys(&bindings), expected);
}

// 非数组索引形态的键（前导零、"-0"、2^32-1）保持普通字符串插入序，排在索引键之后。
#[test]
fn non_index_numeric_looking_keys_follow_insertion_order() {
    let raw = parse_config(
        r#"{"zzz":"ctrl+a","5":"ctrl+b","app.interrupt":"escape","01":"ctrl+c","4294967295":"ctrl+d","4294967294":"ctrl+e","-0":"ctrl+f","00":"ctrl+g"}"#,
    );
    let result = migrate_keybindings_config(&raw);

    assert_eq!(
        keys(&result.config),
        vec![
            "5",
            "4294967294",
            "app.interrupt",
            "-0",
            "00",
            "01",
            "4294967295",
            "zzz"
        ]
    );
}

// 构造器 / setUserBindings 收到的配置在 JS 里恒为普通对象：整数索引键总是先按升序枚举。
#[test]
fn manager_normalizes_integer_keys_like_a_js_object() {
    let mut user = KeybindingsConfig::new();
    user.insert("10".to_string(), KeyIdList::One("ctrl+a".to_string()));
    user.insert("2".to_string(), KeyIdList::One("ctrl+b".to_string()));
    user.insert(
        "app.clear".to_string(),
        KeyIdList::One("ctrl+c".to_string()),
    );

    let manager = KeybindingsManager::new(user, None);
    assert_eq!(
        keys(&manager.get_user_bindings()),
        vec!["2", "10", "app.clear"]
    );

    let mut user = KeybindingsConfig::new();
    user.insert("11".to_string(), KeyIdList::One("ctrl+a".to_string()));
    user.insert("3".to_string(), KeyIdList::One("ctrl+b".to_string()));
    let mut manager = KeybindingsManager::new(KeybindingsConfig::new(), None);
    manager.set_user_bindings(user);
    assert_eq!(keys(&manager.get_user_bindings()), vec!["3", "11"]);
}

// ---------------------------------------------------------------------------
// 审计回归：`in` 命中原型链
// ---------------------------------------------------------------------------

// TS: migrateKeybindingsConfig({"toString":"ctrl+z"}) ->
//   {config: {"function toString() { [native code] }": "ctrl+z"}, migrated: true}
#[test]
fn object_prototype_members_are_treated_as_legacy_names() {
    let raw = parse_config(r#"{"toString":"ctrl+z"}"#);
    let result = migrate_keybindings_config(&raw);
    assert!(result.migrated);
    assert_eq!(
        keys(&result.config),
        vec!["function toString() { [native code] }"]
    );

    for (name, target) in [
        ("constructor", "function Object() { [native code] }"),
        ("__proto__", "[object Object]"),
        ("valueOf", "function valueOf() { [native code] }"),
        (
            "hasOwnProperty",
            "function hasOwnProperty() { [native code] }",
        ),
        (
            "toLocaleString",
            "function toLocaleString() { [native code] }",
        ),
        (
            "isPrototypeOf",
            "function isPrototypeOf() { [native code] }",
        ),
        (
            "propertyIsEnumerable",
            "function propertyIsEnumerable() { [native code] }",
        ),
        (
            "__defineGetter__",
            "function __defineGetter__() { [native code] }",
        ),
        (
            "__defineSetter__",
            "function __defineSetter__() { [native code] }",
        ),
        (
            "__lookupGetter__",
            "function __lookupGetter__() { [native code] }",
        ),
        (
            "__lookupSetter__",
            "function __lookupSetter__() { [native code] }",
        ),
    ] {
        let raw = config(vec![(name, json!("ctrl+z"))]);
        let result = migrate_keybindings_config(&raw);
        assert!(result.migrated, "{name}");
        assert_eq!(keys(&result.config), vec![target], "{name}");
        assert_eq!(result.config[target], json!("ctrl+z"), "{name}");
    }
}

// TS: 原名与被强制转换后的键同时存在时，新键获胜（`Object.hasOwn(rawConfig, nextKey)`）。
#[test]
fn coerced_prototype_key_wins_when_both_exist() {
    let raw =
        parse_config(r#"{"toString":"ctrl+z","function toString() { [native code] }":"ctrl+y"}"#);
    let result = migrate_keybindings_config(&raw);

    assert!(result.migrated);
    assert_eq!(result.config.len(), 1);
    assert_eq!(
        result.config["function toString() { [native code] }"],
        json!("ctrl+y")
    );
}

#[test]
fn renames_are_detected_even_when_the_target_is_missing() {
    let raw = config(vec![("deleteSessionNoninvasive", json!("ctrl+backspace"))]);
    let result = migrate_keybindings_config(&raw);
    assert!(result.migrated);
    assert_eq!(keys(&result.config), vec!["app.session.deleteNoninvasive"]);
}

#[test]
fn nonexistent_or_invalid_files_load_as_empty_bindings() {
    let missing = TempDir::new().unwrap();
    let manager = KeybindingsManager::create(missing.path());
    assert!(manager.get_user_bindings().is_empty());

    let invalid = TempDir::new().unwrap();
    fs::write(invalid.path().join("keybindings.json"), "{ not json").unwrap();
    let manager = KeybindingsManager::create(invalid.path());
    assert!(manager.get_user_bindings().is_empty());

    let non_object = TempDir::new().unwrap();
    fs::write(non_object.path().join("keybindings.json"), "42").unwrap();
    let manager = KeybindingsManager::create(non_object.path());
    assert!(manager.get_user_bindings().is_empty());
}

#[test]
fn invalid_values_are_silently_dropped_and_unknown_valid_keys_kept() {
    let agent_dir = create_agent_dir(&json!({
        "app.session.new": "ctrl+n",
        "extension.invalid": "ctrl+not-a-key",
        "extension.number": 3,
        "extension.array": ["ctrl+a", "bogus"],
        "extension.empty": [],
    }));

    let manager = KeybindingsManager::create(agent_dir.path());
    let bindings = manager.get_user_bindings();
    assert_eq!(keys(&bindings), vec!["app.session.new", "extension.empty"]);
    assert_eq!(
        bindings["extension.empty"],
        kiana_config::keybindings::KeyIdList::Many(vec![])
    );
    assert_eq!(
        manager.get_effective_config()["app.session.new"],
        kiana_config::keybindings::KeyIdList::One("ctrl+n".to_string())
    );
}

#[test]
fn reload_rereads_the_configuration_file() {
    let agent_dir = create_agent_dir(&json!({ "app.interrupt": "ctrl+x" }));
    let mut manager = KeybindingsManager::create(agent_dir.path());
    assert_eq!(
        manager.get_user_bindings()["app.interrupt"],
        kiana_config::keybindings::KeyIdList::One("ctrl+x".to_string())
    );

    fs::write(
        agent_dir.path().join("keybindings.json"),
        "{\"app.interrupt\":\"escape\"}",
    )
    .unwrap();
    manager.reload();
    assert_eq!(
        manager.get_user_bindings()["app.interrupt"],
        kiana_config::keybindings::KeyIdList::One("escape".to_string())
    );
}

#[test]
fn top_level_array_is_treated_as_an_object() {
    // JS 的 `typeof [] === "object"`，`loadRawConfig` 因此接受顶层数组；
    // `Object.entries` 给出 "0"、"1"…，随后按值校验（合法数组值被保留为额外键）。
    let agent_dir = TempDir::new().unwrap();
    fs::write(
        agent_dir.path().join("keybindings.json"),
        r#"["ctrl+a", "ctrl+b"]"#,
    )
    .unwrap();
    let manager = KeybindingsManager::create(agent_dir.path());
    let bindings = manager.get_user_bindings();
    assert_eq!(keys(&bindings), vec!["0", "1"]);
}

#[test]
fn bom_prefixed_file_is_parsed() {
    let agent_dir = TempDir::new().unwrap();
    let path: PathBuf = agent_dir.path().join("keybindings.json");
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(br#"{"app.interrupt":"escape"}"#);
    fs::write(path, bytes).unwrap();

    let manager = KeybindingsManager::create(agent_dir.path());
    assert_eq!(
        manager.get_user_bindings()["app.interrupt"],
        kiana_config::keybindings::KeyIdList::One("escape".to_string())
    );
}
