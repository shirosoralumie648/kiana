//! `utils/html.ts` 的测试（TS 无对应测试文件；黄金语料由 TS 参考实现生成）。

use kiana_core::html::{DecodedHtmlEntity, decode_html_entity, decode_html_entity_at};
use serde_json::Value;

#[test]
fn decodes_named_entities() {
    assert_eq!(decode_html_entity("amp").as_deref(), Some("&"));
    assert_eq!(decode_html_entity("lt").as_deref(), Some("<"));
    assert_eq!(decode_html_entity("gt").as_deref(), Some(">"));
    assert_eq!(decode_html_entity("quot").as_deref(), Some("\""));
    assert_eq!(decode_html_entity("apos").as_deref(), Some("'"));
    assert_eq!(decode_html_entity("AMP"), None);
    assert_eq!(decode_html_entity(""), None);
}

#[test]
fn decodes_numeric_entities_with_parse_int_semantics() {
    assert_eq!(decode_html_entity("#x41").as_deref(), Some("A"));
    assert_eq!(decode_html_entity("#X41").as_deref(), Some("A"));
    assert_eq!(decode_html_entity("#65").as_deref(), Some("A"));
    assert_eq!(decode_html_entity("# 65").as_deref(), Some("A"));
    assert_eq!(decode_html_entity("#+65").as_deref(), Some("A"));
    assert_eq!(decode_html_entity("#x0x41").as_deref(), Some("A"));
    assert_eq!(decode_html_entity("#10abc").as_deref(), Some("\n"));
    assert_eq!(decode_html_entity("#x1g").as_deref(), Some("\u{1}"));
    assert_eq!(decode_html_entity("#0x10").as_deref(), Some("\0"));
    assert_eq!(decode_html_entity("#x"), None);
    assert_eq!(decode_html_entity("#"), None);
    assert_eq!(decode_html_entity("#-1"), None);
    assert_eq!(decode_html_entity("#1114112"), None);
    assert_eq!(
        decode_html_entity("#1114111").as_deref(),
        Some("\u{10FFFF}")
    );
}

#[test]
fn decode_html_entity_at_uses_utf16_offsets() {
    // `😀` 占 2 个 UTF-16 code unit。
    assert_eq!(
        decode_html_entity_at("😀&amp;", 2),
        Some(DecodedHtmlEntity {
            text: "&".to_string(),
            length: 5
        })
    );
    assert_eq!(decode_html_entity_at("😀&amp;", 1), None);
    assert_eq!(decode_html_entity_at("&amp;", 0).map(|d| d.length), Some(5));
    assert_eq!(decode_html_entity_at("&amp;", 5), None);
    assert_eq!(decode_html_entity_at("&amp", 0), None);
    assert_eq!(
        decode_html_entity_at("&quot;&apos;", 6).map(|d| d.text),
        Some("'".to_string())
    );
}

#[test]
fn surrogate_code_points_become_replacement_characters() {
    // TS `String.fromCodePoint(0xD800)` 产生孤立代理项；Rust 字符串无法表示 → U+FFFD。
    assert_eq!(decode_html_entity("#xD800").as_deref(), Some("\u{FFFD}"));
    assert_eq!(
        decode_html_entity_at("&#xD800;", 0).map(|decoded| decoded.text),
        Some("\u{FFFD}".to_string())
    );
}

/// 黄金语料：与 TS `decodeHtmlEntity` / `decodeHtmlEntityAt` 逐字比较。
#[test]
fn matches_typescript_reference_corpus() {
    let fixture: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/html_corpus.json"
    )))
    .expect("valid html corpus fixture");

    let mut failures = Vec::new();

    for case in fixture["entities"].as_array().expect("entities array") {
        let entity = case["entity"].as_str().expect("entity string");
        let expected = case["decoded"].as_str().map(str::to_string);
        let actual = decode_html_entity(entity);
        if actual != expected {
            failures.push(format!(
                "entity {entity:?}: expected {expected:?}, got {actual:?}"
            ));
        }
    }

    for case in fixture["at"].as_array().expect("at array") {
        let text = case["text"].as_str().expect("text string");
        let index = case["index"].as_u64().expect("index number") as usize;
        let expected = if case["decoded"].is_null() {
            None
        } else {
            Some(DecodedHtmlEntity {
                text: case["decoded"]["text"]
                    .as_str()
                    .expect("decoded text")
                    .to_string(),
                length: case["decoded"]["length"].as_u64().expect("decoded length") as usize,
            })
        };
        let actual = decode_html_entity_at(text, index);
        if actual != expected {
            failures.push(format!(
                "at {text:?} {index}: expected {expected:?}, got {actual:?}"
            ));
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
