//! `edit-diff.ts` 的黄金测试。
//!
//! fixture `tests/fixtures/file-tools/edit_diff.golden.json` 由 TS 参考实现
//! （`edit-diff.ts` + `edit.ts#prepareArguments`，diff@8.0.4）生成，逐字节锁定：
//! Myers 行分组、unified patch 头、展示 diff 的行号/跳过行、模糊归一化、九种错误文案与
//! `prepareArguments` 的键顺序。

use std::fs;

use kiana_ext_api::tool_types::ReplaceEdit;
use kiana_tools::edit::prepare_edit_arguments;
use kiana_tools::edit_diff::{
    apply_edits_to_normalized_content, generate_diff_string_with_context,
    generate_unified_patch_with_context, normalize_for_fuzzy_match,
};
use serde_json::{Value, json};

fn fixture() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/file-tools/edit_diff.golden.json"
    );
    let text = fs::read_to_string(path).expect("read edit_diff golden");
    serde_json::from_str(&text).expect("parse edit_diff golden")
}

#[test]
fn unified_patch_and_display_diff_match_the_reference_bytes() {
    let fixture = fixture();
    let pairs = fixture["pairs"].as_array().expect("pairs");
    assert!(pairs.len() > 100);
    for pair in pairs {
        let name = pair["name"].as_str().expect("name");
        let old = pair["old"].as_str().expect("old");
        let new = pair["new"].as_str().expect("new");
        let context = pair["context"].as_u64().expect("context") as usize;

        let patch = generate_unified_patch_with_context("src/file.txt", old, new, context);
        assert_eq!(
            patch,
            pair["patch"].as_str().expect("patch"),
            "patch mismatch for {name}"
        );

        let result = generate_diff_string_with_context(old, new, context);
        assert_eq!(
            result.diff,
            pair["diff"].as_str().expect("diff"),
            "diff mismatch for {name}"
        );
        let expected_first = pair["firstChangedLine"].as_u64();
        assert_eq!(
            result.first_changed_line, expected_first,
            "firstChangedLine mismatch for {name}"
        );
    }
}

#[test]
fn apply_edits_matches_the_reference_results_and_errors() {
    let fixture = fixture();
    let cases = fixture["apply"].as_array().expect("apply");
    assert!(cases.len() > 30);
    for case in cases {
        let name = case["name"].as_str().expect("name");
        let content = case["content"].as_str().expect("content");
        let edits: Vec<ReplaceEdit> = serde_json::from_value(case["edits"].clone()).expect("edits");
        let expected_error = case["error"].as_str();
        match apply_edits_to_normalized_content(content, &edits, "file.txt") {
            Ok(result) => {
                assert!(
                    expected_error.is_none(),
                    "expected error {expected_error:?} for {name}, got success"
                );
                assert_eq!(
                    result.base_content,
                    case["baseContent"].as_str().expect("baseContent"),
                    "baseContent mismatch for {name}"
                );
                assert_eq!(
                    result.new_content,
                    case["newContent"].as_str().expect("newContent"),
                    "newContent mismatch for {name}"
                );
            }
            Err(error) => {
                assert_eq!(
                    Some(error.message.as_str()),
                    expected_error,
                    "error mismatch for {name}"
                );
            }
        }
    }
}

#[test]
fn fuzzy_normalization_matches_the_reference() {
    let fixture = fixture();
    for case in fixture["normalize"].as_array().expect("normalize") {
        let input = case["input"].as_str().expect("input");
        assert_eq!(
            normalize_for_fuzzy_match(input),
            case["expected"].as_str().expect("expected"),
            "normalize mismatch for {input:?}"
        );
    }
}

#[test]
fn prepare_arguments_matches_the_reference_and_preserves_key_order() {
    let fixture = fixture();
    for case in fixture["prepare"].as_array().expect("prepare") {
        let name = case["name"].as_str().expect("name");
        let mut input = case.get("input").cloned().unwrap_or(Value::Null);
        let returned = prepare_edit_arguments(&mut input);
        // TS：legacy 折叠返回新对象（`{...rest, edits}`），其余情况返回同一个（就地改写的）
        // 对象。执行时的有效参数总是「返回值，否则原对象」。
        let effective = returned.unwrap_or(input);

        let expected = &case["output"];
        if expected.get("__undefined").and_then(Value::as_bool) == Some(true) {
            assert!(
                effective.is_null(),
                "undefined input should stay null-ish for {name}"
            );
            continue;
        }
        assert_eq!(
            serde_json::to_string(&effective).expect("serialize"),
            serde_json::to_string(expected).expect("serialize"),
            "prepare output mismatch for {name}"
        );
    }
}

#[test]
fn patch_headers_and_trailing_newline_follow_the_measured_reference() {
    // §8.3 的「67 个 = / 无尾随换行 / 相同内容为空串」与 diff@8.0.4 的实测不符。
    assert_eq!(
        kiana_tools::edit_diff::generate_unified_patch("foo.ts", "a\nb\nc\n", "a\nB\nc\n"),
        "--- foo.ts\n+++ foo.ts\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n"
    );
    assert_eq!(
        kiana_tools::edit_diff::generate_unified_patch("foo.ts", "same\n", "same\n"),
        "--- foo.ts\n+++ foo.ts\n"
    );
    assert_eq!(
        kiana_tools::edit_diff::generate_unified_patch("foo.ts", "", "a\n"),
        "--- foo.ts\n+++ foo.ts\n@@ -0,0 +1,1 @@\n+a\n"
    );
}

#[test]
fn normalize_preserves_the_documented_transformation_order() {
    // NFKC 先把全角空白/标点折成半角，再 trimEnd，最后做引号/破折号映射。
    assert_eq!(normalize_for_fuzzy_match("ＡＢＣ　１２３  \n"), "ABC 123\n");
    assert_eq!(normalize_for_fuzzy_match("“a” ‘b’"), "\"a\" 'b'");
    assert_eq!(normalize_for_fuzzy_match("a\u{2014}b\u{2212}c"), "a-b-c");
    assert_eq!(normalize_for_fuzzy_match("a\u{00a0}b"), "a b");
}

#[test]
fn diff_result_serializes_with_camel_case_and_omits_absent_first_line() {
    let result = generate_diff_string_with_context("a\n", "a\n", 4);
    let value = serde_json::to_value(&result).expect("serialize");
    assert_eq!(value, json!({ "diff": "" }));
    let result = generate_diff_string_with_context("a\n", "b\n", 4);
    let value = serde_json::to_value(&result).expect("serialize");
    assert_eq!(value["firstChangedLine"], json!(1));
}
