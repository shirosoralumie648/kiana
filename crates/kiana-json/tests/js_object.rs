//! describe: "js_object" —— JSON.stringify 键序/缩进/深比较

use kiana_json::js_object::{deep_equal, sort_js_keys, stringify, stringify_pretty};
use kiana_json::js_parse::parse;
use serde_json::Value;

fn load(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture readable"))
        .expect("fixture is valid JSON")
}

#[test]
fn stringify_compact_fixture() {
    let fixture = load("stringify.json");
    let mut failures = Vec::new();
    for case in fixture["compact"].as_array().expect("compact") {
        let value = parse(case["value"].as_str().expect("value")).expect("value parses");
        let expected = case["expected"].as_str().expect("expected");
        let got = stringify(&value);
        if got != expected {
            failures.push(format!("{:?}: got {got}, want {expected}", case["value"]));
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
fn stringify_pretty_fixture() {
    let fixture = load("stringify.json");
    let mut failures = Vec::new();
    for case in fixture["pretty"].as_array().expect("pretty") {
        let value = parse(case["value"].as_str().expect("value")).expect("value parses");
        let indent = case["indent"].as_u64().expect("indent") as usize;
        let expected = case["expected"].as_str().expect("expected");
        let got = stringify_pretty(&value, indent);
        if got != expected {
            failures.push(format!(
                "{:?} indent {indent}: got {got:?}, want {expected:?}",
                case["value"]
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
fn indent_zero_is_compact() {
    // JS: JSON.stringify(v, null, 0) === JSON.stringify(v)
    let value = parse(r#"{"a":[1,{"b":null}],"c":[]}"#).unwrap();
    assert_eq!(stringify_pretty(&value, 0), stringify(&value));
}

#[test]
fn indent_above_ten_clamps() {
    // JS 的 space 超过 10 按 10 处理。
    let value = parse(r#"{"a":1}"#).unwrap();
    assert_eq!(stringify_pretty(&value, 12), stringify_pretty(&value, 10));
}

#[test]
fn deep_equal_ignores_key_order_and_number_representation() {
    // 键序不同但内容相同。
    let a = parse(r#"{"a":1,"b":[2,{"c":3}]}"#).unwrap();
    let b = parse(r#"{"b":[2,{"c":3}],"a":1}"#).unwrap();
    assert!(deep_equal(&a, &b));
    // serde_json::Number 的 == 区分 PosInt/Float，JS 的 === 不区分。
    let one_int: Value = serde_json::from_str("1").unwrap();
    let one_float: Value = serde_json::from_str("1.0").unwrap();
    assert!(deep_equal(&one_int, &one_float));
    // -0 === 0
    let neg_zero: Value = serde_json::from_str("-0").unwrap();
    let zero: Value = serde_json::from_str("0").unwrap();
    assert!(deep_equal(&neg_zero, &zero));
    // NaN 装不进 Value（serde_json 的 Value::from(f64::NAN) 是 Null），因此只能在
    // js_strict_equal 上验证 NaN !== NaN；deep_equal 复用相同的数字比较逻辑。
}

#[test]
fn deep_equal_distinguishes_values() {
    assert!(!deep_equal(&parse("1").unwrap(), &parse("2").unwrap()));
    assert!(!deep_equal(
        &parse("[1]").unwrap(),
        &parse("[1,2]").unwrap()
    ));
    assert!(!deep_equal(
        &parse(r#"{"a":1}"#).unwrap(),
        &parse(r#"{"a":1,"b":2}"#).unwrap()
    ));
    assert!(!deep_equal(&Value::Null, &Value::Bool(false)));
    assert!(deep_equal(&Value::Null, &Value::Null));
}

#[test]
fn sort_js_keys_recurses_and_handles_index_keys() {
    let value = parse(r#"{"b":{"10":1,"2":2,"a":3},"2":4,"01":5}"#).unwrap();
    let sorted = sort_js_keys(&value);
    assert_eq!(
        stringify(&sorted),
        r#"{"2":4,"b":{"2":2,"10":1,"a":3},"01":5}"#
    );
}

// ---------------------------------------------------------------------------
// 回归：深比较不递归
// ---------------------------------------------------------------------------

#[test]
fn deep_structures_compare_without_recursion() {
    // `deep_equal` 曾是递归的（~200k 层溢出）。在 64 MB 栈的线程里跑，把 `serde_json::Value`
    // 自身递归 `Drop` 的影响排除掉（外部类型，见 js_object 模块文档）。
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            fn items(depth: usize) -> String {
                format!("{}{}", "[".repeat(depth), "]".repeat(depth))
            }
            let depth = 100_000;
            let a = parse(&items(depth)).unwrap();
            let b = parse(&items(depth)).unwrap();
            assert!(deep_equal(&a, &b));
            // 末端多一个元素 -> false（长度不同）。
            let longer = parse(&format!("{}1{}", "[".repeat(depth), "]".repeat(depth))).unwrap();
            assert!(!deep_equal(&a, &longer));
            // 同深度但叶子不同 -> false。
            let other = parse(&format!(
                "{}true{}",
                "[".repeat(depth - 1),
                "]".repeat(depth - 1)
            ))
            .unwrap();
            assert!(!deep_equal(&a, &other));
        })
        .expect("thread spawns")
        .join()
        .expect("deep_equal handles deep input");
}
