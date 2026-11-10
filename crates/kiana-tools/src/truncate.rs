//! core/tools/truncate.ts。
//!
//! 工具输出的共享截断工具：两个独立上限（行数默认 2000、字节默认 50 KiB）先命中者胜。
//! 头部截断**从不返回半行**（首行就超字节上限时返回空内容并置
//! [`TruncationResult::first_line_exceeds_limit`]）；尾部截断允许把末行从行尾截一半
//! （`last_line_partial`）。
//!
//! # 跨 crate 契约
//!
//! [`TruncationResult`] 与 [`TruncatedBy`] **定义在** [`kiana_ext_api::tool_types`]：
//! `BashToolDetails.truncation` 等会进会话文件，`kiana-tools`（L8）依赖 `kiana-ext-api`（L7），
//! 因此这里只再导出（`ARCHITECTURE.md` §3.5 的「一份定义」规则），字段顺序即
//! `JSON.stringify(details)` 的键顺序。
//!
//! # JS 语义
//!
//! - `formatSize` 的 `toFixed(1)` 走 [`kiana_ai::model_catalog::js_to_fixed`]（JS 的中点远离零，
//!   Rust `{:.1}` 是取偶：`256B -> "0.3KB"` 而不是 `"0.2KB"`）。
//! - [`truncate_line`] 的 `maxChars` 与 `slice` 按 **UTF-16 code unit** 计；切在代理对中间时
//!   JS 产生孤立代理，这里用 `String::from_utf16_lossy` 输出 U+FFFD（已记录的偏差）。
//!   残余差异只有 JSON 序列化：JS 的 `JSON.stringify` 把孤立代理写成 `\ud83d` 转义，
//!   而 U+FFFD 按原文输出；UTF-8 字节数（3）与 UTF-16 长度（1）两者一致。
//! - [`truncate_middle`] 的 `removedChars` 按 **code point** 计（`Array.from(str).length`）。
//!
//! # 调用方与选型
//!
//! - 文件类工具（`read`/`find`/`ls`/`grep`）保留头部，用 [`truncate_head`]；
//! - bash 执行器与流式输出累积器保留尾部（错误与最终结果在末尾），用 [`truncate_tail`]；
//! - `grep` 的每条匹配行用 [`truncate_line`]；MCP 等展示型输出用 [`truncate_middle`]
//!   （该函数在本 crate 内没有调用点，调用方在 `kiana-builtin-extensions`）；
//! - [`format_size`] 用于 `(50.0KB limit)` 之类的截断说明文案。
//!
//! 本模块没有 IO、锁或全局状态：所有函数都是纯计算，输入借用，返回拥有所有权的结构。
//! [`TruncationOptions`] 的 `None` 字段在**每次调用时**解析为默认上限，不在构造时固化。
//!
//! # 边界
//!
//! 两个上限都使用「先命中者胜」；`max_lines = 0` 或 `max_bytes = 0` 是合法输入（随机差分
//! fixture 覆盖），此时头/尾截断分别返回空内容。行数统计不受末尾换行影响，但字节统计
//! 使用原始 UTF-8 字节数。

use kiana_ai::model_catalog::js_to_fixed;
pub use kiana_ext_api::tool_types::{TruncatedBy, TruncationResult};

/// `DEFAULT_MAX_LINES`。
pub const DEFAULT_MAX_LINES: usize = 2000;
/// `DEFAULT_MAX_BYTES`（50 KiB = 51200）。
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
/// `GREP_MAX_LINE_LENGTH`：每个 grep 匹配行的最大字符数（UTF-16 code unit；`... [truncated]`
/// 后缀不计入这个上限）。
pub const GREP_MAX_LINE_LENGTH: usize = 500;

/// `TruncationOptions`：缺省字段用 [`DEFAULT_MAX_LINES`] / [`DEFAULT_MAX_BYTES`]。
///
/// `None` 表示「调用时用默认值」，不是 0；`Default::default()` 的两个字段都是 `None`。
/// 因此 `TruncationOptions::default()` 与显式写死的 `2000/51200` 当前等价，但默认值改变时
/// 只有前者跟随。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct TruncationOptions {
    /// 最大行数。
    pub max_lines: Option<usize>,
    /// 最大字节数。
    pub max_bytes: Option<usize>,
}

impl TruncationOptions {
    /// 空选项（即两个默认上限）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置行数上限。
    pub fn with_max_lines(mut self, max_lines: usize) -> Self {
        self.max_lines = Some(max_lines);
        self
    }

    /// 设置字节上限。
    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = Some(max_bytes);
        self
    }

    /// 解析成具体的 `(max_lines, max_bytes)`：`None` 字段分别取 [`DEFAULT_MAX_LINES`] 与
    /// [`DEFAULT_MAX_BYTES`]。每次调用都重新解析，选项在两次调用之间改变不会缓存旧值。
    fn resolve(&self) -> (usize, usize) {
        (
            self.max_lines.unwrap_or(DEFAULT_MAX_LINES),
            self.max_bytes.unwrap_or(DEFAULT_MAX_BYTES),
        )
    }
}

/// JS 字符串的 UTF-16 code unit 数（`str.length`）。
///
/// TS: `truncate.ts#truncateLine` 的 `line.length`（同样的语义在 `render-utils`、
/// `bash-executor` 的滚动缓冲里使用）。
pub(crate) fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

/// 按 UTF-16 code unit 截取 `value[start..end]`（JS 的 `slice`，参数非负且已 clamp）。
///
/// 切点落在代理对中间时 JS 产生孤立代理（无法用 Rust `String` 表示），这里输出 U+FFFD。
/// TS: `truncate.ts#truncateLine` 的 `line.slice(0, maxChars)`。
///
/// `start`/`end` 会先 clamp 到码元总数；`end < start` 时按 `start` 处理（结果为空串）。
/// 实现会先把整个输入收集成 `Vec<u16>`；本模块对它的调用都发生在单行文本上
/// （[`truncate_line`] 的输入、`render_utils` 的折叠参数预览）。
pub(crate) fn utf16_slice(value: &str, start: usize, end: usize) -> String {
    let units: Vec<u16> = value.encode_utf16().collect();
    let start = start.min(units.len());
    let end = end.min(units.len()).max(start);
    String::from_utf16_lossy(&units[start..end])
}

/// `splitLinesForCounting(content)`：按 `"\n"` 切行；结尾换行不额外算一行；空串 0 行。
///
/// TS: `splitLinesForCounting`。`""` -> 0 行，`"a\n"` -> `["a"]`，`"a\n\n"` -> `["a", ""]`。
fn split_lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// 把字节数格式化为人类可读大小。
///
/// TS: `formatSize(bytes)`。分支顺序固定：`< 1024` 裸字节，`< 1 MiB` 用 KB，否则 MB；
/// `toFixed(1)` 采用 JS 的中点远离零语义（见模块文档）。
///
/// 示例：`1023 -> "1023B"`、`1536 -> "1.5KB"`、`1_048_575 -> "1024.0KB"`（仍在 KB 分支，
/// 不会因四舍五入成 1024.0 而提前进入 MB）、`1_048_576 -> "1.0MB"`。输入是非负字节数，
/// 因此没有 NaN/负数分支。
pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{}KB", js_to_fixed(bytes as f64 / 1024.0, 1))
    } else {
        format!("{}MB", js_to_fixed(bytes as f64 / (1024.0 * 1024.0), 1))
    }
}

/// 构造「无需截断」的结果：`truncated: false`、`truncated_by: None`，输出统计等于全文统计。
///
/// 调用方已确认 `total_lines <= max_lines && total_bytes <= max_bytes`；这里只负责把
/// 已解析的上限原样写进结果，避免两处重复的字段拼装。
fn unchecked_result(
    content: String,
    total_lines: usize,
    total_bytes: usize,
    max_lines: usize,
    max_bytes: usize,
) -> TruncationResult {
    TruncationResult {
        content,
        truncated: false,
        truncated_by: None,
        total_lines: total_lines as u64,
        total_bytes: total_bytes as u64,
        output_lines: total_lines as u64,
        output_bytes: total_bytes as u64,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines: max_lines as u64,
        max_bytes: max_bytes as u64,
    }
}

/// 从头部截断（保留前 N 行/字节），适合文件读取。
///
/// TS: `truncateHead(content, options = {})`。从不返回半行；首行本身超过字节上限时返回
/// 空内容并置 `first_line_exceeds_limit`。
///
/// 步骤：
/// 1. 行数与字节数都未超上限 → 原样返回（`truncated: false`）；
/// 2. 首行（不含换行）的字节数超过 `max_bytes` → 空内容 + `first_line_exceeds_limit`；
/// 3. 顺序收集完整行，累积字节时**除首行外每行加 1 个换行**，放不下就停止；
/// 4. 收集满 `max_lines` 且未超字节上限时 `truncated_by = "lines"`，否则为 `"bytes"`。
///
/// `output_bytes` 是 `join("\n")` 之后重新计算的字节数；累加值同样只为非首行加 1 个换行，
/// 所以在本实现里两者相等，而 `truncated_by` 的判定使用的是累加值。`last_line_partial`
/// 在该分支恒为 false（只会截断整行）。
pub fn truncate_head(content: &str, options: &TruncationOptions) -> TruncationResult {
    let (max_lines, max_bytes) = options.resolve();

    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return unchecked_result(
            content.to_string(),
            total_lines,
            total_bytes,
            max_lines,
            max_bytes,
        );
    }

    // 首行单独就超过字节上限：不返回任何内容。
    let first_line_bytes = lines.first().map_or(0, |line| line.len());
    if first_line_bytes > max_bytes {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some(TruncatedBy::Bytes),
            total_lines: total_lines as u64,
            total_bytes: total_bytes as u64,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines: max_lines as u64,
            max_bytes: max_bytes as u64,
        };
    }

    let mut output_lines: Vec<&str> = Vec::new();
    let mut output_bytes_count = 0usize;
    let mut truncated_by = TruncatedBy::Lines;

    for (index, line) in lines.iter().enumerate() {
        if index >= max_lines {
            break;
        }
        // 第 0 行没有换行；其余行各占一个换行字节。
        let line_bytes = line.len() + usize::from(index > 0);
        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        output_lines.push(line);
        output_bytes_count += line_bytes;
    }

    // 因行数上限退出（且没有触发字节上限）。
    if output_lines.len() >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let output_content = output_lines.join("\n");
    let final_output_bytes = output_content.len();

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines: total_lines as u64,
        total_bytes: total_bytes as u64,
        output_lines: output_lines.len() as u64,
        output_bytes: final_output_bytes as u64,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines: max_lines as u64,
        max_bytes: max_bytes as u64,
    }
}

/// 从尾部截断（保留最后 N 行/字节），适合 bash 输出（错误与最终结果在末尾）。
///
/// TS: `truncateTail(content, options = {})`。最后一行本身超过字节上限时从行尾截取一部分
/// 并置 `last_line_partial`。
///
/// 从末行往前收集完整行（用 `insert(0, ..)` 保持原顺序），累积字节时只对本行之前已有更新
/// 行的情况加 1 个换行。若连一行都放不下且最后一行本身超预算，则从行尾按字符边界截取
/// `max_bytes` 字节（结果可能略短），`output_lines` 记为 1。行数上限的判定与
/// [`truncate_head`] 相同；`output_bytes` 同样按 `join("\n")` 重算。
pub fn truncate_tail(content: &str, options: &TruncationOptions) -> TruncationResult {
    let (max_lines, max_bytes) = options.resolve();

    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return unchecked_result(
            content.to_string(),
            total_lines,
            total_bytes,
            max_lines,
            max_bytes,
        );
    }

    // 从末尾往前收集完整行。
    let mut output_lines: Vec<String> = Vec::new();
    let mut output_bytes_count = 0usize;
    let mut truncated_by = TruncatedBy::Lines;
    let mut last_line_partial = false;

    for line in lines.iter().rev() {
        if output_lines.len() >= max_lines {
            break;
        }
        let line_bytes = line.len() + usize::from(!output_lines.is_empty());
        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            // 边界情况：还没有加入任何行且该行本身就超限——取行尾的一部分。
            if output_lines.is_empty() {
                let truncated_line = truncate_string_to_bytes_from_end(line, max_bytes);
                output_bytes_count = truncated_line.len();
                output_lines.insert(0, truncated_line);
                last_line_partial = true;
            }
            break;
        }
        output_lines.insert(0, (*line).to_string());
        output_bytes_count += line_bytes;
    }

    // 因行数上限退出。
    if output_lines.len() >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let output_content = output_lines.join("\n");
    let final_output_bytes = output_content.len();

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines: total_lines as u64,
        total_bytes: total_bytes as u64,
        output_lines: output_lines.len() as u64,
        output_bytes: final_output_bytes as u64,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines: max_lines as u64,
        max_bytes: max_bytes as u64,
    }
}

/// 把字符串按 UTF-8 字符边界截到 `max_bytes` 字节以内（从尾部保留）。
///
/// TS: `truncateStringToBytesFromEnd(str, maxBytes)`（内部函数）。起点先取
/// `len - max_bytes`，再向后跳过 UTF-8 续字节（`10xxxxxx`），保证不切断多字节字符；
/// 因此返回值可能少于 `max_bytes` 字节。`max_bytes = 0` 时返回空串。
fn truncate_string_to_bytes_from_end(value: &str, max_bytes: usize) -> String {
    let buffer = value.as_bytes();
    if buffer.len() <= max_bytes {
        return value.to_string();
    }

    let mut start = buffer.len() - max_bytes;
    // 跳过续字节，落在字符起始处。
    while start < buffer.len() && (buffer[start] & 0xc0) == 0x80 {
        start += 1;
    }
    String::from_utf8_lossy(&buffer[start..]).into_owned()
}

/// [`truncate_line`] 的返回值：`{ text, wasTruncated }`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncatedLine {
    /// 截断后的文本（超限时带 `... [truncated]` 后缀）。
    pub text: String,
    /// 是否发生了截断。
    pub was_truncated: bool,
}

/// 把单行截到 `max_chars` 个字符，超出时追加 `... [truncated]`。
///
/// TS: `truncateLine(line, maxChars = GREP_MAX_LINE_LENGTH)`。字符数按 UTF-16 code unit 计。
/// 后缀本身不计入 `max_chars`，所以结果可能比上限长；切点落在代理对中间时按模块文档的
/// 约定输出 U+FFFD。
pub fn truncate_line(line: &str, max_chars: usize) -> TruncatedLine {
    if utf16_len(line) <= max_chars {
        return TruncatedLine {
            text: line.to_string(),
            was_truncated: false,
        };
    }
    TruncatedLine {
        text: format!("{}... [truncated]", utf16_slice(line, 0, max_chars)),
        was_truncated: true,
    }
}

/// `truncateMiddle` 的返回值。
///
/// TS: `MiddleTruncationResult`。字段顺序即 JSON 键顺序。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiddleTruncationResult {
    /// 开头 + `…N chars truncated…` 标记 + 结尾。
    pub content: String,
    /// 是否发生截断。
    pub truncated: bool,
    /// 被省略的字符数（按 code point 计，不是字节、不是 UTF-16 码元）。
    pub removed_chars: u64,
    /// 原始内容的字节数。
    pub total_bytes: u64,
    /// 原始内容的行数。
    pub total_lines: u64,
}

/// 保留开头与结尾各一半，把中间替换为 `…N chars truncated…` 标记（与 Codex 一致）。
///
/// TS: `truncateMiddle(content, maxBytes)`。只在字符边界切；`removedChars` 是 code point 数。
/// 分隔符是 U+2026，前后无空格；数字与 " chars truncated" 之间一个空格。
///
/// 切点计算：`head_end = max_bytes / 2` 向前退到字符起始；`tail_start =
/// len - (max_bytes - max_bytes / 2)` 向后进到字符起始。head 与 tail 的字节数之和不超过
/// `max_bytes`（因边界回退可能更少），但**标记本身不计入上限**，所以返回的 `content`
/// 字节数可能超过 `max_bytes`。`removed_chars` 统计 head 与 tail 之间的原文（同样按字符
/// 边界切分，解码不会丢字符）。`max_bytes = 0` 时 head/tail 均为空，只剩标记。
pub fn truncate_middle(content: &str, max_bytes: usize) -> MiddleTruncationResult {
    let buffer = content.as_bytes();
    let total_lines = split_lines_for_counting(content).len();
    if buffer.len() <= max_bytes {
        return MiddleTruncationResult {
            content: content.to_string(),
            truncated: false,
            removed_chars: 0,
            total_bytes: buffer.len() as u64,
            total_lines: total_lines as u64,
        };
    }

    // 续字节（10xxxxxx）不是字符起始。
    let is_boundary = |index: usize| index >= buffer.len() || (buffer[index] & 0xc0) != 0x80;

    let mut head_end = max_bytes / 2;
    while head_end > 0 && !is_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = buffer.len() - (max_bytes - max_bytes / 2);
    while tail_start < buffer.len() && !is_boundary(tail_start) {
        tail_start += 1;
    }
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let tail = String::from_utf8_lossy(&buffer[tail_start..]).into_owned();
    let removed_chars = String::from_utf8_lossy(&buffer[head_end..tail_start])
        .chars()
        .count();

    MiddleTruncationResult {
        content: format!("{head}\u{2026}{removed_chars} chars truncated\u{2026}{tail}"),
        truncated: true,
        removed_chars: removed_chars as u64,
        total_bytes: buffer.len() as u64,
        total_lines: total_lines as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_lines_counts_like_js() {
        assert_eq!(split_lines_for_counting(""), Vec::<&str>::new());
        assert_eq!(split_lines_for_counting("a\n"), vec!["a"]);
        assert_eq!(split_lines_for_counting("a\n\n"), vec!["a", ""]);
        assert_eq!(split_lines_for_counting("\n"), vec![""]);
    }

    #[test]
    fn truncate_line_slices_utf16_units() {
        let line = "a".repeat(600);
        let truncated = truncate_line(&line, GREP_MAX_LINE_LENGTH);
        assert!(truncated.was_truncated);
        assert_eq!(
            truncated.text,
            format!("{}... [truncated]", "a".repeat(500))
        );

        // emoji 占两个 UTF-16 code unit。
        let emoji = "\u{1F600}".repeat(300);
        let truncated = truncate_line(&emoji, 500);
        assert!(truncated.was_truncated);
        // 500 个码元 = 250 个完整 emoji，没有孤立代理。
        assert_eq!(
            truncated.text,
            format!("{}... [truncated]", "\u{1F600}".repeat(250))
        );
    }

    #[test]
    fn truncate_line_replaces_a_split_surrogate_with_u_fffd() {
        // 499 个 'a' + emoji = 501 个 UTF-16 码元；切在第 500 个码元会落在代理对中间。
        // TS `line.slice(0, 500)` 保留孤立的高代理（无法用 Rust `String` 表示），这里输出
        // U+FFFD：UTF-8 字节数与 UTF-16 长度都和 JS 把孤立代理编码后的结果一致。
        let line = format!("{}{}", "a".repeat(499), "\u{1F600}");
        let truncated = truncate_line(&line, 500);
        assert!(truncated.was_truncated);
        assert_eq!(
            truncated.text,
            format!("{}\u{FFFD}... [truncated]", "a".repeat(499))
        );
        assert_eq!(utf16_len(&truncated.text), 500 + "... [truncated]".len());
    }
}
