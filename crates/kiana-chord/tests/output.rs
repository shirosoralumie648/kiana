//! `packages/chord/test/output.test.ts` 的 Rust 端口（24 条）。
//!
//! 差异说明：TS 输入字符串可以包含孤立 UTF-16 代理项（`TextEncoder` 会把它们编码成
//! U+FFFD，3 字节）。Rust 的 `String` 只能是合法 UTF-8，因此相关用例改用 U+FFFD 本身，
//! 覆盖的是同一段字节/字符边界逻辑。`Progress` 的 vitest 假定时器用例改为在可注入的
//! `ProgressClock` 上驱动，判定的是同一套调度状态机。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use kiana_chord::output::{
    BoundedOutput, OutputBuffer, OutputChunk, OutputLimits, OutputSkip, Progress, ProgressClock,
    ProgressFailure, Retain, SystemClock, bound_output, character_end, sanitize_output,
    utf8_byte_length,
};

fn head(max_lines: usize, max_bytes: usize) -> OutputLimits {
    OutputLimits::new(max_bytes, max_lines, Retain::Head)
}

fn tail(max_lines: usize, max_bytes: usize) -> OutputLimits {
    OutputLimits::new(max_bytes, max_lines, Retain::Tail)
}

fn bound(text: &str, limits: OutputLimits) -> (String, usize, usize) {
    let slice = bound_output(text, &limits);
    (slice.text, slice.dropped_bytes, slice.dropped_lines)
}

/// `Buffer.from(content, "utf8")` 后按字符边界取尾部。
fn buffer_tail(content: &str, max_bytes: usize) -> String {
    let bytes = content.as_bytes();
    if bytes.len() <= max_bytes {
        return content.to_owned();
    }
    let mut start = bytes.len() - max_bytes;
    while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
        start += 1;
    }
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

fn assert_matches_buffer_tail(input: &str, max_byte_values: Option<&[usize]>) {
    let total_bytes = utf8_byte_length(input);
    let generated: Vec<usize> = (0..=total_bytes + 4).collect();
    let values: &[usize] = match max_byte_values {
        Some(values) => values,
        None => &generated,
    };
    for &max_bytes in values {
        let kept = bound_output(input, &tail(10, max_bytes)).text;
        let expected = buffer_tail(input, max_bytes);
        assert_eq!(
            kept, expected,
            "tail mismatch input={input:?} maxBytes={max_bytes}"
        );
        assert!(
            utf8_byte_length(&kept) <= max_bytes,
            "tail output exceeded {max_bytes} bytes"
        );
    }
}

fn sampled_byte_limits(input: &str) -> Vec<usize> {
    let total_bytes = utf8_byte_length(input);
    let mut candidates = vec![
        0,
        1,
        2,
        3,
        4,
        5,
        8,
        total_bytes / 2,
        total_bytes.saturating_sub(4),
        total_bytes.saturating_sub(1),
        total_bytes,
        total_bytes + 1,
    ];
    candidates.sort_unstable();
    candidates.dedup();
    candidates
}

#[test]
fn removes_control_characters_but_keeps_tabs_newlines_and_other_text() {
    assert_eq!(
        sanitize_output("a\0b\tc\nd\re\u{7}f\u{fff9}g\u{fffb}h😀"),
        "ab\tc\ndefgh😀"
    );
}

#[test]
fn keeps_output_within_the_limits_unchanged() {
    assert_eq!(bound("a\nb\n", head(2, 1000)), ("a\nb\n".into(), 0, 0));
    assert_eq!(bound("a\nb", tail(2, 1000)), ("a\nb".into(), 0, 0));
    assert_eq!(bound("", tail(2, 1000)), ("".into(), 0, 0));
}

#[test]
fn keeps_nothing_with_a_zero_limit() {
    assert_eq!(bound("ab\ncd\n", head(10, 0)), ("".into(), 6, 2));
    assert_eq!(bound("ab\ncd\n", tail(0, 1000)), ("".into(), 6, 2));
}

#[test]
fn keeps_exact_slices_of_whole_lines_trailing_newline_included() {
    assert_eq!(bound("a\nb\nc\n", head(2, 1000)), ("a\nb\n".into(), 2, 1));
    assert_eq!(bound("a\nb\nc\n", tail(2, 1000)), ("b\nc\n".into(), 2, 1));
    assert_eq!(bound("a\nb\nc", tail(2, 1000)), ("b\nc".into(), 2, 1));
    // 空行也是行。
    assert_eq!(
        bound("a\nb\nc\n\n", tail(3, 1000)),
        ("b\nc\n\n".into(), 2, 1)
    );
}

#[test]
fn cuts_at_the_byte_limit_on_whole_lines_when_possible() {
    assert_eq!(
        bound("aa\nbb\ncc\n", head(10, 7)),
        ("aa\nbb\n".into(), 3, 1)
    );
    assert_eq!(
        bound("aa\nbb\ncc\n", tail(10, 7)),
        ("bb\ncc\n".into(), 3, 1)
    );
}

#[test]
fn cuts_a_single_line_longer_than_the_byte_limit_on_a_character_boundary() {
    // "é" 是两字节；五个字节能放下两个完整字符。
    assert_eq!(bound("ééé\n", head(10, 5)), ("éé".into(), 3, 0));
    assert_eq!(bound("x\néééé", tail(10, 5)), ("éé".into(), 6, 1));
}

#[test]
fn keeps_a_u_feff_at_the_start_of_a_kept_slice() {
    assert_eq!(bound("x\u{feff}a", tail(10, 4)), ("\u{feff}a".into(), 1, 0));
}

#[test]
fn cuts_tails_of_surrogate_edge_cases_exactly_like_buffer() {
    // TS 用孤立代理项；Rust 里它们的编码结果就是 U+FFFD（3 字节）。
    for input in [
        "a\u{fffd}",
        "\u{fffd}b",
        "a\u{fffd}b",
        "\u{fffd}\u{fffd}\u{fffd}",
        "😀\u{fffd}",
        "👩‍💻",
    ] {
        assert_matches_buffer_tail(input, None);
    }
}

#[test]
fn cuts_tails_exactly_like_buffer_across_deterministic_fuzz_cases() {
    let alphabet = [
        "a", "\u{7f}", "\u{80}", "é", "\u{7ff}", "\u{800}", "中", "\u{d7ff}", "\u{fffd}", "🙂",
        "\u{e000}", "\u{ffff}",
    ];
    fn check_exhaustive(prefix: &mut String, depth: usize, alphabet: &[&str]) {
        let limits = sampled_byte_limits(prefix);
        assert_matches_buffer_tail(prefix, Some(&limits));
        if depth == 0 {
            return;
        }
        for character in alphabet {
            let length = prefix.len();
            prefix.push_str(character);
            check_exhaustive(prefix, depth - 1, alphabet);
            prefix.truncate(length);
        }
    }
    check_exhaustive(&mut String::new(), 3, &alphabet);

    let mut seed: u32 = 0x1234_5678;
    let mut random = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        f64::from(seed) / 4_294_967_296.0
    };
    for _ in 0..1_000 {
        let mut input = String::new();
        let length = (random() * 80.0) as usize;
        for _ in 0..length {
            let index = (random() * alphabet.len() as f64) as usize;
            input.push_str(alphabet[index.min(alphabet.len() - 1)]);
        }
        let limits = sampled_byte_limits(&input);
        assert_matches_buffer_tail(&input, Some(&limits));
    }
}

#[test]
fn keeps_exact_totals_across_chunks_and_decodes_utf8_split_across_byte_chunks() {
    let mut buffer = OutputBuffer::new(tail(2, 1000));
    let bytes = "😀\n".as_bytes();
    buffer.push_text("a\nb\n");
    buffer.push_bytes(&bytes[..2]);
    buffer.push_bytes(&bytes[2..]);
    assert_eq!(
        buffer.snapshot(),
        BoundedOutput {
            text: "b\n😀\n".into(),
            dropped_bytes: 2,
            dropped_lines: 1
        }
    );
}

#[test]
fn sanitizes_the_retained_text_but_counts_the_raw_stream() {
    let mut buffer = OutputBuffer::new(tail(1, 1000));
    buffer.push_text("a\u{7}\n");
    assert_eq!(
        buffer.snapshot(),
        BoundedOutput {
            text: "a\n".into(),
            dropped_bytes: 0,
            dropped_lines: 0
        }
    );
    buffer.push_text("b\u{1b}\n");
    assert_eq!(
        buffer.snapshot(),
        BoundedOutput {
            text: "b\n".into(),
            dropped_bytes: 3,
            dropped_lines: 1
        }
    );
}

#[test]
fn drops_a_byte_order_mark_only_at_the_start_of_the_output() {
    let bom = [0xef, 0xbb, 0xbf];
    let mut buffer = OutputBuffer::new(tail(10, 1000));
    buffer.push_bytes(&bom[..1]);
    buffer.push_bytes(&[0xbb, 0xbf, 0x61]);
    buffer.push_text("b");
    // 字符串块之后的 U+FEFF 是文本。
    buffer.push_bytes(&[bom[0], bom[1], bom[2], 0x63]);
    buffer.end();
    assert_eq!(buffer.snapshot().text, "ab\u{feff}c");
}

#[test]
fn flushes_an_incomplete_character_before_a_string_chunk_and_at_the_end() {
    let mut buffer = OutputBuffer::new(tail(10, 1000));
    let euro = "€".as_bytes();
    buffer.push_bytes(&euro[..1]);
    buffer.push_text("x");
    buffer.push_bytes(&euro[..2]);
    buffer.end();
    assert_eq!(buffer.snapshot().text, "\u{fffd}x\u{fffd}");
}

#[test]
fn matches_bounding_the_whole_stream_when_several_chunks_arrive_between_snapshots() {
    for limits in [head(3, 40), tail(3, 40), head(50, 25), tail(50, 25)] {
        let mut buffer = OutputBuffer::new(limits);
        let mut stream = String::new();
        for index in 0..300usize {
            let chunk = if index % 7 == 0 {
                format!("{}\n", "é".repeat(index % 30))
            } else {
                format!("line {index}\n")
            };
            stream.push_str(&chunk);
            buffer.push_text(&chunk);
            if index % 5 != 4 {
                continue;
            }
            let expected = bound_output(&stream, &limits);
            assert_eq!(
                buffer.snapshot(),
                BoundedOutput {
                    text: expected.text,
                    dropped_bytes: expected.dropped_bytes,
                    dropped_lines: expected.dropped_lines,
                },
                "limits {limits:?} index {index}"
            );
        }
    }
}

#[test]
fn stops_storing_head_output_once_the_window_is_full() {
    let mut buffer = OutputBuffer::new(head(2, 1000));
    for index in 0..1000usize {
        buffer.push_text(&format!("line {index}\n"));
    }
    assert!(
        buffer.stored_bytes() < 20,
        "stored {}",
        buffer.stored_bytes()
    );
    assert_eq!(
        buffer.snapshot(),
        BoundedOutput {
            text: "line 0\nline 1\n".into(),
            dropped_bytes: 8876,
            dropped_lines: 998
        }
    );
}

#[test]
fn stores_only_the_tail_window_after_each_snapshot() {
    let mut buffer = OutputBuffer::new(tail(3, 100));
    let mut stream = String::new();
    for index in 0..2000usize {
        let chunk = format!("line {index}\n\n");
        stream.push_str(&chunk);
        buffer.push_text(&chunk);
        let snapshot = buffer.snapshot();
        assert!(
            buffer.stored_bytes() <= 100,
            "stored {} at {index}",
            buffer.stored_bytes()
        );
        assert_eq!(snapshot.text, bound_output(&stream, &tail(3, 100)).text);
    }
}

// ---------------------------------------------------------------------------
// Progress
// ---------------------------------------------------------------------------

struct TestTimer {
    delay_ms: f64,
    due: f64,
    callback: Option<Box<dyn FnOnce() + Send>>,
    armed: Arc<AtomicBool>,
}

/// 可注入时钟：记录每次 `setTimer` 的相对延迟，测试手动推进或触发。
struct TestClock {
    now: Mutex<f64>,
    timers: Mutex<Vec<TestTimer>>,
}

impl TestClock {
    fn new(now: f64) -> Arc<Self> {
        Arc::new(TestClock {
            now: Mutex::new(now),
            timers: Mutex::new(Vec::new()),
        })
    }

    fn now(&self) -> f64 {
        *self.now.lock().unwrap()
    }

    fn set_now(&self, now: f64) {
        *self.now.lock().unwrap() = now;
    }

    fn advance(&self, ms: f64) {
        let now = {
            let mut current = self.now.lock().unwrap();
            *current += ms;
            *current
        };
        loop {
            let callback = {
                let mut timers = self.timers.lock().unwrap();
                let position = timers.iter().position(|timer| {
                    timer.armed.load(Ordering::SeqCst)
                        && timer.callback.is_some()
                        && timer.due <= now
                });
                match position {
                    Some(index) => timers[index].callback.take(),
                    None => None,
                }
            };
            match callback {
                Some(callback) => callback(),
                None => break,
            }
        }
    }

    fn fire(&self, index: usize) {
        let callback = {
            let mut timers = self.timers.lock().unwrap();
            let timer = &mut timers[index];
            assert!(timer.armed.load(Ordering::SeqCst), "timer {index} disarmed");
            timer.callback.take()
        };
        callback.expect("timer already fired")();
    }

    fn delays(&self) -> Vec<f64> {
        self.timers
            .lock()
            .unwrap()
            .iter()
            .map(|timer| timer.delay_ms)
            .collect()
    }

    fn armed(&self, index: usize) -> bool {
        self.timers.lock().unwrap()[index]
            .armed
            .load(Ordering::SeqCst)
    }
}

impl ProgressClock for TestClock {
    fn now(&self) -> f64 {
        TestClock::now(self)
    }

    fn set_timer(
        &self,
        delay_ms: f64,
        callback: Box<dyn FnOnce() + Send>,
    ) -> Box<dyn FnOnce() + Send> {
        let armed = Arc::new(AtomicBool::new(true));
        let due = self.now() + delay_ms;
        self.timers.lock().unwrap().push(TestTimer {
            delay_ms,
            due,
            callback: Some(callback),
            armed: Arc::clone(&armed),
        });
        Box::new(move || armed.store(false, Ordering::SeqCst))
    }
}

async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

async fn wait_for_len<T>(items: &Arc<Mutex<Vec<T>>>, expected: usize) {
    for _ in 0..200 {
        if items.lock().unwrap().len() >= expected {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!(
        "timed out waiting for {expected} items, got {}",
        items.lock().unwrap().len()
    );
}

#[tokio::test]
async fn commits_the_first_change_at_once_then_waits_at_least_100ms_and_the_written_size() {
    let clock = TestClock::new(0.0);
    let commits = Arc::new(Mutex::new(Vec::<f64>::new()));
    let size = Arc::new(Mutex::new(50 * 1024usize));
    let progress = Progress::with_clock(
        {
            let clock = Arc::clone(&clock);
            let commits = Arc::clone(&commits);
            let size = Arc::clone(&size);
            move || {
                let clock = Arc::clone(&clock);
                let commits = Arc::clone(&commits);
                let size = Arc::clone(&size);
                Box::pin(async move {
                    commits.lock().unwrap().push(clock.now());
                    Ok(*size.lock().unwrap())
                })
            }
        },
        |_error| {},
        100.0,
        Arc::clone(&clock) as Arc<dyn ProgressClock>,
    );
    progress.mark();
    wait_for_len(&commits, 1).await;
    assert_eq!(*commits.lock().unwrap(), vec![0.0]);
    // 50 KiB 买 500 ms；期间的变化合并成一次提交。
    *size.lock().unwrap() = 10;
    settle().await;
    progress.mark();
    progress.mark();
    settle().await;
    clock.advance(499.0);
    settle().await;
    assert_eq!(*commits.lock().unwrap(), vec![0.0]);
    clock.advance(1.0);
    wait_for_len(&commits, 2).await;
    assert_eq!(*commits.lock().unwrap(), vec![0.0, 500.0]);
    // 小提交仍等最小 100 ms。
    settle().await;
    progress.mark();
    clock.advance(99.0);
    settle().await;
    assert_eq!(*commits.lock().unwrap(), vec![0.0, 500.0]);
    clock.advance(1.0);
    wait_for_len(&commits, 3).await;
    assert_eq!(*commits.lock().unwrap(), vec![0.0, 500.0, 600.0]);
    let _ = progress.stop().await;
}

#[tokio::test]
async fn waits_the_configured_minimum_interval_between_small_commits() {
    let clock = TestClock::new(0.0);
    let commits = Arc::new(Mutex::new(Vec::<f64>::new()));
    let progress = Progress::with_clock(
        {
            let clock = Arc::clone(&clock);
            let commits = Arc::clone(&commits);
            move || {
                let clock = Arc::clone(&clock);
                let commits = Arc::clone(&commits);
                Box::pin(async move {
                    commits.lock().unwrap().push(clock.now());
                    Ok(10)
                })
            }
        },
        |_error| {},
        500.0,
        Arc::clone(&clock) as Arc<dyn ProgressClock>,
    );
    progress.mark();
    wait_for_len(&commits, 1).await;
    settle().await;
    progress.mark();
    clock.advance(499.0);
    settle().await;
    assert_eq!(*commits.lock().unwrap(), vec![0.0]);
    clock.advance(1.0);
    wait_for_len(&commits, 2).await;
    assert_eq!(*commits.lock().unwrap(), vec![0.0, 500.0]);
    let _ = progress.stop().await;
}

#[tokio::test]
async fn paces_on_the_clock_it_is_given() {
    let clock = TestClock::new(1_000.0);
    let commits = Arc::new(Mutex::new(Vec::<f64>::new()));
    let progress = Progress::with_clock(
        {
            let clock = Arc::clone(&clock);
            let commits = Arc::clone(&commits);
            move || {
                let clock = Arc::clone(&clock);
                let commits = Arc::clone(&commits);
                Box::pin(async move {
                    commits.lock().unwrap().push(clock.now());
                    Ok(10)
                })
            }
        },
        |_error| {},
        100.0,
        Arc::clone(&clock) as Arc<dyn ProgressClock>,
    );
    progress.mark_and_wait().await.expect("first commit");
    assert_eq!(*commits.lock().unwrap(), vec![1_000.0]);
    settle().await;
    clock.set_now(1_040.0);
    let second = progress.mark_and_wait();
    assert_eq!(clock.delays(), vec![60.0]);
    // 提前触发的定时器什么都不提交，继续等剩下的时间。
    clock.set_now(1_080.0);
    clock.fire(0);
    assert_eq!(*commits.lock().unwrap(), vec![1_000.0]);
    assert_eq!(clock.delays(), vec![60.0, 20.0]);
    clock.set_now(1_100.0);
    clock.fire(1);
    second.await.expect("second commit");
    assert_eq!(*commits.lock().unwrap(), vec![1_000.0, 1_100.0]);
    settle().await;
    progress.mark();
    assert_eq!(clock.delays(), vec![60.0, 20.0, 100.0]);
    let _ = progress.stop().await;
    assert!(!clock.armed(2), "timer 2 should be disarmed");
}

#[tokio::test]
async fn rejects_the_waiters_of_a_failed_commit_and_reports_its_error() {
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let progress = Progress::new(
        || {
            Box::pin(async {
                Err::<usize, ProgressFailure>(Arc::new(std::io::Error::other("commit failed")))
            })
        },
        {
            let errors = Arc::clone(&errors);
            move |error: ProgressFailure| errors.lock().unwrap().push(error.to_string())
        },
        100.0,
    );
    let failure = progress.mark_and_wait().await.expect_err("failed commit");
    assert_eq!(failure.to_string(), "commit failed");
    assert_eq!(*errors.lock().unwrap(), vec!["commit failed".to_string()]);
}

#[tokio::test]
async fn stops_waits_for_the_commit_in_flight_and_hands_back_waiters_no_commit_covered_yet() {
    let (release, released) = tokio::sync::oneshot::channel::<()>();
    let released = Arc::new(Mutex::new(Some(released)));
    let progress = Progress::new(
        {
            let released = Arc::clone(&released);
            move || {
                let released = Arc::clone(&released);
                Box::pin(async move {
                    let receiver = released.lock().unwrap().take();
                    if let Some(receiver) = receiver {
                        let _ = receiver.await;
                    }
                    Ok::<usize, ProgressFailure>(0)
                })
            }
        },
        |_error| {},
        100.0,
    );
    let first = progress.mark_and_wait();
    let second = progress.mark_and_wait();
    let stopped = progress.stop();
    let _ = release.send(());
    let pending = stopped.await;
    first.await.expect("first wait");
    assert_eq!(pending.len(), 1);
    pending
        .into_iter()
        .next()
        .unwrap()
        .send(Ok(()))
        .expect("settle second waiter");
    second.await.expect("second wait");
}

#[test]
fn system_clock_reports_wall_clock_milliseconds() {
    let clock = SystemClock;
    let now = clock.now();
    assert!(now > 1_600_000_000_000.0, "now = {now}");
}

#[test]
fn character_end_walks_back_to_a_boundary() {
    assert_eq!(character_end("ééé\n".as_bytes(), 5), 4);
    assert_eq!(character_end("ééé\n".as_bytes(), 4), 4);
    assert_eq!(character_end("abc".as_bytes(), 3), 3);
    assert_eq!(character_end(&[], 0), 0);
}

#[test]
fn output_chunk_text_and_bytes_agree_for_valid_utf8() {
    let mut text_buffer = OutputBuffer::new(OutputLimits::new(100, 10, Retain::Tail));
    let mut byte_buffer = OutputBuffer::new(OutputLimits::new(100, 10, Retain::Tail));
    text_buffer.push(OutputChunk::Text("a😀é\n"), None).unwrap();
    byte_buffer
        .push(OutputChunk::Bytes("a😀é\n".as_bytes()), None)
        .unwrap();
    assert_eq!(text_buffer.snapshot(), byte_buffer.snapshot());
}

#[test]
fn skip_defaults_match_the_wire_shape() {
    let skip = OutputSkip {
        bytes: 3,
        newlines: 1,
        ends_with_newline: true,
    };
    assert_eq!(skip.bytes, 3);
    assert_eq!(
        OutputSkip::default(),
        OutputSkip {
            bytes: 0,
            newlines: 0,
            ends_with_newline: false
        }
    );
}

#[tokio::test]
async fn in_flight_commit_still_settles_waiters_after_the_progress_is_dropped() {
    // TS 的 promise 链在 Progress 被丢弃后仍持有对象：在飞的提交结算时 waiter 照常完成。
    let (started_sender, started_receiver) = tokio::sync::oneshot::channel::<()>();
    let (release_sender, release_receiver) = tokio::sync::oneshot::channel::<()>();
    let started_sender = Arc::new(Mutex::new(Some(started_sender)));
    let release_receiver = Arc::new(Mutex::new(Some(release_receiver)));
    let progress = Progress::new(
        {
            let started_sender = Arc::clone(&started_sender);
            let release_receiver = Arc::clone(&release_receiver);
            move || {
                let started_sender = started_sender.lock().unwrap().take();
                let release_receiver = release_receiver.lock().unwrap().take();
                Box::pin(async move {
                    if let Some(sender) = started_sender {
                        let _ = sender.send(());
                    }
                    if let Some(receiver) = release_receiver {
                        let _ = receiver.await;
                    }
                    Ok(7usize)
                })
            }
        },
        |_error| {},
        0.0,
    );
    let wait = progress.mark_and_wait();
    started_receiver.await.expect("commit started");
    drop(progress);
    let _ = release_sender.send(());
    wait.await
        .expect("waiter settles with the in-flight commit");
}

#[test]
fn schedules_commits_from_synchronous_code_with_an_injected_clock() {
    // TS 的 Progress 不要求调用方处在任何运行时里：同步代码里的 `mark()` 只是安排提交，
    // promise 稍后结算。Rust 侧在无 Tokio 运行时时不能 panic（`tokio::spawn` 会），
    // 也不能把 `mark()` 变成同步等待。
    let clock = TestClock::new(1_000.0);
    let commits = Arc::new(Mutex::new(Vec::<f64>::new()));
    let progress = Progress::with_clock(
        {
            let clock = Arc::clone(&clock);
            let commits = Arc::clone(&commits);
            move || {
                let clock = Arc::clone(&clock);
                let commits = Arc::clone(&commits);
                Box::pin(async move {
                    commits.lock().unwrap().push(clock.now());
                    Ok(10)
                })
            }
        },
        |_error| {},
        100.0,
        Arc::clone(&clock) as Arc<dyn ProgressClock>,
    );
    progress.mark();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while commits.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(*commits.lock().unwrap(), vec![1_000.0]);
    // `mark_and_wait` 的 waiter 同样在无运行时时结算。
    clock.set_now(1_100.0);
    futures::executor::block_on(progress.mark_and_wait()).expect("second commit");
    assert_eq!(*commits.lock().unwrap(), vec![1_000.0, 1_100.0]);
    futures::executor::block_on(progress.stop());
}
