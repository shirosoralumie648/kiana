//! `packages/ai/test/streaming-json.test.ts` 的移植。
//!
//! 差分部分有两层：
//! - fixture（`tests/fixtures/utils/streaming_json.json`）由 TS 参照实现（`upstream-reference`）逐片记录
//!   快照与变更序列，覆盖转义、代理对、`__proto__`、重复键与畸形文本；
//! - 原生差分模糊测试（`tests/support/streaming_json_reference.rs`）逐字移植 TS 的种子生成器
//!   与独立递归下降 `referenceSnapshot`，按 TS 的完整规模（250/400/300/150 个文档，chunk size
//!   1/2/3/7/random）在每个前缀上对拍，并校验变更流维护的副本。

use kiana_ai::utils::streaming_json::{
    JsonChange, PathSegment, StreamingJsonParser, ToolCallArgumentsStream,
    parse_tool_call_arguments,
};
use serde_json::Value;

#[path = "support/streaming_json_reference.rs"]
mod ts_reference;

use ts_reference::{Rand, is_js_space, random_documents, read32, reference_snapshot};

fn load_fixture() -> Value {
    let path = format!(
        "{}/tests/fixtures/utils/streaming_json.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture readable"))
        .expect("fixture is valid JSON")
}

fn object_from_json(value: Value) -> kiana_ai::types::JsonObject {
    value
        .as_object()
        .expect("object")
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// JS 与 Rust 的数字表示差异（`12` vs `12.0`）不应算作不同；`-0` 与 `0` 也不应。
fn json_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => match (a.as_f64(), b.as_f64()) {
            (Some(a), Some(b)) => a == b || (a.is_nan() && b.is_nan()),
            _ => a == b,
        },
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| json_equal(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, value)| b.get(key).is_some_and(|other| json_equal(value, other)))
        }
        _ => left == right,
    }
}

fn path_from_json(value: &Value) -> Vec<PathSegment> {
    value
        .as_array()
        .expect("path array")
        .iter()
        .map(|segment| match segment {
            Value::Object(map) => {
                if let Some(key) = map.get("key") {
                    PathSegment::Key(key.as_str().expect("key").to_string())
                } else {
                    PathSegment::Index(
                        map.get("index").and_then(Value::as_u64).expect("index") as usize
                    )
                }
            }
            other => panic!("unexpected path segment {other:?}"),
        })
        .collect()
}

/// 变更的文本形式（与 fixture 的 `value_text` / `text` 同构）。
fn change_text(change: &JsonChange) -> (String, String, String) {
    match change {
        JsonChange::Set { path, value } => (
            "set".into(),
            path_text(path),
            kiana_json::js_object::stringify(value),
        ),
        JsonChange::Append { path, text } => ("append".into(), path_text(path), text.clone()),
    }
}

fn path_text(path: &[PathSegment]) -> String {
    path.iter()
        .map(|segment| match segment {
            PathSegment::Key(key) => format!("k:{key}"),
            PathSegment::Index(index) => format!("i:{index}"),
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn expected_change_text(value: &Value) -> (String, String, String) {
    let kind = value.get("kind").and_then(Value::as_str).expect("kind");
    let path = path_text(&path_from_json(value.get("path").expect("path")));
    let body = match kind {
        "set" => value
            .get("value_text")
            .and_then(Value::as_str)
            .expect("value_text"),
        "append" => value.get("text").and_then(Value::as_str).expect("text"),
        other => panic!("unexpected change kind {other}"),
    };
    (kind.to_string(), path, body.to_string())
}

#[test]
fn matches_the_reference_after_every_piece_and_reports_the_same_changes() {
    let fixture = load_fixture();
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let text = case["text"].as_str().expect("text");
        let mut parser = StreamingJsonParser::new();
        for step in case["steps"].as_array().expect("steps") {
            let chunk = step["chunk"].as_str().expect("chunk");
            let read = step["read"].as_str().expect("read");
            let expected_changes: Vec<(String, String, String)> = step["changes"]
                .as_array()
                .expect("changes")
                .iter()
                .map(expected_change_text)
                .collect();
            let actual_changes: Vec<(String, String, String)> = parser
                .append_changes(chunk)
                .iter()
                .map(change_text)
                .collect();
            if actual_changes != expected_changes {
                failures.push(format!(
                    "text {text:?} read {read:?}: changes\n  got {actual_changes:?}\n  want {expected_changes:?}"
                ));
                break;
            }
            if step["reference_readable"].as_bool().unwrap_or(false) {
                let actual = parser
                    .snapshot()
                    .map(|value| kiana_json::js_object::stringify(&value));
                let expected = step["snapshot_text"].as_str().map(str::to_string);
                if actual != expected {
                    failures.push(format!(
                        "text {text:?} read {read:?}: snapshot\n  got {actual:?}\n  want {expected:?}"
                    ));
                    break;
                }
            }
        }
        if failures.len() > 5 {
            break;
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn reads_complete_documents_exactly_like_json_parse() {
    let fixture = load_fixture();
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let Some(expected) = case["final_json_text"].as_str() else {
            continue;
        };
        let text = case["text"].as_str().expect("text");
        let mut parser = StreamingJsonParser::new();
        parser.append(text);
        let actual = parser.snapshot().map_or_else(
            || "null".to_string(),
            |v| kiana_json::js_object::stringify(&v),
        );
        if actual != expected {
            failures.push(format!("{text:?}: got {actual}, want {expected}"));
        }
        if failures.len() > 5 {
            break;
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn repairs_raw_control_characters_and_invalid_escapes_like_parse_streaming_json() {
    let malformed = [
        "\n", "\t", "\u{0}", "\\d", "\\x41", "\\u12", "\\uZZZZ", "\\u00e9", "\\\\", "\\q",
    ];
    let mut state: u64 = 6;
    let mut next = || {
        state = (state * 1_103_515_245 + 12345) & 0x7fff_ffff;
        state as f64 / 0x7fff_ffff as f64
    };
    for round in 0..300 {
        let mut content = String::new();
        for _ in 0..6 {
            let index = (next() * (malformed.len() + 3) as f64) as usize;
            content.push_str(match index {
                i if i < malformed.len() => malformed[i],
                i if i == malformed.len() => "a",
                i if i == malformed.len() + 1 => " ",
                _ => "é",
            });
        }
        let text = format!("{{\"path\":\"p\",\"content\":\"{content}\",\"n\":{round}}}");
        let mut parser = StreamingJsonParser::new();
        for ch in text.chars() {
            parser.append(&ch.to_string());
        }
        let actual = parser.snapshot().expect("snapshot");
        let expected = kiana_ai::utils::json_parse::parse_streaming_json(Some(&text));
        if !json_equal(&actual, &expected) {
            panic!("{text:?}: got {actual}, want {expected}");
        }
    }
}

#[test]
fn shows_partial_arguments_as_an_object_and_finishes_with_one_full_parse() {
    let mut args = ToolCallArgumentsStream::new();
    assert_eq!(args.append("  "), kiana_ai::types::JsonObject::new());
    assert_eq!(
        args.append(r#"{"path":"a.ts","content":"x"#),
        object_from_json(serde_json::json!({"path": "a.ts", "content": "x"}))
    );
    args.append("y");
    let before = args.partial();
    args.append("");
    assert_eq!(args.partial(), before);
    assert_eq!(args.text(), r#"  {"path":"a.ts","content":"xy"#);
    assert_eq!(
        args.finish(),
        parse_tool_call_arguments(r#"  {"path":"a.ts","content":"xy"#)
    );
    args.append(r#""}"#);
    let finish = args.finish();
    assert_eq!(finish.arguments_error, None);
    assert_eq!(
        finish.arguments.get("content").and_then(Value::as_str),
        Some("xy")
    );
}

#[test]
fn shows_non_object_roots_as_no_arguments_yet() {
    let mut args = ToolCallArgumentsStream::new();
    assert_eq!(args.append("[1,2"), kiana_ai::types::JsonObject::new());
    args.append("]");
    assert!(
        args.finish()
            .arguments_error
            .expect("error")
            .starts_with("Tool arguments must be a JSON object")
    );
}

#[test]
fn keeps_the_partial_handle_while_the_snapshot_stays_undefined() {
    // TS `#partial()`：`snapshot === this.#snapshot`（两边都是 `undefined`）→ 返回同一个
    // `#arguments` 对象，不产生新 partial（reviewer probe partial2: " ", "x", "y"）。
    let mut args = ToolCallArgumentsStream::new();
    let before = args.partial();
    for delta in [" ", "x", "y"] {
        args.append(delta);
        let after = args.partial();
        assert!(
            std::sync::Arc::ptr_eq(&before.arguments, &after.arguments),
            "append {delta:?} replaced the partial handle"
        );
        assert_eq!(after.revision, before.revision);
    }
}

#[test]
fn keeps_the_partial_handle_for_strict_equal_primitive_snapshots() {
    // 原始值在 JS 里按 `===` 比较：重复的相同布尔/数字/字符串快照不算变化。
    for (first, second) in [("true", " "), ("1", " "), ("\"a", "\"")] {
        let mut args = ToolCallArgumentsStream::new();
        args.append(first);
        let before = args.partial();
        args.append(second);
        let after = args.partial();
        assert!(
            std::sync::Arc::ptr_eq(&before.arguments, &after.arguments),
            "{first:?} then {second:?} replaced the partial handle"
        );
        assert_eq!(after.revision, before.revision, "{first:?}/{second:?}");
    }
}

#[test]
fn replaces_the_partial_handle_when_an_object_snapshot_is_rebuilt() {
    // 对象按引用比较：即使内容相同，解析器重建的快照也是新对象 → TS 返回新的 arguments。
    let mut args = ToolCallArgumentsStream::new();
    args.append(r#"{"a":1"#);
    let before = args.partial();
    args.append("}");
    let after = args.partial();
    assert!(!std::sync::Arc::ptr_eq(&before.arguments, &after.arguments));
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(*after.arguments, *before.arguments);
}

#[test]
fn replaces_the_text_with_the_providers_complete_text_returning_the_missing_piece() {
    let mut args = ToolCallArgumentsStream::new();
    args.append(r#"{"a":"x"#);
    let replace = args.replace(r#"{"a":"xyz"}"#);
    assert_eq!(replace.delta.as_deref(), Some(r#"yz"}"#));
    assert_eq!(
        replace.arguments,
        object_from_json(serde_json::json!({"a": "xyz"}))
    );
    let replace = args.replace(r#"{"b":2}"#);
    assert_eq!(replace.delta, None);
    assert_eq!(
        replace.arguments,
        object_from_json(serde_json::json!({"b": 2}))
    );
    assert_eq!(args.text(), r#"{"b":2}"#);
}

#[test]
fn keeps_object_members_in_first_insertion_order_for_duplicate_keys() {
    let mut parser = StreamingJsonParser::new();
    parser.append(r#"{"a":1,"b":2,"a":"three"}"#);
    let snapshot = parser.snapshot().expect("snapshot");
    let keys: Vec<&String> = snapshot.as_object().expect("object").keys().collect();
    assert_eq!(keys, vec!["a", "b"]);
    assert_eq!(snapshot["a"], Value::String("three".into()));
}

#[test]
fn makes_proto_an_own_member() {
    let text = r#"{"__proto__":{"polluted":true},"b":{"__proto__":"x"}}"#;
    for split in 0..=text.len() {
        if !text.is_char_boundary(split) {
            continue;
        }
        let mut parser = StreamingJsonParser::new();
        parser.append(&text[..split]);
        let _ = parser.snapshot();
        parser.append(&text[split..]);
        let snapshot = parser.snapshot().expect("snapshot");
        let expected: Value = serde_json::from_str(text).expect("json");
        assert_eq!(snapshot, expected);
        assert!(
            snapshot
                .as_object()
                .expect("object")
                .contains_key("__proto__")
        );
    }
}

#[test]
fn joins_surrogate_pairs_split_across_pieces_escaped_or_raw() {
    let escaped = r#"{"a":"😀!"}"#;
    let raw = "{\"a\":\"😀!\"}";
    for text in [escaped, raw] {
        for split in 0..=text.len() {
            if !text.is_char_boundary(split) {
                continue;
            }
            let mut parser = StreamingJsonParser::new();
            parser.append(&text[..split]);
            let _ = parser.snapshot();
            parser.append(&text[split..]);
            assert_eq!(parser.snapshot(), Some(serde_json::json!({"a": "😀!"})));
        }
    }
}

#[test]
fn keeps_trailing_whitespace_of_an_unterminated_string() {
    let mut parser = StreamingJsonParser::new();
    parser.append("{\"code\":\"x\t\t  ");
    assert_eq!(
        parser.snapshot(),
        Some(serde_json::json!({"code": "x\t\t  "}))
    );
}

#[test]
fn keeps_invalid_unicode_escapes_literal_without_dropping_fields() {
    assert_eq!(
        final_snapshot(
            r#"{"path":"a.tex","content":"\usepackage{x}","mode":"w"}"#,
            1
        ),
        Some(serde_json::json!({"path": "a.tex", "content": "\\usepackage{x}", "mode": "w"}))
    );
    let mut parser = StreamingJsonParser::new();
    parser.append(r#"{"text":"ab\u00"#);
    assert_eq!(parser.snapshot(), Some(serde_json::json!({"text": "ab"})));
}

#[test]
fn reads_the_ported_partial_json_and_fix_json_vectors() {
    let vectors: &[(&str, Option<Value>)] = &[
        ("\"", Some(serde_json::json!(""))),
        ("[\"", Some(serde_json::json!([""]))),
        ("{\"\": \"", Some(serde_json::json!({"": ""}))),
        ("n", Some(Value::Null)),
        ("t", Some(Value::Bool(true))),
        ("f", Some(Value::Bool(false))),
        ("0", Some(serde_json::json!(0))),
        ("-1.25e+4", Some(serde_json::json!(-12500.0))),
        ("-1.25e+", Some(serde_json::json!(-1.25))),
        ("-1.25e", Some(serde_json::json!(-1.25))),
        ("\" \\x12", Some(serde_json::json!(" \\x12"))),
        ("I", None),
        ("-I", None),
        ("N", None),
        ("", None),
        ("nul", Some(Value::Null)),
        ("fals", Some(Value::Bool(false))),
        ("12.2", Some(serde_json::json!(12.2))),
        ("-12", Some(serde_json::json!(-12))),
        ("-", None),
        ("2.5e", Some(serde_json::json!(2.5))),
        ("2.5e-", Some(serde_json::json!(2.5))),
        ("2.5e3", Some(serde_json::json!(2500.0))),
        ("-2.5e3", Some(serde_json::json!(-2500.0))),
        ("2.5E", Some(serde_json::json!(2.5))),
        ("2.5E-", Some(serde_json::json!(2.5))),
        ("2.5E3", Some(serde_json::json!(2500.0))),
        ("-2.5E3", Some(serde_json::json!(-2500.0))),
        ("12.34e", Some(serde_json::json!(12.34))),
        ("5e", Some(serde_json::json!(5))),
        ("\"abc", Some(serde_json::json!("abc"))),
        (
            "\"value with \\\"quoted\\\" text and \\\\ escape",
            Some(serde_json::json!(
                "value with \"quoted\" text and \\ escape"
            )),
        ),
        ("\"value with \\", Some(serde_json::json!("value with "))),
        ("\"\\u", Some(serde_json::json!(""))),
        ("\"\\u12", Some(serde_json::json!(""))),
        ("\"text \\u00", Some(serde_json::json!("text "))),
        ("{\"a\":\"\\u12", Some(serde_json::json!({"a": ""}))),
        (
            "\"value with unicode \u{3c}\"",
            Some(serde_json::json!("value with unicode <")),
        ),
        ("[", Some(serde_json::json!([]))),
        ("[[1], [2", Some(serde_json::json!([[1], [2]]))),
        ("[[\"1\"], [\"2", Some(serde_json::json!([["1"], ["2"]]))),
        ("[[false], [nu", Some(serde_json::json!([[false], [null]]))),
        ("[[[]], [[]", Some(serde_json::json!([[[]], [[]]]))),
        ("[[{}], [{", Some(serde_json::json!([[{}], [{}]]))),
        ("[1, ", Some(serde_json::json!([1]))),
        ("[[], 123", Some(serde_json::json!([[], 123]))),
        ("{\"key\":", Some(serde_json::json!({}))),
        (
            "{\"a\": {\"b\": 1}, \"c\": {\"d\": 2",
            Some(serde_json::json!({"a": {"b": 1}, "c": {"d": 2}})),
        ),
        ("{\"ke", Some(serde_json::json!({}))),
        ("{\"k1\": 1, \"k2", Some(serde_json::json!({"k1": 1}))),
        ("{\"k1\": 1, \"k2\":", Some(serde_json::json!({"k1": 1}))),
        (
            "{\"key\": \"value\"  ",
            Some(serde_json::json!({"key": "value"})),
        ),
        (
            "{\"a\": {\"b\": {}",
            Some(serde_json::json!({"a": {"b": {}}})),
        ),
        ("[1, [2, 3, [", Some(serde_json::json!([1, [2, 3, []]]))),
        (
            "[false, [true, [",
            Some(serde_json::json!([false, [true, []]])),
        ),
        (
            "{\"key\": {\"subKey\":",
            Some(serde_json::json!({"key": {}})),
        ),
        (
            "{\"key\": 123, \"key2\": {\"subKey\":",
            Some(serde_json::json!({"key": 123, "key2": {}})),
        ),
        (
            "{\"key\": null, \"key2\": {\"subKey\":",
            Some(serde_json::json!({"key": null, "key2": {}})),
        ),
        (
            "{\"key\": [1, 2, {",
            Some(serde_json::json!({"key": [1, 2, {}]})),
        ),
        (
            "[1, 2, {\"key\": \"value\",",
            Some(serde_json::json!([1, 2, {"key": "value"}])),
        ),
        (
            "{\"a\": {\"b\": [\"c\", {\"d\": \"e\",",
            Some(serde_json::json!({"a": {"b": ["c", {"d": "e"}]}})),
        ),
        (
            "{\"a\": {\"b\": {\"c\": {\"d\":",
            Some(serde_json::json!({"a": {"b": {"c": {}}}})),
        ),
        (
            "{\"a\": 1, \"b\": [",
            Some(serde_json::json!({"a": 1, "b": []})),
        ),
        (
            "{\"a\": 1, \"b\": {",
            Some(serde_json::json!({"a": 1, "b": {}})),
        ),
        (
            "{\"a\": 1, \"b\": \"",
            Some(serde_json::json!({"a": 1, "b": ""})),
        ),
        ("12.", None),
        ("12.e", None),
    ];
    let mut failures = Vec::new();
    for (input, expected) in vectors {
        for size in [input.len().max(1), 1, 2] {
            let actual = final_snapshot(input, size);
            let ok = match (expected, &actual) {
                (None, None) => true,
                (Some(expected), Some(actual)) => json_equal(actual, expected),
                _ => false,
            };
            if !ok {
                failures.push(format!(
                    "{input:?} (size {size}): got {actual:?}, want {expected:?}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn tolerates_missing_commas_and_colons_and_drops_values_in_invalid_positions() {
    let check = |text: &str, expected: Value| {
        let actual = final_snapshot(text, 1).expect("snapshot");
        assert!(
            json_equal(&actual, &expected),
            "{text:?}: got {actual}, want {expected}"
        );
    };
    check(r#"{"a":1 "b":2}"#, serde_json::json!({"a": 1, "b": 2}));
    check(r#"{"a" 1, "b": 2}"#, serde_json::json!({"a": 1, "b": 2}));
    check("[1 2, , 3,]", serde_json::json!([1, 2, 3]));
    check(
        r#"{"a":NaN,"b":Infinity,"e":1}"#,
        serde_json::json!({"e": 1}),
    );
    check(
        r#"{"a":"x"} trailing garbage {"b":2}"#,
        serde_json::json!({"a": "x"}),
    );
}

#[test]
fn never_throws_and_keeps_snapshots_plain_json_on_malformed_text() {
    let malformed = [
        r#"{"a":1 "b":2}"#,
        r#"{"a" 1, "b": 2}"#,
        "{a:1, 'b': 2}",
        r#"{"a":1,,"b":2,}"#,
        "[1 2, , 3,]",
        r#"{"a":[1,2}, "b":3]"#,
        "]]]}}}{",
        r#"{"a":NaN,"b":Infinity,"c":-Infinity,"d":undefined,"e":1}"#,
        r#"{"a":01,"b":1.,"c":.5,"d":+1,"e":0x10,"f":1e}"#,
        r#"{"a":"x"} trailing garbage {"b":2}"#,
        r#"{"a": tru, "b": nulll, "c": falsey}"#,
        r#"{"a":{"b":{"c":"#,
        r#"{"a\": 1}"#,
        r#""unterminated \"#,
        "// comment\n{\"a\":1}",
        "\u{0}{\u{1}\"a\"\u{2}:\u{3}1}",
        r#"{"a":"x"}}}]]]"#,
    ];
    for text in malformed {
        for size in [1, 2, 3, 7] {
            let mut parser = StreamingJsonParser::new();
            let mut read = String::new();
            let mut index = 0;
            while index < text.len() {
                let end = (index + size).min(text.len());
                if !text.is_char_boundary(end) {
                    index = end;
                    continue;
                }
                parser.append(&text[index..end]);
                read.push_str(&text[index..end]);
                index = end;
                let snapshot = parser.snapshot();
                if let Some(snapshot) = &snapshot {
                    assert_json_value(snapshot, &read);
                }
                assert_eq!(snapshot, parser.snapshot());
            }
            // 每种切分读出的最终值都相同。
            let last = parser.snapshot();
            let mut whole = StreamingJsonParser::new();
            whole.append(text);
            assert_eq!(last, whole.snapshot());
        }
    }
}

fn assert_json_value(value: &Value, label: &str) {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => {}
        Value::Number(number) => assert!(
            number.as_f64().is_some_and(f64::is_finite),
            "{label}: non-finite number"
        ),
        Value::Array(items) => items.iter().for_each(|item| assert_json_value(item, label)),
        Value::Object(members) => members
            .values()
            .for_each(|item| assert_json_value(item, label)),
    }
}

fn final_snapshot(text: &str, chunk_size: usize) -> Option<Value> {
    let mut parser = StreamingJsonParser::new();
    let size = chunk_size.max(1);
    let mut index = 0;
    while index < text.len() {
        let end = (index + size).min(text.len());
        let end = if text.is_char_boundary(end) {
            end
        } else {
            let mut e = end;
            while e < text.len() && !text.is_char_boundary(e) {
                e += 1;
            }
            e
        };
        parser.append(&text[index..end]);
        index = end;
    }
    parser.snapshot()
}

/// 与 TS 同名的用例：读 200,000 层嵌套不递归，且代价随深度**线性**增长。
///
/// TS 断言 `performance.now() - started < 5_000`（V8 下约 0.8–1.0 秒）。Rust 的 debug 构建
/// 每次快照仍要为每个打开的层分配一个容器（与 TS 的 `slice()`/`{...}` 同阶），常数大一个
/// 数量级，绝对的 5 秒阈值在 debug 上无法达到；真正的契约是「线性」，因此这里断言
/// 10 倍深度的时间增长远低于二次方（< 20 倍），并保留一个宽松的绝对上限挡住病态回归。
/// 这条差异记在交付报告的 deviations 里。
#[test]
fn reads_deep_nesting_without_recursion_and_in_linear_time() {
    fn elapsed_for(depth: usize) -> std::time::Duration {
        let text = format!("{}1{}", "[".repeat(depth), "]".repeat(depth));
        let started = std::time::Instant::now();
        let mut parser = StreamingJsonParser::new();
        let mut index = 0;
        while index < text.len() {
            let end = (index + 7).min(text.len());
            parser.append(&text[index..end]);
            // 用共享句柄而不是 `snapshot()`：`Value` 的递归 `Drop` 是外部类型限制
            // （`kiana_json::js_object` 模块文档），每次丢弃一份深值副本会在测试里爆栈。
            let _ = parser.snapshot_shared();
            index = end;
        }
        let snapshot = parser.snapshot_shared().expect("snapshot");
        // 逐层取第一个元素（借用，不复制：TS 同样只是移动引用）。
        let mut value = &*snapshot;
        for _ in 0..depth {
            value = value
                .as_array()
                .and_then(|a| a.first())
                .expect("array element");
        }
        assert_eq!(value, &serde_json::json!(1));
        // 200k 层的快照无法被递归 `Drop` 安全释放（外部类型限制）；测试进程直接放弃它。
        std::mem::forget(snapshot);
        std::mem::forget(parser);
        started.elapsed()
    }

    let small = elapsed_for(20_000);
    let large = elapsed_for(200_000);
    assert!(
        large.as_secs_f64() < small.as_secs_f64() * 20.0,
        "10x depth took {large:?} vs {small:?}"
    );
    assert!(large.as_secs() < 60, "200k depth took {large:?}");
}

#[test]
fn reads_one_huge_piece_in_linear_time() {
    let content: String = (0..20_000)
        .map(|line| format!("const line{line} = \"value {line}\"; // \\\\ done\n"))
        .collect();
    let text =
        serde_json::json!({"path": "/src/file.ts", "content": &content[..500_000]}).to_string();
    let started = std::time::Instant::now();
    let mut parser = StreamingJsonParser::new();
    parser.append(&text);
    let snapshot = parser.snapshot().expect("snapshot");
    assert!(snapshot.is_object());
    assert!(started.elapsed().as_secs() < 5);
}

#[test]
fn streams_large_arguments_without_per_delta_reparsing() {
    let content: String = (0..20_000)
        .map(|line| format!("const line{line} = \"value {line}\"; // \\\\ done\n"))
        .collect();
    let text =
        serde_json::json!({"path": "/src/file.ts", "content": &content[..500_000]}).to_string();
    let started = std::time::Instant::now();
    let mut args = ToolCallArgumentsStream::new();
    let mut index = 0;
    while index < text.len() {
        let end = (index + 32).min(text.len());
        let mut e = end;
        while e < text.len() && !text.is_char_boundary(e) {
            e += 1;
        }
        args.append(&text[index..e]);
        index = e;
    }
    let finish = args.finish();
    assert_eq!(finish.arguments_error, None);
    assert!(started.elapsed().as_secs() < 10);
}

// ---------------------------------------------------------------------------
// TS `streaming-json.test.ts` 的差分模糊测试（原生移植）
// ---------------------------------------------------------------------------

fn option_json_equal(left: Option<&Value>, right: Option<&Value>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => json_equal(left, right),
        _ => false,
    }
}

/// 按 Unicode 标量值（`char`）切分：切点始终落在字符边界上，不会拆开代理对（见 `support` 模块文档）。
fn chunk_pieces(text: &str, size: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    chars
        .chunks(size)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

/// TS 的 `stream(text, random(read32(text)))`：每片 `1 + floor(rand() * 12)` 个码元。
fn random_pieces(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut rand = Rand::new(f64::from(read32(text)));
    let mut pieces = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let size = 1 + (rand.next() * 12.0).floor() as usize;
        let end = (index + size).min(chars.len());
        pieces.push(chars[index..end].iter().collect());
        index = end;
    }
    pieces
}

fn pieces_for(text: &str, size: Option<usize>) -> Vec<String> {
    match size {
        Some(size) => chunk_pieces(text, size),
        None => random_pieces(text),
    }
}

const CHUNK_SIZES: [Option<usize>; 5] = [Some(1), Some(2), Some(3), Some(7), None];

#[test]
fn differential_fuzz_matches_the_reference_after_every_piece() {
    // TS: "matches the reference after every piece, at every chunk size, with insignificant whitespace"
    for text in random_documents(1, 250, true) {
        for size in CHUNK_SIZES {
            let mut parser = StreamingJsonParser::new();
            let mut read = String::new();
            for piece in pieces_for(&text, size) {
                parser.append(&piece);
                read.push_str(&piece);
                let actual = parser.snapshot();
                let expected = reference_snapshot(&read, true);
                assert!(
                    option_json_equal(actual.as_ref(), expected.as_ref()),
                    "text {text:?} read {read:?} (chunk size {size:?}): got {actual:?}, want {expected:?}"
                );
            }
        }
    }
}

#[test]
fn differential_fuzz_matches_parse_streaming_json_at_every_prefix() {
    // TS: "matches today's parseStreamingJson at every prefix of compact documents"
    for text in random_documents(2, 400, false) {
        let mut parser = StreamingJsonParser::new();
        let mut read = String::new();
        for piece in chunk_pieces(&text, 1) {
            parser.append(&piece);
            read.push_str(&piece);
            // parseStreamingJson 会去掉结尾空白；未完成的根 `null` 显示为 {}。
            if read.chars().last().is_some_and(is_js_space) {
                continue;
            }
            let snapshot = parser.snapshot();
            if matches!(snapshot.as_ref(), Some(Value::Null)) && "null".starts_with(&read) {
                continue;
            }
            let expected = kiana_ai::utils::json_parse::parse_streaming_json(Some(&read));
            let actual = snapshot.unwrap_or_else(|| Value::Object(serde_json::Map::new()));
            assert!(
                json_equal(&actual, &expected),
                "text {text:?} read {read:?}: got {actual:?}, want {expected:?}"
            );
        }
    }
}

#[test]
fn differential_fuzz_reads_complete_documents_exactly_like_json_parse() {
    // TS: "reads complete documents exactly like JSON.parse"（seeds 3/4，random 用固定 5）
    let documents: Vec<String> = random_documents(3, 300, true)
        .into_iter()
        .chain(random_documents(4, 300, false))
        .collect();
    for text in documents {
        let expected: Value =
            kiana_json::js_parse::parse(&text).expect("generated document is valid JSON");
        for size in [1usize, 2, 3, 7, 5] {
            let mut parser = StreamingJsonParser::new();
            for piece in chunk_pieces(&text, size) {
                parser.append(&piece);
            }
            let actual = parser.snapshot().expect("final snapshot");
            assert!(
                json_equal(&actual, &expected),
                "text {text:?} (chunk size {size}): got {actual:?}, want {expected:?}"
            );
        }
    }
}

#[test]
fn differential_fuzz_snapshots_never_change_once_returned() {
    // TS: "never changes a snapshot once it is returned"
    for text in random_documents(5, 150, true) {
        let mut parser = StreamingJsonParser::new();
        let mut snapshots = Vec::new();
        for piece in random_pieces(&text) {
            parser.append(&piece);
            let snapshot = parser.snapshot();
            let copy = snapshot.clone();
            snapshots.push((snapshot, copy));
        }
        for (snapshot, copy) in snapshots {
            assert!(
                option_json_equal(snapshot.as_ref(), copy.as_ref()),
                "text {text:?}: {snapshot:?} != {copy:?}"
            );
        }
    }
}

fn navigate<'a>(mirror: &'a mut Option<Value>, path: &[PathSegment]) -> &'a mut Value {
    let mut current = mirror.as_mut().expect("mirror parent");
    for segment in path {
        current = match segment {
            PathSegment::Key(key) => current
                .as_object_mut()
                .and_then(|members| members.get_mut(key))
                .expect("mirror key"),
            PathSegment::Index(index) => current
                .as_array_mut()
                .and_then(|items| items.get_mut(*index))
                .expect("mirror index"),
        };
    }
    current
}

fn apply_changes(mirror: &mut Option<Value>, changes: &[JsonChange]) {
    for change in changes {
        match change {
            JsonChange::Set { path, value } => {
                if path.is_empty() {
                    *mirror = Some(value.clone());
                    continue;
                }
                let (last, parents) = path.split_last().expect("non-empty path");
                let parent = navigate(mirror, parents);
                match last {
                    PathSegment::Key(key) => {
                        parent
                            .as_object_mut()
                            .expect("mirror object")
                            .insert(key.clone(), value.clone());
                    }
                    PathSegment::Index(index) => {
                        let items = parent.as_array_mut().expect("mirror array");
                        if *index < items.len() {
                            items[*index] = value.clone();
                        } else {
                            items.push(value.clone());
                        }
                    }
                }
            }
            JsonChange::Append { path, text } => {
                if path.is_empty() {
                    let current = mirror.as_mut().expect("mirror root string");
                    let appended = format!("{}{text}", current.as_str().expect("mirror string"));
                    *current = Value::String(appended);
                    continue;
                }
                let (last, parents) = path.split_last().expect("non-empty path");
                let parent = navigate(mirror, parents);
                let target = match last {
                    PathSegment::Key(key) => parent
                        .as_object_mut()
                        .expect("mirror object")
                        .get_mut(key)
                        .expect("mirror string"),
                    PathSegment::Index(index) => parent
                        .as_array_mut()
                        .expect("mirror array")
                        .get_mut(*index)
                        .expect("mirror string"),
                };
                let appended = format!("{}{text}", target.as_str().expect("mirror string"));
                *target = Value::String(appended);
            }
        }
    }
}

#[test]
fn differential_fuzz_changes_keep_a_mirror_equal_to_the_reference() {
    // TS: "keep a copy equal to the value read so far, at every chunk size, and to JSON.parse at the end"
    for text in random_documents(9, 200, true) {
        for size in CHUNK_SIZES {
            let mut parser = StreamingJsonParser::new();
            let mut mirror: Option<Value> = None;
            let mut read = String::new();
            for piece in pieces_for(&text, size) {
                apply_changes(&mut mirror, &parser.append_changes(&piece));
                read.push_str(&piece);
                // 变更流不报告未完成的数字/字面量（快照才显示）。
                let expected = reference_snapshot(&read, false);
                assert!(
                    option_json_equal(mirror.as_ref(), expected.as_ref()),
                    "text {text:?} read {read:?} (chunk size {size:?}): got {mirror:?}, want {expected:?}"
                );
            }
            let expected: Value = kiana_json::js_parse::parse(&text).expect("valid document");
            assert!(
                mirror
                    .as_ref()
                    .is_some_and(|value| json_equal(value, &expected)),
                "text {text:?} (chunk size {size:?}): got {mirror:?}, want {expected:?}"
            );
        }
    }
}
