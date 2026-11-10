//! themed-text.ts。
//!
//! 内容带主题颜色的 `Text`：普通 `Text` 会保留构建字符串时的颜色，主题切换后就会过时；
//! 本组件在每次 `invalidate()` 之后用 `build` 重新构建字符串。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/themed-text.ts`。
//!
//! 与 TS 的差异：Rust 没有类继承，这里用**组合**包装 [`Text`]（TS 是 `extends Text`）。
//! `build` 的契约相同：每次必须返回内容相同、只有颜色不同的字符串。
//!
//! # 构建时机与缓存
//!
//! 构造时只保存 `build` 并把 `stale` 置为 `true`，**不会调用它**。第一次 `render` 才构建，
//! 之后在同一个 stale 周期内无论宽度怎么变都不再调用（宽度变化由内部 [`Text`] 自己的
//! 原文+宽度缓存处理）。[`Component::invalidate`] 把 `stale` 重新置 `true` 并清空内部
//! `Text` 缓存，下一次 `render` 重新构建。
//!
//! 因此组件不会自己观察主题变化：切换主题后只有再次 `invalidate()` 才会用新颜色重建；
//! 而 `invalidate()` 本身也不会立即构建，真正的构建推迟到下一次渲染（测试
//! `builds_lazily_and_rebuilds_with_the_current_theme_after_invalidation` 锁定）。
//!
//! # 调用方
//!
//! `kiana-interactive` 的各对话框与状态行（reload/session/events/扩展 UI 等）用它显示需要
//! 跟随主题重着色的固定文案；`kiana-interactive/src/components/index.rs` 再导出本类型。

use kiana_tui::components::text_component::Text;
use kiana_tui::tui::Component;

/// 延迟构建主题化内容的文本组件（TS `class ThemedText extends Text`）。
pub struct ThemedText {
    text: Text,
    build: Box<dyn Fn() -> String>,
    stale: bool,
}

impl ThemedText {
    /// TS `constructor(build, paddingX = 1, paddingY = 1)`。
    ///
    /// 等价于 `with_padding(build, 1, 1)`；`build` 延迟到首次 `render` 才调用。
    pub fn new(build: Box<dyn Fn() -> String>) -> Self {
        ThemedText::with_padding(build, 1, 1)
    }

    /// 指定留白的构造（TS 的三参数构造）。
    ///
    /// `padding_x` 是左右各留的列数、`padding_y` 是上下各追加的整宽空行数，原样交给内部
    /// [`Text`]（宽度不足时 `padding_x` 会按 Text 的规则收窄）。构造期不调用 `build`。
    pub fn with_padding(
        build: Box<dyn Fn() -> String>,
        padding_x: usize,
        padding_y: usize,
    ) -> Self {
        ThemedText {
            text: Text::with_padding("", padding_x, padding_y),
            build,
            stale: true,
        }
    }

    /// 当前缓存的文本（构建前为空串）。
    ///
    /// 返回最近一次构建的结果；`invalidate()` 之后、下一次 `render` 之前仍是旧字符串。
    /// 返回的是未换行/未补白的原文。
    pub fn text(&self) -> &str {
        self.text.text()
    }
}

impl Component for ThemedText {
    /// 惰性构建：只在 `stale` 时调用一次 `build`，把结果写入内部 [`Text`]（`set_text` 会清掉
    /// 它按原文+宽度维护的缓存），然后委托渲染。
    fn render(&mut self, width: usize) -> Vec<String> {
        if self.stale {
            self.stale = false;
            self.text.set_text((self.build)());
        }
        self.text.render(width)
    }

    fn handles_input(&self) -> bool {
        // TS `Text` 没有 `handleInput`。
        false
    }

    /// 只清缓存并标记过期，不调用 `build`；下一次 `render` 才会重建。重复调用是安全的。
    fn invalidate(&mut self) {
        // TS：`super.invalidate(); this.stale = true;`
        self.text.invalidate();
        self.stale = true;
    }
}
