//! `core/timings.ts` 的公开 API 测试（TS 无对应测试文件，按 `ARCHITECTURE.md` §4.8 第 9 条新写）。
//!
//! 格式黄金值来自 TS 参考实现的实测输出（`console.error` 的实参序列），见
//! `spec/ca-resources.md` §2.3：
//! `\n--- Startup Timings: main ---\n  a: 0ms\n  b: 3ms\n  TOTAL: 3ms\n` + 29 个 `-` + `\n\n`。
//!
//! 时间值本身不可控，因此结构用正则断言；打印通道用真实的 fd 2 重定向验证。
//!
//! TS: `packages/coding-agent/src/core/timings.ts`。

use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::sync::Mutex;

use kiana_core::timings::{
    TimingLabel, clear_timings_for_test, print_timings, reset_timings, set_timing_enabled_for_test,
    time, timings_report_for_test,
};
use regex::Regex;

/// 这些测试改写模块级状态（计时表、强制开关、fd 2），串行执行。
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn lock_tests() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 把 fd 2 重定向到临时文件；`finish`/Drop 恢复。
struct StderrCapture {
    saved: i32,
    file: std::fs::File,
    restored: bool,
}

impl StderrCapture {
    fn new() -> Self {
        // 先冲掉 Rust 侧缓冲，避免把先前内容带进捕获文件。
        let _ = std::io::stderr().flush();
        let file = tempfile::tempfile().expect("tempfile");
        // SAFETY: `dup`/`dup2` 只操作进程文件描述符；捕获期间由测试锁串行化。
        unsafe {
            let saved = libc::dup(2);
            assert!(saved >= 0, "dup(2) failed");
            assert!(libc::dup2(file.as_raw_fd(), 2) >= 0, "dup2 failed");
            Self {
                saved,
                file,
                restored: false,
            }
        }
    }

    fn read(&mut self) -> String {
        let _ = std::io::stderr().flush();
        self.file.seek(SeekFrom::Start(0)).expect("seek");
        let mut text = String::new();
        self.file.read_to_string(&mut text).expect("read stderr");
        text
    }

    fn finish(mut self) -> String {
        self.restore();
        self.read()
    }

    fn restore(&mut self) {
        if self.restored {
            return;
        }
        // SAFETY: 恢复保存的 fd 并关闭副本；只调用一次。
        unsafe {
            libc::dup2(self.saved, 2);
            libc::close(self.saved);
        }
        self.restored = true;
    }
}

impl Drop for StderrCapture {
    fn drop(&mut self) {
        self.restore();
    }
}

#[test]
fn disabled_without_kiana_timing_is_a_no_op() {
    let _guard = lock_tests();
    let kiana_timing_enabled = std::env::var_os("KIANA_TIMING").is_some_and(|value| value == "1");
    if kiana_timing_enabled {
        // The environment of this test process enables timings; the disabled path is untestable here.
        return;
    }
    set_timing_enabled_for_test(None);
    clear_timings_for_test();
    reset_timings(TimingLabel::Main);
    time("ignored", TimingLabel::Main);
    assert_eq!(timings_report_for_test(), "");
    print_timings();
    assert_eq!(timings_report_for_test(), "");
}

#[test]
fn renders_groups_in_insertion_order_with_the_ts_layout() {
    let _guard = lock_tests();
    set_timing_enabled_for_test(Some(true));
    clear_timings_for_test();

    reset_timings(TimingLabel::Main);
    time("a", TimingLabel::Main);
    time("b", TimingLabel::Main);
    reset_timings(TimingLabel::Extensions);
    time("c", TimingLabel::Extensions);

    let report = timings_report_for_test();
    let layout = Regex::new(
        r"(?s)^\n--- Startup Timings: main ---\n  a: (-?\d+)ms\n  b: (-?\d+)ms\n  TOTAL: (-?\d+)ms\n(-{29})\n\n\n--- Startup Timings: extensions ---\n  c: (-?\d+)ms\n  TOTAL: (-?\d+)ms\n(-{35})\n\n$",
    )
    .expect("valid regex");
    let captures = layout
        .captures(&report)
        .unwrap_or_else(|| panic!("report does not match the TS layout: {report:?}"));
    let a: i64 = captures[1].parse().expect("a ms");
    let b: i64 = captures[2].parse().expect("b ms");
    let main_total: i64 = captures[3].parse().expect("main total");
    let extensions_total: i64 = captures[5].parse().expect("extensions total");
    assert_eq!(main_total, a + b);
    assert!(extensions_total >= 0);
}

#[test]
fn empty_groups_print_nothing_at_all() {
    let _guard = lock_tests();
    set_timing_enabled_for_test(Some(true));
    clear_timings_for_test();
    reset_timings(TimingLabel::Main);
    reset_timings(TimingLabel::Extensions);
    assert_eq!(timings_report_for_test(), "");
}

#[test]
fn first_time_creates_the_namespace_like_a_reset() {
    let _guard = lock_tests();
    set_timing_enabled_for_test(Some(true));
    clear_timings_for_test();

    // Without a preceding reset, `time` creates the namespace; the first entry measures
    // `now - Date.now()` taken inside reset, so it is 0 or negative and may be filtered out.
    time("lone", TimingLabel::Main);
    let report = timings_report_for_test();
    if !report.is_empty() {
        assert!(
            report.starts_with("\n--- Startup Timings: main ---\n  lone: "),
            "unexpected report: {report:?}"
        );
        assert!(report.contains("TOTAL: "));
    }
}

#[test]
fn print_timings_writes_the_report_to_stderr() {
    let _guard = lock_tests();
    set_timing_enabled_for_test(Some(true));
    clear_timings_for_test();
    reset_timings(TimingLabel::Main);
    time("x", TimingLabel::Main);

    let expected = timings_report_for_test();
    assert!(expected.starts_with("\n--- Startup Timings: main ---\n"));

    let capture = StderrCapture::new();
    print_timings();
    let captured = capture.finish();
    assert_eq!(captured, expected);
}
