//! describe: "js_string" —— String(v) / typeof / truthiness / js_quote

use kiana_json::js_parse::parse;
use kiana_json::js_string::{
    JsType, is_truthy, js_quote, js_strict_equal, js_string, js_trim, js_typeof,
};
use serde_json::Value;

fn load(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture readable"))
        .expect("fixture is valid JSON")
}

#[test]
fn string_semantics_fixture() {
    let fixture = load("string_semantics.json");
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let literal = case["literal"].as_str().expect("literal");
        let value = parse(literal).expect("literal parses");
        let expected_string = case["string"].as_str().expect("string");
        let expected_typeof = case["typeof"].as_str().expect("typeof");
        let expected_truthy = case["truthy"].as_bool().expect("truthy");
        if js_string(Some(&value)) != expected_string {
            failures.push(format!(
                "{literal}: String() got {:?}, want {expected_string:?}",
                js_string(Some(&value))
            ));
        }
        if js_typeof(&value).as_str() != expected_typeof {
            failures.push(format!(
                "{literal}: typeof got {:?}, want {expected_typeof:?}",
                js_typeof(&value).as_str()
            ));
        }
        if is_truthy(Some(&value)) != expected_truthy {
            failures.push(format!("{literal}: truthy mismatch"));
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
fn js_quote_fixture() {
    let fixture = load("js_quote.json");
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let input = case["input"].as_str().expect("input");
        let expected = case["expected"].as_str().expect("expected");
        let got = js_quote(input);
        if got != expected {
            failures.push(format!("{input:?}: got {got:?}, want {expected:?}"));
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
fn js_string_of_undefined_and_null() {
    assert_eq!(js_string(None), "undefined");
    assert_eq!(js_string(Some(&Value::Null)), "null");
    assert_eq!(js_typeof(&Value::Null), JsType::Object);
    assert_eq!(js_typeof(&Value::Bool(true)), JsType::Boolean);
    assert_eq!(js_typeof(&Value::from(1)), JsType::Number);
    assert_eq!(js_typeof(&Value::String("x".into())), JsType::String);
    assert_eq!(js_typeof(&Value::Array(vec![])), JsType::Object);
    assert_eq!(JsType::Undefined.as_str(), "undefined");
    assert_eq!(JsType::Function.as_str(), "function");
}

#[test]
fn js_string_non_finite_numbers_use_javascript_words() {
    // String(NaN) === "NaN"，String(Infinity) === "Infinity"（不是 JSON 的 "null"）。
    //
    // `Value::from(f64::NAN)` 是 `Null`（serde_json 的 From<f64> 对非有限值返回 Null），
    // 所以这里直接构造 `Number`——这正是 `js_string` 在内部按 `as_f64()` 取到的情形。
    fn num(v: f64) -> Value {
        Value::Number(
            serde_json::Number::from_f64(v).unwrap_or_else(|| serde_json::Number::from(0)),
        )
    }
    assert_eq!(js_string(Some(&Value::from(f64::NAN))), "null");
    // NaN/Infinity 只能通过 `Number` 的 f64 分支触达；验证 `format_js_number` 的配对语义。
    assert_eq!(js_string(Some(&num(2.5))), "2.5");
    // 数组里的 null 元素渲染为空串（`[NaN]` 在 JS 里也是因为 NaN 不是 null 才写 "NaN"，
    // 但 Rust 的 Value 无法持有 NaN，落点为 null → 空串）。
    assert_eq!(
        js_string(Some(&Value::Array(vec![Value::Null, Value::from(1)]))),
        ",1"
    );
}

#[test]
fn js_strict_equal_matches_javascript() {
    assert!(js_strict_equal(
        &parse("1").unwrap(),
        &parse("1.0").unwrap()
    ));
    assert!(js_strict_equal(&parse("-0").unwrap(), &parse("0").unwrap()));
    assert!(!js_strict_equal(&parse("1").unwrap(), &parse("2").unwrap()));
    assert!(!js_strict_equal(&Value::Null, &Value::Bool(false)));
    assert!(!js_strict_equal(
        &parse("[]").unwrap(),
        &parse("[]").unwrap()
    ));
}

#[test]
fn is_truthy_matches_javascript() {
    assert!(!is_truthy(None));
    assert!(!is_truthy(Some(&Value::Null)));
    assert!(!is_truthy(Some(&Value::Bool(false))));
    assert!(!is_truthy(Some(&Value::from(0))));
    assert!(!is_truthy(Some(&parse("-0").unwrap())));
    assert!(!is_truthy(Some(&Value::String(String::new()))));
    assert!(is_truthy(Some(&Value::Array(vec![]))));
    assert!(is_truthy(Some(&parse("{}").unwrap())));
    assert!(is_truthy(Some(&Value::String("0".into()))));
}

// ---------------------------------------------------------------------------
// 回归：深值不能 abort 进程（`String(v)` 在 V8 里抛可捕获的 RangeError）
// ---------------------------------------------------------------------------

#[test]
fn deep_arrays_do_not_overflow_the_stack() {
    // 旧的递归实现对每个数组元素递归一次：8 MB 栈上 ~44k 层就 `fatal runtime error:
    // stack overflow, aborting`（只有 ~88 KB 输入），而本 crate 的 `parse` 是迭代的、
    // 能构造出任意深度的值。`js_string` 必须与 `js_object` 的遍历一样用显式栈。
    //
    // `serde_json::Value` 自身的 `Drop` 仍是递归的（外部类型，见 js_object 模块文档），
    // 所以深值在 64 MB 栈的线程里构造，让断言只反映 `js_string` 本身。
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            const DEPTH: usize = 200_000;
            let text = format!("{}1{}", "[".repeat(DEPTH), "]".repeat(DEPTH));
            let value = parse(&text).expect("deep arrays parse");
            // `[[[…1…]]]` 的 `String()` 是 `"1"`（每层的 join 只有单个元素）。
            assert_eq!(js_string(Some(&value)), "1");

            // 深层字符串元素同样穿透到底。
            let text = format!("{}\"x\"{}", "[".repeat(DEPTH), "]".repeat(DEPTH));
            let value = parse(&text).expect("deep arrays parse");
            assert_eq!(js_string(Some(&value)), "x");

            // 对象在 `String()` 里是 `"[object Object]"`，不递归；确认不 abort。
            let text = format!("{}1{}", "{\"a\":".repeat(50_000), "}".repeat(50_000));
            let value = parse(&text).expect("deep objects parse");
            assert_eq!(js_string(Some(&value)), "[object Object]");

            // 广数组里的 null/undefined 元素仍写空串（多元素场景走显式栈的分隔符路径）。
            let wide = Value::Array(vec![Value::from(1), Value::Null, Value::from(2)]);
            assert_eq!(js_string(Some(&wide)), "1,,2");
        })
        .expect("thread spawns")
        .join()
        .expect("deep values handled without aborting");
}

// ---------------------------------------------------------------------------
// 验收：`String.prototype.trim`（`parseToolCallArguments` 的 `!json?.trim()` 依赖它）
// ---------------------------------------------------------------------------

#[test]
fn js_trim_matches_string_prototype_trim() {
    // fixture 里 16 个 JS 空白全部去掉；U+0085/U+180E/U+200B/U+001C..1F 保留。
    for ws in [
        "\u{0009}", "\u{000a}", "\u{000b}", "\u{000c}", "\u{000d}", "\u{0020}", "\u{00a0}",
        "\u{1680}", "\u{2000}", "\u{2007}", "\u{200a}", "\u{2028}", "\u{2029}", "\u{202f}",
        "\u{205f}", "\u{3000}", "\u{feff}",
    ] {
        assert_eq!(
            js_trim(&format!("{ws}x{ws}")),
            "x",
            "U+{:04X}",
            ws.chars().next().unwrap() as u32
        );
        assert_eq!(
            js_trim(ws),
            "",
            "U+{:04X}",
            ws.chars().next().unwrap() as u32
        );
    }
    for not_ws in [
        "\u{0085}", "\u{180e}", "\u{200b}", "\u{001c}", "\u{001d}", "\u{001e}", "\u{001f}",
        "\u{001b}",
    ] {
        assert_eq!(
            js_trim(&format!("{not_ws}x{not_ws}")),
            format!("{not_ws}x{not_ws}"),
            "U+{:04X}",
            not_ws.chars().next().unwrap() as u32
        );
    }
    assert_eq!(js_trim(""), "");
    assert_eq!(js_trim(" a b "), "a b");
}
