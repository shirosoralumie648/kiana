//! components/spacer.ts。
//!
//! 渲染若干空行的占位组件。
//!
//! 只实现渲染与失效通知；`handles_input` 返回 `false`，因此 TUI 不会把键盘输入路由给它
//! （TS 的 `Spacer` 没有 `handleInput`）。调用方（如 `kiana-interactive` 的消息/设置
//! 组件与 `kiana-extensions` 的 JS 绑定）用它制造固定纵向间距。
//!
//! 行数保存为 [`f64`] 而不是整数，是为了复刻 TS 循环的边界语义；实际渲染行数统一由
//! [`Spacer::line_count`] 计算（上整、负数与非有限值按 0）。

use crate::tui::Component;

/// 空行占位组件（TS `class Spacer`）。
///
/// TS 的 `lines` 是普通 `number`，`for (let i = 0; i < lines; i++)` 对非整数取上整
/// （`2.5` → 3 行）、负数 → 0 行。Rust 侧保留 `f64` 并用 `ceil` 复现；`NaN` → 0 行
/// （JS 同样 0 行），`Infinity` 在 JS 里会死循环，这里取 0 行（见模块文档）。
pub struct Spacer {
    /// 原始行数，未做校验或取整；解释见 [`Spacer::line_count`]。
    lines: f64,
}

impl Default for Spacer {
    fn default() -> Self {
        Spacer::new()
    }
}

impl Spacer {
    /// 默认 1 行。
    pub fn new() -> Self {
        Spacer { lines: 1.0 }
    }

    /// 指定行数（TS `constructor(lines)`）。
    ///
    /// 原样保存，不在构造时取整或夹取；非法值（负数、`NaN`、`Infinity`）在渲染时按
    /// [`Spacer::line_count`] 的规则处理。
    pub fn with_lines(lines: f64) -> Self {
        Spacer { lines }
    }

    /// `setLines(lines)`。
    ///
    /// 与构造一样原样保存，下一次渲染生效。
    pub fn set_lines(&mut self, lines: f64) {
        self.lines = lines;
    }

    /// 实际渲染的行数（`max(0, ceil(lines))`；非有限值 → 0）。
    ///
    /// 对很大的有限值不做上限截断：Rust 的浮点转整数是饱和转换，结果会接近
    /// `usize::MAX`，随后 `render` 会按该数量分配空行。需要可靠上界时由调用方保证。
    pub fn line_count(&self) -> usize {
        if self.lines.is_finite() {
            self.lines.max(0.0).ceil() as usize
        } else {
            0
        }
    }
}

impl Component for Spacer {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec![String::new(); self.line_count()]
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        // TS：无可失效的缓存状态。
    }
}
