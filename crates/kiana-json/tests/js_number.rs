//! describe: "js_num" —— ECMAScript Number::toString / radix / Number(string)

use kiana_json::js_num::{format_js_number, is_js_integer, parse_js_number, to_radix_string};
use serde_json::Value;

fn load(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture readable"))
        .expect("fixture is valid JSON")
}

#[test]
fn number_format_fixture() {
    let fixture = load("number_format.json");
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let literal = case["literal"].as_str().expect("literal");
        let parsed: f64 = literal
            .parse()
            .unwrap_or_else(|_| panic!("literal {literal}"));
        let expect_stringify = case["stringify"].as_str().expect("stringify");
        let expect_string = case["string"].as_str().expect("string");
        let is_neg_zero = case["is_neg_zero"].as_bool().expect("is_neg_zero");
        // `stringify` 列是 JSON.stringify（非有限数 -> null）；
        // `string` 列是 String(v)（非有限数 -> "NaN"/"Infinity"/"-Infinity"）。
        let got = format_js_number(parsed);
        if got != expect_stringify {
            failures.push(format!(
                "{literal}: JSON.stringify got {got}, want {expect_stringify}"
            ));
        }
        let got_string = if parsed.is_nan() {
            "NaN".to_string()
        } else if parsed == f64::INFINITY {
            "Infinity".to_string()
        } else if parsed == f64::NEG_INFINITY {
            "-Infinity".to_string()
        } else {
            got.clone()
        };
        if got_string != expect_string {
            failures.push(format!(
                "{literal}: String() got {got_string}, want {expect_string}"
            ));
        }
        if parsed == 0.0 && (parsed.is_sign_negative() != is_neg_zero) {
            failures.push(format!("{literal}: negative-zero sign mismatch"));
        }
        if let Some(bits) = case["bits"].as_str() {
            let want = u64::from_str_radix(bits, 16).expect("bits");
            if parsed.to_bits() != want {
                failures.push(format!(
                    "{literal}: bits {:016x} != {want:016x}",
                    parsed.to_bits()
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
fn random_doubles_fixture() {
    let fixture = load("random_doubles.json");
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let bits = u64::from_str_radix(case["bits"].as_str().expect("bits"), 16).expect("hex");
        let value = f64::from_bits(bits);
        let expected = case["string"].as_str().expect("string");
        let got = format_js_number(value);
        if got != expected {
            failures.push(format!("{bits:016x}: got {got}, want {expected}"));
        }
        if let Some(expect_stringify) = case["stringify"].as_str()
            && got != expect_stringify
        {
            failures.push(format!(
                "{bits:016x}: stringify got {got}, want {expect_stringify}"
            ));
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
fn radix_fixture() {
    let fixture = load("radix.json");
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let literal = case["value"].as_str().expect("value");
        let radix = case["radix"].as_u64().expect("radix") as u32;
        let expected = case["expected"].as_str().expect("expected");
        let value: f64 = literal
            .parse()
            .unwrap_or_else(|_| panic!("literal {literal}"));
        let got = to_radix_string(value, radix);
        if got != expected {
            failures.push(format!(
                "{literal} radix {radix}: got {got}, want {expected}"
            ));
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
fn number_coercion_fixture() {
    let fixture = load("number_coercion.json");
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let input = case["input"].as_str().expect("input");
        // number 列是 String(Number(input))；"-0" 是负零的记号。
        let expected = case["number"].as_str().expect("number");
        let got_value = parse_js_number(input);
        let got = if got_value == 0.0 && got_value.is_sign_negative() {
            "-0".to_string()
        } else if got_value.is_nan() {
            "NaN".to_string()
        } else if got_value == f64::INFINITY {
            "Infinity".to_string()
        } else if got_value == f64::NEG_INFINITY {
            "-Infinity".to_string()
        } else if got_value == 0.0 {
            "0".to_string()
        } else {
            format_js_number(got_value)
        };
        if got != expected {
            failures.push(format!("{input:?}: got {got}, want {expected}"));
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
fn radix_edge_cases() {
    assert_eq!(to_radix_string(f64::NAN, 2), "NaN");
    assert_eq!(to_radix_string(f64::INFINITY, 36), "Infinity");
    assert_eq!(to_radix_string(f64::NEG_INFINITY, 36), "-Infinity");
    assert_eq!(to_radix_string(-0.0, 2), "0");
    // radix 10 走 Number::toString（含指数形式）。
    assert_eq!(to_radix_string(1e21, 10), "1e+21");
}

#[test]
#[should_panic(expected = "radix out of range")]
fn radix_out_of_range_panics() {
    // JS 抛 RangeError；签名返回 String，因此 panic。
    let _ = to_radix_string(1.0, 1);
}

#[test]
fn is_js_integer_matches_number_is_integer() {
    assert!(is_js_integer(1.0));
    assert!(is_js_integer(-0.0));
    assert!(is_js_integer(1e21));
    assert!(!is_js_integer(1.5));
    assert!(!is_js_integer(f64::NAN));
    assert!(!is_js_integer(f64::INFINITY));
}

// ---------------------------------------------------------------------------
// 回归：`to_radix_string` 的非有限值（radix 10 曾先走到 `format_js_number` 的 JSON 语境）
// ---------------------------------------------------------------------------

#[test]
fn radix_non_finite_uses_javascript_words() {
    // `(NaN).toString(10)` 是 "NaN"，`Infinity.toString(10)` 是 "Infinity"——不是 JSON 的 "null"。
    for radix in [2u32, 10, 16, 36] {
        assert_eq!(to_radix_string(f64::NAN, radix), "NaN", "radix {radix}");
        assert_eq!(
            to_radix_string(f64::INFINITY, radix),
            "Infinity",
            "radix {radix}"
        );
        assert_eq!(
            to_radix_string(f64::NEG_INFINITY, radix),
            "-Infinity",
            "radix {radix}"
        );
        assert_eq!(to_radix_string(-0.0, radix), "0", "radix {radix}");
    }
}

// ---------------------------------------------------------------------------
// 回归：`Number("0x…")` 必须整串精确累加后只舍入一次
// ---------------------------------------------------------------------------

#[test]
fn prefixed_literals_round_exactly_once() {
    // Node 26: Number("0xc6C03A14c6c8a41691E9CCEaeec18c") 的 bit 模式。
    assert_eq!(
        parse_js_number("0xc6C03A14c6c8a41691E9CCEaeec18c").to_bits(),
        0x4768_d807_4298_d915
    );
    // f64 累加会在这两个 53 位边界上差 1 ulp。
    assert_eq!(
        parse_js_number("0xffffffffffffffffffffffff").to_bits(),
        0x45f0_0000_0000_0000
    );
    assert_eq!(parse_js_number("0x20000000000001"), 9007199254740992.0);
    let eighty_ones = format!("0b{}", "1".repeat(80));
    assert_eq!(parse_js_number(&eighty_ones), 2f64.powi(80) - 1.0);
    // 超出 f64 范围 -> Infinity（V8 同样）。
    assert_eq!(
        parse_js_number(&format!("0x{}", "f".repeat(400))),
        f64::INFINITY
    );
}

#[test]
fn number_coercion_prefix_edge_cases() {
    // 前缀只接受无符号形式；空数字段不是字面量。
    assert!(parse_js_number("-0x10").is_nan());
    assert!(parse_js_number("+0x10").is_nan());
    assert!(parse_js_number("0x").is_nan());
    assert!(parse_js_number("0x10px").is_nan());
    assert_eq!(parse_js_number("  0X1f  "), 31.0);
    assert_eq!(parse_js_number("0o17"), 15.0);
    assert_eq!(parse_js_number("0b101"), 5.0);
}

// ---------------------------------------------------------------------------
// 黄金对照：`Number("0x…"/"0b…"/"0o…")` 的 300 条 Node 26 向量
// ---------------------------------------------------------------------------

#[test]
fn prefixed_literal_golden_vectors() {
    let fixture = load("number_prefix_golden.json");
    let mut failures = Vec::new();
    for case in fixture["prefixed"].as_array().expect("prefixed") {
        let text = case["text"].as_str().expect("text");
        let value = parse_js_number(text);
        if case["is_nan"].as_bool().expect("is_nan") {
            if !value.is_nan() {
                failures.push(format!("{text}: expected NaN, got {value}"));
            }
            continue;
        }
        let want = u64::from_str_radix(case["bits"].as_str().expect("bits"), 16).expect("bits");
        if value.to_bits() != want {
            failures.push(format!(
                "{text}: bits {:016x} != {want:016x}",
                value.to_bits()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
