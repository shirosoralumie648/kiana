//! `window.rs`：Durable 的输出窗口（`ShellExecOptions.window`、`ShellOutputInfo.skipped`）。
//! 未送达的输出被保留并合并；后面还跟着超过窗口（按字节或按行）的文本不可能出现在调用方尾部，改为计数。
//! 送达节奏跟随调用方的提交速度，且上一帧未发送完时等待。
//!
//! TS 源：packages/env/daemon/src/window.rs（原 daemon 本身就是 Rust；逐字移植）。
//! Durable's output window (`ShellExecOptions.window`, `ShellOutputInfo.skipped`): undelivered command output is held
//! and coalesced; text followed by more than the window (by a byte or a line) can never be in the caller's tail and is
//! replaced by counts. Delivery follows the caller's commit pace, and waits while an earlier frame is still unsent.
//!
//! # 它解决什么问题
//! 命令输出可能远大于调用方愿意/需要保留的量。输出窗口的语义是：调用方只关心"最近的至多
//! `maxBytes` 字节、至多 `maxLines` 行"以及未送达期间产生了多少输出。若某段文本后面已经跟着
//! 超过窗口大小的输出，它就绝不可能出现在调用方最终看到的尾部，于是可以把它替换成
//! `skipped { bytes, newlines, endsWithNewline }` 计数，把内存占用限制在窗口量级。
//!
//! # 在系统中的位置
//! - `exec` 请求带 `window` 字段时，`ExecRequest::from_json` 用 `Window::from_json` 解析；
//!   `exec::run` 的事件循环用 `Pending` 收集已解码的 stdout/stderr 文本，按 `next_send` 的节奏
//!   调用 `take` 生成 `output` 事件；
//! - 没有窗口时 `exec` 不使用本模块，输出逐块发送，由 `Output` 的 bulk 上限做反压；
//! - `Pending` 保存的是解码后的 `String`，因此这里统计的 `bytes` 是 UTF-8 文本字节数，不是原始输出字节数。
//!
//! # 主要流程
//! 1. `push` 追加文本：相邻同流片段合并，累计字节/换行数；累计超过约 4 倍窗口时触发 `trim`（摊薄扫描成本）。
//! 2. `trim` 从最旧的文本开始删，直到剩下的文本"自身仍超出窗口"，从而证明被删部分不可能出现在调用方
//!    尾部；被删内容累计进 `Skipped`。
//! 3. `take` 取走全部待发文本并生成事件：有跳过时合并为一个事件（流名取最后一段的），否则按流分段
//!    原样输出；同时按本次交付量计算下一次允许发送的时间 `next_send`。
//!
//! # 重要约束
//! - 本模块没有内部锁，`Pending` 只应在单个请求线程内使用（`exec::run` 的局部变量）。
//! - `Window::from_json` 是全有或全无：四个字段缺任意一个或类型不对，整个窗口视为未提供，
//!   `exec` 退化为不合并输出，而不是使用 0 值窗口。
//! - `skipped` 产生后必须一直带到下一次 `take`，否则调用方会误以为没有丢失输出。
//! - 行数只按 `\n` 统计；CRLF 的 `\r` 计入字节数但不构成额外的一行。
//!
//! # 阅读顺序
//! `Window::from_json` → `Pending::push`/`trim`（截断证明）→ `Pending::take`（事件形状与节奏）。

use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// 输出窗口参数（对应 Durable 的 `ShellExecOptions.window`）。
///
/// 四个字段都来自请求 JSON 且必须存在：
/// - `max_bytes`：调用方关心的尾部字节数上限；
/// - `max_lines`：调用方关心的尾部行数上限（按 `\n` 计数）；
/// - `min_interval`：两次交付之间的最小间隔（由 `minIntervalMs` 换算，负数按 0）；
/// - `bytes_per_second`：限速，至少 1.0，避免除零以及 `Duration::from_secs_f64` 收到非有限值。
#[derive(Clone, Copy)]
pub struct Window {
    /// 调用方关心的尾部字节数上限。
    pub max_bytes: usize,
    /// 调用方关心的尾部行数上限（按 `\n` 计数）。
    pub max_lines: usize,
    /// 两次交付之间的最小时间间隔。
    pub min_interval: Duration,
    /// 限速（字节/秒），至少 1.0。
    pub bytes_per_second: f64,
}

impl Window {
    /// 解析窗口参数；四个字段缺一不可，任何字段类型不符都返回 `None`。
    ///
    /// 返回 `None` 表示"没有窗口"，`exec` 会退化为不合并的逐块输出，而不是使用 0 值窗口。
    /// `maxBytes`/`maxLines` 以 `u64` 读取后转 `usize`；`minIntervalMs` 接受小数，负数夹到 0；
    /// `bytesPerSecond` 低于 1 时夹到 1。
    pub fn from_json(json: &Value) -> Option<Window> {
        let object = json.as_object()?;
        Some(Window {
            max_bytes: object.get("maxBytes")?.as_u64()? as usize,
            max_lines: object.get("maxLines")?.as_u64()? as usize,
            min_interval: Duration::from_secs_f64(
                object.get("minIntervalMs")?.as_f64()?.max(0.0) / 1000.0,
            ),
            bytes_per_second: object.get("bytesPerSecond")?.as_f64()?.max(1.0),
        })
    }
}

/// 一次交付之前被省略的文本统计，对应协议里的 `skipped` 对象。
///
/// `bytes` 与 `newlines` 是累计值（自上次 `take` 以来）；`ends_with_newline` 记录被省略文本
/// 是否以换行结束，原样作为协议字段 `endsWithNewline` 发给客户端。
#[derive(Default)]
struct Skipped {
    bytes: u64,
    newlines: u64,
    ends_with_newline: bool,
}

/// One output event: its stream, text, and the output omitted right before it.
///
/// `stream` 为 `"stdout"` 或 `"stderr"`；`skipped` 为 `Some` 时表示 `text` 之前有一批输出
/// 已被计数替换，JSON 形状即协议里的 `skipped` 对象。
pub struct Event {
    /// 输出流名：`"stdout"` 或 `"stderr"`；合并了跳过的那个事件取最后一段的流名。
    pub stream: &'static str,
    /// 本次交付的文本（已解码的 UTF-8）。
    pub text: String,
    /// 紧挨在 `text` 之前被省略的输出统计；没有省略时为 `None`。
    pub skipped: Option<Value>,
}

/// 一个命令尚未交付的输出缓冲区。
///
/// 生命周期（见 `exec::run`）：`Pending::new` → 多次 `push` → `take` 生成事件 → 循环往复，
/// 直到命令结束。它只在持有它的请求线程内被访问，不跨线程共享。
pub struct Pending {
    window: Window,
    /// Undelivered text in arrival order, by stream.
    segments: Vec<(&'static str, String)>,
    /// 待交付文本的总字节数与换行数，用于快速判断是否需要 `trim` 以及计算交付节奏。
    bytes: usize,
    newlines: usize,
    /// 自上次 `take` 以来被省略文本的累计统计；`take` 时取走。
    skipped: Option<Skipped>,
    /// 下一次允许交付的时间点；由上一次 `take` 按窗口限速推进。
    next_send: Instant,
}

// 按 `\n` 字节计数：UTF-8 的多字节字符不会包含 0x0A，所以按字节扫描与按字符判断等价。
fn newline_count(text: &str) -> usize {
    text.bytes().filter(|byte| *byte == b'\n').count()
}

impl Pending {
    /// 创建空缓冲区；`next_send` 初始为当前时刻，因此首次交付不被 `min_interval` 延迟。
    pub fn new(window: Window) -> Pending {
        Pending {
            window,
            segments: Vec::new(),
            bytes: 0,
            newlines: 0,
            skipped: None,
            next_send: Instant::now(),
        }
    }

    /// 是否没有待交付文本。只检查 `segments`：按 `trim` 的保留条件，产生 `skipped` 时一定还留有文本。
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// When the next delivery may happen.
    ///
    /// 值是上一次 `take` 计算出的时间点；没有待交付内容时 `exec` 不会查询它。
    pub fn next_send(&self) -> Instant {
        self.next_send
    }

    /// 追加一段已解码文本，并按需触发截断。
    ///
    /// 相邻且同流的文本直接拼接到最后一个片段，避免每块输出都产生一个事件；同时累加字节与换行数。
    /// 为摊薄扫描成本，只有累计超过约 4 个窗口（字节或行任一维度）时才调用 `trim`。
    pub fn push(&mut self, stream: &'static str, text: String) {
        if text.is_empty() {
            return;
        }
        self.bytes += text.len();
        self.newlines += newline_count(&text);
        match self.segments.last_mut() {
            Some((last, existing)) if *last == stream => existing.push_str(&text),
            _ => self.segments.push((stream, text)),
        }
        // Trimming costs a pass over the held text; do it once a few windows have accumulated.
        if self.bytes > 4 * (self.window.max_bytes + 1)
            || self.newlines > 4 * (self.window.max_lines + 1)
        {
            self.trim();
        }
    }

    /// Whether the text after a cut still proves the cut text is outside the window.
    ///
    /// 只要"剩下的文本"自身超过任一窗口上限，它之前的文本就不可能出现在调用方保留的尾部：
    /// 字节超限说明前面的内容距离末尾超过 `maxBytes`；行数超限说明距离末尾超过 `maxLines` 行。
    fn proves(&self, bytes: usize, newlines: usize) -> bool {
        bytes > self.window.max_bytes || newlines > self.window.max_lines
    }

    /// Replace the longest prefix whose remainder still proves exclusion by counts.
    ///
    /// 删除分两步（与函数内注释对应）：
    /// 1. 整段删除：只要去掉第一段后剩余部分仍满足 `proves`，就整段删；
    /// 2. 对第一段做前缀切割：分别按字节和按行算出必须保留多少，取两者中更大的切割点；
    ///    字节切割点会回退到 UTF-8 字符边界，绝不把一个多字节字符切成两半。
    ///
    /// 被删文本按出现顺序拼进 `removed`，最后累计进 `Skipped`，并更新 `ends_with_newline`。
    /// 注意：保留量只保证在某个维度上超出窗口（至少 +1）；字符边界回退还可能再多保留最多 3 字节。
    /// 因此一次 `take` 交付的文本可能大于窗口：`trim` 刚结束时约为"窗口 + 少量字节"，之后继续
    /// `push` 还可累积到接近 4 倍窗口（超过该阈值才会再次 `trim`）。
    fn trim(&mut self) {
        let mut removed = String::new();
        // Whole segments first.
        // 第一步：整段删除，直到只删掉第一段就无法再证明剩余文本超出窗口为止。
        while self.segments.len() > 1 {
            let (bytes, newlines) = {
                let first = &self.segments[0].1;
                (first.len(), newline_count(first))
            };
            if !self.proves(self.bytes - bytes, self.newlines - newlines) {
                break;
            }
            let (_, text) = self.segments.remove(0);
            self.bytes -= bytes;
            self.newlines -= newlines;
            removed.push_str(&text);
        }
        // Then a prefix of the first remaining segment, at a character boundary.
        // 第二步：计算第一段的前缀切点；`rest_*` 是除第一段以外部分的总量。
        let rest_bytes = self.bytes - self.segments[0].1.len();
        let rest_newlines = self.newlines - newline_count(&self.segments[0].1);
        let first = &self.segments[0].1;
        let mut cut = 0;
        // By bytes: keep more than `max_bytes` overall.
        // 按字节：总量要保留 `max_bytes + 1`，换算成本段需要保留的字节数；本段更短就整段保留。
        let keep_bytes = (self.window.max_bytes + 1).saturating_sub(rest_bytes);
        if first.len() > keep_bytes {
            let mut index = first.len() - keep_bytes;
            // 回退到 UTF-8 字符边界：宁可多保留最多 3 个字节，也不劈开一个字符。
            while index > 0 && !first.is_char_boundary(index) {
                index -= 1;
            }
            cut = cut.max(index);
        }
        // By lines: keep more than `max_lines` newlines overall, starting at a newline of this segment.
        // 按行：总量要保留 `max_lines + 1` 个换行；切点取本段倒数第 `keep_newlines` 个换行的位置，
        // 即保留该换行及其后的内容。
        let keep_newlines = (self.window.max_lines + 1).saturating_sub(rest_newlines);
        if keep_newlines > 0 {
            let positions: Vec<usize> = first
                .bytes()
                .enumerate()
                .filter(|(_, byte)| *byte == b'\n')
                .map(|(index, _)| index)
                .collect();
            if positions.len() >= keep_newlines {
                cut = cut.max(positions[positions.len() - keep_newlines]);
            }
            // 本段换行不足时行数条件无法由本段满足，不做行数切割（字节切割或后续数据会补上）。
        } else {
            // 后面段落已经有足够多的换行，第一段可以整体删除。
            cut = first.len();
        }
        if cut > 0 {
            // 实际删除前缀；第一段被删空时从列表移除。
            let first = &mut self.segments[0].1;
            let prefix: String = first.drain(..cut).collect();
            self.bytes -= prefix.len();
            self.newlines -= newline_count(&prefix);
            removed.push_str(&prefix);
            if first.is_empty() {
                self.segments.remove(0);
            }
        }
        if !removed.is_empty() {
            // 累计到 `skipped`：字节与行数按删除文本统计；结尾状态取本次最后删除的文本。
            let skipped = self.skipped.get_or_insert_with(Skipped::default);
            skipped.bytes += removed.len() as u64;
            skipped.newlines += newline_count(&removed) as u64;
            skipped.ends_with_newline = removed.ends_with('\n');
        }
    }

    /// Deliver everything held: one event carrying the skip and all text after it, or one event per segment.
    ///
    /// 有跳过时：`stream` 取最后一个片段的流（没有片段时为 `"stdout"`），`text` 是按到达顺序拼接的
    /// 全部剩余文本，`skipped` 是累计计数——调用方因此能知道"被省略了多少、接着的原文是什么"。
    /// 注意被省略部分只留计数，不再保留逐流的归属。没有跳过时：每个片段一个事件，保留各自的流名。
    ///
    /// 同时重置计数并按本次交付量设置 `next_send`：至少等待 `min_interval`，且按
    /// `min(delivered, max_bytes) / bytes_per_second` 估算这批数据发完所需时间。
    pub fn take(&mut self) -> Vec<Event> {
        // 取走待发片段，结构立刻可以继续接收新数据。
        let segments = std::mem::take(&mut self.segments);
        let delivered = self.bytes;
        self.bytes = 0;
        self.newlines = 0;
        // The caller writes at most the window per sample, and paces itself by what it wrote.
        // 交付节奏：按交付字节数与限速估算，且不小于最小间隔；上限截到窗口大小（近似值）。
        let pace = Duration::from_secs_f64(
            delivered.min(self.window.max_bytes) as f64 / self.window.bytes_per_second,
        );
        self.next_send = Instant::now() + self.window.min_interval.max(pace);
        match self.skipped.take() {
            Some(skipped) => {
                // 有跳过：合成一个事件；流名取最后一段，前面各流的归属只体现在计数里。
                let stream = segments
                    .last()
                    .map(|(stream, _)| *stream)
                    .unwrap_or("stdout");
                let text: String = segments.into_iter().map(|(_, text)| text).collect();
                vec![Event {
                    stream,
                    text,
                    skipped: Some(json!({
                        "bytes": skipped.bytes,
                        "newlines": skipped.newlines,
                        "endsWithNewline": skipped.ends_with_newline,
                    })),
                }]
            }
            // 没有跳过：保持原有分段与流名，逐段交付。
            None => segments
                .into_iter()
                .map(|(stream, text)| Event {
                    stream,
                    text,
                    skipped: None,
                })
                .collect(),
        }
    }
}

// 测试：跳过与交付的字节/行数守恒、跳过后保留的尾部确实是原文后缀、跨流时的分段行为。
#[cfg(test)]
mod tests {
    use super::*;

    fn window(max_bytes: usize, max_lines: usize) -> Window {
        Window {
            max_bytes,
            max_lines,
            min_interval: Duration::ZERO,
            bytes_per_second: 1e12,
        }
    }

    /// Bytes and newlines delivered or skipped, and the text after the last skip.
    fn total(events: &[Event]) -> (u64, u64, String) {
        let mut bytes = 0;
        let mut newlines = 0;
        let mut after = String::new();
        for event in events {
            if let Some(skipped) = &event.skipped {
                bytes += skipped["bytes"].as_u64().unwrap();
                newlines += skipped["newlines"].as_u64().unwrap();
                after.clear();
            }
            bytes += event.text.len() as u64;
            newlines += newline_count(&event.text) as u64;
            after.push_str(&event.text);
        }
        (bytes, newlines, after)
    }

    #[test]
    fn counts_everything_and_keeps_more_than_the_window_after_a_skip() {
        let mut pending = Pending::new(window(10, 2));
        let mut full = String::new();
        let mut events = Vec::new();
        for index in 0..500 {
            let text = format!("line {index} é😀\n");
            full.push_str(&text);
            pending.push(if index % 7 == 0 { "stderr" } else { "stdout" }, text);
            if index % 50 == 49 {
                events.extend(pending.take());
            }
        }
        events.extend(pending.take());
        let (bytes, newlines, after) = total(&events);
        assert_eq!(bytes, full.len() as u64);
        assert_eq!(newlines, newline_count(&full) as u64);
        assert!(full.ends_with(&after));
        assert!(after.len() > 10 || newline_count(&after) > 2);
        assert!(events.iter().any(|event| event.skipped.is_some()));
    }

    #[test]
    fn delivers_streams_apart_without_skips() {
        let mut pending = Pending::new(window(1000, 1000));
        pending.push("stdout", "a".into());
        pending.push("stderr", "b".into());
        pending.push("stdout", "c".into());
        let events = pending.take();
        let streams: Vec<_> = events.iter().map(|event| event.stream).collect();
        assert_eq!(streams, ["stdout", "stderr", "stdout"]);
    }
}

// 测试：窗口解析必须四字段齐全、类型正确；小数毫秒、负数间隔与低速率的夹取。
#[cfg(test)]
mod parsing_tests {
    use super::*;

    #[test]
    fn a_window_needs_all_four_fields_and_keeps_fractions() {
        assert!(Window::from_json(&json!(null)).is_none());
        assert!(Window::from_json(&json!(7)).is_none());
        let full = json!({
            "maxBytes": 1000,
            "maxLines": 20,
            "minIntervalMs": 20,
            "bytesPerSecond": 1_000_000,
        });
        let window = Window::from_json(&full).unwrap();
        assert_eq!(window.max_bytes, 1000);
        assert_eq!(window.max_lines, 20);
        assert_eq!(window.min_interval, Duration::from_millis(20));
        assert_eq!(window.bytes_per_second, 1_000_000.0);
        // Every field is required; a missing one makes the whole window absent.
        for key in ["maxBytes", "maxLines", "minIntervalMs", "bytesPerSecond"] {
            let mut partial = full.clone();
            partial.as_object_mut().unwrap().remove(key);
            assert!(Window::from_json(&partial).is_none(), "missing {key}");
        }
        // A wrong type is the same as missing.
        let mut wrong = full.clone();
        wrong["maxBytes"] = json!("1000");
        assert!(Window::from_json(&wrong).is_none());
    }

    #[test]
    fn fractional_milliseconds_clamp_negative_intervals_and_a_slow_rate() {
        let window = Window::from_json(&json!({
            "maxBytes": 0,
            "maxLines": 0,
            "minIntervalMs": 0.5,
            "bytesPerSecond": 0.25,
        }))
        .unwrap();
        assert_eq!(window.min_interval, Duration::from_secs_f64(0.0005));
        assert_eq!(
            window.bytes_per_second, 1.0,
            "clamped up to one byte a second"
        );
        let window = Window::from_json(&json!({
            "maxBytes": 1,
            "maxLines": 1,
            "minIntervalMs": -5,
            "bytesPerSecond": -1,
        }))
        .unwrap();
        assert_eq!(window.min_interval, Duration::ZERO);
        assert_eq!(window.bytes_per_second, 1.0);
    }
}

// 测试：空 push 被忽略、同流片段合并、`take` 重置计数与推进节奏、跳过事件的合并形状，
// 以及截断不会劈开 UTF-8 字符。
#[cfg(test)]
mod pending_tests {
    use super::*;

    fn window(max_bytes: usize, max_lines: usize) -> Window {
        Window {
            max_bytes,
            max_lines,
            min_interval: Duration::ZERO,
            bytes_per_second: 1e12,
        }
    }

    #[test]
    fn an_empty_push_is_ignored_and_segments_merge_per_stream() {
        let mut pending = Pending::new(window(100, 100));
        assert!(pending.is_empty());
        pending.push("stdout", String::new());
        assert!(pending.is_empty(), "empty text adds nothing");
        pending.push("stdout", "a".into());
        pending.push("stdout", "b".into());
        assert_eq!(pending.segments, [("stdout", "ab".to_string())]);
        pending.push("stderr", "c".into());
        assert_eq!(pending.segments.len(), 2);
        assert!(!pending.is_empty());
    }

    #[test]
    fn take_resets_the_counters_and_paces_the_next_send() {
        let mut pending = Pending::new(Window {
            max_bytes: 1000,
            max_lines: 1000,
            min_interval: Duration::from_millis(20),
            bytes_per_second: 1000.0,
        });
        pending.push("stdout", "abcdef".into());
        let events = pending.take();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].text, "abcdef");
        assert!(events[0].skipped.is_none());
        // The next send waits at least `min_interval`, and at least bytes/rate for the delivered bytes.
        let now = Instant::now();
        let wait = pending.next_send().saturating_duration_since(now);
        assert!(wait >= Duration::from_millis(5), "waited {wait:?}");
        assert!(pending.is_empty());
    }

    #[test]
    fn a_skip_collapses_the_segments_into_one_event_with_counts() {
        // Two windows of unread text with distinct streams: the skip joins them into one event that keeps the last
        // stream's name and holds only text from the cut onwards, a suffix of everything pushed.
        let mut pending = Pending::new(window(2, 2));
        let mut full = String::new();
        for index in 0..40 {
            let text = format!("line {index}\n");
            full.push_str(&text);
            pending.push(if index % 2 == 0 { "stdout" } else { "stderr" }, text);
        }
        let events = pending.take();
        assert_eq!(
            events.len(),
            1,
            "a skip merges every segment into one event"
        );
        let skipped = events[0].skipped.as_ref().expect("skipped counts");
        assert!(skipped["bytes"].as_u64().unwrap() > 0);
        assert!(skipped["newlines"].as_u64().unwrap() > 0);
        assert!(skipped["endsWithNewline"].is_boolean());
        // The event's stream is the last segment's.
        assert_eq!(events[0].stream, "stderr");
        assert!(full.ends_with(&events[0].text), "kept {:?}", events[0].text);
        // Counted and kept text together account for every byte and newline.
        assert_eq!(
            skipped["bytes"].as_u64().unwrap() + events[0].text.len() as u64,
            full.len() as u64
        );
        assert_eq!(
            skipped["newlines"].as_u64().unwrap() + newline_count(&events[0].text) as u64,
            newline_count(&full) as u64
        );
    }

    #[test]
    fn trimming_never_splits_a_character() {
        // A window too small for one character forces the trim on multi-byte text.
        let mut pending = Pending::new(window(0, 0));
        for _ in 0..64 {
            pending.push("stdout", "é".into());
        }
        let events = pending.take();
        assert_eq!(events.len(), 1);
        assert!(events[0].text.is_char_boundary(0));
        assert!(events[0].text.chars().all(|character| character == 'é'));
    }
}
