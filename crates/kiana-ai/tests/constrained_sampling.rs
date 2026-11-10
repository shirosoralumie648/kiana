//! `constrained-sampling.test.ts` 的 Rust 移植（本单元负责的纯函数部分）。
//!
//! TS 测试里 `convertResponsesTools` / `convertResponsesMessages` / `processResponsesStream`
//! 属于 `openai-responses` 适配器单元，这里按同样的输入直接断言共享层的判定函数
//! （`resolveJsonSchemaStrictSampling` / `resolveGrammarConstrainedSampling` /
//! `makeStrictJsonSchema` / grammar 输入拼装），即那些适配器实际调用的函数。
//!
//! 未移植：`streams custom tool input in time and memory linear in its length` 里的
//! `allocatedBytes` 比值断言（它驱动的是 `processResponsesStream`，本单元没有该实现）；
//! 这里保留同等规模的输入做**正确性**检查。

use kiana_ai::api::constrained_sampling::{
    GrammarFormat, GrammarToolInputJsonBuffer, append_grammar_tool_input_json_delta,
    close_grammar_tool_input_json, create_grammar_tool_input_properties, make_strict_json_schema,
    resolve_grammar_constrained_sampling, resolve_json_schema_strict_sampling,
};
use kiana_ai::types::{
    ConstrainedSampling, ConstrainedSamplingConfig, GrammarVariants, StrictMode, Tool,
};
use serde_json::{Value, json};

fn make_tool(overrides: Value) -> Tool {
    let mut base = json!({
        "name": "sample_tool",
        "description": "Sample tool",
        "parameters": {
            "type": "object",
            "required": ["payload"],
            "properties": { "payload": { "type": "string" } },
            "additionalProperties": false
        }
    });
    if let (Some(base), Some(overrides)) = (base.as_object_mut(), overrides.as_object()) {
        for (key, value) in overrides {
            base.insert(key.clone(), value.clone());
        }
    }
    serde_json::from_value(base).expect("tool")
}

fn tool_with_sampling(sampling: Value) -> Tool {
    make_tool(json!({ "constrainedSampling": sampling }))
}

#[test]
fn converts_supported_constraints_and_falls_back_when_unsupported() {
    // `strict: "prefer"` + 支持的 schema → strict。
    assert_eq!(
        resolve_json_schema_strict_sampling(
            &tool_with_sampling(json!({ "type": "json_schema", "strict": "prefer" })),
            true,
            None,
        )
        .expect("resolves"),
        Some(true)
    );

    // `supportsStrictMode: false` + `require` → 报错（逐字）。
    let error = resolve_json_schema_strict_sampling(
        &tool_with_sampling(json!({ "type": "json_schema", "strict": "require" })),
        false,
        None,
    )
    .expect_err("require without strict support");
    assert!(
        error
            .message
            .starts_with("Tool \"sample_tool\" requires JSON-schema constrained sampling")
    );

    // grammar + lark → custom grammar。
    let grammar_tool = tool_with_sampling(
        json!({ "type": "grammar", "variants": { "openai_lark": "start: /[a-z]+/" } }),
    );
    let grammar = resolve_grammar_constrained_sampling(&grammar_tool, true)
        .expect("resolves")
        .expect("grammar");
    assert_eq!(grammar.format, GrammarFormat::Lark);
    assert_eq!(grammar.definition, "start: /[a-z]+/");
    assert_eq!(grammar.input_property, "payload");

    // 无可用 grammar 变体 → 报错（逐字）。
    let error = resolve_grammar_constrained_sampling(
        &tool_with_sampling(json!({ "type": "grammar", "variants": {} })),
        true,
    )
    .expect_err("no grammar variant");
    assert_eq!(
        error.message,
        "Tool \"sample_tool\" cannot use grammar constrained sampling: no supported grammar variant was provided."
    );

    // 不支持 grammar 工具的 provider → 回落到普通 function（`None`）。
    assert_eq!(
        resolve_grammar_constrained_sampling(&grammar_tool, false).expect("resolves"),
        None
    );

    // `constrainedSampling: false` 与「没有配置」等价。
    let disabled = make_tool(json!({ "constrainedSampling": false }));
    assert!(matches!(
        disabled.constrained_sampling,
        Some(ConstrainedSampling::Disabled(false))
    ));
    assert_eq!(
        resolve_json_schema_strict_sampling(&disabled, true, None).expect("resolves"),
        None
    );
    assert_eq!(
        resolve_grammar_constrained_sampling(&disabled, true).expect("resolves"),
        None
    );
}

/// TS 的 TypeBox 输出：`Type.Object({...})` 的 `required` 只含非 Optional 属性。
fn typebox_parameters() -> Value {
    json!({
        "type": "object",
        "required": ["path", "metadata"],
        "properties": {
            "path": { "type": "string" },
            "offset": { "type": "number" },
            "metadata": {
                "type": "object",
                "properties": { "enabled": { "type": "boolean" } }
            },
            "nullable": { "anyOf": [{ "type": "string" }, { "type": "null" }] }
        }
    })
}

#[test]
fn derives_strict_provider_schemas_without_changing_tool_definitions() {
    let parameters = typebox_parameters();
    let original = parameters.clone();
    let strict = make_strict_json_schema(&parameters, None).expect("strict schema");

    // 输入不被修改。
    assert_eq!(parameters, original);
    assert!(parameters.get("additionalProperties").is_none());
    assert_eq!(parameters["required"], json!(["path", "metadata"]));

    assert_eq!(strict["additionalProperties"], json!(false));
    assert_eq!(
        strict["required"],
        json!(["path", "offset", "metadata", "nullable"])
    );
    assert_eq!(
        strict["properties"]["offset"],
        json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] })
    );
    assert_eq!(
        strict["properties"]["metadata"],
        json!({
            "type": "object",
            "properties": { "enabled": { "anyOf": [{ "type": "boolean" }, { "type": "null" }] } },
            "required": ["enabled"],
            "additionalProperties": false
        })
    );
    assert_eq!(
        strict["properties"]["nullable"],
        json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] })
    );
}

#[test]
fn falls_back_or_rejects_schemas_that_cannot_be_safely_converted() {
    let cases: Vec<(Value, &str)> = vec![
        (
            json!({
                "type": "object",
                "required": ["metadata"],
                "properties": {
                    "metadata": { "type": "object", "additionalProperties": { "type": "string" } }
                }
            }),
            "additionalProperties is unsupported",
        ),
        (
            json!({
                "type": "object",
                "allOf": [
                    { "type": "object", "properties": { "a": { "type": "string" } }, "required": ["a"] },
                    { "type": "object", "properties": { "b": { "type": "number" } }, "required": ["b"] }
                ]
            }),
            "allOf schemas are unsupported",
        ),
        (
            json!({
                "type": "object",
                "required": ["value"],
                "properties": {
                    "value": { "anyOf": [{ "type": "object", "properties": { "nested": { "type": "string" } } }, { "type": "null" }] }
                }
            }),
            "object and array unions are unsupported",
        ),
        (
            json!({
                "type": "object",
                "properties": { "child": { "$ref": "https://example.com/child.json" } },
                "required": ["child"]
            }),
            "$ref schemas are unsupported",
        ),
    ];

    for (parameters, expected_error) in cases {
        let tool = make_tool(json!({
            "parameters": parameters,
            "constrainedSampling": { "type": "json_schema", "strict": "prefer" }
        }));

        let error = make_strict_json_schema(&tool.parameters, None).expect_err("unsupported");
        assert!(error.message.contains(expected_error), "{}", error.message);

        // prefer → 回落非 strict。
        assert_eq!(
            resolve_json_schema_strict_sampling(&tool, true, None).expect("resolves"),
            None
        );

        // require → 报错（含具体原因）。
        let require_tool = make_tool(json!({
            "parameters": tool.parameters.clone(),
            "constrainedSampling": { "type": "json_schema", "strict": "require" }
        }));
        let error = resolve_json_schema_strict_sampling(&require_tool, true, None)
            .expect_err("require rejects");
        assert!(error.message.contains(expected_error), "{}", error.message);
    }
}

#[test]
fn keeps_grammar_input_json_deltas_append_only() {
    let mut buffer = GrammarToolInputJsonBuffer::default();
    let first = append_grammar_tool_input_json_delta(&mut buffer, "payload", "a\"")
        .expect("append")
        .expect("delta");
    let second = close_grammar_tool_input_json(&mut buffer, "payload", Some("a\"\nb"))
        .expect("close")
        .expect("close output");

    let parsed: Value = serde_json::from_str(&format!("{first}{second}")).expect("valid JSON");
    assert_eq!(parsed, json!({ "payload": "a\"\nb" }));

    assert_eq!(
        close_grammar_tool_input_json(&mut buffer, "payload", Some("a\"\nb")).expect("idempotent"),
        None
    );
    assert_eq!(
        close_grammar_tool_input_json(&mut buffer, "payload", Some("changed"))
            .expect_err("changed after close")
            .message,
        "grammar tool input for property \"payload\" changed after it was closed"
    );
    assert_eq!(
        append_grammar_tool_input_json_delta(&mut buffer, "payload", "x")
            .expect_err("append after close")
            .message,
        "grammar tool input for property \"payload\" changed after it was closed"
    );
}

#[test]
fn rejects_a_final_grammar_input_that_does_not_continue_the_streamed_input() {
    let mut buffer = GrammarToolInputJsonBuffer::default();
    append_grammar_tool_input_json_delta(&mut buffer, "payload", "abc").expect("append");
    assert_eq!(
        close_grammar_tool_input_json(&mut buffer, "payload", Some("abd"))
            .expect_err("non monotonic")
            .message,
        "grammar tool input for property \"payload\" changed non-monotonically"
    );
}

#[test]
fn streams_large_custom_tool_input_in_linear_pieces() {
    // TS 测试用 25_600 与 256_000 字节验证内存线性；这里验证同样规模下输出逐字节正确。
    for size in [25_600usize, 256_000] {
        let deltas: Vec<String> = (0..size / 32)
            .map(|index| {
                let mut chunk = format!("chunk {index} \"quoted\" \\ \n");
                while chunk.len() < 32 {
                    chunk.push('x');
                }
                chunk
            })
            .collect();
        let mut buffer = GrammarToolInputJsonBuffer::default();
        let mut output = String::new();
        for delta in &deltas {
            if let Some(piece) =
                append_grammar_tool_input_json_delta(&mut buffer, "payload", delta).expect("append")
            {
                output.push_str(&piece);
            }
        }
        if let Some(piece) =
            close_grammar_tool_input_json(&mut buffer, "payload", Some(&deltas.join("")))
                .expect("close")
        {
            output.push_str(&piece);
        }
        let parsed: Value = serde_json::from_str(&output).expect("valid JSON");
        assert_eq!(parsed, json!({ "payload": deltas.join("") }));
    }
}

#[test]
fn create_grammar_tool_input_properties_only_includes_grammar_tools() {
    let grammar_tool = tool_with_sampling(
        json!({ "type": "grammar", "variants": { "openai_regex": "^[a-z]+$" } }),
    );
    let plain_tool = make_tool(json!({}));
    let json_schema_tool = tool_with_sampling(json!({ "type": "json_schema", "strict": "prefer" }));

    let properties = create_grammar_tool_input_properties(
        Some(&[plain_tool, grammar_tool, json_schema_tool]),
        true,
    )
    .expect("properties");
    assert_eq!(
        properties.into_iter().collect::<Vec<_>>(),
        vec![("sample_tool".to_string(), "payload".to_string())]
    );

    // 同名工具后者覆盖前者。
    let first = make_tool(json!({
        "name": "same",
        "parameters": { "type": "object", "required": ["a"], "properties": { "a": { "type": "string" } } },
        "constrainedSampling": { "type": "grammar", "variants": { "openai_lark": "x" } }
    }));
    let second = make_tool(json!({
        "name": "same",
        "parameters": { "type": "object", "required": ["b"], "properties": { "b": { "type": "string" } } },
        "constrainedSampling": { "type": "grammar", "variants": { "openai_lark": "x" } }
    }));
    let properties =
        create_grammar_tool_input_properties(Some(&[first, second]), true).expect("properties");
    assert_eq!(properties.get("same").map(String::as_str), Some("b"));
}

#[test]
fn strict_mode_enum_matches_wire_values() {
    // `ConstrainedSamplingConfig` 的 JSON 形状（供适配器反序列化工具定义）。
    let config: ConstrainedSamplingConfig =
        serde_json::from_value(json!({ "type": "json_schema", "strict": "require" })).unwrap();
    assert!(matches!(
        config,
        ConstrainedSamplingConfig::JsonSchema {
            strict: StrictMode::Require
        }
    ));
    let config: ConstrainedSamplingConfig = serde_json::from_value(json!({
        "type": "grammar",
        "variants": { "openai_lark": "x", "openai_regex": "y" }
    }))
    .unwrap();
    assert!(matches!(
        config,
        ConstrainedSamplingConfig::Grammar {
            variants: GrammarVariants {
                openai_lark: Some(_),
                openai_regex: Some(_)
            }
        }
    ));
}

#[test]
fn javascript_trim_governs_grammar_variants() {
    // U+FEFF 在 JS 的 `String.prototype.trim` 里算空白：BOM-only 的变体视为未提供。
    let error = resolve_grammar_constrained_sampling(
        &tool_with_sampling(
            json!({ "type": "grammar", "variants": { "openai_lark": "\u{FEFF}" } }),
        ),
        true,
    )
    .expect_err("BOM-only lark is empty");
    assert_eq!(
        error.message,
        "Tool \"sample_tool\" cannot use grammar constrained sampling: no supported grammar variant was provided."
    );

    // BOM-only lark 被忽略，regex 胜出。
    let grammar = resolve_grammar_constrained_sampling(
        &tool_with_sampling(
            json!({ "type": "grammar", "variants": { "openai_lark": "\u{FEFF}", "openai_regex": "^x$" } }),
        ),
        true,
    )
    .expect("resolves")
    .expect("grammar");
    assert_eq!(grammar.format, GrammarFormat::Regex);
    assert_eq!(grammar.definition, "^x$");

    // U+0085（NEL）在 JS trim 里不是空白，仍然算「已提供」。
    let grammar = resolve_grammar_constrained_sampling(
        &tool_with_sampling(
            json!({ "type": "grammar", "variants": { "openai_lark": "\u{0085}" } }),
        ),
        true,
    )
    .expect("resolves")
    .expect("grammar");
    assert_eq!(grammar.format, GrammarFormat::Lark);
    assert_eq!(grammar.definition, "\u{0085}");
}

#[test]
fn null_grammar_parameters_report_the_javascript_type_error() {
    // TS 读 `null.type` 抛 TypeError，消息被外层 catch 原样拼接。
    let tool = make_tool(json!({
        "parameters": null,
        "constrainedSampling": { "type": "grammar", "variants": { "openai_lark": "x" } }
    }));
    let error = resolve_grammar_constrained_sampling(&tool, true).expect_err("null parameters");
    assert_eq!(
        error.message,
        "Tool \"sample_tool\" cannot use grammar constrained sampling: Cannot read properties of null (reading 'type')."
    );

    // 布尔 schema 在 JS 里只是取到 `undefined`（属性访问不抛错）→ 友好消息。
    let tool = make_tool(json!({
        "parameters": true,
        "constrainedSampling": { "type": "grammar", "variants": { "openai_lark": "x" } }
    }));
    let error = resolve_grammar_constrained_sampling(&tool, true).expect_err("boolean parameters");
    assert_eq!(
        error.message,
        "Tool \"sample_tool\" cannot use grammar constrained sampling: grammar constrained sampling requires an object parameter schema."
    );
}

/// TS 在 `try` 之前读 `config.variants.openai_lark`：`variants` 缺失或为 `null` 时抛出
/// 未包装的 V8 TypeError（不是友好的 "no supported grammar variant" 消息）。
///
/// node 探针（`resolveGrammarConstrainedSampling(tool, true)`）：
/// - `{"type":"grammar"}` → TypeError: Cannot read properties of undefined (reading 'openai_lark')
/// - `{"type":"grammar","variants":null}` → TypeError: Cannot read properties of null (reading 'openai_lark')
/// - `supportsOpenAIGrammarTools: false` → `undefined`（不读 variants，不抛错）。
#[test]
fn missing_or_null_grammar_variants_reproduce_the_typescript_type_error() {
    for (sampling, message) in [
        (
            json!({ "type": "grammar" }),
            "Cannot read properties of undefined (reading 'openai_lark')",
        ),
        (
            json!({ "type": "grammar", "variants": null }),
            "Cannot read properties of null (reading 'openai_lark')",
        ),
    ] {
        let tool = tool_with_sampling(sampling.clone());
        let error = resolve_grammar_constrained_sampling(&tool, true)
            .expect_err("TS throws before the try block");
        assert_eq!(error.message, message, "{sampling}");
        // 支持标志在读取 variants 之前判断：false 时静默回落。
        assert_eq!(
            resolve_grammar_constrained_sampling(&tool, false).expect("resolves"),
            None,
            "{sampling}"
        );
    }

    // 非对象 variants：属性访问不抛错（TS 的 `typeof === "string"` 判定为未设置）。
    for variants in [json!("x"), json!(5), json!(true), json!([])] {
        let tool = tool_with_sampling(json!({ "type": "grammar", "variants": variants }));
        let error = resolve_grammar_constrained_sampling(&tool, true).expect_err("no variants");
        assert_eq!(
            error.message,
            "Tool \"sample_tool\" cannot use grammar constrained sampling: no supported grammar variant was provided.",
            "{variants}"
        );
    }
}

/// TS 从不改写工具定义（`constrainedSampling` 原样存进 session / `toolsAdded`）：宽松形状
/// 必须逐字往返，不能被归一成 `{"type":"json_schema","strict":"prefer"}` /
/// `{"type":"grammar","variants":{}}` / `false`。
#[test]
fn lax_constrained_sampling_json_round_trips_verbatim() {
    for sampling in [
        json!({ "type": "json_schema" }),
        json!({ "type": "json_schema", "strict": null }),
        json!({ "type": "json_schema", "strict": "require", "extra": 1 }),
        json!({ "type": "grammar" }),
        json!({ "type": "grammar", "variants": null }),
        json!({ "type": "grammar", "variants": {} }),
        json!({ "type": "grammar", "variants": { "openai_lark": 5 } }),
        json!({ "type": "grammar", "variants": { "openai_lark": "x", "unknown": 2 } }),
        json!({ "type": "unknown" }),
        json!({}),
        json!(true),
        json!(false),
        json!("x"),
        json!(5),
    ] {
        let tool = tool_with_sampling(sampling.clone());
        assert_eq!(
            serde_json::to_value(&tool).expect("serializes")["constrainedSampling"],
            sampling,
            "{sampling}"
        );
    }

    // 规范形状仍归一成 `Config`（resolver 的快速路径）。
    let tool = tool_with_sampling(json!({ "type": "json_schema", "strict": "require" }));
    assert!(matches!(
        tool.constrained_sampling,
        Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::JsonSchema {
                strict: StrictMode::Require
            }
        ))
    ));
    let tool = tool_with_sampling(json!({ "type": "grammar", "variants": { "openai_lark": "x" } }));
    assert!(matches!(
        tool.constrained_sampling,
        Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::Grammar { .. }
        ))
    ));
}

#[test]
fn lax_constrained_sampling_json_matches_typescript() {
    // TS 的类型只是编译期约束：缺 `strict`、`strict: null` 都按 "prefer" 处理。
    for sampling in [
        json!({ "type": "json_schema" }),
        json!({ "type": "json_schema", "strict": null }),
        json!({ "type": "json_schema", "strict": "prefer" }),
    ] {
        let tool = make_tool(json!({ "constrainedSampling": sampling }));
        assert_eq!(
            resolve_json_schema_strict_sampling(&tool, true, None).expect("resolves"),
            Some(true),
            "{sampling}"
        );
        // 不支持 strict 时 prefer 静默回落。
        assert_eq!(
            resolve_json_schema_strict_sampling(&tool, false, None).expect("resolves"),
            None,
            "{sampling}"
        );
    }

    // 未知 / 缺失 type、任意非对象值：两个 resolver 都等价于「没有配置」。
    for sampling in [
        json!({ "type": "unknown" }),
        json!({}),
        json!(true),
        json!("x"),
        json!(5),
    ] {
        let tool = make_tool(json!({ "constrainedSampling": sampling }));
        assert_eq!(
            resolve_json_schema_strict_sampling(&tool, true, None).expect("resolves"),
            None,
            "{sampling}"
        );
        assert_eq!(
            resolve_grammar_constrained_sampling(&tool, true).expect("resolves"),
            None,
            "{sampling}"
        );
    }

    // 合法形状的序列化不变。
    let tool = tool_with_sampling(json!({ "type": "json_schema", "strict": "require" }));
    assert_eq!(
        serde_json::to_value(&tool).unwrap()["constrainedSampling"],
        json!({ "type": "json_schema", "strict": "require" })
    );
    let tool = tool_with_sampling(json!({
        "type": "grammar",
        "variants": { "openai_lark": "L", "openai_regex": "R" }
    }));
    assert_eq!(
        serde_json::to_value(&tool).unwrap()["constrainedSampling"],
        json!({ "type": "grammar", "variants": { "openai_lark": "L", "openai_regex": "R" } })
    );

    // 非字符串的 grammar 变体按未设置处理（TS 的 `typeof === "string"`）。
    let tool = make_tool(json!({
        "constrainedSampling": { "type": "grammar", "variants": { "openai_lark": 5, "openai_regex": "^x$" } }
    }));
    let grammar = resolve_grammar_constrained_sampling(&tool, true)
        .expect("resolves")
        .expect("grammar");
    assert_eq!(grammar.format, GrammarFormat::Regex);
    assert_eq!(grammar.definition, "^x$");
}
