//! 移植 TS `test/mcp-extension.test.ts` 的 `describe("MCP tools")`（6 个 it），
//! 并直接锁定 §8.4 第 5/6 条的 $ref 重定位、工具名冲突与 20KB 截断细节。

mod mcp_core_support;

use std::sync::{Arc, Mutex};

use kiana_agent_core::AgentToolResult;
use kiana_ai::types::{JsonValue, TextContent, TextKind, UserContentBlock};
use kiana_builtin_extensions::mcp::config::McpExposure;
use kiana_builtin_extensions::mcp::tools::{
    ConvertMcpResultOptions, MCP_OUTPUT_MAX_BYTES, McpOutputSaver, convert_mcp_result,
    create_mcp_result_schema, create_mcp_tool_name, limit_mcp_content, to_tool_exposure,
};
use kiana_ext_api::tool_types::ToolExposure;
use kiana_mcp::protocol_types::{AudioContent, CallToolResult, ContentBlock};
use mcp_core_support::{blob_resource, embedded, resource_link, text_resource};
use regex::Regex;
use serde_json::{Map, Value, json};

/// 模型内容块的 JSON 视图。
fn content_json(result: &AgentToolResult<JsonValue>) -> Value {
    serde_json::to_value(&result.content).expect("serialize content")
}

/// 一个 text 内容块。
fn text_block(text: impl Into<String>) -> UserContentBlock {
    UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.into(),
        text_signature: None,
    })
}

/// 记录下来的 `(data, extension)`。
type SavedOutputs = Arc<Mutex<Vec<(Vec<u8>, String)>>>;
/// 记录下来的全文数据。
type SavedData = Arc<Mutex<Vec<Vec<u8>>>>;

/// 记录保存的 `saveOutput` 实现；返回的路径是 `"{prefix}{extension}"`。
fn recording_saver(prefix: &'static str, saved: SavedOutputs) -> McpOutputSaver {
    Arc::new(move |data: Vec<u8>, extension: String| {
        let saved = saved.clone();
        Box::pin(async move {
            saved
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((data, extension.clone()));
            Ok(format!("{prefix}{extension}"))
        })
    })
}

#[test]
fn creates_provider_safe_tool_names() {
    assert_eq!(
        create_mcp_tool_name("docs", "search", |_| false),
        "mcp__docs__search"
    );
    assert_eq!(
        create_mcp_tool_name("my-server", "get.item/v2", |_| false),
        "mcp__my_server__get_item_v2"
    );
    let long = create_mcp_tool_name("server", &"x".repeat(100), |_| false);
    assert_eq!(long.len(), 64);
    assert!(
        Regex::new(r"^mcp__server__x+_[0-9a-f]{8}$")
            .expect("regex")
            .is_match(&long)
    );
    assert_ne!(
        create_mcp_tool_name("server", &format!("{}y", "x".repeat(100)), |_| false),
        long
    );
    // Names that sanitize to one already taken by another tool get a hash suffix.
    let taken = create_mcp_tool_name("s", "a_b", |_| false);
    let second = create_mcp_tool_name("s", "a-b", |name| name == taken);
    assert!(
        Regex::new(r"^mcp__s__a_b_[0-9a-f]{8}$")
            .expect("regex")
            .is_match(&second)
    );
}

#[test]
fn maps_mcp_exposure_to_tool_exposure() {
    // TS: `exposure === "codemode" ? "deferred" : exposure` —— 其余值同名。
    assert_eq!(
        to_tool_exposure(McpExposure::Codemode),
        ToolExposure::Deferred
    );
    assert_eq!(
        to_tool_exposure(McpExposure::Deferred),
        ToolExposure::Deferred
    );
    assert_eq!(to_tool_exposure(McpExposure::Direct), ToolExposure::Direct);
    assert_eq!(to_tool_exposure(McpExposure::Hidden), ToolExposure::Hidden);
}

#[test]
fn hashes_the_nul_joined_server_and_tool_name() {
    let name = create_mcp_tool_name("s", "a-b", |_| true);
    let expected = &kiana_ai::model_catalog::sha256_hex("s\u{0}a-b")[..8];
    assert!(name.ends_with(&format!("_{expected}")));
    assert_ne!(
        expected,
        &kiana_ai::model_catalog::sha256_hex("sa-b")[..8],
        "the separator is NUL, not concatenation"
    );
}

#[test]
fn sanitizes_every_utf16_code_unit_of_non_ascii_names() {
    // JS 的 `/[^A-Za-z0-9_]/g` 按 UTF-16 码元替换：一个 astral 字符产生两个下划线。
    assert_eq!(
        create_mcp_tool_name("s", "\u{1F600}", |_| false),
        format!("mcp__s__{}", "__")
    );
}

#[tokio::test]
async fn declares_the_call_tool_result_scripts_receive_keeping_references_in_the_servers_output_schema()
 {
    let content = "content: Array<{ data?: string; mimeType?: string; text?: string; type: \"text\" | \"image\" | \"audio\" | \"resource_link\" | \"resource\"; [key: string]: unknown; }>; isError?: boolean;";
    // #10416: without an output schema, structured content may be any JSON value.
    assert_eq!(
        kiana_codemode::render_tool_output_type(Some(&create_mcp_result_schema(None))),
        format!("{{ {content} structuredContent?: unknown; [key: string]: unknown; }}")
    );

    let hit = json!({ "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] });
    let raw = json!({
        "type": "object",
        "properties": {
            "hits": { "type": "array", "items": { "$ref": "#/$defs/Hit~1v1" } },
            "again": { "$ref": "#/properties/hits" },
            "children": { "type": "array", "items": { "$ref": "#" } },
            // A property named like a keyword whose value is data.
            "default": { "$ref": "#/definitions/Hit" },
            "remote": { "$ref": "https://example.com/schema.json" }
        },
        "required": ["hits"],
        "$defs": { "Hit/v1": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"], "default": { "$ref": "#/stays" } } },
        "definitions": { "Hit": hit }
    });
    let schema = create_mcp_result_schema(raw.as_object());

    // The `$ref` matrix is relocated by schema position (data positions stay untouched).
    let structured = &schema["properties"]["structuredContent"];
    assert_eq!(
        structured["properties"]["hits"]["items"]["$ref"],
        json!("#/properties/structuredContent/$defs/Hit~1v1")
    );
    assert_eq!(
        structured["properties"]["again"]["$ref"],
        json!("#/properties/structuredContent/properties/hits")
    );
    assert_eq!(
        structured["properties"]["children"]["items"]["$ref"],
        json!("#/properties/structuredContent")
    );
    assert_eq!(
        structured["properties"]["default"]["$ref"],
        json!("#/properties/structuredContent/definitions/Hit")
    );
    assert_eq!(
        structured["properties"]["remote"]["$ref"],
        json!("https://example.com/schema.json")
    );
    // Data that looks like a reference is kept as is.
    assert_eq!(
        structured["$defs"]["Hit/v1"]["default"],
        json!({ "$ref": "#/stays" })
    );
    assert_eq!(
        kiana_codemode::render_tool_output_type(Some(&schema)),
        format!(
            "{{ {content} structuredContent?: {{ again?: Array<{{ id: string; }}>; children?: Array<{{ again?: Array<{{ id: string; }}>; children?: Array<unknown>; default?: {{ id: string; }}; hits: Array<{{ id: string; }}>; remote?: unknown; }}>; default?: {{ id: string; }}; hits: Array<{{ id: string; }}>; remote?: unknown; }}; [key: string]: unknown; }}"
        )
    );
}

#[tokio::test]
async fn converts_results_passing_the_call_tool_result_to_scripts_and_flagging_errors() {
    let blocks = vec![
        resource_link("file:///a", "a", None, None, None, None),
        embedded(text_resource("file:///b", "b text")),
        ContentBlock::Audio(AudioContent::new("", "audio/wav")),
    ];
    let mut meta = Map::new();
    meta.insert("trace".to_string(), Value::String("x".to_string()));
    let converted = convert_mcp_result(
        "docs",
        "t",
        CallToolResult {
            content: blocks.clone(),
            structured_content: Some(json!({ "ok": true })),
            is_error: None,
            meta: Some(meta),
            extra: Map::new(),
        },
        ConvertMcpResultOptions::default(),
    )
    .await
    .expect("convert");
    assert_eq!(
        content_json(&converted),
        json!([
            { "type": "text", "text": "[Resource file:///a \"a\"]" },
            { "type": "text", "text": "b text" },
            { "type": "text", "text": "[audio audio/wav omitted]" }
        ])
    );
    assert_eq!(converted.details, json!({ "server": "docs", "tool": "t" }));
    // Scripts get the server's blocks as sent, without `_meta`.
    assert_eq!(
        converted.structured_content,
        Some(json!({
            "content": serde_json::to_value(&blocks).expect("blocks"),
            "structuredContent": { "ok": true }
        }))
    );
    assert_eq!(converted.is_error, None);

    let fallback = convert_mcp_result(
        "docs",
        "t",
        CallToolResult {
            content: Vec::new(),
            structured_content: Some(json!({ "n": 1 })),
            ..CallToolResult::default()
        },
        ConvertMcpResultOptions::default(),
    )
    .await
    .expect("convert");
    assert_eq!(
        content_json(&fallback),
        json!([{ "type": "text", "text": "{\n  \"n\": 1\n}" }])
    );

    // #10416: array structured content reaches the model as JSON and scripts as sent.
    let array = convert_mcp_result(
        "docs",
        "t",
        CallToolResult {
            content: Vec::new(),
            structured_content: Some(json!([{ "id": 1 }])),
            ..CallToolResult::default()
        },
        ConvertMcpResultOptions::default(),
    )
    .await
    .expect("convert");
    assert_eq!(
        array,
        AgentToolResult {
            content: vec![text_block("[\n  {\n    \"id\": 1\n  }\n]")],
            details: json!({ "server": "docs", "tool": "t" }),
            structured_content: Some(json!({ "content": [], "structuredContent": [{ "id": 1 }] })),
            usage: None,
            is_error: None,
            terminate: None,
        }
    );

    let error = convert_mcp_result(
        "docs",
        "t",
        CallToolResult {
            content: vec![ContentBlock::Text(
                kiana_mcp::protocol_types::TextContent::new("nope"),
            )],
            is_error: Some(true),
            ..CallToolResult::default()
        },
        ConvertMcpResultOptions::default(),
    )
    .await
    .expect("convert");
    assert_eq!(
        error,
        AgentToolResult {
            content: vec![text_block("nope")],
            details: json!({ "server": "docs", "tool": "t" }),
            structured_content: Some(json!({
                "content": [{ "type": "text", "text": "nope" }],
                "isError": true
            })),
            usage: None,
            is_error: Some(true),
            terminate: None,
        }
    );

    let empty_error = convert_mcp_result(
        "docs",
        "t",
        CallToolResult {
            content: Vec::new(),
            is_error: Some(true),
            ..CallToolResult::default()
        },
        ConvertMcpResultOptions::default(),
    )
    .await
    .expect("convert");
    assert_eq!(
        content_json(&empty_error),
        json!([{ "type": "text", "text": "MCP tool docs/t returned an error" }])
    );
}

#[tokio::test]
async fn points_resource_links_to_read_mcp_resource_and_saves_binary_resources() {
    let saved: SavedOutputs = Arc::new(Mutex::new(Vec::new()));
    let converted = convert_mcp_result(
        "docs",
        "t",
        CallToolResult {
            content: vec![
                resource_link(
                    "docs://guide",
                    "guide",
                    Some("The Guide"),
                    Some("How to use it"),
                    Some("text/markdown"),
                    Some(2048.0),
                ),
                embedded(blob_resource(
                    "file:///r/report.pdf",
                    Some("application/pdf"),
                    "JVBERg==",
                )),
                embedded(blob_resource("docs://logo", Some("image/png"), "AAAA")),
            ],
            ..CallToolResult::default()
        },
        ConvertMcpResultOptions {
            save_output: Some(recording_saver("/tmp/saved", saved.clone())),
            readable_resources: true,
        },
    )
    .await
    .expect("convert");
    assert_eq!(
        content_json(&converted),
        json!([
            {
                "type": "text",
                "text": "[Resource docs://guide \"The Guide\" (text/markdown, 2.0KB): How to use it. Read it with read_mcp_resource (server \"docs\")]"
            },
            { "type": "text", "text": "[Binary resource file:///r/report.pdf (application/pdf, 4B) saved to /tmp/saved.pdf]" },
            { "type": "image", "data": "AAAA", "mimeType": "image/png" }
        ])
    );
    assert_eq!(
        *saved.lock().expect("saved"),
        vec![(b"%PDF".to_vec(), ".pdf".to_string())]
    );
}

#[tokio::test]
async fn cuts_the_middle_of_model_facing_text_over_20kb_and_keeps_the_full_result_for_scripts() {
    let saved: SavedData = Arc::new(Mutex::new(Vec::new()));
    let saver: McpOutputSaver = {
        let saved = saved.clone();
        Arc::new(move |data, _extension| {
            let saved = saved.clone();
            Box::pin(async move {
                saved
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(data);
                Ok("/tmp/full.txt".to_string())
            })
        })
    };
    let lines: Vec<String> = (1..=3000).map(|index| format!("line {index}")).collect();
    let full = lines.join("\n");
    let image = ContentBlock::Image(kiana_mcp::protocol_types::ImageContent::new(
        "AAAA",
        "image/png",
    ));
    let result = CallToolResult {
        content: vec![
            ContentBlock::Text(kiana_mcp::protocol_types::TextContent::new(full.clone())),
            image.clone(),
        ],
        ..CallToolResult::default()
    };
    let converted = convert_mcp_result(
        "docs",
        "snapshot",
        result.clone(),
        ConvertMcpResultOptions {
            save_output: Some(saver.clone()),
            readable_resources: false,
        },
    )
    .await
    .expect("convert");
    assert_eq!(converted.content.len(), 2);
    let text = match &converted.content[0] {
        UserContentBlock::Text(text) => text.text.clone(),
        UserContentBlock::Image(_) => panic!("expected text first"),
    };
    // Codex's format: a header, the start and end of the text, then the file with the full text.
    let tokens = full.encode_utf16().count().div_ceil(4);
    assert!(
        text.starts_with(&format!(
            "Warning: truncated output (original token count: {tokens})\nTotal output lines: 3000\n\nline 1\nline 2\n"
        )),
        "{text}"
    );
    assert!(
        Regex::new("…\\d+ chars truncated…")
            .expect("regex")
            .is_match(&text)
    );
    assert!(
        text.ends_with("line 3000\n\n[Full output: /tmp/full.txt (read it with offset/limit)]")
    );
    assert!(text.len() < 21 * 1024);
    assert_eq!(
        serde_json::to_value(&converted.content[1]).expect("image"),
        serde_json::to_value(&image).expect("image")
    );
    assert_eq!(
        converted.details,
        json!({ "server": "docs", "tool": "snapshot", "fullOutputPath": "/tmp/full.txt" })
    );
    assert_eq!(
        *saved.lock().expect("saved"),
        vec![full.clone().into_bytes()]
    );
    assert_eq!(
        converted.structured_content,
        Some(serde_json::to_value(&result).expect("result"))
    );

    // Text within the limit is not saved.
    convert_mcp_result(
        "docs",
        "small",
        CallToolResult {
            content: vec![ContentBlock::Text(
                kiana_mcp::protocol_types::TextContent::new("ok"),
            )],
            ..CallToolResult::default()
        },
        ConvertMcpResultOptions {
            save_output: Some(saver),
            readable_resources: false,
        },
    )
    .await
    .expect("convert");
    assert_eq!(saved.lock().expect("saved").len(), 1);
}

#[test]
fn cuts_multi_byte_text_only_at_character_boundaries() {
    let text = format!("{}end", "é".repeat(20_000));
    let result = kiana_tools::truncate::truncate_middle(&text, 1001);
    assert!(result.truncated);
    assert!(!result.content.contains('\u{FFFD}'));
    assert!(result.content.ends_with("end"));
    let parts: Vec<&str> = Regex::new("…\\d+ chars truncated…")
        .expect("regex")
        .split(&result.content)
        .collect();
    assert_eq!(parts.len(), 2);
    assert!(parts[0].len() <= 500);
    assert!(parts[1].len() <= 501);
    assert_eq!(
        parts[0].chars().count() + parts[1].chars().count() + result.removed_chars as usize,
        text.chars().count()
    );
}

#[tokio::test]
async fn reports_a_failed_full_output_save() {
    let saver: McpOutputSaver =
        Arc::new(|_data, _extension| Box::pin(async move { Err(std::io::Error::other("boom")) }));
    let text = "x".repeat(MCP_OUTPUT_MAX_BYTES + 10);
    let (content, full_output_path) = limit_mcp_content(vec![text_block(text)], Some(saver)).await;
    assert_eq!(full_output_path, None);
    let UserContentBlock::Text(block) = &content[0] else {
        panic!("expected text block");
    };
    assert!(
        block
            .text
            .ends_with("[Could not save the full output: boom]"),
        "{}",
        block.text
    );
}

#[tokio::test]
async fn counts_truncated_output_tokens_in_utf8_bytes_like_the_ts_truncate_middle() {
    // TS `truncateMiddle` 的 `totalBytes` 是 `Buffer.from(content).length`（UTF-8 字节），
    // `limitMcpContent` 用 `Math.ceil(totalBytes / 4)`：多字节文本按**字节**计数。
    let text = "é".repeat(20_000);
    let saver: McpOutputSaver =
        Arc::new(|_data, _extension| Box::pin(async move { Ok("/tmp/full.txt".to_string()) }));
    let (content, _) = limit_mcp_content(vec![text_block(text.clone())], Some(saver)).await;
    let UserContentBlock::Text(block) = &content[0] else {
        panic!("expected text block");
    };
    let expected_tokens = text.len().div_ceil(4);
    assert!(
        block.text.starts_with(&format!(
            "Warning: truncated output (original token count: {expected_tokens})\n"
        )),
        "{}",
        block.text
    );
    assert_eq!(expected_tokens, 10_000);
}

#[test]
fn keeps_resource_schemas_verbatim() {
    let schema = create_mcp_result_schema(None);
    assert_eq!(schema["required"], json!(["content"]));
    assert_eq!(schema["additionalProperties"], json!(true));
    assert_eq!(
        schema["properties"]["content"],
        json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "type": { "enum": ["text", "image", "audio", "resource_link", "resource"] },
                    "text": { "type": "string" },
                    "data": { "type": "string" },
                    "mimeType": { "type": "string" }
                },
                "required": ["type"],
                "additionalProperties": true
            }
        })
    );
    assert_eq!(
        schema["properties"]["structuredContent"],
        json!({}),
        "no output schema means any JSON value"
    );
}

/// TS 参考实现直接生成的黄金文件（`createMcpResultSchema` / `createMcpToolName` /
/// `convertMcpResult` / `formatMcpLogMessage`）。用 Node 26 + vitest 别名跑
/// `packages/coding-agent/src/extensions/mcp/{tools,log}.ts` 得到。
const GOLDEN: &str = include_str!("fixtures/mcp_core_golden.json");

#[tokio::test]
async fn matches_the_ts_golden_fixture() {
    let golden: Value = serde_json::from_str(GOLDEN).expect("golden json");
    let raw = json!({
        "type": "object",
        "properties": {
            "hits": { "type": "array", "items": { "$ref": "#/$defs/Hit~1v1" } },
            "again": { "$ref": "#/properties/hits" },
            "children": { "type": "array", "items": { "$ref": "#" } },
            "default": { "$ref": "#/definitions/Hit" },
            "remote": { "$ref": "https://example.com/schema.json" }
        },
        "required": ["hits"],
        "$defs": { "Hit/v1": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"], "default": { "$ref": "#/stays" } } },
        "definitions": { "Hit": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] } }
    });
    assert_eq!(create_mcp_result_schema(None), golden["noSchema"]);
    assert_eq!(
        create_mcp_result_schema(raw.as_object()),
        golden["withSchema"]
    );

    let name_case = |key: &str| golden["names"][key].as_str().expect("golden name");
    assert_eq!(
        create_mcp_tool_name("docs", "search", |_| false),
        name_case("plain")
    );
    assert_eq!(
        create_mcp_tool_name("my-server", "get.item/v2", |_| false),
        name_case("sanitized")
    );
    assert_eq!(
        create_mcp_tool_name("server", &"x".repeat(100), |_| false),
        name_case("long")
    );
    assert_eq!(
        create_mcp_tool_name("server", &format!("{}y", "x".repeat(100)), |_| false),
        name_case("longY")
    );
    assert_eq!(
        create_mcp_tool_name("s", "a-b", |name| name == "mcp__s__a_b"),
        name_case("taken")
    );

    let saver: McpOutputSaver =
        Arc::new(|_data, extension| Box::pin(async move { Ok(format!("/tmp/saved{extension}")) }));
    let conversion =
        |value: AgentToolResult<JsonValue>| serde_json::to_value(&value).expect("serialize result");

    let blocks = convert_mcp_result(
        "docs",
        "t",
        CallToolResult {
            content: vec![
                resource_link("file:///a", "a", None, None, None, None),
                embedded(text_resource("file:///b", "b text")),
                ContentBlock::Audio(AudioContent::new("", "audio/wav")),
            ],
            structured_content: Some(json!({ "ok": true })),
            meta: Some({
                let mut meta = Map::new();
                meta.insert("trace".to_string(), Value::String("x".to_string()));
                meta
            }),
            ..CallToolResult::default()
        },
        ConvertMcpResultOptions::default(),
    )
    .await
    .expect("convert");
    assert_eq!(conversion(blocks), golden["conversions"]["blocks"]);

    let fallback = convert_mcp_result(
        "docs",
        "t",
        CallToolResult {
            content: Vec::new(),
            structured_content: Some(json!({ "n": 1 })),
            ..CallToolResult::default()
        },
        ConvertMcpResultOptions::default(),
    )
    .await
    .expect("convert");
    assert_eq!(conversion(fallback), golden["conversions"]["fallback"]);

    let array = convert_mcp_result(
        "docs",
        "t",
        CallToolResult {
            content: Vec::new(),
            structured_content: Some(json!([{ "id": 1 }])),
            ..CallToolResult::default()
        },
        ConvertMcpResultOptions::default(),
    )
    .await
    .expect("convert");
    assert_eq!(conversion(array), golden["conversions"]["array"]);

    let is_error = convert_mcp_result(
        "docs",
        "t",
        CallToolResult {
            content: vec![ContentBlock::Text(
                kiana_mcp::protocol_types::TextContent::new("nope"),
            )],
            is_error: Some(true),
            ..CallToolResult::default()
        },
        ConvertMcpResultOptions::default(),
    )
    .await
    .expect("convert");
    assert_eq!(conversion(is_error), golden["conversions"]["isError"]);

    let empty_error = convert_mcp_result(
        "docs",
        "t",
        CallToolResult {
            content: Vec::new(),
            is_error: Some(true),
            ..CallToolResult::default()
        },
        ConvertMcpResultOptions::default(),
    )
    .await
    .expect("convert");
    assert_eq!(conversion(empty_error), golden["conversions"]["emptyError"]);

    let links = convert_mcp_result(
        "docs",
        "t",
        CallToolResult {
            content: vec![
                resource_link(
                    "docs://guide",
                    "guide",
                    Some("The Guide"),
                    Some("How to use it"),
                    Some("text/markdown"),
                    Some(2048.0),
                ),
                embedded(blob_resource(
                    "file:///r/report.pdf",
                    Some("application/pdf"),
                    "JVBERg==",
                )),
                embedded(blob_resource("docs://logo", Some("image/png"), "AAAA")),
            ],
            ..CallToolResult::default()
        },
        ConvertMcpResultOptions {
            save_output: Some(saver.clone()),
            readable_resources: true,
        },
    )
    .await
    .expect("convert");
    assert_eq!(conversion(links), golden["conversions"]["links"]);

    let full = (1..=3000)
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let large = convert_mcp_result(
        "docs",
        "snapshot",
        CallToolResult {
            content: vec![
                ContentBlock::Text(kiana_mcp::protocol_types::TextContent::new(full)),
                ContentBlock::Image(kiana_mcp::protocol_types::ImageContent::new(
                    "AAAA",
                    "image/png",
                )),
            ],
            ..CallToolResult::default()
        },
        ConvertMcpResultOptions {
            save_output: Some(saver),
            readable_resources: false,
        },
    )
    .await
    .expect("convert");
    assert_eq!(conversion(large), golden["conversions"]["large"]);

    assert_eq!(
        kiana_builtin_extensions::mcp::log::format_mcp_log_message(
            "x",
            &json!({ "level": "error", "logger": "db", "data": "a\nb" }),
            1_704_164_645_678,
        ),
        golden["logs"][0].as_str().expect("log 0")
    );
    assert_eq!(
        kiana_builtin_extensions::mcp::log::format_mcp_log_message("s", &Value::Null, 0),
        golden["logs"][1].as_str().expect("log 1")
    );
    assert_eq!(
        kiana_builtin_extensions::mcp::log::format_mcp_log_message(
            "s",
            &json!({ "data": { "b": 1, "a": [2] } }),
            0,
        ),
        golden["logs"][2].as_str().expect("log 2")
    );
}

/// 评审回归：`toParameters` 的 `schema.type ?? "object"` 也替换显式 `null`（`??` 只认
/// `null`/`undefined`，`false` 等其它值保留）。
#[test]
fn guarantees_an_object_parameters_schema() {
    use kiana_builtin_extensions::mcp::tools::{
        McpGetClientFn, McpToolDefinitionOptions, create_mcp_tool_definition,
    };
    use kiana_mcp::auth_provider::McpTransportError;

    fn definition(input_schema: Value) -> Value {
        let get_client: McpGetClientFn = Arc::new(|| {
            Box::pin(async { Err(McpTransportError::new("unused")) })
                as kiana_ext_api::BoxFuture<
                    'static,
                    Result<
                        Arc<dyn kiana_builtin_extensions::mcp::tools::McpToolCaller>,
                        McpTransportError,
                    >,
                >
        });
        create_mcp_tool_definition(McpToolDefinitionOptions {
            server: "s".to_string(),
            tool: kiana_mcp::protocol_types::Tool {
                name: "t".to_string(),
                input_schema: input_schema.as_object().expect("schema").clone(),
                ..kiana_mcp::protocol_types::Tool::default()
            },
            name: "n".to_string(),
            exposure: kiana_builtin_extensions::mcp::config::McpExposure::Direct,
            namespace: kiana_ext_api::tool_types::ToolNamespace {
                name: "mcp".to_string(),
                aliases: None,
                description: None,
                instructions: None,
            },
            timeout_ms: 1000.0,
            get_client,
            readable_resources: None,
        })
        .parameters
    }

    assert_eq!(
        definition(json!({ "type": null })),
        json!({ "type": "object", "properties": {} })
    );
    // `properties` 的缺省判定是 `=== undefined`：显式 null 保留。
    assert_eq!(
        definition(json!({ "type": false, "properties": null })),
        json!({ "type": false, "properties": null })
    );
}

/// 评审回归：非字符串的 `$ref` 值不是引用，按通用分支继续递归（TS 的 `else` 分支）。
#[test]
fn relocates_references_by_schema_position_not_value() {
    let raw = json!({ "properties": { "weird": { "$ref": { "$ref": "#" } } } });
    let schema = create_mcp_result_schema(raw.as_object());
    assert_eq!(
        schema["properties"]["structuredContent"]["properties"]["weird"]["$ref"]["$ref"],
        json!("#/properties/structuredContent")
    );
}

/// 评审回归：`blockToContent` 用原始值渲染资源链接（缺 `name`、小数/显式 null 的 `size` 等）。
#[tokio::test]
async fn formats_resource_links_like_js_including_fractional_and_null_sizes() {
    async fn text_of_block(block: ContentBlock) -> String {
        let result = convert_mcp_result(
            "s",
            "t",
            CallToolResult {
                content: vec![block],
                ..CallToolResult::default()
            },
            ConvertMcpResultOptions::default(),
        )
        .await
        .expect("convert");
        match &result.content[0] {
            UserContentBlock::Text(text) => text.text.clone(),
            UserContentBlock::Image(_) => panic!("expected text"),
        }
    }

    async fn text_of(block: Value) -> String {
        text_of_block(serde_json::from_value(block).expect("content block")).await
    }

    // 缺 `name` 时 TS 仍走 resource_link 分支（title ?? name）。
    assert_eq!(
        text_of(json!({
            "type": "resource_link",
            "uri": "docs://link",
            "title": "OnlyTitle",
            "mimeType": "text/plain"
        }))
        .await,
        "[Resource docs://link \"OnlyTitle\" (text/plain)]"
    );
    assert_eq!(
        text_of(json!({ "type": "resource_link", "uri": "docs://b", "name": "b", "size": 1023.9 }))
            .await,
        "[Resource docs://b \"b\" (1023.9B)]"
    );
    assert_eq!(
        text_of(json!({ "type": "resource_link", "uri": "docs://c", "name": "c", "size": 1075.2 }))
            .await,
        "[Resource docs://c \"c\" (1.1KB)]"
    );
    // 显式 null 的 size 在 TS 里走 `formatSize(null)` → `nullB`。真实客户端把显式 null 补进
    // `extra`，`as_value()` 的视图就是这里构造的原始块。
    assert_eq!(
        text_of_block(ContentBlock::Other(json!({
            "type": "resource_link",
            "uri": "docs://d",
            "name": "d",
            "size": null
        })))
        .await,
        "[Resource docs://d \"d\" (nullB)]"
    );
    assert_eq!(
        text_of(json!({ "type": "resource_link", "uri": "docs://e", "name": "e", "size": "2048" }))
            .await,
        "[Resource docs://e \"e\" (2.0KB)]"
    );
    assert_eq!(
        text_of(json!({
            "type": "resource_link",
            "uri": "docs://f",
            "name": "f",
            "mimeType": null,
            "title": null
        }))
        .await,
        "[Resource docs://f \"f\"]"
    );
}

/// 评审回归：畸形内容块按 TS 的 TypeError 拒绝（`Buffer.from`、`in` 运算符、可选链）。
#[tokio::test]
async fn propagates_ts_type_errors_for_malformed_content_blocks() {
    async fn error_of(block: Value) -> String {
        let block: ContentBlock = serde_json::from_value(block).expect("content block");
        convert_mcp_result(
            "s",
            "t",
            CallToolResult {
                content: vec![block],
                ..CallToolResult::default()
            },
            ConvertMcpResultOptions::default(),
        )
        .await
        .expect_err("malformed block")
        .message
    }

    let buffer_prefix = "The first argument must be of type string or an instance of Buffer, ArrayBuffer, or Array or an Array-like Object.";
    assert_eq!(
        error_of(json!({
            "type": "resource",
            "resource": { "uri": "docs://bad", "mimeType": "application/pdf", "blob": null }
        }))
        .await,
        format!("{buffer_prefix} Received null")
    );
    assert_eq!(
        error_of(json!({
            "type": "resource",
            "resource": { "uri": "u", "mimeType": 5, "blob": "aGk=" }
        }))
        .await,
        "block.resource.mimeType?.startsWith is not a function"
    );
    assert_eq!(
        error_of(json!({ "type": "resource" })).await,
        "Cannot use 'in' operator to search for 'blob' in undefined"
    );
    assert_eq!(
        error_of(json!({ "type": "resource", "resource": null })).await,
        "Cannot use 'in' operator to search for 'blob' in null"
    );
    assert_eq!(
        error_of(json!({ "type": "resource", "resource": "x" })).await,
        "Cannot use 'in' operator to search for 'blob' in x"
    );
}

/// 评审回归：缺失的日志 `params` 是 `undefined`，显式 JSON `null` 仍是 `null`。
#[test]
fn formats_missing_log_params_as_undefined() {
    use kiana_builtin_extensions::mcp::log::format_mcp_log_message;

    assert_eq!(
        format_mcp_log_message("s", None::<&Value>, 0),
        "1970-01-01T00:00:00.000Z [s] info undefined\n"
    );
    assert_eq!(
        format_mcp_log_message("s", Some(&Value::Null), 0),
        "1970-01-01T00:00:00.000Z [s] info null\n"
    );
}
