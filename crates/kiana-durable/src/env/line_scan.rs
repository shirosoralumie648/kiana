//! `env/line-scan.ts`：按顺序喂入文件字节，在有限内存里算出 [`LineScan`]。
//!
//! 状态机逐字移植自 TS，包括：
//! - 头部 3 字节缓冲：先判断整份文件是否以 UTF-8 BOM 开头，再决定解码区间要不要跳过它；
//! - `selected` 与 `firstLine` 两个独立解码器（`rangeDecoder()`，`ignoreBOM: true`）；
//! - 计行只看 `0x0a`，`0x0d 0x0a` 里的 `0x0d` 是行内容；
//! - 解码字节数用 UTF-8 字节长度（`TextEncoder` 的语义；U+FFFD 记 3 字节）。
//!
//! # 为什么需要
//! `scanLines` 要在不把整份文件读进内存、不保留解码结果的前提下，给出选区
//! `[startLine, endLine)` 的原始字节偏移，以及这些行按整文件 `TextDecoder` 视图解码后的
//! UTF-8 字节数（`tools/read.rs` 用它决定截断与诊断文本）。解码按选区流式进行，但选区可能
//! 覆盖整份文件（`startLine = 0`、`endLine = None`），此时除开头的 BOM 外，每个 `push` 的
//! 字节都要交给选区的增量解码器。扫描器因此只保留最多 3 字节的 BOM 判定缓冲、两个增量
//! 解码器和若干计数器；它不保留文件内容，内存占用与文件大小无关。
//!
//! # 调用方
//! - `env/node.rs` 的 `BinaryReader::scan_lines`：以 64 KiB 块（`TEXT_CHUNK`）从头到尾 `push`，
//!   读到 EOF 后 `finish`；`LineScanner::new` 报错时映射为 `FileErrorCode::Invalid`。
//! - `tools/read.rs`：用 `LineScan` 的 `start`/`end` 再读取原始字节区间，用 `first_line_bytes`
//!   生成“首行超限”的诊断。
//! - `testing/env_conformance.rs` 与 `tests/env_line_scan.rs`：把选区与“整份解码后按 `\n`
//!   切分再取行”对比。
//!
//! # 主要流程
//! 1. [`LineScanner::new`] 校验行区间；`startLine == 0` 时立即开启两个解码器；
//! 2. 前 3 个字节先缓存在 `head`，凑满（或流结束）后判断整份输入是否以 UTF-8 BOM 开头，
//!    再把这段字节交给 `process`；
//! 3. `process` 逐字节找 `0x0A`，在每个换行处按固定顺序更新边界与解码计数；
//! 4. `finish` 处理没有结尾换行的最后一行、冲刷解码器残留并产出 [`LineScan`]。
//!
//! # 不变量
//! - `selected_bytes` / `first_line_bytes` 是**解码后文本**的 UTF-8 字节长度；非法序列按
//!   U+FFFD（3 字节）计，所以可能大于原始字节跨度。
//! - 偏移字段（`start`/`end`/`first_line_end`/`last_line_start`）是**原始字节**偏移。选区左闭
//!   右开：`end` 通常指向选中最后一行结尾换行的位置；最后一行为未终止行（或选区越过最后
//!   一行）时 `end` 为输入长度。换行本身不计入 `selected_bytes`；选区内部的换行则计入。
//! - 整份输入开头的 BOM 不计入解码字节数（整文件 `TextDecoder` 会丢弃它），但 `start` 仍是
//!   原始偏移，可能指向 BOM 的第一个字节（见 `skips_a_leading_bom` 测试）。
//! - 只按 `0x0A` 划分行：`\r\n` 中的 `\r` 属于行内容，也计入解码字节数。
//! - `newlines` 统计**整份输入**的换行数，即使选区早已结束也继续计数（文件有 `newlines + 1`
//!   行）。
//!
//! # 使用约束
//! - 必须按流顺序 `push`，所有块喂完后调用 [`LineScanner::finish`]（它按值消费扫描器）。
//!   扫描器不持有文件句柄等外部资源，中途放弃直接丢弃即可。
//! - `push`/`finish` 不返回错误：参数校验只在 `new` 发生，IO 错误与取消由调用方处理。
//! - `startLine` 大于等于文件行数时（选区起点在最后一行之后），结果退化为四个偏移都等于
//!   输入长度、解码计数为 0。

use crate::env::LineScan;
use crate::env::decode::{RangeDecoder, starts_with_bom};

const NEWLINE: u8 = 0x0a;

/// `RangeError("Invalid line range")`：行区间必须是 `endLine > startLine >= 0` 的安全整数，
/// `endLine` 缺省表示到文件末尾。
///
/// 只由 [`LineScanner::new`] 返回；`Display` 与 TS 的异常消息相同，`node.rs` 收到后把它映射成
/// `FileErrorCode::Invalid` 的 `FileError`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Invalid line range")]
pub struct InvalidLineRange;

/// `Number.isSafeInteger` 的等价判断：有限、无小数部分、绝对值不超过 `2^53 - 1`。
///
/// 只有安全整数才能保证 TS 的 `number` 与 Rust 整数之间的转换不丢精度；`NaN`、`±Infinity`
/// 和小数都返回 `false`。
fn is_safe_integer(value: f64) -> bool {
    value.is_finite() && value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_991.0
}

/// 解码后文本的 UTF-8 字节长度；`str::len` 正是 `TextEncoder().encode(text).length`。
///
/// 非法序列已被解码器替换成 U+FFFD，每个占 3 字节，所以这里统计的是替换后的长度。
fn decoded_bytes(text: &str) -> usize {
    text.len()
}

/// 逐字节计算 `LineScan`（TS: `LineScanner`）。
///
/// 状态分四组：
/// - 输入条件：`start_line`、`end_line`（`None` 表示直到输入末尾）；
/// - 原始字节定位：`position`（已接收字节数，也是下一块的起始绝对偏移）、`newlines`
///   （已见换行数）、`line_start`（当前行首偏移），以及输出用的 `start`/`end`/
///   `first_line_end`/`last_line_start`；
/// - 解码计数：`selected_bytes`、`first_line_bytes`；
/// - BOM 与解码器：`head`（最多 3 字节的待判定前缀）、`bom`、`selection`、`first_line`。
///
/// `selection` 与 `first_line` 必须各自独立：两个区间都从 `startLine` 的行首开始，但首行在
/// 第一个换行处就要冲刷（`decode_end`），冲刷会结束解码流，而选区之后还要继续解码，共用
/// 解码状态无法同时满足两者。
#[derive(Debug)]
pub struct LineScanner {
    start_line: u64,
    /// `None` 表示 `endLine` 是 `Infinity`：一直到文件末尾。
    end_line: Option<u64>,
    position: usize,
    newlines: u64,
    line_start: usize,
    start: Option<usize>,
    end: Option<usize>,
    first_line_end: Option<usize>,
    last_line_start: Option<usize>,
    selected_bytes: usize,
    first_line_bytes: usize,
    selection: Option<RangeDecoder>,
    first_line: Option<RangeDecoder>,
    /// 最初的字节，先留着判断是不是 BOM。
    head: Option<Vec<u8>>,
    bom: bool,
}

impl LineScanner {
    /// `new LineScanner(startLine, endLine = Infinity)`；`None` 表示 `Infinity`。
    ///
    /// 参数用 `f64` 是为了逐个对应 TS 的 `number`：`startLine` 必须是非负安全整数，`endLine`
    /// 必须是安全整数且严格大于 `startLine`（`None` 视为 `Infinity`，不受此限）。不满足时返回
    /// [`InvalidLineRange`]，其 `Display` 正是 TS 的 `Invalid line range`。
    ///
    /// `startLine == 0` 时立即开启解码器（选区从偏移 0 开始）；否则要等扫描到对应换行才开启。
    pub fn new(start_line: f64, end_line: Option<f64>) -> Result<Self, InvalidLineRange> {
        let end_value = end_line.unwrap_or(f64::INFINITY);
        if !is_safe_integer(start_line)
            || start_line < 0.0
            || end_value.partial_cmp(&start_line) != Some(std::cmp::Ordering::Greater)
        {
            return Err(InvalidLineRange);
        }
        if end_value != f64::INFINITY && !is_safe_integer(end_value) {
            return Err(InvalidLineRange);
        }
        let start_line = start_line as u64;
        let end_line = if end_value == f64::INFINITY {
            None
        } else {
            Some(end_value as u64)
        };
        let mut scanner = LineScanner {
            start_line,
            end_line,
            position: 0,
            newlines: 0,
            line_start: 0,
            start: None,
            end: None,
            first_line_end: None,
            last_line_start: None,
            selected_bytes: 0,
            first_line_bytes: 0,
            selection: None,
            first_line: None,
            head: Some(Vec::new()),
            bom: false,
        };
        if start_line == 0 {
            scanner.begin(0);
        }
        Ok(scanner)
    }

    /// 喂入下一块字节。
    ///
    /// 块可以是任意长度（包括空块），但必须按输入顺序调用，并且要在 `finish` 之前喂完全部
    /// 字节。前 3 个字节会被缓冲到能判断 BOM 为止，调用方不需要自己对齐块边界。本方法不做
    /// IO，也不会失败。
    pub fn push(&mut self, chunk: &[u8]) {
        let mut chunk = chunk;
        if let Some(head) = self.head.as_mut() {
            // 只差最后几个字节就能凑满 3 字节前缀；不够就先留着，整块消费掉。
            let take = (3 - head.len()).min(chunk.len());
            head.extend_from_slice(&chunk[..take]);
            if head.len() < 3 {
                return;
            }
            self.release_head();
            chunk = &chunk[take..];
        }
        self.process(chunk);
    }

    /// 前缀凑满 3 字节（或流结束时不足 3 字节）后调用：判定 BOM 并把缓冲的字节交给 `process`。
    ///
    /// 不足 3 字节时 `starts_with_bom` 必为 `false`，按“没有 BOM”处理。释放时 `position`
    /// 仍为 0（此前没有处理过任何字节），所以 `process` 以偏移 0 开始。
    fn release_head(&mut self) {
        let Some(head) = self.head.take() else {
            return;
        };
        self.bom = starts_with_bom(&head);
        self.process(&head);
    }

    fn process(&mut self, chunk: &[u8]) {
        // `base` 是这一块在整份输入中的绝对起始偏移；`position` 要等整块处理完才前进，
        // 所以循环里用 `base + index` 计算绝对偏移。
        //
        // 每遇到一个换行，按固定顺序处理（顺序决定边界是否包含换行）：
        // 1. 把换行之前、本行剩下的内容喂给仍在解码的区间；
        // 2. 若本行是 `startLine`，结束首行（换行不属于首行）；
        // 3. 若本行是选区的最后一行，结束选区（这个换行不属于选区）；
        // 4. 把换行本身喂给仍在解码的区间（只有选区内部能走到这里）；
        // 5. 行号加一、更新行首，必要时开启选区或记录最后一行行首。
        let base = self.position;
        let mut from = 0usize;
        for index in 0..chunk.len() {
            // 只认 LF；`\r\n` 里的 CR 留在行内容中。
            if chunk[index] != NEWLINE {
                continue;
            }
            // 换行结束第 `newlines` 行；它只属于所选行之间的区间。
            self.feed(chunk, base, from, index);
            let line = self.newlines;
            let position = base + index;
            if line == self.start_line {
                self.end_first_line(position);
            }
            if self.end_line.is_some_and(|end| line + 1 == end) {
                self.end_selection(position);
            }
            // 换行本身：首行刚结束、选区若也结束则已关闭，所以只有选区内部的换行会被计入。
            self.feed(chunk, base, index, index + 1);
            from = index + 1;
            self.newlines += 1;
            self.line_start = position + 1;
            // 已完成 `start_line` 个换行：下一行（从 `line_start` 开始）就是选区起点。
            if self.newlines == self.start_line {
                self.begin(self.line_start);
            }
            // 下一行是选区的最后一行（`end - 1`），记录它的行首，供 `finish` 返回。
            if self.end_line.is_some_and(|end| self.newlines + 1 == end) {
                self.last_line_start = Some(self.line_start);
            }
        }
        // 末尾没有换行的剩余内容（可能是最后一行，也可能是当前行的一部分）。
        self.feed(chunk, base, from, chunk.len());
        self.position += chunk.len();
    }

    /// 流结束，返回扫描结果。
    ///
    /// 按值消费扫描器，调用后不可复用。先释放 `head` 里不足 3 字节的残余，再给仍未闭合的
    /// 首行/选区收尾（文件结尾没有换行时，它们以输入长度结束）。
    ///
    /// `startLine` 在输入里不存在时（文件行数 ≤ `startLine`），返回的四个偏移都等于输入长度、
    /// 解码计数为 0；`newlines` 仍会如实返回。
    pub fn finish(mut self) -> LineScan {
        if self.head.is_some() {
            self.release_head();
        }
        let size = self.position;
        let Some(start) = self.start else {
            return LineScan {
                newlines: self.newlines,
                start: size,
                end: size,
                first_line_end: size,
                last_line_start: size,
                selected_bytes: 0,
                first_line_bytes: 0,
            };
        };
        if self.first_line_end.is_none() {
            self.end_first_line(size);
        }
        if self.end.is_none() {
            self.end_selection(size);
        }
        LineScan {
            newlines: self.newlines,
            start,
            end: self.end.unwrap_or(size),
            first_line_end: self.first_line_end.unwrap_or(size),
            // 越过最后一行的选区以最后一行为结尾。
            // `endLine` 为 Infinity 或大于文件行数时，`process` 不会记录 `last_line_start`；
            // 此时当前行的行首（`line_start`）就是最后一行（可能是结尾换行后的空行）的起点。
            last_line_start: self.last_line_start.unwrap_or(self.line_start),
            selected_bytes: self.selected_bytes,
            first_line_bytes: self.first_line_bytes,
        }
    }

    /// 开启选区：记录 `start`，创建两个独立解码器。
    ///
    /// 单选一行（`end == start + 1`）时还要直接置 `last_line_start = start`。这条赋值是
    /// `startLine == 0` 所必需的：`begin` 在构造函数里、任何换行之前调用，该行结尾换行处
    /// `newlines` 自增后 `newlines + 1 == end` 不成立，不会再记录同一个行首。`startLine > 0`
    /// 时 `begin` 在 `newlines` 自增之后调用，同一轮循环里 `newlines + 1 == end` 分支会记录
    /// 同样的值。
    fn begin(&mut self, start: usize) {
        self.start = Some(start);
        if self.end_line.is_some_and(|end| self.start_line + 1 == end) {
            self.last_line_start = Some(start);
        }
        self.selection = Some(RangeDecoder::new());
        self.first_line = Some(RangeDecoder::new());
    }

    /// 记录首行结束位置（换行的绝对偏移）并冲刷首行解码器。
    ///
    /// `decode_end` 会把行尾残留的不完整序列替换成 U+FFFD 并计入 `first_line_bytes`。
    /// `0x0A` 不可能是 UTF-8 续字节，所以换行前的残留必定非法，这与整文件解码在同一位置
    /// 产生的替换结果一致；而换行本身还没有被喂入，因此不计入首行。
    fn end_first_line(&mut self, position: usize) {
        self.first_line_end = Some(position);
        if let Some(mut decoder) = self.first_line.take() {
            self.first_line_bytes += decoded_bytes(&decoder.decode_end());
        }
    }

    /// 记录选区结束位置（最后一行的换行偏移）并冲刷选区解码器。
    ///
    /// 在换行处调用时，它发生在喂入该换行之前，所以结束换行不计入 `selected_bytes`；选区内部
    /// 的换行在 `process` 里已作为普通字节喂给 `selection`。`finish` 里则用输入长度收尾
    /// （选区越过最后一行或最后一行为未终止行）。
    fn end_selection(&mut self, position: usize) {
        self.end = Some(position);
        if let Some(mut decoder) = self.selection.take() {
            self.selected_bytes += decoded_bytes(&decoder.decode_end());
        }
    }

    /// 把 `chunk[from, to)`（起始文件偏移 `base`）喂给仍在解码的区间。
    ///
    /// BOM 处理：整份输入解码时会丢弃开头的 `EF BB BF`。`bom` 只有在 `head` 攒满 3 字节后
    /// 才可知，而 `head` 释放时恰好从偏移 0 开始处理，所以这里按**绝对偏移**跳过前 3 字节，
    /// 与块怎么切分无关。`base + from < 3` 同时保证 `3 - base` 不会下溢。
    fn feed(&mut self, chunk: &[u8], base: usize, mut from: usize, to: usize) {
        // 解码整份文件会丢弃开头的字节序标记。
        if self.bom && base + from < 3 {
            from = to.min(3 - base);
        }
        if to <= from {
            // 空段，或整段都落在被跳过的 BOM 里。
            return;
        }
        let bytes = &chunk[from..to];
        // 两个区间各自推进：尚未开启（选区起点还没到）或已经结束的区间为 None。
        if let Some(decoder) = self.selection.as_mut() {
            self.selected_bytes += decoded_bytes(&decoder.decode_stream(bytes));
        }
        if let Some(decoder) = self.first_line.as_mut() {
            self.first_line_bytes += decoded_bytes(&decoder.decode_stream(bytes));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_all(file: &[u8], start: f64, end: Option<f64>) -> LineScan {
        let mut scanner = LineScanner::new(start, end).expect("valid range");
        for chunk in file.chunks(1) {
            scanner.push(chunk);
        }
        scanner.finish()
    }

    #[test]
    fn rejects_invalid_ranges() {
        assert!(LineScanner::new(2.0, Some(2.0)).is_err());
        assert!(LineScanner::new(-1.0, None).is_err());
        assert!(LineScanner::new(1.5, None).is_err());
        assert!(LineScanner::new(f64::NAN, None).is_err());
        assert!(LineScanner::new(0.0, Some(0.0)).is_err());
    }

    #[test]
    fn scans_simple_file() {
        let file = b"one\ntwo\nthree";
        let scan = scan_all(file, 1.0, Some(2.0));
        assert_eq!(scan.newlines, 2);
        assert_eq!(&file[scan.start..scan.end], b"two");
        assert_eq!(scan.selected_bytes, 3);
        assert_eq!(scan.first_line_bytes, 3);
        assert_eq!(scan.first_line_end, 7);
        assert_eq!(scan.last_line_start, 4);
    }

    #[test]
    fn skips_a_leading_bom() {
        let file = b"\xef\xbb\xbfone\ntwo";
        let scan = scan_all(file, 0.0, None);
        assert_eq!(scan.newlines, 1);
        assert_eq!(&file[scan.start..scan.first_line_end], b"\xef\xbb\xbfone");
        assert_eq!(scan.first_line_bytes, 3);
    }

    #[test]
    fn empty_selection_past_the_end() {
        let file = b"a\n";
        let scan = scan_all(file, 5.0, None);
        assert_eq!(scan.start, file.len());
        assert_eq!(scan.end, file.len());
        assert_eq!(scan.selected_bytes, 0);
        assert_eq!(scan.first_line_bytes, 0);
    }
}
