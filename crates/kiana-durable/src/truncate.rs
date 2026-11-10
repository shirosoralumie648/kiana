//! `truncate.ts`：工具输出的按行/按字节截断。
//!
//! 截断以两个独立上限中先触发的那个为准（默认 2000 行、50 KiB），并且**从不返回半行**：
//! 若第一行本身就超过字节上限，结果内容为空并置 `firstLineExceedsLimit`。
//!
//! 与 TS 的对应关系：
//! - `utf8ByteLength` 复用 [`kiana_chord::output::utf8_byte_length`]（TS 从 chord/output 再导出）。
//! - `formatSize` 的 `toFixed(1)` 走 [`kiana_ai::model_catalog::js_to_fixed`]（Rust 的
//!   `format!("{:.1}")` 是半数取偶，而 JS 是半数远离零，见 `format_size(1280)` 的 `1.25`）。
//! - 空串与结尾换行的计行规则：`""` 是 0 行，`"a\n"` 是 1 行，`"\n"` 是 1 行。
//!
//! # 在系统中的位置
//!
//! - `crate::tools::read` 的 read 工具用 [`truncate_head_of`] 处理“有界读取”得到的前缀，
//!   从而不必为判断截断而读完整文件（见该文件的 `read_head`）；
//! - `crate::harness::tool` 用 [`DEFAULT_MAX_LINES`] / [`DEFAULT_MAX_BYTES`] 作为工具输出
//!   缓冲区的缺省上限；
//! - 测试 `tests/env_truncate.rs` 与 `tests/tools_read_differential.rs` 锁定逐字语义。
//!
//! # 执行流程（[`truncate_head_of`]）
//!
//! 1. 解析上限：`options` 里为 `None` 的字段回落到默认常量；
//! 2. 全文没超限（行数与字节数都不超）：原样返回，`truncated == false`；
//! 3. 第一行单独超过字节上限：返回空内容并置 `first_line_exceeds_limit`；即使没有输出，
//!    `total_*` 仍报告全文统计；
//! 4. 否则逐行收集：行间换行符计 1 字节；字节数会超就停在上一行，行数到 `maxLines`
//!    也停；
//! 5. 若没有发生字节越界，再区分“确实漏掉了行”（`Lines`）还是“仅结尾换行导致字节数
//!    超限”（`Bytes`）；
//! 6. 用 `\n` 拼回内容，并按实际内容重新统计输出字节数。
//!
//! # 不变量与边界
//!
//! - 截断路径里 `content` 永远由完整行组成，不会以半行结尾，且
//!   `output_bytes == content.len()`。
//! - 未截断路径直接使用调用方给的 `totals`：`output_bytes` 是 `totals.bytes`，
//!   `output_lines` 是 `totals.lines`（[`truncate_head`] 传入的自算值保证二者等于全文）。
//! - `truncated_by` 为 `Some` 当且仅当 `truncated == true`。
//! - `last_line_partial` 在这个文件里恒为 `false`：这里只有头部截断，没有尾部截断。
//! - 两个上限都是“最多允许”的含义：等于上限不算超限。

use kiana_ai::model_catalog::js_to_fixed;
pub use kiana_chord::output::utf8_byte_length;

/// `DEFAULT_MAX_LINES`：2000 行，`TruncationOptions` 未设置行数上限时使用。
pub const DEFAULT_MAX_LINES: usize = 2000;
/// `DEFAULT_MAX_BYTES`：50 KiB（51200 字节），未设置字节上限时使用。
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;

/// 触发截断的上限（TS: `"lines" | "bytes" | null`）。
///
/// 只有“被截断时命中了哪个上限”才有值；未截断由外层 `Option` 的 `None` 表达（TS 的
/// `null`），因此这里没有“未截断”变体。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TruncatedBy {
    /// 行数上限。
    Lines,
    /// 字节上限。
    Bytes,
}

/// `TruncationResult`。
///
/// 与 TS 的 `TruncationResult` 字段一一对应；`truncated_by` 用 `Option<TruncatedBy>`
/// 表示 TS 的 `null`。`total_*` 是输入全文的统计，`output_*` 是实际返回内容的统计；
/// 在 [`truncate_head_of`] 的前缀契约成立时，截断路径上二者满足 `output <= total`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncationResult {
    /// 截断后的内容。
    ///
    /// 只包含完整行；未截断时等于输入前缀。
    pub content: String,
    /// 是否发生截断。
    pub truncated: bool,
    /// 命中的上限；未截断时是 `None`（TS 的 `null`）。
    pub truncated_by: Option<TruncatedBy>,
    /// 原始内容的总行数。
    ///
    /// 来自调用方给的 `totals`（[`truncate_head`] 时为全文统计），不是从 `content` 重算的。
    pub total_lines: usize,
    /// 原始内容的总字节数。
    ///
    /// 同样来自 `totals`；[`truncate_head_of`] 的前缀可能比全文短。
    pub total_bytes: usize,
    /// 截断输出里的完整行数。
    ///
    /// 未截断时等于 `total_lines`；截断时等于实际收集到的行数（空行也算一行，所以
    /// `content` 为空时这个字段仍可能是 1，例如只收集到一个空行）。
    pub output_lines: usize,
    /// 截断输出的字节数。
    ///
    /// 截断路径里等于 `content.len()`；未截断路径里等于 `total_bytes`。
    pub output_bytes: usize,
    /// 最后一行是否被部分截断（仅尾部截断的边界情况）。
    ///
    /// 本文件只实现头部截断，因此这个字段恒为 `false`；保留它是为了与 TS 的结果形状一致。
    pub last_line_partial: bool,
    /// 第一行是否超过字节上限（头部截断）。
    ///
    /// 为 `true` 时 `content` 为空、`output_lines == 0`、`truncated_by == Some(Bytes)`；
    /// 调用方通常改为显示行首的一段字节（read 工具就是这样做的）。
    pub first_line_exceeds_limit: bool,
    /// 实际生效的行数上限。
    pub max_lines: usize,
    /// 实际生效的字节上限。
    pub max_bytes: usize,
}

/// `TruncationOptions`：缺省字段用 [`DEFAULT_MAX_LINES`] / [`DEFAULT_MAX_BYTES`]。
///
/// 这里的 `None` 只是“未设置”；默认值在 [`truncate_head_of`] 入口才解析，所以同一个
/// options 可以在多次调用间复用，含义不随调用时刻变化（常量是编译期固定的）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TruncationOptions {
    /// 最大行数；`None` 用 [`DEFAULT_MAX_LINES`]。
    pub max_lines: Option<usize>,
    /// 最大字节数；`None` 用 [`DEFAULT_MAX_BYTES`]。
    pub max_bytes: Option<usize>,
}

impl TruncationOptions {
    /// 空选项：两个上限都未设置，调用时回落到默认常量。
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置行数上限（链式构造）。
    pub fn with_max_lines(mut self, max_lines: usize) -> Self {
        self.max_lines = Some(max_lines);
        self
    }

    /// 设置字节上限（链式构造）。
    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = Some(max_bytes);
        self
    }
}

/// 计行：`content.split("\n")`，但结尾换行不额外算一行；空串是 0 行。
///
/// 空串要特判：`"".split('\n')` 会产生一个空行，TS 的 `content.length === 0` 分支直接
/// 返回空数组。结尾换行 `pop` 掉是因为它开启的是“下一行”而不是新内容：`"a\n"` 是 1 行，
/// `"\n"` 是 1 行（一个空行）。
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

/// 把字节数格式化为人类可读大小（TS: `formatSize`）。
///
/// 分支规则：小于 1024 输出整数字节（如 `format_size(256) == "256B"`）；小于 1 MiB
/// 除以 1024 后保留一位小数并加 `KB`；否则除以 1 MiB 加 `MB`。
///
/// 小数部分走 [`kiana_ai::model_catalog::js_to_fixed`]：ECMAScript 的 `toFixed(1)` 是
/// “半数远离零”，`1.25` 得到 `"1.3"`（`format_size(1280) == "1.3KB"`）；Rust 的
/// `format!("{:.1}")` 是半数取偶，会得到 `"1.2"`，不能直接用。
pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{}KB", js_to_fixed(bytes as f64 / 1024.0, 1))
    } else {
        format!("{}MB", js_to_fixed(bytes as f64 / (1024.0 * 1024.0), 1))
    }
}

/// 从头部截断（保留前 N 行/字节），适合文件读取。
///
/// 从不返回半行；第一行就超过字节上限时返回空内容并置
/// [`TruncationResult::first_line_exceeds_limit`]。
///
/// 本函数先统计全文再委托 [`truncate_head_of`]，因此调用方不需要自己算 `totals`，也不受
/// 前缀契约影响；代价是必须先拿到完整 `content`。
pub fn truncate_head(content: &str, options: &TruncationOptions) -> TruncationResult {
    let totals = TruncationTotals {
        lines: split_lines_for_counting(content).len(),
        bytes: utf8_byte_length(content),
    };
    truncate_head_of(content, totals, options)
}

/// `truncateHead` 的已知总量版本。
///
/// `prefix` 必须就是全文，或者长于 `maxBytes + 1` 个 UTF-8 字节，或者至少含 `maxLines`
/// 个换行；满足这些前提时结果与 [`truncate_head`] 作用于全文完全相同。`totals.lines`
/// 按 [`truncate_head`] 的规则计（忽略结尾换行）。
///
/// 这两个值不参与逐行收集（逐行收集始终只看 `prefix`），但参与 [`truncate_head_of`] 的
/// 三处判断：阶段 2 的“是否截断”、结果里的 `total_lines` / `total_bytes`，以及阶段 4
/// 决定 `truncated_by` 取 `Lines` 还是 `Bytes`（比较 `output_lines.len() < total_lines`）。
/// 所以前缀可以比全文短，但必须包含足以做出结论的行/字节（read 工具的 `read_head` 就是
/// 按这个契约提前停止读取的）。调用方给错 `totals` 不会 panic，但 `total_*` 会与 `content`
/// 的实际情况矛盾，并且可能得出错误的 `truncated_by`（read 工具据此生成的诊断文案也会
/// 随之改变，见 `tools/read.rs`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TruncationTotals {
    /// 全文行数（按 `truncateHead` 的计法）。可能大于 `prefix` 自身的行数。
    pub lines: usize,
    /// 全文字节数。可能大于 `prefix.len()`。
    pub bytes: usize,
}

/// 见 [`TruncationTotals`]。
///
/// # 参数与返回
///
/// - `prefix`：全文或满足契约的前缀，是输出内容的唯一来源；
/// - `totals`：全文统计，决定“是否需要截断”并原样写进结果的 `total_lines` / `total_bytes`；
/// - `options`：未设置的字段回落到 [`DEFAULT_MAX_LINES`] / [`DEFAULT_MAX_BYTES`]。
///
/// 返回 `content` 为完整前缀、前缀的前若干完整行，或空串。函数不返回错误：所有输入组合
/// 都有结果，前提是否成立只影响结果的正确性，不影响内存安全或是否 panic。
pub fn truncate_head_of(
    prefix: &str,
    totals: TruncationTotals,
    options: &TruncationOptions,
) -> TruncationResult {
    // 阶段 1：解析本次调用实际生效的上限，并取出调用方给的全文统计。
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

    let total_bytes = totals.bytes;
    let lines = split_lines_for_counting(prefix);
    let total_lines = totals.lines;

    // 阶段 2：两个上限都没超，原样返回（此时 prefix 按契约就是全文）。
    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: prefix.to_owned(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    // 第一行单独就超过字节上限：不返回任何内容。
    // `lines.first()` 对空前缀给出 `None`，这里按 0 字节处理；空串本身不会触发这个分支
    // （0 字节不可能超过上限）。
    let first_line_bytes = lines.first().map_or(0, |line| utf8_byte_length(line));
    if first_line_bytes > max_bytes {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some(TruncatedBy::Bytes),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines,
            max_bytes,
        };
    }

    // 阶段 3：逐行收集完整行。任一上限先到就停，绝不在行中间切断。
    let mut output_lines: Vec<&str> = Vec::new();
    let mut output_bytes_count = 0usize;
    let mut truncated_by = TruncatedBy::Lines;

    for (i, line) in lines.iter().enumerate() {
        if i >= max_lines {
            break;
        }
        // 第 0 行不加换行；其余行前各有一个换行字节。
        let line_bytes = utf8_byte_length(line) + usize::from(i > 0);
        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        output_lines.push(line);
        output_bytes_count += line_bytes;
    }

    // 阶段 4：没有触发字节上限时，只有确实漏掉了行才能说是行数截断，否则是结尾换行超了
    // 字节。例：`"hello\nworld\n"`、maxLines=2、maxBytes=11：两行内容都收下，但全文
    // 12 字节超限，结论是 Bytes 而不是 Lines（见 tests/env_truncate.rs 的
    // `reports_bytes_when_only_a_trailing_newline_exceeds_limits_at_the_line_cap`）。
    if truncated_by != TruncatedBy::Bytes {
        truncated_by = if output_lines.len() < total_lines {
            TruncatedBy::Lines
        } else {
            TruncatedBy::Bytes
        };
    }

    // 阶段 5：拼回内容，并按实际输出重新统计字节数（不是累加值）。
    let output_content = output_lines.join("\n");
    let final_output_bytes = utf8_byte_length(&output_content);

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: output_lines.len(),
        output_bytes: final_output_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_byte_length_counts_bytes() {
        assert_eq!(utf8_byte_length("aé🙂\nb"), 9);
        assert_eq!(utf8_byte_length(""), 0);
    }

    #[test]
    fn format_size_matches_js_to_fixed() {
        assert_eq!(format_size(1023), "1023B");
        assert_eq!(format_size(1536), "1.5KB");
        assert_eq!(format_size(3 * 1024 * 1024), "3.0MB");
        // 1023 以内是裸字节数（TS 分支顺序）；256 走 B 分支。
        assert_eq!(format_size(256), "256B");
        // JS 的半数远离零：1.25.toFixed(1) === "1.3"（Rust 的 {:.1} 会得到 "1.2"）。
        assert_eq!(format_size(1280), "1.3KB");
    }
}
