//! 验收清单（`spec/ai-core.md` §2.2.25、`ARCHITECTURE.md` §3.2/§4.9、ARCH §8 第 31–33 条）的
//! 一站式黄金对照：从 TS 参照实现对每个输入记录**全部**可观察量，Rust 侧逐列比对。
//!
//! fixture `api_golden.json` 由 `packages/ai/src/utils/json-parse.ts` +
//! `partial-json@0.1.7`（node 26，`/home/ctyun/kiana-work/upstream-reference`）生成；生成脚本与原始输出见
//! 仓库外的 `/home/ctyun/kiana-work/scratch/gate__kiana-json/ts/`。列的含义：
//!
//! - `repair`：`repairJson(input)` 的返回值（TS 永远有返回，Rust 用 `Option`，`Some` 必须相等）；
//! - `parse.ok/value/keys/error` / `with_repair.*` / `streaming.*`：严格 `JSON.parse`、
//!   `parseJsonWithRepair`、`parseStreamingJson` 的值与 V8 错误文本；
//! - `keys`：`Object.keys()` 的枚举序（**直接**读 `serde_json::Map` 的迭代序，不经
//!   `stringify`/`sort_js_keys`，否则会把缺陷重排掉）；
//! - `tool_call.arguments/arguments_error`：`parseToolCallArguments` 的逐字输出（含
//!   `"\n\nReceived arguments:\n"` 两行格式）；
//! - `allow_cases`：`partial-json` 的 `Allow` 位掩码（`partial::parse(input, mask)`）。
//!
//! 两处已声明的被迫偏差（ARCH §8 第 31、32 条）在 fixture 里预先抹平：孤立代理项写成 U+FFFD，
//! 非有限数写成 `null`；`lone_surrogate` 标出输入本身含孤立代理项转义的用例，值列对它们跳过。
//!
//! `parseToolCallArguments` 的 Rust 实现在 `kiana-ai`（`utils/streaming_json.rs` 的
//! `parse_tool_call_arguments`，由 ai-team 移植）。本测试不重复实现它，而是**逐字**断言它的
//! 两条规则组合在 `kiana-json` 的四个原语上（`trim` 语义 + `parse_with_repair` + `is_object` +
//! 错误文本拼接）：等 `kiana-ai` 落地后，`tool_call` 列就是它的验收向量。

use kiana_json::js_object::stringify;
use kiana_json::js_parse::partial::{allow, parse as partial_parse};
use kiana_json::js_parse::{parse, parse_streaming, parse_with_repair, repair};
use serde_json::Value;

fn load() -> Value {
    let path = format!(
        "{}/tests/fixtures/api_golden.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture readable"))
        .expect("fixture is valid JSON")
}

/// JS 对象的自有键枚举序：直接读返回值的 `Map` 迭代序，**不做**重排。
fn js_key_order(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// `parseToolCallArguments` 的两条规则（`spec/ai-core.md` §2.2.25）在 `kiana-json` 原语上的等价物。
///
/// 只用于对照 fixture；`kiana-ai` 的实现必须与它逐字一致（`tests/api_golden.rs` 的 `tool_call`
/// 列就是那份契约）。
fn tool_call_arguments(json: &str) -> (Value, Option<String>) {
    // `!json?.trim()` -> `{arguments:{}}`（**无** `argumentsError` 键）。
    // 注意是 JS 的 `String.prototype.trim`（含 U+FEFF），不是 Rust 的 `str::trim`。
    if kiana_json::js_trim(json).is_empty() {
        return (Value::Object(Default::default()), None);
    }
    match parse_with_repair(json) {
        Ok(value) => {
            // `typeof parsed !== "object" || parsed === null || Array.isArray(parsed)`
            if !value.is_object() {
                return (
                    Value::Object(Default::default()),
                    Some(format!(
                        "Tool arguments must be a JSON object\n\nReceived arguments:\n{json}"
                    )),
                );
            }
            (value, None)
        }
        Err(error) => (
            Value::Object(Default::default()),
            Some(format!("{}\n\nReceived arguments:\n{json}", error.message)),
        ),
    }
}

#[test]
fn repair_json_golden() {
    let fixture = load();
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let input = case["input"].as_str().expect("input");
        let expected = case["repair"].as_str().expect("repair");
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
fn json_parse_golden_including_v8_error_text() {
    let fixture = load();
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let input = case["input"].as_str().expect("input");
        let ts = &case["parse"];
        if ts["ok"].as_bool().expect("ok") {
            match parse(input) {
                Ok(value) => {
                    let got = stringify(&value);
                    let want = ts["value"].as_str().expect("value");
                    if got != want {
                        // 孤立代理项：Rust 只能得到 U+FFFD（ARCH §8 第 31 条）。
                        let lone = case["lone_surrogate"].as_bool().unwrap_or(false);
                        if !lone {
                            failures.push(format!("{input:?}: got {got}, want {want}"));
                        }
                    }
                    if let Some(keys) = ts["keys"].as_array() {
                        let want: Vec<&str> = keys.iter().map(|k| k.as_str().unwrap()).collect();
                        let got = js_key_order(&value);
                        if got != want {
                            failures.push(format!("{input:?}: keys {got:?} != {want:?}"));
                        }
                    }
                }
                Err(error) => failures.push(format!("{input:?}: failed: {}", error.message)),
            }
        } else {
            let want = ts["error"].as_str().expect("error");
            match parse(input) {
                Ok(value) => failures.push(format!("{input:?}: parsed to {value}, want error")),
                Err(error) => {
                    if error.message != want {
                        failures.push(format!(
                            "{input:?}:\n  got:  {}\n  want: {want}",
                            error.message
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
fn parse_with_repair_golden() {
    let fixture = load();
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let input = case["input"].as_str().expect("input");
        let lone = case["lone_surrogate"].as_bool().unwrap_or(false);
        let ts = &case["with_repair"];
        if ts["ok"].as_bool().expect("ok") {
            match parse_with_repair(input) {
                Ok(value) => {
                    let got = stringify(&value);
                    let want = ts["value"].as_str().expect("value");
                    if got != want && !lone {
                        failures.push(format!("{input:?}: got {got}, want {want}"));
                    }
                }
                Err(error) => failures.push(format!("{input:?}: failed: {}", error.message)),
            }
        } else {
            let want = ts["error"].as_str().expect("error");
            match parse_with_repair(input) {
                Ok(value) => failures.push(format!("{input:?}: parsed to {value}, want error")),
                Err(error) => {
                    if error.message != want && !lone {
                        failures.push(format!(
                            "{input:?}:\n  got:  {}\n  want: {want}",
                            error.message
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
fn parse_streaming_golden() {
    let fixture = load();
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let input = case["input"].as_str().expect("input");
        let lone = case["lone_surrogate"].as_bool().unwrap_or(false);
        let ts = &case["streaming"];
        if !ts["ok"].as_bool().expect("ok") {
            failures.push(format!("{input:?}: fixture records an unexpected throw"));
            continue;
        }
        let (value, _complete) = parse_streaming(input);
        let got = stringify(&value);
        let want = ts["value"].as_str().expect("value");
        if got != want && !lone {
            failures.push(format!("{input:?}: got {got}, want {want}"));
        }
        if let Some(keys) = ts["keys"].as_array() {
            let want: Vec<&str> = keys.iter().map(|k| k.as_str().unwrap()).collect();
            let got = js_key_order(&value);
            if got != want {
                failures.push(format!("{input:?}: keys {got:?} != {want:?}"));
            }
        }
        // 非有限数在 TS 里是 number（`is_null === false`），Rust 只能落成 `Value::Null`
        // （ARCH §8 第 32 条），因此 `is_null` 列对这些用例不适用。
        if !ts["non_finite_number"].as_bool().unwrap_or(false)
            && ts["is_null"].as_bool().unwrap_or(false) != value.is_null()
        {
            failures.push(format!("{input:?}: is_null mismatch"));
        }
        if ts["is_array"].as_bool().unwrap_or(false) != value.is_array() {
            failures.push(format!("{input:?}: is_array mismatch"));
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
fn tool_call_arguments_composition_golden() {
    // `kiana-ai::utils::streaming_json::parse_tool_call_arguments` 的验收向量（见文件头注释）。
    let fixture = load();
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let input = case["input"].as_str().expect("input");
        let lone = case["lone_surrogate"].as_bool().unwrap_or(false);
        let ts = &case["tool_call"];
        let (arguments, error) = tool_call_arguments(input);
        let got = stringify(&arguments);
        let want = ts["arguments"].as_str().expect("arguments");
        if got != want && !lone {
            failures.push(format!("{input:?}: arguments {got} != {want}"));
        }
        let want_error = ts["arguments_error"].as_str();
        match (error.as_deref(), want_error) {
            (None, None) => {}
            (Some(got), Some(want)) if got == want || lone => {}
            (got, want) => failures.push(format!("{input:?}: error {got:?} != {want:?}")),
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
fn partial_allow_masks_golden() {
    // `partial-json` 的 `Allow` 位掩码语义（数值逐字对齐 options.js）。
    assert_eq!(allow::STR, 0b000000001);
    assert_eq!(allow::NUM, 0b000000010);
    assert_eq!(allow::ARR, 0b000000100);
    assert_eq!(allow::OBJ, 0b000001000);
    assert_eq!(allow::NULL, 0b000010000);
    assert_eq!(allow::BOOL, 0b000100000);
    assert_eq!(allow::NAN, 0b001000000);
    assert_eq!(allow::INFINITY, 0b010000000);
    assert_eq!(allow::_INFINITY, 0b100000000);
    assert_eq!(allow::INF, allow::INFINITY | allow::_INFINITY);
    assert_eq!(
        allow::SPECIAL,
        allow::NULL | allow::BOOL | allow::INF | allow::NAN
    );
    assert_eq!(allow::ATOM, allow::STR | allow::NUM | allow::SPECIAL);
    assert_eq!(allow::COLLECTION, allow::ARR | allow::OBJ);
    assert_eq!(allow::ALL, allow::ATOM | allow::COLLECTION);
    assert_eq!(kiana_json::js_parse::partial::ALLOW_ALL, allow::ALL);

    let fixture = load();
    let mut failures = Vec::new();
    for case in fixture["allow_cases"].as_array().expect("allow_cases") {
        let input = case["input"].as_str().expect("input");
        let mask = case["mask"].as_u64().expect("mask") as u32;
        let name = case["mask_name"].as_str().expect("mask_name");
        match partial_parse(input, mask) {
            Ok(value) => {
                if !case["ok"].as_bool().expect("ok") {
                    failures.push(format!(
                        "{input:?} [{name}]: parsed but TS threw {:?}",
                        case["error"].as_str()
                    ));
                    continue;
                }
                let got = stringify(&value);
                let want = case["value"].as_str().expect("value");
                if got != want {
                    failures.push(format!("{input:?} [{name}]: got {got}, want {want}"));
                }
            }
            Err(error) => {
                if case["ok"].as_bool().expect("ok") {
                    failures.push(format!(
                        "{input:?} [{name}]: failed ({:?} {}), want {:?}",
                        error.kind,
                        error.message,
                        case["value"].as_str()
                    ));
                    continue;
                }
                let want = case["error"].as_str().expect("error");
                if error.message != want {
                    failures.push(format!(
                        "{input:?} [{name}]:\n  got:  {:?}\n  want: {want:?}",
                        error.message
                    ));
                }
                let want_class = case["kind"].as_str().expect("kind");
                let got_class = match error.kind {
                    kiana_json::js_parse::partial::errors::PartialErrorKind::Partial => {
                        "PartialJSON"
                    }
                    kiana_json::js_parse::partial::errors::PartialErrorKind::Malformed => {
                        "MalformedJSON"
                    }
                    kiana_json::js_parse::partial::errors::PartialErrorKind::Plain => "Error",
                    kiana_json::js_parse::partial::errors::PartialErrorKind::Syntax => {
                        "SyntaxError"
                    }
                };
                if got_class != want_class {
                    failures.push(format!(
                        "{input:?} [{name}]: class {got_class} != {want_class}"
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
fn every_public_symbol_is_exercised() {
    // ARCH §4.8 第 9 条：每个公开 API 至少被一个测试触达。这里做一次「编译期 + 运行期」点名，
    // 让新增的公开项漏测时这条测试先红（新增项请同时补行为断言）。
    use kiana_json::js_num::{format_js_number, is_js_integer, parse_js_number, to_radix_string};
    use kiana_json::js_object::{deep_equal, sort_js_keys, stringify_pretty};
    use kiana_json::js_string::{
        JsType, is_truthy, js_quote, js_strict_equal, js_string, js_trim, js_typeof,
    };
    use kiana_json::{
        normalize_numbers as root_normalize, parse as root_parse,
        parse_streaming as root_streaming, parse_with_repair as root_with_repair,
        stringify as root_stringify,
    };

    assert_eq!(format_js_number(1.5), "1.5");
    assert!(is_js_integer(2.0));
    assert_eq!(parse_js_number("0x10"), 16.0);
    assert_eq!(to_radix_string(255.0, 16), "ff");
    assert_eq!(stringify_pretty(&root_parse("{}").unwrap(), 2), "{}");
    assert!(deep_equal(
        &root_parse("1").unwrap(),
        &root_parse("1.0").unwrap()
    ));
    assert_eq!(
        root_stringify(&sort_js_keys(&root_parse(r#"{"b":1,"2":2}"#).unwrap())),
        r#"{"2":2,"b":1}"#
    );
    assert_eq!(js_string(None), "undefined");
    assert!(is_truthy(Some(&Value::Bool(true))));
    assert_eq!(js_quote("x"), "\"x\"");
    assert_eq!(js_trim("\u{feff} a \u{feff}"), "a");
    assert_eq!(
        root_stringify(&root_normalize(serde_json::json!(u64::MAX))),
        "18446744073709552000"
    );
    assert!(js_strict_equal(&Value::Null, &Value::Null));
    assert_eq!(js_typeof(&Value::Null), JsType::Object);
    assert_eq!(JsType::Undefined.as_str(), "undefined");
    assert!(root_with_repair("{}").is_ok());
    assert!(root_streaming("{}").1);
    // `ParseError::new` / 位置字段。
    let error = kiana_json::ParseError::new("boom");
    assert_eq!(error.message, "boom");
    assert_eq!(error.position, None);
    assert_eq!(error.to_string(), "boom");
    // `"Unexpected token 'x', ..."` 模板不带位置（V8 只在有位置时才加后缀）；
    // 带位置的模板（如 `Expected …`）填 `(line, column)`。
    let positioned = parse(r#"{"a":1,}"#).unwrap_err();
    assert_eq!(positioned.position, Some((1, 8)));
}
