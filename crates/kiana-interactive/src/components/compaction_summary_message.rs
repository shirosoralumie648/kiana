//! components/compaction-summary-message.ts。
//!
//! 可折叠的压缩摘要块（背景与自定义消息一致）。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/compaction-summary-message.ts`。
//!
//! ## Public API for InteractiveMode
//!
//! - [`CompactionSummaryMessageComponent::new`]（message, markdown theme, outputPad）
//! - `set_expanded` / `set_output_pad` / `js_to_locale_string`
//!
//! # 职责与形态
//!
//! 一条 `compactionSummary` 消息（上下文压缩后生成的摘要）在转录区里的展示块。它不参与
//! 压缩逻辑，只把已经算好的 [`CompactionSummaryMessage`] 渲染成可折叠卡片；调用方是
//! `crate::interactive_mode::render` 的按 role 分派。
//!
//! 结构与交互和分支摘要块一致：外层 [`BoxComponent`] 提供 `output_pad` 与
//! `ThemeBg::CustomMessageBg` 背景，整块内容包在 [`MouseRegion`] 里，左键点击切换
//! `expanded`；点击回调通过共享的 `Rc<Cell<bool>>` 置位 `dirty`，下一次 `render` 重建。
//!
//! # 为什么需要 `js_to_locale_string`
//!
//! 折叠/展开文案里的 `tokensBefore` 在 TS 中经 `Number.prototype.toLocaleString()` 输出
//! （见 `tests/components_messages.rs` 里以 node 结果为准的用例），而 Rust 的 `f64`
//! 格式化规则不同：需要千分位逗号、最多 3 位小数、half-expand 舍入、指数展开与 `-0` 保号。
//! [`js_to_locale_string`] 就是这层逐字对齐；本组件只用它格式化 `tokensBefore`，
//! `interactive_mode/commands/session.rs` 也复用它格式化统计数字。
//!
//! 数值流水线：
//!
//! 1. `NaN` / `±Infinity` 直接返回 `NaN` / `∞` / `-∞`；
//! 2. 取 `abs()` 后由 `kiana_json::js_num::format_js_number` 生成 JS `Number::toString`
//!    文本（可能带小写 `e` 指数）；
//! 3. `expand_js_number` 把指数形式展开成 `(整数部分, 小数部分)` 两段纯数字；
//! 4. `round_fraction` 按 `maximumFractionDigits = 3` 舍入（第 4 位 >= 5 进位），
//!    再去掉小数尾零；整数进位可能变长（`999.9999 -> 1,000`）；
//! 5. 整数部分每 3 位插入 `,`，最后补回负号。
//!
//! # 注意事项
//!
//! - 舍入是“看第 4 位十进制数字是否 >= 5 就进位”，与二进制浮点的 f64 值配合，
//!   结果对齐测试用例；不要换成 `format!("{:.3}")`（舍入与尾零规则都不同）。
//! - 标签同样使用原始 ANSI 加粗序列 `\x1b[1m...\x1b[22m` 以保持 TS 字节。
//! - `set_output_pad` 只改外层留白，不重建内容。

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use kiana_session_core::messages::CompactionSummaryMessage;
use kiana_theme::theme_tokens::{ThemeBg, ThemeColor};
use kiana_tui::components::box_component::BoxComponent;
use kiana_tui::components::markdown::{DefaultTextStyle, Markdown, MarkdownTheme, StyleFn};
use kiana_tui::components::mouse_region::MouseRegion;
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::Text;
use kiana_tui::tui::{
    Component, Container, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType,
};

fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

fn bg(color: ThemeBg, text: &str) -> String {
    kiana_theme::THEME
        .bg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// 把 JS 数字字符串（可能是指数形式）展开成 `(整数部分, 小数部分)` 的纯十进制。
///
/// 输入来自 `format_js_number`：非负、指数固定为小写 `e`（正指数带 `+`）。
/// 空的小数部分用空串表示；`point <= 0` 时整数部分是 `"0"`，并在前面补零。
fn expand_js_number(text: &str) -> (String, String) {
    // 先拆尾数/指数。解析失败按 0 处理（输入由 format_js_number 生成，正常不会失败）。
    let (mantissa, exponent) = match text.split_once('e') {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i64>().unwrap_or(0)),
        None => (text, 0),
    };
    let (int_part, frac_part) = match mantissa.split_once('.') {
        Some((int_part, frac_part)) => (int_part, frac_part),
        None => (mantissa, ""),
    };
    let mut digits = String::with_capacity(int_part.len() + frac_part.len());
    digits.push_str(int_part);
    digits.push_str(frac_part);
    let point = int_part.len() as i64 + exponent;
    if point <= 0 {
        let mut fraction = "0".repeat((-point) as usize);
        fraction.push_str(&digits);
        ("0".to_string(), fraction)
    } else if point as usize >= digits.len() {
        let mut integer = digits;
        integer.push_str(&"0".repeat(point as usize - integer.len()));
        (integer, String::new())
    } else {
        let point = point as usize;
        (digits[..point].to_string(), digits[point..].to_string())
    }
}

/// 十进制整数串加一（保留任意位数）。
///
/// 从个位向左处理：`9` 变 `0` 继续进位，遇到非 `9` 直接加一并停止；最高位仍溢出时
/// 在最前面插入 `'1'`（例如 `"999" -> "1000"`）。
fn increment_decimal(integer: &str) -> String {
    let mut bytes = integer.as_bytes().to_vec();
    let mut index = bytes.len();
    loop {
        if index == 0 {
            bytes.insert(0, b'1');
            break;
        }
        index -= 1;
        if bytes[index] == b'9' {
            bytes[index] = b'0';
        } else {
            bytes[index] += 1;
            break;
        }
    }
    String::from_utf8(bytes).expect("digits are ASCII")
}

/// `Intl.NumberFormat` 默认的 `maximumFractionDigits: 3` / `minimumFractionDigits: 0`，
/// 舍入为 half-expand（第 4 位 >= 5 进位），结果去掉小数尾零。
fn round_fraction(integer: &str, fraction: &str) -> (String, String) {
    const MAX_FRACTION_DIGITS: usize = 3;
    // 小数不超过 3 位：无需舍入，只去掉尾零（输入来自 format_js_number 的最短表示，
    // 不会以 0 结尾，此步只是防御）。
    if fraction.len() <= MAX_FRACTION_DIGITS {
        return (
            integer.to_string(),
            fraction.trim_end_matches('0').to_string(),
        );
    }
    let keep = &fraction[..MAX_FRACTION_DIGITS];
    let mut keep_number: u32 = keep.parse().expect("fraction digits");
    // 半展开舍入：只看被丢弃部分的第一位，>= 5 就进位（不链式查看后续位）。
    if fraction.as_bytes()[MAX_FRACTION_DIGITS] >= b'5' {
        keep_number += 1;
    }
    let (integer, keep) = if keep_number == 10u32.pow(MAX_FRACTION_DIGITS as u32) {
        // 形如 999.9999：小数进位到 1000，整数加一并把小数归零。
        (increment_decimal(integer), "0".repeat(MAX_FRACTION_DIGITS))
    } else {
        (
            integer.to_string(),
            format!("{keep_number:0width$}", width = MAX_FRACTION_DIGITS),
        )
    };
    (integer, keep.trim_end_matches('0').to_string())
}

/// JS `Number.prototype.toLocaleString()`（默认 locale 的千分位逗号）。
///
/// 该函数在本单元只有 `tokensBefore` 一处调用。默认 `Intl.NumberFormat` 选项下：
/// 每 3 位插入 `,`，小数最多 3 位（half-expand 舍入后去掉尾零），指数形式展开成全数字，
/// `-0` 保留符号；`NaN`/`±Infinity` 用 en-US 字形。
pub fn js_to_locale_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-∞".to_string()
        } else {
            "∞".to_string()
        };
    }
    let negative = value.is_sign_negative();
    // 先按绝对值格式化，负号最后统一补回；这样 `-0` 也能保留符号（abs 后仍是 0）。
    let text = kiana_json::js_num::format_js_number(value.abs());
    let (integer, fraction) = expand_js_number(&text);
    let (integer, fraction) = round_fraction(&integer, &fraction);
    // 千分位：从左往右，若从当前位起（含当前位）剩余数字个数是 3 的倍数，就在当前位之前插入逗号。
    let mut grouped = String::new();
    let digits: Vec<char> = integer.chars().collect();
    for (index, ch) in digits.iter().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(*ch);
    }
    if !fraction.is_empty() {
        grouped.push('.');
        grouped.push_str(&fraction);
    }
    if negative {
        format!("-{grouped}")
    } else {
        grouped
    }
}

/// `class CompactionSummaryMessageComponent extends Box`。
///
/// `expanded` 与 `dirty` 用 `Rc<Cell<..>>` 与鼠标回调共享，原因与
/// `BranchSummaryMessageComponent` 相同：回调在事件分发期间执行，不能借用整个组件。
pub struct CompactionSummaryMessageComponent {
    bx: BoxComponent,
    message: CompactionSummaryMessage,
    markdown_theme: MarkdownTheme,
    expanded: Rc<Cell<bool>>,
    dirty: Rc<Cell<bool>>,
}

impl CompactionSummaryMessageComponent {
    /// `constructor(message, markdownTheme = getMarkdownTheme(), outputPad = 1)`。
    pub fn new(
        message: CompactionSummaryMessage,
        markdown_theme: MarkdownTheme,
        output_pad: usize,
    ) -> Self {
        let mut component = CompactionSummaryMessageComponent {
            bx: BoxComponent::new(
                output_pad,
                1,
                Some(Arc::new(|text: &str| bg(ThemeBg::CustomMessageBg, text))),
            ),
            message,
            markdown_theme,
            expanded: Rc::new(Cell::new(false)),
            dirty: Rc::new(Cell::new(false)),
        };
        component.update_display();
        component
    }

    /// `setExpanded(expanded)`。
    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded.set(expanded);
        self.update_display();
    }

    /// `setOutputPad(outputPad)`：只改 paddingX。
    pub fn set_output_pad(&mut self, output_pad: usize) {
        self.bx.set_padding_x(output_pad);
    }

    fn update_display(&mut self) {
        // 重建后内容已与 expanded 对齐，清掉 dirty 标记。
        self.dirty.set(false);
        self.bx.clear();
        let mut content = Container::new();

        // token 数在这里一次性格式化，折叠与展开文案共用同一个结果。
        let token_str = js_to_locale_string(self.message.tokens_before);
        let label = fg(
            ThemeColor::CustomMessageLabel,
            "\x1b[1m[compaction]\x1b[22m",
        );
        content.add_child(Box::new(Text::with_padding(label, 0, 0)));
        content.add_child(Box::new(Spacer::new()));

        if self.expanded.get() {
            let header = format!("**Compacted from {token_str} tokens**\n\n");
            let color: StyleFn = Arc::new(|text: &str| fg(ThemeColor::CustomMessageText, text));
            content.add_child(Box::new(
                Markdown::new(
                    format!("{header}{}", self.message.summary),
                    0,
                    0,
                    self.markdown_theme.clone(),
                )
                .with_default_text_style(DefaultTextStyle {
                    color: Some(color),
                    ..DefaultTextStyle::default()
                }),
            ));
        } else {
            let text = format!(
                "{}{}{}",
                fg(
                    ThemeColor::CustomMessageText,
                    &format!("Compacted from {token_str} tokens ("),
                ),
                fg(ThemeColor::Dim, &kiana_ui_kit::key_text("app.tools.expand")),
                fg(ThemeColor::CustomMessageText, " to expand)"),
            );
            content.add_child(Box::new(Text::with_padding(text, 0, 0)));
        }

        // 点击只改共享状态并置 dirty；内容重建留给下一次 render。
        let expanded = Rc::clone(&self.expanded);
        let dirty = Rc::clone(&self.dirty);
        let region = MouseRegion::new(
            Box::new(content),
            Box::new(move |event: &TuiMouseEvent| {
                if event.event_type != TuiMouseEventType::Click
                    || event.button != TuiMouseButton::Left
                {
                    return None;
                }
                expanded.set(!expanded.get());
                dirty.set(true);
                Some(TuiMouseEventResult {
                    handled: Some(true),
                    ..TuiMouseEventResult::default()
                })
            }),
        );
        self.bx.add_child(Box::new(region));
    }
}

impl Component for CompactionSummaryMessageComponent {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn render(&mut self, width: usize) -> Vec<String> {
        // 点击回调留下 dirty 时先按当前 expanded 重建，再交给 Box 渲染。
        if self.dirty.get() {
            self.update_display();
        }
        self.bx.render(width)
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.bx.handle_mouse(event)
    }

    fn invalidate(&mut self) {
        // 丢弃 Box 的渲染缓存后立即重建，主题/宽度变化下一次 render 即生效。
        self.bx.invalidate();
        self.update_display();
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        Some(&self.bx.children)
    }

    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.bx.children)
    }
}
