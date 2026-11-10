//! `output.ts`：有界输出缓冲与自适应进度提交。
//!
//! `OutputBuffer` 按块保留一段 head/tail 窗口（代价与块大小成正比），`Progress` 按写入
//! 体积给提交节流。
//!
//! JS 语义要点（逐条对应 TS 源）：
//! - 所有长度/切分都在 **UTF-8 字节** 上进行；`boundOutput` 的切片落在字符边界。
//! - `TextDecoder("utf-8", { ignoreBOM: true })`：流式解码，未完成的字符留到下一块；
//!   `decode()` 无参调用把残留字节冲成 **一个** U+FFFD；BOM 不被解码器剥离，
//!   `OutputBuffer` 只在流的最开头、且首块是字节块时手工剥一次。
//! - `sanitizeOutput` 删除控制字符（保留 tab 与换行），只作用于保留文本，计数仍按原始流。
//! - Rust 的 `String` 不能包含孤立代理项；TS 里会把孤立代理项编码成 U+FFFD（3 字节），
//!   因此 [`utf8_byte_length`] 就是 `text.len()`（合法 UTF-8 的字节长度），行为等价。
//!
//! `Progress`：同一时刻最多一个提交在飞；期间 `mark` 合并进下一次；首次（`nextAt = 0`）
//! 立即提交；`stop` 返回未被任何提交覆盖的 waiter，由调用方结算。
//!
//! # 在 crate 中的位置
//!
//! 这是一个纯工具模块：不引用 crate 内其它模块，也不做网络、文件或数据库访问。`lib.rs`
//! 仅用 `pub mod output;` 声明本模块。仓库内的使用方包括 `kiana-durable` 的
//! `harness/tool.rs`、`tools/read.rs`、`truncate.rs` 及其集成测试，另有本文件的单元测试与
//! `tests/output.rs`（对应 `packages/chord/test/output.test.ts`）、`tests/output_skip.rs`
//! （对应 `packages/chord/test/output-skip.test.ts`）。TS 源是
//! `packages/chord/src/output.ts`。
//!
//! # 两部分职责
//!
//! 1. **有界输出**（[`OutputBuffer`]）：工具调用或进程的输出可能无限长，而调用方通常只需要
//!    开头若干行（head）或结尾若干行（tail）。缓冲按块保存，同时用整条流的计数器保证
//!    “丢弃了多少”始终是精确值，与存储里还剩哪些块无关。
//! 2. **进度提交节流**（[`Progress`]）：值一变就安排异步写入，而写入可能很慢。同一时刻
//!    最多一个提交在飞，期间的变化合并；下一次提交的时间由最小间隔和写入体积共同决定。
//!
//! # 一次输出块的处理流程
//!
//! 1. [`OutputBuffer::push`] 决定解码方式：字符串块直接用，字节块交给流式解码器；如果这是
//!    字符串块，或者该块带有 skip，先冲掉上一个字节块残留的不完整字符。
//! 2. 解码后的文本交给内部的 `accept`：先累计整条流的字节/换行计数，再按 head 或 tail
//!    策略决定是否存储、如何淘汰旧块。
//! 3. [`OutputBuffer::snapshot`] 把已存块拼接，交给 [`bound_output`] 做整行窗口裁切，再用
//!    [`sanitize_output`] 删控制字符，最后用整条流的计数算出 dropped 值。
//!
//! # 不变量
//!
//! - 计数在**解码后、消毒前**的文本上累计，包括已经被丢弃的块和由 [`OutputSkip`] 上报的
//!   省略部分；dropped 的精确性因此不依赖存储里还剩哪些块。
//! - `bound_output` 的切点只落在换行之后或 UTF-8 字符边界上，不会产生半个字符。
//! - [`OutputBuffer::snapshot`] 需要 `&mut self`：它会压缩内部存储（tail 只保留比窗口多
//!   一字节或一行的后缀，head 合并成一块），而压缩不得改变窗口内容。
//!
//! # 并发与生命周期
//!
//! [`Progress`] 的全部可变状态在 `std::sync::Mutex<ProgressState>` 后面；`mark` 只改动状态并
//! 尝试安排，真正的写入由 `spawn_detached` 在 Tokio 任务（有运行时）或名为 `chord-progress`
//! 的专用线程（无运行时）上执行。后台任务持有状态的 `Arc`，因此 `Progress` 被丢弃不会取消
//! 已在飞的提交。`write` 与 `on_error` 都不在持有锁时调用，`stop` 也不持锁等待在飞提交。
//!
//! # 失败模式
//!
//! - `push` 唯一会报的错是 head 保留却收到 [`OutputSkip`]；此时块已经被解码（见
//!   [`OutputBuffer::push`]），所以这个错误不是“无副作用”的。
//! - `Progress` 的写入失败由 [`ProgressFailure`] 广播给这一批 waiter，并调用一次错误回调；
//!   下一次提交至少等 `min_interval_ms`。
//! - [`Progress::stop`] 返回的 waiter 必须由调用方结算；丢弃发送端会让等待方得到
//!   [`ProgressAbandoned`]。

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::oneshot;

/// 换行字节（TS: `NEWLINE`，非导出）。
const NEWLINE: u8 = 0x0a;
/// 替换字符（解码失败/未完成序列）。
const REPLACEMENT: char = '\u{fffd}';

/// 保留哪一端（TS: `retain: "head" | "tail"`）。
///
/// 它还决定 [`OutputBuffer`] 是否接受 [`OutputSkip`]：只有 [`Retain::Tail`] 能接受，
/// head 保留收到 skip 会返回 [`OutputError`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Retain {
    /// 保留开头。
    Head,
    /// 保留结尾。
    Tail,
}

/// 一条输出流的保留限制（TS: `OutputLimits`；全必需）。
///
/// `max_bytes` 与 `max_lines` 同时生效，先触顶的那个决定窗口大小；任一为 0 时窗口为空
/// （[`bound_output`] 返回空切片）。构造用 [`OutputLimits::new`] 时注意参数是**位置参数**
/// `(max_bytes, max_lines, retain)`，与 TS 的对象字段顺序无关。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OutputLimits {
    /// 最大保留字节数。
    ///
    /// 窗口的硬上限；`max_lines` 先触顶时实际保留可能更少。
    pub max_bytes: usize,
    /// 最大保留行数。
    ///
    /// 行以 U+000A 计；末尾没有换行的半行也算一行。
    pub max_lines: usize,
    /// 保留端。
    ///
    /// head 丢弃尾部，tail 丢弃头部。
    pub retain: Retain,
}

impl OutputLimits {
    /// 构造限制。
    ///
    /// 参数顺序是 `(max_bytes, max_lines, retain)`；两个便捷构造见
    /// [`OutputLimits::head`] 与 [`OutputLimits::tail`]。
    pub fn new(max_bytes: usize, max_lines: usize, retain: Retain) -> Self {
        OutputLimits {
            max_bytes,
            max_lines,
            retain,
        }
    }

    /// head 限制的便捷构造。
    pub fn head(max_bytes: usize, max_lines: usize) -> Self {
        OutputLimits::new(max_bytes, max_lines, Retain::Head)
    }

    /// tail 限制的便捷构造。
    pub fn tail(max_bytes: usize, max_lines: usize) -> Self {
        OutputLimits::new(max_bytes, max_lines, Retain::Tail)
    }
}

/// 保留的输出以及限制丢弃了什么（TS: `BoundedOutput`）。
///
/// `dropped_bytes` / `dropped_lines` 以**整条流**为基准：整条流的计数减去实际保留的部分，
/// 因此包含早已从存储里丢掉的块和 skip 省略的内容。它们按裁切后的字节数计算，而
/// [`sanitize_output`] 发生在其后，所以被删掉的控制字符仍算“保留”：它们不会出现在
/// `text` 里，也不会让 dropped 变大。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedOutput {
    /// 已消毒的保留文本。
    ///
    /// 控制字符已经由 [`sanitize_output`] 删除。
    pub text: String,
    /// 整个流里被丢弃的字节数。
    ///
    /// 含 [`OutputSkip`] 上报的省略部分。
    pub dropped_bytes: usize,
    /// 整个流里被丢弃的行数。
    ///
    /// 含 skip 省略的行；行数以原始换行为准。
    pub dropped_lines: usize,
}

/// 输入在限制内的精确切片，以及被略过的部分（TS: `OutputSlice`）。
///
/// 由 [`bound_output`] 产生：`text` 是输入的精确前缀（head）或后缀（tail），未裁剪时与输入
/// 逐字节相同；`bytes` 是切片的 UTF-8 字节长度（等于 `text.len()`），`dropped_bytes` 是
/// “输入字节数 - `bytes`”。`dropped_lines` 是两边行数的差，因此**被字节上限劈开的半行
/// 仍算保留**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSlice {
    /// 保留文本（未裁剪时与输入相同）。
    pub text: String,
    /// 保留字节数。
    pub bytes: usize,
    /// 被丢弃的字节数。
    pub dropped_bytes: usize,
    /// 被丢弃的行数。
    pub dropped_lines: usize,
}

/// 生产者在某块之前省略的输出，按解码后的文本计量：每个 U+FFFD 记 3 字节，
/// 不做任何消毒（TS: `OutputSkip`）。
///
/// 这是调用方必须遵守的契约，而不是可以随意填的统计：省略的部分必须比 tail 窗口至少多
/// 一字节或一行。`push` 只检查保留端，无法验证这个关系；上报不足会让之后的窗口与“从未
/// 省略”的结果不一致。`tests/output_skip.rs` 用“完整缓冲 vs 跳过缓冲”逐位对比来验证
/// 合法用法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct OutputSkip {
    /// 被省略文本的 UTF-8 字节长度。
    ///
    /// 每个 U+FFFD 记 3 字节，与解码后文本的度量一致。
    pub bytes: usize,
    /// 被省略文本里的换行（U+000A）数。
    ///
    /// 回车 U+000D 不算换行。
    pub newlines: usize,
    /// 被省略文本是否以换行结尾。
    ///
    /// 它决定省略文本的最后一行是否延续到后续文本里。
    pub ends_with_newline: bool,
}

/// 删除会破坏显示/转录的控制字符；tab 与换行保留（TS: `sanitizeOutput`）。
///
/// 被删的是 U+0000–U+0008、U+000B–U+001F（含回车 U+000D）与 U+FFF9–U+FFFB；制表符 U+0009
/// 与换行 U+000A 保留。它只作用于要展示的文本，不影响任何字节/行计数。
pub fn sanitize_output(text: &str) -> String {
    text.chars()
        .filter(|character| {
            let code = *character as u32;
            !((0x00..=0x08).contains(&code)
                || (0x0b..=0x1f).contains(&code)
                || (0xfff9..=0xfffb).contains(&code))
        })
        .collect()
}

/// 把 `text` 限制为 `limits` 内的整行：head 取前几行，tail 取后几行。结果是精确切片，
/// 含尾随换行；单行超过 `max_bytes` 时在字节限制处按字符边界切（TS: `boundOutput`）。
///
/// 输入是合法的 `&str`（UTF-8），切点只落在换行之后或字符边界上，所以下面的
/// `from_utf8_lossy` 不会真的产生替换字符。
///
/// 边界：
/// - `max_bytes == 0` 或 `max_lines == 0`：返回空文本，`bytes = 0`，全部计入 dropped
///   （见 `tests/output.rs` 的 `keeps_nothing_with_a_zero_limit`）。
/// - head：先按行取前 `max_lines` 行；若仍超过字节上限，回退到“前 `max_bytes` 字节里最后
///   一个换行之后”，没有换行时在该字节位置按字符边界切（可能劈开一行）。
/// - tail：对称地从尾部取行；整行太长时在窗口起点按字符边界切。
pub fn bound_output(text: &str, limits: &OutputLimits) -> OutputSlice {
    let bytes = text.as_bytes();
    let (from, to) = match limits.retain {
        Retain::Head => head_range(bytes, limits),
        Retain::Tail => tail_range(bytes, limits),
    };
    let kept = &bytes[from..to];
    // 未裁剪时直接复用原字符串；否则从字节切片重建（切点在字符边界上，不会丢内容）。
    let kept_text = if kept.len() == bytes.len() {
        text.to_owned()
    } else {
        String::from_utf8_lossy(kept).into_owned()
    };
    OutputSlice {
        text: kept_text,
        bytes: kept.len(),
        dropped_bytes: bytes.len() - kept.len(),
        // 两边行数相减：被字节上限劈开的半行在 kept 里仍算一行，不会计入 dropped。
        dropped_lines: line_count(bytes) - line_count(kept),
    }
}

// head 窗口的字节范围 `[0, end)`，两阶段：
// 1) 从前往后数换行，切在第 `max_lines` 个换行之后（窗口因此包含尾随换行）；
// 2) 若 `end` 仍超过 `max_bytes`，退回到“前 `max_bytes` 字节里最后一个换行之后”，
//    一个换行都没有时就在 `max_bytes` 处按字符边界切。
// 限制非零时返回的 `end` 不超过 `max_bytes`，且一定落在字符边界上。
fn head_range(bytes: &[u8], limits: &OutputLimits) -> (usize, usize) {
    if limits.max_lines == 0 || limits.max_bytes == 0 {
        return (0, 0);
    }
    let mut end = bytes.len();
    let mut lines = 0usize;
    let mut index = find_newline(bytes, 0);
    while let Some(at) = index {
        lines += 1;
        if lines == limits.max_lines {
            end = at + 1;
            break;
        }
        index = find_newline(bytes, at + 1);
    }
    if end > limits.max_bytes {
        // `bytes.lastIndexOf(NEWLINE, maxBytes - 1)`：只在 [0, maxBytes) 里找。
        let newline = bytes[..limits.max_bytes.min(bytes.len())]
            .iter()
            .rposition(|byte| *byte == NEWLINE);
        end = match newline {
            Some(at) => at + 1,
            None => character_end(bytes, limits.max_bytes),
        };
    }
    (0, end)
}

// tail 窗口的字节范围 `[start, len)`，两阶段：
// 1) 从尾部往前数行。尾随换行只是结束最后一行，不另起一行；找到第 `max_lines` 个换行，
//    切在它之后。
// 2) 若剩余长度超过 `max_bytes`，从 `len - max_bytes` 起找窗口里（或紧邻窗口之前）的第一个
//    换行，切到它之后；整行都太长时按字符边界切。
// 限制非零时返回的 `start` 落在字符边界上，切片长度不超过 `max_bytes`。
fn tail_range(bytes: &[u8], limits: &OutputLimits) -> (usize, usize) {
    if limits.max_lines == 0 || limits.max_bytes == 0 {
        return (bytes.len(), bytes.len());
    }
    // 尾随换行结束最后一行，而不是另起一行。
    let last = if bytes.last() == Some(&NEWLINE) {
        bytes.len() as isize - 2
    } else {
        bytes.len() as isize - 1
    };
    let mut start = 0usize;
    let mut lines = 1usize;
    let mut index: Option<usize> = if last < 0 {
        None
    } else {
        rfind_newline(bytes, last as usize + 1)
    };
    while let Some(at) = index {
        if lines == limits.max_lines {
            start = at + 1;
            break;
        }
        lines += 1;
        index = if at == 0 {
            None
        } else {
            rfind_newline(bytes, at)
        };
    }
    if bytes.len() - start > limits.max_bytes {
        let from = bytes.len() - limits.max_bytes;
        let newline = find_newline(bytes, from.saturating_sub(1));
        // 字节窗口内的第一个行首；整行都太长时切最后一行。
        start = match newline {
            Some(at) if at + 1 < bytes.len() => at + 1,
            _ => character_start(bytes, from),
        };
    }
    (start, bytes.len())
}

/// `bytes.indexOf(NEWLINE, from)`，找不到为 `None`。
fn find_newline(bytes: &[u8], from: usize) -> Option<usize> {
    if from >= bytes.len() {
        return None;
    }
    bytes[from..]
        .iter()
        .position(|byte| *byte == NEWLINE)
        .map(|at| from + at)
}

/// 在 `bytes[..end]` 里从后往前找换行。
fn rfind_newline(bytes: &[u8], end: usize) -> Option<usize> {
    bytes[..end.min(bytes.len())]
        .iter()
        .rposition(|byte| *byte == NEWLINE)
}

/// 在 `index` 处或之前的最后一个字符边界（TS: `characterEnd`，导出）。
///
/// 判定的是 `bytes[index]` 本身：UTF-8 续字节的高两位是 `10`，遇到就向前退，直到
/// `index == 0` 或该位置不是续字节。索引越界时 `bytes.get` 取到 `None`（按 0 处理，与 TS
/// 的 `bytes[end] ?? 0` 一致），于是原样返回 `index`。返回值在 `0..=index` 内；当
/// `index <= bytes.len()` 时，把它用作切片终点不会劈开字符。
pub fn character_end(bytes: &[u8], index: usize) -> usize {
    let mut end = index;
    while end > 0 && (bytes.get(end).copied().unwrap_or(0) & 0xc0) == 0x80 {
        end -= 1;
    }
    end
}

/// 在 `index` 处或之后的第一个字符边界（TS: `characterStart`，非导出）。
///
/// 与 [`character_end`] 方向相反：从 `index` 开始向后跳过续字节。对 `index <= bytes.len()`
/// （本模块的调用点都满足），返回值要么落在字符边界，要么等于 `bytes.len()`。
fn character_start(bytes: &[u8], index: usize) -> usize {
    let mut start = index;
    while start < bytes.len() && (bytes.get(start).copied().unwrap_or(0) & 0xc0) == 0x80 {
        start += 1;
    }
    start
}

// 空文本 0 行；否则数换行，末尾没有换行时那个半行也算一行。
// 因此 "a\n" 是 1 行，"a" 是 1 行，"a\n\n" 是 2 行。
fn line_count(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    let newlines = bytes.iter().filter(|byte| **byte == NEWLINE).count();
    newlines + usize::from(bytes[bytes.len() - 1] != NEWLINE)
}

/// `content` 的 UTF-8 字节长度（TS: `utf8ByteLength`）。
///
/// 返回的是编码后的字节数，不是字符数：`é` 为 2、`😀` 为 4。实现就是 `str::len`，因为
/// `&str` 已经是合法 UTF-8。
///
/// TS 里孤立的 UTF-16 代理项记 3 字节（与 `TextEncoder` 一致）；Rust 的 `&str` 不可能含
/// 孤立代理项，因此合法的 UTF-8 字节长度就是答案。
pub fn utf8_byte_length(content: &str) -> usize {
    content.len()
}

// 只数 U+000A；回车 U+000D 不算换行（消毒时它会被删除，但计数发生在消毒之前）。
fn count_newlines(text: &str) -> usize {
    text.as_bytes()
        .iter()
        .filter(|byte| **byte == NEWLINE)
        .count()
}

/// `lines(newlines, terminated)`：`newlines` 个换行；末尾未终止的行也算一行。
fn lines(newlines: usize, terminated: bool) -> usize {
    newlines + usize::from(!terminated)
}

/// 一块输出：字符串块或字节块（TS: `string | Uint8Array`）。
///
/// 两种块的解码路径不同：`Text` 不经过解码器，并会先把之前不完整的字节序列冲成 U+FFFD；
/// `Bytes` 可以跨块切分字符。除解码路径外，流首 BOM 的处理也不同：只有 `Bytes` 块会在
/// 流的最开头剥掉 U+FEFF，`Text` 块开头的 U+FEFF 保留。因此仅当首块不以 U+FEFF 开头时，
/// 同一段合法 UTF-8 用两种块投递才得到相同快照
/// （`tests/output.rs` 的 `output_chunk_text_and_bytes_agree_for_valid_utf8`）。
#[derive(Debug, Clone, Copy)]
pub enum OutputChunk<'a> {
    /// 字符串块（不再按 UTF-8 解码）。
    ///
    /// 调用方保证它已经是合法文本；`&str` 本身也保证不含半个字符。
    Text(&'a str),
    /// UTF-8 字节块（可跨块切分字符）。
    ///
    /// 任意切分都合法：不完整的序列留到下一块，或者在 [`OutputBuffer::end`] 时变成 U+FFFD。
    Bytes(&'a [u8]),
}

/// `push` 的契约错误；消息是稳定的对外契约（改动需同步测试）。
///
/// 目前只有一种：head 保留收到 [`OutputSkip`]。没有错误码或分类，调用方只能按消息或
/// [`OutputError::TAIL_RETENTION`] 判断；`Display` 输出的就是 `message`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputError {
    /// 完整消息。
    ///
    /// 是稳定的错误文案，测试会直接比较字符串（改动需同步测试）。
    pub message: String,
}

impl OutputError {
    /// head 保留拒绝 `skipped`。
    ///
    /// 这段文本是契约的一部分：`push` 的错误消息是稳定文案，不要改写（改动需同步测试）。
    pub const TAIL_RETENTION: &'static str = "Skipped output requires tail retention";

    fn tail_retention() -> Self {
        OutputError {
            message: Self::TAIL_RETENTION.to_owned(),
        }
    }
}

impl std::fmt::Display for OutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OutputError {}

/// 流式 UTF-8 解码器，语义与 `TextDecoder("utf-8", { ignoreBOM: true })` 的流式模式一致：
/// - 未完成的序列留到下一块；
/// - 刷写（[`Utf8StreamDecoder::flush`]）把残留序列冲成 **一个** U+FFFD；
/// - 非法字节按 WHATWG 的"最大子部分"规则各产生一个 U+FFFD，并重新处理越界字节；
/// - 不剥离 BOM（`ignoreBOM: true`）。
///
/// 之所以手写而不是用 `std`：逐块解码必须把“已读到半个字符”的状态留到下一块，
/// 这正是下面这几个字段存在的意义。状态里的 `bytes_needed == 0` 表示当前不在序列中。
///
/// 注意：派生的 `Default` 会把 `lower`/`upper` 置为 0，与 [`Utf8StreamDecoder::new`] 的
/// `0x80`/`0xbf` 不同。`Default` 的 0 初值会让多数首字节后的续字节被拒绝，但 `ED`/`F4`
/// 只收窄上界（`lower` 仍为 0），仍可能放行部分续字节；`Default` 不可使用，本模块只用
/// `new()` 构造。
#[derive(Debug, Default)]
struct Utf8StreamDecoder {
    /// 已累积码点的低位部分。
    code_point: u32,
    /// 当前序列已读到的续字节数。
    bytes_seen: usize,
    /// 当前序列总共还需要的续字节数（0 表示空闲）。
    bytes_needed: usize,
    /// 下一个续字节允许的下界；首字节 E0/ED/F0/F4 会收窄它。
    lower: u8,
    /// 下一个续字节允许的上界。
    upper: u8,
}

impl Utf8StreamDecoder {
    fn new() -> Self {
        Utf8StreamDecoder {
            code_point: 0,
            bytes_seen: 0,
            bytes_needed: 0,
            lower: 0x80,
            upper: 0xbf,
        }
    }

    /// `decode(chunk, { stream: true })`。
    ///
    /// 只产生本块能确定的字符；末尾不完整的序列留在 `self` 里。
    fn decode_stream(&mut self, bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len());
        for byte in bytes {
            self.feed(*byte, &mut out);
        }
        out
    }

    /// `decode()`：冲掉未完成的序列。
    ///
    /// 没有残留时返回空串；有残留时无论已经读到几个续字节，都只产生**一个** U+FFFD 并重置
    /// 状态（与 `TextDecoder.decode()` 无参调用一致）。
    fn flush(&mut self) -> String {
        if self.bytes_needed == 0 {
            return String::new();
        }
        self.reset();
        REPLACEMENT.to_string()
    }

    fn reset(&mut self) {
        self.code_point = 0;
        self.bytes_seen = 0;
        self.bytes_needed = 0;
        self.lower = 0x80;
        self.upper = 0xbf;
    }

    /// 处理一个字节；不完整的序列状态留在 `self` 里。
    fn feed(&mut self, byte: u8, out: &mut String) {
        if self.bytes_needed == 0 {
            // 空闲状态：按首字节决定序列长度。孤立的续字节、0xC0/0xC1 与 0xF5..=0xFF
            // 都不是合法首字节，直接产生一个 U+FFFD。
            match byte {
                0x00..=0x7f => out.push(char::from(byte)),
                0xc2..=0xdf => {
                    self.bytes_needed = 1;
                    self.code_point = u32::from(byte & 0x1f);
                }
                0xe0..=0xef => {
                    // E0 收窄下界以拒绝过长编码；ED 收窄上界以拒绝代理项。
                    if byte == 0xe0 {
                        self.lower = 0xa0;
                    }
                    if byte == 0xed {
                        self.upper = 0x9f;
                    }
                    self.bytes_needed = 2;
                    self.code_point = u32::from(byte & 0x0f);
                }
                0xf0..=0xf4 => {
                    // F0 拒绝过长编码；F4 的上界保证码点不超过 U+10FFFF。
                    if byte == 0xf0 {
                        self.lower = 0x90;
                    }
                    if byte == 0xf4 {
                        self.upper = 0x8f;
                    }
                    self.bytes_needed = 3;
                    self.code_point = u32::from(byte & 0x07);
                }
                _ => out.push(REPLACEMENT),
            }
            return;
        }
        if byte < self.lower || byte > self.upper {
            // 非法续字节：先冲一个 U+FFFD，再把该字节放回队首重新处理。
            // 它可能本身是新的首字节（例如 ASCII），所以递归重入；递归至多一层。
            self.reset();
            out.push(REPLACEMENT);
            self.feed(byte, out);
            return;
        }
        // 首字节设下的收窄范围只约束第一个续字节，之后恢复普通续字节范围。
        self.lower = 0x80;
        self.upper = 0xbf;
        self.code_point = (self.code_point << 6) | u32::from(byte & 0x3f);
        self.bytes_seen += 1;
        if self.bytes_seen != self.bytes_needed {
            return;
        }
        let code_point = self.code_point;
        self.reset();
        // 上面的范围检查已经排除了代理项与越界码点，`unwrap_or` 只是兜底。
        out.push(char::from_u32(code_point).unwrap_or(REPLACEMENT));
    }
}

/// 一块已解码文本及其预先算好的字节/换行计数；`accept` 的淘汰判断与 `snapshot` 的行数
/// 计算都用它们，避免重复扫描文本。
#[derive(Debug, Clone)]
struct StoredChunk {
    text: String,
    bytes: usize,
    newlines: usize,
}

/// 一条流（如工具调用或进程）的有界运行输出（TS: `OutputBuffer`）。
///
/// 内部按块保存解码后的文本与预先算好的计数，另用 `total_bytes` / `total_newlines` 记录
/// 整条流（包括已经从存储里丢掉的块和 skip 省略的部分），所以丢弃计数是精确的。
///
/// 两种保留策略的存储行为不同：
/// - head：窗口装满后置 `full`，之后只计数、不再存储（`stops_storing_head_output_once_the_window_is_full`）。
/// - tail：每次 `accept` 从头部丢块，保证剩余部分仍比窗口多出定位行首所需的余量；
///   `snapshot` 再把它压缩成一个后缀块。
///
/// 空流的行数是 0，靠的是 `ends_with_newline` 的初值 `true`。
#[derive(Debug)]
pub struct OutputBuffer {
    /// 保留限制，构造后不变。
    limits: OutputLimits,
    /// 跨块保留未完成的 UTF-8 序列。
    decoder: Utf8StreamDecoder,
    /// 是否已有内容或 skip 进入；决定 BOM 是否还可能在流的最开头被剥掉。
    started: bool,
    /// head 保存流开头；tail 保存仍包含下一窗口的后缀。
    chunks: VecDeque<StoredChunk>,
    /// 已存块的字节总和（不含已丢弃的块与 skip 省略的部分）。
    stored_bytes: usize,
    /// 已存块的换行总和。
    stored_newlines: usize,
    /// 仅 head 使用：窗口已满，后续块只计数不再存储。
    full: bool,
    /// 整条流的字节数（解码后、消毒前，含 skip）。
    total_bytes: usize,
    /// 整条流的换行数。
    total_newlines: usize,
    /// 整条流（或最后一次 skip）是否以换行结尾；初值 `true` 让空流的行数为 0。
    ends_with_newline: bool,
}

impl OutputBuffer {
    /// 按限制新建缓冲。
    pub fn new(limits: OutputLimits) -> Self {
        // `ends_with_newline = true` 不是笔误：它让空流的行数为 `lines(0, true) = 0`。
        OutputBuffer {
            limits,
            decoder: Utf8StreamDecoder::new(),
            started: false,
            chunks: VecDeque::new(),
            stored_bytes: 0,
            stored_newlines: 0,
            full: false,
            total_bytes: 0,
            total_newlines: 0,
            ends_with_newline: true,
        }
    }

    /// 当前持有的字节数；上界为限制加一个块。
    ///
    /// 这是存储量而不是整条流的量：不含已经丢弃的块与 skip 省略的字节。head 满后不再增长；
    /// tail 在每次 [`OutputBuffer::snapshot`] 压缩后会降到窗口后缀附近。
    pub fn stored_bytes(&self) -> usize {
        self.stored_bytes
    }

    /// 限制。
    pub fn limits(&self) -> &OutputLimits {
        &self.limits
    }

    /// 接受一块输出，返回是否接受了内容。`skipped` 是紧挨在该块之前被省略的输出，
    /// 必须比 tail 窗口多至少一字节或一行；只有 tail 保留接受它。
    ///
    /// 与 TS 一样，解码副作用先于 tail 检查：head 保留传入 `skipped` 时，块已被解码，
    /// 然后返回 [`OutputError::TAIL_RETENTION`]。
    ///
    /// 返回值：不带 `skipped` 时是内部计数是否增加（空块返回 `false`）；带 `skipped` 时
    /// 恒为 `true`，即使该块文本为空。
    ///
    /// 错误：只有“head 保留 + `Some(skipped)`”一种。因为此时解码器已经推进/冲掉，
    /// 缓冲的解码状态已经变化，所以不能把这次调用当作“原子失败、可原样重试”。
    ///
    /// `skipped.bytes == 0` 时 `skip` 直接返回：不增加计数，也不清空已存块。
    pub fn push(
        &mut self,
        chunk: OutputChunk<'_>,
        skipped: Option<OutputSkip>,
    ) -> Result<bool, OutputError> {
        // 字符串块或 skip 会结束之前字节块留下的不完整字符（冲成 U+FFFD）。
        // 单独的字节块则继续使用同一个解码状态，半字符留到后续块。
        let pending = match (chunk, skipped) {
            (OutputChunk::Text(_), _) => self.decoder.flush(),
            (OutputChunk::Bytes(_), Some(_)) => self.decoder.flush(),
            (OutputChunk::Bytes(_), None) => String::new(),
        };
        let mut text = match chunk {
            OutputChunk::Text(text) => text.to_owned(),
            OutputChunk::Bytes(bytes) => self.decoder.decode_stream(bytes),
        };
        // 只有流的最开始、且此前没有任何内容或 skip 时，BOM 才可能被剥掉。
        let first = !self.started && pending.is_empty() && skipped.is_none();
        if !pending.is_empty() || !text.is_empty() || skipped.is_some() {
            self.started = true;
        }
        if first && matches!(chunk, OutputChunk::Bytes(_)) && text.starts_with('\u{feff}') {
            // 只丢流最开头的 BOM；字符串块开头的 U+FEFF 保留。
            text = text[3..].to_owned();
        }
        if let Some(skipped) = skipped {
            if self.limits.retain != Retain::Tail {
                return Err(OutputError::tail_retention());
            }
            // 先计数 pending，再让 skip 清空已存块（pending 属于被省略部分之前的内容，
            // 不再是窗口需要的），最后计数并保留 skip 之后的这块文本。
            self.accept(&pending);
            self.skip(skipped);
            self.accept(&text);
            return Ok(true);
        }
        Ok(self.accept(&(pending + &text)))
    }

    /// 接受字符串块（不带 skip）。
    ///
    /// 等价于 `push(OutputChunk::Text(text), None)`；无 skip 路径不可能失败，内部的
    /// `panic!` 只是把这种不可能性写出来。
    pub fn push_text(&mut self, text: &str) -> bool {
        self.push(OutputChunk::Text(text), None)
            .unwrap_or_else(|error| panic!("push without skip cannot fail: {error}"))
    }

    /// 接受字节块（不带 skip）。
    ///
    /// 等价于 `push(OutputChunk::Bytes(bytes), None)`；字节可在任意位置切分，不完整的字符
    /// 留到后续块，或在 [`OutputBuffer::end`] 时变成 U+FFFD。
    pub fn push_bytes(&mut self, bytes: &[u8]) -> bool {
        self.push(OutputChunk::Bytes(bytes), None)
            .unwrap_or_else(|error| panic!("push without skip cannot fail: {error}"))
    }

    /// 把尾部不完整的字符冲成替换字符；流结束时调用。
    ///
    /// 只冲解码器，不“关闭”缓冲：之后仍可继续 push；没有残留时是空操作。
    pub fn end(&mut self) {
        let text = self.decoder.flush();
        self.accept(&text);
    }

    /// 计数被省略的输出；在它之后的文本到达后，之前的存储不可能还在窗口里。
    fn skip(&mut self, skipped: OutputSkip) {
        if skipped.bytes == 0 {
            // 零字节 skip 不表示省略任何内容：计数与已存块都保持不变。
            return;
        }
        self.total_bytes += skipped.bytes;
        self.total_newlines += skipped.newlines;
        self.ends_with_newline = skipped.ends_with_newline;
        self.chunks.clear();
        self.stored_bytes = 0;
        self.stored_newlines = 0;
    }

    // 存储并计数一块非空文本；返回是否计入了内容（空块返回 false）。
    fn accept(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let bytes = utf8_byte_length(text);
        let newlines = count_newlines(text);
        // 整条流的计数先更新，且与是否存储无关；dropped 的精确性全靠这里。
        self.total_bytes += bytes;
        self.total_newlines += newlines;
        self.ends_with_newline = text.ends_with('\n');
        if self.full {
            return true;
        }
        self.chunks.push_back(StoredChunk {
            text: text.to_owned(),
            bytes,
            newlines,
        });
        self.stored_bytes += bytes;
        self.stored_newlines += newlines;
        if self.limits.retain == Retain::Head {
            // 完整窗口之后的内容永远用不到。
            // `full` 是单向的：置位后只累计计数，不再检查或存储新块。
            self.full = self.stored_bytes > self.limits.max_bytes
                || self.stored_newlines >= self.limits.max_lines;
            return true;
        }
        // 只要剩余部分还多于一个窗口（字节数超过 maxBytes、换行数超过 maxLines，各多留 1），
        // 就从头丢块，这样之后仍能找到窗口的行首。每块只丢一次。
        while self.chunks.len() > 1 {
            let first_bytes = self.chunks[0].bytes;
            let first_newlines = self.chunks[0].newlines;
            let bytes_after = self.stored_bytes - first_bytes;
            let newlines_after = self.stored_newlines - first_newlines;
            if bytes_after <= self.limits.max_bytes + 1
                && newlines_after <= self.limits.max_lines + 1
            {
                break;
            }
            self.chunks.pop_front();
            self.stored_bytes = bytes_after;
            self.stored_newlines = newlines_after;
        }
        true
    }

    /// 保留的、已消毒的输出，以及限制从整个流里丢弃了什么。
    ///
    /// 会修改内部状态（所以是 `&mut self`）：为了不让存储随流无限增长，tail 只保留“比窗口
    /// 多一字节或一行”的最短后缀，head 把多块合并成一块。压缩必须对后续结果透明——
    /// `tests/output_skip.rs` 会在随机时刻插入快照，再与从不压缩的缓冲对比最终结果。
    ///
    /// 计算顺序：
    /// 1. 拼接已存块（单块时直接克隆）；
    /// 2. 用 [`bound_output`] 取整行窗口，得到 `kept`；
    /// 3. 用已存块的 `stored_newlines` 换算已存文本的行数，再减去 `kept.dropped_lines`；
    /// 4. 按需压缩存储；
    /// 5. 对窗口文本做 [`sanitize_output`]，用整条流计数减 `kept.bytes` / 保留行数得到
    ///    dropped。
    pub fn snapshot(&mut self) -> BoundedOutput {
        // 单块时避免一次拼接；多块时按流顺序（`VecDeque` 从旧到新）拼接。
        let stored: String = if self.chunks.len() == 1 {
            self.chunks[0].text.clone()
        } else {
            self.chunks
                .iter()
                .map(|chunk| chunk.text.as_str())
                .collect()
        };
        let kept = bound_output(&stored, &self.limits);
        // 已存文本的行数：以存的换行数为准，末尾没有换行的半行算一行；空文本 0 行。
        let stored_lines = lines(
            self.stored_newlines,
            stored.is_empty() || stored.ends_with('\n'),
        );
        // 窗口里实际保留的行数 = 已存行数 - 窗口丢掉的行数。
        let kept_lines = stored_lines - kept.dropped_lines;
        // tail 窗口不会越过当前窗口，但定位之后窗口的行首需要它前面的内容：
        // 保留比窗口多一字节或一行的最短后缀，与 `accept` 一致。
        // head 且已存多块时不需要裁剪，只是把块合并成一个（`bytes` 用已有的总计）。
        if self.limits.retain == Retain::Tail || self.chunks.len() > 1 {
            let text = if self.limits.retain == Retain::Tail {
                tail_margin(&stored, &self.limits)
            } else {
                stored
            };
            let bytes = if self.limits.retain == Retain::Tail {
                utf8_byte_length(&text)
            } else {
                self.stored_bytes
            };
            self.chunks = if text.is_empty() {
                VecDeque::new()
            } else {
                let mut chunks = VecDeque::new();
                chunks.push_back(StoredChunk {
                    bytes,
                    newlines: count_newlines(&text),
                    text,
                });
                chunks
            };
            self.stored_bytes = bytes;
            self.stored_newlines = self.chunks.front().map(|chunk| chunk.newlines).unwrap_or(0);
        }
        // dropped 以整条流为基准：总字节/总行数减去窗口保留的字节/行数。
        // `text` 是消毒后的，但 dropped 用的是消毒前的 `kept.bytes`。
        BoundedOutput {
            text: sanitize_output(&kept.text),
            dropped_bytes: self.total_bytes - kept.bytes,
            dropped_lines: lines(self.total_newlines, self.ends_with_newline) - kept_lines,
        }
    }
}

/// `text` 的最短后缀，其字节数超过 `maxBytes` 或换行数超过 `maxLines`（否则整段）。
///
/// 任何以该后缀结尾的文本，后面再接任何内容，其 tail 窗口都与 `text` 后接同样的内容相同。
fn tail_margin(text: &str, limits: &OutputLimits) -> String {
    let bytes = text.as_bytes();
    // 比窗口多一个字节的最短后缀起点：从 `len - maxBytes - 1` 向前退到字符边界，
    // 这样后缀长度至少是 maxBytes + 1。
    let byte_start = if bytes.len() > limits.max_bytes {
        character_end(bytes, bytes.len() - limits.max_bytes - 1)
    } else {
        0
    };
    // 从尾部数换行；第 maxLines + 1 个换行处切，保证后缀里的换行数严格大于 maxLines。
    let mut line_start = 0usize;
    let mut newlines = 0usize;
    let mut index = rfind_newline(bytes, bytes.len());
    while let Some(at) = index {
        newlines += 1;
        if newlines > limits.max_lines {
            line_start = at;
            break;
        }
        if at == 0 {
            break;
        }
        index = rfind_newline(bytes, at);
    }
    // 取两者中更靠后的起点（后缀更短）：只要满足“字节多一”或“行多一”之一即可，
    // 这样既够用又不会多留内容。
    let start = byte_start.max(line_start);
    if start == 0 {
        text.to_owned()
    } else {
        String::from_utf8_lossy(&bytes[start..]).into_owned()
    }
}

/// 每个进度提交还会按其写入体积买一段暂停（TS: `PROGRESS_BYTES_PER_SECOND`）。
///
/// 单位是字节/秒（100 KiB/s）。提交完成后计算
/// `max(min_interval_ms, bytes * 1000 / PROGRESS_BYTES_PER_SECOND)`，再以调度侧记录的
/// `started` 为基准得到下一次提交的最早时刻 `next_at`；完成时刻不是计时基准，写入耗时
/// 不会在完成后再额外加一段。
pub const PROGRESS_BYTES_PER_SECOND: f64 = (100 * 1024) as f64;

/// 进度提交所依据的时钟与定时器（TS: `ProgressClock`）。
///
/// 实现约定：
/// - `now` 的单位是毫秒，只需在同一实现的多次调用之间可比（用于计算时间差）；
/// - `set_timer` **不得在返回前同步调用 `callback`**：`Progress` 在持有内部
///   `std::sync::Mutex` 时调用它，同步回调会重入同一把不可重入的锁而死锁；
/// - 返回的解除函数应能安全调用：`stop` 取到定时器后就会调用它，不检查回调是否已经触发，
///   因此解除可能与回调并发；
/// - 定时器可以提前触发，`Progress` 会重新读时钟并按剩余时间重新安排。
pub trait ProgressClock: Send + Sync {
    /// 当前时间（毫秒；TS `Date.now()`）。
    fn now(&self) -> f64;

    /// 大约 `delay_ms` 后调用一次 `callback`；返回解除定时器的函数。
    ///
    /// 注意参数顺序与 TS 的 `setTimer(callback, delayMs)` 相反：Rust 版是
    /// `(delay_ms, callback)`。
    fn set_timer(
        &self,
        delay_ms: f64,
        callback: Box<dyn FnOnce() + Send>,
    ) -> Box<dyn FnOnce() + Send>;
}

/// 系统时钟：`Date.now` 与 `setTimeout`/`clearTimeout` 的 tokio 等价物。
///
/// `now` 返回 `SystemTime` 相对 `UNIX_EPOCH` 的毫秒数；系统时间早于 epoch 这种极端情况下
/// 返回 `0.0`（`unwrap_or(0.0)`）。
///
/// `set_timer` 直接调用 `tokio::spawn`，因此调用线程必须处于 Tokio 运行时上下文——否则
/// `tokio::spawn` 会 panic（本文件 `spawn_detached` 的注释给出了同一依据）。`delay_ms <= 0`
/// 时也是先 spawn、在任务里立即回调，而不是同步调用；返回的解除函数调用 `abort`。
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl ProgressClock for SystemClock {
    fn now(&self) -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs_f64() * 1000.0)
            .unwrap_or(0.0)
    }

    fn set_timer(
        &self,
        delay_ms: f64,
        callback: Box<dyn FnOnce() + Send>,
    ) -> Box<dyn FnOnce() + Send> {
        let handle = tokio::spawn(async move {
            // 延迟为 0 或负数时也先进入任务，保证不在 `set_timer` 返回前同步回调。
            if delay_ms > 0.0 {
                tokio::time::sleep(Duration::from_secs_f64(delay_ms / 1000.0)).await;
            }
            callback();
        });
        // `abort` 对已完成的任务是空操作，所以解除函数可以无条件调用。
        Box::new(move || handle.abort())
    }
}

/// 启动一个分离任务：有 Tokio 运行时就用它，否则在专用线程上用一个单线程运行时驱动。
///
/// TS 的 promise 不要求调用方处在任何运行时里；`Progress::mark` 必须保持"安排提交并
/// 立即返回"，所以没有运行时时既不能用 `tokio::spawn`（会 panic，见 `Handle::try_current`），
/// 也不能阻塞调用线程（会把 `mark` 变成同步等待）。注入的 [`ProgressClock`]（假定时器
/// 测试）正是这种同步调用场景。
///
/// 线程路径会新建一个单线程运行时并阻塞到 future 完成；线程创建失败被 `let _ =` 静默忽略，
/// 此时 future 不会运行。`mark` 只承诺“已安排”，不承诺执行时机或执行线程。
fn spawn_detached(future: impl Future<Output = ()> + Send + 'static) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(future);
        return;
    }
    let _ = std::thread::Builder::new()
        .name("chord-progress".to_owned())
        .spawn(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(future),
                // 建运行时都失败时退回纯 future 执行器（不依赖 Tokio 资源的写入仍可完成）。
                Err(_) => futures::executor::block_on(future),
            }
        });
}

/// 提交失败：与 TS 的 `unknown` 一样不透明，但可克隆以便广播给所有 waiter。
///
/// `Arc` 让同一个错误同时交给这一批的所有 waiter 与错误回调；没有分类或退出码，调用方
/// 通常只能展示它的消息。
pub type ProgressFailure = Arc<dyn std::error::Error + Send + Sync>;

/// waiter 在提交结算前被丢弃（`stop` 后调用方没有结算它）。
///
/// 只在 waiter 的发送端被丢弃、通道关闭时产生（[`ProgressWait`] 的 `Err(_)` 分支）；
/// 它不是写入失败，也没有关联的 [`ProgressFailure`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressAbandoned;

impl std::fmt::Display for ProgressAbandoned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("progress was stopped before the commit settled")
    }
}

impl std::error::Error for ProgressAbandoned {}

/// 写入函数：返回本次提交写入的字节数。
///
/// 每次提交调用一次；返回的字节数用来按 100 KiB/s 推迟下一次提交。闭包在后台任务里执行，
/// 因此需要 `Send + Sync + 'static`，不能借用调用方的局部状态。
pub type ProgressWrite = Arc<
    dyn Fn() -> Pin<Box<dyn Future<Output = Result<usize, ProgressFailure>> + Send>> + Send + Sync,
>;

/// 错误回调。
///
/// 写入失败时调用一次；位置在该批 waiter 结算之后、`in_flight` 清除之前，且不持有内部锁。
pub type ProgressOnError = Arc<dyn Fn(ProgressFailure) + Send + Sync>;

/// `mark_and_wait()` 的 future。
///
/// 由 [`Progress::mark_and_wait`] 返回。通道关闭（发送端被丢弃，例如 `stop` 之后调用方没有
/// 结算 waiter）时得到 [`ProgressAbandoned`]；否则原样传递那次提交的结果。
pub struct ProgressWait {
    receiver: oneshot::Receiver<Result<(), ProgressFailure>>,
}

impl Future for ProgressWait {
    type Output = Result<(), ProgressFailure>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.receiver).poll(context) {
            Poll::Ready(Ok(result)) => Poll::Ready(result),
            // 发送端被丢弃 = 没有任何提交结算这个 waiter。
            Poll::Ready(Err(_)) => Poll::Ready(Err(Arc::new(ProgressAbandoned) as ProgressFailure)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl ProgressWait {
    /// 等待结算。
    pub async fn wait(self) -> Result<(), ProgressFailure> {
        self.await
    }
}

/// `Progress` 的内部状态；所有读写都经过 `Progress::inner` 的互斥锁。
struct ProgressState {
    /// 写入函数；每批提交调用一次。
    write: ProgressWrite,
    /// 失败回调。
    on_error: ProgressOnError,
    /// 两次提交之间的最小间隔（毫秒）。
    min_interval_ms: f64,
    /// 计时与定时器。
    clock: Arc<dyn ProgressClock>,
    /// 等待某一批提交结算的发送端；`flush` 开始时整体取走。
    waiters: Vec<oneshot::Sender<Result<(), ProgressFailure>>>,
    /// 解除当前定时器的函数。
    timer: Option<Box<dyn FnOnce() + Send>>,
    /// 在飞提交的完成信号。
    in_flight: Option<oneshot::Receiver<()>>,
    /// 下一次提交最早发生的时钟时刻（毫秒）；初值 0 让第一次 `mark` 立即提交。
    next_at: f64,
    /// 有变化还没被任何提交覆盖。
    dirty: bool,
    /// `stop` 之后为 true，此后不再安排提交。
    stopped: bool,
}

/// 自适应进度提交：空闲期后的第一次变化立即提交；之后每次提交按最小间隔与写入体积
/// （100 KiB/s）推迟下一次。同一时刻最多一个提交在飞；期间的变化合并进下一次。
/// 在 `clock` 上计时，默认是 `Date.now` 与 `setTimeout`（TS: `Progress`）。
///
/// 调度状态（`ProgressState`）：
/// - `dirty`：有变化还没被任何提交覆盖；
/// - `timer`：已安排的定时器及解除函数，存在时不再重复安排；
/// - `in_flight`：当前提交的完成信号，存在时不会开始第二次提交；
/// - `next_at`：下一次提交最早发生的时刻（时钟毫秒），初值 0 让第一次 `mark` 立即提交；
/// - `stopped`：`stop` 之后为 true，此后的 `mark` 不再安排提交。
///
/// 写入 future 正常结束（返回 `Ok` 或 `Err`）后都会清 `in_flight`；若期间又置了 `dirty`，
/// 会重新走 `schedule`，未停止时尾巴上的变化不会丢。后台任务持有状态的 `Arc`（不是
/// `Weak`），所以即使 `Progress` 在提交途中被丢弃，waiter 仍由该提交结算。
pub struct Progress {
    inner: Arc<Mutex<ProgressState>>,
}

impl std::fmt::Debug for Progress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Progress").finish_non_exhaustive()
    }
}

impl Progress {
    /// 用系统时钟构造。
    ///
    /// `write` 每次提交调用一次，返回写入的字节数；`on_error` 在失败时调用一次；
    /// `min_interval_ms` 是两次提交之间的最小间隔（毫秒），首次提交不受它限制（`next_at`
    /// 初值为 0）。
    pub fn new<W, E>(write: W, on_error: E, min_interval_ms: f64) -> Self
    where
        W: Fn() -> Pin<Box<dyn Future<Output = Result<usize, ProgressFailure>> + Send>>
            + Send
            + Sync
            + 'static,
        E: Fn(ProgressFailure) + Send + Sync + 'static,
    {
        Self::with_clock(write, on_error, min_interval_ms, Arc::new(SystemClock))
    }

    /// 用给定时钟构造。
    ///
    /// 测试用假时钟驱动同一套调度逻辑（`tests/output.rs` 的 `TestClock`）；实现必须遵守
    /// [`ProgressClock`] 的约定，尤其 `set_timer` 不能在返回前同步回调。
    pub fn with_clock<W, E>(
        write: W,
        on_error: E,
        min_interval_ms: f64,
        clock: Arc<dyn ProgressClock>,
    ) -> Self
    where
        W: Fn() -> Pin<Box<dyn Future<Output = Result<usize, ProgressFailure>> + Send>>
            + Send
            + Sync
            + 'static,
        E: Fn(ProgressFailure) + Send + Sync + 'static,
    {
        Progress {
            inner: Arc::new(Mutex::new(ProgressState {
                write: Arc::new(write),
                on_error: Arc::new(on_error),
                min_interval_ms,
                clock,
                waiters: Vec::new(),
                timer: None,
                in_flight: None,
                next_at: 0.0,
                dirty: false,
                stopped: false,
            })),
        }
    }

    /// 安排一次提交。
    ///
    /// 只置 `dirty` 并尝试安排，不阻塞：若已有定时器或在飞提交则立即返回，这次变化合并进
    /// 下一次提交。
    pub fn mark(&self) {
        {
            let mut state = self.inner.lock().expect("progress state");
            state.dirty = true;
        }
        Self::schedule(&self.inner);
    }

    /// 安排一次提交；返回的 future 在包含本次变化的提交结算时完成。
    ///
    /// 先登记 waiter 再调用 [`Progress::mark`]，因此只要还没 `stop`，这次变化就会被下一次
    /// 提交覆盖并结算该 waiter。若 [`Progress::stop`] 在提交发生前接管，`stop` 会取走当时
    /// 已登记的 waiter 交调用方结算；在 `stop` 取走之后才登记的 waiter 不在其中。发送端被
    /// 丢弃时，等待方得到 [`ProgressAbandoned`]。
    pub fn mark_and_wait(&self) -> ProgressWait {
        let (sender, receiver) = oneshot::channel();
        {
            let mut state = self.inner.lock().expect("progress state");
            state.waiters.push(sender);
        }
        self.mark();
        ProgressWait { receiver }
    }

    /// 停止提交并等待在飞提交；返回最终提交必须结算的 waiter。
    ///
    /// 顺序：置 `stopped` → 解除定时器 → 等待在飞提交结束 → 取走剩余 waiter。已经由在飞
    /// 提交结算的 waiter 不在返回值里；返回的是还没有任何提交覆盖的 waiter。
    ///
    /// 调用方必须为每个返回的 waiter `send` 一个结果；直接丢弃会让对应的 [`ProgressWait`]
    /// 以 [`ProgressAbandoned`] 结束。`stop` 之后再 `mark`/`mark_and_wait` 不会安排提交
    /// （`schedule` 见到 `stopped` 直接返回）；之后登记的 waiter 不会被提交结算，要么由
    /// 下一次 `stop` 返回给调用方，要么等到 `Progress` 被丢弃、通道关闭时以
    /// [`ProgressAbandoned`] 结束。
    pub async fn stop(&self) -> Vec<oneshot::Sender<Result<(), ProgressFailure>>> {
        let in_flight = {
            let mut state = self.inner.lock().expect("progress state");
            state.stopped = true;
            let timer = state.timer.take();
            let in_flight = state.in_flight.take();
            drop(state);
            // 解除函数可能在锁外做任意事（SystemClock 会 abort 任务），所以不持锁调用。
            if let Some(disarm) = timer {
                disarm();
            }
            in_flight
        };
        if let Some(receiver) = in_flight {
            // 在飞提交自己的 waiter 已由它结算；等它结束只为确保之后取到的都是未覆盖的。
            let _ = receiver.await;
        }
        let mut state = self.inner.lock().expect("progress state");
        // 剩下的 waiter 没有被任何提交覆盖，交给调用方结算。
        std::mem::take(&mut state.waiters)
    }

    // 尝试开始一次提交或安排定时器；本函数自己加锁，调用方不得持有 state 的锁。
    fn schedule(inner: &Arc<Mutex<ProgressState>>) {
        let mut state = inner.lock().expect("progress state");
        // 已停止、已有定时器或有提交在飞时什么都不做：变化已经记在 `dirty` 里。
        if state.stopped || state.timer.is_some() || state.in_flight.is_some() {
            return;
        }
        // 距离最早允许的提交还有多久；<= 0 表示现在就可以。
        let wait = state.next_at - state.clock.now();
        if wait <= 0.0 {
            Self::flush(inner, state);
            return;
        }
        let callback: Box<dyn FnOnce() + Send> = {
            let inner = Arc::clone(inner);
            Box::new(move || {
                // 定时器回调：先清 `timer`，再重新 schedule。提前触发时这里会算出剩余时间，
                // 再安排一个更短的定时器。
                {
                    let mut state = inner.lock().expect("progress state");
                    state.timer = None;
                }
                Self::schedule(&inner);
            })
        };
        let clock = Arc::clone(&state.clock);
        // `set_timer` 不得在返回前同步调用回调（与 TS 的 `setTimeout` 一致）。
        // 这里仍持有 `state` 的锁：同步回调会重入这把 `std::sync::Mutex` 而死锁。
        state.timer = Some(clock.set_timer(wait, callback));
    }

    // 开始一次提交：取走这一批 waiter、记录起点、登记 `in_flight`，然后在后台任务里执行
    // 写入。调用时持有 `state` 的锁；锁在 spawn 之前释放。
    fn flush(
        inner: &Arc<Mutex<ProgressState>>,
        mut state: std::sync::MutexGuard<'_, ProgressState>,
    ) {
        if state.stopped || !state.dirty {
            return;
        }
        // 这一批之后的变化会重新置 `dirty`，合并进下一次提交。
        state.dirty = false;
        // 本批 waiter 随提交一起走；之后注册的 waiter 留在 `state` 里。
        let waiters = std::mem::take(&mut state.waiters);
        // “开始时间”在调度侧取，而不是后台任务真正开始运行的时间。
        let started = state.clock.now();
        let (done_sender, done_receiver) = oneshot::channel();
        state.in_flight = Some(done_receiver);
        let write = Arc::clone(&state.write);
        let on_error = Arc::clone(&state.on_error);
        let min_interval_ms = state.min_interval_ms;
        // 任务持有强引用而不是 `Weak`：TS 的 promise 链会在 `Progress` 被丢弃后继续
        // 持有对象，在飞提交结算时照常结算 waiter。用 `Weak` 会让被丢弃的 Progress
        // 的 waiter 以 [`ProgressAbandoned`] 失败，与 TS 不同。
        let inner = Arc::clone(inner);
        drop(state);
        spawn_detached(async move {
            let result = write().await;
            match result {
                Ok(bytes) => {
                    {
                        let mut state = inner.lock().expect("progress state");
                        // 成功：下次提交至少等 min_interval_ms，并按写入体积（100 KiB/s）
                        // 再推迟。
                        state.next_at = started
                            + min_interval_ms
                                .max(bytes as f64 * 1000.0 / PROGRESS_BYTES_PER_SECOND);
                    }
                    for waiter in waiters {
                        let _ = waiter.send(Ok(()));
                    }
                }
                Err(error) => {
                    {
                        let mut state = inner.lock().expect("progress state");
                        // 失败：退化为只等最小间隔，不按（未知的）写入量推迟。
                        state.next_at = started + min_interval_ms;
                    }
                    // 同一个错误克隆给每个 waiter，然后交给错误回调（在锁外调用）。
                    for waiter in waiters {
                        let _ = waiter.send(Err(Arc::clone(&error)));
                    }
                    on_error(error);
                }
            }
            let dirty = {
                let mut state = inner.lock().expect("progress state");
                // 先清 in_flight，下一次 schedule 才可能开始新提交。
                state.in_flight = None;
                state.dirty
            };
            let _ = done_sender.send(());
            // 提交期间又有变化：立刻尝试下一批。
            if dirty {
                Self::schedule(&inner);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_tabs_and_newlines() {
        assert_eq!(
            sanitize_output("a\0b\tc\nd\re\u{7}f\u{fff9}g\u{fffb}h😀"),
            "ab\tc\ndefgh😀"
        );
    }

    #[test]
    fn bounds_whole_lines() {
        let head = OutputLimits::head(1000, 2);
        assert_eq!(
            bound_output("a\nb\nc\n", &head),
            OutputSlice {
                text: "a\nb\n".into(),
                bytes: 4,
                dropped_bytes: 2,
                dropped_lines: 1
            }
        );
        let tail = OutputLimits::tail(1000, 2);
        assert_eq!(
            bound_output("a\nb\nc\n", &tail),
            OutputSlice {
                text: "b\nc\n".into(),
                bytes: 4,
                dropped_bytes: 2,
                dropped_lines: 1
            }
        );
    }

    #[test]
    fn cuts_on_character_boundaries() {
        assert_eq!(bound_output("ééé\n", &OutputLimits::head(5, 10)).text, "éé");
        assert_eq!(
            bound_output("x\néééé", &OutputLimits::tail(5, 10)).text,
            "éé"
        );
        assert_eq!(character_end("ééé\n".as_bytes(), 5), 4);
    }

    #[test]
    fn buffer_keeps_tail_window() {
        let mut buffer = OutputBuffer::new(OutputLimits::tail(1000, 2));
        buffer.push_text("a\nb\n");
        buffer.push_bytes(&[0xf0, 0x9f]);
        buffer.push_bytes(&[0x98, 0x80, 0x0a]);
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
    fn buffer_strips_only_the_leading_byte_order_mark() {
        let mut buffer = OutputBuffer::new(OutputLimits::tail(1000, 10));
        buffer.push_bytes(&[0xef]);
        buffer.push_bytes(&[0xbb, 0xbf, 0x61]);
        buffer.push_text("b");
        buffer.push_bytes(&[0xef, 0xbb, 0xbf, 0x63]);
        buffer.end();
        assert_eq!(buffer.snapshot().text, "ab\u{feff}c");
    }

    #[test]
    fn buffer_flushes_incomplete_characters() {
        let mut buffer = OutputBuffer::new(OutputLimits::tail(1000, 10));
        buffer.push_bytes(&[0xe2]);
        buffer.push_text("x");
        buffer.push_bytes(&[0xe2, 0x82]);
        buffer.end();
        assert_eq!(buffer.snapshot().text, "\u{fffd}x\u{fffd}");
    }

    #[test]
    fn head_retention_refuses_skips() {
        let mut buffer = OutputBuffer::new(OutputLimits::head(10, 2));
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

    #[test]
    fn reports_utf8_byte_length() {
        assert_eq!(utf8_byte_length("abc"), 3);
        assert_eq!(utf8_byte_length("é"), 2);
        assert_eq!(utf8_byte_length("😀"), 4);
    }
}
