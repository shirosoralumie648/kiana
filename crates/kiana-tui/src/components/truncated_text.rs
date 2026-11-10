//! components/truncated-text.ts。
//!
//! 单行截断文本：只保留第一行，按可用宽度截断（带省略号），并补齐到整宽。
//!
//! # 这个文件解决什么问题
//! 本组件把任意文本压成一行内容：只取首个 `\n` 之前的内容，按可用宽度截断（带省略号），
//! 并把内容行补白到 `width` 列（留白过宽时的例外见下方“注意事项”）。它适合作为单行行式
//! 组件放进按行排列的容器（`Container`、[`crate::components::v_stack::VStack`] 等）；
//! 仓库内的调用方见 `kiana-interactive` 的登录对话框、选择器与状态提示行
//! （`login_dialog.rs`、`oauth_selector.rs`、`interactive_mode/render.rs`）。
//!
//! # 职责边界
//! - 只做展示：不处理键盘输入（[`crate::tui::Component::handles_input`] 返回 false），
//!   没有缓存，也没有 `setText`（文本只读）。
//! - 不换行：只取第一个 `\n` 之前的内容，其余行直接丢弃；单独的 `\r` 不会切行，会留在首行
//!   字符串里交给宽度计算（[`crate::text::visible_width`] 把它按控制字符记 0 列）。
//! - 截断与 ANSI 处理委托给 [`crate::text::truncate_to_width`]（省略号 `"..."`、不补白）；
//!   本组件只负责左右留白与按可见宽度的最终补白。
//!
//! # render 的执行流程
//! 1. 生成 `padding_y` 个整宽空格行（上方留白）。
//! 2. 计算可用宽度 `max(1, width - padding_x * 2)`。
//! 3. 取首个 `\n` 之前的内容，按可用宽度截断。
//! 4. 左右各拼 `padding_x` 个空格，再按整行的**可见宽度**在右侧补空格到 `width`。
//! 5. 生成 `padding_y` 个整宽空格行（下方留白）；总共 `2 * padding_y + 1` 行。
//!
//! # 注意事项
//! - 宽度单位是终端列，不是字节：ANSI 转义序列零宽、CJK/emoji 占 2 列、TAB 记 3 列
//!   （见 [`crate::text::visible_width`]）。用 `String::len` 补白会导致列错位。
//! - 只有 `width > padding_x * 2` 时“每行可见宽度等于 `width`”才成立；留白宽于可用宽度时
//!   可用宽度被夹到 1 列，而左右留白照常输出，内容行会比 `width` 宽（右侧补白饱和为 0）。
//! - 常见调用形态是 `with_padding(text, 1, 0)`：左右各留一列。调用方包括 `kiana-interactive`
//!   的登录/选择器/状态提示行与 JS 绑定（`kiana-extensions` 的 `JsTruncatedText`）。

use crate::text::{truncate_to_width, visible_width};
use crate::tui::Component;

/// 单行截断文本（TS `class TruncatedText`）。
///
/// 持有原始文本与两组留白：`text` 原样保存（可以含 `\n`），`padding_x` 是左右各留的列数，
/// `padding_y` 是上下各留的行数。每次 render 都重新截断与补白，没有缓存字段，因此
/// [`crate::tui::Component::invalidate`] 没有可清的状态。
pub struct TruncatedText {
    text: String,
    padding_x: usize,
    padding_y: usize,
}

impl TruncatedText {
    /// `constructor(text, paddingX = 0, paddingY = 0)`。
    ///
    /// 无留白的快捷构造，等价于 `with_padding(text, 0, 0)`。TS 的构造参数可选，
    /// `new TruncatedText(text)` 就是同一个构造函数省略后两个参数的形式。
    pub fn new(text: impl Into<String>) -> Self {
        TruncatedText::with_padding(text, 0, 0)
    }

    /// 指定留白的构造。
    ///
    /// `padding_x` 是**左右各**留的列数（一行合计 `2 * padding_x`），`padding_y` 是上下各留
    /// 的行数（总行数 `2 * padding_y + 1`）。这里不做归一化：Rust 参数已是 `usize`；JS 绑定
    /// （`kiana-extensions` 的 `JsTruncatedText`）在构造前用 `non_negative_floor` 把脚本传入
    /// 的数字夹到非负整数。
    pub fn with_padding(text: impl Into<String>, padding_x: usize, padding_y: usize) -> Self {
        TruncatedText {
            text: text.into(),
            padding_x,
            padding_y,
        }
    }

    /// `setText` 在 TS 里**不存在**（文本只读）；这里同样只提供读取。
    ///
    /// 返回构造时保存的完整文本（可能含多行），而不是 render 输出的首行。
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl Component for TruncatedText {
    fn render(&mut self, width: usize) -> Vec<String> {
        // 输出形状固定为：padding_y 个整宽空格行 + 1 个内容行 + padding_y 个整宽空格行。
        // 留白行用空格填满而不是空串，父容器（Container/Stack）按行拼接时不会重新补宽。
        let mut result: Vec<String> = Vec::new();
        let empty_line = " ".repeat(width);

        for _ in 0..self.padding_y {
            result.push(empty_line.clone());
        }

        // 左右各留 padding_x，因此内容可用列宽是 width - 2 * padding_x；至少保留 1 列，
        // 因为 truncate_to_width 在 max_width = 0 时会直接返回空串。
        let available_width = (width.saturating_sub(self.padding_x * 2)).max(1);

        // 只取第一行（按 `\n` 切；`\r` 保留）。
        let single_line_text = match self.text.find('\n') {
            Some(index) => &self.text[..index],
            None => self.text.as_str(),
        };

        // 截断用默认省略号 "..." 且不补白（pad = false）：右侧补白要等左右 padding 拼好后
        // 按整行的可见宽度统一计算，避免在 truncate_to_width 与本函数里补两次。
        let display_text = truncate_to_width(single_line_text, available_width, "...", false);

        // 按可见宽度而不是字节/字符数补白：ANSI 序列零宽、CJK/emoji 占 2 列。
        let padding = " ".repeat(self.padding_x);
        let line_with_padding = format!("{padding}{display_text}{padding}");
        let line_visible_width = visible_width(&line_with_padding);
        // saturating_sub：左右留白已经把行撑得比 width 宽时饱和到 0，不产生负补白也不 panic。
        let padding_needed = width.saturating_sub(line_visible_width);
        result.push(format!("{line_with_padding}{}", " ".repeat(padding_needed)));

        for _ in 0..self.padding_y {
            result.push(empty_line.clone());
        }

        result
    }

    // TS 的 `TruncatedText` 只实现 `render`/`invalidate`，没有 `handleInput`；返回 false 让
    // 键盘分发跳过本组件（见 `Component::handles_input` 的文档）。
    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        // TS：无可失效的缓存状态。
        // Rust 侧同样没有缓存字段，每次 render 都重新截断，空实现就是完整语义。
    }
}
