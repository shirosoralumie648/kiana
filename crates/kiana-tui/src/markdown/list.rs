//! 列表渲染：`Markdown#renderList`。
//!
//! 每层缩进 4 个空格；嵌套列表的行**不加**外层 marker 前缀；marker 的可见宽度决定续行缩进。
//! `preserveOrderedListMarkers` 时从 `item.raw` 还原源 marker（有序保留数字与原分隔符，
//! 无序统一为「字符 + 一个空格」）。
//!
//! # 调用关系
//! `crate::components::markdown::Markdown::render_token` 遇到 [`crate::markdown::Token::List`]
//! 时调用 `render_list(list, 0, width, style_context)`；本函数对嵌套列表递归调用自身并传
//! `depth + 1`。返回值是尚未加背景/左右 padding 的文本行，由外层组件统一处理。
//!
//! # 排版规则
//! - 缩进固定为 `4 * depth` 个空格；嵌套列表的行由递归调用直接产出（自带缩进），外层
//!   **不再**为它们补 marker 或续行前缀。
//! - marker 可见宽度决定续行缩进：首行是 `indent + theme.list_bullet(marker)`，后续行是
//!   `indent + visible_width(marker)` 个空格。主题只作用于首行前缀，续行前缀是纯空格，
//!   因此即使主题给 marker 加了 ANSI 样式，对齐也不受影响。
//! - 条目内容宽度 = `width - 首行前缀可见宽度`，下限 1，避免换行函数收到 0 宽度。
//! - `ListToken::loose` 为真时，除最后一个条目外每个条目后补一个空行；最后一个条目后
//!   不补。
//! - 空条目也输出一行 marker，否则该条目会从输出里“消失”。
//!
//! # 注意
//! - `item.tokens` 里可能存在 `Checkbox` token，但本渲染器不为它输出文本；复选框由本
//!   函数依据 `item.task` / `item.checked` 重新画进 marker，两者不会重复。
//! - 嵌套列表递归时传的是原始 `width` 而不是 `item_width`：子层会自行扣掉更深的缩进，
//!   正常情况下每行不超过 `width`；但 `width` 很窄（`item_width` 被 `.max(1)` 压到 1）或
//!   单个字素宽于可用宽度时，换行器可能让该行超出 `width`（见 `text::break_long_word`）。
//! - `preserve_ordered_list_markers` 只改变 marker 文本（因而也会改变续行缩进的宽度），
//!   不改变排版算法。

use std::sync::OnceLock;

use fancy_regex::Regex;

use crate::components::markdown::{InlineStyleContext, Markdown};
use crate::markdown::{ListItem, ListToken, Token};
use crate::text::{visible_width, wrap_text_with_ansi};

/// 从条目源码开头匹配有序 marker：最多 3 个前导空格、1–9 位数字、`.` 或 `)`，后跟空白。
///
/// 只在 `preserve_ordered_list_markers` 打开时用于还原源码 marker；数字限定 1–9 位，
/// 更长的数字串匹配失败，调用方会回退到自动编号。
fn ordered_marker_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"^(?: {0,3})(\d{1,9}[.)])[ \t]+").expect("ordered marker regex")
    })
}

/// 从条目源码开头匹配无序 marker：最多 3 个前导空格、`-`/`+`/`*`，后面是空白，或直接
/// 到行尾/串尾（空条目）。
///
/// `\r?\n` 兼容 CRLF；`Lexer::lex` 会先把 CRLF 归一化成 `\n`，所以实际输入通常只有 `\n`。
/// 只在 `preserve_ordered_list_markers` 打开时使用。
fn unordered_marker_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"^(?: {0,3})([-+*])(?:[ \t]+|(?=\r?\n|$))").expect("unordered marker regex")
    })
}

impl Markdown {
    /// `getOrderedListMarker(item)`：从 `item.raw` 还原 `1. ` / `1) `。
    ///
    /// 返回 `"{数字}{分隔符} "`，空白归一化为一个空格；`item.raw` 不匹配（缩进超过 3、
    /// 数字超过 9 位等）时返回 `None`，由 `render_list` 回退到 `start + index` 的自动编号。
    /// 只在 `preserve_ordered_list_markers` 打开时调用。
    pub(crate) fn get_ordered_list_marker(&self, item: &ListItem) -> Option<String> {
        let cap = ordered_marker_regex().captures(&item.raw).ok().flatten()?;
        Some(format!("{} ", cap.get(1)?.as_str()))
    }

    /// `getUnorderedListMarker(item)`：从 `item.raw` 还原 `- ` / `+ ` / `* `。
    ///
    /// 无论源码里 marker 后有几个空白，返回值都统一成「字符 + 一个空格」；不匹配时返回
    /// `None`，由 `render_list` 回退到 `- `。只在 `preserve_ordered_list_markers` 打开时调用。
    pub(crate) fn get_unordered_list_marker(&self, item: &ListItem) -> Option<String> {
        let cap = unordered_marker_regex()
            .captures(&item.raw)
            .ok()
            .flatten()?;
        Some(format!("{} ", cap.get(1)?.as_str()))
    }

    /// `renderList(token, depth, width, styleContext?)`。
    ///
    /// - `depth`：嵌套层级，0 表示顶层；缩进 = `4 * depth` 个空格。
    /// - `width`：当前深度的可用总列数（调用方已扣掉左右 padding），缩进也计算在内。
    /// - `style_context`：行内样式上下文，原样传给 `render_token`；列表 marker 的样式来自
    ///   `self.theme.list_bullet`。
    ///
    /// 返回渲染后的行，不含背景与左右 padding；嵌套列表的行由递归调用直接插入 `lines`，
    /// 不会再被外层加前缀。
    pub(crate) fn render_list(
        &self,
        token: &ListToken,
        depth: usize,
        width: usize,
        style_context: Option<&InlineStyleContext>,
    ) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let indent = "    ".repeat(depth);
        // `token.start` 缺省为 1（有序列表）
        let start_number = token.start.unwrap_or(1) as i64;

        for (index, item) in token.items.iter().enumerate() {
            let is_last_item = index + 1 == token.items.len();
            // marker 选择：有序列表默认重新编号（start + index），保留源码 marker 时
            // 优先用 item.raw 里的原文；无序列表同理，缺失时回退为 "- "。
            let bullet = if token.ordered {
                if self.options.preserve_ordered_list_markers {
                    self.get_ordered_list_marker(item)
                        .unwrap_or_else(|| format!("{}. ", start_number + index as i64))
                } else {
                    format!("{}. ", start_number + index as i64)
                }
            } else if self.options.preserve_ordered_list_markers {
                self.get_unordered_list_marker(item)
                    .unwrap_or_else(|| "- ".to_string())
            } else {
                "- ".to_string()
            };
            // 复选框由 item 状态重建；item.tokens 里的 Checkbox token 不参与渲染，
            // 否则会与这里画出的 marker 重复。
            let task_marker = if item.task {
                format!(
                    "[{}] ",
                    if item.checked.unwrap_or(false) {
                        "x"
                    } else {
                        " "
                    }
                )
            } else {
                String::new()
            };
            let marker = format!("{bullet}{task_marker}");
            // 首行前缀带主题样式；续行用纯空格，宽度按未着色的 marker 计算。
            let first_prefix = format!("{indent}{}", (self.theme.list_bullet)(&marker));
            let continuation_prefix = format!("{indent}{}", " ".repeat(visible_width(&marker)));
            // 扣掉首行前缀的可见宽度；下限 1，避免宽度为 0 时换行函数退化。
            let item_width = width.saturating_sub(visible_width(&first_prefix)).max(1);
            let mut rendered_any_line = false;

            for item_token in &item.tokens {
                if let Token::List(nested) = item_token {
                    // 嵌套列表直接递归（depth + 1）：它自带缩进，外层不再补前缀。
                    lines.extend(self.render_list(nested, depth + 1, width, style_context));
                    rendered_any_line = true;
                    continue;
                }

                // 条目内的每个块统一不传后随类型（与 TS `renderList` 的
                // `renderToken(itemToken, itemWidth, undefined, styleContext)` 对齐）：
                // `item.tokens` 仍可能包含多个块，传 `None` 可避免条目内部按“后随块”补空行。
                let item_lines = self.render_token(item_token, item_width, None, style_context);
                for line in &item_lines {
                    // 渲染出的行可能仍超过内容宽度，这里统一再换行一次。
                    for wrapped_line in wrap_text_with_ansi(line, item_width) {
                        let line_prefix = if rendered_any_line {
                            &continuation_prefix
                        } else {
                            &first_prefix
                        };
                        lines.push(format!("{line_prefix}{wrapped_line}"));
                        rendered_any_line = true;
                    }
                }
            }

            if !rendered_any_line {
                // 空 item 也占一行
                lines.push(first_prefix);
            }

            if token.loose && !is_last_item {
                // 松散列表：条目之间补空行；最后一项之后不补。
                lines.push(String::new());
            }
        }

        lines
    }
}
