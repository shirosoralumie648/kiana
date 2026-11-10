//! dynamic-border.ts。
//!
//! 自适应宽度的水平边框组件。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/dynamic-border.ts`。
//!
//! 与 TS 的差异：
//! * TS 构造函数有默认参数 `color = (str) => theme.fg("border", str)`；Rust 没有默认参数，
//!   默认闭包由 [`DynamicBorder::default()`] 提供（每次 render 时才读全局主题，因此主题切换
//!   后无需重建，与 TS 的惰性默认参数一致）。
//! * `kiana_theme` 的实际 API 使用类型化 token（[`ThemeColor`]）并可能返回错误；TS 在主题未
//!   初始化时抛异常。这里的辅助函数 `fg` 在主题未初始化时 panic 相同的语义（正常宿主总是
//!   先 `initTheme`）。
//!
//! 宽度不变式：`render(0)` 输出 1 列（`─`，U+2500），这是 TS 里唯一允许溢出的组件。
//!
//! # 调用方与用法
//!
//! 需要整宽分隔线的界面直接构造它：`BorderedLoader` 用两行作为上下边框，
//! `kiana-interactive` 的多个 selector / 对话框、`kiana-builtin-extensions` 的 `/mcp`
//! 视图用它包住内容。它没有内部状态，`invalidate()` 是空操作，每次 `render` 都重新生成
//! 整行。
//!
//! `color` 是 [`StyleFn`]（`Arc<dyn Fn(&str) -> String + Send + Sync>`），可被多个组件
//! 共享；组件**不校验**返回值：默认实现只加 ANSI 颜色、可见宽度不变，而自定义函数若加入
//! 可见字符，行宽会超出传入的 `width`（测试里 `from_fn` 的 `<────>` 就是这种情况）。

use kiana_theme::ThemeColor;
use kiana_tui::components::text_component::StyleFn;
use kiana_tui::tui::Component;

/// 按全局主题着色（TS `theme.fg(token, str)`）。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// 自适应宽度的水平边框（TS `class DynamicBorder implements Component`）。
pub struct DynamicBorder {
    color: StyleFn,
}

impl DynamicBorder {
    /// 用显式颜色函数构造（TS `constructor(color)`）。
    ///
    /// 函数不在构造时调用，而是每次 `render` 时以整行文本（已重复到 `max(1, width)` 列）
    /// 调用一次。
    pub fn new(color: StyleFn) -> Self {
        DynamicBorder { color }
    }

    /// 用普通闭包构造的便捷入口。
    ///
    /// 闭包被包进 `Arc` 以满足 [`StyleFn`] 的 `Send + Sync + 'static` 约束；与 [`DynamicBorder::new`]
    /// 保存同一个 trait object，渲染行为完全一致。
    pub fn from_fn(color: impl Fn(&str) -> String + Send + Sync + 'static) -> Self {
        DynamicBorder {
            color: std::sync::Arc::new(color),
        }
    }

    /// 默认颜色函数：每次 render 读全局主题的 `border` token（惰性）。
    ///
    /// 主题切换后无需重建组件。主题未初始化时 `render` 会 panic（`fg` 里的 `expect`，对应
    /// TS 在同样情况下抛异常）。
    pub fn with_theme_border_color() -> Self {
        DynamicBorder {
            color: std::sync::Arc::new(|text: &str| fg(ThemeColor::Border, text)),
        }
    }
}

/// 等价于 [`DynamicBorder::with_theme_border_color`]（TS 的默认参数）。
impl Default for DynamicBorder {
    fn default() -> Self {
        DynamicBorder::with_theme_border_color()
    }
}

impl Component for DynamicBorder {
    /// 恰好一行：`color("─".repeat(max(1, width)))`（U+2500）。
    ///
    /// 没有上下留白；`width == 0` 仍输出一列，调用方若要求严格不超宽需要自己跳过它。
    fn render(&mut self, width: usize) -> Vec<String> {
        let line = "─".repeat(width.max(1));
        vec![(self.color)(&line)]
    }

    fn handles_input(&self) -> bool {
        // TS `DynamicBorder` 没有 `handleInput`。
        false
    }

    fn invalidate(&mut self) {
        // TS：`invalidate()` 是空实现（目前没有缓存状态）。
    }
}
