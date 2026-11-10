//! 与 Node 26.12.1 / ICU 78.3 的 `Intl.Segmenter("word")` 逐段对照。
//!
//! `fixtures/word_segments_node.json` 是用 Node 直接生成的黄金数据：
//! `[...new Intl.Segmenter(undefined, {granularity: "word"}).segment(text)]`，
//! 每条记录 `{ kind, text, segments: [[segment, index, isWordLike], ...] }`。
//! 语料覆盖 CJK 词典词、假名/汉字随机串、半角片假名与兼容汉字（NFKC 路径）、
//! 泰/老/高棉/缅词典词与随机串、前导组合标记、以及拉丁/标点/emoji/韩文混排。
//!
//! 这个测试是 [`kiana_tui::text::get_word_segments`] 分段行为的主要证据（黄金数据由 Node/ICU 生成）：
//! 规则引擎与词典引擎（`cjdict` 等 ICU 78.3 词典）两条腿都参与。

use kiana_tui::text::get_word_segments;
use serde_json::Value;

#[test]
fn node_intl_segmenter_parity() {
    let raw = include_str!("fixtures/word_segments_node.json");
    let cases: Vec<Value> = serde_json::from_str(raw).expect("fixture is valid JSON");
    assert!(cases.len() > 1000, "fixture should be non-trivial");

    let mut failures: Vec<String> = Vec::new();
    for case in &cases {
        let kind = case["kind"].as_str().unwrap_or("");
        let text = case["text"].as_str().expect("text is a string");
        let expected: Vec<(String, usize, bool)> = case["segments"]
            .as_array()
            .expect("segments array")
            .iter()
            .map(|segment| {
                (
                    segment[0].as_str().unwrap().to_string(),
                    segment[1].as_u64().unwrap() as usize,
                    segment[2].as_bool().unwrap(),
                )
            })
            .collect();
        let actual: Vec<(String, usize, bool)> = get_word_segments(text)
            .into_iter()
            .map(|segment| (segment.segment, segment.index, segment.is_word_like))
            .collect();
        if actual != expected {
            failures.push(format!(
                "[{kind}] {text:?}\n  expected: {expected:?}\n  actual:   {actual:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases diverge from Node Intl.Segmenter:\n{}",
        failures.len(),
        cases.len(),
        failures
            .iter()
            .take(25)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
