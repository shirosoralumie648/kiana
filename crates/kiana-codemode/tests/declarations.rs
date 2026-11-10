//! `packages/codemode/test/declarations.test.ts` 的逐用例移植。
//!
//! 每个 `describe`/`it` 标题逐字保留（ARCHITECTURE.md §4.8 第 1 条），便于与黄金样例及历史失败报告对照。
//! 另外三组测试锁定从 TS 参照实现生成的黄金样例（`tests/fixtures/declarations/`）：
//! - `schema_cases.json`：`declarations.test.ts` 的每条断言 + 探针用例，由 TS 的 `schemaToType` 生成；
//! - `tool_cases.json`：`renderToolSignature`/`renderToolSample`/`renderToolOutputType` 的逐字输出；
//! - `declaration_cases.json`：`renderDeclarations` 的逐字节输出（节分隔、命名空间、doc comment）；
//! - `fuzz_cases.json`：300 条确定性随机 schema，覆盖 tag 组合、`$ref`、预算与排序的交叉情形。

use std::sync::Arc;

use kiana_codemode::declarations::{
    DEFAULT_INPUT_SCHEMA_MAX_CHARS, RenderDeclarationsOptions, render_declarations,
    render_tool_output_type, render_tool_sample, render_tool_signature, schema_to_type,
};
use kiana_codemode::types::{CodemodeJsonSchema, CodemodeTool, CodemodeToolExecute};
use serde_json::{Value, json};

/// TS 测试里的 `const execute = () => undefined;`：从不被调用的执行函数。
fn execute() -> CodemodeToolExecute {
    Arc::new(|_args, _context| Box::pin(async { Ok(Value::Null) }))
}

/// 从 fixture 的 JSON 对象构造 `CodemodeTool`（`execute` 是占位）。
fn tool_from_json(value: &Value) -> CodemodeTool {
    let empty = serde_json::Map::new();
    let object = value.as_object().unwrap_or(&empty);
    CodemodeTool {
        name: object
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        description: object
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
        input_schema: object.get("inputSchema").cloned(),
        output_schema: object.get("outputSchema").cloned(),
        spread: None,
        signature: object
            .get("signature")
            .and_then(Value::as_str)
            .map(str::to_string),
        execute: execute(),
    }
}

/// `schemaToType(schema, {maxChars})` 的便捷包装。
fn render(schema: &Value) -> String {
    schema_to_type(schema, None)
}

/// `schemaToType(schema, {maxChars})` 的便捷包装（无预算）。
fn render_budget(schema: &Value, max_chars: usize) -> String {
    schema_to_type(schema, Some(max_chars))
}

fn load_fixture(name: &str) -> Value {
    let text = match name {
        "schema_cases" => include_str!("fixtures/declarations/schema_cases.json"),
        "tool_cases" => include_str!("fixtures/declarations/tool_cases.json"),
        "declaration_cases" => include_str!("fixtures/declarations/declaration_cases.json"),
        "fuzz_cases" => include_str!("fixtures/declarations/fuzz_cases.json"),
        "ref_protocol_cases" => include_str!("fixtures/declarations/ref_protocol_cases.json"),
        "non_finite_cases" => include_str!("fixtures/declarations/non_finite_cases.json"),
        other => panic!("unknown fixture {other}"),
    };
    // 与生产路径同源的解析器：`serde_json` 会拒绝 `1e400`（number out of range），而 TS 的
    // `JSON.parse` 接受它（得到 `Infinity`）；`kiana_json::parse` 按 ARCHITECTURE.md §8 第 32 条
    // 存成 `null`。用同一个解析器，fixture 里的数字字面量就不会出现「加载失败」这种伪差异。
    kiana_json::parse(text).expect("fixture parses")
}

// describe("schemaToType")

// it("renders primitives, literals, and unions")
#[test]
fn renders_primitives_literals_and_unions() {
    assert_eq!(render(&json!({ "type": "string" })), "string");
    assert_eq!(render(&json!({ "type": "integer" })), "number");
    assert_eq!(
        render(&json!({ "type": ["string", "null"] })),
        "string | null"
    );
    assert_eq!(render(&json!({ "const": "a" })), "\"a\"");
    assert_eq!(
        render(&json!({ "enum": ["a", 1, null] })),
        "\"a\" | 1 | null"
    );
    assert_eq!(
        render(&json!({ "anyOf": [{ "type": "string" }, { "type": "number" }] })),
        "string | number"
    );
    assert_eq!(
        render(&json!({ "anyOf": [{ "type": "string" }, {}] })),
        "unknown"
    );
    assert_eq!(
        render(&json!({
            "allOf": [
                { "anyOf": [{ "type": "string" }, { "type": "number" }] },
                { "const": 1 },
            ],
        })),
        "(string | number) & 1"
    );
    assert_eq!(render(&json!({ "$ref": "#/defs/x" })), "unknown");
    assert_eq!(render(&json!(true)), "unknown");
    assert_eq!(render(&json!(false)), "never");
}

// it("renders objects on one line with sorted properties")
#[test]
fn renders_objects_on_one_line_with_sorted_properties() {
    assert_eq!(
        render(&json!({
            "type": "object",
            "properties": { "city": { "type": "string" }, "max-lines": { "type": "number" } },
            "required": ["city"],
            "additionalProperties": false,
        })),
        "{ city: string; \"max-lines\"?: number; }"
    );
    assert_eq!(
        render(&json!({ "type": "object", "additionalProperties": { "type": "number" } })),
        "{ [key: string]: number; }"
    );
    assert_eq!(
        render(&json!({ "type": "object" })),
        "{ [key: string]: unknown; }"
    );
    assert_eq!(
        render(&json!({ "type": "object", "properties": {}, "additionalProperties": false })),
        "{}"
    );
}

// it("puts property descriptions on comment lines")
#[test]
fn puts_property_descriptions_on_comment_lines() {
    assert_eq!(
        render(&json!({
            "type": "object",
            "properties": {
                "weather": {
                    "type": "array",
                    "description": "look up weather for a given list of locations",
                    "items": { "type": "object", "properties": { "location": { "type": "string" } }, "required": ["location"] },
                },
            },
            "required": ["weather"],
        })),
        "{\n  // look up weather for a given list of locations\n  weather: Array<{ location: string; }>;\n}"
    );
    assert_eq!(
        render(&json!({
            "type": "object",
            "properties": {
                "outer": {
                    "type": "object",
                    "description": "Outer",
                    "properties": { "inner": { "type": "string", "description": "Inner" } },
                },
            },
        })),
        "{\n  // Outer\n  outer?: {\n    // Inner\n    inner?: string;\n  };\n}"
    );
}

// it("puts constraints the type cannot express on property comment lines")
#[test]
fn puts_constraints_the_type_cannot_express_on_property_comment_lines() {
    assert_eq!(
        render(&json!({
            "type": "object",
            "properties": {
                "timeoutMs": { "type": "integer", "minimum": 0, "maximum": 30000, "default": 0, "description": "Wait in milliseconds." },
                "id": { "type": "string", "pattern": "^[a-z]+$", "maxLength": 8 },
                "tags": { "type": "array", "items": { "type": "string" }, "maxItems": 5, "uniqueItems": true, "deprecated": true },
                "name": { "type": "string" },
            },
            "required": ["id"],
        })),
        [
            "{",
            "  // @maxLength 8 @pattern ^[a-z]+$",
            "  id: string;",
            "  name?: string;",
            "  // @maxItems 5 @uniqueItems @deprecated",
            "  tags?: Array<string>;",
            "  // Wait in milliseconds.",
            "  // @integer @minimum 0 @maximum 30000 @default 0",
            "  timeoutMs?: number;",
            "}",
        ]
        .join("\n")
    );
}

// it("reads constraints of optional fields from the non-null branch")
#[test]
fn reads_constraints_of_optional_fields_from_the_non_null_branch() {
    assert_eq!(
        render(&json!({
            "type": "object",
            "properties": {
                "limit": {
                    "anyOf": [{ "type": "integer", "minimum": 1, "maximum": 100 }, { "type": "null" }],
                    "default": null,
                    "title": "Limit",
                },
            },
        })),
        "{\n  // @integer @minimum 1 @maximum 100\n  limit?: number | null;\n}"
    );
    assert_eq!(
        render(&json!({
            "type": "object",
            "properties": {
                "n": {
                    "anyOf": [{ "type": "integer", "minimum": 10, "maximum": 50, "format": "int32" }, { "type": "null" }],
                    "minimum": 0,
                    "maximum": 20,
                    "format": "port",
                },
            },
        })),
        "{\n  // @integer @minimum 10 @maximum 20 @format port @format int32\n  n?: number | null;\n}"
    );
}

// it("reads draft-04 boolean exclusive bounds")
#[test]
fn reads_draft_04_boolean_exclusive_bounds() {
    assert_eq!(
        render(&json!({
            "type": "object",
            "properties": {
                "n": { "type": "integer", "minimum": 0, "exclusiveMinimum": true, "maximum": 10, "exclusiveMaximum": false },
                "m": { "anyOf": [{ "type": "number", "maximum": 5, "exclusiveMaximum": true }, { "type": "null" }], "maximum": 8 },
            },
        })),
        [
            "{",
            "  // @maximum 8 @exclusiveMaximum 5",
            "  m?: number | null;",
            "  // @integer @exclusiveMinimum 0 @maximum 10",
            "  n?: number;",
            "}",
        ]
        .join("\n")
    );
}

// it("leaves out long tags and escapes line terminators")
#[test]
fn leaves_out_long_tags_and_escapes_line_terminators() {
    assert_eq!(
        render(&json!({
            "type": "object",
            "properties": { "config": { "type": "object", "default": { "text": "x".repeat(100) } } },
        })),
        "{ config?: { [key: string]: unknown; }; }"
    );
    assert_eq!(
        render(&json!({
            "type": "object",
            "properties": {
                "sep": { "type": "string", "pattern": "^[\r\n\u{2028}\u{2029}]$", "default": "\u{2028}", "description": "a\u{2028}b" },
            },
        })),
        [
            "{",
            "  // a",
            "  // b",
            "  // @pattern ^[\\u000d\\u000a\\u2028\\u2029]$ @default \"\\u2028\"",
            "  sep?: string;",
            "}",
        ]
        .join("\n")
    );
}

// it("drops constraint tags before rendering a type over the budget as unknown")
#[test]
fn drops_constraint_tags_before_rendering_a_type_over_the_budget_as_unknown() {
    let schema =
        json!({ "type": "object", "properties": { "n": { "type": "integer", "minimum": 1 } } });
    assert_eq!(
        render(&schema),
        "{\n  // @integer @minimum 1\n  n?: number;\n}"
    );
    assert_eq!(render_budget(&schema, 20), "{ n?: number; }");
    assert_eq!(render_budget(&schema, 10), "unknown");
}

// it("resolves local references and stops at recursive ones")
#[test]
fn resolves_local_references_and_stops_at_recursive_ones() {
    let schema = json!({
        "type": "object",
        "properties": {
            "item": { "$ref": "#/$defs/Item" },
            "legacy": { "$ref": "#/definitions/Legacy" },
            "remote": { "$ref": "https://example.com/schema.json" },
        },
        "required": ["item"],
        "$defs": {
            "Item": {
                "type": "object",
                "properties": { "id": { "type": "string" }, "parent": { "$ref": "#/$defs/Item" } },
                "required": ["id"],
            },
        },
        "definitions": { "Legacy": { "enum": ["a", "b"] } },
    });
    assert_eq!(
        render(&schema),
        "{ item: { id: string; parent?: unknown; }; legacy?: \"a\" | \"b\"; remote?: unknown; }"
    );
}

// it("renders arrays and tuples")
#[test]
fn renders_arrays_and_tuples() {
    assert_eq!(
        render(&json!({ "type": "array", "items": { "type": "string" } })),
        "Array<string>"
    );
    assert_eq!(
        render(
            &json!({ "type": "array", "prefixItems": [{ "type": "string" }, { "type": "number" }] })
        ),
        "[string, number]"
    );
    assert_eq!(render(&json!({ "type": "array" })), "unknown[]");
}

// it("renders types over the budget as unknown")
#[test]
fn renders_types_over_the_budget_as_unknown() {
    let mut properties = serde_json::Map::new();
    for index in 0..50 {
        properties.insert(format!("field{index}"), json!({ "type": "string" }));
    }
    let schema = json!({ "type": "object", "properties": properties });
    assert_eq!(render_budget(&schema, 100), "unknown");
    assert!(render(&schema).contains("field49?: string;"));
}

// describe("tool declarations")

// it("renders signatures with normalized identifiers")
#[test]
fn renders_signatures_with_normalized_identifiers() {
    let tool = tool_from_json(&json!({
        "name": "hidden-dynamic-tool",
        "inputSchema": {
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"],
            "additionalProperties": false,
        },
        "outputSchema": { "type": "object", "properties": { "ok": { "type": "boolean" } }, "required": ["ok"] },
    }));
    assert_eq!(
        render_tool_signature(&tool, None),
        "hidden_dynamic_tool(args: { city: string; }): Promise<{ ok: boolean; }>;"
    );
    let free = tool_from_json(&json!({ "name": "free" }));
    assert_eq!(
        render_tool_signature(&free, None),
        "free(args: unknown): Promise<unknown>;"
    );
}

// it("renders output schemas shaped like an MCP CallToolResult as plain types")
#[test]
fn renders_output_schemas_shaped_like_an_mcp_call_tool_result_as_plain_types() {
    let tool = tool_from_json(&json!({
        "name": "plain",
        "inputSchema": { "type": "object", "properties": {} },
        "outputSchema": {
            "type": "object",
            "properties": {
                "content": { "type": "array", "items": { "type": "object" } },
                "isError": { "type": "boolean" },
                "_meta": { "type": "object" },
            },
            "required": ["content"],
        },
    }));
    assert_eq!(
        render_tool_signature(&tool, None),
        "plain(args: { [key: string]: unknown; }): Promise<{ _meta?: { [key: string]: unknown; }; content: Array<{ [key: string]: unknown; }>; isError?: boolean; }>;"
    );
}

// it("renders the per-tool sample")
#[test]
fn renders_the_per_tool_sample() {
    let tool = tool_from_json(
        &json!({ "name": "foo", "description": "bar", "inputSchema": { "type": "string" } }),
    );
    assert_eq!(
        render_tool_sample(&tool, None),
        "bar\n\ncodemode tool declaration:\n```ts\ndeclare const tools: { foo(args: string): Promise<unknown>; };\n```"
    );
}

// describe("renderDeclarations")

// it("renders tools and globals")
#[test]
fn renders_tools_and_globals() {
    let tools = vec![
        tool_from_json(&json!({
            "name": "read",
            "description": "Read a file.\nSecond line.",
            "inputSchema": { "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] },
            "outputSchema": { "type": "string" },
        })),
        tool_from_json(&json!({ "name": "remote-api" })),
    ];
    let globals = vec![tool_from_json(&json!({
        "name": "attach",
        "description": "Attach it.",
        "inputSchema": { "type": "string" },
    }))];
    let text = render_declarations(RenderDeclarationsOptions {
        tools: &tools,
        globals: &globals,
    });
    assert_eq!(
        text,
        [
            "declare const tools: {",
            "  /**",
            "   * Read a file.",
            "   * Second line.",
            "   */",
            "  read(args: { path: string; }): Promise<string>;",
            "  remote_api(args: unknown): Promise<unknown>;",
            "};",
            "",
            "/** Attach it. */",
            "declare function attach(args: string): Promise<unknown>;",
        ]
        .join("\n")
    );
}

// it("renders namespaced globals and explicit signatures")
#[test]
fn renders_namespaced_globals_and_explicit_signatures() {
    let globals = vec![
        tool_from_json(&json!({
            "name": "models.list",
            "description": "List models.",
            "signature": "(type: string): Promise<string[]>",
        })),
        tool_from_json(&json!({ "name": "models.get", "inputSchema": { "type": "string" } })),
        tool_from_json(&json!({ "name": "plain", "signature": "(): void" })),
    ];
    let text = render_declarations(RenderDeclarationsOptions {
        tools: &[],
        globals: &globals,
    });
    assert_eq!(
        text,
        [
            "declare function plain(): void;",
            "",
            "declare const models: {",
            "  /** List models. */",
            "  list(type: string): Promise<string[]>;",
            "  get(args: string): Promise<unknown>;",
            "};",
        ]
        .join("\n")
    );
}

// it("escapes comment terminators in descriptions")
#[test]
fn escapes_comment_terminators_in_descriptions() {
    let tools = vec![tool_from_json(
        &json!({ "name": "x", "description": "a */ b" }),
    )];
    let text = render_declarations(RenderDeclarationsOptions {
        tools: &tools,
        globals: &[],
    });
    assert!(text.contains("/** a *\\/ b */"), "got {text:?}");
}

// --- 常量与选项 ---

#[test]
fn default_input_schema_max_chars_is_16000() {
    assert_eq!(DEFAULT_INPUT_SCHEMA_MAX_CHARS, 16_000);
}

#[test]
fn render_declarations_options_helpers_match_partial_options() {
    let tools = vec![tool_from_json(
        &json!({ "name": "t", "signature": "(): void" }),
    )];
    assert_eq!(
        render_declarations(RenderDeclarationsOptions::tools(&tools)),
        "declare const tools: {\n  t(args: unknown): Promise<unknown>;\n};"
    );
    assert_eq!(
        render_declarations(RenderDeclarationsOptions::default()),
        ""
    );
    let globals = vec![tool_from_json(
        &json!({ "name": "g", "signature": "(): void" }),
    )];
    assert_eq!(
        render_declarations(RenderDeclarationsOptions::globals(&globals)),
        "declare function g(): void;"
    );
}

#[test]
fn render_tool_output_type_defaults_to_unknown() {
    assert_eq!(render_tool_output_type(None), "unknown");
    assert_eq!(
        render_tool_output_type(Some(&json!({ "type": "string" }))),
        "string"
    );
}

#[test]
fn render_tool_signature_honours_input_max_chars() {
    let tool = tool_from_json(&json!({
        "name": "big",
        "inputSchema": { "type": "object", "properties": { "n": { "type": "integer", "minimum": 1 } } },
    }));
    assert_eq!(
        render_tool_signature(&tool, None),
        "big(args: {\n  // @integer @minimum 1\n  n?: number;\n}): Promise<unknown>;"
    );
    assert_eq!(
        render_tool_signature(&tool, Some(20)),
        "big(args: { n?: number; }): Promise<unknown>;"
    );
    assert_eq!(
        render_tool_signature(&tool, Some(10)),
        "big(args: unknown): Promise<unknown>;"
    );
}

// --- 黄金样例（TS 参照实现生成） ---

/// 畸形百分号转义在 TS 里抛 `URIError: URI malformed`，`schemaToType` 没有错误通道。
///
/// 冻结的 Rust 签名返回 `String`，因此与 `kiana-json::to_radix_string` 对越界 radix 的先例一致：
/// panic 而不是静默当作 `unknown`（模块文档「已知差异」；已上报架构者）。
#[test]
fn malformed_percent_escapes_panic_like_uri_error() {
    for reference in [
        "#/$defs/%ZZ",
        "#/$defs/%",
        "#/$defs/%E0%A4%A",
        "#/$defs/%ED%A0%80",
        "#/$defs/%4",
    ] {
        // `$defs` 必须在：逐段下钻，前一段不存在时 TS 直接返回 `undefined`，不会解码后面的段。
        let schema = json!({
            "type": "object",
            "properties": { "a": { "$ref": reference } },
            "$defs": { "ok": { "const": 1 } },
        });
        let outcome = std::panic::catch_unwind(|| schema_to_type(&schema, None));
        let payload = outcome.expect_err("malformed escape must panic like URIError");
        let message = payload
            .downcast_ref::<&str>()
            .map(|text| (*text).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        assert_eq!(message, "URI malformed", "reference {reference}");
    }
    // 合法的百分号转义（含 UTF-8 多字节）照常解码。
    let schema = json!({
        "type": "object",
        "properties": { "a": { "$ref": "#/$defs/caf%C3%A9" }, "b": { "$ref": "#/$defs/a%20b" } },
        "$defs": { "café": { "const": 1 }, "a b": { "const": 2 } },
    });
    assert_eq!(render(&schema), "{ a?: 1; b?: 2; }");
}

/// `expansions` 是每次 `schemaToType` 渲染的**全局**计数（不是每分支）：三个兄弟引用各展开
/// 一层再加内层，仍远低于 32；超过 32 之后的引用渲染为 `unknown`。
#[test]
fn ref_expansions_are_counted_per_render() {
    let schema = json!({
        "type": "object",
        "properties": {
            "a": { "$ref": "#/$defs/X" },
            "b": { "$ref": "#/$defs/X" },
            "c": { "$ref": "#/$defs/X" },
        },
        "$defs": { "X": { "type": "object", "properties": { "y": { "$ref": "#/$defs/Y" } } }, "Y": { "type": "string" } },
    });
    assert_eq!(
        render(&schema),
        "{ a?: { y?: string; }; b?: { y?: string; }; c?: { y?: string; }; }"
    );
    // 单链 40 层：第 33 层起是 unknown（32 次展开）。
    let mut defs = serde_json::Map::new();
    for index in 0..40 {
        defs.insert(
            format!("D{index}"),
            json!({ "type": "object", "properties": { "n": { "$ref": format!("#/$defs/D{}", index + 1) } } }),
        );
    }
    defs.insert("D40".to_string(), json!({ "type": "string" }));
    let schema =
        json!({ "type": "object", "properties": { "a": { "$ref": "#/$defs/D0" } }, "$defs": defs });
    let rendered = render(&schema);
    // 32 次展开 → 32 层 `{ n?: … }`，最内层是第 33 次引用撞上上限后的 `n?: unknown`。
    assert_eq!(rendered.matches("{ n?: ").count(), 32, "got {rendered}");
    assert_eq!(rendered.matches("n?: ").count(), 32, "got {rendered}");
    assert!(rendered.contains("n?: unknown; };"), "got {rendered}");
    assert!(
        !rendered.contains("string"),
        "the tail never expands: {rendered}"
    );
}

/// 句柄里的非 BMP 字符（emoji）按码点保留；属性排序仍是 UTF-16 码元序（emoji 的首个
/// 代理码元 `D83D` 大于 `FFFF`，所以排在 `\u{FFFF}` 之后）。
#[test]
fn non_bmp_property_names_sort_by_utf16_units() {
    assert_eq!(
        render(&json!({
            "type": "object",
            "properties": {
                "\u{1F600}": { "type": "string" },
                "\u{FFFF}": { "type": "string" },
                "z": { "type": "string" },
                "\u{FFFD}": { "type": "string" },
                "\u{E000}": { "type": "string" },
            },
        })),
        // 首个 UTF-16 码元：z=7A < 😀=D83D < U+E000 < U+FFFD < U+FFFF（emoji 是代理对，排在本区之前）。
        "{ z?: string; \"\u{1F600}\"?: string; \"\u{E000}\"?: string; \"\u{FFFD}\"?: string; \"\u{FFFF}\"?: string; }"
    );
    assert_eq!(
        render(&json!({
            "type": "object",
            "properties": { "a": { "$ref": "#/$defs/\u{1F600}" } },
            "$defs": { "\u{1F600}": { "const": 1 } },
        })),
        "{ a?: 1; }"
    );
}

/// `String.prototype.trim` 的空白集合与 Rust 的 `str::trim` 不同：U+3000/U+FEFF 去掉，
/// U+0085/U+180E/U+200B 保留。
#[test]
fn description_trim_uses_the_javascript_whitespace_set() {
    assert_eq!(
        render(
            &json!({ "type": "object", "properties": { "a": { "type": "string", "description": "\u{3000}x\u{3000}" } } })
        ),
        "{\n  // x\n  a?: string;\n}"
    );
    for kept in ["\u{0085}", "\u{180E}", "\u{200B}"] {
        let description = format!("{kept}x{kept}");
        let schema = json!({ "type": "object", "properties": { "a": { "type": "string", "description": description } } });
        assert_eq!(
            render(&schema),
            format!("{{\n  // {kept}x{kept}\n  a?: string;\n}}"),
            "U+{:04X} must be preserved by trim",
            kept.chars().next().unwrap() as u32
        );
    }
}

/// `maxChars` 与 `MAX_TAG_CHARS` 都按 UTF-16 码元计：一个 emoji 是 2 个单位。
#[test]
fn budgets_are_measured_in_utf16_units() {
    // `{ "😀"?: string; }` 是 17 个码元 / 18 个 UTF-16 单元：17 个码元的预算判为超长，
    // 按 UTF-16 计的 18 也不够；20 才通过。
    let schema = json!({ "type": "object", "properties": { "\u{1F600}": { "type": "string" } } });
    assert_eq!(render(&schema), "{ \"\u{1F600}\"?: string; }");
    assert_eq!(render_budget(&schema, 17), "unknown");
    assert_eq!(render_budget(&schema, 18), "{ \"\u{1F600}\"?: string; }");
    // `@pattern ` + 35 个 emoji = 9 + 70 = 79 个 UTF-16 单元 → 保留（按字节算会是 149）。
    let emoji = json!({ "type": "object", "properties": { "x": { "type": "string", "pattern": "\u{1F600}".repeat(35) } } });
    assert_eq!(
        render(&emoji),
        "{\n  // @pattern ".to_string() + &"\u{1F600}".repeat(35) + "\n  x?: string;\n}"
    );
    // 36 个 emoji = 81 单元 → tag 被丢弃（若按码点算只有 45，会误留）。
    let over = json!({ "type": "object", "properties": { "x": { "type": "string", "pattern": "\u{1F600}".repeat(36) } } });
    assert_eq!(render(&over), "{ x?: string; }");
}

/// globals 的 `signature` 优先于 schema；tools 的 `signature` 被忽略（TS 的 `renderToolSignature`
/// 只读 name/inputSchema/outputSchema）。
#[test]
fn global_signatures_win_and_tool_signatures_are_ignored() {
    let globals = vec![tool_from_json(&json!({
        "name": "g",
        "signature": "(): void",
        "inputSchema": { "type": "integer" },
    }))];
    assert_eq!(
        render_declarations(RenderDeclarationsOptions::globals(&globals)),
        "declare function g(): void;"
    );
    let globals = vec![tool_from_json(&json!({
        "name": "g",
        "inputSchema": { "type": "integer" },
        "outputSchema": { "enum": ["a", "b"] },
    }))];
    assert_eq!(
        render_declarations(RenderDeclarationsOptions::globals(&globals)),
        "declare function g(args: number): Promise<\"a\" | \"b\">;"
    );
    let tools = vec![tool_from_json(
        &json!({ "name": "t", "signature": "(): void" }),
    )];
    assert_eq!(
        render_declarations(RenderDeclarationsOptions::tools(&tools)),
        "declare const tools: {\n  t(args: unknown): Promise<unknown>;\n};"
    );
}

/// `inputMaxChars: 0` 是「显式给了 0」（`??` 只对 null/undefined 回退），不是缺省。
#[test]
fn explicit_zero_input_max_chars_is_honoured() {
    let tool = tool_from_json(&json!({ "name": "x", "inputSchema": { "type": "string" } }));
    assert_eq!(
        render_tool_signature(&tool, Some(0)),
        "x(args: unknown): Promise<unknown>;"
    );
    assert_eq!(
        render_tool_signature(&tool, None),
        "x(args: string): Promise<unknown>;"
    );
}

#[test]
fn golden_schema_cases_match_the_typescript_reference() {
    let cases = load_fixture("schema_cases");
    let cases = cases.as_array().expect("array");
    assert!(!cases.is_empty());
    let mut failures: Vec<String> = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let schema: &CodemodeJsonSchema = case.get("schema").expect("schema");
        let max_chars = case
            .get("maxChars")
            .and_then(Value::as_u64)
            .map(|n| n as usize);
        let expected = case
            .get("expected")
            .and_then(Value::as_str)
            .expect("expected");
        let actual = schema_to_type(schema, max_chars);
        if actual != expected {
            failures.push(format!(
                "case {index}: schema {schema}\n  expected {expected:?}\n  actual   {actual:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn golden_tool_cases_match_the_typescript_reference() {
    let cases = load_fixture("tool_cases");
    let cases = cases.as_array().expect("array");
    assert!(!cases.is_empty());
    let mut failures: Vec<String> = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let tool = tool_from_json(case);
        let max_chars = case
            .get("inputMaxChars")
            .and_then(Value::as_u64)
            .map(|n| n as usize);
        let expected = |field: &str| {
            case.get(field)
                .and_then(Value::as_str)
                .expect("fixture field")
                .to_string()
        };
        let checks = [
            ("signature", render_tool_signature(&tool, max_chars)),
            ("sample", render_tool_sample(&tool, max_chars)),
            (
                "outputType",
                render_tool_output_type(tool.output_schema.as_ref()),
            ),
        ];
        for (field, actual) in checks {
            let expected = expected(field);
            if actual != expected {
                failures.push(format!(
                    "case {index} ({}) {field}:\n  expected {expected:?}\n  actual   {actual:?}",
                    tool.name
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn golden_declaration_cases_match_the_typescript_reference() {
    let cases = load_fixture("declaration_cases");
    let cases = cases.as_array().expect("array");
    assert!(!cases.is_empty());
    let mut failures: Vec<String> = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let tools: Vec<CodemodeTool> = case
            .get("tools")
            .and_then(Value::as_array)
            .expect("tools")
            .iter()
            .map(tool_from_json)
            .collect();
        let globals: Vec<CodemodeTool> = case
            .get("globals")
            .and_then(Value::as_array)
            .expect("globals")
            .iter()
            .map(tool_from_json)
            .collect();
        let expected = case
            .get("expected")
            .and_then(Value::as_str)
            .expect("expected");
        let actual = render_declarations(RenderDeclarationsOptions {
            tools: &tools,
            globals: &globals,
        });
        if actual != expected {
            let name = case.get("name").and_then(Value::as_str).unwrap_or("?");
            failures.push(format!(
                "case {index} ({name}):\n  expected {expected:?}\n  actual   {actual:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn golden_fuzz_cases_match_the_typescript_reference() {
    let cases = load_fixture("fuzz_cases");
    let cases = cases.as_array().expect("array");
    assert_eq!(cases.len(), 300);
    let mut failures: Vec<String> = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let schema: &CodemodeJsonSchema = case.get("schema").expect("schema");
        let max_chars = case
            .get("maxChars")
            .and_then(Value::as_u64)
            .map(|n| n as usize);
        let expected = case
            .get("expected")
            .and_then(Value::as_str)
            .expect("expected");
        let actual = schema_to_type(schema, max_chars);
        if actual != expected {
            failures.push(format!(
                "case {index}: schema {schema} maxChars {max_chars:?}\n  expected {expected:?}\n  actual   {actual:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

// --- 回归：reviewer 报告的偏差 ---

/// 回归（reviewer 第 3 条）：TS 的 `declarations.ts` 顶层 `export { toCodemodeIdentifier };`，
/// 所以 `@earendil-works/kiana-codemode/declarations` 子入口暴露该函数（spec §2.2.4 导出清单、
/// §3.3「主入口 + /declarations + /portable」）。Rust 的 `declarations` 模块必须同样可访问；
/// 这里用完全限定的路径调用，缺了 `pub use` 就编译失败。
#[test]
fn declarations_module_reexports_to_codemode_identifier() {
    assert_eq!(
        kiana_codemode::declarations::to_codemode_identifier("my-tool"),
        "my_tool"
    );
    // 与 identifier 模块是同一个函数（同一个符号，不是包装）。
    let via_declarations: fn(&str) -> String = kiana_codemode::declarations::to_codemode_identifier;
    let via_identifier: fn(&str) -> String = kiana_codemode::identifier::to_codemode_identifier;
    assert_eq!(via_declarations as usize, via_identifier as usize);
}

/// 在指定栈大小的线程里执行 `f` 并取回结果（借用外层变量，故用 `scope`）。
fn with_stack<T: Send>(stack_size: usize, f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(stack_size)
            .spawn_scoped(scope, f)
            .expect("spawn")
            .join()
            .expect("join")
    })
}

/// `{"type":"object","properties":{"a": …}}` 嵌套 `depth` 层，最内层是 `{"type":"string"}`。
///
/// 用显式的 `Map::insert` 构造，**不走 `json!`**：`json!` 对非字面量表达式会走
/// `serde_json::to_value`，那是递归的序列化，深值会在构造阶段就爆栈。
fn nested_object_chain(depth: usize) -> Value {
    let mut schema = json!({ "type": "string" });
    for _ in 0..depth {
        let mut properties = serde_json::Map::new();
        properties.insert("a".to_string(), schema);
        let mut object = serde_json::Map::new();
        object.insert("type".to_string(), json!("object"));
        object.insert("properties".to_string(), Value::Object(properties));
        schema = Value::Object(object);
    }
    schema
}

/// 回归（reviewer 第 2 条）：TS 的递归在超深 schema 上抛**可捕获**的 `RangeError`，进程存活；
/// Rust 的原实现是栈溢出 `fatal runtime error: stack overflow, aborting`，会杀掉整个 agent
/// 进程。现在 `to_type` 有显式深度上限（`MAX_TYPE_DEPTH = 128`），超限部分渲染 `unknown`。
///
/// 栈安排：`serde_json::Value` 的 `Drop`（以及 `json!` 的 `to_value`）是递归的，深值在 2 MiB
/// 的测试线程栈上约 810 层就会爆——那是 ARCHITECTURE.md §8 第 33 条已决的外部类型残留限制，
/// 不在本 crate 的修复范围。所以构造与析构放在 32 MiB 线程，**渲染**放在 1 MiB 线程：无深度
/// 上限时 1000 层对象链（debug 每层约 3.5 KiB 栈）必然爆栈，有上限时最多 128 层递归
/// （约 0.5 MiB）。
#[test]
fn deep_schemas_render_unknown_instead_of_overflowing_the_stack() {
    let (objects, has_unknown_tail, has_string) = with_stack(32 * 1024 * 1024, || {
        let schema = nested_object_chain(1000);
        let rendered = with_stack(1024 * 1024, || schema_to_type(&schema, None));
        (
            rendered.matches("{ a?: ").count(),
            rendered.contains("a?: unknown;"),
            rendered.contains("string"),
        )
    });
    // 根是深度 0；进入第 128 层时 `depth >= MAX_TYPE_DEPTH`，该层直接是 `unknown`。
    // 所以外面 128 层各渲染一个 `{ a?: … }`，最内层是 `{ a?: unknown; }`。
    assert_eq!(objects, 128);
    assert!(
        has_unknown_tail,
        "the innermost member is cut off at the depth limit"
    );
    assert!(!has_string, "the innermost string is never reached");

    // 128 层以内不受影响（输出由黄金样例锁定）。
    let shallow = nested_object_chain(100);
    let rendered = schema_to_type(&shallow, None);
    assert_eq!(rendered.matches("{ a?: ").count(), 100);
    assert!(rendered.contains("a?: string;"), "got {rendered}");
}

/// 回归（reviewer 第 2 条的数组/联合分支）：`array_type` 与 `allOf`/`anyOf` 的递归同样由
/// 深度上限兜底（这两种形状每层用的栈比对象链更多）。
#[test]
fn deep_array_and_union_schemas_are_bounded_too() {
    let (arrays, unions) = with_stack(32 * 1024 * 1024, || {
        let mut array = json!({ "type": "string" });
        for _ in 0..1000 {
            let mut object = serde_json::Map::new();
            object.insert("type".to_string(), json!("array"));
            object.insert("items".to_string(), array);
            array = Value::Object(object);
        }
        let mut union = json!({ "type": "string" });
        for _ in 0..1000 {
            let mut inner = serde_json::Map::new();
            inner.insert("anyOf".to_string(), Value::Array(vec![union]));
            let mut outer = serde_json::Map::new();
            outer.insert(
                "allOf".to_string(),
                Value::Array(vec![Value::Object(inner)]),
            );
            union = Value::Object(outer);
        }
        (
            with_stack(1024 * 1024, || schema_to_type(&array, None)),
            with_stack(1024 * 1024, || schema_to_type(&union, None)),
        )
    });
    assert_eq!(arrays.matches("Array<").count(), 128, "got {arrays}");
    assert!(arrays.contains("unknown"), "got {arrays}");
    assert!(unions.contains("unknown"), "got {unions}");
}

/// 回归：TS 的 `type` 是数组时用 `toType({ ...schema, type: entry })`——**浅**展开（只换 `type`
/// 一个键）。Rust 若无脑照抄成 `map.clone()`，`serde_json::Map` 的 `Clone` 是深拷贝，深子树会在
/// 深度上限之前爆栈（实测 1 MiB 线程栈上 700 层兄弟子树即溢出）。这里用**渲染在 1 MiB 栈上**
/// 的深兄弟子树锁住该行为。
#[test]
fn type_arrays_do_not_deep_clone_the_schema() {
    let rendered = with_stack(32 * 1024 * 1024, || {
        let mut deep = json!({ "type": "string" });
        for _ in 0..1500 {
            let mut properties = serde_json::Map::new();
            properties.insert("a".to_string(), deep);
            let mut object = serde_json::Map::new();
            object.insert("type".to_string(), json!("object"));
            object.insert("properties".to_string(), Value::Object(properties));
            deep = Value::Object(object);
        }
        let mut properties = serde_json::Map::new();
        properties.insert("deep".to_string(), deep);
        let mut schema = serde_json::Map::new();
        schema.insert("type".to_string(), json!(["object", "null"]));
        schema.insert("properties".to_string(), Value::Object(properties));
        let schema = Value::Object(schema);
        with_stack(1024 * 1024, || schema_to_type(&schema, None))
    });
    // `{ deep?: <被深度上限截断的对象链>; } | null`：最内层是 `a?: unknown;`，永远读不到
    // 真正的 `string`（深度由根对象与 `type` 分支共同消耗，具体层数不是契约）。
    assert!(rendered.ends_with("; } | null"), "got {rendered}");
    assert!(rendered.contains("a?: unknown;"), "got {rendered}");
    assert!(!rendered.contains("string"), "got {rendered}");
}

/// `{ ...schema, type: entry }` 的浅展开语义（输出由黄金样例锁定）：嵌套的类型数组、非字符串条目、
/// `properties` 随每个分支一起生效。
#[test]
fn nested_type_arrays_match_the_typescript_reference() {
    assert_eq!(render(&json!({ "type": [["string"]] })), "string");
    assert_eq!(
        render(&json!({ "type": [["string", "null"], "number"] })),
        "string | null | number"
    );
    assert_eq!(
        render(&json!({ "type": [["string"], []] })),
        "string | never"
    );
    assert_eq!(render(&json!({ "type": [5] })), "unknown");
    assert_eq!(render(&json!({ "type": [[["boolean"]]] })), "boolean");
    assert_eq!(render(&json!({ "type": [null, "string"] })), "unknown");
    assert_eq!(render(&json!({ "type": [true, "string"] })), "unknown");
    assert_eq!(
        render(&json!({ "type": [{ "type": "string" }, "number"] })),
        "unknown"
    );
    // 每个分支都带着 schema 的其它键。
    assert_eq!(
        render(&json!({ "type": ["object", ["array", "string"]] })),
        "{ [key: string]: unknown; } | unknown[] | string"
    );
    assert_eq!(
        render(&json!({ "type": ["object"], "properties": { "a": { "type": "string" } } })),
        "{ a?: string; }"
    );
}

/// `JSON.parse("1e400")` 在 V8 里是 `Infinity`（`typeof` 为 `number`）；`kiana_json::parse` 按
/// ARCHITECTURE.md §8 第 32 条把它存成 `Value::Null`，而 `Value::Null` 与真正的 JSON `null`
/// 在 `Value` 里不可区分。TS 的两处判断依赖「是不是 number / 是不是 null」，所以带非有限数的
/// schema 会偏离。**下面的 `TS:` 字符串全部由 TS 参照实现实测得到**（`schemaToType`），
/// `GOT:` 是 Rust 侧的输出；本测试把两者都锁住，避免把偏差写成「一致」。
///
/// 1. **数值类 tag 关键字**：TS 的 `typeof value === "number"` 对 `Infinity` 为真，所以渲染
///    `@minimum Infinity` 等；这里值是 `null`，该 tag 整条消失（不会变成 `@minimum null`）。
///    注意 `@integer` 只看 `type`，不受影响。
/// 2. **顶层非有限的 `@default`**：TS 的守卫 `default !== undefined && default !== null` 对
///    `Infinity` 为**真**，渲染 `@default null`；这里值是 `null`，守卫为假，tag 整条消失。
///    `const`/`enum` 与**嵌套**在默认值里的非有限数不受影响（`JSON.stringify` 两边都写 `null`）。
///
/// 不能在本层修：把 `null` 当作 `Infinity` 会同时让真正的 `"default": null`（Pydantic 的
/// `Field(None)`，极常见）与 `"minimum": null` 产生 tag，而 TS 对它们都是抑制的，净效果更差。
/// 消除它需要 `kiana-json` 换数字表示（§4.9 契约变更），不属于本单元，已上报架构者。
#[test]
fn non_finite_numbers_become_null_and_lose_their_tags() {
    let schema = kiana_json::parse(
        r#"{"type":"object","properties":{"x":{"type":"integer","minimum":1e400},"y":{"type":"string","default":1e400}}}"#,
    )
    .expect("parses");
    // `1e400` 已在解析期变成 `null`（与真正的 JSON `null` 不可区分）。
    assert_eq!(schema["properties"]["x"]["minimum"], Value::Null);
    assert_eq!(schema["properties"]["y"]["default"], Value::Null);
    assert_eq!(
        schema_to_type(&schema, None),
        // TS（实测）：`{\n  // @integer @minimum Infinity\n  x?: number;\n  // @default null\n  y?: string;\n}`
        // 两处差异都源于 `Infinity` 落成 `null`：`@minimum Infinity` 与 `@default null` 各丢一条。
        "{\n  // @integer\n  x?: number;\n  y?: string;\n}"
    );
}

/// 上一条的 `@default` 分支单独锁定（reviewer 第 1 条指出的正是这里）：顶层 `default` 是
/// 非有限数时 TS 渲染 `@default null`，Rust 因 `Value::Null` 而整条丢掉。
#[test]
fn non_finite_top_level_default_loses_the_default_tag() {
    let schema = kiana_json::parse(
        r#"{"type":"object","properties":{"a":{"type":"string","default":1e400}}}"#,
    )
    .expect("parses");
    assert_eq!(
        schema_to_type(&schema, None),
        // TS（实测）：`{\n  // @default null\n  a?: string;\n}`
        "{ a?: string; }"
    );
    // 真正的 `null` 在 TS 里同样不产生 tag —— 这正是不能靠「null 即 Infinity」修补的原因。
    let literal_null = kiana_json::parse(
        r#"{"type":"object","properties":{"a":{"type":"string","default":null}}}"#,
    )
    .expect("parses");
    assert_eq!(schema_to_type(&literal_null, None), "{ a?: string; }");
}

/// 非有限数落在其它数值型 tag 关键字上时同样丢 tag（TS 会写 `@<keyword> Infinity`）。
/// 这些与上一条同源，一并锁住，避免文档再声称「只有边界 tag 受影响」。
#[test]
fn non_finite_values_lose_every_numeric_tag_keyword() {
    let cases = [
        // TS: `{\n  // @multipleOf Infinity\n  a?: number;\n}`
        (
            r#"{"type":"object","properties":{"a":{"type":"number","multipleOf":1e400}}}"#,
            "{ a?: number; }",
        ),
        // TS: `{\n  // @minLength Infinity\n  a?: string;\n}`
        (
            r#"{"type":"object","properties":{"a":{"type":"string","minLength":1e400}}}"#,
            "{ a?: string; }",
        ),
        // TS: `{\n  // @maxLength Infinity\n  a?: string;\n}`
        (
            r#"{"type":"object","properties":{"a":{"type":"string","maxLength":1e400}}}"#,
            "{ a?: string; }",
        ),
        // TS: `{\n  // @format Infinity\n  a?: string;\n}`
        (
            r#"{"type":"object","properties":{"a":{"type":"string","format":1e400}}}"#,
            "{ a?: string; }",
        ),
        // TS: `{\n  // @pattern Infinity\n  a?: string;\n}`
        (
            r#"{"type":"object","properties":{"a":{"type":"string","pattern":1e400}}}"#,
            "{ a?: string; }",
        ),
        // TS: `{\n  // @minItems Infinity @maxItems Infinity\n  a?: unknown[];\n}`
        (
            r#"{"type":"object","properties":{"a":{"type":"array","minItems":1e400,"maxItems":1e400}}}"#,
            "{ a?: unknown[]; }",
        ),
        // 上界与另一个有限下界共存时，只丢掉非有限的那一条。TS: `@integer @minimum 0 @maximum Infinity`
        (
            r#"{"type":"object","properties":{"a":{"type":"integer","minimum":0,"maximum":1e400}}}"#,
            "{\n  // @integer @minimum 0\n  a?: number;\n}",
        ),
        // draft-04 布尔排他边界搬迁后同样丢 tag。TS: `@integer @exclusiveMinimum Infinity`
        (
            r#"{"type":"object","properties":{"a":{"type":"integer","minimum":1e400,"exclusiveMinimum":true}}}"#,
            "{\n  // @integer\n  a?: number;\n}",
        ),
    ];
    for (raw, expected) in cases {
        let schema = kiana_json::parse(raw).expect("parses");
        assert_eq!(schema_to_type(&schema, None), expected, "schema {raw}");
    }
}

/// 与上一条互补：`const`/`enum` 以及**嵌套**在默认值里的非有限数，因为
/// `JSON.stringify` 只会把非有限数写成 `null`，输出文本因此相同。这条锁住「不要顺手把它们也改坏」。
#[test]
fn non_finite_inside_json_stringify_matches_the_typescript_reference() {
    let cases = [
        (r#"{"const":1e400}"#, "null"),
        (r#"{"const":-1e400}"#, "null"),
        (r#"{"const":[1e400]}"#, "[null]"),
        (r#"{"const":{"v":1e400}}"#, "{\"v\":null}"),
        (r#"{"enum":[1e400,-1e400,null]}"#, "null"),
        (r#"{"enum":[[1e400]]}"#, "[null]"),
        (
            r#"{"type":"object","properties":{"a":{"type":"string","default":[1e400]}}}"#,
            "{\n  // @default [null]\n  a?: string;\n}",
        ),
        (
            r#"{"type":"object","properties":{"a":{"type":"string","default":{"v":1e400}}}}"#,
            "{\n  // @default {\"v\":null}\n  a?: string;\n}",
        ),
        (
            r#"{"type":"object","properties":{"a":{"type":"string","default":{"a":[1e400,-1e400]}}}}"#,
            "{\n  // @default {\"a\":[null,null]}\n  a?: string;\n}",
        ),
    ];
    for (raw, expected) in cases {
        let schema = kiana_json::parse(raw).expect("parses");
        assert_eq!(schema_to_type(&schema, None), expected, "schema {raw}");
    }
}

// ---------------------------------------------------------------------------
// `$ref` 解析协议：百分号转义与 `in` 的原型链语义
// ---------------------------------------------------------------------------

/// `$ref` 段在**解码之前**就决定了「是否抛错」，而「段是否存在」用的是 JS 的 `key in current`
/// ——它走原型链，所以 `#/toString/%ZZ` 这种 ref 在 TS 里照样抛 `URIError`（`toString` 是
/// `Object.prototype` 的成员，`in` 为真；随后的畸形段先解码、再发现 `current` 不是普通对象）。
/// Rust 若只用 `map.get`，会在第一个不存在的自有键处提前返回 `unknown`，漏掉那次抛错。
///
/// 这是 reviewer 清单之外由差分测试发现的一条：`{"$ref":"#/toString/%ZZ"}` 在 TS 抛
/// `URIError: URI malformed`，修复前在这里返回 `"unknown"`。
#[test]
fn ref_segments_walk_the_prototype_chain_like_the_in_operator() {
    // 抛错类：原型链成员 + 畸形百分号段。
    for raw in [
        r##"{"$ref":"#/toString/%ZZ"}"##,
        r##"{"$ref":"#/__proto__/%ZZ"}"##,
        r##"{"$ref":"#/valueOf/%ZZ"}"##,
        r##"{"$ref":"#/constructor/%ZZ"}"##,
        r##"{"$ref":"#/hasOwnProperty/%ZZ"}"##,
        r##"{"$ref":"#/__lookupGetter__/%ZZ"}"##,
        r##"{"$ref":"#/toLocaleString/%ZZ"}"##,
        // 普通对象上的原型链成员后面跟一个正常段：不抛，渲染 `unknown`。
    ] {
        let schema = kiana_json::parse(raw).expect("parses");
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            schema_to_type(&schema, None)
        }));
        assert!(outcome.is_err(), "schema {raw} must panic like URIError");
    }
    // 非抛错类：原型链成员后面是**合法**段时仍是 `unknown`（函数不是普通对象）。
    for (raw, expected) in [
        (r##"{"$ref":"#/toString/x"}"##, "unknown"),
        (r##"{"$ref":"#/toString"}"##, "unknown"),
        (r##"{"$ref":"#/constructor"}"##, "unknown"),
        (r##"{"$ref":"#/__proto__"}"##, "unknown"),
        (r##"{"$ref":"#/__proto__/constructor/x"}"##, "unknown"),
        (r##"{"$ref":"#/__proto__/__proto__"}"##, "unknown"),
        // 自有键优先：同名自有键正常解析。
        (
            r##"{"$ref":"#/toString","toString":{"const":"own"}}"##,
            "\"own\"",
        ),
        // 不存在的普通键：`unknown`，且不会误触原型链。
        (r##"{"$ref":"#/nope"}"##, "unknown"),
        (r##"{"$ref":"#/nope/%ZZ"}"##, "unknown"),
    ] {
        let schema = kiana_json::parse(raw).expect("parses");
        assert_eq!(schema_to_type(&schema, None), expected, "schema {raw}");
    }
}

/// 与 TS 参照实现的黄金样例：640 条 `$ref` 解析用例（原型链成员、`~0`/`~1` 反转义顺序、
/// 空段、合法/畸形百分号转义、`$defs` 为 null/数组/字符串、自有键遮蔽原型成员、以及
/// `__proto__` 链上的 `Object.prototype` 与 JS `null`）。每条要么给出
/// 逐字相同的类型，要么同样抛错（无效百分号转义触发 panic，与黄金样例一致）。
///
/// 由 `packages/codemode/src/declarations.ts` 的 `schemaToType` 生成后按下标取自
/// `/home/ctyun/kiana-work/scratch/kiana-codemode__declarations/`（见 fixture 里的 `error` 字段）。
#[test]
fn golden_ref_protocol_cases_match_the_typescript_reference() {
    let cases = load_fixture("ref_protocol_cases");
    let mut checked = 0;
    for case in cases.as_array().expect("array of cases") {
        let schema = &case["schema"];
        let expects_error = !case["error"].is_null();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            schema_to_type(schema, None)
        }));
        match (outcome, expects_error) {
            (Ok(actual), false) => {
                let expected = case["expected"].as_str().expect("expected string");
                assert_eq!(actual, expected, "schema {}", kiana_json::stringify(schema));
            }
            (Err(_), true) => {}
            (Ok(actual), true) => panic!(
                "schema {} must panic like URIError, got {actual:?}",
                kiana_json::stringify(schema)
            ),
            (Err(_), false) => panic!(
                "schema {} must render {:?}, but panicked",
                kiana_json::stringify(schema),
                case["expected"]
            ),
        }
        checked += 1;
    }
    assert_eq!(checked, 640, "fixture case count");
}

/// 与 TS 参照实现的黄金样例：33 条含非有限数（`1e400`）的 schema，`ts` 是 TS 的逐字输出。
///
/// fixture 存的是**原始 JSON 文本**而不是对象：`JSON.stringify(Infinity)` 是 `"null"`，若把它
/// 写成对象，`1e400` 就退化成普通的 `null` 用例，将无法区分「真正的 null」与「非有限数」——
/// 而这两者在 TS 里给出相反的结果（见模块文档「非有限数字」）。
///
/// 这份样例同时锁住**已决偏差**与**一致部分**：
/// - 一致：`const`/`enum`、以及嵌套在默认值里的非有限数（`JSON.stringify` 两边都写 `null`）。
/// - 偏差：顶层 `@default` 非有限 → TS `@default null`，这里无 tag；数值类 tag 关键字 →
///   TS `@<kw> Infinity`，这里无 tag。
///
/// 偏差条目逐条钉死「只少了那一条 tag，其余逐字相同」，因此偏差既不会被误当成 bug 修掉
/// （那会把真正的 `null` 也变成 tag），也不会在别处悄悄扩大。若 `kiana-json` 将来换数字表示，
/// 这里会失败并提示按 `ts` 更新。
#[test]
fn golden_non_finite_cases_match_the_typescript_reference() {
    let cases = load_fixture("non_finite_cases");
    let cases = cases.as_array().expect("array of cases");
    assert_eq!(cases.len(), 33, "fixture case count");

    /// 偏差许可：这里的 tag 序列必须是 TS 的**子序列**，且被跳过的每一条都在 `LOSSY` 里。
    ///
    /// 贪心子序列匹配：相同就同时前进，不同就记一条「丢失」并只推进 TS 侧。结束时若这里还有
    /// 没对上的 tag（说明多出了 TS 没有的 tag）就返回 `None`。这样「多丢一条」「换成别的值」
    /// 「丢了一条不该丢的」都会失败，偏差范围被严格限制。
    fn lost_tags(ts: &str, actual: &str) -> Option<Vec<String>> {
        fn tags(text: &str) -> Vec<&str> {
            text.lines()
                .filter_map(|line| line.trim().strip_prefix("// "))
                .flat_map(|body| body.split(' '))
                .filter(|word| word.starts_with('@'))
                .collect()
        }
        let ts_tags = tags(ts);
        let actual_tags = tags(actual);
        let mut lost = Vec::new();
        let mut index = 0;
        for tag in &ts_tags {
            if actual_tags.get(index) == Some(tag) {
                index += 1;
            } else {
                lost.push((*tag).to_string());
            }
        }
        (index == actual_tags.len()).then_some(lost)
    }

    // 允许丢失的 tag：`@default`（顶层非有限默认值）与数值类的 11 个关键字。
    const LOSSY: [&str; 12] = [
        "@default",
        "@minimum",
        "@maximum",
        "@exclusiveMinimum",
        "@exclusiveMaximum",
        "@multipleOf",
        "@minLength",
        "@maxLength",
        "@pattern",
        "@format",
        "@minItems",
        "@maxItems",
    ];

    /// 去掉所有注释行与缩进后的「骨架」：tag 丢失会连带改变单行/多行排版（没有注释时
    /// `objectType` 走单行分支），所以这里只比对成员本身。
    fn skeleton(text: &str) -> Vec<String> {
        text.lines()
            .map(str::trim)
            .filter(|line| !line.starts_with("//"))
            .map(|line| {
                line.trim_start_matches('{')
                    .trim_end_matches('}')
                    .trim()
                    .to_string()
            })
            .filter(|line| !line.is_empty())
            .collect()
    }

    let mut same = 0;
    let mut diverged = 0;
    for case in cases {
        let text = case["text"].as_str().expect("raw schema text");
        let schema = kiana_json::parse(text).expect("schema parses");
        let ts = case["ts"].as_str().expect("ts output");
        let actual = schema_to_type(&schema, None);
        if actual == ts {
            same += 1;
            continue;
        }
        diverged += 1;
        let lost = lost_tags(ts, &actual).unwrap_or_else(|| {
            panic!("divergence is not a tag-subsequence of TS for {text}\n  ts:  {ts:?}\n  got: {actual:?}")
        });
        assert!(
            !lost.is_empty(),
            "output differs from TS for {text} without dropping a tag\n  ts:  {ts:?}\n  got: {actual:?}"
        );
        for tag in &lost {
            assert!(
                LOSSY
                    .iter()
                    .any(|allowed| tag == allowed || tag.starts_with(&format!("{allowed} "))),
                "tag {tag:?} may not be lost for {text}\n  ts:  {ts:?}\n  got: {actual:?}"
            );
        }
        // 除 tag 之外的部分必须逐字一致：把两侧的 tag 都抹掉再比。
        assert_eq!(
            skeleton(&actual),
            skeleton(ts),
            "non-tag content differs for {text}"
        );
    }
    // 19 条一致 + 14 条已决偏差（见模块文档「非有限数字」）。
    assert_eq!(same, 19, "byte-identical cases");
    assert_eq!(diverged, 14, "documented divergence cases");
}
