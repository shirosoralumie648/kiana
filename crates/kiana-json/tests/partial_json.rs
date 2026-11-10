//! describe: "partial-json 0.1.7" —— parseStreamingJson 的第 3/4 级回退
//!
//! fixture 记录 `partial-json@0.1.7 parse(input)`（`Allow.ALL`）的 value / error.class /
//! error.message 与结果对象的自有键。

use kiana_json::js_object::stringify;
use kiana_json::js_parse::partial::errors::PartialErrorKind;
use kiana_json::js_parse::partial::{parse, parse_partial};
use kiana_json::js_parse::repair;
use serde_json::Value;

/// JS 对象的自有键枚举序：直接读返回值的 `Map` 迭代序，**不做**重排。
///
/// 关键：不能先 `sort_js_keys` 再读键——那会把「错误但看起来对」的插入序重新整理成 JS 序，
/// 从而掩盖 `partial-json` 未重排的缺陷。`parseStreamingJson` 在 TS 里返回真实 JS 对象，
/// 其 `Object.keys` 永远按 JS 序枚举。
fn js_key_order(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// 直接用 serde 序列化（不经 `stringify` 的数字/键序重排）时 fixture 里已知的差异类别。
///
/// fixture 的 `value` 列是 TS `JSON.stringify` 的输出，而 JS 的数字格式化与
/// `serde_json::to_string` 不同（`1e+21` vs `1e21`、`1.0` vs `1`）；因此文本仍用
/// `stringify` 比对，但**键序**必须用返回值的真实迭代序独立验证。
fn load(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture readable"))
        .expect("fixture is valid JSON")
}

#[test]
fn partial_json_fixture() {
    let fixture = load("partial_json.json");
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let input = case["input"].as_str().expect("input");
        match parse_partial(input) {
            Ok(value) => {
                if !case["error"].is_null() {
                    failures.push(format!("{input:?}: parsed but fixture expects an error"));
                    continue;
                }
                if let Some(expected) = case["value"].as_str() {
                    let got = stringify(&value);
                    if got != expected {
                        failures.push(format!("{input:?}: got {got}, want {expected}"));
                    }
                }
                if let Some(keys) = case["object_keys"].as_array() {
                    let expected_keys: Vec<&str> = keys.iter().filter_map(|k| k.as_str()).collect();
                    let got_keys = js_key_order(&value);
                    if got_keys != expected_keys {
                        failures.push(format!("{input:?}: keys {got_keys:?} != {expected_keys:?}"));
                    }
                }
            }
            Err(e) => {
                let Some(error) = case["error"].as_object() else {
                    failures.push(format!("{input:?}: unexpected error {}", e.message));
                    continue;
                };
                let expected = error["message"].as_str().expect("message");
                if e.message != expected {
                    failures.push(format!(
                        "{input:?}:\n  got:  {}\n  want: {expected}",
                        e.message
                    ));
                }
                let expected_class = error["class"].as_str().expect("class");
                let got_class = match e.kind {
                    kiana_json::js_parse::partial::errors::PartialErrorKind::Partial => {
                        "PartialJSON"
                    }
                    kiana_json::js_parse::partial::errors::PartialErrorKind::Malformed => {
                        "MalformedJSON"
                    }
                    kiana_json::js_parse::partial::errors::PartialErrorKind::Plain => "Error",
                    // partial-json 的第二种 `JSON.parse` 不在 try 里，抛出的 V8 SyntaxError
                    // 原样逃逸（fixture 里没有这种用例，但语料里存在）。
                    kiana_json::js_parse::partial::errors::PartialErrorKind::Syntax => {
                        "SyntaxError"
                    }
                };
                if got_class != expected_class {
                    failures.push(format!("{input:?}: class {got_class} != {expected_class}"));
                }
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
fn empty_input_message_uses_untrimmed_text() {
    assert_eq!(parse("", 0x1ff).unwrap_err().message, " is empty");
    assert_eq!(parse("   ", 0x1ff).unwrap_err().message, "    is empty");
}

#[test]
fn proto_key_is_dropped_like_a_javascript_prototype_set() {
    // partial-json 用 `obj[key] = value`：`__proto__` 改的是原型，不是自有键，因此被丢弃。
    // 这与 `JSON.parse` 相反（后者保留 `__proto__` 为普通自有键）。
    let value = parse_partial(r#"{"__proto__":{"x":1},"a":2}"#).unwrap();
    assert_eq!(stringify(&value), r#"{"a":2}"#);
    assert_eq!(
        stringify(&kiana_json::js_parse::parse(r#"{"__proto__":{"x":1},"a":2}"#).unwrap()),
        r#"{"__proto__":{"x":1},"a":2}"#
    );
}

#[test]
fn streaming_second_fallback_uses_repaired_text() {
    // `{"a":"\url{x}"}` 严格解析失败、修复后成功 -> 第 2 级（parseJsonWithRepair）就返回。
    let (value, from_strict) = kiana_json::js_parse::parse_streaming(r#"{"a":"\url{x}"}"#);
    assert!(from_strict);
    assert_eq!(stringify(&value), r#"{"a":"\\url{x}"}"#);
    // repair 后仍失败时才会进入 partial-json；确认回退路径不会 panic。
    let (value, from_strict) = kiana_json::js_parse::parse_streaming(r#"{"a":1 "b":2}"#);
    assert!(!from_strict);
    assert_eq!(stringify(&value), "{}");
    // `repair` 本身对这段文本不改变内容。
    assert_eq!(
        repair(r#"{"a":1 "b":2}"#).as_deref(),
        Some(r#"{"a":1 "b":2}"#)
    );
}

#[test]
fn second_string_fallback_error_escapes_as_a_v8_syntax_error() {
    // `parseStr` 的第二次 `JSON.parse` 不在 try 里，因此它抛出的 V8 SyntaxError
    // 原样逃逸：`SyntaxError.constructor.name === "SyntaxError"`，消息**不带**
    // ` at position N` 后缀。`"a<TAB>b` 未闭合且含裸控制字符，两次补引号都救不回来。
    let err = parse_partial("\"a\tb").unwrap_err();
    assert_eq!(err.kind, PartialErrorKind::Syntax);
    assert_eq!(
        err.message,
        "Unterminated string in JSON at position 1 (line 1 column 2)"
    );
    // 补引号成功的例子（同样未闭合但没有控制字符）不会走到这里。
    assert_eq!(stringify(&parse_partial("\"abc").unwrap()), "\"abc\"");
}

#[test]
fn top_level_number_errors_use_the_v8_message() {
    // `index == 0` 的 parseNum 走 `JSON.parse(整串)`，失败时 `throwMalformedError(String(e))`：
    // 消息带 `SyntaxError: ` 前缀，位置是 V8 的 UTF-16 偏移，末尾再追加 ` at position 0`。
    let err = parse_partial("x").unwrap_err();
    assert_eq!(err.kind, PartialErrorKind::Malformed);
    assert_eq!(
        err.message,
        "SyntaxError: Unexpected token 'x', \"x\" is not valid JSON at position 0"
    );
    let err = parse_partial("-a").unwrap_err();
    assert_eq!(
        err.message,
        "SyntaxError: No number after minus sign in JSON at position 1 (line 1 column 2) at position 0"
    );
}

#[test]
fn prototype_key_drop_only_applies_to_own_property_writes() {
    // 回归：`remaining_is_prefix_of` 曾把 UTF-16 code unit 截断成 Latin-1，
    // 使 `Ů` 之类被当作 `null` 的前缀而误判成 null。
    let err = parse_partial("\u{16e}").unwrap_err();
    assert_eq!(err.kind, PartialErrorKind::Malformed);
}

// ---------------------------------------------------------------------------
// 回归：partial-json 结果是普通 JS 对象，枚举序必须按 JS 序（每一层）
// ---------------------------------------------------------------------------

#[test]
fn partial_results_enumerate_in_javascript_key_order() {
    // TS: `Object.keys(partialParse('{"b":1,"2":2}'))` -> `["2","b"]`。
    // 这里的断言直接读 `Map` 的迭代序（不经过 `sort_js_keys`/`stringify`），
    // 否则会在序列化时被重新排序，测试看不见缺陷。
    for (input, expected) in [
        (r#"{"b":1,"2":2}"#, vec!["2", "b"]),
        (r#"{"b":1,"2":2"#, vec!["2", "b"]),
        (r#"{"10":1,"9":2,"a":3}"#, vec!["9", "10", "a"]),
        (r#"{"b":1,"2":2,"a":3,"1":4}"#, vec!["1", "2", "b", "a"]),
        (
            r#"{"01":1,"1":2,"4294967295":3,"4294967294":4}"#,
            vec!["1", "4294967294", "01", "4294967295"],
        ),
    ] {
        let value = parse_partial(input).unwrap();
        assert_eq!(js_key_order(&value), expected, "input {input:?}");
    }

    // 每一层都要重排：嵌套对象与数组里的对象。
    let value = parse_partial(r#"{"b":{"z":1,"10":2},"2":[{"y":1,"0":2}]}"#).unwrap();
    assert_eq!(js_key_order(&value), vec!["2", "b"]);
    assert_eq!(js_key_order(&value["b"]), vec!["10", "z"]);
    assert_eq!(js_key_order(&value["2"][0]), vec!["0", "y"]);
    // TS 的值：`{"2":[{"0":2,"y":1}],"b":{"10":2,"z":1}}`。
    assert_eq!(
        stringify(&value),
        r#"{"2":[{"0":2,"y":1}],"b":{"10":2,"z":1}}"#
    );

    // 重复键：`obj[key] = value` 的值取最后一个，位置由 JS 的索引键规则决定。
    let value = parse_partial(r#"{"a":1,"2":2,"a":3,"1":4}"#).unwrap();
    assert_eq!(js_key_order(&value), vec!["1", "2", "a"]);
    assert_eq!(value["a"], serde_json::json!(3));
}

// ---------------------------------------------------------------------------
// 验收：`Allow` 位掩码的公开面（`partial_allow_masks_golden` 用 fixture 覆盖行为）
// ---------------------------------------------------------------------------

#[test]
fn allow_masks_and_tagged_parsing_are_public() {
    use kiana_json::js_parse::partial::{ALLOW_ALL, PartialKind, allow, parse, parse_tagged};

    // 每个位都等于 `partial-json@0.1.7` 的 `options.js`。
    assert_eq!(allow::STR, 1);
    assert_eq!(allow::NUM, 2);
    assert_eq!(allow::ARR, 4);
    assert_eq!(allow::OBJ, 8);
    assert_eq!(allow::NULL, 16);
    assert_eq!(allow::BOOL, 32);
    assert_eq!(allow::NAN, 64);
    assert_eq!(allow::INFINITY, 128);
    assert_eq!(allow::_INFINITY, 256);
    assert_eq!(ALLOW_ALL, 511);

    // 关掉 STR 后，未终止字符串按文档示例被丢弃。
    assert_eq!(
        stringify(&parse(r#"["complete string", "incompl"#, ALLOW_ALL).unwrap()),
        r#"["complete string","incompl"]"#
    );
    assert_eq!(
        stringify(&parse(r#"["complete string", "incompl"#, ALLOW_ALL & !allow::STR).unwrap()),
        r#"["complete string"]"#
    );
    // 只有 ALL 同时允许 OBJ 与 ARR 时才返回部分嵌套对象。
    assert_eq!(
        stringify(&parse(r#"[{"a": 1, "b": 2}, {"a": 3,"#, allow::ARR).unwrap()),
        r#"[{"a":1,"b":2}]"#
    );
    assert_eq!(
        parse(r#"[{"a": 1, "b": 2}, {"a": 3,"#, allow::OBJ)
            .unwrap_err()
            .message,
        "Expected ']' at end of array at position 27"
    );

    // `parse_tagged`：非有限数在 Rust 侧落为 Null，但 JS 里是 number（`?? {}` 不替换它）。
    let (value, kind) = parse_tagged("NaN", ALLOW_ALL).unwrap();
    assert!(value.is_null());
    assert_eq!(kind, PartialKind::NonFiniteNumber);
    let (value, kind) = parse_tagged("nul", ALLOW_ALL).unwrap();
    assert!(value.is_null());
    assert_eq!(kind, PartialKind::Value, "null 字面量不是非有限数");
    // 内层的非有限数不改变顶层种类（`[Infinity]` 是数组）。
    let (_, kind) = parse_tagged("[Infinity]", ALLOW_ALL).unwrap();
    assert_eq!(kind, PartialKind::Value);
}
