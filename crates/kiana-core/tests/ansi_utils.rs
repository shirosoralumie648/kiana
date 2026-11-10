//! 移植 `packages/coding-agent/test/ansi-utils.test.ts`，并用 TS 参考实现生成的黄金语料
//! 覆盖 `stripAnsi` 与 `splitIncompleteAnsiSuffix`（`test/fixtures/ansi_corpus.json`）。
//!
//! 与 TS 的差异：`stripAnsi` 对非 string 抛 `TypeError` 的分支在 `&str` API 下不可表示
//! （编译期即拒绝），对应测试标 `#[ignore]` 并保留原标题。

use kiana_core::ansi::{split_incomplete_ansi_suffix, strip_ansi};

#[derive(serde::Deserialize)]
struct CorpusCase {
    input: String,
    output: Option<String>,
    strip: Option<String>,
    complete: Option<String>,
    pending: Option<String>,
    kind: Option<String>,
}

fn corpus() -> Vec<CorpusCase> {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/ansi_corpus.json"
    )))
    .expect("valid ansi corpus fixture")
}

#[test]
fn matches_chalk_strip_ansi_for_generated_compatibility_inputs() {
    let mut failures = Vec::new();
    for case in corpus() {
        let Some(expected) = case.output else {
            continue;
        };
        let actual = strip_ansi(&case.input);
        if actual != expected {
            failures.push(format!(
                "{:?}: expected {:?}, got {:?}",
                case.input, expected, actual
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
#[ignore = "&str API: non-string inputs are a compile error in Rust (documented deviation)"]
fn throws_the_same_typeerror_as_chalk_strip_ansi_for_non_string_values() {}

#[test]
fn strips_ris_without_leaking_the_final_byte() {
    assert_eq!(strip_ansi("\x1bcdone"), "done");
}

#[test]
fn strips_single_byte_esc_sequences_without_leaking_final_bytes() {
    for code in 'g' as u32..='m' as u32 {
        let input = format!("\x1b{}ok", char::from_u32(code).unwrap());
        assert_eq!(strip_ansi(&input), "ok");
    }
    for code in 'r' as u32..='t' as u32 {
        let input = format!("\x1b{}ok", char::from_u32(code).unwrap());
        assert_eq!(strip_ansi(&input), "ok");
    }
}

#[test]
fn strips_common_ansi_sequences_used_in_tool_output() {
    let input = "a\x1b[31mred\x1b[0m\x1b]8;;https://example.com\x07link\x1b]8;;\x07z";
    assert_eq!(strip_ansi(input), "aredlinkz");
}

/// 黄金语料：`splitIncompleteAnsiSuffix` 与 `stripAnsi` 的 TS 真实输出。
///
/// 覆盖 `spec/ca-tools-compaction-export.md` §2.28.5 的流式用例、256 code unit 窗口与
/// 代理对边界。
#[test]
fn matches_typescript_reference_corpus() {
    let mut failures = Vec::new();
    for case in corpus() {
        if case.kind.as_deref() != Some("split") {
            continue;
        }
        let split = split_incomplete_ansi_suffix(&case.input);
        if let Some(expected) = &case.complete
            && &split.complete != expected
        {
            failures.push(format!(
                "complete {:?}: expected {:?}, got {:?}",
                case.input, expected, split.complete
            ));
        }
        if let Some(expected) = &case.pending
            && &split.pending != expected
        {
            failures.push(format!(
                "pending {:?}: expected {:?}, got {:?}",
                case.input, expected, split.pending
            ));
        }
        if let Some(expected) = &case.strip {
            let actual = strip_ansi(&case.input);
            if &actual != expected {
                failures.push(format!(
                    "strip {:?}: expected {:?}, got {:?}",
                    case.input, expected, actual
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// §2.28.5 的三条流式验收（`bash-executor` 的「先拼 pending、再解码、再切分」顺序）。
#[test]
fn streaming_split_keeps_incomplete_escape_sequences_pending() {
    fn run_streaming(chunks: &[&str]) -> String {
        let mut pending = String::new();
        let mut output = String::new();
        for chunk in chunks {
            let split = split_incomplete_ansi_suffix(&format!("{pending}{chunk}"));
            pending = split.pending;
            output.push_str(&strip_ansi(&split.complete));
        }
        output.push_str(&strip_ansi(&pending));
        output
    }

    // ["\x1b[31mERROR: file.py:1\x1b[0", "m\n"] → "ERROR: file.py:1\n"
    assert_eq!(
        run_streaming(&["\x1b[31mERROR: file.py:1\x1b[0", "m\n"]),
        "ERROR: file.py:1\n"
    );

    // ["before\x1b", "[32mafter\n"] → "beforeafter\n"
    let split = split_incomplete_ansi_suffix("before\x1b");
    assert_eq!(split.complete, "before");
    assert_eq!(split.pending, "\x1b");
    assert_eq!(
        run_streaming(&["before\x1b", "[32mafter\n"]),
        "beforeafter\n"
    );

    // ["a\x1b]0;window ", "title\x1b", "\\b\n"] → "ab\n"
    // 第三块是字面反斜杠 + `b` + 换行（`ESC \` 才构成 ST）。
    assert_eq!(
        run_streaming(&["a\x1b]0;window ", "title\x1b", "\\b\n"]),
        "ab\n"
    );
}

/// `MAX_PENDING_ANSI_LENGTH = 256` 是窗口：更长的未终止序列不无限扣留。
#[test]
fn overly_long_unterminated_sequences_are_not_held_back() {
    let input = format!("\x1b]{}", "x".repeat(300));
    let split = split_incomplete_ansi_suffix(&input);
    assert_eq!(split.complete, input);
    assert_eq!(split.pending, "");
}
