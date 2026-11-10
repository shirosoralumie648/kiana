//! `packages/codemode/test/source.test.ts` 的逐用例移植。
//!
//! 每个 `describe`/`it` 标题逐字保留（ARCHITECTURE.md §4.8 第 1 条），让每条 Rust 测试与
//! 原始用例一一对应，便于定位失败。
//! `it` 标题里的字符（空格、`#`、冒号）在 Rust 测试名里换成 `_`，注释保留原文。

use kiana_codemode::source::{
    CODEMODE_OPTIONS_PREFIX, CODEMODE_SOURCE_GRAMMAR, CodemodeSourceError, CodemodeSourceOptions,
    parse_codemode_source,
};

fn options(max_output_tokens: Option<f64>, timeout_ms: Option<f64>) -> CodemodeSourceOptions {
    CodemodeSourceOptions {
        max_output_tokens,
        timeout_ms,
    }
}

// it("returns plain code unchanged")
#[test]
fn returns_plain_code_unchanged() {
    let parsed = parse_codemode_source("text('hi')").unwrap();
    assert_eq!(parsed.code, "text('hi')");
    assert_eq!(parsed.options, options(None, None));

    let parsed = parse_codemode_source("// just a comment\nreturn 1").unwrap();
    assert_eq!(parsed.code, "// just a comment\nreturn 1");
    assert_eq!(parsed.options, options(None, None));
}

// it("parses the options line and keeps line numbers")
#[test]
fn parses_the_options_line_and_keeps_line_numbers() {
    let parsed =
        parse_codemode_source("// @options: {\"timeout_ms\": 10}\nconst a = 1;\ntext(a)").unwrap();
    assert_eq!(parsed.code, "\nconst a = 1;\ntext(a)");
    assert_eq!(parsed.options, options(None, Some(10.0)));

    let parsed = parse_codemode_source(
        "  // @options:{\"max_output_tokens\":0,\"timeout_ms\":1500}\r\ntext(1)",
    )
    .unwrap();
    assert_eq!(parsed.options, options(Some(0.0), Some(1500.0)));
    // TS 的 `input.slice(newline)` 从 LF 开始，CRLF 的前导 CR 被丢掉（行号仍不变）。
    assert_eq!(parsed.code, "\ntext(1)");

    let parsed = parse_codemode_source("// @options: {}\ntext(1)").unwrap();
    assert_eq!(parsed.code, "\ntext(1)");
    assert_eq!(parsed.options, options(None, None));
}

// it("only treats the first line as an options line")
#[test]
fn only_treats_the_first_line_as_an_options_line() {
    assert_eq!(
        parse_codemode_source("// @optionsx {}\ntext(1)")
            .unwrap()
            .options,
        options(None, None)
    );
    assert_eq!(
        parse_codemode_source("text(\"// @options: {}\")")
            .unwrap()
            .options,
        options(None, None)
    );
}

// it("rejects an options line that is not the first line")  — #10631
#[test]
fn rejects_an_options_line_that_is_not_the_first_line() {
    let cases: [(&str, usize); 4] = [
        (
            "// count things\n// @options: {\"timeout_ms\": 1000}\nwhile (true) {}",
            2,
        ),
        (
            "\n  // @options: {\"timeout_ms\": 1000}\nwhile (true) {}",
            2,
        ),
        ("text(1)\r\n// @options: {\"timeout_ms\": 1}", 2),
        (
            "// @options: {\"timeout_ms\": 1}\ntext(1)\n// @options: {\"timeout_ms\": 2}",
            3,
        ),
    ];
    for (input, line) in cases {
        let error = parse_codemode_source(input).expect_err(input);
        assert!(matches!(error, CodemodeSourceError { .. }));
        assert_eq!(
            error.to_string(),
            format!(
                "The // @options: line must be the first line of the input; found one on line {line}"
            ),
            "{input}"
        );
    }
}

// it("rejects empty input and invalid options")
#[test]
fn rejects_empty_input_and_invalid_options() {
    let cases: [(&str, &str); 10] = [
        ("", "Expected JavaScript source text (non-empty)"),
        ("  \n", "Expected JavaScript source text (non-empty)"),
        (
            "// @options:\ntext(1)",
            "@options must be a JSON object with supported fields",
        ),
        (
            "// @options: {timeout_ms: 1}\ntext(1)",
            "@options must be valid JSON with supported fields",
        ),
        (
            "// @options: [1]\ntext(1)",
            "@options must be a JSON object with supported fields",
        ),
        (
            "// @options: {\"yield\": 1}\ntext(1)",
            "@options only supports `max_output_tokens` and `timeout_ms`; got `yield`",
        ),
        (
            "// @options: {\"max_output_tokens\": 1.5}\ntext(1)",
            "@options field `max_output_tokens` must be a non-negative safe integer",
        ),
        (
            "// @options: {\"timeout_ms\": 0}\ntext(1)",
            "@options field `timeout_ms` must be a positive integer",
        ),
        (
            "// @options: {\"timeout_ms\": 1}",
            "The @options line must be followed by JavaScript source on subsequent lines",
        ),
        (
            "// @options: {\"timeout_ms\": 1}\n  \n",
            "The @options line must be followed by JavaScript source on subsequent lines",
        ),
    ];
    for (input, message) in cases {
        let error = parse_codemode_source(input).expect_err(input);
        assert!(
            error.to_string().contains(message),
            "{input:?}: expected {message:?} in {:?}",
            error.to_string()
        );
    }
}

// 额外：逐字锁定错误类型名与导出常量（TS 的 CodemodeSourceError.name / 两个常量）。
#[test]
fn exports_match_typescript() {
    assert_eq!(CODEMODE_OPTIONS_PREFIX, "// @options:");
    assert_eq!(CODEMODE_SOURCE_GRAMMAR.len(), 184);
    assert!(CODEMODE_SOURCE_GRAMMAR.starts_with("\nstart: options_source | plain_source\n"));
    let error = CodemodeSourceError::new("x");
    assert_eq!(error.name(), "CodemodeSourceError");
    assert_eq!(error.to_string(), "x");
}

// ---------------------------------------------------------------------------
// 黄金用例（在 TS 参照实现上生成；见文件头说明）
// ---------------------------------------------------------------------------

/// `parseCodemodeSource` 的黄金用例：覆盖 spec §2.2.2 的每条分支，包括几个只靠读代码不
/// 明显的点——U+FEFF/NBSP/U+3000 是 JS 空白、U+0085 不是；CRLF 的 options 行会丢掉前导 CR；
/// `\r` 单独出现不算换行；`1e400`（Infinity）不是安全整数；`null` 值不是 `undefined`；
/// 未知键先于字段检查报错，且只报 JSON 插入序里的第一个。
///
/// 生成脚本（一次性，结果已提交）：
/// `/home/ctyun/kiana-work/scratch/kiana-codemode__core/gen_source_cases.mjs`。
mod source_golden {
    use kiana_codemode::source::{CodemodeSourceOptions, parse_codemode_source};
    use serde_json::Value;

    /// 把 fixture 的 `options` 对象（camelCase）转成 Rust 结构体。
    fn expected_options(value: &Value) -> CodemodeSourceOptions {
        CodemodeSourceOptions {
            max_output_tokens: value.get("maxOutputTokens").and_then(Value::as_f64),
            timeout_ms: value.get("timeoutMs").and_then(Value::as_f64),
        }
    }

    #[test]
    fn golden_cases_match_the_typescript_reference() {
        let fixture: Value = serde_json::from_str(include_str!("fixtures/source_cases.json"))
            .expect("fixture parses");
        let cases = fixture.as_array().expect("fixture is an array");
        assert!(cases.len() > 30, "fixture should cover many cases");
        for case in cases {
            let input = case["input"].as_str().expect("input is a string");
            let outcome = parse_codemode_source(input);
            match case["ok"].as_bool().expect("ok flag") {
                true => {
                    let parsed = outcome.unwrap_or_else(|error| {
                        panic!("{input:?} should parse, got error {error:?}")
                    });
                    assert_eq!(
                        parsed.code,
                        case["code"].as_str().unwrap(),
                        "code mismatch for {input:?}"
                    );
                    assert_eq!(
                        parsed.options,
                        expected_options(&case["options"]),
                        "options mismatch for {input:?}"
                    );
                }
                false => {
                    let error = outcome.expect_err(&format!("{input:?} should fail"));
                    assert_eq!(
                        error.message,
                        case["message"].as_str().unwrap(),
                        "message mismatch for {input:?}"
                    );
                    assert_eq!(case["name"].as_str(), Some("CodemodeSourceError"));
                }
            }
        }
    }
}

/// `@options` 里含**未配对代理项**的键：TS/V8 把它原样放进错误消息（`got `a\ud800``），
/// Rust 的 `String` 装不下孤立代理项，`kiana_json::parse` 按 `ARCHITECTURE.md` §8 第 31 条把它
/// 换成 U+FFFD。这是 recorded forced deviation；此处锁定 U+FFFD 的实际行为，避免它被无意改回
/// 或继续漂移。
///
/// TS 侧实测（node 26，`parseCodemodeSource('// @options: {"a\\ud800": 1}\ntext(1)')`）：
/// `@options only supports `max_output_tokens` and `timeout_ms`; got `a\ud800``
/// （消息里是 U+D800 单个 code unit）。
mod unpaired_surrogate {
    use kiana_codemode::source::parse_codemode_source;

    #[test]
    fn unknown_key_with_unpaired_surrogate_becomes_replacement_character() {
        // 输入本身是合法 UTF-8：代理项以 `\uXXXX` 转义出现在 JSON 文本里，由 `JSON.parse`
        // 产生孤立代理项（Rust 侧由 `kiana_json::parse` 换成 U+FFFD）。
        let input = "// @options: {\"a\\ud800\": 1}\ntext(1)";
        let error = parse_codemode_source(input).expect_err("unknown key");
        assert_eq!(
            error.message,
            "@options only supports `max_output_tokens` and `timeout_ms`; got `a\u{FFFD}`"
        );
        // 低代理项（`\udc00`）同样。
        let input = "// @options: {\"a\\udc00b\": 1}\ntext(1)";
        let error = parse_codemode_source(input).expect_err("unknown key");
        assert_eq!(
            error.message,
            "@options only supports `max_output_tokens` and `timeout_ms`; got `a\u{FFFD}b`"
        );
    }
}

/// `toCodemodeIdentifier` 的黄金用例：逐码点变成一个 `_`（`😀` 一个码点、两个 UTF-16
/// 码元 → `"_"`；`日本語` → `"___"`），以及首字符规则。
///
/// 生成脚本：`/home/ctyun/kiana-work/scratch/kiana-codemode__core/gen_identifier_cases.mjs`。
mod identifier_golden {
    use kiana_codemode::identifier::to_codemode_identifier;
    use serde_json::Value;

    #[test]
    fn golden_cases_match_the_typescript_reference() {
        let fixture: Value = serde_json::from_str(include_str!("fixtures/identifier_cases.json"))
            .expect("fixture parses");
        let cases = fixture.as_array().expect("fixture is an array");
        assert!(cases.len() > 30);
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let expected = case["identifier"].as_str().unwrap();
            assert_eq!(
                to_codemode_identifier(name),
                expected,
                "identifier mismatch for {name:?}"
            );
        }
    }
}
