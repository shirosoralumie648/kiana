//! `packages/ai/test/validation.test.ts` 的移植 + TypeBox 错误目录的黄金断言。
//!
//! 期望的错误列表来自 `tests/fixtures/utils/validation.json`，由 TS 参照实现（`upstream-reference`）用
//! typebox 1.3.27 的 `Compile(schema).Check/Errors` 生成；生成脚本见
//! `/home/ctyun/kiana-work/scratch/kiana-ai__utils/gen_validation_golden.ts`。

use kiana_ai::error::AiErrorKind;
use kiana_ai::types::{JsonValue, Tool, ToolCall};
use kiana_ai::utils::validation::{
    check_json_schema, collect_json_schema_errors, validate_tool_arguments,
};
use serde_json::{Value, json};

fn load_fixture() -> Value {
    let path = format!(
        "{}/tests/fixtures/utils/validation.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture readable"))
        .expect("fixture is valid JSON")
}

/// fixture 里的一条用例。
#[test]
fn matches_the_typescript_validator_error_catalogue() {
    let fixture = load_fixture();
    let cases = fixture["cases"].as_array().expect("cases");
    assert_eq!(cases.len(), 95, "fixture case count");
    let mut checked = 0usize;
    for case in cases {
        let label = case["label"].as_str().expect("label");
        let schema = &case["schema"];
        let value = &case["value"];
        let expected_check = case["check"].as_bool().expect("check");
        let actual_check = check_json_schema(schema, value);
        assert_eq!(actual_check, expected_check, "check mismatch for {label}");
        let actual_errors: Vec<Value> = collect_json_schema_errors(schema, value)
            .into_iter()
            .map(|error| {
                json!({
                    "keyword": error.keyword,
                    "instancePath": error.instance_path,
                    "message": error.message,
                    "params": error.params,
                })
            })
            .collect();
        let expected_errors = case["errors"].as_array().expect("errors");
        assert_eq!(
            &actual_errors, expected_errors,
            "errors mismatch for {label}"
        );
        checked += expected_errors.len();
    }
    assert_eq!(checked, 117, "fixture error count");
}

// ---------------------------------------------------------------------------
// validateToolArguments
// ---------------------------------------------------------------------------

fn tool_call_with_plain_schema(schema: JsonValue, value: JsonValue) -> (Tool, ToolCall) {
    let tool = Tool {
        name: "echo".into(),
        description: "Echo tool".into(),
        parameters: json!({
            "type": "object",
            "properties": { "value": schema },
            "required": ["value"],
        }),
        constrained_sampling: None,
    };
    let tool_call = ToolCall {
        name: "echo".into(),
        id: "tool-1".into(),
        arguments: indexmap::IndexMap::from([("value".to_string(), value)]),
        ..Default::default()
    };
    (tool, tool_call)
}

fn tool(parameters: JsonValue) -> Tool {
    Tool {
        name: "echo".into(),
        description: "Echo tool".into(),
        parameters,
        constrained_sampling: None,
    }
}

fn call(arguments: JsonValue) -> ToolCall {
    let mut tool_call = ToolCall {
        name: "echo".into(),
        id: "tool-1".into(),
        ..Default::default()
    };
    if let Value::Object(map) = arguments {
        tool_call.arguments = map.into_iter().collect();
    }
    tool_call
}

fn validated(tool: &Tool, tool_call: &ToolCall) -> JsonValue {
    validate_tool_arguments(tool, tool_call).expect("valid arguments")
}

#[test]
fn still_validates_when_function_constructor_is_unavailable() {
    // TS 里这条用 CSP 屏蔽 `new Function`；Rust 侧没有 JIT，但同样要完成 "42" -> 42 的强制转换。
    let tool = tool(json!({
        "type": "object",
        "required": ["count"],
        "properties": { "count": { "type": "number" } },
    }));
    let tool_call = call(json!({ "count": "42" }));
    assert_eq!(validated(&tool, &tool_call), json!({ "count": 42 }));
}

/// TS: "coerces serialized plain JSON schemas with AJV-compatible primitive rules"。
/// 普通 JSON schema（没有 `~kind`）在 TS 里来自 `JSON.parse`（例如 MCP 的 `inputSchema`），
/// `Value.Convert` 对它整体空操作，只有 `coerceWithJsonSchema` 生效。
#[test]
fn coerces_serialized_plain_json_schemas_with_ajv_compatible_primitive_rules() {
    let passing_cases: Vec<(JsonValue, JsonValue, JsonValue)> = vec![
        (json!({"type": "number"}), json!("42"), json!(42)),
        (json!({"type": "number"}), json!(true), json!(1)),
        (json!({"type": "number"}), Value::Null, json!(0)),
        (json!({"type": "integer"}), json!("42"), json!(42)),
        (json!({"type": "boolean"}), json!("true"), json!(true)),
        (json!({"type": "boolean"}), json!("false"), json!(false)),
        (json!({"type": "boolean"}), json!(1), json!(true)),
        (json!({"type": "boolean"}), json!(0), json!(false)),
        // 普通 schema 的 `coercePrimitiveByType("string")` 把 null 转成 ""；TypeBox 的
        // `TryString(null) -> "null"` 只发生在活节点上（见 `converts_live_typebox_nodes...`）。
        (json!({"type": "string"}), Value::Null, json!("")),
        (json!({"type": "string"}), json!(true), json!("true")),
        (json!({"type": "null"}), json!(""), Value::Null),
        (json!({"type": "null"}), json!(0), Value::Null),
        (json!({"type": "null"}), json!(false), Value::Null),
        (
            json!({"type": ["number", "string"]}),
            json!("1"),
            json!("1"),
        ),
        (json!({"type": ["boolean", "number"]}), json!("1"), json!(1)),
    ];
    for (schema, input, expected) in passing_cases {
        let (tool, tool_call) = tool_call_with_plain_schema(schema.clone(), input.clone());
        let result = validate_tool_arguments(&tool, &tool_call)
            .unwrap_or_else(|error| panic!("{schema} / {input} 应该通过：{error}"));
        assert_eq!(
            result,
            json!({ "value": expected }),
            "schema {schema} with input {input}"
        );
    }
}

#[test]
fn throws_for_invalid_regular_expressions_like_compile() {
    // TS 的 `getValidator(parameters)` 在这里 `Compile`，非法 `pattern` 直接抛 V8 的
    // `SyntaxError`；Rust 返回同文本的错误而不是把正则错误降级成“永不匹配”。
    let invalid_pattern_tool = tool(json!({
        "type": "object",
        "required": ["value"],
        "properties": { "value": { "type": "string", "pattern": "[invalid" } },
    }));
    let error = validate_tool_arguments(&invalid_pattern_tool, &call(json!({ "value": "x" })))
        .expect_err("invalid pattern");
    assert_eq!(error.kind, AiErrorKind::Request);
    assert_eq!(
        error.message,
        "Invalid regular expression: /[invalid/u: Unterminated character class"
    );

    // `patternProperties` 的键同样会被编译。
    let pattern_properties = tool(json!({
        "type": "object",
        "patternProperties": { "(?<broken": { "type": "number" } },
    }));
    let error = validate_tool_arguments(&pattern_properties, &call(json!({ "value": 1 })))
        .expect_err("invalid patternProperties key");
    assert_eq!(error.kind, AiErrorKind::Request);
    assert!(
        error
            .message
            .starts_with("Invalid regular expression: /(?<broken/u: "),
        "unexpected message: {}",
        error.message
    );
}

#[test]
fn accepts_ipv_future_literals_in_iri_like_typebox() {
    // TypeBox 在 `URL.canParse` 前把 IPvFuture 字面量替换成 `[::1]`（RFC 3987 允许，WHATWG
    // URL 拒绝）；`iri-reference` 不做这一步。
    let iri = json!({ "type": "string", "format": "iri" });
    assert!(check_json_schema(&iri, &json!("http://[v1.fe]/")));
    assert!(check_json_schema(&iri, &json!("http://[V1.ab]/path?q=1")));
    // 没有 `.` 的不是 IPvFuture。
    assert!(!check_json_schema(&iri, &json!("http://[v1]/")));
    // 达到 2048 个 UTF-16 码元后不再替换（TypeBox 的 review-optimization）。
    let short = format!("http://[v1.fe]/{}", "a".repeat(2000));
    assert!(short.encode_utf16().count() < 2048);
    assert!(check_json_schema(&iri, &json!(short)));
    let long = format!("http://[v1.fe]/{}", "a".repeat(2048));
    assert!(long.encode_utf16().count() >= 2048);
    assert!(!check_json_schema(&iri, &json!(long)));

    let iri_reference = json!({ "type": "string", "format": "iri-reference" });
    assert!(check_json_schema(&iri_reference, &json!("/relative/path")));
    assert!(!check_json_schema(
        &iri_reference,
        &json!("http://[v1.fe]/")
    ));
}

#[test]
fn matches_patterns_with_ecmascript_semantics() {
    let pattern_tool = |pattern: &str| {
        tool(json!({
            "type": "object",
            "required": ["value"],
            "properties": { "value": { "type": "string", "pattern": pattern } },
        }))
    };

    // JS `\d` 只认 ASCII 数字（`fancy_regex` 的 Unicode 字符类会误放行阿拉伯-印度数字）。
    let digits = pattern_tool("^\\d+$");
    assert!(validate_tool_arguments(&digits, &call(json!({ "value": "123" }))).is_ok());
    assert!(validate_tool_arguments(&digits, &call(json!({ "value": "١٢٣" }))).is_err());

    // JS `$`（无 `m`）只匹配输入末尾，不在末尾换行之前。
    let line = pattern_tool("^a$");
    assert!(validate_tool_arguments(&line, &call(json!({ "value": "a" }))).is_ok());
    assert!(validate_tool_arguments(&line, &call(json!({ "value": "a\n" }))).is_err());

    // JS `.` 不匹配换行符。
    let any = pattern_tool("^.$");
    assert!(validate_tool_arguments(&any, &call(json!({ "value": "x" }))).is_ok());
    assert!(validate_tool_arguments(&any, &call(json!({ "value": "\n" }))).is_err());

    // JS `\s` 包含 U+FEFF（BOM）；`regex`/`fancy_regex` 的 Unicode 空白不含它。
    let space = pattern_tool("^\\s$");
    assert!(validate_tool_arguments(&space, &call(json!({ "value": "\u{FEFF}" }))).is_ok());
}

#[test]
fn treats_null_as_omission_for_optional_non_nullable_properties() {
    let tool = tool(json!({
        "type": "object",
        "required": ["path"],
        "properties": {
            "path": { "type": "string" },
            "offset": { "type": "number" },
            "nullable": { "anyOf": [{ "type": "string" }, { "type": "null" }] },
            "metadata": {
                "type": "object",
                "properties": { "enabled": { "type": "boolean" } },
            },
        },
    }));
    let tool_call = call(json!({
        "path": "file.txt",
        "offset": null,
        "nullable": null,
        "metadata": { "enabled": null },
    }));
    assert_eq!(
        validated(&tool, &tool_call),
        json!({ "path": "file.txt", "nullable": null, "metadata": {} })
    );
}

#[test]
fn preserves_optional_nulls_whose_referenced_schema_is_nullable() {
    let tool = tool(json!({
        "type": "object",
        "properties": { "value": { "$ref": "#/$defs/value" } },
        "$defs": { "value": { "anyOf": [{ "type": "number" }, { "type": "null" }] } },
    }));
    let tool_call = call(json!({ "value": null }));
    assert_eq!(validated(&tool, &tool_call), json!({ "value": null }));
}

#[test]
fn preserves_a_value_that_already_matches_a_nullable_union_arm() {
    let tool = tool(json!({
        "type": "object",
        "required": ["value"],
        "properties": { "value": { "anyOf": [{ "type": "number" }, { "type": "null" }] } },
    }));
    let tool_call = call(json!({ "value": null }));
    assert_eq!(validated(&tool, &tool_call), json!({ "value": null }));
}

#[test]
fn preserves_a_value_that_already_matches_a_oneof_nullable_union_arm() {
    let (tool, tool_call) = tool_call_with_plain_schema(
        json!({ "oneOf": [{ "type": "number" }, { "type": "null" }] }),
        Value::Null,
    );
    assert_eq!(validated(&tool, &tool_call), json!({ "value": null }));
}

#[test]
fn still_coerces_nullable_unions_when_the_original_value_does_not_match_any_arm() {
    let (tool, tool_call) = tool_call_with_plain_schema(
        json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] }),
        json!("42"),
    );
    assert_eq!(validated(&tool, &tool_call), json!({ "value": 42 }));
}

#[test]
fn accepts_null_for_nullable_array_schemas_with_items() {
    let (tool, tool_call) = tool_call_with_plain_schema(
        json!({ "type": ["array", "null"], "items": { "type": "string" } }),
        Value::Null,
    );
    // TS 这条还断言 `new Function(Compile(...).Code())` 的生成校验器；Rust 侧只需校验通过。
    let arguments = Value::Object(
        tool_call
            .arguments
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );
    assert!(check_json_schema(&tool.parameters, &arguments));
    assert_eq!(validated(&tool, &tool_call), json!({ "value": null }));
}

/// 活 TypeBox 节点用显式 `"~kind"` 键标记（TS 里是**不可枚举**属性，`JSON.stringify` 会丢弃；
/// Rust 侧用普通键表示，与 `kiana-chord::schema::mark_live_schema` 的约定一致）。只有带 `~kind`
/// 的节点参与 `Value.Convert`，转换结果经父对象的属性赋值保留。
#[test]
fn converts_live_typebox_nodes_before_validation() {
    let passing_cases: Vec<(JsonValue, JsonValue, JsonValue)> = vec![
        (
            json!({"~kind": "Number", "type": "number"}),
            json!(""),
            json!(0),
        ),
        (
            json!({"~kind": "Number", "type": "number"}),
            json!("true"),
            json!(1),
        ),
        (
            json!({"~kind": "Number", "type": "number"}),
            json!("123n"),
            json!(123),
        ),
        (
            json!({"~kind": "Number", "type": "number"}),
            json!("1.9"),
            json!(1.9),
        ),
        (
            json!({"~kind": "String", "type": "string"}),
            Value::Null,
            json!("null"),
        ),
        (
            json!({"~kind": "String", "type": "string"}),
            json!(12),
            json!("12"),
        ),
        (
            json!({"~kind": "Boolean", "type": "boolean"}),
            json!("TRUE"),
            json!(true),
        ),
        (
            json!({"~kind": "Boolean", "type": "boolean"}),
            json!("False"),
            json!(false),
        ),
        (
            json!({"~kind": "Integer", "type": "integer"}),
            json!("42.1"),
            json!(42),
        ),
        (
            json!({"~kind": "Null", "type": "null"}),
            json!("null"),
            Value::Null,
        ),
        (
            json!({"~kind": "Null", "type": "null"}),
            json!("undefined"),
            Value::Null,
        ),
        (
            json!({"~kind": "Union", "anyOf": [
                {"~kind": "Number", "type": "number"},
                {"~kind": "Null", "type": "null"},
            ]}),
            json!(""),
            json!(0),
        ),
        (
            json!({"~kind": "Array", "type": "array", "items": {"~kind": "Number", "type": "number"}}),
            json!("42"),
            json!([42]),
        ),
        (
            json!({"~kind": "Literal", "type": "number", "const": 1}),
            json!("1"),
            json!(1),
        ),
        (
            json!({"~kind": "Enum", "enum": [1, "a"]}),
            json!("1"),
            json!(1),
        ),
    ];
    for (property_schema, input, expected) in passing_cases {
        let tool = tool(json!({
            "~kind": "Object",
            "type": "object",
            "properties": { "value": property_schema },
            "required": ["value"],
        }));
        let tool_call = call(json!({ "value": input }));
        let result = validate_tool_arguments(&tool, &tool_call)
            .unwrap_or_else(|error| panic!("live {} / {input} 应该通过：{error}", property_schema));
        assert_eq!(
            result,
            json!({ "value": expected }),
            "live schema with input {input}"
        );
    }
}

#[test]
fn rejects_invalid_coercions_for_serialized_json_schemas() {
    let failing_cases: Vec<(JsonValue, JsonValue)> = vec![
        // TS: "rejects invalid coercions for serialized plain JSON schemas"
        (json!({"type": "boolean"}), json!("1")),
        (json!({"type": "boolean"}), json!("0")),
        (json!({"type": "null"}), json!("null")),
        (json!({"type": "integer"}), json!("42.1")),
        // `Value.Convert` 不作用于普通 JSON schema（findings：MCP 的 inputSchema）：
        // 整数 2.5 不截断、对象不包装成数组、null 不变成 "null"、枚举不做字面量转换。
        (json!({"type": "integer"}), json!(2.5)),
        (json!({"type": "array"}), json!({"a": "5"})),
        (json!({"type": "number"}), json!("")),
        (json!({"type": "number"}), json!("TRUE")),
        (json!({"type": "boolean"}), json!("TRUE")),
        (json!({"enum": [1, "a"]}), json!("1")),
        // 其余错误输入
        (json!({"type": "boolean"}), json!("yes")),
        (json!({"type": "null"}), json!("none")),
        (json!({"type": "integer"}), json!("abc")),
        (json!({"type": "number"}), json!("abc")),
        (json!({"type": "number", "const": 2}), json!("1")),
    ];
    for (schema, input) in failing_cases {
        let (tool, tool_call) = tool_call_with_plain_schema(schema.clone(), input.clone());
        let error = validate_tool_arguments(&tool, &tool_call)
            .expect_err(&format!("{schema} / {input} 应该被拒绝"));
        assert!(
            error.message.starts_with("Validation failed"),
            "unexpected message: {}",
            error.message
        );
    }
}

#[test]
fn reports_malformed_arguments_json_instead_of_validating_empty_arguments() {
    // #10897
    let tool = tool(json!({
        "type": "object",
        "properties": { "value": { "type": "string" } },
    }));
    let mut tool_call = call(json!({}));
    tool_call.arguments_error = Some("Unexpected token".into());
    let error = validate_tool_arguments(&tool, &tool_call).expect_err("argumentsError");
    assert_eq!(
        error.message,
        "Invalid arguments for tool \"echo\": Unexpected token"
    );
}

#[test]
fn formats_the_validation_failure_message_verbatim() {
    let tool = tool(json!({
        "type": "object",
        "required": ["count"],
        "properties": { "count": { "type": "number" } },
    }));
    let tool_call = call(json!({ "count": "abc" }));
    let error = validate_tool_arguments(&tool, &tool_call).expect_err("invalid");
    assert_eq!(
        error.message,
        "Validation failed for tool \"echo\":\n  - count: must be number\n\nReceived arguments:\n{\n  \"count\": \"abc\"\n}"
    );
}

#[test]
fn reports_missing_tools_by_name() {
    let tools: Vec<Tool> = vec![tool(json!({"type": "object"}))];
    let tool_call = call(json!({}));
    let mut tool_call = tool_call;
    tool_call.name = "missing".into();
    let error = kiana_ai::utils::validation::validate_tool_call(&tools, &tool_call)
        .expect_err("missing tool");
    assert_eq!(error.message, "Tool \"missing\" not found");
}

// ---------------------------------------------------------------------------
// TypeBox `format` 关键字（黄金结果由 upstream-reference 的 `Compile(...).Check(v)` 生成）
// ---------------------------------------------------------------------------

fn load_json_fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/utils/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture readable"))
        .expect("fixture is valid JSON")
}

fn format_accepts(format: &str, value: &str) -> bool {
    check_json_schema(
        &json!({ "type": "string", "format": format }),
        &Value::String(value.to_string()),
    )
}

/// 每个 format 的输入矩阵逐条对齐 TypeBox（findings: uri / uri-reference / idn-*）。
#[test]
fn matches_typebox_format_checks() {
    let fixture = load_json_fixture("formats.json");
    let formats = fixture.as_object().expect("formats object");
    assert!(formats.len() >= 10);
    let mut checked = 0usize;
    for (format, cases) in formats {
        let cases = cases.as_array().expect("cases");
        for case in cases {
            let value = case["value"].as_str().expect("value");
            let expected = case["check"].as_bool().expect("check");
            assert_eq!(
                format_accepts(format, value),
                expected,
                "format {format} value {value:?}"
            );
            checked += 1;
        }
    }
    assert!(checked > 150, "fixture size {checked}");
}

/// `format: "regex"` 必须是 `new RegExp(value, "u")` 的语法校验（不是 Rust 正则语法）。
#[test]
fn matches_javascript_regex_syntax() {
    let fixture = load_json_fixture("js_regex.json");
    let cases = fixture.as_object().expect("patterns object");
    assert!(cases.len() > 100);
    for (pattern, expected) in cases {
        let expected = expected["check"].as_bool().expect("check");
        assert_eq!(
            format_accepts("regex", pattern),
            expected,
            "pattern {pattern:?}"
        );
    }
}

/// 回归（critical）：`format: "hostname"` 的字节切片曾在中文字符上 panic。
#[test]
fn hostname_rejects_multibyte_labels_without_panicking() {
    // 原实现的字节切片 `&label[2..4]` 在多字节字符中间 panic；punycode 解码又把字节
    // 下标用在 UTF-16 数组上。两者都必须返回 false 而不是 panic。
    for value in [
        "例え",
        "日本語",
        "xn--例例例-abc",
        "xn--例例例例例例-abc",
        "例え.テスト",
    ] {
        assert!(!format_accepts("hostname", value), "hostname {value:?}");
    }
    // 纯 IDN 标签是合法的；畸形的 puny 标签不是。
    assert!(format_accepts("idn-hostname", "例え.テスト"));
    for value in ["xn--例例例-abc", "xn--例例例例例例-abc"] {
        assert!(
            !format_accepts("idn-hostname", value),
            "idn-hostname {value:?}"
        );
    }
}

/// 回归（major）：idn-hostname / idn-email 先做 NFC，再按 UTF-16 码元计长度。
#[test]
fn idn_formats_normalize_nfc_and_count_utf16_units() {
    // `e` + U+0301 组合后是合法标签。
    let decomposed = "e\u{0301}";
    assert!(format_accepts("idn-email", &format!("a@{decomposed}.com")));
    assert!(format_accepts(
        "idn-hostname",
        &format!("{}.com", decomposed.repeat(32))
    ));
    // 每 2 个 UTF-16 码元的 astral 字符：253 码元上限。
    assert!(!format_accepts(
        "idn-hostname",
        &format!("{}.com", "😀".repeat(239))
    ));
    assert!(!format_accepts(
        "idn-hostname",
        &format!("{}.com", "😀".repeat(149))
    ));
}

// ---------------------------------------------------------------------------
// 回归：unevaluatedProperties/unevaluatedItems 的 TypeBox 求值上下文语义
// ---------------------------------------------------------------------------

/// 黄金夹具由 `typebox 1.3.27` 的 `Compile(schema).Check/Errors` 生成（生成脚本
/// `/home/ctyun/kiana-work/scratch/gate__kiana-ai/gen_validate_golden.mjs`），覆盖
/// `properties`/`patternProperties`/`additionalProperties`/`allOf`/`anyOf`/`oneOf`/`if`/`not`/
/// `$ref`/`contains`/`items`/`prefixItems`/`additionalItems` 对 `unevaluated*` 的共享求值上下文，
/// 以及 `minContains: 0` 与 `maxContains` 组合、嵌套 `$defs` 的 `$ref` 解析。
#[test]
fn matches_typescript_unevaluated_keyword_semantics() {
    let path = format!(
        "{}/tests/fixtures/utils/unevaluated.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let fixture: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture readable"))
            .expect("fixture is valid JSON");
    let cases = fixture["cases"].as_array().expect("cases");
    assert_eq!(cases.len(), 123, "fixture case count");
    for case in cases {
        let label = case["label"].as_str().expect("label");
        let schema = &case["schema"];
        let value = &case["value"];
        let expected_check = case["check"].as_bool().expect("check");
        assert_eq!(
            check_json_schema(schema, value),
            expected_check,
            "check mismatch for {label}"
        );
        let actual_errors: Vec<Value> = collect_json_schema_errors(schema, value)
            .into_iter()
            .map(|error| {
                json!({
                    "keyword": error.keyword,
                    "instancePath": error.instance_path,
                    "message": error.message,
                    "params": error.params,
                })
            })
            .collect();
        assert_eq!(
            &actual_errors,
            case["errors"].as_array().expect("errors"),
            "errors mismatch for {label}"
        );
    }
}

/// Seeded random corpus (400 generated schema/value pairs, 369 compile+validate
/// successfully in TypeBox 1.3.27) covering the cross-product of properties,
/// patternProperties, additionalProperties, unevaluated*, items/prefixItems/
/// additionalItems, contains, allOf/anyOf/oneOf, if, not and nested $defs.
#[test]
fn matches_typescript_seeded_fuzz_corpus() {
    let path = format!(
        "{}/tests/fixtures/utils/fuzz.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let fixture: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture readable"))
            .expect("fixture is valid JSON");
    let cases = fixture["cases"].as_array().expect("cases");
    assert_eq!(cases.len(), 398, "fixture case count");
    for case in cases {
        let label = case["label"].as_str().expect("label");
        let schema = &case["schema"];
        let value = &case["value"];
        let expected_check = case["check"].as_bool().expect("check");
        assert_eq!(
            check_json_schema(schema, value),
            expected_check,
            "check mismatch for {label}"
        );
        let actual_errors: Vec<Value> = collect_json_schema_errors(schema, value)
            .into_iter()
            .map(|error| {
                json!({
                    "keyword": error.keyword,
                    "instancePath": error.instance_path,
                    "message": error.message,
                    "params": error.params,
                })
            })
            .collect();
        assert_eq!(
            &actual_errors,
            case["errors"].as_array().expect("errors"),
            "errors mismatch for {label}"
        );
    }
}

/// `validateToolArguments` 的端到端黄金用例：直接在 Node 类型剥离下运行 TS 参照实现
/// （`packages/ai/src/utils/validation.ts` + TypeBox 1.3.27），覆盖普通 JSON schema
/// （只有 `coerceWithJsonSchema`）与活 TypeBox schema（`~kind` 标记，`Value.Convert` 生效）
/// 的强制转换矩阵、根级返回值丢弃语义与错误文本。活节点的 `~kind` 在 TS 里是不可枚举属性，
/// 生成器用 `Object.getOwnPropertyNames` 保留它，Rust 侧因此可以看到显式标记。
#[test]
fn matches_typescript_validate_tool_arguments_coercion_matrix() {
    let fixture = load_json_fixture("validate_convert.json");
    let cases = fixture["cases"].as_array().expect("cases");
    assert_eq!(cases.len(), 63, "fixture case count");
    for case in cases {
        let label = case["label"].as_str().expect("label");
        let tool = tool(case["schema"].clone());
        let tool_call = call(case["arguments"].clone());
        if let Some(expected) = case.get("ok") {
            let result = validate_tool_arguments(&tool, &tool_call)
                .unwrap_or_else(|error| panic!("{label}: 应该通过，实际错误：{}", error.message));
            assert_eq!(&result, expected, "result mismatch for {label}");
        } else {
            let expected = case["error"].as_str().expect("error");
            let error = validate_tool_arguments(&tool, &tool_call)
                .expect_err(&format!("{label}: 应该被拒绝"));
            assert_eq!(error.message, expected, "error mismatch for {label}");
        }
    }
}

/// findings 回归：TS 的 `Value.Convert(tool.parameters, args)` 丢弃返回值，只保留原地写入。
/// 根级 Object/Record/Tuple 的属性/元素赋值会保留；根级 Array 的 `map` 结果被丢弃（元素内部的
/// 原地修改仍保留）；联合/枚举/字面量/原始类型的新值被丢弃；普通 JSON schema 整体空操作。
#[test]
fn value_convert_only_keeps_in_place_mutations() {
    use kiana_ai::utils::validation::value_convert;

    // 根级 Object：属性赋值保留。
    let schema = json!({"~kind": "Object", "type": "object", "properties": {
        "n": {"~kind": "Number", "type": "number"},
    }});
    let mut value = json!({"n": "5"});
    value_convert(&schema, &mut value);
    assert_eq!(value, json!({"n": 5}));

    // 根级 Tuple：元素赋值保留（FromTuple 直接写回同一个数组）。
    let schema = json!({"~kind": "Tuple", "type": "array", "items": [
        {"~kind": "Number", "type": "number"},
        {"~kind": "String", "type": "string"},
    ]});
    let mut value = json!(["1", 2]);
    value_convert(&schema, &mut value);
    assert_eq!(value, json!([1, "2"]));

    // 根级 Array：FromArray 的 `map` 生成新数组并被丢弃，元素赋值不保留。
    let schema =
        json!({"~kind": "Array", "type": "array", "items": {"~kind": "Number", "type": "number"}});
    let mut value = json!(["1", "2"]);
    value_convert(&schema, &mut value);
    assert_eq!(value, json!(["1", "2"]));

    // 根级 Array 的元素是对象时，元素内部的原地修改仍保留。
    let schema = json!({"~kind": "Array", "type": "array", "items": {
        "~kind": "Object", "type": "object",
        "properties": {"n": {"~kind": "Number", "type": "number"}},
    }});
    let mut value = json!([{"n": "5"}]);
    value_convert(&schema, &mut value);
    assert_eq!(value, json!([{"n": 5}]));

    // 根级 Array + 非数组值：TryArray 的包装数组被丢弃，原值不变。
    let schema =
        json!({"~kind": "Array", "type": "array", "items": {"~kind": "Number", "type": "number"}});
    let mut value = json!({"a": "5"});
    value_convert(&schema, &mut value);
    assert_eq!(value, json!({"a": "5"}));

    // 根级原始类型/联合：新值被丢弃。
    let mut value = json!("42");
    value_convert(&json!({"~kind": "Number", "type": "number"}), &mut value);
    assert_eq!(value, json!("42"));
    let mut value = json!("42");
    value_convert(
        &json!({"~kind": "Union", "anyOf": [
            {"~kind": "Number", "type": "number"},
            {"~kind": "Null", "type": "null"},
        ]}),
        &mut value,
    );
    assert_eq!(value, json!("42"));

    // 普通 JSON schema（无 `~kind`）：Value.Convert 是空操作。
    let mut value = json!({"n": "5"});
    value_convert(
        &json!({"type": "object", "properties": {"n": {"type": "number"}}}),
        &mut value,
    );
    assert_eq!(value, json!({"n": "5"}));
}

/// findings 回归：根级 `{"type": "array"}` 的对象参数在 TS 里不会被包装成数组——普通 schema
/// 只有 `coerceWithJsonSchema`，活节点的 `Value.Convert` 返回值又被丢弃。必须报
/// `root: must be array`。
#[test]
fn rejects_object_arguments_for_root_array_schemas() {
    let expected = "Validation failed for tool \"echo\":\n  - root: must be array\n\nReceived arguments:\n{\n  \"a\": 5\n}";

    let plain_tool = tool(json!({"type": "array"}));
    let error = validate_tool_arguments(&plain_tool, &call(json!({"a": 5}))).expect_err("plain");
    assert_eq!(error.message, expected);

    let live_tool = tool(json!({"~kind": "Array", "type": "array"}));
    let error = validate_tool_arguments(&live_tool, &call(json!({"a": 5}))).expect_err("live");
    assert_eq!(error.message, expected);
}
