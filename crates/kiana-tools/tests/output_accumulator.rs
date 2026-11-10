//! `output_accumulator.rs` 的测试：由 spec §2.4 的样本、`tools.test.ts` 的
//! 「trailing newline is not an extra line」以及流式 UTF-8 / 临时文件行为推导而来。

use kiana_tools::output_accumulator::{
    OutputAccumulator, OutputAccumulatorError, OutputAccumulatorOptions,
};

fn accumulator(max_bytes: usize) -> OutputAccumulator {
    OutputAccumulator::new(OutputAccumulatorOptions {
        max_bytes: Some(max_bytes),
        ..OutputAccumulatorOptions::default()
    })
}

#[tokio::test]
async fn trailing_newline_is_not_an_extra_truncated_line() {
    let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions::default());
    accumulator.append(b"a\n").expect("append");
    accumulator.finish().expect("finish");
    let snapshot = accumulator.snapshot(false);
    assert_eq!(snapshot.truncation.total_lines, 1);
    assert!(!snapshot.truncation.truncated);
    assert_eq!(accumulator.get_last_line_bytes(), 0);
}

#[tokio::test]
async fn line_count_truncation_matches_the_bash_tool_expectations() {
    let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions::default());
    for index in 1..=4000 {
        accumulator
            .append(format!("line-{index}\n").as_bytes())
            .expect("append");
    }
    accumulator.finish().expect("finish");
    let snapshot = accumulator.snapshot(false);

    assert_eq!(snapshot.truncation.total_lines, 4000);
    assert_eq!(snapshot.truncation.output_lines, 2000);
    assert!(snapshot.truncation.truncated);
    assert_eq!(
        snapshot.truncation.truncated_by,
        Some(kiana_ext_api::tool_types::TruncatedBy::Lines)
    );
    assert!(snapshot.content.contains("line-2001"));
    assert!(snapshot.content.contains("line-4000"));
    assert!(!snapshot.content.contains("line-2000\n"));
    assert!(!snapshot.content.contains("4001"));
}

#[tokio::test]
async fn streaming_decoder_retains_a_split_emoji_across_appends() {
    let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions::default());
    let emoji = "\u{1F600}".as_bytes();
    accumulator.append(&emoji[..2]).expect("append");
    accumulator.append(&emoji[2..]).expect("append");
    accumulator.append(b"\n").expect("append");
    accumulator.finish().expect("finish");

    let snapshot = accumulator.snapshot(false);
    assert_eq!(snapshot.content, "\u{1F600}\n");
    // 原始 5 字节 = 4（emoji）+ 1（换行）；解码后同样是 5 字节。
    assert_eq!(accumulator.total_raw_bytes(), 5);
    assert_eq!(accumulator.total_decoded_bytes(), 5);
}

#[tokio::test]
async fn invalid_utf8_increases_decoded_byte_count() {
    let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions::default());
    // 单个 0xFF 非法字节 -> U+FFFD（3 字节）。
    accumulator.append(&[0xFF]).expect("append");
    accumulator.finish().expect("finish");
    assert_eq!(accumulator.total_raw_bytes(), 1);
    assert_eq!(accumulator.total_decoded_bytes(), 3);
    assert_eq!(accumulator.snapshot(false).content, "\u{FFFD}");
}

#[tokio::test]
async fn spills_to_a_temp_file_and_preserves_raw_bytes() {
    let mut accumulator = accumulator(32);
    let payload = b"0123456789abcdefghijklmnopqrstuvwxyz\n".repeat(4);
    accumulator.append(&payload).expect("append");
    accumulator.finish().expect("finish");

    let snapshot = accumulator.snapshot(true);
    assert!(snapshot.truncation.truncated);
    let path = snapshot.full_output_path.expect("temp file path");
    accumulator.close_temp_file().await.expect("close");

    let on_disk = std::fs::read(&path).expect("read temp file");
    assert_eq!(on_disk, payload);

    let full = accumulator
        .read_full_output(usize::MAX)
        .await
        .expect("read");
    assert!(!full.truncated);
    assert_eq!(full.content.as_bytes(), payload.as_slice());
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn read_full_output_omits_the_middle_with_the_marker() {
    let mut accumulator = accumulator(16);
    let payload = "abcdefghij".repeat(10);
    accumulator.append(payload.as_bytes()).expect("append");
    accumulator.finish().expect("finish");
    let _ = accumulator.snapshot(true);
    accumulator.close_temp_file().await.expect("close");

    let full = accumulator.read_full_output(20).await.expect("read");
    assert!(full.truncated);
    assert!(
        full.content.contains("[... 80 bytes omitted ...]"),
        "{}",
        full.content
    );
    assert!(full.content.starts_with("abcdefghij"));
    assert!(full.content.ends_with("abcdefghij"));
    // 省略字节数 = size - head - tail。
    assert_eq!(
        full.content,
        "abcdefghij\n\n[... 80 bytes omitted ...]\n\nabcdefghij"
    );
}

#[tokio::test]
async fn leading_bom_is_dropped_from_the_in_memory_full_output() {
    // TS `readFullOutput` 的 no-tempFile 分支：`new TextDecoder().decode(Buffer.concat(rawChunks))`
    // 会丢弃流开头的 BOM。
    let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions::default());
    accumulator.append(b"\xEF\xBB\xBFhi").expect("append");
    accumulator.finish().expect("finish");
    assert_eq!(accumulator.snapshot(false).content, "hi");

    let full = accumulator.read_full_output(24).await.expect("read");
    assert_eq!(full.content, "hi");
    assert!(!full.truncated);
}

#[tokio::test]
async fn leading_bom_is_dropped_from_the_temp_file_views() {
    // maxBytes = 16：解码后 100 字节触发落盘；原始字节（含 BOM）原样写入。
    let payload: Vec<u8> = [&b"\xEF\xBB\xBF"[..], "abcdefghij".repeat(10).as_bytes()].concat();
    let mut accumulator = accumulator(16);
    accumulator.append(&payload).expect("append");
    accumulator.finish().expect("finish");
    let snapshot = accumulator.snapshot(true);
    let path = snapshot.full_output_path.expect("temp file path");
    accumulator.close_temp_file().await.expect("close");
    assert!(
        std::fs::read(&path)
            .expect("read temp")
            .starts_with(b"\xEF\xBB\xBF")
    );
    assert_eq!(accumulator.total_raw_bytes(), 103);
    assert_eq!(accumulator.total_decoded_bytes(), 100);

    // size <= maxBytes：整文件解码，开头的 BOM 被丢弃。
    let full = accumulator.read_full_output(1000).await.expect("read");
    assert_eq!(full.content, "abcdefghij".repeat(10));
    assert!(!full.truncated);

    // 截断读取：head 从文件开头解码（BOM 丢弃），
    // 尾部跳过前导续字节后用新的 TextDecoder 解码。
    let full = accumulator.read_full_output(20).await.expect("read");
    assert_eq!(
        full.content,
        "abcdefg\n\n[... 83 bytes omitted ...]\n\nabcdefghij"
    );
    assert!(full.truncated);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn bom_bytes_do_not_count_toward_the_decoded_byte_limit() {
    // 50 KiB 边界：BOM + 51200 个 ASCII 字节。TS 的 totalDecodedBytes = 51200（BOM 已被
    // TextDecoder 丢弃），快照不被判为截断；修复前这里会因多出的 3 个 BOM 字节而 truncate。
    let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions::default());
    accumulator.append(b"\xEF\xBB\xBF").expect("append");
    accumulator.append(&vec![b'a'; 51200]).expect("append");
    accumulator.finish().expect("finish");
    let snapshot = accumulator.snapshot(false);
    assert!(!snapshot.truncation.truncated);
    assert_eq!(snapshot.truncation.total_bytes, 51200);
    assert_eq!(snapshot.truncation.total_lines, 1);
    assert_eq!(snapshot.content.len(), 51200);
    // 原始字节数仍然计入临时文件判定（TS 的 shouldUseTempFile 同样看 totalRawBytes）。
    let path = snapshot.full_output_path.expect("temp file path");
    accumulator.close_temp_file().await.expect("close");
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn read_full_output_drops_a_bom_at_the_tail_boundary() {
    // TS 对尾部同样新建 TextDecoder：尾部恰好以 BOM 开头时也会丢弃它。
    // payload = "abcdef" + BOM + "xy"（11 字节）；readFullOutput(9) → head 4 / tail 5。
    let payload: Vec<u8> = [&b"abcdef"[..], &b"\xEF\xBB\xBF"[..], &b"xy"[..]].concat();
    let mut accumulator = accumulator(8);
    accumulator.append(&payload).expect("append");
    accumulator.finish().expect("finish");
    let snapshot = accumulator.snapshot(true);
    let path = snapshot.full_output_path.expect("temp file path");
    accumulator.close_temp_file().await.expect("close");

    let full = accumulator.read_full_output(9).await.expect("read");
    assert_eq!(full.content, "abcd\n\n[... 2 bytes omitted ...]\n\nxy");
    assert!(full.truncated);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn append_after_finish_uses_the_verbatim_ts_error() {
    let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions::default());
    accumulator.finish().expect("finish");
    let error = accumulator.append(b"x").expect_err("must fail");
    assert!(matches!(error, OutputAccumulatorError::Finished));
    assert_eq!(
        error.to_string(),
        "Cannot append to a finished output accumulator"
    );
}

#[tokio::test]
async fn snapshot_uses_total_lines_and_tail_output_counts() {
    let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions {
        max_lines: Some(1),
        ..OutputAccumulatorOptions::default()
    });
    accumulator.append(b"one\ntwo\nthree\n").expect("append");
    accumulator.finish().expect("finish");
    let snapshot = accumulator.snapshot(false);
    assert!(snapshot.truncation.truncated);
    assert_eq!(
        snapshot.truncation.truncated_by,
        Some(kiana_ext_api::tool_types::TruncatedBy::Lines)
    );
    // totalLines/totalBytes 来自全量统计，outputLines/outputBytes 来自快照文本。
    assert_eq!(snapshot.truncation.total_lines, 3);
    assert_eq!(snapshot.truncation.total_bytes, 14);
    assert_eq!(snapshot.truncation.output_lines, 1);
    assert_eq!(snapshot.content, "three");
}

#[tokio::test]
async fn trimmed_tail_drops_the_partial_first_line() {
    let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions {
        max_bytes: Some(4),
        ..OutputAccumulatorOptions::default()
    });
    // maxRollingBytes = 8；填充超过 16 字节触发裁剪。
    for line in ["aaaa", "bbbb", "cccc", "dddd"] {
        accumulator
            .append(format!("{line}\n").as_bytes())
            .expect("append");
    }
    accumulator.finish().expect("finish");
    let snapshot = accumulator.snapshot(false);
    // 快照从完整行边界开始；不包含被裁掉的开头。
    assert!(!snapshot.content.contains("aaaa"));
    assert!(snapshot.content.contains("dddd"));
}

#[tokio::test]
async fn snapshot_temp_file_failure_does_not_swallow_the_decoder_flush() {
    // 回归（TS `snapshot` / `ensureTempFile`）：TS 的 `snapshot({persistIfTruncated})` 会从
    // ensureTempFile 同步抛出；冻结签名 `-> OutputSnapshot` 没有错误通道，所以失败只表现为
    // `fullOutputPath == None`。后续 append/finish 会重试创建并返回原始 I/O 错误，且 finish
    // 必须先 flush 解码器（临时文件错误不能丢掉尾部字节），第二次 finish 与无流时的
    // closeTempFile 与 TS 一样安静返回。
    // 用一个不存在的子目录让 createOutputFileStream 稳定失败（前缀参与文件名构造）。
    let missing_dir = format!(
        "kiana-output-missing-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions {
        max_bytes: Some(3),
        temp_file_prefix: Some(format!("{missing_dir}/chunk")),
        ..OutputAccumulatorOptions::default()
    });

    // 解码出 4 字节 > maxBytes：append 自己就会尝试落盘并失败（与 TS 相同）。
    let error = accumulator
        .append(b"abcd")
        .expect_err("temp file creation must fail");
    assert!(matches!(error, OutputAccumulatorError::Io(_)));

    // 再喂半个 emoji：解码器保留 2 字节，append 再次在 ensureTempFile 处失败。
    let emoji = "\u{1F600}".as_bytes();
    let error = accumulator
        .append(&emoji[..2])
        .expect_err("temp file creation must fail");
    assert!(matches!(error, OutputAccumulatorError::Io(_)));
    assert_eq!(accumulator.total_decoded_bytes(), 4);

    let snapshot = accumulator.snapshot(true);
    assert!(snapshot.truncation.truncated);
    assert!(snapshot.full_output_path.is_none());

    // finish：先 flush 半个 emoji -> U+FFFD（3 字节），再报告临时文件错误。
    let error = accumulator
        .finish()
        .expect_err("finish retries the temp file");
    assert!(matches!(error, OutputAccumulatorError::Io(_)));
    assert_eq!(accumulator.total_decoded_bytes(), 7);
    // TS：finish 已把 `finished` 置位，第二次调用直接返回。
    accumulator.finish().expect("second finish is a no-op");
    // TS：没有 tempFileStream 时 closeTempFile 直接 resolve。
    accumulator
        .close_temp_file()
        .await
        .expect("close without a stream");
}

#[tokio::test]
async fn get_last_line_bytes_tracks_the_open_line() {
    let mut accumulator = OutputAccumulator::new(OutputAccumulatorOptions::default());
    accumulator.append(b"abc").expect("append");
    assert_eq!(accumulator.get_last_line_bytes(), 3);
    accumulator.append(b"de\nxy").expect("append");
    assert_eq!(accumulator.get_last_line_bytes(), 2);
    accumulator.append(b"z").expect("append");
    assert_eq!(accumulator.get_last_line_bytes(), 3);
}

const GOLDEN: &str = include_str!("fixtures/primitives/accumulator.golden.json");

fn decode_chunks(value: &serde_json::Value) -> Vec<Vec<u8>> {
    value
        .as_array()
        .expect("chunks")
        .iter()
        .map(|chunk| {
            chunk
                .as_array()
                .expect("bytes")
                .iter()
                .map(|byte| byte.as_u64().expect("byte") as u8)
                .collect()
        })
        .collect()
}

fn options_from(value: &serde_json::Value) -> OutputAccumulatorOptions {
    OutputAccumulatorOptions {
        max_lines: value
            .get("maxLines")
            .and_then(serde_json::Value::as_u64)
            .map(|v| v as usize),
        max_bytes: value
            .get("maxBytes")
            .and_then(serde_json::Value::as_u64)
            .map(|v| v as usize),
        temp_file_prefix: None,
    }
}

#[tokio::test]
async fn matches_the_ts_golden_samples() {
    let cases: Vec<serde_json::Value> = serde_json::from_str(GOLDEN).expect("fixture");
    assert!(!cases.is_empty());
    for case in cases {
        let name = case["name"].as_str().expect("name");
        let mut accumulator = OutputAccumulator::new(options_from(&case["options"]));
        for chunk in decode_chunks(&case["chunks"]) {
            accumulator.append(&chunk).expect("append");
        }
        accumulator.finish().expect("finish");

        let snapshot = accumulator.snapshot(false);
        let persisted = accumulator.snapshot(true);
        assert_eq!(
            snapshot.content,
            case["content"].as_str().expect("content"),
            "{name}"
        );
        assert_eq!(
            serde_json::to_string(&snapshot.truncation).expect("serialize"),
            serde_json::to_string(&case["truncation"]).expect("serialize"),
            "{name} truncation"
        );
        assert_eq!(
            accumulator.get_last_line_bytes(),
            case["lastLineBytes"].as_u64().expect("lastLineBytes") as usize,
            "{name}"
        );
        assert_eq!(
            snapshot.full_output_path.is_some(),
            case["hasFullOutputPath"]
                .as_bool()
                .expect("hasFullOutputPath"),
            "{name}"
        );
        assert_eq!(
            persisted.full_output_path.is_some(),
            case["persistedHasPath"]
                .as_bool()
                .expect("persistedHasPath"),
            "{name}"
        );

        accumulator.close_temp_file().await.expect("close");
        let full = accumulator.read_full_output(24).await.expect("read");
        assert_eq!(
            full.content,
            case["fullOutput"]["content"]
                .as_str()
                .expect("full content"),
            "{name} full output"
        );
        assert_eq!(
            full.truncated,
            case["fullOutput"]["truncated"]
                .as_bool()
                .expect("full truncated"),
            "{name}"
        );
        if let Some(path) = snapshot.full_output_path {
            let _ = std::fs::remove_file(path);
        }
    }
}
