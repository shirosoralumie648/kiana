//! `modes/rpc/jsonl.ts` 的移植测试：严格 JSONL 框架 + Node `StringDecoder("utf8")` 语义。
//!
//! - 四个用例标题沿用 `test/rpc-jsonl.test.ts` 的原始标题。
//! - `tests/fixtures/jsonl/reader_cases.json` 由 Node v26.12.1 的
//!   `attachJsonlLineReader` 对同一组分块输入生成（黄金数据）。
//! - `tests/fixtures/jsonl/decoder_fuzz_cases.json` 由真实 TS `attachJsonlLineReader`
//!   对 437 组随机“字节 + 字符串”混合分块生成（`StringDecoder` 缓冲规则对拍）。
//!
//! describe: "RPC JSONL framing"

use kiana_rpc_print::jsonl::{
    JsonlLineReader, attach_jsonl_line_reader, pump_jsonl, serialize_json_line,
};
use serde_json::{Value, json};

const READER_FIXTURE: &str = include_str!("fixtures/jsonl/reader_cases.json");
const DECODER_FUZZ_FIXTURE: &str = include_str!("fixtures/jsonl/decoder_fuzz_cases.json");

fn from_hex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2), "hex string has even length");
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("valid hex"))
        .collect()
}

fn feed_case(case: &Value) -> Vec<String> {
    let mut reader = JsonlLineReader::new();
    let mut lines = Vec::new();
    for chunk in case["chunks"].as_array().expect("chunks") {
        match chunk["kind"].as_str().expect("chunk kind") {
            "text" => lines.extend(reader.push_str(chunk["text"].as_str().expect("text"))),
            "bytes" => {
                lines.extend(reader.push_bytes(&from_hex(chunk["hex"].as_str().expect("hex"))))
            }
            other => panic!("unknown chunk kind {other}"),
        }
    }
    lines.extend(reader.finish());
    lines
}

/// TS: "serializes strict JSONL records without escaping Unicode separators"
#[test]
fn serializes_strict_jsonl_records_without_escaping_unicode_separators() {
    let line = serialize_json_line(&json!({"text": "a\u{2028}b\u{2029}c"}));

    assert!(line.contains("a\u{2028}b\u{2029}c"));
    assert!(line.ends_with('\n'));
    let parsed: Value = serde_json::from_str(line.trim_end()).expect("valid JSON");
    assert_eq!(parsed, json!({"text": "a\u{2028}b\u{2029}c"}));
}

/// TS: "splits on LF only and preserves U+2028/U+2029 inside payloads"
#[test]
fn splits_on_lf_only_and_preserves_u2028_u2029_inside_payloads() {
    let payload = serialize_json_line(&json!({"text": "a\u{2028}b\u{2029}c"}));
    let mut reader = JsonlLineReader::new();
    let lines = reader.push_bytes(payload.as_bytes());
    assert_eq!(reader.finish(), Vec::<String>::new());
    assert_eq!(lines.len(), 1);
    let parsed: Value = serde_json::from_str(&lines[0]).expect("valid JSON");
    assert_eq!(parsed, json!({"text": "a\u{2028}b\u{2029}c"}));
}

/// TS: "handles CRLF-delimited input"
#[test]
fn handles_crlf_delimited_input() {
    let mut reader = JsonlLineReader::new();
    let lines = reader.push_bytes(b"{\"a\":1}\r\n{\"b\":2}\r\n");
    assert_eq!(
        lines,
        vec!["{\"a\":1}".to_string(), "{\"b\":2}".to_string()]
    );
}

/// TS: "emits a final line without trailing LF"
#[test]
fn emits_a_final_line_without_trailing_lf() {
    let mut reader = JsonlLineReader::new();
    assert!(reader.push_bytes(b"{\"a\":1}").is_empty());
    assert_eq!(reader.finish(), vec!["{\"a\":1}".to_string()]);
}

/// spec §4.1 的逐条规则：单 CR 剥离、空行、U+2028 不切分、无尾随 LF。
#[test]
fn framing_edges_match_the_spec() {
    // a\r\r\n -> "a\r"（只剥离一个 CR）。
    let mut reader = JsonlLineReader::new();
    assert_eq!(reader.push_str("a\r\r\n"), vec!["a\r".to_string()]);

    // 空行以 "" 回调；连续两个 LF 产生两行。
    let mut reader = JsonlLineReader::new();
    assert_eq!(
        reader.push_bytes(b"\n\n"),
        vec![String::new(), String::new()]
    );

    // U+2028 / U+2029 / NEL / VT / FF 都不是记录边界。
    let mut reader = JsonlLineReader::new();
    let text = "a\u{2028}b\u{2029}c\u{0085}d\u{000b}e\u{000c}f";
    assert!(reader.push_str(text).is_empty());
    assert_eq!(reader.finish(), vec![text.to_string()]);

    // 无尾随 LF 的最后一行；空 buffer 不回调。
    let mut reader = JsonlLineReader::new();
    assert!(reader.finish().is_empty());
}

/// 黄金数据：与 Node v26.12.1 `attachJsonlLineReader` 的分块行为逐字节一致
/// （含 §4.1.1 的 15 行替换表）。
#[test]
fn matches_node_string_decoder_replacement_table() {
    let fixture: Value = serde_json::from_str(READER_FIXTURE).expect("fixture JSON");
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let name = case["name"].as_str().expect("name");
        let expected: Vec<String> = case["lines"]
            .as_array()
            .expect("lines")
            .iter()
            .map(|line| line.as_str().expect("line").to_string())
            .collect();
        let actual = feed_case(case);
        if actual != expected {
            failures.push(format!("{name}: got {actual:?}, want {expected:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// `StringDecoder` 的“形状”缓冲规则：range 无效但形状上未完成的尾部字节也留到下一 chunk，
/// 字符 chunk 先进入行缓冲，替换字符随后出现（reviewer 回归：`e0 80` + `"\n"`）。
#[test]
fn range_invalid_partial_sequences_stay_buffered_across_string_chunks() {
    let mut reader = JsonlLineReader::new();
    assert!(reader.push_bytes(&[0xE0, 0x80]).is_empty());
    assert_eq!(reader.push_str("\n"), vec![String::new()]);
    assert_eq!(reader.finish(), vec!["\u{FFFD}\u{FFFD}".to_string()]);

    let mut reader = JsonlLineReader::new();
    assert!(reader.push_bytes(&[0xE0, 0x80]).is_empty());
    assert!(reader.push_str("x").is_empty());
    assert_eq!(reader.finish(), vec!["x\u{FFFD}\u{FFFD}".to_string()]);

    let mut reader = JsonlLineReader::new();
    assert!(reader.push_bytes(&[0xF0, 0x80, 0x80]).is_empty());
    assert!(reader.push_str("q").is_empty());
    assert_eq!(
        reader.push_bytes(b"\n"),
        vec!["q\u{FFFD}\u{FFFD}\u{FFFD}".to_string()]
    );
}

/// 黄金数据：437 组随机分块（字节 + 字符串混合）与 TS `attachJsonlLineReader` 逐行一致。
///
/// 由 `node tsattach.mjs` + `gen_fuzz.mjs` 在 Node v26.12.1 上对真实 TS 实现生成；
/// 覆盖 `StringDecoder` 的 `pending`/`missing` 状态迁移。
#[test]
fn matches_node_string_decoder_for_random_mixed_chunkings() {
    let fixture: Value = serde_json::from_str(DECODER_FUZZ_FIXTURE).expect("fixture JSON");
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().expect("cases") {
        let name = case["name"].as_str().expect("name");
        let expected: Vec<String> = case["lines"]
            .as_array()
            .expect("lines")
            .iter()
            .map(|line| line.as_str().expect("line").to_string())
            .collect();
        let actual = feed_case(case);
        if actual != expected {
            failures.push(format!("{name}: got {actual:?}, want {expected:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// 跨 chunk 的合法 UTF-8 序列不产生替换；被截断的合法前缀在 `finish` 时只补一个 U+FFFD。
#[test]
fn incomplete_sequences_are_held_until_the_next_chunk_or_finish() {
    let mut reader = JsonlLineReader::new();
    assert!(reader.push_bytes(&[0xe2, 0x82]).is_empty());
    assert_eq!(
        reader.push_bytes(&[0xa9, b'\n']),
        vec!["\u{20a9}".to_string()]
    );

    let mut reader = JsonlLineReader::new();
    assert!(reader.push_bytes(&[0xf0, 0x9f]).is_empty());
    assert_eq!(reader.finish(), vec!["\u{fffd}".to_string()]);
}

#[tokio::test]
async fn pump_jsonl_reads_to_eof_and_flushes_the_tail() {
    let input = b"{\"a\":1}\n{\"b\":2}".to_vec();
    let mut cursor: &[u8] = &input;
    let mut lines = Vec::new();
    pump_jsonl(&mut cursor, &mut |line| lines.push(line))
        .await
        .expect("pump");
    assert_eq!(
        lines,
        vec!["{\"a\":1}".to_string(), "{\"b\":2}".to_string()]
    );

    let mut cursor: &[u8] = &input;
    let mut lines = Vec::new();
    attach_jsonl_line_reader(&mut cursor, &mut |line| lines.push(line))
        .await
        .expect("attach");
    assert_eq!(lines.len(), 2);
}
