//! `JSON.stringify` / `JSON.parse` 桥接对拍：`serialize_json_line` 的数字/转义/键序，
//! 以及 RPC 解析失败响应用的 V8 `SyntaxError.message`。
//!
//! 黄金数据 `tests/fixtures/jsonl/serialize_cases.json` 由 Node v26.12.1 的
//! `serializeJsonLine` 生成（spec §4.2/§4.3）。

use kiana_json::js_parse::parse;
use kiana_rpc_print::jsonl::serialize_json_line;
use serde_json::{Number, Value, json};

const SERIALIZE_FIXTURE: &str = include_str!("fixtures/jsonl/serialize_cases.json");

/// `serializeJsonLine` 的数字格式与 Node 的 `JSON.stringify` 逐字节一致。
#[test]
fn serialize_json_line_matches_node_numbers_and_escapes() {
    assert_eq!(serialize_json_line(&json!(1e21)), "1e+21\n");
    assert_eq!(serialize_json_line(&json!(1e20)), "100000000000000000000\n");
    assert_eq!(serialize_json_line(&json!(5e-7)), "5e-7\n");
    assert_eq!(
        serialize_json_line(&json!(0.0030000000000000005)),
        "0.0030000000000000005\n"
    );
    assert_eq!(serialize_json_line(&json!(0.000001)), "0.000001\n");
    assert_eq!(
        serialize_json_line(&json!(123456789012345680000.0)),
        "123456789012345680000\n"
    );

    // -0 -> 0（`Number::from_f64` 保留符号位，写出时归一）。
    let minus_zero = Value::Number(Number::from_f64(-0.0).expect("finite"));
    assert_eq!(serialize_json_line(&minus_zero), "0\n");

    // U+2028 / U+2029 原样输出，不转义。
    let line = serialize_json_line(&json!({"text": "a\u{2028}b\u{2029}c"}));
    assert_eq!(line, "{\"text\":\"a\u{2028}b\u{2029}c\"}\n");

    // 字符串转义：只转义引号、反斜杠与控制字符；U+007F/U+0080/U+00A0/U+FEFF 原样。
    assert_eq!(
        serialize_json_line(&json!("a\"b\\c\n\t\u{0000}\u{001f}")),
        "\"a\\\"b\\\\c\\n\\t\\u0000\\u001f\"\n"
    );
    assert_eq!(
        serialize_json_line(&json!("\u{7f}\u{80}\u{a0}\u{feff}\u{2028}")),
        "\"\u{7f}\u{80}\u{a0}\u{feff}\u{2028}\"\n"
    );
}

/// 黄金数据：解析 fixture 里的 JSON 文本后再序列化，与 Node 的同一输入输出一致
/// （覆盖整数键序、嵌套数字、emoji 与控制字符）。
#[test]
fn serialize_json_line_matches_node_golden_cases() {
    let fixture: Value = serde_json::from_str(SERIALIZE_FIXTURE).expect("fixture JSON");
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let input = case["input_json"].as_str().expect("input_json");
        let expected = case["line"].as_str().expect("line");
        match parse(input) {
            Ok(value) => {
                let line = serialize_json_line(&value);
                if line != expected {
                    failures.push(format!("{input:?}: got {line:?}, want {expected:?}"));
                }
            }
            Err(error) => failures.push(format!("{input:?}: parse failed: {}", error.message)),
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// fixture 的 special 表：Rust 只能表达 `-0` / `null`；NaN/±Infinity 由
/// `format_js_number` 直接验证（`Value` 无法承载非有限数）。
#[test]
fn serialize_json_line_matches_node_special_values() {
    let fixture: Value = serde_json::from_str(SERIALIZE_FIXTURE).expect("fixture JSON");
    let special = fixture["special"].as_array().expect("special");
    let line_for = |name: &str| {
        special
            .iter()
            .find(|case| case["name"] == name)
            .and_then(|case| case["line"].as_str())
            .unwrap_or_else(|| panic!("missing special case {name}"))
            .to_string()
    };

    assert_eq!(
        serialize_json_line(&Value::Number(Number::from_f64(-0.0).expect("finite"))),
        line_for("minus_zero")
    );
    assert_eq!(serialize_json_line(&Value::Null), line_for("null"));
    assert_eq!(kiana_json::js_num::format_js_number(f64::NAN), "null");
    assert_eq!(kiana_json::js_num::format_js_number(f64::INFINITY), "null");
    assert_eq!(
        kiana_json::js_num::format_js_number(f64::NEG_INFINITY),
        "null"
    );
}

/// spec §4.3 的 V8 `JSON.parse` 错误消息表（RPC 的 `Failed to parse command: <message>` 用它）。
#[test]
fn js_parse_messages_match_v8() {
    let table: &[(&str, &str)] = &[
        ("", "Unexpected end of JSON input"),
        ("  ", "Unexpected end of JSON input"),
        ("\t", "Unexpected end of JSON input"),
        (
            "{",
            "Expected property name or '}' in JSON at position 1 (line 1 column 2)",
        ),
        ("}", "Unexpected token '}', \"}\" is not valid JSON"),
        (
            "{\"a\"}",
            "Expected ':' after property name in JSON at position 4 (line 1 column 5)",
        ),
        (
            "{\"a\":}",
            "Unexpected token '}', \"{\"a\":}\" is not valid JSON",
        ),
        (
            "{\"a\":1,}",
            "Expected double-quoted property name in JSON at position 7 (line 1 column 8)",
        ),
        ("[1,]", "Unexpected token ']', \"[1,]\" is not valid JSON"),
        ("tru", "Unexpected end of JSON input"),
        ("nul", "Unexpected end of JSON input"),
        ("NaN", "\"NaN\" is not valid JSON"),
        ("undefined", "\"undefined\" is not valid JSON"),
        (
            "01",
            "Unexpected number in JSON at position 1 (line 1 column 2)",
        ),
        (
            "1.",
            "Unterminated fractional number in JSON at position 2 (line 1 column 3)",
        ),
        (".5", "Unexpected token '.', \".5\" is not valid JSON"),
        (
            "1e",
            "Exponent part is missing a number in JSON at position 2 (line 1 column 3)",
        ),
        (
            "\"a",
            "Unterminated string in JSON at position 2 (line 1 column 3)",
        ),
        (
            "\"a\\q\"",
            "Bad escaped character in JSON at position 3 (line 1 column 4)",
        ),
        (
            "{a:1}",
            "Expected property name or '}' in JSON at position 1 (line 1 column 2)",
        ),
        (
            "{\"a\" 1}",
            "Expected ':' after property name in JSON at position 5 (line 1 column 6)",
        ),
        (
            "{} x",
            "Unexpected non-whitespace character after JSON at position 3 (line 1 column 4)",
        ),
        (
            "{} {}",
            "Unexpected non-whitespace character after JSON at position 3 (line 1 column 4)",
        ),
        (
            "123abc",
            "Unexpected non-whitespace character after JSON at position 3 (line 1 column 4)",
        ),
        (
            "-",
            "No number after minus sign in JSON at position 1 (line 1 column 2)",
        ),
        (
            "-x",
            "No number after minus sign in JSON at position 1 (line 1 column 2)",
        ),
        ("+1", "Unexpected token '+', \"+1\" is not valid JSON"),
        (
            "[1 2]",
            "Expected ',' or ']' after array element in JSON at position 3 (line 1 column 4)",
        ),
        (
            "{,}",
            "Expected property name or '}' in JSON at position 1 (line 1 column 2)",
        ),
        (
            "\"\\u12\"",
            "Bad Unicode escape in JSON at position 5 (line 1 column 6)",
        ),
    ];

    let mut failures = Vec::new();
    for (input, expected) in table {
        match parse(input) {
            Ok(value) => failures.push(format!("{input:?}: unexpectedly parsed: {value}")),
            Err(error) if error.message == *expected => {}
            Err(error) => failures.push(format!(
                "{input:?}:\n  got:  {}\n  want: {expected}",
                error.message
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );

    // RPC 的解析失败响应文本：`Failed to parse command: <message>`。
    let error = parse("").expect_err("empty input fails");
    assert_eq!(
        format!("Failed to parse command: {}", error.message),
        "Failed to parse command: Unexpected end of JSON input"
    );

    // `1e400` 解析成功（V8 得到 Infinity；Rust 侧按 `JSON.stringify(Infinity)` 记为 null）。
    let value = parse("1e400").expect("1e400 is accepted");
    assert_eq!(serialize_json_line(&value), "null\n");
    // 尾随空白允许。
    assert!(parse("{\"a\":1}\n").is_ok());
}
