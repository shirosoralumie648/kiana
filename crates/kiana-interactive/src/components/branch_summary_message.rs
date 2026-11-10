//! components/branch-summary-message.ts。
//!
//! 可折叠的分支摘要块（背景与自定义消息一致）。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/branch-summary-message.ts`。
//!
//! ## Public API for InteractiveMode
//!
//! - [`BranchSummaryMessageComponent::new`]（message, markdown theme, outputPad）
//! - `set_expanded` / `set_output_pad` / `Component::{render,invalidate}`
//!
//! # 职责与形态
//!
//! 一条 `branchSummary` 消息（切换会话分支时生成的摘要）在转录区里的展示块。它不参与
//! 分支切换逻辑，只把已经算好的 [`BranchSummaryMessage`] 渲染成可折叠卡片；消息来源与
//! 调用时机由 `crate::interactive_mode::render` 负责（该文件按 message role 分派）。
//!
//! # 结构与交互
//!
//! - 外层 [`BoxComponent`] 提供左右 `output_pad`、上下 1 行的留白和
//!   `ThemeBg::CustomMessageBg` 背景；
//! - 内容由 `[branch]` 标签、一个空行、以及折叠/展开两种正文之一组成；
//! - 整块内容包在 [`MouseRegion`] 里：左键点击切换 `expanded`，其余鼠标事件原样返回
//!   （继续冒泡）。点击回调与组件共享 `Rc<Cell<bool>>`，因此回调不持有组件借用，
//!   由 `dirty` 标记通知下一次 `render` 重建内容。
//!
//! # 折叠语义
//!
//! 折叠态只显示一行提示并带上键位文本；展开态用 [`Markdown`] 渲染
//! `**Branch Summary**\n\n` 加摘要正文，正文默认颜色是 `ThemeColor::CustomMessageText`。
//! `set_expanded` 无条件重建（即使值没变），`set_output_pad` 只改外层留白、不重建内容。
//!
//! # 注意事项
//!
//! - 标签里用的是原始 ANSI 加粗序列 `\x1b[1m...\x1b[22m`，是为了与 TS 的输出字节保持一致，
//!   不要替换成别的加粗 API。
//! - `Component::container_children` 直接暴露 `BoxComponent` 的子项，供挂载/遍历代码
//!   看到内部节点；`MouseRegion` 自身不转发 `container_children`（见其模块文档）。

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use kiana_session_core::messages::BranchSummaryMessage;
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

fn custom_message_bg() -> StyleFn {
    Arc::new(|text: &str| bg(ThemeBg::CustomMessageBg, text))
}

/// `class BranchSummaryMessageComponent extends Box`。
///
/// `expanded` 与 `dirty` 都用 `Rc<Cell<..>>` 而不是普通字段：鼠标回调需要在
/// `render` 之外持有它们（回调存进 `MouseRegion`，生命周期与组件内部节点一致），
/// 用共享单元避免回调反过来借用整个组件。`dirty` 只表示“需要重建内容”，由点击回调
/// 置位、由 `render` 消费。
pub struct BranchSummaryMessageComponent {
    bx: BoxComponent,
    message: BranchSummaryMessage,
    markdown_theme: MarkdownTheme,
    /// TS 的 `expanded` 字段；点击处理器与组件共享。
    expanded: Rc<Cell<bool>>,
    dirty: Rc<Cell<bool>>,
}

impl BranchSummaryMessageComponent {
    /// `constructor(message, markdownTheme = getMarkdownTheme(), outputPad = 1)`。
    pub fn new(
        message: BranchSummaryMessage,
        markdown_theme: MarkdownTheme,
        output_pad: usize,
    ) -> Self {
        let mut component = BranchSummaryMessageComponent {
            bx: BoxComponent::new(output_pad, 1, Some(custom_message_bg())),
            message,
            markdown_theme,
            expanded: Rc::new(Cell::new(false)),
            dirty: Rc::new(Cell::new(false)),
        };
        component.update_display();
        component
    }

    /// `setExpanded(expanded)`：无条件置位并重建（即使值没变，TS 也不做变化守卫）。
    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded.set(expanded);
        self.update_display();
    }

    /// `setOutputPad(outputPad)`：只改 paddingX，不重建。
    pub fn set_output_pad(&mut self, output_pad: usize) {
        self.bx.set_padding_x(output_pad);
    }

    fn update_display(&mut self) {
        // 每次重建都先清 dirty：内容与当前 expanded 对齐后无需再标记。
        self.dirty.set(false);
        self.bx.clear();
        let mut content = Container::new();

        // 标签与消息正文之间固定一个空行（TS 用 Spacer）。
        let label = fg(ThemeColor::CustomMessageLabel, "\x1b[1m[branch]\x1b[22m");
        content.add_child(Box::new(Text::with_padding(label, 0, 0)));
        content.add_child(Box::new(Spacer::new()));

        if self.expanded.get() {
            // 展开态：标题也是 Markdown 的一部分，因此和正文一起交给 Markdown 解析；
            // 默认文字颜色覆盖主题默认值，保持与自定义消息的观感一致。
            let header = "**Branch Summary**\n\n";
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
                fg(ThemeColor::CustomMessageText, "Branch summary ("),
                fg(ThemeColor::Dim, &kiana_ui_kit::key_text("app.tools.expand")),
                fg(ThemeColor::CustomMessageText, " to expand)"),
            );
            content.add_child(Box::new(Text::with_padding(text, 0, 0)));
        }

        // 点击只切换布尔状态并打 dirty 标记；真正重建推迟到下一次 render，
        // 避免在事件分发栈上重入 BoxComponent 的渲染状态。
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

impl Component for BranchSummaryMessageComponent {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn render(&mut self, width: usize) -> Vec<String> {
        // 点击回调只置 dirty，真正的内容更新在这里发生（见 update_display）。
        if self.dirty.get() {
            self.update_display();
        }
        self.bx.render(width)
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.bx.handle_mouse(event)
    }

    fn invalidate(&mut self) {
        // 先丢弃 Box 的渲染缓存，再按当前 expanded 重建内容。
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
