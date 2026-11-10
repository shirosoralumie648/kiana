//! `packages/ai/test/json-parse.test.ts`。
//!
//! 解析实现位于 [`kiana_ai::utils::json_parse`]（底层 `kiana_json::js_parse` 的修复/流式解析）。

use kiana_ai::utils::json_parse::{
    TOOL_ARGUMENTS_NOT_OBJECT, parse_streaming_json, parse_tool_call_arguments,
};
use serde_json::json;

fn object(value: serde_json::Value) -> kiana_ai::types::JsonObject {
    value
        .as_object()
        .expect("object")
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

// describe("parseStreamingJson")

#[test]
fn keeps_invalid_unicode_escapes_as_literal_text_without_dropping_fields() {
    // #10868
    assert_eq!(
        parse_streaming_json(Some(r#"{"text":"\url{x}"}"#)),
        json!({"text": "\\url{x}"})
    );
    assert_eq!(
        parse_streaming_json(Some(
            r#"{"path":"a.tex","content":"\usepackage{x}","mode":"w"}"#
        )),
        json!({
            "path": "a.tex",
            "content": "\\usepackage{x}",
            "mode": "w",
        })
    );
}

#[test]
fn decodes_valid_unicode_escapes() {
    assert_eq!(
        parse_streaming_json(Some(r#"{"text":"café \url"}"#)),
        json!({"text": "café \\url"})
    );
}

#[test]
fn omits_a_unicode_escape_that_is_cut_off_mid_stream() {
    assert_eq!(
        parse_streaming_json(Some(r#"{"text":"ab\u00"#)),
        json!({"text": "ab"})
    );
}

// describe("parseToolCallArguments") —— #10897

#[test]
fn parses_complete_json_and_repairs_invalid_escapes() {
    let parsed = parse_tool_call_arguments(Some(r#"{"kind":"seek","note":"\url"}"#));
    assert_eq!(
        parsed.arguments,
        object(json!({"kind": "seek", "note": "\\url"}))
    );
    assert_eq!(parsed.arguments_error, None);
}

#[test]
fn uses_empty_arguments_for_empty_text() {
    let parsed = parse_tool_call_arguments(Some(""));
    assert!(parsed.arguments.is_empty());
    assert_eq!(parsed.arguments_error, None);
}

#[test]
fn does_not_keep_the_valid_prefix_of_malformed_json() {
    let raw = r#"{"action":"apply","actions":[{"kind":"seek","time":.5}]}"#;
    let result = parse_tool_call_arguments(Some(raw));
    assert!(result.arguments.is_empty());
    let error = result.arguments_error.expect("arguments error");
    assert!(error.contains("\n\nReceived arguments:\n"));
    assert!(error.ends_with(raw));
}

#[test]
fn does_not_complete_cut_off_json() {
    let raw = r#"{"kind":"seek","clipId":"clip-fl"#;
    let result = parse_tool_call_arguments(Some(raw));
    assert!(result.arguments.is_empty());
    assert!(
        result
            .arguments_error
            .expect("arguments error")
            .ends_with(raw)
    );
}

#[test]
fn rejects_json_that_is_not_an_object() {
    let result = parse_tool_call_arguments(Some("[1,2]"));
    assert!(result.arguments.is_empty());
    assert_eq!(
        result.arguments_error,
        Some(format!(
            "{TOOL_ARGUMENTS_NOT_OBJECT}\n\nReceived arguments:\n[1,2]"
        ))
    );
}
