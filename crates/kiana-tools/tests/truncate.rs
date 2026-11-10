//! `truncate.rs` 的黄金测试（fixture 由 TS 参考实现 `truncate.ts` 生成）。

use kiana_tools::truncate::{
    GREP_MAX_LINE_LENGTH, TruncationOptions, format_size, truncate_head, truncate_line,
    truncate_middle, truncate_tail,
};
use serde_json::Value;

const GOLDEN: &str = include_str!("fixtures/primitives/truncate.golden.json");
const GOLDEN_RANDOM: &str = include_str!("fixtures/primitives/truncate.random.golden.json");

fn golden() -> Value {
    serde_json::from_str(GOLDEN).expect("fixture parses")
}

fn options_from(value: &Value) -> TruncationOptions {
    TruncationOptions {
        max_lines: value
            .get("maxLines")
            .and_then(Value::as_u64)
            .map(|v| v as usize),
        max_bytes: value
            .get("maxBytes")
            .and_then(Value::as_u64)
            .map(|v| v as usize),
    }
}

fn assert_json_equal(actual: &impl serde::Serialize, expected: &Value, context: &str) {
    let actual = serde_json::to_value(actual).expect("serializes");
    assert_eq!(
        serde_json::to_string(&actual).expect("serializes"),
        serde_json::to_string(expected).expect("serializes"),
        "{context}"
    );
}

#[test]
fn truncate_head_and_tail_match_the_ts_reference() {
    let fixture = golden();
    let cases = fixture["truncations"]
        .as_array()
        .expect("truncations array");
    assert!(!cases.is_empty());
    for case in cases {
        let content = case["content"].as_str().expect("content");
        let options = options_from(&case["options"]);
        let context = format!("content={content:?} options={options:?}");
        assert_json_equal(
            &truncate_head(content, &options),
            &case["head"],
            &format!("head {context}"),
        );
        assert_json_equal(
            &truncate_tail(content, &options),
            &case["tail"],
            &format!("tail {context}"),
        );
    }
}

#[test]
fn format_size_matches_js_to_fixed() {
    let fixture = golden();
    for pair in fixture["formatSize"].as_array().expect("formatSize") {
        let bytes = pair[0].as_u64().expect("bytes");
        let expected = pair[1].as_str().expect("formatted");
        assert_eq!(format_size(bytes), expected, "{bytes}");
    }
    // JS 的中点远离零（spec §8.4 必测用例）。注意 `formatSize(256)` / `formatSize(768)`
    // 先命中 `bytes < 1024` 的裸字节分支（TS 源码为准），中点只在 KB 分支可见。
    assert_eq!(format_size(256), "256B");
    assert_eq!(format_size(768), "768B");
    assert_eq!(format_size(1280), "1.3KB");
    assert_eq!(format_size(2304), "2.3KB");
    assert_eq!(format_size(5120), "5.0KB");
    assert_eq!(format_size(1023), "1023B");
    assert_eq!(format_size(1536), "1.5KB");
    assert_eq!(format_size(1_048_575), "1024.0KB");
    assert_eq!(format_size(1_048_576), "1.0MB");
}

#[test]
fn truncate_line_matches_the_ts_reference() {
    let fixture = golden();
    for case in fixture["truncateLine"].as_array().expect("truncateLine") {
        let line = case[0].as_str().expect("line");
        let max_chars = case[1].as_u64().expect("maxChars") as usize;
        let expected = &case[2];
        let actual = truncate_line(line, max_chars);
        assert_eq!(actual.text, expected["text"].as_str().expect("text"));
        assert_eq!(
            actual.was_truncated,
            expected["wasTruncated"].as_bool().expect("wasTruncated")
        );
    }
    // 默认上限。
    let long = "x".repeat(GREP_MAX_LINE_LENGTH + 1);
    assert!(truncate_line(&long, GREP_MAX_LINE_LENGTH).was_truncated);
}

#[test]
fn truncate_middle_matches_the_ts_reference() {
    let fixture = golden();
    for case in fixture["truncateMiddle"]
        .as_array()
        .expect("truncateMiddle")
    {
        let content = case[0].as_str().expect("content");
        let max_bytes = case[1].as_u64().expect("maxBytes") as usize;
        let expected = &case[2];
        let actual = truncate_middle(content, max_bytes);
        assert_eq!(
            actual.content,
            expected["content"].as_str().expect("content")
        );
        assert_eq!(
            actual.truncated,
            expected["truncated"].as_bool().expect("truncated")
        );
        assert_eq!(
            actual.removed_chars,
            expected["removedChars"].as_u64().expect("removedChars")
        );
        assert_eq!(
            actual.total_bytes,
            expected["totalBytes"].as_u64().expect("totalBytes")
        );
        assert_eq!(
            actual.total_lines,
            expected["totalLines"].as_u64().expect("totalLines")
        );
    }
}

#[test]
fn truncation_result_serializes_with_ts_key_order_and_null_truncated_by() {
    let result = truncate_head("abc", &TruncationOptions::default());
    let serialized = serde_json::to_string(&result).expect("serializes");
    assert_eq!(
        serialized,
        r#"{"content":"abc","truncated":false,"truncatedBy":null,"totalLines":1,"totalBytes":3,"outputLines":1,"outputBytes":3,"lastLinePartial":false,"firstLineExceedsLimit":false,"maxLines":2000,"maxBytes":51200}"#
    );
}

/// 随机差分用例（400 个 head/tail + 120 个 middle，由 TS 参考实现生成）。
///
/// 覆盖 CRLF、U+2028、BOM、emoji、零上限（`maxLines: 0` / `maxBytes: 0`）与随机截断点。
#[test]
fn randomized_truncation_matches_the_ts_reference() {
    let fixture: Value = serde_json::from_str(GOLDEN_RANDOM).expect("random fixture parses");
    let cases = fixture["truncations"].as_array().expect("truncations");
    assert!(cases.len() >= 400);
    for (index, case) in cases.iter().enumerate() {
        let content = case["content"].as_str().expect("content");
        let options = options_from(&case["options"]);
        let context = format!("case {index}: content={content:?} options={options:?}");
        assert_json_equal(
            &truncate_head(content, &options),
            &case["head"],
            &format!("head {context}"),
        );
        assert_json_equal(
            &truncate_tail(content, &options),
            &case["tail"],
            &format!("tail {context}"),
        );
    }
    for (index, case) in fixture["truncateMiddle"]
        .as_array()
        .expect("truncateMiddle")
        .iter()
        .enumerate()
    {
        let content = case[0].as_str().expect("content");
        let max_bytes = case[1].as_u64().expect("maxBytes") as usize;
        let expected = &case[2];
        let actual = truncate_middle(content, max_bytes);
        assert_eq!(
            actual.content,
            expected["content"].as_str().expect("content"),
            "middle case {index}: content={content:?} maxBytes={max_bytes}"
        );
        assert_eq!(
            actual.removed_chars,
            expected["removedChars"].as_u64().expect("removedChars"),
            "middle case {index}"
        );
        assert_eq!(
            actual.total_bytes,
            expected["totalBytes"].as_u64().expect("totalBytes"),
            "middle case {index}"
        );
        assert_eq!(
            actual.total_lines,
            expected["totalLines"].as_u64().expect("totalLines"),
            "middle case {index}"
        );
    }
}

/// `formatSize` 的 3117 个差分样本（0..2100 连续 + 1000 个随机大值 + 边界）。
///
/// 覆盖 JS `toFixed(1)` 的半数远离零语义（`256B`/`768B` 命中裸字节分支，
/// `1280→"1.3KB"`/`2304→"2.3KB"` 命中 KB 中点）。
#[test]
fn format_size_matches_the_ts_reference_on_a_wide_range() {
    let fixture: Value = serde_json::from_str(GOLDEN_RANDOM).expect("random fixture parses");
    let pairs = fixture["formatSize"].as_array().expect("formatSize");
    assert!(pairs.len() >= 3000);
    for pair in pairs {
        let bytes = pair[0].as_u64().expect("bytes");
        let expected = pair[1].as_str().expect("formatted");
        assert_eq!(format_size(bytes), expected, "{bytes}");
    }
}
