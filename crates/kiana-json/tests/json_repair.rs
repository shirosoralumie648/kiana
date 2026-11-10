//! describe: "repairJson" / "parseJsonWithRepair"
//!
//! 覆盖修复规则与「抛修复后那次错误」的分支这两类测试意图。

use kiana_json::js_object::stringify;
use kiana_json::js_parse::{parse, parse_with_repair, repair};
use serde_json::Value;

fn load(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture readable"))
        .expect("fixture is valid JSON")
}

#[test]
fn repair_json_fixture() {
    let fixture = load("repair.json");
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let input = case["input"].as_str().expect("input");
        let expected = case["expected"].as_str().expect("expected");
        match repair(input) {
            Some(got) if got == expected => {}
            Some(got) => failures.push(format!("{input:?}: got {got:?}, want {expected:?}")),
            None => failures.push(format!("{input:?}: got None, want {expected:?}")),
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
fn parse_with_repair_fixture() {
    let fixture = load("parse_with_repair.json");
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let input = case["input"].as_str().expect("input");
        if case["ok"].as_bool().expect("ok") {
            let expected = case["value_fffd"]
                .as_str()
                .or_else(|| case["value"].as_str())
                .expect("value");
            match parse_with_repair(input) {
                Ok(value) => {
                    let got = stringify(&value);
                    if got != expected {
                        failures.push(format!("{input:?}: got {got}, want {expected}"));
                    }
                }
                Err(e) => failures.push(format!("{input:?}: failed: {}", e.message)),
            }
        } else {
            let expected = case["message"].as_str().expect("message");
            match parse_with_repair(input) {
                Ok(v) => failures.push(format!("{input:?}: parsed to {v}")),
                Err(e) => {
                    if e.message != expected {
                        failures.push(format!(
                            "{input:?}:\n  got:  {}\n  want: {expected}",
                            e.message
                        ));
                    }
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
fn failed_repair_reports_the_repaired_error_not_the_original() {
    // `{"a":"x"` 修复后仍是 `{"a":"x"`（没有可改的），因此抛最初的错误。
    let original = parse("{\"a\":\"x\"").unwrap_err();
    let with_repair = parse_with_repair("{\"a\":\"x\"").unwrap_err();
    assert_eq!(with_repair.message, original.message);
    assert_eq!(
        with_repair.message,
        "Expected ',' or '}' after property value in JSON at position 8 (line 1 column 9)"
    );
}

#[test]
fn repair_only_escapes_inside_strings() {
    // 字符串外一律原样（不补括号/引号/逗号）。
    assert_eq!(repair("{\"a\":1,}").as_deref(), Some("{\"a\":1,}"));
    assert_eq!(repair("plain").as_deref(), Some("plain"));
    // 串内控制字符转义。
    assert_eq!(
        repair("{\"a\":\"x\ny\"}").as_deref(),
        Some("{\"a\":\"x\\ny\"}")
    );
    // 串内非法转义双写反斜杠，且不消费后继字符。
    assert_eq!(
        repair("{\"a\":\"\\url{x}\"}").as_deref(),
        Some("{\"a\":\"\\\\url{x}\"}")
    );
    // 合法 \uXXXX 原样保留。
    assert_eq!(
        repair("{\"a\":\"\\u1234\"}").as_deref(),
        Some("{\"a\":\"\\u1234\"}")
    );
    // 不完整的 \u 被双写（4 位非全 hex）。
    assert_eq!(
        repair("{\"a\":\"\\u12\"}").as_deref(),
        Some("{\"a\":\"\\\\u12\"}")
    );
    // 末尾反斜杠双写。
    assert_eq!(repair("\"a\\").as_deref(), Some("\"a\\\\"));
}

// ---------------------------------------------------------------------------
// 回归：极小的负指数不能让 `parse` panic / 得到 Infinity
// ---------------------------------------------------------------------------

#[test]
fn huge_negative_exponent_underflows_to_zero() {
    // JS: JSON.parse("1.55e-9223372036854775807") === 0。
    // 骨架的 `exp10 - frac_digits.len()` 在 debug 下 panic、release 下回绕成 Infinity。
    for input in [
        "1.55e-9223372036854775807",
        "1.555e-9223372036854775807",
        "1.5555e-9223372036854775807",
        "123.456e-9223372036854775807",
        "-1.55e-9223372036854775807",
    ] {
        let value = parse(input).expect("literal parses");
        assert_eq!(stringify(&value), "0", "input {input:?}");
    }
    // 走流式入口同样不能变成 Infinity（`{}`）。
    assert_eq!(
        stringify(&kiana_json::js_parse::parse_streaming("1.55e-9223372036854775807").0),
        "0"
    );
    // 正指数仍然溢出成 Infinity（Rust 侧为 null）。
    assert_eq!(
        stringify(&parse("1.55e9223372036854775807").unwrap()),
        "null"
    );
}
