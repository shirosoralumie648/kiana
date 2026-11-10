//! `scan.rs`：Durable `LineScanner`（`env/line-scan.ts`）的移植——按顺序喂入文件内容，定位
//! `[start_line, end_line)` 行并按 `new TextDecoder().decode(file)` 的视图测量解码后的 UTF-8 字节数。
//!
//! TS 源：packages/env/daemon/src/scan.rs（原 daemon 本身就是 Rust；逐字移植）。
//! Port of Durable's `LineScanner` (`env/line-scan.ts`): locates lines `[start_line, end_line)` of a file fed in order
//! and measures their decoded UTF-8 size as `new TextDecoder().decode(file)` would produce it.
//!
//! # 职责与边界
//! `scanLines` 操作的基础：客户端持有远端文件句柄时，要在不解码整个文件的前提下定位第 `startLine` 行
//! 到第 `endLine` 行的原始字节范围，并算出选中文本按 UTF-8 解码后的字节数。本模块以流式方式完成：
//! 任意大小的数据块按顺序 `push` 进来，结果在 `finish` 时一次性返回。
//!
//! # 在系统中的位置
//! - `main::scan_lines` 负责参数校验（给了 `endLine` 时必须大于 `startLine`）与分块读取
//!   （`SCAN_CHUNK` = 64 KiB，每块之间检查取消），把文件从头到尾喂给 `LineScanner`，最后调用 `finish`；
//! - 解码规则来自 `decode` 模块：两个测量解码器都用 `range_decoder`（不处理 BOM）；只有文件最开头
//!   3 个字节可能是 BOM，由 `head`/`feed` 按绝对偏移跳过；
//! - 返回的 JSON 就是协议里 `scanLines` 的 `LineScan`：`newlines`、`start`、`end`、`firstLineEnd`、
//!   `lastLineStart`、`selectedBytes`、`firstLineBytes`。
//!
//! # 主要流程
//! 1. 文件前 3 个字节先缓冲在 `head`，凑满（或 `finish`）后判断是否为 UTF-8 BOM
//!    （byte-order mark，字节序标记；UTF-8 下是 `EF BB BF`，整文件解码时会被丢弃）；
//! 2. 逐字节扫描换行符 `\n`，维护绝对字节偏移 `position`、当前行号 `newlines` 与行首偏移 `line_start`；
//! 3. 每遇到换行：先把该行内容喂给已开启的测量解码器，再按需结束"第一行"或"选区"，最后喂换行本身
//!    （选区中间的换行计入，最后一行末尾的换行不计入）；
//! 4. `finish` 补上"没有结尾换行的最后一行"、刷新解码器的残留字节，返回 JSON。
//!
//! # 关键约束与注意
//! - 只把 `\n`（0x0A）当换行：`\r\n` 中的 `\r` 留在行内容里，也计入 `selectedBytes`。
//! - `start`/`end`/`firstLineEnd`/`lastLineStart` 是**原始字节**偏移；`selectedBytes`/`firstLineBytes`
//!   是**解码后文本的 UTF-8 字节数**（`firstLineBytes` 不含首行结尾换行，`selectedBytes` 不含选区
//!   末尾换行）。非法序列会被替换为 U+FFFD（编码后 3 字节），所以解码字节数可能大于原始跨度
//!   （测试中的非法两字节序列即如此）。
//! - 选区是半开区间 `[start, end)`：`end` 指向选中最后一行结尾换行的位置，换行本身不计入选区。
//! - 文件行数不足 `start_line` 时，`finish` 返回 `start == end == <文件大小>` 且计数为 0。
//! - 本结构没有内部锁，且按顺序消费：必须先 `push` 完所有块再 `finish`（`finish` 按值消费 `self`）。
//! - 扫描器本身不返回 `Result`：读取错误、取消与参数校验都在 `main::scan_lines`；中途放弃时直接丢弃
//!   `LineScanner` 即可，它不持有文件句柄等外部资源。
//!
//! # 阅读顺序
//! `LineScanner::new` → `push`（BOM 缓冲）→ `process`（逐行切分）→ `finish`（收尾与输出 JSON）。

use crate::decode::{decode_into, range_decoder, starts_with_bom};
use encoding_rs::Decoder;
use serde_json::{Value, json};

/// 行分隔符：只认 0x0A；`\r` 属于行内容。
const NEWLINE: u8 = 0x0a;

/// 流式行扫描器：按顺序接收文件字节，最后给出行范围与解码字节数。
///
/// 一次只用于一个文件、一个范围，`finish` 之后不可复用。字段分组：
/// - 输入条件：`start_line`、`end_line`（开区间上界，`None` 表示直到文件末尾）；
/// - 原始字节定位：`position` 是已接收的总字节数（即下一块的起始绝对偏移），`newlines` 是已见换行数，
///   `line_start` 是当前行首的绝对偏移；
/// - 输出偏移：`start` 选区首字节、`end` 选区结尾（不含最后换行）、`first_line_end` 首行换行位置、
///   `last_line_start` 末行行首；
/// - 解码计数：`selected_bytes`/`first_line_bytes` 为解码后文本的字节数；
/// - 解码器与被延迟处理的前 3 字节：`selection`、`first_line`、`head`、`bom`。
pub struct LineScanner {
    start_line: u64,
    end_line: Option<u64>,
    position: u64,
    newlines: u64,
    line_start: u64,
    start: Option<u64>,
    end: Option<u64>,
    first_line_end: Option<u64>,
    last_line_start: Option<u64>,
    selected_bytes: u64,
    first_line_bytes: u64,
    selection: Option<Decoder>,
    first_line: Option<Decoder>,
    head: Option<Vec<u8>>,
    bom: bool,
}

impl LineScanner {
    /// `end_line`, when given, must be greater than `start_line`.
    ///
    /// 创建时若 `start_line == 0` 立即 `begin(0)`：第 0 行从文件偏移 0 开始，解码器需要从第一个字节
    /// 起累计；`head` 预留 3 字节用于判断 BOM。
    pub fn new(start_line: u64, end_line: Option<u64>) -> LineScanner {
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
            head: Some(Vec::with_capacity(3)),
            bom: false,
        };
        if start_line == 0 {
            scanner.begin(0);
        }
        scanner
    }

    // `end_line` 是开区间上界：最后一行行号是 `end - 1`，所以 `line + 1 == end` 表示当前行是选区末行。
    fn is_last_line(&self, line: u64) -> bool {
        self.end_line.is_some_and(|end| line + 1 == end)
    }

    /// 追加一块文件内容。块边界任意，可以短于 3 字节；前 3 个字节先缓冲起来判断 BOM。
    ///
    /// 若连同已有缓冲仍不足 3 字节，直接返回：数据留在 `head` 里，等下一块或 `finish`。
    /// 凑满 3 字节后，`release_head` 先处理这 3 字节，再处理本块的剩余部分。
    pub fn push(&mut self, mut chunk: &[u8]) {
        if let Some(head) = &mut self.head {
            let take = (3 - head.len()).min(chunk.len());
            head.extend_from_slice(&chunk[..take]);
            if head.len() < 3 {
                return;
            }
            chunk = &chunk[take..];
            self.release_head();
        }
        self.process(chunk);
    }

    // 处理被缓冲的前 3 字节：判断是否为 BOM 后交给 `process`。`head` 置空表示之后的内容不再需要
    // 特殊处理；`bom` 标志供 `feed` 跳过这 3 个字节。
    fn release_head(&mut self) {
        let head = self.head.take().unwrap_or_default();
        self.bom = starts_with_bom(&head);
        self.process(&head);
    }

    // 逐行扫描一块数据。`base` 是本块第一个字节的绝对偏移，`index`/`from` 是块内下标。
    //
    // 对每个位于 `newline` 的换行：
    // 1. 把"上一段行内容"（`from..newline`）喂给仍开启的解码器；
    // 2. 该行是 `start_line` 时，在换行之前结束"第一行"测量；
    // 3. 该行是选区末行时，在换行之前结束选区测量——因此末尾换行不计入选区；
    // 4. 喂换行字节本身：它只会被仍开启的解码器累计（选区中间的换行）；若上一行是首行或选区末行，
    //    对应解码器已在上一步关闭，换行不会计入；
    // 5. 行号 +1、更新行首；行的编号到达 `start_line` 时开启测量，到达末行时记录 `last_line_start`。
    // 循环结束后把本块剩余尾部（可能没有换行）喂给解码器，并推进总偏移 `position`。
    fn process(&mut self, chunk: &[u8]) {
        let base = self.position;
        let mut from = 0;
        let mut index = 0;
        while let Some(found) = chunk[index..].iter().position(|byte| *byte == NEWLINE) {
            let newline = index + found;
            // The newline ends line `self.newlines`; it belongs to the selection between selected lines only.
            self.feed(chunk, base, from, newline);
            let line = self.newlines;
            let position = base + newline as u64;
            if line == self.start_line {
                self.end_first_line(position);
            }
            if self.is_last_line(line) {
                self.end_selection(position);
            }
            self.feed(chunk, base, newline, newline + 1);
            from = newline + 1;
            self.newlines += 1;
            self.line_start = position + 1;
            if self.newlines == self.start_line {
                self.begin(self.line_start);
            }
            if self.is_last_line(self.newlines) {
                self.last_line_start = Some(self.line_start);
            }
            index = newline + 1;
        }
        self.feed(chunk, base, from, chunk.len());
        self.position += chunk.len() as u64;
    }

    /// 结束扫描并返回 `LineScan` JSON；按值消费，之后不能继续 `push`。
    ///
    /// 收尾工作：文件不足 3 字节时释放 `head`；若首行或选区还没有结束（文件在换行前就结束），
    /// 用 `position` 补上结尾（解码器以 `last = true` 刷新，未完成的字符变成 U+FFFD）。
    /// `start_line` 超出文件行数时返回全为文件大小的偏移与 0 计数。各字段含义见模块文档。
    pub fn finish(mut self) -> Value {
        // 文件短于 3 字节：缓冲从未释放，这里仍要判断 BOM 并处理全部内容。
        if self.head.is_some() {
            self.release_head();
        }
        let size = self.position;
        // `start_line` 超出文件行数：没有选区，所有偏移都退化为文件大小。
        let Some(start) = self.start else {
            return json!({
                "newlines": self.newlines, "start": size, "end": size, "firstLineEnd": size,
                "lastLineStart": size, "selectedBytes": 0, "firstLineBytes": 0,
            });
        };
        // 文件没有以换行结尾：首行延伸到 EOF。
        if self.first_line_end.is_none() {
            self.end_first_line(size);
        }
        // 选区一直延伸到 EOF。
        if self.end.is_none() {
            self.end_selection(size);
        }
        json!({
            "newlines": self.newlines,
            "start": start,
            "end": self.end.unwrap_or(size),
            "firstLineEnd": self.first_line_end.unwrap_or(size),
            // A selection that reaches past the last line ends with the last line.
            "lastLineStart": self.last_line_start.unwrap_or(self.line_start),
            "selectedBytes": self.selected_bytes,
            "firstLineBytes": self.first_line_bytes,
        })
    }

    // 开始测量从 `start` 起的选区与第一行：两个解码器都从行首重新开始。这里用不带 BOM 处理的
    // 解码器：BOM 已由 `feed` 按绝对偏移跳过，且行首不会落在多字节字符中间（0x0A 不可能是 UTF-8
    // 多字节序列的组成字节），所以从行首重新解码与整文件解码在该范围上的结果一致。
    fn begin(&mut self, start: u64) {
        self.start = Some(start);
        if self.is_last_line(self.start_line) {
            self.last_line_start = Some(start);
        }
        self.selection = Some(range_decoder());
        self.first_line = Some(range_decoder());
    }

    // 第一行在换行处结束：记录原始偏移，并以 `last = true` 刷新解码器，把悬挂的半个字符
    // 变成 U+FFFD 后计入 `first_line_bytes`。
    fn end_first_line(&mut self, position: u64) {
        self.first_line_end = Some(position);
        if let Some(mut decoder) = self.first_line.take() {
            self.first_line_bytes += decode_into(&mut decoder, &[], true).len() as u64;
        }
    }

    // 选区在末行换行处结束：记录原始偏移（不含换行），并刷新解码器的残留字节。
    fn end_selection(&mut self, position: u64) {
        self.end = Some(position);
        if let Some(mut decoder) = self.selection.take() {
            self.selected_bytes += decode_into(&mut decoder, &[], true).len() as u64;
        }
    }

    // 把块内 `[from, to)` 喂给仍开启的解码器，并累计解码后的字节数。
    //
    // BOM 跳过：文件以 BOM 开头时，绝对偏移落在前 3 字节内的部分整体丢弃（`from` 推到 3 之后），
    // 与整文件解码时 BOM 被移除一致。BOM 能跨块正确处理，靠的是 `head` 缓冲而不是这里的偏移判断：
    // `push` 先收集前 3 字节，未凑满时既不调用本函数也不推进 `position`；凑满后 `release_head`
    // 在绝对偏移 0 处一次性处理这 3 字节，此后 `position` 已越过 BOM。因此 `base + from < 3`
    // 只会在处理那段 3 字节缓冲时成立，作用仅是丢弃其中的 BOM 前缀。
    fn feed(&mut self, chunk: &[u8], base: u64, mut from: usize, to: usize) {
        // Decoding the whole file drops a leading byte-order mark.
        if self.bom && base + (from as u64) < 3 {
            from = to.min((3 - base) as usize);
        }
        if to <= from {
            return;
        }
        let bytes = &chunk[from..to];
        if let Some(decoder) = &mut self.selection {
            self.selected_bytes += decode_into(decoder, bytes, false).len() as u64;
        }
        if let Some(decoder) = &mut self.first_line {
            self.first_line_bytes += decode_into(decoder, bytes, false).len() as u64;
        }
    }
}

// 测试策略：用一段同时包含 BOM、非法序列、空行、文件中部的 U+FEFF 和结尾无换行的字节序列，
// 以 1..=5 的各种分块大小运行，锁定"流式结果与整文件解码一致"；另覆盖选区边界、
// BOM 丢弃、行数不足等情形。
#[cfg(test)]
mod tests {
    use super::*;

    fn scan(file: &[u8], start: u64, end: Option<u64>, chunk: usize) -> Value {
        let mut scanner = LineScanner::new(start, end);
        for piece in file.chunks(chunk.max(1)) {
            scanner.push(piece);
        }
        scanner.finish()
    }

    #[test]
    fn locates_and_measures_lines_like_whole_file_decoding() {
        // BOM, "a", invalid sequence, empty line, a later U+FEFF, and no final newline.
        let file = [
            0xef, 0xbb, 0xbf, 0x61, 0x0a, 0xe2, 0x82, 0x0a, 0x0a, 0xef, 0xbb, 0xbf, 0x62, 0x0a,
            0xc3, 0xa9,
        ];
        for chunk in 1..6 {
            let all = scan(&file, 0, None, chunk);
            assert_eq!(all["newlines"], 4);
            // "a\n\u{fffd}\n\n\u{feff}b\né" is 1+1+3+1+1+3+1+1+2 = 14 bytes.
            assert_eq!(all["selectedBytes"], 14);
            assert_eq!(all["firstLineBytes"], 1);
            let middle = scan(&file, 1, Some(3), chunk);
            assert_eq!(
                (middle["start"].as_u64(), middle["end"].as_u64()),
                (Some(5), Some(8))
            );
            assert_eq!(middle["selectedBytes"], 4);
            assert_eq!(middle["lastLineStart"], 8);
            let past = scan(&file, 9, None, chunk);
            assert_eq!(past["start"], 16);
        }
    }
}
