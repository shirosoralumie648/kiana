//! `read_codemode_store`：分支上的 `codemode-store` 条目，从根开始应用。

use kiana_builtin_extensions::codemode::execute::read_codemode_store;
use kiana_session_core::session_manager::SessionEntry;
use serde_json::{Value, json};

fn entry(value: Value) -> SessionEntry {
    serde_json::from_value(value).expect("valid session entry")
}

fn custom_store(data: Value) -> SessionEntry {
    entry(json!({
        "type": "custom",
        "customType": "codemode-store",
        "data": data,
    }))
}

fn keys(map: &indexmap::IndexMap<String, Value>) -> Vec<&str> {
    map.keys().map(String::as_str).collect()
}

#[test]
fn applies_entries_from_the_root_in_branch_order() {
    let branch = vec![
        custom_store(json!({"set": {"a": 1, "b": 2}, "delete": []})),
        // 同一条目内先 delete 再 set。
        custom_store(json!({"set": {"a": 3}, "delete": ["b"]})),
    ];
    let store = read_codemode_store(&branch);
    assert_eq!(store.get("a"), Some(&json!(3)));
    assert!(!store.contains_key("b"));
}

#[test]
fn deleting_then_reinserting_moves_the_key_to_the_end() {
    let branch = vec![
        custom_store(json!({"set": {"a": 1, "b": 2}, "delete": []})),
        custom_store(json!({"set": {}, "delete": ["a"]})),
        custom_store(json!({"set": {"a": 9, "c": 3}, "delete": []})),
    ];
    let store = read_codemode_store(&branch);
    assert_eq!(keys(&store), vec!["b", "a", "c"]);
    assert_eq!(store.get("a"), Some(&json!(9)));
    assert_eq!(store.get("c"), Some(&json!(3)));
}

#[test]
fn delete_runs_before_set_within_one_entry() {
    let branch = vec![custom_store(json!({"set": {"x": 2}, "delete": ["x"]}))];
    let store = read_codemode_store(&branch);
    assert_eq!(store.get("x"), Some(&json!(2)));
}

#[test]
fn ignores_malformed_entries_and_other_custom_types() {
    let branch = vec![
        custom_store(json!({"set": {"kept": true}, "delete": []})),
        // `set` 不是对象/数组。
        custom_store(json!({"set": "nope", "delete": []})),
        // `delete` 不是字符串数组。
        custom_store(json!({"set": {"bad": 1}, "delete": [1]})),
        // data 不是对象。
        custom_store(Value::Null),
        // 其它 customType。
        entry(
            json!({"type": "custom", "customType": "other", "data": {"set": {"bad": 2}, "delete": []}}),
        ),
        // 非 custom 条目。
        entry(json!({"type": "session_info", "name": "x"})),
        custom_store(json!({"set": {"later": 1}, "delete": ["kept"]})),
    ];
    let store = read_codemode_store(&branch);
    assert!(!store.contains_key("kept"));
    assert!(!store.contains_key("bad"));
    assert_eq!(store.get("later"), Some(&json!(1)));
    assert_eq!(keys(&store), vec!["later"]);
}

#[test]
fn accepts_array_valued_sets_like_javascript_objects() {
    // TS 的 `typeof set === "object"` 不排除数组；`Object.entries` 用下标键。
    let branch = vec![custom_store(json!({"set": [10, 20], "delete": []}))];
    let store = read_codemode_store(&branch);
    assert_eq!(keys(&store), vec!["0", "1"]);
    assert_eq!(store.get("1"), Some(&json!(20)));
}
