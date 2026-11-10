//! `packages/chord/test/output-skip.test.ts`（5 条）的 Rust 端口。
//!
//! `OutputSkip`：仅 tail 限制、跳过计数、清空已存块、endsWithNewline；以及"快照可以
//! 随时发生且不改变窗口"的模糊用例。确定性 PRNG（mulberry32）保证同一 seed 下每次运行产生相同序列。

use kiana_chord::output::{
    BoundedOutput, OutputBuffer, OutputChunk, OutputLimits, OutputSkip, Retain,
};

/// mulberry32：与 TS 测试相同的确定性 PRNG。
fn random(seed: u32) -> impl FnMut() -> f64 {
    let mut state = seed;
    move || {
        state = state.wrapping_add(0x6d2b_79f5);
        let mut t = state;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        f64::from(t ^ (t >> 14)) / 4_294_967_296.0
    }
}

/// 已解码的 shell 输出：ASCII、多字节/星光面字符、替换字符、CR/LF、tab，
/// 以及消毒时会被删除的控制字符。
const ALPHABET: [&str; 15] = [
    "a", "b", "z", " ", "\n", "\n", "\n", "\r\n", "\t", "é", "€", "😀", "\u{fffd}", "\u{1}",
    "\u{1b}",
];

fn random_chunk(next: &mut impl FnMut() -> f64) -> String {
    let length = (next() * 12.0) as usize;
    let mut text = String::new();
    for _ in 0..length {
        let index = (next() * ALPHABET.len() as f64) as usize;
        text.push_str(ALPHABET[index.min(ALPHABET.len() - 1)]);
    }
    text
}

fn measure(text: &str) -> OutputSkip {
    let newlines = text.bytes().filter(|byte| *byte == b'\n').count();
    OutputSkip {
        bytes: text.len(),
        newlines,
        ends_with_newline: text.ends_with('\n'),
    }
}

fn exceeds_window(text: &str, limits: &OutputLimits) -> bool {
    let skip = measure(text);
    skip.bytes > limits.max_bytes || skip.newlines > limits.max_lines
}

/// `text` 的码点边界，保证切分不会劈开代理对。
fn boundaries(text: &str) -> Vec<usize> {
    let mut result = Vec::new();
    let mut offset = 0usize;
    result.push(offset);
    for character in text.chars() {
        offset += character.len_utf8();
        result.push(offset);
    }
    result
}

/// 把随机输出完整喂给一个缓冲，并按 `ShellOutputInfo.skipped` 的契约把同一输出喂给
/// 另一个缓冲：它持有未投递文本，flush 时若剩余部分超出窗口，可以把前缀替换成计数。
/// 两个缓冲看到同样的输出时快照必须相同。
fn check_seed(seed: u32) {
    let mut next = random(seed);
    let limits = OutputLimits::new(
        1 + (next() * 40.0) as usize,
        1 + (next() * 5.0) as usize,
        Retain::Tail,
    );
    let mut full = OutputBuffer::new(limits);
    let mut skipping = OutputBuffer::new(limits);
    let mut pending = String::new();
    let mut skips = 0usize;
    let flush = |skipping: &mut OutputBuffer,
                 full: &mut OutputBuffer,
                 pending: &mut String,
                 next: &mut dyn FnMut() -> f64,
                 skips: &mut usize| {
        if pending.is_empty() {
            return;
        }
        let cuts: Vec<usize> = boundaries(pending)
            .into_iter()
            .filter(|cut| *cut > 0 && exceeds_window(&pending[*cut..], &limits))
            .collect();
        if !cuts.is_empty() && next() < 0.7 {
            let index = ((next() * cuts.len() as f64) as usize).min(cuts.len() - 1);
            let cut = cuts[index];
            let skipped = measure(&pending[..cut]);
            skipping
                .push(OutputChunk::Text(&pending[cut..]), Some(skipped))
                .expect("tail retention");
            *skips += 1;
        } else {
            skipping
                .push(OutputChunk::Text(pending), None)
                .expect("push");
        }
        pending.clear();
        // 进度快照发生在投递之间，并压缩已存块。
        if next() < 0.5 {
            skipping.snapshot();
        }
        assert_eq!(skipping.snapshot(), full.snapshot(), "seed {seed}");
    };
    let chunks = (next() * 40.0) as usize;
    for _ in 0..chunks {
        let chunk = random_chunk(&mut next);
        full.push_text(&chunk);
        if next() < 0.3 {
            full.snapshot();
        }
        pending.push_str(&chunk);
        if next() < 0.3 {
            flush(
                &mut skipping,
                &mut full,
                &mut pending,
                &mut next,
                &mut skips,
            );
        }
    }
    flush(
        &mut skipping,
        &mut full,
        &mut pending,
        &mut next,
        &mut skips,
    );
    full.end();
    skipping.end();
    assert_eq!(skipping.snapshot(), full.snapshot(), "seed {seed}");
    let _ = skips;
}

#[test]
fn keeps_the_same_tail_whenever_progress_snapshots_happen() {
    // 进度提交会在任意时刻快照；快照压缩存储，但绝不能改变窗口。
    // 曾经压缩到恰好是保留窗口，丢掉了决定之后窗口行首的字符。
    for seed in 1..=3000u32 {
        let mut next = random(seed);
        let limits = OutputLimits::new(
            1 + (next() * 40.0) as usize,
            1 + (next() * 5.0) as usize,
            Retain::Tail,
        );
        let mut plain = OutputBuffer::new(limits);
        let mut sampled = OutputBuffer::new(limits);
        let chunks = (next() * 30.0) as usize;
        for _ in 0..chunks {
            let chunk = random_chunk(&mut next);
            plain.push_text(&chunk);
            sampled.push_text(&chunk);
            if next() < 0.4 {
                sampled.snapshot();
            }
        }
        assert_eq!(sampled.snapshot(), plain.snapshot(), "seed {seed}");
    }
}

#[test]
fn matches_the_full_stream_for_every_legal_skip_pattern() {
    for seed in 1..=3000u32 {
        check_seed(seed);
    }
}

#[test]
fn counts_skipped_bytes_lines_and_the_final_newline_exactly() {
    let limits = OutputLimits::new(1000, 2, Retain::Tail);
    let mut full = OutputBuffer::new(limits);
    let mut skipping = OutputBuffer::new(limits);
    // 被跳过文本不以换行结尾，因此它的最后一行在投递文本里继续。
    let omitted = "one\ntwo\nthr";
    let kept = "ee\nfour\nfive\nsix";
    full.push_text(&format!("{omitted}{kept}"));
    skipping
        .push(OutputChunk::Text(kept), Some(measure(omitted)))
        .expect("tail retention");
    assert_eq!(skipping.snapshot(), full.snapshot());
    assert_eq!(
        skipping.snapshot(),
        BoundedOutput {
            text: "five\nsix".into(),
            dropped_bytes: 19,
            dropped_lines: 4,
        }
    );
}

#[test]
fn refuses_skips_for_head_retention() {
    let mut buffer = OutputBuffer::new(OutputLimits::new(10, 2, Retain::Head));
    let error = buffer
        .push(
            OutputChunk::Text("x\ny\nz\n"),
            Some(OutputSkip {
                bytes: 3,
                newlines: 1,
                ends_with_newline: true,
            }),
        )
        .expect_err("head retention");
    assert_eq!(error.message, "Skipped output requires tail retention");
}
