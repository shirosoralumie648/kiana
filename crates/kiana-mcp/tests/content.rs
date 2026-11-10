//! 移植自 `packages/mcp/test/content.test.ts`（2 个 `it`，用例标题与原文件一致），
//! 外加用 TS 参考实现跑出的边界向量（`/home/ctyun/kiana-work/scratch/kiana-mcp-core-protocol/probe.mjs`）。
//!
//! TS 的断言是 `expect(toLlmContent(...)).toEqual([...])`；Rust 侧把期望值写成
//! `serde_json::Value` 并比较 `serde_json::to_value`，从而同时校验 **JSON 形状与键序**
//! （`{type,data,mimeType}` 的键序也是 ts 的输出序）。

use kiana_mcp::protocol_types::{
    CallToolResult, ContentAnnotations, ContentBlock, ImageContent, LlmContent, ResourceContents,
    ResourceLinkContent, TextContent, to_llm_content,
};
use serde_json::{Value, json};

fn result_from_blocks(blocks: Vec<ContentBlock>) -> CallToolResult {
    CallToolResult::new(blocks)
}

fn as_json(content: Vec<LlmContent>) -> Value {
    serde_json::to_value(content).expect("LlmContent serializes")
}

#[test]
fn passes_text_and_images_through_and_replaces_other_blocks_with_text() {
    // `test/content.test.ts`: "passes text and images through and replaces other blocks with text"
    let blocks: Vec<ContentBlock> = vec![
        ContentBlock::Text(TextContent {
            annotations: Some(ContentAnnotations {
                priority: Some(1.0),
                ..ContentAnnotations::default()
            }),
            ..TextContent::new("hello")
        }),
        ContentBlock::Image(ImageContent {
            meta: Some(serde_json::Map::from_iter([("x".to_owned(), json!(1))])),
            ..ImageContent::new("aW1n", "image/png")
        }),
        ContentBlock::Audio(kiana_mcp::protocol_types::AudioContent::new(
            "YXVk",
            "audio/wav",
        )),
        ContentBlock::ResourceLink(ResourceLinkContent::new("file:///a.txt", "a.txt")),
        ContentBlock::EmbeddedResource(kiana_mcp::protocol_types::EmbeddedResourceContent {
            kind: kiana_mcp::protocol_types::EmbeddedResourceKind::Resource,
            resource: ResourceContents::Text(kiana_mcp::protocol_types::TextResourceContents {
                uri: "file:///b.txt".to_owned(),
                text: "inline".to_owned(),
                ..Default::default()
            }),
            annotations: None,
            meta: None,
            extra: serde_json::Map::new(),
        }),
        ContentBlock::EmbeddedResource(kiana_mcp::protocol_types::EmbeddedResourceContent {
            kind: kiana_mcp::protocol_types::EmbeddedResourceKind::Resource,
            resource: ResourceContents::Blob(kiana_mcp::protocol_types::BlobResourceContents {
                uri: "file:///c.png".to_owned(),
                mime_type: Some("image/png".to_owned()),
                blob: "Yw==".to_owned(),
                ..Default::default()
            }),
            annotations: None,
            meta: None,
            extra: serde_json::Map::new(),
        }),
        ContentBlock::EmbeddedResource(kiana_mcp::protocol_types::EmbeddedResourceContent {
            kind: kiana_mcp::protocol_types::EmbeddedResourceKind::Resource,
            resource: ResourceContents::Blob(kiana_mcp::protocol_types::BlobResourceContents {
                uri: "file:///d.bin".to_owned(),
                mime_type: None,
                blob: "ZA==".to_owned(),
                ..Default::default()
            }),
            annotations: None,
            meta: None,
            extra: serde_json::Map::new(),
        }),
    ];
    assert_eq!(
        as_json(to_llm_content(&result_from_blocks(blocks))),
        json!([
            {"type": "text", "text": "hello"},
            {"type": "image", "data": "aW1n", "mimeType": "image/png"},
            {"type": "text", "text": "[audio audio/wav omitted]"},
            {"type": "text", "text": "a.txt: file:///a.txt"},
            {"type": "text", "text": "inline"},
            {"type": "image", "data": "Yw==", "mimeType": "image/png"},
            {"type": "text", "text": "[binary resource file:///d.bin (unknown type) omitted]"},
        ])
    );
}

#[test]
fn falls_back_to_the_structured_content_as_json_when_there_are_no_blocks() {
    // `test/content.test.ts`: "falls back to the structured content as JSON when there are no blocks"
    // #10416: structured content may be any JSON value.
    assert_eq!(
        as_json(to_llm_content(&CallToolResult {
            content: Vec::new(),
            structured_content: Some(json!([1, 2])),
            ..CallToolResult::default()
        })),
        json!([{"type": "text", "text": "[\n  1,\n  2\n]"}])
    );
    assert_eq!(
        as_json(to_llm_content(&CallToolResult {
            content: Vec::new(),
            structured_content: Some(Value::Null),
            ..CallToolResult::default()
        })),
        json!([{"type": "text", "text": "null"}])
    );
    assert_eq!(
        as_json(to_llm_content(&CallToolResult {
            content: Vec::new(),
            structured_content: Some(json!({"n": 1})),
            ..CallToolResult::default()
        })),
        json!([{"type": "text", "text": "{\n  \"n\": 1\n}"}])
    );
    assert_eq!(
        as_json(to_llm_content(&CallToolResult {
            content: vec![ContentBlock::Text(TextContent::new("n=1"))],
            structured_content: Some(json!({"n": 1})),
            ..CallToolResult::default()
        })),
        json!([{"type": "text", "text": "n=1"}])
    );
}

#[test]
fn structured_content_undefined_never_falls_back() {
    // 缺省 `structuredContent`（`undefined`）且没有内容块 → `[]`（§11）。
    assert_eq!(
        to_llm_content(&CallToolResult::default()),
        Vec::<LlmContent>::new()
    );
}

#[test]
fn structured_content_uses_js_key_order_and_pretty_layout() {
    // 从 TS 参考实现取得：JSON.stringify 的整数键前置 + 2 空格缩进。
    assert_eq!(
        as_json(to_llm_content(&CallToolResult {
            content: Vec::new(),
            structured_content: Some(json!({
                "b": 1, "2": 2, "a": [], "1": {"z": [1, 2]}
            })),
            ..CallToolResult::default()
        })),
        json!([{
            "type": "text",
            "text": "{\n  \"1\": {\n    \"z\": [\n      1,\n      2\n    ]\n  },\n  \"2\": 2,\n  \"b\": 1,\n  \"a\": []\n}"
        }])
    );
    assert_eq!(
        as_json(to_llm_content(&CallToolResult {
            content: Vec::new(),
            structured_content: Some(json!({})),
            ..CallToolResult::default()
        })),
        json!([{"type": "text", "text": "{}"}])
    );
    assert_eq!(
        as_json(to_llm_content(&CallToolResult {
            content: Vec::new(),
            structured_content: Some(json!("hi")),
            ..CallToolResult::default()
        })),
        json!([{"type": "text", "text": "\"hi\""}])
    );
    assert_eq!(
        as_json(to_llm_content(&CallToolResult {
            content: Vec::new(),
            structured_content: Some(json!(false)),
            ..CallToolResult::default()
        })),
        json!([{"type": "text", "text": "false"}])
    );
    assert_eq!(
        as_json(to_llm_content(&CallToolResult {
            content: Vec::new(),
            structured_content: Some(json!(0)),
            ..CallToolResult::default()
        })),
        json!([{"type": "text", "text": "0"}])
    );
}

#[test]
fn unknown_and_malformed_blocks_use_the_ts_placeholder_values() {
    // 向量来自 TS 参考实现（probe.mjs）。
    let cases: Vec<(Value, Value)> = vec![
        (
            json!({"type": "weird", "x": 1}),
            json!({"type": "text", "text": "[unsupported MCP content weird]"}),
        ),
        (
            json!({"text": "hi"}),
            json!({"type": "text", "text": "[unsupported MCP content undefined]"}),
        ),
        (
            json!({"type": "audio", "data": "x"}),
            json!({"type": "text", "text": "[audio undefined omitted]"}),
        ),
        (
            json!({"type": "audio", "data": "x", "mimeType": null}),
            json!({"type": "text", "text": "[audio null omitted]"}),
        ),
        (
            json!({"type": "resource", "resource": {"uri": "u", "blob": "b"}}),
            json!({"type": "text", "text": "[binary resource u (unknown type) omitted]"}),
        ),
        (
            json!({"type": "resource", "resource": {"uri": "u", "mimeType": null, "blob": "b"}}),
            json!({"type": "text", "text": "[binary resource u (unknown type) omitted]"}),
        ),
        (
            json!({"type": "resource", "resource": {"uri": "u", "mimeType": "", "blob": "b"}}),
            json!({"type": "text", "text": "[binary resource u () omitted]"}),
        ),
        (
            json!({"type": "resource", "resource": {"uri": "u", "mimeType": "image/jpeg", "blob": "b"}}),
            json!({"type": "image", "data": "b", "mimeType": "image/jpeg"}),
        ),
        (
            json!({"type": "resource_link", "uri": "file:///a"}),
            json!({"type": "text", "text": "undefined: file:///a"}),
        ),
    ];
    for (block, expected) in cases {
        let parsed: ContentBlock =
            serde_json::from_value(block.clone()).unwrap_or(ContentBlock::Other(block.clone()));
        assert_eq!(
            as_json(to_llm_content(&CallToolResult {
                content: vec![parsed],
                ..CallToolResult::default()
            })),
            json!([expected]),
            "block: {block}"
        );
    }
}

/// 与 TS 参考实现的差分测试：`tests/fixtures/content_golden.json` 由
/// `/home/ctyun/kiana-work/scratch/kiana-mcp-core-protocol/gen2.mjs` 在 node 26 上跑真实
/// `content.ts` 生成（381 个去重输入，其中 52 个落在下面的 `excluded` 里）。
///
/// 比较对象是 `JSON.stringify(toLlmContent(...))` 的**文本**，因此键序、`mimeType` 拼写与
/// 占位字符串逐字一致。
///
/// fixture 的 `excluded` 记录了被排除的 52 个输入：TS 的输出里某个字段的运行时值是
/// `undefined`（例如 `{type:"image"}` 没有 `data`，序列化后整个 `data` 键消失），而
/// [`LlmContent`] 的 `String` 字段无法表达"缺失"——见下面对这条偏差的单独断言。
#[test]
fn matches_the_typescript_reference_on_generated_corpus() {
    let raw = include_str!("fixtures/content_golden.json");
    let fixture: Value = serde_json::from_str(raw).expect("fixture is valid JSON");
    let cases = fixture["cases"].as_array().expect("cases array");
    let mut checked = 0usize;
    for case in cases {
        let input = &case["input"];
        let expected: String = case["expectedJson"]
            .as_str()
            .expect("expectedJson string")
            .to_owned();
        let result: CallToolResult = serde_json::from_value(input.clone())
            .unwrap_or_else(|error| panic!("fixture input must parse: {error}\n{input}"));
        let actual = kiana_json::js_object::stringify(
            &serde_json::to_value(to_llm_content(&result)).unwrap(),
        );
        assert_eq!(actual, expected, "input: {input}");
        checked += 1;
    }
    assert_eq!(checked, 329);
}

/// §30.12 的同类偏差：`LlmContent` 的字段是 `String`，无法表达 TS 里 `undefined`/`null`
/// 导致"键被 `JSON.stringify` 省略"或"值是 `null`"的情况。
///
/// 逐条列出 Rust 侧的行为（TS 的期望值见 fixture 的 `excluded`）：
#[test]
fn documents_the_unrepresentable_llm_content_fields() {
    // TS: `{type:"image", data: undefined, mimeType:"image/png"}` → `{"type":"image","mimeType":"image/png"}`
    // Rust: `data` 变成空串。
    let parsed: ContentBlock =
        serde_json::from_value(json!({"type":"image","mimeType":"image/png"})).unwrap_or(
            ContentBlock::Other(json!({"type":"image","mimeType":"image/png"})),
        );
    assert_eq!(
        as_json(to_llm_content(&CallToolResult {
            content: vec![parsed],
            ..CallToolResult::default()
        })),
        json!([{"type":"image","data":"","mimeType":"image/png"}])
    );

    // TS: `{type:"text"}` → `{"type":"text"}`（text 缺失）。
    // Rust: 空串。
    let parsed: ContentBlock = serde_json::from_value(json!({"type":"text"}))
        .unwrap_or(ContentBlock::Other(json!({"type":"text"})));
    assert_eq!(
        as_json(to_llm_content(&CallToolResult {
            content: vec![parsed],
            ..CallToolResult::default()
        })),
        json!([{"type":"text","text":""}])
    );

    // 评审 finding（minor）：显式 `null` 也是同一类不可表达的字段。TS 原样保留
    // `{type:"text", text:null}`（Node 26 实测），Rust 的 `String` 只能给空串。
    let parsed: ContentBlock = serde_json::from_value(json!({"type":"text","text":null}))
        .unwrap_or(ContentBlock::Other(json!({"type":"text","text":null})));
    assert_eq!(
        as_json(to_llm_content(&CallToolResult {
            content: vec![parsed],
            ..CallToolResult::default()
        })),
        json!([{"type":"text","text":""}])
    );
    let parsed: ContentBlock =
        serde_json::from_value(json!({"type":"image","data":null,"mimeType":"image/png"}))
            .unwrap_or(ContentBlock::Other(
                json!({"type":"image","data":null,"mimeType":"image/png"}),
            ));
    assert_eq!(
        as_json(to_llm_content(&CallToolResult {
            content: vec![parsed],
            ..CallToolResult::default()
        })),
        json!([{"type":"image","data":"","mimeType":"image/png"}])
    );

    // TS: `{type:"resource", resource:{uri:"u", text: 0}}` → `{"type":"text","text":0}`。
    // Rust 的类型化 `LlmContent::Text{text: String}` 只能写字符串，因此按 `String()` 语义
    // 转成 `"0"`（与 §30.12 的"缺失当空串"不同：这里是**值存在但不是字符串**）。
    let parsed: ContentBlock = serde_json::from_value(json!({
        "type":"resource","resource":{"uri":"u","text":0}
    }))
    .unwrap_or(ContentBlock::Other(json!({
        "type":"resource","resource":{"uri":"u","text":0}
    })));
    assert_eq!(
        as_json(to_llm_content(&CallToolResult {
            content: vec![parsed],
            ..CallToolResult::default()
        })),
        json!([{"type":"text","text":"0"}])
    );
}

/// 回归测试（评审 finding #1，major）：`"text" in resource` 是**键存在**判定，
/// 与值的类型无关。`{text: null, mimeType:"image/png", blob:"b"}` 在 TS 里走文本分支，
/// 绝不能被当成图片。
#[test]
fn resource_with_a_text_key_never_becomes_an_image() {
    let cases: Vec<(Value, Value)> = vec![
        (
            json!({"type":"resource","resource":{"uri":"u","text":null,"mimeType":"image/png","blob":"b"}}),
            json!({"type":"text","text":""}),
        ),
        (
            json!({"type":"resource","resource":{"uri":"u","text":0,"mimeType":"image/png","blob":"b"}}),
            json!({"type":"text","text":"0"}),
        ),
        (
            json!({"type":"resource","resource":{"uri":"u","text":false,"mimeType":"image/png","blob":"b"}}),
            json!({"type":"text","text":"false"}),
        ),
        (
            json!({"type":"resource","resource":{"uri":"u","text":"t","mimeType":"image/png","blob":"b"}}),
            json!({"type":"text","text":"t"}),
        ),
    ];
    for (block, expected) in cases {
        let parsed: ContentBlock = serde_json::from_value(block.clone()).expect("block parses");
        // `text` 键存在就走文本分支：类型化 `Text`（`uri`/`text` 都是字符串）或
        // `Other`（`text` 不是字符串时保留原值逐字往返），**绝不**是 `Blob`。
        assert!(
            !matches!(parsed, ContentBlock::EmbeddedResource(ref e)
                if matches!(e.resource, ResourceContents::Blob(_))),
            "text key must select the text branch: {block}"
        );
        assert_eq!(
            as_json(to_llm_content(&CallToolResult {
                content: vec![parsed],
                ..CallToolResult::default()
            })),
            json!([expected]),
            "block: {block}"
        );
    }
}

// ---------------------------------------------------------------------------
// 评审回归测试（核心协议单元）
// ---------------------------------------------------------------------------

/// 读取 `tests/fixtures/content_edges.json`（TS 参考实现在 node 26 上生成）。
fn content_edges() -> Value {
    serde_json::from_str(include_str!("fixtures/content_edges.json")).expect("valid fixture")
}

/// 评审 finding #1（major）：嵌入式 **blob** 资源的 `uri` 缺失 / `null` / 非字符串时，
/// TS 的模板字面量给出 `"undefined"` / `"null"` / `String(value)`，而不是空串。
///
/// 之前 `BlobResourceContents::from_object` 用 `required_string` 把缺失读成 `""`，占位文本因此
/// 少了一段。现在这类对象走 [`ResourceContents::Other`] 保留原值。
#[test]
fn blob_resources_with_a_non_string_uri_use_js_template_literals() {
    let cases = [
        (
            json!({"blob": "b"}),
            "[binary resource undefined (unknown type) omitted]",
        ),
        (
            json!({"uri": null, "blob": "b"}),
            "[binary resource null (unknown type) omitted]",
        ),
        (
            json!({"uri": 5, "blob": "b"}),
            "[binary resource 5 (unknown type) omitted]",
        ),
        (
            json!({"uri": {}, "blob": "b"}),
            "[binary resource [object Object] (unknown type) omitted]",
        ),
        (
            json!({"uri": [1, 2], "blob": "b"}),
            "[binary resource 1,2 (unknown type) omitted]",
        ),
        (
            json!({"uri": true, "blob": "b"}),
            "[binary resource true (unknown type) omitted]",
        ),
    ];
    for (resource, text) in cases {
        let block = json!({"type": "resource", "resource": resource});
        let parsed: ContentBlock = serde_json::from_value(block.clone()).expect("block parses");
        assert!(
            matches!(&parsed, ContentBlock::EmbeddedResource(e)
                if matches!(e.resource, ResourceContents::Other(_))),
            "non-string uri must stay verbatim: {block}"
        );
        assert_eq!(
            as_json(to_llm_content(&CallToolResult {
                content: vec![parsed],
                ..CallToolResult::default()
            })),
            json!([{"type": "text", "text": text}]),
            "block: {block}"
        );
    }
}

/// 评审 finding #1 的同类问题：嵌入式 **blob** 资源的 `blob` 字段类型不符时，
/// `required_string` 会把它写成 `"5"` / `"[object Object]"`，往返后服务器原值就丢了。
///
/// `BlobResourceContents.blob` 是冻结的 `String`，无法逐字表达非字符串值，因此这类对象必须
/// 走 [`ResourceContents::Other`]（`resource_contents_from_value` 的"能逐字表达才类型化"）。
#[test]
fn blob_resources_with_a_non_string_blob_round_trip_verbatim() {
    for blob in [json!(5), json!({}), json!([1, 2]), json!(true), json!(null)] {
        let block = json!({"type": "resource", "resource": {"uri": "u", "blob": blob}});
        let parsed: ContentBlock = serde_json::from_value(block.clone()).expect("block parses");
        assert!(
            matches!(&parsed, ContentBlock::EmbeddedResource(e)
                if matches!(e.resource, ResourceContents::Other(_))),
            "non-string blob must stay verbatim: {block}"
        );
        assert_eq!(
            serde_json::to_value(&parsed).expect("serializes"),
            block,
            "non-string blob must round trip: {block}"
        );
    }
}

/// 评审 finding #4（major）：`resource.mimeType` 存在但不是字符串时，TS 抛
/// `TypeError: resource.mimeType?.startsWith is not a function`（可选链只放行 `null`/缺失）。
///
/// [`to_llm_content`] 的签名冻结为 `-> Vec<LlmContent>`，不能抛；这里断言两件事：
/// 1. [`try_to_llm_content`] 给出的 `TypeError` 信封文本是当前稳定契约（改动需同步测试）；
/// 2. [`to_llm_content`] 降级成模板字面量的占位文本（而不是伪造一个 `(5)` 之类的 mime）。
#[test]
fn non_string_resource_mime_type_throws_like_typescript() {
    for mime in [json!(5), json!({}), json!([]), json!(true)] {
        let block =
            json!({"type": "resource", "resource": {"uri": "u", "mimeType": mime, "blob": "b"}});
        let parsed: ContentBlock = serde_json::from_value(block.clone()).expect("block parses");
        let result = CallToolResult {
            content: vec![parsed],
            ..CallToolResult::default()
        };

        let error = kiana_mcp::protocol_types::try_to_llm_content(&result).unwrap_err();
        assert_eq!(error.name(), "TypeError");
        assert_eq!(
            error.to_string(),
            "resource.mimeType?.startsWith is not a function"
        );

        // 宽松版不抛：按 `String(mimeType)` 渲染占位文本。
        let expected = format!(
            "[binary resource u ({}) omitted]",
            kiana_json::js_string::js_string(Some(&mime))
        );
        assert_eq!(
            as_json(to_llm_content(&result)),
            json!([{"type": "text", "text": expected}]),
            "block: {block}"
        );
    }
}

/// 评审 finding #5（major）：`content` 存在但**不是数组**时 TS 抛
/// `TypeError: (result.content ?? []).map is not a function`。
///
/// `CallToolResult::from_value` 现在拒绝这种形状（`serde_json::from_value` 报错），
/// [`try_to_llm_content_from_value`] 给出逐字的 TypeError。之前 `from_value` 把它读成空数组，
/// `to_llm_content` 于是返回 `structuredContent` 回退文本——对畸形服务器输出编造了另一个结果。
#[test]
fn non_array_content_is_rejected_instead_of_invented() {
    for content in [json!("x"), json!(5), json!({}), json!(true)] {
        let value = json!({"content": content, "structuredContent": {"a": 1}});
        assert!(
            CallToolResult::from_value(&value).is_none(),
            "non-array content must not deserialize: {value}"
        );
        assert!(
            serde_json::from_value::<CallToolResult>(value.clone()).is_err(),
            "serde path must reject too: {value}"
        );
        let error = kiana_mcp::protocol_types::try_to_llm_content_from_value(&value).unwrap_err();
        assert_eq!(error.name(), "TypeError");
        assert_eq!(
            error.to_string(),
            "(result.content ?? []).map is not a function"
        );
    }
    // `content: null` 与字段缺失仍然读成空数组（§30.12）。
    for value in [
        json!({"content": null, "structuredContent": {"a": 1}}),
        json!({"structuredContent": {"a": 1}}),
    ] {
        let parsed = CallToolResult::from_value(&value).expect("null/missing content is fine");
        assert_eq!(
            as_json(to_llm_content(&parsed)),
            json!([{"type": "text", "text": "{\n  \"a\": 1\n}"}]),
            "value: {value}"
        );
    }
}

/// 断言某条 fixture 的 `expectedJson` 与 Rust 的转换结果一致。
///
/// 唯一允许的偏差是 `LlmContent::Image { data: String }` 无法表达非字符串 `data`（§30.12 的
/// 同类偏差）：TS 原样给出数字/布尔，这里只能 `String()` 成 `"5"` / `"true"`。
fn assert_matches_ts_json(case: &Value) {
    let input = &case["input"];
    let expected = case["expectedJson"].as_str().expect("expectedJson");
    let actual = kiana_mcp::protocol_types::try_to_llm_content_from_value(input)
        .unwrap_or_else(|error| panic!("TS did not throw for {input}: {error}"));
    let actual = kiana_json::js_object::stringify(&serde_json::to_value(&actual).unwrap());
    if expected.contains("\"data\":5") || expected.contains("\"data\":true") {
        let degraded = expected
            .replace("\"data\":5", "\"data\":\"5\"")
            .replace("\"data\":true", "\"data\":\"true\"");
        assert_eq!(actual, degraded, "input: {input}");
    } else {
        assert_eq!(actual, expected, "input: {input}");
    }
}

/// `toLlmContent` 抛错路径的完整差分测试：`tests/fixtures/content_edges.json` 的 `cases`
/// （正常返回）与 `throws`（TypeError 逐字）两半都对照 TS 参考实现。
///
/// `try_to_llm_content_from_value` 是 `toLlmContent(result: Pick<…>)` 的对应物
/// （接受**任意** JSON 值，因此 `content` 非数组、`result` 为 `null` 都能复刻）。
#[test]
fn raw_json_conversion_matches_the_typescript_throw_paths() {
    let fixture = content_edges();
    let mut checked = 0usize;

    for case in fixture["cases"].as_array().expect("cases") {
        assert_matches_ts_json(case);
        checked += 1;
    }

    for case in fixture["mixed"].as_array().expect("mixed") {
        assert_matches_ts_json(case);
        checked += 1;
    }

    for case in fixture["throws"].as_array().expect("throws") {
        let input = &case["input"];
        let expected = case["message"].as_str().expect("message");
        let expected_name = case["name"].as_str().expect("name");
        let error = kiana_mcp::protocol_types::try_to_llm_content_from_value(input)
            .err()
            .unwrap_or_else(|| panic!("expected a throw for {input}"));
        assert_eq!(error.name(), expected_name, "input: {input}");
        assert_eq!(error.to_string(), expected, "input: {input}");
        checked += 1;
    }

    assert_eq!(checked, 51, "fixture drift: cases + mixed + throws");
}

/// 评审 finding #6（minor）：已知但**类型不符**的字段必须原样保留（TS 的接口不做运行期校验）。
///
/// `restore_unconsumed` 之前只覆盖 ClientCapabilities/ServerCapabilities/ToolExecution/Tool/
/// ListToolsResult/CallToolResult，其余结构体会把 `_meta: 1`、`size: "9"`、`annotations: 7`、
/// `mimeType: 5` 之类的值丢掉或改写成 `"5"`。现在：
/// - **可选**字段只在类型相符时进类型化字段，否则原值进 `extra`（`optional_string` /
///   `restore_unconsumed`），逐字往返；
/// - **必填**字段（`uri`、`inputSchema`、`capabilities` 等）无法缺席，类型不符时仍会被
///   `String()`/默认值改写——这是模块文档登记的偏差，本测试不覆盖。
#[test]
fn mistyped_known_fields_round_trip_verbatim() {
    let cases: Vec<Value> = vec![
        json!({"contents": [{"uri": "u", "text": "t", "mimeType": 5, "_meta": 1}], "_meta": 1}),
        json!({"contents": [{"uri": "u", "text": "t", "mimeType": "text/plain"}], "_meta": [1]}),
        json!({"contents": [{"uri": "u", "blob": "b", "_meta": {"a": 1}}], "_meta": 1}),
        json!({
            "resources": [{"uri": "u", "name": "n", "size": "9", "annotations": 7, "_meta": 1,
                           "mimeType": 5, "title": ["t"], "description": {"d": 1}}],
            "_meta": 1
        }),
        json!({
            "resourceTemplates": [{"uriTemplate": "t", "name": "n", "annotations": 7, "_meta": 1,
                                   "mimeType": 5, "title": 5, "description": [1]}],
            "_meta": 1
        }),
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {"listChanged": true}},
            "serverInfo": {"name": "s", "version": "1", "_meta": 1, "title": 5},
            "instructions": 5,
            "_meta": 1
        }),
        json!({
            "content": [{"type": "text", "text": "t", "annotations": 7, "_meta": 1}],
            "structuredContent": null
        }),
        json!({
            "content": [{"type": "resource_link", "uri": "u", "name": "n", "size": "9",
                          "annotations": 7, "mimeType": 5, "title": 5, "description": 5}],
        }),
        json!({
            "content": [{"type": "resource", "resource": {"uri": "u", "text": "t", "_meta": 1}}],
        }),
        json!({"contents": [{"uri": "u", "text": "t", "annotations": [1, 2]}]}),
        json!({"tools": [{"name": "t", "inputSchema": {}, "icons": [1], "annotations": 5,
                           "execution": 7, "outputSchema": "x", "_meta": 1}], "_meta": 1}),
    ];
    for raw in cases {
        let text = serde_json::to_string(&raw).expect("input serializes");
        assert_round_trips_as::<kiana_mcp::protocol_types::ReadResourceResult>(&raw, &text);
        assert_round_trips_as::<kiana_mcp::protocol_types::ListResourcesResult>(&raw, &text);
        assert_round_trips_as::<kiana_mcp::protocol_types::ListResourceTemplatesResult>(
            &raw, &text,
        );
        assert_round_trips_as::<kiana_mcp::protocol_types::InitializeResult>(&raw, &text);
        assert_round_trips_as::<kiana_mcp::protocol_types::CallToolResult>(&raw, &text);
        assert_round_trips_as::<kiana_mcp::protocol_types::ListToolsResult>(&raw, &text);
        if let Some(block) = raw.get("content").and_then(|c| c.get(0)) {
            assert_round_trips_as::<kiana_mcp::protocol_types::ContentBlock>(block, &text);
        }
    }
}

/// 逐个比较「原值的每个叶子都在输出里逐字存在」（键序无关，覆盖 `extra` 的往返）。
fn assert_round_trips_as<T>(raw: &Value, source: &str)
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let Ok(parsed) = serde_json::from_value::<T>(raw.clone()) else {
        return; // 该结构体不适用于这个形状
    };
    let back = serde_json::to_value(&parsed).expect("serializes");
    assert_leaves_preserved(raw, &back, source);
    // `serde(flatten)` 在「类型化字段 + `extra` 同名键」时会写出**重复键**；`to_value` 会把它们
    // 静默合并，因此这里在逐字文本里按对象深度统计键名。
    assert_no_duplicate_keys(&serde_json::to_string(&parsed).expect("serializes"), source);
}

/// 逐字 JSON 文本里的重复对象键检测（每个对象内，每个键最多出现一次）。
fn assert_no_duplicate_keys(text: &str, source: &str) {
    let bytes = text.as_bytes();
    let mut index = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut current = String::new();
    // 每层：`None` 表示数组，`Some(map)` 表示对象。
    let mut stack: Vec<Option<std::collections::BTreeMap<String, usize>>> = Vec::new();
    let mut expect_key = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                current.push(byte as char);
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
                if expect_key && let Some(Some(counts)) = stack.last_mut() {
                    *counts.entry(std::mem::take(&mut current)).or_insert(0) += 1;
                    expect_key = false;
                }
            } else {
                current.push(byte as char);
            }
            index += 1;
            continue;
        }
        match byte {
            b'"' => {
                in_string = true;
                current.clear();
            }
            b'{' => {
                stack.push(Some(std::collections::BTreeMap::new()));
                expect_key = true;
            }
            b'[' => {
                stack.push(None);
                expect_key = false;
            }
            b'}' => {
                if let Some(Some(counts)) = stack.pop() {
                    for (key, count) in counts {
                        assert_eq!(count, 1, "duplicate key {key:?} in {text} (from {source})");
                    }
                }
                expect_key = false;
            }
            b']' => {
                stack.pop();
                expect_key = false;
            }
            b',' => expect_key = true,
            _ => {}
        }
        index += 1;
    }
}

/// 原值的每个叶子都应在输出里出现。`null` 与缺失在 TS 的 `JSON.stringify` 里等价（都省略），
/// 因此跳过 `null`；JSON 对象键的比较不看顺序（结构体按声明序写）。
fn assert_leaves_preserved(raw: &Value, back: &Value, source: &str) {
    match raw {
        Value::Object(map) => {
            for (key, value) in map {
                if value.is_null() {
                    continue;
                }
                match back.get(key) {
                    Some(rest) => assert_leaves_preserved(value, rest, source),
                    None => panic!("key {key:?} dropped: {source} -> {back}"),
                }
            }
        }
        Value::Array(items) => {
            let back_items = back
                .as_array()
                .unwrap_or_else(|| panic!("array became non-array: {source} -> {back}"));
            assert_eq!(items.len(), back_items.len(), "array length: {source}");
            for (item, back_item) in items.iter().zip(back_items) {
                assert_leaves_preserved(item, back_item, source);
            }
        }
        other => assert_eq!(other, back, "leaf changed: {source}"),
    }
}

/// 评审 finding 4（minor）：图片分支的 `data`/`mimeType` 按 `String()` 语义转换
/// （与文本分支的 `js_field_text`、`ImageContent::from_object` 的 `required_string` 一致）。
///
/// TS 的 `blockToLlmContent` 原样保留数字（Node 26 实测：
/// `{type:"image",data:123,mimeType:"image/png"}` → `data` 仍是数字 `123`）；
/// [`LlmContent`] 的字段是 `String`，只能给出 `"123"`（模块文档登记的偏差）。
/// 修复前这两个字段被 `.and_then(as_str)` 折成空串，与同一输入的类型化路径（"123"）矛盾。
#[test]
fn image_fields_use_string_semantics_in_the_raw_path() {
    let actual = kiana_mcp::protocol_types::try_to_llm_content_from_value(&json!({
        "content": [{"type": "image", "data": 123, "mimeType": "image/png"}],
    }))
    .expect("no throw");
    assert_eq!(
        as_json(actual),
        json!([{"type": "image", "data": "123", "mimeType": "image/png"}])
    );

    let actual = kiana_mcp::protocol_types::try_to_llm_content_from_value(&json!({
        "content": [{"type": "image", "data": "aW1n", "mimeType": 456}],
    }))
    .expect("no throw");
    assert_eq!(
        as_json(actual),
        json!([{"type": "image", "data": "aW1n", "mimeType": "456"}])
    );

    // `null`/缺失 → 空串（与类型化路径的 `required_string` 一致；TS 给出 `null`/`undefined`，
    // `String` 字段表达不了，见模块文档）。
    let actual = kiana_mcp::protocol_types::try_to_llm_content_from_value(&json!({
        "content": [{"type": "image", "data": null, "mimeType": null}],
    }))
    .expect("no throw");
    assert_eq!(
        as_json(actual),
        json!([{"type": "image", "data": "", "mimeType": ""}])
    );
    let actual = kiana_mcp::protocol_types::try_to_llm_content_from_value(&json!({
        "content": [{"type": "image"}],
    }))
    .expect("no throw");
    assert_eq!(
        as_json(actual),
        json!([{"type": "image", "data": "", "mimeType": ""}])
    );

    // 类型化路径给出同样的结果（两条 API 必须一致）。
    let typed = CallToolResult::from_value(&json!({
        "content": [{"type": "image", "data": 123, "mimeType": "image/png"}],
    }))
    .expect("parses");
    assert_eq!(
        as_json(to_llm_content(&typed)),
        json!([{"type": "image", "data": "123", "mimeType": "image/png"}])
    );
}
