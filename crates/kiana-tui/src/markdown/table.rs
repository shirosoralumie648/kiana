//! 表格渲染：`Markdown#renderTable`、`getLongestWordWidth`、`wrapCellText`。
//!
//! 列宽算法逐行对照 TS：自然宽度 / 最小词宽（上限 30）/ 权重分配 / 轮转补齐；
//! 太窄时回退为原始 markdown。边框字符与空格数是稳定的输出契约，改动需同步测试。
//!
//! # 调用关系
//! `crate::components::markdown::Markdown::render_token` 遇到 [`crate::markdown::Token::Table`]
//! 时调用 `render_table(token, width, next_token_type, style_context)`。`width` 是已扣掉
//! 左右 padding 的内容宽度；`next_token_type` 只用于决定表格后是否补一个空行。
//!
//! # 执行流程
//! 1. 列数取自表头；没有表头直接返回空，不渲染。
//! 2. 测量：把每个单元格的行内 token 渲染出来，得到“自然宽度”（整格一行的宽度）与
//!    “最小词宽”（最长不可断词，上限 30）。
//! 3. 分配：总宽放不下时以最小词宽为底、按比例分配；若连最小词宽都放不下，则每列先
//!    保底 1 列宽、再按 `(最小词宽 - 1)` 的权重分摊剩余宽度；最后把除不尽的列逐列 +1。
//! 4. 输出：上边框、表头（逐行换行、加粗、补空格）、分隔线、数据行（行间复用分隔线）、
//!    下边框。
//!
//! # 重要约束
//! - 渲染时用 `token.header.len()` 作为列数并据此索引 `column_widths`。`block` 的表格
//!   tokenizer 保证每个数据行恰好有同样多的单元格（不足补空、超出截断）；手工构造
//!   `TableToken` 时必须维持该不变量，否则这里可能越界 panic。
//! - 列对齐信息（`TableToken::align` / `TableCell::align`）当前**不参与渲染**，所有单元格
//!   都左对齐——本文件没有任何地方读取这两个字段。
//! - 宽度过窄（每列连 1 列宽都分不到）时整体回退为原始 markdown 文本并按 `width` 换行，
//!   不会渲染半张表。
//! - 边框字符（`┌ ─ ┬ ┐ │ ├ ┼ ┤ └ ┴ ┘`）和空格数是可观察输出的一部分，测试按精确字符串
//!   断言，改动前先看 `tests/markdown.rs` 的表格用例。

use crate::components::markdown::{InlineStyleContext, Markdown};
use crate::markdown::TableToken;
use crate::text::{is_js_whitespace, visible_width, wrap_text_with_ansi};

impl Markdown {
    /// `getLongestWordWidth(text, maxWidth?)`：按 JS 空白切词，取可见宽度最大值。
    ///
    /// 只把 JS 空白当分隔符，标点不算词边界；宽度用 [`visible_width`] 计算，因此 ANSI
    /// 样式不计入。`max_width` 在取到最大值**之后**截断（`render_table` 传 `Some(30)`，
    /// 防止一个超长单词把整列撑开）。空串或全空白返回 0。
    pub(crate) fn get_longest_word_width(&self, text: &str, max_width: Option<usize>) -> usize {
        let mut longest = 0usize;
        let mut current = String::new();
        for ch in text.chars() {
            if is_js_whitespace(ch) {
                if !current.is_empty() {
                    longest = longest.max(visible_width(&current));
                    current.clear();
                }
            } else {
                current.push(ch);
            }
        }
        if !current.is_empty() {
            longest = longest.max(visible_width(&current));
        }
        match max_width {
            Some(max_width) => longest.min(max_width),
            None => longest,
        }
    }

    /// `wrapCellText(text, maxWidth, stylePrefix = "")`。
    ///
    /// 先按 `max_width.max(1)` 做 ANSI 感知换行（零宽换行没有意义，统一抬到 1）。
    /// 每行末尾都追加 `style_prefix` 恢复外层样式；除最后一行外还会先追加一个 SGR reset
    /// （关闭粗体/斜体/下划线/删除线等），防止单元格内部的样式泄漏到补白和下一列。输入为
    /// 空时返回一个空行（`wrap_text_with_ansi` 的行为）。
    pub(crate) fn wrap_cell_text(
        &self,
        text: &str,
        max_width: usize,
        style_prefix: &str,
    ) -> Vec<String> {
        let lines = wrap_text_with_ansi(text, max_width.max(1));
        let line_count = lines.len();
        lines
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                // 非最后一行在行尾重置文本样式，再恢复外层样式
                let style_reset = if index + 1 < line_count {
                    "\x1b[22;23;24;25;27;28;29;39m"
                } else {
                    ""
                };
                format!("{line}{style_reset}{style_prefix}")
            })
            .collect()
    }

    /// `renderTable(token, availableWidth, nextTokenType?, styleContext?)`。
    ///
    /// - `available_width`：内容区总列数（已扣 padding），**包含**表格边框。
    /// - `next_token_type`：下一个兄弟 token 的类型；存在且不是 `"space"` 时在表格后补一个
    ///   空行（space token 自己会产生空行，避免重复）。
    /// - `style_context`：行内样式上下文；其 `style_prefix` 会追加到每个换行后的单元格行尾。
    ///
    /// 返回渲染后的行（含边框与补白），不含背景/左右 padding。表头为空时返回空 `Vec`。
    ///
    /// 列宽是“先测量后分配”：自然宽度按整格渲染结果算，最小宽度按最长不可断词（上限 30）
    /// 算；两者都可能超过可用宽度，因此先压缩最小宽度，再按权重分配剩余空间，最后处理
    /// 整数除法的舍入误差（逐列 +1）。
    pub(crate) fn render_table(
        &self,
        token: &TableToken,
        available_width: usize,
        next_token_type: Option<&str>,
        style_context: Option<&InlineStyleContext>,
    ) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let num_cols = token.header.len();
        // 表头决定列数；没有表头就没有可渲染的列（数据行的单元格也无从定位）。
        if num_cols == 0 {
            return lines;
        }

        // 边框占用："│ " + (n-1) * " │ " + " │" = 3n + 1
        let border_overhead = 3 * num_cols + 1;
        let available_for_cells = available_width as i64 - border_overhead as i64;
        if available_for_cells < num_cols as i64 {
            // 太窄，回退为原始 markdown
            let mut fallback_lines = if token.raw.is_empty() {
                Vec::new()
            } else {
                wrap_text_with_ansi(&token.raw, available_width)
            };
            if next_token_type.is_some_and(|kind| kind != "space") {
                // 与正常路径一致：后随非 space 块时补空行。
                fallback_lines.push(String::new());
            }
            return fallback_lines;
        }
        let available_for_cells = available_for_cells as usize;
        let max_unbroken_word_width = 30usize;

        // 自然列宽（无约束时每列需要多宽）与最小词宽
        // 测量阶段先渲染行内 token（含主题样式），再用可见宽度统计；min_word_widths
        // 至少为 1，保证后面压缩时每列还能放下一个字符。
        let mut natural_widths = vec![0usize; num_cols];
        let mut min_word_widths = vec![0usize; num_cols];
        for (index, cell) in token.header.iter().enumerate() {
            let header_text = self.render_inline_tokens(&cell.tokens, style_context);
            natural_widths[index] = visible_width(&header_text);
            min_word_widths[index] = self
                .get_longest_word_width(&header_text, Some(max_unbroken_word_width))
                .max(1);
        }
        for row in &token.rows {
            for (index, cell) in row.iter().enumerate() {
                let cell_text = self.render_inline_tokens(&cell.tokens, style_context);
                natural_widths[index] = natural_widths[index].max(visible_width(&cell_text));
                min_word_widths[index] = min_word_widths[index].max(
                    self.get_longest_word_width(&cell_text, Some(max_unbroken_word_width))
                        .max(1),
                );
            }
        }

        let mut min_column_widths = min_word_widths.clone();
        let mut min_cells_width: usize = min_column_widths.iter().sum();

        // 连最小词宽之和都放不下：每列先保底 1 列宽，再按 `(最小词宽 - 1)` 的权重分摊
        // 剩余宽度；权重为 0 的列（最小词宽为 1）在比例分摊阶段不增长，但整数除法的
        // 余数仍会按列顺序逐列 +1，因此这些列也可能多拿 1 列宽。
        if min_cells_width > available_for_cells {
            min_column_widths = vec![1; num_cols];
            let remaining = available_for_cells - num_cols;

            if remaining > 0 {
                let total_weight: usize = min_word_widths
                    .iter()
                    .map(|width| width.saturating_sub(1))
                    .sum();
                let growth: Vec<usize> = min_word_widths
                    .iter()
                    .map(|width| {
                        let weight = width.saturating_sub(1);
                        if total_weight > 0 {
                            ((weight as u128 * remaining as u128) / total_weight as u128) as usize
                        } else {
                            0
                        }
                    })
                    .collect();

                for index in 0..num_cols {
                    min_column_widths[index] += growth[index];
                }

                let allocated: usize = growth.iter().sum();
                let mut leftover = remaining - allocated;
                // 整数除法剩下的零头按列顺序逐列 +1（一轮最多 num_cols 列）。
                let mut index = 0usize;
                while leftover > 0 && index < num_cols {
                    min_column_widths[index] += 1;
                    leftover -= 1;
                    index += 1;
                }
            }

            min_cells_width = min_column_widths.iter().sum();
        }

        // 适配可用宽度的列宽
        let total_natural_width: usize = natural_widths.iter().sum::<usize>() + border_overhead;
        let mut column_widths: Vec<usize>;

        if total_natural_width <= available_width {
            // 自然放下
            // 每列取自然宽度与最小宽度的较大者。
            column_widths = (0..num_cols)
                .map(|index| natural_widths[index].max(min_column_widths[index]))
                .collect();
        } else {
            // 需要压缩列宽
            // 以最小宽度为底，按“自然宽度 - 最小宽度”的比例分配可用宽度。
            let total_grow_potential: usize = (0..num_cols)
                .map(|index| natural_widths[index].saturating_sub(min_column_widths[index]))
                .sum();
            let extra_width = available_for_cells.saturating_sub(min_cells_width);
            column_widths = (0..num_cols)
                .map(|index| {
                    let natural_width = natural_widths[index];
                    let min_width = min_column_widths[index];
                    let min_width_delta = natural_width.saturating_sub(min_width);
                    let grow = if total_grow_potential > 0 {
                        ((min_width_delta as u128 * extra_width as u128)
                            / total_grow_potential as u128) as usize
                    } else {
                        0
                    };
                    min_width + grow
                })
                .collect();

            // 分配舍入误差
            // 只加给还没到自然宽度的列；一轮下来一列都加不了就退出，避免死循环。
            let allocated: usize = column_widths.iter().sum();
            let mut remaining = available_for_cells.saturating_sub(allocated);
            while remaining > 0 {
                let mut grew = false;
                for index in 0..num_cols {
                    if remaining == 0 {
                        break;
                    }
                    if column_widths[index] < natural_widths[index] {
                        column_widths[index] += 1;
                        remaining -= 1;
                        grew = true;
                    }
                }
                if !grew {
                    break;
                }
            }
        }

        // 顶边框
        let top_border_cells: Vec<String> = column_widths
            .iter()
            .map(|width| "─".repeat(*width))
            .collect();
        lines.push(format!("┌─{}─┐", top_border_cells.join("─┬─")));

        // 表头（带换行）
        let mut header_cell_lines: Vec<Vec<String>> = Vec::new();
        for (index, cell) in token.header.iter().enumerate() {
            let text = self.render_inline_tokens(&cell.tokens, style_context);
            header_cell_lines.push(
                self.wrap_cell_text(
                    &text,
                    column_widths[index],
                    style_context
                        .map(|context| context.style_prefix.as_str())
                        .unwrap_or(""),
                ),
            );
        }
        // 表头行高 = 该行所有单元格换行后的最大行数，短单元格在下方补空串。
        let header_line_count = header_cell_lines
            .iter()
            .map(|cell_lines| cell_lines.len())
            .max()
            .unwrap_or(0);

        for line_index in 0..header_line_count {
            let mut row_parts: Vec<String> = Vec::new();
            for (column_index, cell_lines) in header_cell_lines.iter().enumerate() {
                let text = cell_lines.get(line_index).map(String::as_str).unwrap_or("");
                // 按可见宽度补空格到列宽，然后整体加粗；加粗在补白之后，不影响宽度计算。
                let padded = format!(
                    "{text}{}",
                    " ".repeat(column_widths[column_index].saturating_sub(visible_width(text)))
                );
                row_parts.push((self.theme.bold)(&padded));
            }
            lines.push(format!("│ {} │", row_parts.join(" │ ")));
        }

        // 分隔线
        let separator_cells: Vec<String> = column_widths
            .iter()
            .map(|width| "─".repeat(*width))
            .collect();
        let separator_line = format!("├─{}─┤", separator_cells.join("─┼─"));
        lines.push(separator_line.clone());

        // 数据行（带换行）
        // 与表头相同的换行/补白流程，但不套 theme.bold。
        for (row_index, row) in token.rows.iter().enumerate() {
            let mut row_cell_lines: Vec<Vec<String>> = Vec::new();
            for (index, cell) in row.iter().enumerate() {
                let text = self.render_inline_tokens(&cell.tokens, style_context);
                row_cell_lines.push(
                    self.wrap_cell_text(
                        &text,
                        column_widths[index],
                        style_context
                            .map(|context| context.style_prefix.as_str())
                            .unwrap_or(""),
                    ),
                );
            }
            let row_line_count = row_cell_lines
                .iter()
                .map(|cell_lines| cell_lines.len())
                .max()
                .unwrap_or(0);

            for line_index in 0..row_line_count {
                let mut row_parts: Vec<String> = Vec::new();
                for (column_index, cell_lines) in row_cell_lines.iter().enumerate() {
                    let text = cell_lines.get(line_index).map(String::as_str).unwrap_or("");
                    row_parts.push(format!(
                        "{text}{}",
                        " ".repeat(column_widths[column_index].saturating_sub(visible_width(text)))
                    ));
                }
                lines.push(format!("│ {} │", row_parts.join(" │ ")));
            }

            if row_index + 1 < token.rows.len() {
                // 行之间复用同一条分隔线；最后一行之后不再补。
                lines.push(separator_line.clone());
            }
        }

        // 底边框
        let bottom_border_cells: Vec<String> = column_widths
            .iter()
            .map(|width| "─".repeat(*width))
            .collect();
        lines.push(format!("└─{}─┘", bottom_border_cells.join("─┴─")));

        if next_token_type.is_some_and(|kind| kind != "space") {
            // 表格后补空行
            // next token 是 space 时由 space token 自己产生空行，避免重复。
            lines.push(String::new());
        }
        lines
    }
}
