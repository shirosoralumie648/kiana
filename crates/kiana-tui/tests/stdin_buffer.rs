//! `test/stdin-buffer.test.ts` 的移植（66 个 it）。
//!
//! TS 用 `node:test` 的 mock timers 推进 `setTimeout`；Rust 侧 [`StdinBuffer`] 把时间抽象成
//! `process_at(now, …)` 与 `poll_timeout(now)`，测试用虚拟时钟（[`Harness::tick`]）得到与 TS
//! `t.mock.timers.tick(ms)` 完全相同的确定性行为。

use std::time::{Duration, Instant};

use kiana_tui::keys::matches_key;
use kiana_tui::stdin::{StdinBuffer, StdinBufferOptions, StdinEvent};

/// 测试夹具：buffer + 虚拟时钟 + 已收到的事件。
struct Harness {
    buffer: StdinBuffer,
    now: Instant,
    data: Vec<String>,
    paste: Vec<String>,
}

impl Harness {
    fn with_options(options: StdinBufferOptions) -> Self {
        Harness {
            buffer: StdinBuffer::new(options),
            now: Instant::now(),
            data: Vec::new(),
            paste: Vec::new(),
        }
    }

    /// `new StdinBuffer({ timeout: 10 })`。
    fn with_timeout(ms: u64) -> Self {
        Self::with_options(StdinBufferOptions {
            timeout: Some(ms),
            escape_timeout: None,
        })
    }

    /// `new StdinBuffer()`。
    fn with_defaults() -> Self {
        Self::with_options(StdinBufferOptions::default())
    }

    fn process(&mut self, data: &str) {
        let events = self.buffer.process_str_at(self.now, data);
        self.collect(events);
    }

    fn process_bytes(&mut self, data: &[u8]) {
        let events = self.buffer.process_at(self.now, data);
        self.collect(events);
    }

    /// `t.mock.timers.tick(ms)`：推进虚拟时钟并执行到期回调。
    fn tick(&mut self, ms: u64) {
        self.now += Duration::from_millis(ms);
        let events = self.buffer.poll_timeout(self.now);
        self.collect(events);
    }

    fn collect(&mut self, events: Vec<StdinEvent>) {
        for event in events {
            match event {
                StdinEvent::Data(sequence) => self.data.push(sequence),
                StdinEvent::Paste(content) => self.paste.push(content),
            }
        }
    }

    fn flush(&mut self) -> Vec<String> {
        self.buffer.flush()
    }

    fn buffer(&self) -> &str {
        self.buffer.get_buffer()
    }
}

// ---------------------------------------------------------------------------
// Regular Characters
// ---------------------------------------------------------------------------

#[test]
fn passes_through_regular_characters_immediately() {
    let mut harness = Harness::with_timeout(10);
    harness.process("a");
    assert_eq!(harness.data, ["a"]);
}

#[test]
fn passes_through_multiple_regular_characters() {
    let mut harness = Harness::with_timeout(10);
    harness.process("abc");
    assert_eq!(harness.data, ["a", "b", "c"]);
}

#[test]
fn handles_unicode_characters() {
    let mut harness = Harness::with_timeout(10);
    harness.process("hello 世界");
    assert_eq!(harness.data, ["h", "e", "l", "l", "o", " ", "世", "界"]);
}

/// 非 BMP 字符（被迫偏差，见 `src/stdin.rs` 模块文档）。
///
/// TS 的 `extractCompleteSequences` 按 UTF-16 code unit 切分，`"🎉"`（两个 code unit）会发出
/// **两个** `data` 事件（两个孤立代理项）；Rust 的 `String` 不能表示孤立代理项，事件类型
/// `StdinEvent::Data(String)` 只能把它作为一个标量发出。下游把它当文本插入，最终文本一致。
#[test]
fn emits_a_non_bmp_character_as_one_scalar_event() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\u{1F389}");
    assert_eq!(harness.data, ["\u{1F389}"]);

    // ESC + 非 BMP：TS 是 `["\u001b\uD83C", "\uDF89"]` 两个事件，Rust 是一个。
    let mut harness = Harness::with_timeout(10);
    harness.process("\u{1b}\u{1F389}");
    assert_eq!(harness.data, ["\u{1b}\u{1F389}"]);
}

// ---------------------------------------------------------------------------
// Complete Escape Sequences
// ---------------------------------------------------------------------------

#[test]
fn passes_through_complete_mouse_sgr_sequences() {
    let mut harness = Harness::with_timeout(10);
    let mouse_seq = "\x1b[<35;20;5m";
    harness.process(mouse_seq);
    assert_eq!(harness.data, [mouse_seq]);
}

#[test]
fn passes_through_complete_arrow_key_sequences() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[A");
    assert_eq!(harness.data, ["\x1b[A"]);
}

#[test]
fn passes_through_complete_function_key_sequences() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[11~");
    assert_eq!(harness.data, ["\x1b[11~"]);
}

#[test]
fn passes_through_meta_key_sequences() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1ba");
    assert_eq!(harness.data, ["\x1ba"]);
}

#[test]
fn passes_through_ss3_sequences() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1bOA");
    assert_eq!(harness.data, ["\x1bOA"]);
}

// ---------------------------------------------------------------------------
// Partial Escape Sequences
// ---------------------------------------------------------------------------

#[test]
fn buffers_incomplete_mouse_sgr_sequence() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b");
    assert!(harness.data.is_empty());
    assert_eq!(harness.buffer(), "\x1b");

    harness.process("[<35");
    assert!(harness.data.is_empty());
    assert_eq!(harness.buffer(), "\x1b[<35");

    harness.process(";20;5m");
    assert_eq!(harness.data, ["\x1b[<35;20;5m"]);
    assert_eq!(harness.buffer(), "");
}

#[test]
fn buffers_incomplete_csi_sequence() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[");
    assert!(harness.data.is_empty());

    harness.process("1;");
    assert!(harness.data.is_empty());

    harness.process("5H");
    assert_eq!(harness.data, ["\x1b[1;5H"]);
}

#[test]
fn buffers_split_across_many_chunks() {
    let mut harness = Harness::with_timeout(10);
    for chunk in ["\x1b", "[", "<", "3", "5", ";", "2", "0", ";", "5", "m"] {
        harness.process(chunk);
    }
    assert_eq!(harness.data, ["\x1b[<35;20;5m"]);
}

#[test]
fn flushes_incomplete_sequence_after_timeout() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[<35");
    assert!(harness.data.is_empty());

    harness.tick(15);
    assert_eq!(harness.data, ["\x1b[<35"]);
}

#[test]
fn flushes_a_lone_esc_as_escape_when_cr_arrives_after_the_timeout() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b");
    harness.tick(20);
    harness.process("\r");

    assert_eq!(harness.data, ["\x1b", "\r"]);
    assert!(matches_key(&harness.data[0], "escape"));
}

#[test]
fn merges_esc_and_cr_split_across_chunks_within_a_larger_timeout() {
    let mut harness = Harness::with_options(StdinBufferOptions {
        timeout: None,
        escape_timeout: Some(100),
    });
    harness.process("\x1b");
    harness.tick(20);
    harness.process("\r");

    assert_eq!(harness.data, ["\x1b\r"]);
    assert!(matches_key(&harness.data[0], "alt+enter"));
}

#[test]
fn does_not_apply_the_sequence_timeout_to_a_lone_esc() {
    let mut harness = Harness::with_options(StdinBufferOptions {
        timeout: Some(100),
        escape_timeout: None,
    });
    harness.process("\x1b");
    harness.tick(20);
    harness.process("\r");

    assert_eq!(harness.data, ["\x1b", "\r"]);
    assert!(matches_key(&harness.data[0], "escape"));
}

#[test]
fn keeps_fragmented_mouse_sequences_buffered_across_delayed_chunks_by_default() {
    let mut harness = Harness::with_defaults();
    harness.process("\x1b[");
    harness.tick(20);
    assert!(harness.data.is_empty());
    harness.process("<65;48;39M");
    assert_eq!(harness.data, ["\x1b[<65;48;39M"]);
}

// ---------------------------------------------------------------------------
// Terminal Reply Strings
// ---------------------------------------------------------------------------

/// Ghostty's Ayu Light palette color 1.
const REPLY_PALETTE: &str = "\x1b]4;1;rgb:eaea/6c6c/6d6d\x07";
const REPLY_FOREGROUND: &str = "\x1b]10;rgb:ffff/ffff/ffff\x1b\\";
const REPLY_BACKGROUND: &str = "\x1b]11;rgb:0000/0000/0000\x07";
const REPLY_PALETTE_TAIL: &str = "\x1b]4;1;rgb:6464/6464/4d4d\x07";
const REPLY_XTVERSION: &str = "\x1bP>|Ghostty 1.3.1\x1b\\";
const REPLY_KITTY_GRAPHICS: &str = "\x1b_Gi=1;OK\x1b\\";
const REPLY_PROGRAM_STATUS: &str = "\x1b]7501;?\x1b\\";

const REPLIES: [(&str, &str); 7] = [
    ("\x1b]4;1;", REPLY_PALETTE),
    ("\x1b]10;", REPLY_FOREGROUND),
    ("\x1b]11;", REPLY_BACKGROUND),
    ("\x1b]4;1;", REPLY_PALETTE_TAIL),
    ("\x1bP>|", REPLY_XTVERSION),
    ("\x1b_Gi=1;", REPLY_KITTY_GRAPHICS),
    ("\x1b]7501;?", REPLY_PROGRAM_STATUS),
];

#[test]
fn keeps_delayed_replies_intact_at_every_split_after_their_header() {
    for (header, reply) in REPLIES {
        for split in header.len()..reply.len() {
            let mut harness = Harness::with_defaults();
            harness.process(&reply[..split]);
            harness.tick(5000);
            harness.process(&format!("{}x", &reply[split..]));
            assert!(
                !harness
                    .data
                    .iter()
                    .any(|sequence| matches_key(sequence, "ctrl+g")),
                "split {split} of {reply:?} turned into Ctrl+G: {:?}",
                harness.data
            );
            assert_eq!(harness.data, [reply, "x"], "split {split} of {reply:?}");
        }
    }
}

#[test]
fn keeps_an_osc_reply_buffered_across_multiple_expired_sequence_timeouts() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b]4;1;rgb:eaea/6c");
    harness.tick(100);
    assert!(harness.data.is_empty());
    harness.process("6c/6d6d");
    harness.tick(100);
    assert!(harness.data.is_empty());
    harness.process("\x07");
    assert_eq!(harness.data, [REPLY_PALETTE]);
}

#[test]
fn still_times_out_bare_string_introducers_as_alt_keys() {
    let mut harness = Harness::with_timeout(10);
    for prefix in ["\x1b]", "\x1bP", "\x1b_"] {
        harness.process(prefix);
        harness.tick(10);
    }
    assert_eq!(harness.data, ["\x1b]", "\x1bP", "\x1b_"]);
}

const AMBIGUOUS: [&str; 11] = [
    "\x1b]a",
    "\x1b]1",
    "\x1b]10",
    "\x1b]0;title",
    "\x1b]4;",
    "\x1b]4;a;",
    "\x1bPa",
    "\x1bP>",
    "\x1b_a",
    "\x1b_G",
    "\x1b_Gi=1",
];

#[test]
fn recovers_batched_legacy_alt_input_within_the_sequence_timeout() {
    for ambiguous in AMBIGUOUS {
        let mut harness = Harness::with_defaults();
        harness.process(ambiguous);
        harness.tick(20);
        harness.process("x\r\x03\x04");
        harness.tick(49);
        assert!(harness.data.is_empty(), "{ambiguous:?}");
        harness.tick(1);
        let mut expected: Vec<String> = vec![ambiguous[..2].to_string()];
        expected.extend(ambiguous[2..].chars().map(|ch| ch.to_string()));
        expected.extend(["x", "\r", "\x03", "\x04"].map(str::to_string));
        assert_eq!(harness.data, expected, "{ambiguous:?}");
        assert_eq!(harness.buffer(), "");

        harness.process("y\r\x03\x04");
        assert_eq!(
            &harness.data[harness.data.len() - 4..],
            ["y", "\r", "\x03", "\x04"]
        );
    }
}

#[test]
fn recognizes_a_reply_header_assembled_within_the_ambiguous_input_timeout() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b]1");
    harness.tick(5);
    harness.process("0;");
    harness.tick(5000);
    assert!(harness.data.is_empty());
    harness.process("rgb:ffff/ffff/ffff\x1b\\");
    assert_eq!(harness.data, [REPLY_FOREGROUND]);
}

#[test]
fn recovers_from_an_unterminated_reply_when_a_new_escape_sequence_arrives() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b]4;1;rgb:eaea/6c");
    harness.tick(100);
    harness.process("\x1b");
    harness.tick(100);
    harness.process("[Ahello");
    assert_eq!(harness.data, ["\x1b[A", "h", "e", "l", "l", "o"]);
}

#[test]
fn discards_a_canceled_reply_without_swallowing_subsequent_input() {
    let mut harness = Harness::with_timeout(10);
    for cancel in ["\x18", "\x1a"] {
        harness.process(&format!("\x1b]11;rgb:0000{cancel}x"));
    }
    assert_eq!(harness.data, ["x", "x"]);
}

// ---------------------------------------------------------------------------
// Mixed Content
// ---------------------------------------------------------------------------

#[test]
fn handles_characters_followed_by_escape_sequence() {
    let mut harness = Harness::with_timeout(10);
    harness.process("abc\x1b[A");
    assert_eq!(harness.data, ["a", "b", "c", "\x1b[A"]);
}

#[test]
fn handles_escape_sequence_followed_by_characters() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[Aabc");
    assert_eq!(harness.data, ["\x1b[A", "a", "b", "c"]);
}

#[test]
fn handles_multiple_complete_sequences() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[A\x1b[B\x1b[C");
    assert_eq!(harness.data, ["\x1b[A", "\x1b[B", "\x1b[C"]);
}

#[test]
fn handles_partial_sequence_with_preceding_characters() {
    let mut harness = Harness::with_timeout(10);
    harness.process("abc\x1b[<35");
    assert_eq!(harness.data, ["a", "b", "c"]);
    assert_eq!(harness.buffer(), "\x1b[<35");

    harness.process(";20;5m");
    assert_eq!(harness.data, ["a", "b", "c", "\x1b[<35;20;5m"]);
}

// ---------------------------------------------------------------------------
// Kitty Keyboard Protocol
// ---------------------------------------------------------------------------

#[test]
fn handles_kitty_csi_u_press_events() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[97u");
    assert_eq!(harness.data, ["\x1b[97u"]);
}

#[test]
fn handles_kitty_csi_u_release_events() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[97;1:3u");
    assert_eq!(harness.data, ["\x1b[97;1:3u"]);
}

#[test]
fn handles_batched_kitty_press_and_release() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[97u\x1b[97;1:3u");
    assert_eq!(harness.data, ["\x1b[97u", "\x1b[97;1:3u"]);
}

#[test]
fn handles_multiple_batched_kitty_events() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[97u\x1b[97;1:3u\x1b[98u\x1b[98;1:3u");
    assert_eq!(
        harness.data,
        ["\x1b[97u", "\x1b[97;1:3u", "\x1b[98u", "\x1b[98;1:3u"]
    );
}

#[test]
fn handles_kitty_arrow_keys_with_event_type() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[1;1:1A");
    assert_eq!(harness.data, ["\x1b[1;1:1A"]);
}

#[test]
fn handles_kitty_functional_keys_with_event_type() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[3;1:3~");
    assert_eq!(harness.data, ["\x1b[3;1:3~"]);
}

#[test]
fn splits_esc_esc_csi_into_standalone_esc_and_the_csi_sequence() {
    // WezTerm Escape key regression（Kitty press 为裸 ESC，release 为完整 CSI-u）。
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b\x1b[27;129:3u");
    assert_eq!(harness.data, ["\x1b", "\x1b[27;129:3u"]);
}

#[test]
fn splits_esc_esc_csi_with_no_modifier() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b\x1b[27;1:3u");
    assert_eq!(harness.data, ["\x1b", "\x1b[27;1:3u"]);
}

#[test]
fn still_emits_esc_esc_as_a_single_sequence_when_not_followed_by_a_new_escape() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b\x1b");
    assert_eq!(harness.data, ["\x1b\x1b"]);
}

#[test]
fn handles_plain_characters_mixed_with_kitty_sequences() {
    let mut harness = Harness::with_timeout(10);
    harness.process("a\x1b[97;1:3u");
    assert_eq!(harness.data, ["a", "\x1b[97;1:3u"]);
}

#[test]
fn drops_raw_duplicate_character_after_matching_kitty_printable_sequence() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[224uà");
    assert_eq!(harness.data, ["\x1b[224u"]);
}

#[test]
fn drops_raw_duplicate_character_after_matching_kitty_printable_sequence_across_chunks() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[64u");
    harness.process("@");
    assert_eq!(harness.data, ["\x1b[64u"]);
}

#[test]
fn keeps_non_matching_plain_character_after_kitty_printable_sequence() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[97ub");
    assert_eq!(harness.data, ["\x1b[97u", "b"]);
}

#[test]
fn keeps_raw_character_after_modified_kitty_printable_sequence() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[64;3u@");
    assert_eq!(harness.data, ["\x1b[64;3u", "@"]);
}

#[test]
fn handles_rapid_typing_simulation_with_kitty_protocol() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[104u\x1b[104;1:3u\x1b[105u\x1b[105;1:3u");
    assert_eq!(
        harness.data,
        ["\x1b[104u", "\x1b[104;1:3u", "\x1b[105u", "\x1b[105;1:3u"]
    );
}

// ---------------------------------------------------------------------------
// Mouse Events
// ---------------------------------------------------------------------------

#[test]
fn handles_mouse_press_event() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[<0;10;5M");
    assert_eq!(harness.data, ["\x1b[<0;10;5M"]);
}

#[test]
fn handles_mouse_release_event() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[<0;10;5m");
    assert_eq!(harness.data, ["\x1b[<0;10;5m"]);
}

#[test]
fn handles_mouse_move_event() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[<35;20;5m");
    assert_eq!(harness.data, ["\x1b[<35;20;5m"]);
}

#[test]
fn handles_split_mouse_events() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[<3");
    harness.process("5;1");
    harness.process("5;");
    harness.process("10m");
    assert_eq!(harness.data, ["\x1b[<35;15;10m"]);
}

#[test]
fn handles_multiple_mouse_events() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[<35;1;1m\x1b[<35;2;2m\x1b[<35;3;3m");
    assert_eq!(
        harness.data,
        ["\x1b[<35;1;1m", "\x1b[<35;2;2m", "\x1b[<35;3;3m"]
    );
}

#[test]
fn handles_old_style_mouse_sequence() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[M abc");
    assert_eq!(harness.data, ["\x1b[M ab", "c"]);
}

#[test]
fn buffers_incomplete_old_style_mouse_sequence() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[M");
    assert_eq!(harness.buffer(), "\x1b[M");

    harness.process(" a");
    assert_eq!(harness.buffer(), "\x1b[M a");

    harness.process("b");
    assert_eq!(harness.data, ["\x1b[M ab"]);
}

// ---------------------------------------------------------------------------
// Edge Cases
// ---------------------------------------------------------------------------

#[test]
fn handles_empty_input() {
    let mut harness = Harness::with_timeout(10);
    harness.process("");
    assert_eq!(harness.data, [""]);
}

#[test]
fn handles_lone_escape_character_with_timeout() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b");
    assert!(harness.data.is_empty());

    harness.tick(15);
    assert_eq!(harness.data, ["\x1b"]);
}

#[test]
fn flushes_a_lone_escape_promptly_with_the_longer_default_sequence_timeout() {
    let mut harness = Harness::with_defaults();
    harness.process("\x1b");
    harness.tick(20);
    assert_eq!(harness.data, ["\x1b"]);
}

#[test]
fn handles_lone_escape_character_with_explicit_flush() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b");
    assert!(harness.data.is_empty());

    let flushed = harness.flush();
    assert_eq!(flushed, ["\x1b"]);
}

#[test]
fn handles_buffer_input() {
    let mut harness = Harness::with_timeout(10);
    harness.process_bytes(b"\x1b[A");
    assert_eq!(harness.data, ["\x1b[A"]);
}

#[test]
fn handles_very_long_sequences() {
    let mut harness = Harness::with_timeout(10);
    let long_seq = format!("\x1b[{}H", "1;".repeat(50));
    harness.process(&long_seq);
    assert_eq!(harness.data, [long_seq]);
}

// ---------------------------------------------------------------------------
// Flush
// ---------------------------------------------------------------------------

#[test]
fn flushes_incomplete_sequences() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[<35");
    let flushed = harness.flush();
    assert_eq!(flushed, ["\x1b[<35"]);
    assert_eq!(harness.buffer(), "");
}

#[test]
fn returns_empty_array_if_nothing_to_flush() {
    let mut harness = Harness::with_timeout(10);
    assert!(harness.flush().is_empty());
}

#[test]
fn emits_flushed_data_via_timeout() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[<35");
    assert!(harness.data.is_empty());

    harness.tick(15);
    assert_eq!(harness.data, ["\x1b[<35"]);
}

// ---------------------------------------------------------------------------
// Clear
// ---------------------------------------------------------------------------

#[test]
fn clears_buffered_content_without_emitting() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[<35");
    assert_eq!(harness.buffer(), "\x1b[<35");

    harness.buffer.clear();
    assert_eq!(harness.buffer(), "");
    assert!(harness.data.is_empty());
}

// ---------------------------------------------------------------------------
// Bracketed Paste
// ---------------------------------------------------------------------------

#[test]
fn emits_paste_event_for_complete_bracketed_paste() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[200~hello world\x1b[201~");
    assert_eq!(harness.paste, ["hello world"]);
    assert!(harness.data.is_empty());
}

#[test]
fn handles_paste_arriving_in_chunks() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[200~");
    assert!(harness.paste.is_empty());

    harness.process("hello ");
    assert!(harness.paste.is_empty());

    harness.process("world\x1b[201~");
    assert_eq!(harness.paste, ["hello world"]);
    assert!(harness.data.is_empty());
}

#[test]
fn handles_paste_with_input_before_and_after() {
    let mut harness = Harness::with_timeout(10);
    harness.process("a");
    harness.process("\x1b[200~pasted\x1b[201~");
    harness.process("b");

    assert_eq!(harness.data, ["a", "b"]);
    assert_eq!(harness.paste, ["pasted"]);
}

#[test]
fn handles_paste_with_newlines() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[200~line1\nline2\nline3\x1b[201~");
    assert_eq!(harness.paste, ["line1\nline2\nline3"]);
    assert!(harness.data.is_empty());
}

#[test]
fn handles_paste_with_unicode() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[200~Hello 世界 🎉\x1b[201~");
    assert_eq!(harness.paste, ["Hello 世界 🎉"]);
    assert!(harness.data.is_empty());
}

// ---------------------------------------------------------------------------
// Destroy
// ---------------------------------------------------------------------------

#[test]
fn clears_buffer_on_destroy() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[<35");
    assert_eq!(harness.buffer(), "\x1b[<35");

    harness.buffer.destroy();
    assert_eq!(harness.buffer(), "");
}

#[test]
fn clears_pending_timeouts_on_destroy() {
    let mut harness = Harness::with_timeout(10);
    harness.process("\x1b[<35");
    harness.buffer.destroy();

    harness.tick(15);
    assert!(harness.data.is_empty());
}
