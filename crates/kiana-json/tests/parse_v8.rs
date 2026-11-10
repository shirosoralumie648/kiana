//! `JSON.parse` 的 V8 兼容面：消息、位置、excerpt、接受度。
//!
//! describe: "JSON.parse (V8 compatible)"

use kiana_json::js_object::{stringify, stringify_pretty};
use kiana_json::js_parse::parse;
use serde_json::Value;

fn load(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).expect("fixture readable");
    serde_json::from_str(&text).expect("fixture is valid JSON")
}

#[test]
fn curated_v8_parse_cases() {
    let fixture = load("json_parse_v8.json");
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let input = case["input"].as_str().expect("input");
        if case["ok"].as_bool().expect("ok") {
            match parse(input) {
                Ok(value) => {
                    // 孤立代理项：V8 保留它们，Rust 只能得到 U+FFFD，fixture 为此提供 value_fffd。
                    let expected = case["value_fffd"]
                        .as_str()
                        .or_else(|| case["value"].as_str());
                    if let Some(expected) = expected {
                        let actual = stringify(&value);
                        if actual != expected {
                            failures.push(format!("{input:?}: value {actual} != {expected}"));
                        }
                    }
                }
                Err(e) => failures.push(format!("{input:?}: unexpectedly failed: {}", e.message)),
            }
        } else {
            let expected = case["message"].as_str().expect("message");
            match parse(input) {
                Ok(v) => failures.push(format!("{input:?}: unexpectedly parsed to {v}")),
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
fn v8_excerpt_vectors() {
    let fixture = load("v8_excerpts.json");
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let input = case["input"].as_str().expect("input");
        let expected = case["message"].as_str().expect("message");
        match parse(input) {
            Ok(_) => failures.push(format!("{input:?}: unexpectedly parsed")),
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
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn json_test_suite() {
    let fixture = load("json_test_suite.json");
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let input = case["text"].as_str().expect("text");
        let name = case["name"].as_str().expect("name");
        if case["ok"].as_bool().expect("ok") {
            let lone = case["lone_surrogate_in_value"].as_bool().unwrap_or(false);
            let expected = if lone {
                case["value_fffd"]
                    .as_str()
                    .or_else(|| case["value"].as_str())
            } else {
                case["value"]
                    .as_str()
                    .or_else(|| case["value_fffd"].as_str())
            };
            match parse(input) {
                Ok(value) => {
                    if let Some(expected) = expected {
                        let actual = stringify(&value);
                        if actual != expected {
                            failures.push(format!("{name}: value {actual} != {expected}"));
                        }
                    }
                }
                Err(e) => failures.push(format!("{name}: unexpectedly failed: {}", e.message)),
            }
        } else {
            let expected = case["message"].as_str().expect("message");
            match parse(input) {
                Ok(v) => failures.push(format!("{name}: unexpectedly parsed to {v}")),
                Err(e) => {
                    if e.message != expected {
                        failures.push(format!(
                            "{name}:\n  got:  {}\n  want: {expected}",
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
fn fixtures_are_fully_loaded() {
    // 防回归：确保 fixture 没被截断，也不是空的。
    let curated = load("json_parse_v8.json");
    assert_eq!(curated["cases"].as_array().unwrap().len(), 50);
    let suite = load("json_test_suite.json");
    assert_eq!(suite["cases"].as_array().unwrap().len(), 316);
    let excerpts = load("v8_excerpts.json");
    assert_eq!(excerpts["cases"].as_array().unwrap().len(), 94);
    let repair_cases = load("repair.json");
    assert_eq!(repair_cases["cases"].as_array().unwrap().len(), 21);
    let with_repair = load("parse_with_repair.json");
    assert_eq!(with_repair["cases"].as_array().unwrap().len(), 18);
    let streaming = load("parse_streaming.json");
    assert_eq!(streaming["cases"].as_array().unwrap().len(), 56);
    let partial = load("partial_json.json");
    assert_eq!(partial["cases"].as_array().unwrap().len(), 56);
    let numbers = load("number_format.json");
    assert_eq!(numbers["cases"].as_array().unwrap().len(), 34);
    let doubles = load("random_doubles.json");
    assert_eq!(doubles["cases"].as_array().unwrap().len(), 1200);
    let radix = load("radix.json");
    assert_eq!(radix["cases"].as_array().unwrap().len(), 31);
    let coercion = load("number_coercion.json");
    assert_eq!(coercion["cases"].as_array().unwrap().len(), 27);
    let semantics = load("string_semantics.json");
    assert_eq!(semantics["cases"].as_array().unwrap().len(), 16);
    let quotes = load("js_quote.json");
    assert_eq!(quotes["cases"].as_array().unwrap().len(), 316);
    let stringify = load("stringify.json");
    assert_eq!(stringify["compact"].as_array().unwrap().len(), 13);
    assert_eq!(stringify["pretty"].as_array().unwrap().len(), 78);
}

#[test]
fn unterminated_literal_reports_the_string_token() {
    // V8 的 `one_char_json_tokens` 把 `"` 映射为 STRING，走专属模板
    // `Unexpected string in JSON at position …`，而不是 `Unexpected token '"'`。
    for (input, expected) in [
        (
            r#"f".r x}"#,
            "Unexpected string in JSON at position 1 (line 1 column 2)",
        ),
        (
            r#"n"]/urt"#,
            "Unexpected string in JSON at position 1 (line 1 column 2)",
        ),
        (
            r#"tru"e""#,
            "Unexpected string in JSON at position 3 (line 1 column 4)",
        ),
        // `f`+`a` 是 `false` 的真前缀 -> EOS 模板，不是 token 模板。
        (r#"fa"#, "Unexpected end of JSON input"),
    ] {
        assert_eq!(
            parse(input).unwrap_err().message,
            expected,
            "input {input:?}"
        );
    }
}

#[test]
fn unterminated_and_control_character_messages() {
    assert_eq!(
        parse(r#""abc"#).unwrap_err().message,
        "Unterminated string in JSON at position 4 (line 1 column 5)"
    );
    assert_eq!(
        parse("\"a\nb\"").unwrap_err().message,
        "Bad control character in string literal in JSON at position 2 (line 1 column 3)"
    );
    assert_eq!(
        parse("f").unwrap_err().message,
        "Unexpected end of JSON input"
    );
}

// ---------------------------------------------------------------------------
// 回归：整数保留整数表示（ARCHITECTURE.md §4.3 第 4 条）
// ---------------------------------------------------------------------------

#[test]
fn integer_literals_keep_integer_representation() {
    let value = parse(r#"{"a":1,"b":-3,"c":0}"#).unwrap();
    let a = value["a"].as_number().expect("number");
    // `as_u64`/`as_i64` 可用，且 serde 默认写出不是 `1.0`。
    assert_eq!(a.as_u64(), Some(1));
    assert_eq!(a.as_i64(), Some(1));
    assert_eq!(
        serde_json::to_string(&value).unwrap(),
        r#"{"a":1,"b":-3,"c":0}"#
    );
    assert_eq!(value, serde_json::json!({"a":1,"b":-3,"c":0}));

    // 下游的强类型结构体（Frame 的 i64、Usage 的 u64）必须能反序列化。
    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct FrameId {
        id: i64,
    }
    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct UsageInput {
        input: u64,
    }
    assert_eq!(
        serde_json::from_value::<FrameId>(parse(r#"{"id":7}"#).unwrap()).unwrap(),
        FrameId { id: 7 }
    );
    assert_eq!(
        serde_json::from_value::<UsageInput>(parse(r#"{"input":7}"#).unwrap()).unwrap(),
        UsageInput { input: 7 }
    );

    // JS 只有 f64 一个数字类型，字面量拼写不影响整数值：`1.0`/`1e2`/`2.0` 在 V8 里都是
    // `Number.isInteger` 为真的整数，TS 的 TypeBox `Integer` 与 Rust 的 `i64`/`u64` 都照收。
    // 因此这些拼写同样保留整数表示（见本条下面的专项回归）。
    assert_eq!(parse("1.0").unwrap().as_u64(), Some(1));
    assert_eq!(parse("1e3").unwrap().as_u64(), Some(1000));
    assert_eq!(parse("100.0").unwrap().as_u64(), Some(100));
    // 非整数值仍是浮点。
    assert!(parse("1.5").unwrap().as_number().expect("number").is_f64());
    assert!(parse("1e-7").unwrap().as_number().expect("number").is_f64());
    // `-0` 保留符号（Object.is 可观察），不能走整数路径。
    assert!(parse("-0").unwrap().as_f64().unwrap().is_sign_negative());
    assert!(parse("-0.0").unwrap().as_f64().unwrap().is_sign_negative());
    // 超过 2^53 的整数按 JS 的 f64 舍入（不保留 u64 精度）。
    assert_eq!(
        parse("9007199254740993").unwrap().as_f64(),
        Some(9007199254740992.0)
    );
}

// ---------------------------------------------------------------------------
// 回归：整数值的任意拼写都保留整数表示（TypeBox `Integer` 接受 `1e2`/`100.0`）
// ---------------------------------------------------------------------------

#[test]
fn integral_spellings_are_stored_as_integers() {
    // V8：`Number.isInteger(JSON.parse("1e2")) === true`、`String(JSON.parse("100.0")) === "100"`。
    // TS 的消费方（TypeBox `Integer`、`isFrame` 的 `id: number` 整数字段）因此照收这些拼写；
    // Rust 侧若存成 Float，`as_u64()`/`from_value::<i64>` 会拒绝。
    for (input, expected) in [
        ("1e2", 100u64),
        ("1E2", 100),
        ("100.0", 100),
        ("2.0", 2),
        ("1.0", 1),
        ("1e3", 1000),
        ("0.1e1", 1),
        ("1000e-1", 100),
        ("0", 0),
    ] {
        let value = parse(input).unwrap();
        let number = value.as_number().expect("number");
        assert_eq!(number.as_u64(), Some(expected), "input {input:?}: {value}");
        assert!(!number.is_f64(), "input {input:?} must stay integral");
    }
    // 负整数同样保留（`serde_json::Number::from(i64)`）。
    for (input, expected) in [("-1e2", -100i64), ("-2.0", -2), ("-1.0e3", -1000)] {
        let value = parse(input).unwrap();
        let number = value.as_number().expect("number");
        assert_eq!(number.as_i64(), Some(expected), "input {input:?}: {value}");
    }
    // 非整数拼写仍是浮点。
    for input in ["1.5", "0.1", "1e-7", "2.5e-1"] {
        let value = parse(input).unwrap();
        assert!(
            value.as_number().expect("number").is_f64(),
            "input {input:?} must stay floating point"
        );
    }
    // 经过 partial-json 回退的整数值也保留整数表示（`[1e2,3]` -> `[100,3]`）。
    let (value, from_strict) = kiana_json::js_parse::parse_streaming("[1e2,3]");
    assert!(from_strict);
    assert_eq!(serde_json::to_string(&value).unwrap(), "[100,3]");

    // 下游的强类型结构体（kiana-chord 的 Frame id）必须接受这些拼写。
    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct FrameId {
        id: i64,
    }
    for (input, expected) in [
        (r#"{"id":1e2}"#, 100),
        (r#"{"id":100.0}"#, 100),
        (r#"{"id":2.0e1}"#, 20),
    ] {
        assert_eq!(
            serde_json::from_value::<FrameId>(parse(input).unwrap()).unwrap(),
            FrameId { id: expected },
            "input {input:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 回归：V8 的四种整串特例
// ---------------------------------------------------------------------------

#[test]
fn whole_source_special_strings_match_v8() {
    for input in ["[object Object]", "undefined", "NaN", "Infinity"] {
        assert_eq!(
            parse(input).unwrap_err().message,
            format!("\"{input}\" is not valid JSON"),
            "input {input:?}"
        );
    }
    // 只有整串精确相等才走特例（前后空白、后缀都会退回通用模板）。
    assert_eq!(
        parse("[object object]").unwrap_err().message,
        "Unexpected token 'o', \"[object object]\" is not valid JSON"
    );
    assert_eq!(
        parse("[object Object] ").unwrap_err().message,
        "Unexpected token 'o', \"[object Object] \" is not valid JSON"
    );
    assert_eq!(
        parse("undefinedx").unwrap_err().message,
        "Unexpected token 'u', \"undefinedx\" is not valid JSON"
    );
}

// ---------------------------------------------------------------------------
// 回归：`Object.keys` 枚举序 = 整数索引键升序 + 其余插入序
// ---------------------------------------------------------------------------

#[test]
fn parsed_objects_enumerate_in_javascript_key_order() {
    let value = parse(r#"{"b":1,"2":2,"1":3}"#).unwrap();
    let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
    assert_eq!(keys, vec!["1", "2", "b"]);
    // 嵌套对象同样重排。
    let value = parse(r#"{"b":{"10":1,"2":2,"a":3},"2":4,"01":5}"#).unwrap();
    let inner: Vec<&String> = value["b"].as_object().unwrap().keys().collect();
    assert_eq!(inner, vec!["2", "10", "a"]);
    let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
    assert_eq!(keys, vec!["2", "b", "01"]);
    // 重复键：值取最后一个、位置取第一个（`1` 是索引键，位置由索引键规则决定）。
    let value = parse(r#"{"a":1,"2":2,"a":3,"1":4}"#).unwrap();
    let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
    assert_eq!(keys, vec!["1", "2", "a"]);
    assert_eq!(value["a"], serde_json::json!(3));
}

// ---------------------------------------------------------------------------
// 回归：极深输入不能 abort 进程
// ---------------------------------------------------------------------------

#[test]
fn deep_nesting_does_not_overflow_the_stack() {
    // V8 实测 5,000,000 层；旧的递归实现在 release 下 ~16k 层、debug 下 ~4k 层就 abort。
    //
    // 本测试验证的是 **crate 自己的遍历**不再是递归的（`parse`/`stringify`/
    // `sort_js_keys`/`deep_equal`/partial 解析器都是显式栈）。`serde_json::Value` 的 `Drop`
    // 仍是递归的（外部类型，见 js_object 模块文档），所以这里在 64 MB 栈的线程里跑，
    // 让断言只反映本 crate 的行为。
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            const DEPTH: usize = 200_000;
            let text = format!("{}{}", "[".repeat(DEPTH), "]".repeat(DEPTH));
            let value = parse(&text).expect("deep arrays parse");
            assert_eq!(stringify(&value).len(), DEPTH * 2);
            // pretty 的缩进量随深度平方增长，深输入只验证「不 abort」。
            // 深输入的 pretty 输出随深度平方增长（每级缩进），只验证不 abort、形状正确。
            let n = 200usize;
            let shallow = parse(&format!("{}{}", "[".repeat(n), "]".repeat(n))).unwrap();
            let pretty = stringify_pretty(&shallow, 2);
            assert!(pretty.starts_with("[\n  [\n"));
            assert!(pretty.ends_with("]\n]"));
            assert_eq!(pretty.matches('[').count(), n);
            let sorted = kiana_json::js_object::sort_js_keys(&value);
            assert!(kiana_json::js_object::deep_equal(&value, &sorted));
            let (streamed, _) = kiana_json::js_parse::parse_streaming(&text);
            assert!(streamed.is_array());
            drop(sorted);

            // 超长对象链（键序重排同样是迭代的）。
            let text = format!("{}1{}", "{\"a\":".repeat(50_000), "}".repeat(50_000));
            let value = parse(&text).expect("deep objects parse");
            assert!(stringify(&value).starts_with("{\"a\":"));
        })
        .expect("thread spawns")
        .join()
        .expect("deep nesting handled without aborting");
}
