//! describe: "parseStreamingJson" —— 三级回退
//!
//! TS 侧同名测试（`packages/ai/test/json-parse.test.ts` 的 `describe("parseStreamingJson")`）
//! 的 3 个 `it` 逐字保留；其余用例来自 golden fixture。

use kiana_json::js_object::stringify;
use kiana_json::js_parse::parse_streaming;
use serde_json::Value;

/// JS 对象的自有键枚举序：直接读返回值的 `Map` 迭代序，**不做**重排。
///
/// 不能先 `sort_js_keys` 再读键——那会把插入序重新整理成 JS 序，掩盖 partial 回退层
/// 未重排的缺陷。`parseStreamingJson` 在 TS 里返回真实 JS 对象，`Object.keys` 永远按 JS 序。
fn js_key_order(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

fn load(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture readable"))
        .expect("fixture is valid JSON")
}

#[test]
fn keeps_invalid_u_escapes_as_literal_text_without_dropping_fields() {
    // #10868
    assert_eq!(
        parse_streaming(r#"{"text":"\url{x}"}"#).0,
        kiana_json::js_parse::parse(r#"{"text":"\\url{x}"}"#).unwrap()
    );
    assert_eq!(
        parse_streaming(r#"{"path":"a.tex","content":"\usepackage{x}","mode":"w"}"#).0,
        kiana_json::js_parse::parse(r#"{"path":"a.tex","content":"\\usepackage{x}","mode":"w"}"#)
            .unwrap()
    );
}

#[test]
fn decodes_valid_u_escapes() {
    assert_eq!(
        parse_streaming(r#"{"text":"caf\u00e9 \url"}"#).0,
        kiana_json::js_parse::parse(r#"{"text":"café \\url"}"#).unwrap()
    );
}

#[test]
fn omits_a_u_escape_that_is_cut_off_mid_stream() {
    assert_eq!(
        parse_streaming(r#"{"text":"ab\u00"#).0,
        kiana_json::js_parse::parse(r#"{"text":"ab"}"#).unwrap()
    );
}

#[test]
fn parse_streaming_fixture() {
    let fixture = load("parse_streaming.json");
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let input = case["input"].as_str().expect("input");
        let (value, _complete) = parse_streaming(input);
        let got = stringify(&value);
        let expected = case["value"].as_str();
        // 非有限数在 Rust 侧落为 null（见模块文档）。
        if let Some(expected) = expected
            && got != expected
        {
            failures.push(format!("{input:?}: got {got}, want {expected}"));
        }
        // 形状断言：is_array / is_null / object_keys。
        let is_array = value.is_array();
        if case["is_array"].as_bool().unwrap_or(false) != is_array {
            failures.push(format!("{input:?}: is_array {is_array}"));
        }
        // 非有限数在 Rust 侧只能落为 null，fixture 用 non_finite_number 标记，is_null
        // 断言对它们不适用（JS 那边 is_null 是 false，因为它是个 number）。
        let is_null = value.is_null();
        if !case["non_finite_number"].as_bool().unwrap_or(false)
            && case["is_null"].as_bool().unwrap_or(false) != is_null
        {
            failures.push(format!("{input:?}: is_null {is_null}"));
        }
        if let Some(keys) = case["object_keys"].as_array() {
            let expected_keys: Vec<&str> = keys.iter().filter_map(|k| k.as_str()).collect();
            let got_keys = js_key_order(&value);
            if got_keys != expected_keys {
                failures.push(format!("{input:?}: keys {got_keys:?} != {expected_keys:?}"));
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
fn empty_and_whitespace_only_inputs_are_empty_objects() {
    assert_eq!(parse_streaming("").0, Value::Object(Default::default()));
    assert_eq!(parse_streaming("   ").0, Value::Object(Default::default()));
    // 非 JSON 空白（ECMAScript WhiteSpace）同样短路。
    assert_eq!(
        parse_streaming("\u{feff}").0,
        Value::Object(Default::default())
    );
}

#[test]
fn scalar_results_are_returned_as_is() {
    // `result ?? {}` 只在两个 partial 回退分支里做，因此 "null" 返回 null。
    assert_eq!(parse_streaming("null").0, Value::Null);
    assert_eq!(parse_streaming("true").0, Value::Bool(true));
    assert_eq!(stringify(&parse_streaming("123").0), "123");
    assert_eq!(parse_streaming("\"abc\"").0, Value::String("abc".into()));
}

#[test]
fn malformed_nested_object_does_not_leak_the_remainder() {
    // partial-json 的 `parseObj` 提前返回时**不**跳过收尾的 `}`（`index++` 在 `try` 之后），
    // 游标停在原处，外层容器因此解析不出残渣：只有已收集的部分对象。
    for (input, expected) in [
        (r#"{"a":1,"b":[1,2,{c":3}]}"#, r#"{"a":1,"b":[1,2,{}]}"#),
        (r#"{"a":{u"b":{"c":1}}}"#, r#"{"a":{}}"#),
        (r#"[{u"k":"v"}]"#, "[{}]"),
        (r#"[{k":"v"}]"#, "[{}]"),
        (r#"[{"k":}"v"}]"#, "[{}]"),
    ] {
        let (value, complete) = parse_streaming(input);
        assert!(
            !complete,
            "input {input:?} should not be a complete strict parse"
        );
        assert_eq!(stringify(&value), expected, "input {input:?}");
    }
}

// ---------------------------------------------------------------------------
// 回归：partial-json 的 `lastIndexOf("e")` 重试同样要记录溢出
// ---------------------------------------------------------------------------

#[test]
fn numeric_token_that_only_parses_after_stripping_e_is_infinity() {
    // TS: parseStreamingJson("1e400e") -> Infinity（JSON 文本 `null`）。
    // 走的是 parseNum 的 index==0 分支：整串 JSON.parse 失败 -> 去掉最后一个 `e` 重试成功。
    // 该重试不经过 note_overflow 时，Rust 会把非有限数误当成 JS 的 null 换成 `{}`。
    for (input, expected) in [
        ("1e400e", "null"),
        ("-1e400e", "null"),
        ("1e999e", "null"),
        ("1e400e5", "null"),
        ("1e400e-", "null"),
        ("  1e400e  ", "null"),
    ] {
        let (value, from_strict) = parse_streaming(input);
        assert!(
            !from_strict,
            "input {input:?} comes from the partial fallback"
        );
        assert_eq!(stringify(&value), expected, "input {input:?}");
    }
    // 对照：普通溢出（第 2 步严格解析就成功）保持 `null`。
    assert_eq!(stringify(&parse_streaming("1e400").0), "null");
    assert!(parse_streaming("1e400").1);
}

#[test]
fn empty_input_is_not_reported_as_a_complete_parse() {
    // 文档契约：bool 的 `true` 只表示结果来自严格/修复解析；空输入的 `{}` 是函数自己造的。
    assert!(!parse_streaming("").1);
    assert!(!parse_streaming("   ").1);
    assert!(!parse_streaming("\u{feff}").1);
    // 真正解析出来的值才是 true。
    assert!(parse_streaming("{}").1);
    assert!(parse_streaming("null").1);
    assert!(parse_streaming(r#"{"a":"\url"}"#).1); // 修复后成功 -> 第 2 步
    // 走 partial-json 回退（第 3/4 步）的都是 false，即便是合法的部分值。
    assert!(!parse_streaming(r#"{"a":1"#).1);
}

#[test]
fn numeric_retry_keeps_finite_values() {
    // 同一分支的有限数情形（Node 26 黄金值）：去掉最后一个 `e` 后仍是普通数字。
    for (input, expected) in [("12e0e", "12"), ("1e5e", "100000")] {
        let (value, from_strict) = parse_streaming(input);
        assert!(!from_strict, "input {input:?}");
        assert_eq!(stringify(&value), expected, "input {input:?}");
    }
}

// ---------------------------------------------------------------------------
// 回归：parseStreamingJson 的三级回退返回一致键序（TS 里永远是真实 JS 对象）
// ---------------------------------------------------------------------------

#[test]
fn streaming_results_enumerate_in_javascript_key_order() {
    // 严格解析层已经重排；partial 回退层同样必须重排，否则同一函数按输入走不同层时
    // 返回不同枚举序（TS 永远按 JS 序）。
    for (input, expected_keys, expected_json) in [
        (r#"{"b":1,"2":2}"#, vec!["2", "b"], r#"{"2":2,"b":1}"#),
        (r#"{"b":1,"2":2"#, vec!["2", "b"], r#"{"2":2,"b":1}"#),
        (
            r#"{"b":{"z":1,"10":2},"2":[{"y":1,"0":2}]}"#,
            vec!["2", "b"],
            r#"{"2":[{"0":2,"y":1}],"b":{"10":2,"z":1}}"#,
        ),
        (
            r#"{"b":{"z":1,"10":2},"2":[{"y":1,"0":2}]"#,
            vec!["2", "b"],
            r#"{"2":[{"0":2,"y":1}],"b":{"10":2,"z":1}}"#,
        ),
    ] {
        let (value, _from_strict) = parse_streaming(input);
        assert_eq!(js_key_order(&value), expected_keys, "input {input:?}");
        assert_eq!(stringify(&value), expected_json, "input {input:?}");
        // 层内嵌套对象的真实迭代序也必须正确（`stringify` 会重排，读键才能验）。
        if let Some(inner) = value.get("b").and_then(|v| v.as_object()) {
            let keys: Vec<&String> = inner.keys().collect();
            assert_eq!(keys, vec!["10", "z"], "input {input:?}");
        }
    }
}
