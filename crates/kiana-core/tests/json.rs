//! `utils/json.ts` 的测试（TS 无对应测试文件；黄金语料由 TS 参考实现生成）。

use kiana_core::json::strip_json_comments;
use serde_json::Value;

#[test]
fn strips_line_comments_and_trailing_commas() {
    assert_eq!(strip_json_comments("// c\n{\"a\":1,}"), "\n{\"a\":1}");
    assert_eq!(
        strip_json_comments("{\"a\":\"//not comment\"}"),
        "{\"a\":\"//not comment\"}"
    );
    assert_eq!(
        strip_json_comments("{\"a\": [1,2,3,], \"b\": {\"c\": 1,},}"),
        "{\"a\": [1,2,3], \"b\": {\"c\": 1}}"
    );
}

/// 黄金语料：与 TS `stripJsonComments` 逐字比较（覆盖字符串字面量、`//`、尾随逗号与
/// JS `\s` 的 `﻿`/全角空格）。
#[test]
fn matches_typescript_reference_corpus() {
    let fixture: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/json_comments_corpus.json"
    )))
    .expect("valid json comments corpus fixture");

    let mut failures = Vec::new();
    for case in fixture.as_array().expect("fixture is an array") {
        let input = case["input"].as_str().expect("input string");
        let expected = case["output"].as_str().expect("output string");
        let actual = strip_json_comments(input);
        if actual != expected {
            failures.push(format!("{input:?}: expected {expected:?}, got {actual:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
