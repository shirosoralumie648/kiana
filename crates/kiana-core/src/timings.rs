//! `core/timings.ts`：`reset_timings` / `time` / `print_timings`（`KIANA_TIMING=1` 启用）。
//!
//! `kiana-extensions::loader`（L9）与 `kiana-coding-agent::main` 都调用；`kiana-resources::timings`
//! 只再导出（`ARCHITECTURE.md` §3.5）。
//!
//! TS: `packages/coding-agent/src/core/timings.ts`。
//!
//! ## 语义（逐条对齐 TS）
//!
//! - `KIANA_TIMING` 只在**首次使用**时求值一次（[`LazyLock`]），等于 `"1"` 才启用；关闭时三个公开
//!   函数全部立即返回。
//! - 每个 namespace 维护 `{ timings: [{label, ms}], lastTime }`；`reset_timings` 把 `lastTime`
//!   重置为当前毫秒。
//! - `time(label, ns)`：若该 namespace 尚不存在，先按 `reset_timings(ns)` 建立（`lastTime` 取
//!   **此刻之后**的时间，因此紧接着记录的第一条 `ms` 可能是 0 或负数，与 TS 的双次 `Date.now()`
//!   一致）；然后 push `now - lastTime`，再令 `lastTime = now`。
//! - `print_timings` 按 Map 插入顺序遍历 namespace，逐个打印：
//!   `\n--- {title} ---\n`、每条 `  {label}: {ms}ms\n`、`  TOTAL: {sum}ms\n`、
//!   `{"-".repeat(title.length + 8)}\n`，并且末尾因 `console.error` 自带换行多一个 `\n`。
//!   只打印 `ms >= 0` 的项；一组为空时**连标题都不打印**。
//! - 输出走 **stderr**（`console.error`）。
//!
//! 毫秒来自单调时钟（`Instant`），先换算成整数毫秒再相减，与 `Date.now()` 的整数毫秒语义一致。
//! title 的长度按 UTF-16 code unit 计（TS 的 `string.length`）；当前 title 全是 ASCII。
//!
//! ## 并发与全局状态
//!
//! 计时表是进程级全局 `Mutex<IndexMap<TimingLabel, TimingNamespace>>`：
//!
//! - `reset_timings` / `time` 持锁完成查找与写入；锁内不 `.await`、不调用用户代码，锁内
//!   只做内存操作，因此不会因异步任务互相等待而死锁。
//! - `render_timings` 在锁内**克隆**快照：这一步需要短暂持有同一把锁，因此与 `time` /
//!   `reset_timings` 互斥；格式化与 `print_timings` 写 stderr 都在锁外，慢 I/O 不会延长
//!   持锁时间。
//! - 锁被 panic 毒化时 `lock()` 用 `into_inner` 取回内层数据继续使用，而不是让后续调用
//!   全部 panic。锁内的操作只有 `IndexMap` 的查找/插入与 `Vec` 的 push，没有会留下半成品
//!   状态的用户代码（OOM 类 panic 除外，见 `lock()` 的注释）。
//! - `ENABLED` 是 `LazyLock`：`KIANA_TIMING` 在第一次调用 `enabled()` 时求值，首次求值
//!   之后再改环境变量不会生效；测试用 `TEST_ENABLED` 绕过它。
//! - `TEST_ENABLED` 只由测试缝写入（生产路径永远读到 `-1`）。并行测试若要改写全局表，
//!   需要自己串行化（`tests/timings.rs` 用 `TEST_LOCK`）。
//!
//! ## 数据增长与过滤
//!
//! 每个 namespace 的条目是 `Vec`，在 `reset_timings` 之前只增不减；namespace 最多两个
//! （`Main` / `Extensions`），标签由调用方提供且原样保存（不去重、不合并）。
//! `ms < 0` 的条目也会入表，只在渲染时被过滤，因此 `TOTAL` 是**可打印项**之和，而不是
//! 全部条目之和。

use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicI8, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::Instant;

use indexmap::IndexMap;

/// 计时分组。TS: `timings.ts#TimingLabel`（`"main" | "extensions"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimingLabel {
    /// `"main"`：进程主流程。
    Main,
    /// `"extensions"`：扩展/资源加载。
    Extensions,
}

impl TimingLabel {
    /// TS 里的字符串字面量（也是打印时的 namespace 名）。
    pub fn as_str(self) -> &'static str {
        match self {
            TimingLabel::Main => "main",
            TimingLabel::Extensions => "extensions",
        }
    }
}

impl fmt::Display for TimingLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `KIANA_TIMING === "1"`，首次使用时求值一次。
static ENABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("KIANA_TIMING").is_some_and(|value| value == "1"));

/// 测试缝：`-1` = 未设置（用 [`ENABLED`]），`0` = 强制关闭，`1` = 强制开启。
///
/// 生产路径永远读到 `-1`。见 [`set_timing_enabled_for_test`]。
static TEST_ENABLED: AtomicI8 = AtomicI8::new(-1);

fn enabled() -> bool {
    match TEST_ENABLED.load(Ordering::Relaxed) {
        1 => true,
        0 => false,
        _ => *ENABLED,
    }
}

/// 单调时钟的毫秒原点；`now_ms()` 与 `Date.now()` 一样是整数毫秒。
///
/// 原点在**首次调用 `now_ms()` 时**才建立（即第一次 `reset_timings` / `time`），因此
/// 那一次读到的值是 0，而不是进程启动以来的毫秒数。
static CLOCK_ORIGIN: LazyLock<Instant> = LazyLock::new(Instant::now);

fn now_ms() -> i64 {
    // 进程运行时长远小于 i64::MAX 毫秒。
    CLOCK_ORIGIN.elapsed().as_millis() as i64
}

#[derive(Debug)]
struct TimingNamespace {
    timings: Vec<(String, i64)>,
    last_time: i64,
}

static TIMING_NAMESPACES: LazyLock<Mutex<IndexMap<TimingLabel, TimingNamespace>>> =
    LazyLock::new(|| Mutex::new(IndexMap::new()));

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // 锁内没有用户代码，毒化只可能来自 OOM 级别的 panic；恢复内层数据比 panic 更接近 JS。
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 重置（或建立）一个计时分组：清空条目并把 `lastTime` 设为当前毫秒。
/// TS: `timings.ts#resetTimings`。
///
/// 关闭计时时是无操作（不建表、不取时）。对已存在的 namespace 再次调用会**丢弃**其全部
/// 条目并重新计时，而不是追加。
pub fn reset_timings(namespace: TimingLabel) {
    if !enabled() {
        return;
    }
    let mut namespaces = lock(&TIMING_NAMESPACES);
    namespaces.insert(
        namespace,
        TimingNamespace {
            timings: Vec::new(),
            last_time: now_ms(),
        },
    );
}

/// 记录一条计时：`label` 距上一条（或上一次 reset）的毫秒数。
/// TS: `timings.ts#time`。
///
/// namespace 尚不存在时先建立它；此时 `lastTime` 在 `now` 之后取，第一条的 `ms` 可能是 0 或
/// 负数（负数在打印时会被过滤），与 TS 先 `Date.now()` 再 `resetTimings()` 的双次取时一致。
///
/// 关闭计时时是无操作。`label` 原样保存（不去重、不合并）；同一毫秒内的连续调用会得到
/// 0，新建 namespace 的第一条还可能是负数，这些都是预期结果，调用方不要当作错误。
pub fn time(label: &str, namespace: TimingLabel) {
    if !enabled() {
        return;
    }
    let now = now_ms();
    let mut namespaces = lock(&TIMING_NAMESPACES);
    // 与 TS 的「先 Date.now()，再 resetTimings() 里再取一次」一致：新建时重新取 now_ms()，
    // 因此紧接的第一条会记录 now - 后取的时间，可能为 0 或负数。
    if !namespaces.contains_key(&namespace) {
        namespaces.insert(
            namespace,
            TimingNamespace {
                timings: Vec::new(),
                last_time: now_ms(),
            },
        );
    }
    if let Some(entry) = namespaces.get_mut(&namespace) {
        entry
            .timings
            .push((label.to_string(), now - entry.last_time));
        entry.last_time = now;
    }
}

/// 把一组计时渲染到 `out`；为空（没有 `ms >= 0` 的项）时不写任何内容。
fn render_timing_group(out: &mut String, title: &str, timings: &[(String, i64)]) {
    let printable: Vec<&(String, i64)> = timings.iter().filter(|(_, ms)| *ms >= 0).collect();
    if printable.is_empty() {
        return;
    }
    out.push('\n');
    out.push_str("--- ");
    out.push_str(title);
    out.push_str(" ---\n");
    for (label, ms) in &printable {
        out.push_str("  ");
        out.push_str(label);
        out.push_str(": ");
        out.push_str(&ms.to_string());
        out.push_str("ms\n");
    }
    let total: i64 = printable.iter().map(|(_, ms)| *ms).sum();
    out.push_str("  TOTAL: ");
    out.push_str(&total.to_string());
    out.push_str("ms\n");
    // TS: `${"-".repeat(title.length + 8)}\n`，再由 console.error 追加一个换行。
    let width = title.encode_utf16().count() + 8;
    out.push_str(&"-".repeat(width));
    out.push_str("\n\n");
}

/// 渲染全部 namespace 的报告（`print_timings` 写出的确切字节）。
fn render_timings() -> String {
    let snapshot: Vec<(TimingLabel, Vec<(String, i64)>)> = {
        let namespaces = lock(&TIMING_NAMESPACES);
        namespaces
            .iter()
            .map(|(label, ns)| (*label, ns.timings.clone()))
            .collect()
    };
    let mut out = String::new();
    for (label, timings) in snapshot {
        render_timing_group(&mut out, &format!("Startup Timings: {label}"), &timings);
    }
    out
}

/// 打印全部启动计时到 stderr（`KIANA_TIMING=1` 时才有内容）。
/// TS: `timings.ts#printTimings`。
///
/// 关闭计时、或没有任何可打印条目时不产生任何输出（连分组标题都不打印）。写入失败
/// （例如 stderr 已关闭）被静默忽略：函数没有错误返回通道，与 `console.error` 一致。
pub fn print_timings() {
    if !enabled() {
        return;
    }
    let report = render_timings();
    if report.is_empty() {
        return;
    }
    let stderr = io::stderr();
    let mut handle = stderr.lock();
    // `console.error` 不会因 EPIPE 之类失败而抛错；这里同样忽略写失败。
    let _ = handle.write_all(report.as_bytes());
    let _ = handle.flush();
}

// ---------------------------------------------------------------------------
// 测试缝（doc-hidden，非公开 API）
// ---------------------------------------------------------------------------

/// 测试缝：强制开启/关闭计时（`None` 恢复按 `KIANA_TIMING` 求值）。
///
/// **test hook, not public API**。生产代码不得调用。
#[doc(hidden)]
pub fn set_timing_enabled_for_test(enabled: Option<bool>) {
    TEST_ENABLED.store(
        match enabled {
            None => -1,
            Some(false) => 0,
            Some(true) => 1,
        },
        Ordering::Relaxed,
    );
}

/// 测试缝：返回 `print_timings` 会写出的确切文本（不写 stderr）。
///
/// **test hook, not public API**。生产代码不得调用。
#[doc(hidden)]
pub fn timings_report_for_test() -> String {
    if !enabled() {
        return String::new();
    }
    render_timings()
}

/// 测试缝：清空所有 namespace（不影响 `KIANA_TIMING` 求值）。
///
/// **test hook, not public API**。生产代码不得调用。
#[doc(hidden)]
pub fn clear_timings_for_test() {
    lock(&TIMING_NAMESPACES).clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 这些测试都改写模块级状态，串行执行。
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn reset_between_tests() {
        set_timing_enabled_for_test(Some(true));
        clear_timings_for_test();
    }

    #[test]
    fn renders_the_ts_layout_verbatim() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_between_tests();

        let mut out = String::new();
        render_timing_group(
            &mut out,
            "Startup Timings: main",
            &[("a".into(), 0), ("b".into(), 3)],
        );
        assert_eq!(
            out,
            "\n--- Startup Timings: main ---\n  a: 0ms\n  b: 3ms\n  TOTAL: 3ms\n-----------------------------\n\n"
        );
    }

    #[test]
    fn skips_groups_without_printable_timings_and_negative_entries() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_between_tests();

        let mut out = String::new();
        render_timing_group(&mut out, "Startup Timings: main", &[]);
        assert!(out.is_empty());
        render_timing_group(&mut out, "Startup Timings: main", &[("a".into(), -1)]);
        assert!(out.is_empty());

        render_timing_group(
            &mut out,
            "Startup Timings: main",
            &[("a".into(), -1), ("b".into(), 2)],
        );
        assert_eq!(
            out,
            "\n--- Startup Timings: main ---\n  b: 2ms\n  TOTAL: 2ms\n-----------------------------\n\n"
        );
    }

    #[test]
    fn fills_missing_namespaces_like_a_reset_before_timing() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_between_tests();

        time("first", TimingLabel::Extensions);
        let report = render_timings();
        // The first entry of a fresh namespace is `now - Date.now()` inside reset: 0 or negative,
        // so it may be filtered out entirely.
        assert!(
            report.is_empty()
                || report.starts_with("\n--- Startup Timings: extensions ---\n  first: "),
            "unexpected report: {report:?}"
        );
    }

    #[test]
    fn keeps_namespace_insertion_order_and_sums() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_between_tests();

        reset_timings(TimingLabel::Main);
        time("x", TimingLabel::Main);
        reset_timings(TimingLabel::Extensions);
        time("y", TimingLabel::Extensions);
        let report = render_timings();
        let main_at = report.find("Startup Timings: main");
        let extensions_at = report.find("Startup Timings: extensions");
        assert!(main_at.is_some() && extensions_at.is_some());
        assert!(main_at < extensions_at);
    }
}
