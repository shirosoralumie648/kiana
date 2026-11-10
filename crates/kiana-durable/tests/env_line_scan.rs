//! 移植自 `packages/durable/test/env-line-scan.test.ts`。
//!
//! 逐字复刻 TS 的伪随机生成器与分块策略：对 5000 个种子，`StreamDecoder` 的任意分块解码必须
//! 等于一次性解码；`LineScanner` 的选区必须等于“整份解码后按 `\n` 切分再取行”。

use kiana_durable::env::decode::{RangeDecoder, StreamDecoder, decode_whole};
use kiana_durable::env::{InvalidLineRange, LineScanner};

/// TS 测试里的 `random(seed)`：mulberry32。
fn random(seed: u32) -> impl FnMut() -> f64 {
    let mut state = seed;
    move || {
        state = state.wrapping_add(0x6d2b_79f5);
        let mut t = state;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        ((t ^ (t >> 14)) as f64) / 4_294_967_296.0
    }
}

/// 换行、ASCII、字节序标记、合法多字节、以及非法/截断序列。
const PIECES: &[&[u8]] = &[
    &[0x0a],
    &[0x0a],
    &[0x61],
    &[0x62, 0x63],
    &[0xef, 0xbb, 0xbf],
    &[0xc3, 0xa9],
    &[0xe2, 0x82, 0xac],
    &[0xf0, 0x9f, 0x98, 0x80],
    &[0xe2, 0x82],
    &[0xff],
    &[0x80],
    &[0xf0, 0x9f],
    &[0x0d, 0x0a],
];

fn random_file(next: &mut impl FnMut() -> f64) -> Vec<u8> {
    let pieces = (next() * 60.0).floor() as usize;
    let mut bytes = Vec::new();
    for _ in 0..pieces {
        let piece = PIECES[(next() * PIECES.len() as f64).floor() as usize];
        bytes.extend_from_slice(piece);
    }
    bytes
}

/// `new TextDecoder("utf-8", { ignoreBOM: from > 0 })`：`from == 0` 时丢弃流首 BOM。
fn decode_range(file: &[u8], from: usize, to: usize) -> String {
    let mut decoder = RangeDecoder::new();
    let mut text = decoder.decode(Some(&file[from..to]));
    text.push_str(&decoder.decode(None));
    if from == 0 {
        text.strip_prefix('\u{feff}')
            .map(str::to_owned)
            .unwrap_or(text)
    } else {
        text
    }
}

#[test]
fn keeps_a_u_feff_that_does_not_start_the_stream() {
    let bytes = [0xe2, 0x82, 0xef, 0xbb, 0xbf, 0x61];
    let mut decoder = StreamDecoder::new();
    let mut text = String::new();
    for byte in bytes {
        text.push_str(&decoder.decode(Some(&[byte])));
    }
    text.push_str(&decoder.decode(None));
    assert_eq!(text, decode_whole(&bytes));
    assert_eq!(text, "\u{fffd}\u{feff}a");
}

#[test]
fn decodes_any_chunking_like_decoding_the_whole_stream() {
    for seed in 1..=5000u32 {
        let mut next = random(seed);
        let bytes = random_file(&mut next);
        let mut decoder = StreamDecoder::new();
        let mut text = String::new();
        let mut offset = 0usize;
        while offset < bytes.len() {
            let size = 1 + (next() * 7.0).floor() as usize;
            let end = (offset + size).min(bytes.len());
            text.push_str(&decoder.decode(Some(&bytes[offset..end])));
            offset = end;
        }
        text.push_str(&decoder.decode(None));
        assert_eq!(text, decode_whole(&bytes), "seed {seed}");
    }
}

#[test]
fn agrees_with_decoding_and_splitting_the_whole_file() {
    for seed in 1..=5000u32 {
        let mut next = random(seed);
        let file = random_file(&mut next);
        let whole = decode_whole(&file);
        let lines: Vec<&str> = whole.split('\n').collect();
        let start_line = (next() * (lines.len() + 2) as f64).floor() as usize;
        let end_line = if next() < 0.3 {
            None
        } else {
            Some(start_line + 1 + (next() * (lines.len() + 1) as f64).floor() as usize)
        };
        let mut scanner = LineScanner::new(start_line as f64, end_line.map(|value| value as f64))
            .expect("valid range");
        let mut offset = 0usize;
        while offset < file.len() {
            let size = 1 + (next() * 7.0).floor() as usize;
            let end = (offset + size).min(file.len());
            scanner.push(&file[offset..end]);
            offset = end;
        }
        let scan = scanner.finish();
        let message = format!("seed {seed}");

        assert_eq!(scan.newlines, (lines.len() - 1) as u64, "{message}");
        let selected_end = end_line.unwrap_or(lines.len()).min(lines.len());
        let selected: Vec<&str> = if start_line >= lines.len() {
            Vec::new()
        } else {
            lines[start_line..selected_end].to_vec()
        };
        let selected_text = selected.join("\n");
        assert_eq!(
            decode_range(&file, scan.start, scan.end),
            selected_text,
            "{message}"
        );
        assert_eq!(
            scan.selected_bytes,
            kiana_chord::output::utf8_byte_length(&selected_text),
            "{message}"
        );

        if start_line < lines.len() {
            assert_eq!(
                decode_range(&file, scan.start, scan.first_line_end),
                lines[start_line],
                "{message}"
            );
            assert_eq!(
                scan.first_line_bytes,
                kiana_chord::output::utf8_byte_length(lines[start_line]),
                "{message}"
            );
            let last_line = selected_end.min(lines.len()) - 1;
            let last_end = if last_line + 1 < lines.len() {
                file[scan.last_line_start..]
                    .iter()
                    .position(|byte| *byte == 0x0a)
                    .map(|index| scan.last_line_start + index)
                    .expect("newline after the last selected line")
            } else {
                file.len()
            };
            assert_eq!(
                decode_range(&file, scan.last_line_start, last_end),
                lines[last_line],
                "{message}"
            );
        } else {
            assert_eq!(scan.start, file.len(), "{message}");
            assert_eq!(scan.end, file.len(), "{message}");
            assert_eq!(scan.selected_bytes, 0, "{message}");
        }
    }
}

#[test]
fn rejects_empty_or_invalid_ranges() {
    assert_eq!(
        LineScanner::new(2.0, Some(2.0)).unwrap_err(),
        InvalidLineRange
    );
    assert!(LineScanner::new(-1.0, None).is_err());
    assert!(LineScanner::new(1.5, None).is_err());
}
